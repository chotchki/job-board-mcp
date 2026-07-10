//! Workable job-board adapter.
//!
//! List endpoint: `POST apply.workable.com/api/v3/accounts/{token}/jobs` with a JSON
//! filter body — the request the careers SPA makes, returning the published jobs with a
//! keyset cursor (`nextPage`, resent as `token`). Detail uses the widget endpoint
//! (`GET .../api/v1/widget/accounts/{token}?details=true`), which carries the description
//! the list omits.
//!
//! Quirks this adapter owns:
//! - **`workplace` is a clean enum** (`remote` / `hybrid` / `on_site`), read directly.
//! - **A dead-but-known board returns `200` with `total: 0`** — the same silent-migration
//!   shape Lever has (Future plc lives; several probed slugs had migrated off). The
//!   empty-vs-migrated call is left to the diff layer, which sees the previous snapshot.
//! - **`department` is an array**, joined; `locations[]` may carry several, `hidden` ones
//!   dropped.

use serde::Deserialize;
use serde_json::json;

use super::parse;
use super::{Adapter, AdapterError};
use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{Comp, Posting, PostingDetail, ReqId, WorkplaceType, content_hash};

const MAX_POSTINGS: usize = 10_000;

#[derive(Deserialize)]
struct JobsResponse {
    #[serde(default)]
    results: Vec<Job>,
    #[serde(rename = "nextPage", default)]
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct Job {
    shortcode: String,
    title: String,
    #[serde(default)]
    workplace: Option<String>,
    #[serde(default)]
    published: Option<String>,
    #[serde(default)]
    department: Vec<String>,
    #[serde(rename = "type", default)]
    job_type: Option<String>,
    #[serde(default)]
    locations: Vec<Location>,
}

#[derive(Deserialize)]
struct Location {
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    hidden: bool,
}

#[derive(Deserialize)]
struct WidgetResponse {
    #[serde(default)]
    jobs: Vec<WidgetJob>,
}

#[derive(Deserialize)]
struct WidgetJob {
    shortcode: String,
    #[serde(default)]
    description: Option<String>,
}

pub struct WorkableAdapter;

impl WorkableAdapter {
    fn list_url(token: &str) -> String {
        format!("https://apply.workable.com/api/v3/accounts/{token}/jobs")
    }

    fn widget_url(token: &str) -> String {
        format!("https://apply.workable.com/api/v1/widget/accounts/{token}?details=true")
    }

    fn parse_page(
        body: &str,
        board: &BoardConfig,
    ) -> Result<(Vec<Posting>, Option<String>), AdapterError> {
        let parsed: JobsResponse = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("workable jobs", e.to_string()))?;
        let postings = parsed
            .results
            .into_iter()
            .map(|j| Self::to_posting(j, board))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((postings, parsed.next_page))
    }

    fn to_posting(job: Job, board: &BoardConfig) -> Result<Posting, AdapterError> {
        let locations: Vec<String> = job
            .locations
            .into_iter()
            .filter(|l| !l.hidden)
            .map(|l| {
                [l.city, l.region, l.country]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|s| !s.is_empty())
            .collect();
        let workplace_type = map_workplace(job.workplace.as_deref());
        let comp = Comp::None;
        let hash = content_hash(&job.title, &locations, workplace_type, &comp, "");

        Ok(Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(job.shortcode.clone()),
            title: job.title,
            url: format!(
                "https://apply.workable.com/{}/j/{}/",
                board.token.as_str(),
                job.shortcode
            ),
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            posted_at: parse::rfc3339("workable published", job.published.as_deref())?,
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: (!job.department.is_empty()).then(|| job.department.join(", ")),
            employment_type: job.job_type,
            content_hash: hash,
        })
    }
}

