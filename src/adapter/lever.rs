//! Lever job-board adapter.
//!
//! Endpoint: `api.lever.co/v0/postings/{token}?mode=json` (a bare JSON array of
//! postings), and `/postings/{token}/{id}?mode=json` for a single posting's detail.
//!
//! SPEC calls Lever "comparatively sane", and it is: `workplaceType` is a clean
//! lowercase enum, `salaryRange` is structured when present, and `createdAt` is a plain
//! epoch. The only mild wrinkles are that the top level is an array (not an object) and
//! the post date is milliseconds since the epoch rather than a string.

use serde::Deserialize;

use super::parse;
use super::{Adapter, AdapterError};
use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{
    Comp, CompSource, Currency, Posting, PostingDetail, ReqId, WorkplaceType, content_hash,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    text: String,
    hosted_url: String,
    #[serde(default)]
    categories: Categories,
    #[serde(default)]
    workplace_type: Option<String>,
    #[serde(default)]
    created_at: Option<i64>,
    #[serde(default)]
    salary_range: Option<SalaryRange>,
    #[serde(default)]
    description_plain: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Categories {
    #[serde(default)]
    commitment: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    all_locations: Vec<String>,
}

#[derive(Deserialize)]
struct SalaryRange {
    #[serde(default)]
    min: Option<serde_json::Number>,
    #[serde(default)]
    max: Option<serde_json::Number>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    interval: Option<String>,
}

pub struct LeverAdapter;

impl LeverAdapter {
    fn list_url(token: &str) -> String {
        format!("https://api.lever.co/v0/postings/{token}?mode=json")
    }

    fn detail_url(token: &str, req_id: &ReqId) -> String {
        format!("https://api.lever.co/v0/postings/{token}/{req_id}?mode=json")
    }

    fn parse_postings(body: &str, board: &BoardConfig) -> Result<Vec<Posting>, AdapterError> {
        let raw: Vec<Job> = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("lever postings", e.to_string()))?;
        raw.into_iter()
            .map(|p| Self::to_posting(&p, board))
            .collect()
    }

    fn parse_detail(body: &str, board: &BoardConfig) -> Result<PostingDetail, AdapterError> {
        let raw: Job = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("lever posting detail", e.to_string()))?;
        let description_html = raw.description.clone();
        let description_text = raw.description_plain.clone();
        let posting = Self::to_posting(&raw, board)?;
        Ok(PostingDetail {
            posting,
            description_html,
            description_text,
        })
    }

    fn to_posting(raw: &Job, board: &BoardConfig) -> Result<Posting, AdapterError> {
        let locations = if raw.categories.all_locations.is_empty() {
            raw.categories.location.iter().cloned().collect()
        } else {
            raw.categories.all_locations.clone()
        };
        let workplace_type = map_workplace(raw.workplace_type.as_deref());
        let comp = extract_comp(raw.salary_range.as_ref())?;
        let description = raw
            .description_plain
            .as_deref()
            .or(raw.description.as_deref())
            .unwrap_or_default();
        let hash = content_hash(&raw.text, &locations, workplace_type, &comp, description);

        Ok(Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(raw.id.clone()),
            title: raw.text.clone(),
            url: raw.hosted_url.clone(),
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            posted_at: parse::epoch_millis(raw.created_at),
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: raw.categories.department.clone(),
            employment_type: raw.categories.commitment.clone(),
            content_hash: hash,
        })
    }
}

impl Adapter for LeverAdapter {
    async fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> Result<Vec<Posting>, AdapterError> {
        let body = http.get_text(&Self::list_url(board.token.as_str())).await?;
        Self::parse_postings(&body, board)
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
        Self::parse_detail(&body, board)
    }
}

fn map_workplace(value: Option<&str>) -> WorkplaceType {
    match value {
        Some("remote") => WorkplaceType::Remote,
        Some("hybrid") => WorkplaceType::Hybrid,
        Some("onsite") => WorkplaceType::Onsite,
        _ => WorkplaceType::Unknown,
    }
}

fn extract_comp(range: Option<&SalaryRange>) -> Result<Comp, AdapterError> {
    let Some(range) = range else {
        return Ok(Comp::None);
    };
    let currency = match range.currency.as_deref() {
        Some(code) => Currency::new(code)
            .map_err(|e| AdapterError::drift("lever salaryRange.currency", e.to_string()))?,
        None => return Ok(Comp::None),
    };
    let exp = currency.minor_unit_exponent();
    let interval = parse::interval("lever salaryRange.interval", range.interval.as_deref())?;

    let Some(min_minor) = parse::number_to_minor("lever salaryRange.min", range.min.as_ref(), exp)?
    else {
        return Ok(Comp::None);
    };
    let max_minor = parse::number_to_minor("lever salaryRange.max", range.max.as_ref(), exp)?
        .unwrap_or(min_minor);

    if max_minor == min_minor {
        Comp::point(currency, min_minor, interval, CompSource::Api)
    } else {
        Comp::band(currency, min_minor, max_minor, interval, CompSource::Api)
    }
    .map_err(|e| AdapterError::drift("lever salaryRange", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, BoardId, CompInterval};

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("gopuff"),
            ats: Ats::Lever,
            token: AtsToken::new("gopuff"),
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    #[test]
    fn parses_the_real_fixture() {
        let postings =
            LeverAdapter::parse_postings(include_str!("fixtures/lever_postings.json"), &board())
                .unwrap();
        assert_eq!(postings.len(), 2);
        let p = &postings[0];
        assert_eq!(p.ats, Ats::Lever);
        assert_eq!(p.req_id, ReqId::new("e893d05b-91f5-4450-9335-e6e8b2e87090"));
        assert_eq!(p.title, "Retail Store Manager II, Orinda, #490");
        assert_eq!(
            p.url,
            "https://jobs.lever.co/gopuff/e893d05b-91f5-4450-9335-e6e8b2e87090"
        );
        assert_eq!(p.locations, vec!["Orinda, CA".to_owned()]);
        assert_eq!(p.workplace_type, WorkplaceType::Onsite);
        assert_eq!(p.employment_type.as_deref(), Some("Full Time"));
        assert_eq!(p.department.as_deref(), Some("BevMo!"));
        assert!(p.posted_at.is_some());
    }

    #[test]
    fn structured_salary_range_becomes_a_typed_band() {
        let postings =
            LeverAdapter::parse_postings(include_str!("fixtures/lever_postings.json"), &board())
                .unwrap();
        // $63,000–$86,625/yr USD → exact integer minor units.
        assert_eq!(
            postings[0].comp,
            Comp::band(
                Currency::new("USD").unwrap(),
                6_300_000,
                8_662_500,
                CompInterval::Year,
                CompSource::Api,
            )
            .unwrap()
        );
        // The posting without a salaryRange is Comp::None, not a guess.
        assert_eq!(postings[1].comp, Comp::None);
    }

    #[test]
    fn a_changed_shape_is_parse_drift() {
        // Top level is an array; a posting missing `id` must fail loud.
        let broken = r#"[{"text":"x","hostedUrl":"http://x"}]"#;
        assert!(matches!(
            LeverAdapter::parse_postings(broken, &board()).unwrap_err(),
            AdapterError::ParseDrift { .. }
        ));
    }
}
