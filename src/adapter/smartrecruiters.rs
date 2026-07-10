//! SmartRecruiters job-board adapter.
//!
//! Endpoint: `api.smartrecruiters.com/v1/companies/{token}/postings`, paginated with
//! `offset`/`limit`/`totalFound`; detail is `/postings/{id}`. `token` is the company
//! identifier (e.g. `"Visa"`). Comparatively clean — location carries explicit
//! `remote`/`hybrid` booleans, so workplace type is read, not inferred.

use serde::Deserialize;

use super::parse;
use super::{Adapter, AdapterError};
use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{Comp, Posting, PostingDetail, ReqId, WorkplaceType, content_hash};

const PAGE_LIMIT: i64 = 100;
const MAX_POSTINGS: usize = 10_000;

#[derive(Deserialize)]
struct Page {
    #[serde(rename = "totalFound")]
    total_found: i64,
    #[serde(default)]
    content: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    id: String,
    name: String,
    #[serde(default)]
    location: Option<Location>,
    #[serde(rename = "releasedDate", default)]
    released_date: Option<String>,
    #[serde(default)]
    department: Option<Labelled>,
    #[serde(rename = "typeOfEmployment", default)]
    type_of_employment: Option<Labelled>,
}

#[derive(Deserialize)]
struct Location {
    #[serde(rename = "fullLocation", default)]
    full_location: Option<String>,
    #[serde(default)]
    remote: bool,
    #[serde(default)]
    hybrid: bool,
}

#[derive(Deserialize)]
struct Labelled {
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize)]
struct Detail {
    #[serde(flatten)]
    posting: Raw,
    #[serde(rename = "postingUrl", default)]
    posting_url: Option<String>,
    #[serde(rename = "jobAd", default)]
    job_ad: Option<JobAd>,
}

#[derive(Deserialize)]
struct JobAd {
    #[serde(default)]
    sections: std::collections::BTreeMap<String, Section>,
}

#[derive(Deserialize)]
struct Section {
    #[serde(default)]
    text: Option<String>,
}

pub struct SmartRecruitersAdapter;

impl SmartRecruitersAdapter {
    fn postings_url(token: &str, offset: i64) -> String {
        format!(
            "https://api.smartrecruiters.com/v1/companies/{token}/postings?limit={PAGE_LIMIT}&offset={offset}"
        )
    }

    fn detail_url(token: &str, req_id: &ReqId) -> String {
        format!("https://api.smartrecruiters.com/v1/companies/{token}/postings/{req_id}")
    }

    fn parse_page(body: &str, board: &BoardConfig) -> Result<(Vec<Posting>, i64), AdapterError> {
        let page: Page = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("smartrecruiters postings", e.to_string()))?;
        let postings = page
            .content
            .into_iter()
            .map(|p| Self::to_posting(p, board))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((postings, page.total_found))
    }

    fn to_posting(raw: Raw, board: &BoardConfig) -> Result<Posting, AdapterError> {
        let workplace_type = raw
            .location
            .as_ref()
            .map(workplace)
            .unwrap_or(WorkplaceType::Unknown);
        let locations: Vec<String> = raw
            .location
            .as_ref()
            .and_then(|l| l.full_location.clone())
            .into_iter()
            .collect();
        let comp = Comp::None;
        let hash = content_hash(&raw.name, &locations, workplace_type, &comp, "");

        Ok(Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(raw.id.clone()),
            title: raw.name,
            url: format!(
                "https://jobs.smartrecruiters.com/{}/{}",
                board.token.as_str(),
                raw.id
            ),
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            posted_at: parse::rfc3339(
                "smartrecruiters releasedDate",
                raw.released_date.as_deref(),
            )?,
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: raw.department.and_then(|d| d.label),
            employment_type: raw.type_of_employment.and_then(|t| t.label),
            content_hash: hash,
        })
    }
}