impl Adapter for WorkableAdapter {
    async fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> Result<Vec<Posting>, AdapterError> {
        let url = Self::list_url(board.token.as_str());
        let mut postings = Vec::new();
        let mut token: Option<String> = None;
        loop {
            // The SPA's filter body; add the cursor on later pages.
            let mut body = json!({
                "query": "", "location": [], "department": [], "worktype": [], "remote": []
            });
            if let Some(t) = &token {
                body["token"] = json!(t);
            }
            let text = http.post_json(&url, &body).await?;
            let (page, next) = Self::parse_page(&text, board)?;
            let page_len = page.len();
            postings.extend(page);
            match next {
                Some(next) if page_len > 0 && postings.len() < MAX_POSTINGS => token = Some(next),
                _ => break,
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
        // The v3 list omits the description; the widget endpoint carries it. Fetch the
        // board's current listing for the posting fields, then graft the description on.
        let list_body = http
            .post_json(
                &Self::list_url(board.token.as_str()),
                &json!({
                    "query": "", "location": [], "department": [], "worktype": [], "remote": []
                }),
            )
            .await?;
        let (postings, _) = Self::parse_page(&list_body, board)?;
        let posting = postings
            .into_iter()
            .find(|p| p.req_id == *req_id)
            .ok_or_else(|| AdapterError::UnknownBoard(board.id.clone()))?;

        let widget = http
            .get_text(&Self::widget_url(board.token.as_str()))
            .await?;
        let parsed: WidgetResponse = serde_json::from_str(&widget)
            .map_err(|e| AdapterError::drift("workable widget", e.to_string()))?;
        let description_html = parsed
            .jobs
            .into_iter()
            .find(|j| j.shortcode == req_id.as_str())
            .and_then(|j| j.description);

        Ok(PostingDetail {
            posting,
            description_text: description_html.as_deref().map(parse::strip_tags),
            description_html,
        })
    }
}

fn map_workplace(value: Option<&str>) -> WorkplaceType {
    match value {
        Some("remote") => WorkplaceType::Remote,
        Some("hybrid") => WorkplaceType::Hybrid,
        Some("on_site") => WorkplaceType::Onsite,
        _ => WorkplaceType::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, BoardId};

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("futureplc"),
            ats: Ats::Workable,
            token: AtsToken::new("futureplc"),
            site: None,
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    #[test]
    fn parses_the_real_fixture() {
        let (postings, next) =
            WorkableAdapter::parse_page(include_str!("fixtures/workable_jobs.json"), &board())
                .unwrap();
        assert_eq!(postings.len(), 2);
        assert!(next.is_none()); // fixture's nextPage is null
        let p = &postings[0];
        assert_eq!(p.ats, Ats::Workable);
        assert_eq!(p.req_id, ReqId::new("40D624356F"));
        assert_eq!(p.title, "Advertising Sales Manager - The Week");
        assert_eq!(p.url, "https://apply.workable.com/futureplc/j/40D624356F/");
        assert_eq!(
            p.locations,
            vec!["London, England, United Kingdom".to_owned()]
        );
        // workplace enum read directly.
        assert_eq!(p.workplace_type, WorkplaceType::Hybrid);
        assert_eq!(p.department.as_deref(), Some("Revenue - Commercial"));
        assert!(p.posted_at.is_some());
    }

    #[test]
    fn workplace_maps_the_enum() {
        assert_eq!(map_workplace(Some("remote")), WorkplaceType::Remote);
        assert_eq!(map_workplace(Some("on_site")), WorkplaceType::Onsite);
        assert_eq!(map_workplace(Some("hybrid")), WorkplaceType::Hybrid);
        assert_eq!(map_workplace(None), WorkplaceType::Unknown);
    }

    #[test]
    fn a_changed_shape_is_parse_drift() {
        let broken = r#"{"results":[{"title":"x"}],"nextPage":null}"#; // no shortcode
        assert!(matches!(
            WorkableAdapter::parse_page(broken, &board()).unwrap_err(),
            AdapterError::ParseDrift { .. }
        ));
    }
}
