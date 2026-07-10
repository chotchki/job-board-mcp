//! Rippling job-board adapter.
//!
//! Endpoint: `api.rippling.com/platform/api/ats/v1/board/{token}/jobs`, a bare JSON array
//! that is the LISTING source — thin (uuid, name, department, url, one workLocation). The
//! per-job GROUND TRUTH (full location list, description, post date, employment type)
//! lives in the job page's `__NEXT_DATA__` blob, so `fetch_posting` scrapes that.

use serde::Deserialize;

use super::parse;
use super::{Adapter, AdapterError};
use crate::config::BoardConfig;
use crate::http::{FetchCtx, HttpClient};
use crate::model::{Comp, Equity, Posting, PostingDetail, ReqId, WorkplaceType, content_hash};

#[derive(Deserialize)]
struct FeedJob {
    uuid: String,
    name: String,
    url: String,
    #[serde(default)]
    department: Option<Labelled>,
    #[serde(rename = "workLocation", default)]
    work_location: Option<Labelled>,
}

#[derive(Deserialize)]
struct Labelled {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

// The __NEXT_DATA__ shape, only the path we need: props.pageProps.apiData.jobPost.
#[derive(Deserialize)]
struct NextData {
    props: Props,
}
#[derive(Deserialize)]
struct Props {
    #[serde(rename = "pageProps")]
    page_props: PageProps,
}
#[derive(Deserialize)]
struct PageProps {
    #[serde(rename = "apiData")]
    api_data: ApiData,
}
#[derive(Deserialize)]
struct ApiData {
    #[serde(rename = "jobPost")]
    job_post: JobPost,
}
#[derive(Deserialize)]
struct JobPost {
    uuid: String,
    name: String,
    url: String,
    #[serde(default)]
    description: Option<serde_json::Value>,
    #[serde(rename = "workLocations", default)]
    work_locations: Vec<String>,
    #[serde(default)]
    department: Option<Labelled>,
    #[serde(rename = "employmentType", default)]
    employment_type: Option<Labelled>,
    #[serde(rename = "createdOn", default)]
    created_on: Option<String>,
}

pub struct RipplingAdapter;

impl RipplingAdapter {
    fn list_url(token: &str) -> String {
        format!("https://api.rippling.com/platform/api/ats/v1/board/{token}/jobs")
    }

    fn parse_feed(body: &str, board: &BoardConfig) -> Result<Vec<Posting>, AdapterError> {
        let jobs: Vec<FeedJob> = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("rippling jobs feed", e.to_string()))?;

        // The feed emits ONE ROW PER (job × workLocation) — a multi-location req appears
        // many times. Group by uuid so a posting is one req with all its locations merged,
        // in first-seen order, rather than N duplicate rows that inflate the count and let
        // the store's last-write-wins silently drop locations.
        let mut order: Vec<String> = Vec::new();
        let mut merged: std::collections::HashMap<String, (FeedJob, Vec<String>)> =
            std::collections::HashMap::new();
        for job in jobs {
            let location = job.work_location.as_ref().and_then(|l| l.label.clone());
            match merged.get_mut(&job.uuid) {
                Some((_, locations)) => {
                    if let Some(l) = location {
                        if !locations.contains(&l) {
                            locations.push(l);
                        }
                    }
                }
                None => {
                    order.push(job.uuid.clone());
                    merged.insert(job.uuid.clone(), (job, location.into_iter().collect()));
                }
            }
        }

        Ok(order
            .into_iter()
            .map(|uuid| {
                let (job, locations) = merged.remove(&uuid).expect("uuid was inserted");
                Self::to_posting(job, locations, board)
            })
            .collect())
    }

    fn to_posting(job: FeedJob, locations: Vec<String>, board: &BoardConfig) -> Posting {
        let workplace_type = infer_workplace(&locations);
        let comp = Comp::None;
        let hash = content_hash(
            &job.name,
            &locations,
            workplace_type,
            &comp,
            Equity::None,
            "",
        );

        Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(job.uuid),
            title: job.name,
            url: job.url,
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            equity: Equity::None,
            posted_at: None, // the feed carries no date; the detail's createdOn is the truth
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: job.department.and_then(|d| d.label),
            employment_type: None,
            content_hash: hash,
        }
    }

    fn parse_detail(html: &str, board: &BoardConfig) -> Result<PostingDetail, AdapterError> {
        let json = extract_next_data(html)?;
        let data: NextData = serde_json::from_str(json)
            .map_err(|e| AdapterError::drift("rippling __NEXT_DATA__", e.to_string()))?;
        let jp = data.props.page_props.api_data.job_post;

        let description_html = jp.description.as_ref().map(join_description);
        let workplace_type = infer_workplace(&jp.work_locations);
        let comp = Comp::None;
        let description = description_html.clone().unwrap_or_default();
        let hash = content_hash(
            &jp.name,
            &jp.work_locations,
            workplace_type,
            &comp,
            Equity::None,
            &description,
        );

        let posting = Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(jp.uuid),
            title: jp.name,
            url: jp.url,
            locations: jp.work_locations,
            workplace_type,
            remote_scope: None,
            comp,
            equity: Equity::None,
            posted_at: parse::rfc3339("rippling createdOn", jp.created_on.as_deref())?,
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: jp.department.and_then(|d| d.label),
            // employmentType's readable value is in `id` here ("Salaried, full-time"),
            // with `label` holding the code ("SALARIED_FT") — Rippling inverts them.
            employment_type: jp.employment_type.and_then(|e| e.id.or(e.label)),
            content_hash: hash,
        };
        Ok(PostingDetail {
            posting,
            description_text: description_html.as_deref().map(parse::strip_tags),
            description_html,
        })
    }
}

