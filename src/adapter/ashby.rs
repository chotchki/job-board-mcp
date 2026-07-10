//! Ashby job-board adapter.
//!
//! Endpoint: `api.ashbyhq.com/posting-api/job-board/{token}?includeCompensation=true`.
//! The list response carries everything — descriptions AND structured compensation —
//! and there is no public single-job endpoint (it 401s), so `detail` re-fetches the
//! board and filters to the req.
//!
//! Quirks this adapter owns:
//! - **`workplaceType` is the truth; `isRemote` is noise.** SPEC is explicit, and the
//!   data agrees — `isRemote` is a board-wide flag, `workplaceType` is per-posting
//!   (Remote / Hybrid / OnSite). We read the latter and ignore the former entirely.
//! - **Comp is structured in the API.** `compensation.summaryComponents` holds a
//!   `Salary` entry with integer `minValue`/`maxValue`, a `currencyCode` and an
//!   `interval`. We convert to integer minor units through the money path — never a
//!   float. Equity components are ignored (equity is out of the v0.1 comp model).
//! - **May 403 a bare client UA.** The shared HTTP layer sends a browser UA, so this
//!   is handled upstream.

use serde::Deserialize;

use super::parse;
use super::{Adapter, AdapterError};
use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{
    Comp, CompSource, Currency, Posting, PostingDetail, ReqId, WorkplaceType, content_hash,
};

#[derive(Deserialize)]
struct JobBoard {
    jobs: Vec<Job>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    title: String,
    job_url: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    employment_type: Option<String>,
    #[serde(default)]
    workplace_type: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    compensation: Option<Compensation>,
    #[serde(default)]
    description_html: Option<String>,
    #[serde(default)]
    description_plain: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Compensation {
    #[serde(default)]
    summary_components: Vec<CompComponent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompComponent {
    #[serde(default)]
    compensation_type: Option<String>,
    #[serde(default)]
    min_value: Option<serde_json::Number>,
    #[serde(default)]
    max_value: Option<serde_json::Number>,
    #[serde(default)]
    currency_code: Option<String>,
    #[serde(default)]
    interval: Option<String>,
}

pub struct AshbyAdapter;

impl AshbyAdapter {
    fn list_url(token: &str) -> String {
        format!("https://api.ashbyhq.com/posting-api/job-board/{token}?includeCompensation=true")
    }

    fn parse_jobs(body: &str, board: &BoardConfig) -> Result<Vec<Posting>, AdapterError> {
        let parsed: JobBoard = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("ashby job board", e.to_string()))?;
        parsed
            .jobs
            .into_iter()
            .map(|job| Self::to_posting(&job, board))
            .collect()
    }

    fn find_detail(
        body: &str,
        board: &BoardConfig,
        req_id: &ReqId,
    ) -> Result<PostingDetail, AdapterError> {
        let parsed: JobBoard = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("ashby job board", e.to_string()))?;
        let job = parsed
            .jobs
            .into_iter()
            .find(|j| j.id == req_id.as_str())
            .ok_or_else(|| AdapterError::PostingNotFound(req_id.clone()))?;
        let description_html = job.description_html.clone();
        let description_text = job.description_plain.clone();
        let posting = Self::to_posting(&job, board)?;
        Ok(PostingDetail {
            posting,
            description_html,
            description_text,
        })
    }

    fn to_posting(job: &Job, board: &BoardConfig) -> Result<Posting, AdapterError> {
        let locations: Vec<String> = job.location.iter().cloned().collect();
        // workplaceType is the truth; isRemote is deliberately never read.
        let workplace_type = map_workplace(job.workplace_type.as_deref());
        let comp = extract_comp(job.compensation.as_ref())?;
        let description = job
            .description_plain
            .as_deref()
            .or(job.description_html.as_deref())
            .unwrap_or_default();
        let hash = content_hash(&job.title, &locations, workplace_type, &comp, description);

        Ok(Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(job.id.clone()),
            title: job.title.clone(),
            url: job.job_url.clone(),
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            posted_at: parse::rfc3339("ashby publishedAt", job.published_at.as_deref())?,
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: job.department.clone(),
            employment_type: job.employment_type.clone(),
            content_hash: hash,
        })
    }
}

impl Adapter for AshbyAdapter {
    async fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> Result<Vec<Posting>, AdapterError> {
        let body = http.get_text(&Self::list_url(board.token.as_str())).await?;
        Self::parse_jobs(&body, board)
    }

    async fn detail(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
        req_id: &ReqId,
    ) -> Result<PostingDetail, AdapterError> {
        // No single-job endpoint on Ashby; re-fetch the board and filter.
        let body = http.get_text(&Self::list_url(board.token.as_str())).await?;
        Self::find_detail(&body, board, req_id)
    }
}

fn map_workplace(value: Option<&str>) -> WorkplaceType {
    match value {
        Some("Remote") => WorkplaceType::Remote,
        Some("Hybrid") => WorkplaceType::Hybrid,
        Some("OnSite") => WorkplaceType::Onsite,
        _ => WorkplaceType::Unknown,
    }
}