impl Adapter for SmartRecruitersAdapter {
    async fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> Result<Vec<Posting>, AdapterError> {
        let token = board.token.as_str();
        let mut postings = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let body = http.get_text(&Self::postings_url(token, offset)).await?;
            let (page, total) = Self::parse_page(&body, board)?;
            let page_len = page.len();
            postings.extend(page);
            offset += PAGE_LIMIT;
            if page_len == 0 || offset >= total {
                break;
            }
            if postings.len() >= MAX_POSTINGS {
                tracing::warn!(board = %board.id, total, "smartrecruiters board exceeds MAX_POSTINGS; truncating");
                break;
            }
        }
        Ok(postings)
    }

    async fn detail(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
        req_id: &ReqId,
    ) -> Result<PostingDetail, AdapterError> {
        let body = http
            .get_text(&Self::detail_url(board.token.as_str(), req_id))
            .await?;
        let detail: Detail = serde_json::from_str(&body)
            .map_err(|e| AdapterError::drift("smartrecruiters posting detail", e.to_string()))?;
        // Concatenate the jobAd sections into one description, in section order.
        let description_html: String = detail
            .job_ad
            .map(|ad| {
                ad.sections
                    .into_values()
                    .filter_map(|s| s.text)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let mut posting = Self::to_posting(detail.posting, board)?;
        if let Some(url) = detail.posting_url {
            posting.url = url;
        }
        Ok(PostingDetail {
            posting,
            description_html: (!description_html.is_empty()).then_some(description_html),
            description_text: None,
        })
    }
}

fn workplace(loc: &Location) -> WorkplaceType {
    if loc.remote {
        WorkplaceType::Remote
    } else if loc.hybrid {
        WorkplaceType::Hybrid
    } else {
        WorkplaceType::Onsite
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, BoardId};

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("visa"),
            ats: Ats::SmartRecruiters,
            token: AtsToken::new("Visa"),
            site: None,
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    #[test]
    fn parses_the_real_fixture() {
        let (postings, total) = SmartRecruitersAdapter::parse_page(
            include_str!("fixtures/smartrecruiters_postings.json"),
            &board(),
        )
        .unwrap();
        assert_eq!(total, 2);
        assert_eq!(postings.len(), 2);
        let p = &postings[0];
        assert_eq!(p.ats, Ats::SmartRecruiters);
        assert_eq!(p.req_id, ReqId::new("744000133907678"));
        assert_eq!(p.title, "Sr. Manager");
        assert_eq!(
            p.url,
            "https://jobs.smartrecruiters.com/Visa/744000133907678"
        );
        assert_eq!(p.locations, vec!["Austin, TX, United States".to_owned()]);
        // location.remote=false, hybrid=false → Onsite, read not inferred.
        assert_eq!(p.workplace_type, WorkplaceType::Onsite);
        assert_eq!(p.employment_type.as_deref(), Some("Full-time"));
        assert_eq!(
            p.department.as_deref(),
            Some("Software Development/Engineering")
        );
        assert!(p.posted_at.is_some());
    }

    #[test]
    fn workplace_reads_the_location_booleans() {
        assert_eq!(
            workplace(&Location {
                full_location: None,
                remote: true,
                hybrid: false
            }),
            WorkplaceType::Remote
        );
        assert_eq!(
            workplace(&Location {
                full_location: None,
                remote: false,
                hybrid: true
            }),
            WorkplaceType::Hybrid
        );
        assert_eq!(
            workplace(&Location {
                full_location: None,
                remote: false,
                hybrid: false
            }),
            WorkplaceType::Onsite
        );
    }

    #[test]
    fn a_changed_shape_is_parse_drift() {
        let broken = r#"{"totalFound":1,"content":[{"name":"x"}]}"#; // no id
        assert!(matches!(
            SmartRecruitersAdapter::parse_page(broken, &board()).unwrap_err(),
            AdapterError::ParseDrift { .. }
        ));
    }
}