impl Adapter for RipplingAdapter {
    async fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> Result<Vec<Posting>, AdapterError> {
        let body = http
            .get_text(
                &Self::list_url(board.token.as_str()),
                &FetchCtx::from_board(board),
            )
            .await?;
        Self::parse_feed(&body, board)
    }

    async fn detail(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
        req_id: &ReqId,
    ) -> Result<PostingDetail, AdapterError> {
        // The detail is embedded in the job PAGE, not the API — fetch its HTML.
        let url = format!(
            "https://ats.rippling.com/{}/jobs/{}",
            board.token.as_str(),
            req_id
        );
        let html = http.get_text(&url, &FetchCtx::from_board(board)).await?;
        Self::parse_detail(&html, board)
    }
}

/// Pull the `__NEXT_DATA__` JSON blob out of a Next.js page. String slicing, not a regex
/// dependency — find the script open tag, then its closing tag.
fn extract_next_data(html: &str) -> Result<&str, AdapterError> {
    const OPEN: &str = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let start = html
        .find(OPEN)
        .ok_or_else(|| AdapterError::drift("rippling page", "no __NEXT_DATA__ script tag"))?;
    let after = &html[start + OPEN.len()..];
    let end = after
        .find("</script>")
        .ok_or_else(|| AdapterError::drift("rippling page", "unterminated __NEXT_DATA__"))?;
    Ok(&after[..end])
}

/// Rippling's `description` is an object of HTML sections (company, role, …). Join the
/// string values in key order.
fn join_description(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => map
            .values()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn infer_workplace(locations: &[String]) -> WorkplaceType {
    if locations
        .iter()
        .any(|l| l.to_lowercase().contains("remote"))
    {
        WorkplaceType::Remote
    } else if locations.is_empty() {
        WorkplaceType::Unknown
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
            id: BoardId::new("rippling"),
            ats: Ats::Rippling,
            token: AtsToken::new("rippling"),
            site: None,
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    #[test]
    fn feed_rows_are_grouped_by_uuid_with_locations_merged() {
        // The fixture is 3 feed rows: two for one uuid (different locations) + one other.
        // They must collapse to 2 postings, the first carrying BOTH its locations — not
        // 3 rows, and not a location silently dropped.
        let postings =
            RipplingAdapter::parse_feed(include_str!("fixtures/rippling_jobs.json"), &board())
                .unwrap();
        assert_eq!(postings.len(), 2);
        let p = &postings[0];
        assert_eq!(p.ats, Ats::Rippling);
        assert_eq!(p.req_id, ReqId::new("2750c304-5e66-40a5-a073-cf717c425415"));
        assert_eq!(
            p.locations,
            vec![
                "Remote (Oregon, US)".to_owned(),
                "Remote (El Paso, Texas, US)".to_owned(),
            ]
        );
        assert_eq!(p.workplace_type, WorkplaceType::Remote);
        assert_eq!(p.posted_at, None); // feed has no date
    }

    #[test]
    fn detail_from_next_data_carries_the_ground_truth() {
        // Wrap the fixture in a tiny HTML shell to exercise the extraction too.
        let next_data = include_str!("fixtures/rippling_next_data.json");
        let html = format!(
            r#"<html><body><script id="__NEXT_DATA__" type="application/json">{next_data}</script></body></html>"#
        );
        let detail = RipplingAdapter::parse_detail(&html, &board()).unwrap();
        assert_eq!(
            detail.posting.req_id,
            ReqId::new("2f0674e6-f01f-4ecd-b459-e947241c211f")
        );
        // The full location list, not the single feed workLocation.
        assert_eq!(detail.posting.locations.len(), 4);
        assert!(detail.posting.posted_at.is_some()); // createdOn
        assert_eq!(
            detail.posting.employment_type.as_deref(),
            Some("Salaried, full-time")
        );
        assert!(detail.description_html.is_some());
    }

    #[test]
    fn missing_next_data_is_parse_drift() {
        let err =
            RipplingAdapter::parse_detail("<html>no script here</html>", &board()).unwrap_err();
        assert!(matches!(err, AdapterError::ParseDrift { .. }));
    }

    #[test]
    fn a_changed_feed_shape_is_parse_drift() {
        let broken = r#"[{"name":"x","url":"http://x"}]"#; // no uuid
        assert!(matches!(
            RipplingAdapter::parse_feed(broken, &board()).unwrap_err(),
            AdapterError::ParseDrift { .. }
        ));
    }
}