/// Pull a typed [`Comp`] from Ashby's structured components. Only the `Salary` entry is
/// used — equity is out of the v0.1 model — and everything crosses into minor units via
/// [`decimal_to_minor`], never a float. A malformed currency or an unrecognized interval
/// is [`AdapterError::ParseDrift`], not a guess.
fn extract_comp(compensation: Option<&Compensation>) -> Result<Comp, AdapterError> {
    let Some(comp) = compensation else {
        return Ok(Comp::None);
    };
    let Some(salary) = comp
        .summary_components
        .iter()
        .find(|c| c.compensation_type.as_deref() == Some("Salary") && c.min_value.is_some())
    else {
        return Ok(Comp::None);
    };

    let currency = match salary.currency_code.as_deref() {
        Some(code) => Currency::new(code)
            .map_err(|e| AdapterError::drift("ashby compensation.currencyCode", e.to_string()))?,
        None => return Ok(Comp::None),
    };
    let exp = currency.minor_unit_exponent();
    let interval = parse::interval("ashby compensation.interval", salary.interval.as_deref())?;

    let min = parse::number_to_minor(
        "ashby compensation.minValue",
        salary.min_value.as_ref(),
        exp,
    )?;
    let Some(min_minor) = min else {
        return Ok(Comp::None);
    };
    let max_minor = parse::number_to_minor(
        "ashby compensation.maxValue",
        salary.max_value.as_ref(),
        exp,
    )?
    .unwrap_or(min_minor);

    if max_minor == min_minor {
        Comp::point(currency, min_minor, interval, CompSource::Api)
    } else {
        Comp::band(currency, min_minor, max_minor, interval, CompSource::Api)
    }
    .map_err(|e| AdapterError::drift("ashby compensation", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, BoardId, CompInterval};

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("ramp"),
            ats: Ats::Ashby,
            token: AtsToken::new("ramp"),
            site: None,
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    #[test]
    fn parses_the_real_fixture() {
        let postings =
            AshbyAdapter::parse_jobs(include_str!("fixtures/ashby_jobs.json"), &board()).unwrap();
        assert_eq!(postings.len(), 2);
        let remote = &postings[0];
        assert_eq!(remote.ats, Ats::Ashby);
        assert_eq!(
            remote.req_id,
            ReqId::new("03e2d4e1-73ad-4f09-a058-2eb9ce34c2bc")
        );
        // workplaceType is the truth.
        assert_eq!(remote.workplace_type, WorkplaceType::Remote);
        assert_eq!(remote.employment_type.as_deref(), Some("FullTime"));
        assert!(remote.posted_at.is_some());
    }

    #[test]
    fn extracts_structured_comp_as_integer_minor_units() {
        let postings =
            AshbyAdapter::parse_jobs(include_str!("fixtures/ashby_jobs.json"), &board()).unwrap();
        // $151K–$231K/yr USD → integer minor units, no float in the pipeline.
        assert_eq!(
            postings[0].comp,
            Comp::band(
                Currency::new("USD").unwrap(),
                15_100_000, // $151,000.00 in cents
                23_100_000, // $231,000.00 in cents
                CompInterval::Year,
                CompSource::Api,
            )
            .unwrap()
        );
    }

    #[test]
    fn onsite_maps_workplace_and_may_lack_salary() {
        let postings =
            AshbyAdapter::parse_jobs(include_str!("fixtures/ashby_jobs.json"), &board()).unwrap();
        assert_eq!(postings[1].workplace_type, WorkplaceType::Onsite);
    }

    #[test]
    fn detail_filters_to_the_req_and_carries_description() {
        let detail = AshbyAdapter::find_detail(
            include_str!("fixtures/ashby_jobs.json"),
            &board(),
            &ReqId::new("03e2d4e1-73ad-4f09-a058-2eb9ce34c2bc"),
        )
        .unwrap();
        assert!(detail.description_html.is_some());
        assert!(detail.description_text.is_some());
    }

    #[test]
    fn workplace_mapping_reads_workplace_type_not_is_remote() {
        assert_eq!(map_workplace(Some("Hybrid")), WorkplaceType::Hybrid);
        assert_eq!(map_workplace(Some("OnSite")), WorkplaceType::Onsite);
        assert_eq!(map_workplace(Some("something-new")), WorkplaceType::Unknown);
    }

    #[test]
    fn a_salary_component_with_null_min_is_absent_not_present_empty() {
        // Linear's shouldDisplayCompensationOnJobPostings=false (and OpenAI's unfilled
        // tiers) yield a Salary component with null min/max. That's "no published band"
        // → Comp::None, never a broken zero band.
        let body = r#"{"jobs":[{
            "id":"x","title":"t","jobUrl":"http://x","workplaceType":"Remote",
            "compensation":{"summaryComponents":[
                {"compensationType":"Salary","minValue":null,"maxValue":null,"currencyCode":"USD","interval":"1 YEAR"}
            ]}
        }]}"#;
        let postings = AshbyAdapter::parse_jobs(body, &board()).unwrap();
        assert_eq!(postings[0].comp, Comp::None);
    }

    #[test]
    fn a_changed_shape_is_parse_drift() {
        let broken = r#"{"jobs":[{"title":"x","jobUrl":"http://x"}]}"#; // missing id
        assert!(matches!(
            AshbyAdapter::parse_jobs(broken, &board()).unwrap_err(),
            AdapterError::ParseDrift { .. }
        ));
    }
}
