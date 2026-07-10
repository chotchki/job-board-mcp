//! Greenhouse job-board adapter.
//!
//! Endpoint: `boards-api.greenhouse.io/v1/boards/{token}/jobs?content=true`, plus
//! `/jobs/{id}` for a single posting's detail (same job shape, always with `content`).
//!
//! Quirks this adapter owns:
//! - **Comp is site-only.** Greenhouse's API almost never carries the band even when
//!   the company publishes it on its own site, so comp comes from the board's
//!   `comp_site_only` flag: `SiteOnly` when set, `None` otherwise. We do not scrape a
//!   number out of the description body.
//! - **Hosted-URL variants** (`job-boards.` vs `boards.` vs company-hosted) are a
//!   non-issue because `absolute_url` is authoritative — we never reconstruct it.
//! - **Workplace type isn't a field.** We infer `Remote` only when the location name
//!   literally says "remote", and `Unknown` otherwise — a narrow rule that never
//!   asserts a workplace type the data doesn't support.

use serde::Deserialize;

use super::{Adapter, AdapterError};
use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{Comp, Posting, PostingDetail, ReqId, WorkplaceType, content_hash};

#[derive(Deserialize)]
struct JobsResponse {
    jobs: Vec<Job>,
}

// Required fields carry no default — if greenhouse drops or renames one, serde fails
// and we surface ParseDrift rather than invent a value. Genuinely-optional fields
// default.
#[derive(Deserialize)]
struct Job {
    id: i64,
    title: String,
    absolute_url: String,
    #[serde(default)]
    location: Option<Location>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    first_published: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    departments: Vec<Department>,
}

#[derive(Deserialize)]
struct Location {
    name: String,
}

#[derive(Deserialize)]
struct Department {
    name: String,
}

pub struct GreenhouseAdapter;

impl GreenhouseAdapter {
    fn list_url(token: &str) -> String {
        format!("https://boards-api.greenhouse.io/v1/boards/{token}/jobs?content=true")
    }

    fn detail_url(token: &str, req_id: &ReqId) -> String {
        format!("https://boards-api.greenhouse.io/v1/boards/{token}/jobs/{req_id}")
    }

    fn parse_jobs(body: &str, board: &BoardConfig) -> Result<Vec<Posting>, AdapterError> {
        let parsed: JobsResponse = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("greenhouse jobs response", e.to_string()))?;
        parsed
            .jobs
            .into_iter()
            .map(|job| Self::to_posting(job, board))
            .collect()
    }

    fn parse_detail(body: &str, board: &BoardConfig) -> Result<PostingDetail, AdapterError> {
        let job: Job = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("greenhouse job detail", e.to_string()))?;
        let description_html = job.content.clone().map(|c| unescape_html(&c));
        let description_text = description_html.as_deref().map(strip_tags);
        let posting = Self::to_posting(job, board)?;
        Ok(PostingDetail {
            posting,
            description_html,
            description_text,
        })
    }

    fn to_posting(job: Job, board: &BoardConfig) -> Result<Posting, AdapterError> {
        let locations: Vec<String> = job.location.map(|l| vec![l.name]).unwrap_or_default();
        let workplace_type = infer_workplace(locations.first().map(String::as_str));
        let comp = if board.comp_site_only {
            Comp::SiteOnly
        } else {
            Comp::None
        };
        let description = job.content.as_deref().unwrap_or_default();
        let hash = content_hash(&job.title, &locations, workplace_type, &comp, description);

        Ok(Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(job.id.to_string()),
            title: job.title,
            url: job.absolute_url,
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            posted_at: super::parse::rfc3339(
                "greenhouse first_published",
                job.first_published.as_deref(),
            )?,
            updated_at: super::parse::rfc3339("greenhouse updated_at", job.updated_at.as_deref())?,
            updated_at_unreliable: board.updated_at_unreliable,
            department: job.departments.into_iter().next().map(|d| d.name),
            employment_type: None,
            content_hash: hash,
        })
    }
}

impl Adapter for GreenhouseAdapter {
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
        let body = http
            .get_text(&Self::detail_url(board.token.as_str(), req_id))
            .await?;
        Self::parse_detail(&body, board)
    }
}

/// Greenhouse escapes its `content` HTML for embedding (`&lt;div&gt;`); undo one layer
/// to recover the real markup.
fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn infer_workplace(location: Option<&str>) -> WorkplaceType {
    match location {
        Some(name) if name.to_lowercase().contains("remote") => WorkplaceType::Remote,
        _ => WorkplaceType::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, BoardId};

    fn board(comp_site_only: bool) -> BoardConfig {
        BoardConfig {
            id: BoardId::new("gitlab"),
            ats: Ats::Greenhouse,
            token: AtsToken::new("gitlab"),
            comp_site_only,
            updated_at_unreliable: false,
        }
    }

    #[test]
    fn parses_the_real_fixture() {
        let postings = GreenhouseAdapter::parse_jobs(
            include_str!("fixtures/greenhouse_jobs.json"),
            &board(false),
        )
        .unwrap();
        assert_eq!(postings.len(), 2);
        let p = &postings[0];
        assert_eq!(p.ats, Ats::Greenhouse);
        assert_eq!(p.req_id, ReqId::new("8503792002"));
        assert_eq!(p.title, "Account Executive - Italy");
        assert_eq!(
            p.url,
            "https://job-boards.greenhouse.io/gitlab/jobs/8503792002"
        );
        assert_eq!(p.locations, vec!["Remote, Italy".to_owned()]);
        // "Remote, Italy" contains "remote" → Remote; a non-remote location would be Unknown.
        assert_eq!(p.workplace_type, WorkplaceType::Remote);
        assert_eq!(p.department.as_deref(), Some("EMEA - Commercial"));
        assert!(p.posted_at.is_some());
        assert!(p.updated_at.is_some());
    }

    #[test]
    fn comp_follows_the_site_only_flag() {
        let none = GreenhouseAdapter::parse_jobs(
            include_str!("fixtures/greenhouse_jobs.json"),
            &board(false),
        )
        .unwrap();
        assert_eq!(none[0].comp, Comp::None);

        let site = GreenhouseAdapter::parse_jobs(
            include_str!("fixtures/greenhouse_jobs.json"),
            &board(true),
        )
        .unwrap();
        assert_eq!(site[0].comp, Comp::SiteOnly);
    }

    #[test]
    fn detail_carries_description_text() {
        let detail = GreenhouseAdapter::parse_detail(
            include_str!("fixtures/greenhouse_detail.json"),
            &board(false),
        )
        .unwrap();
        assert_eq!(detail.posting.req_id, ReqId::new("8503792002"));
        assert!(detail.description_html.as_deref().unwrap().contains('<'));
        // Tags stripped, entities resolved.
        assert!(!detail.description_text.as_deref().unwrap().contains('<'));
    }

    #[test]
    fn a_changed_shape_is_parse_drift_not_a_guess() {
        // A job missing the required `id` must fail loud, never default to something.
        let broken = r#"{"jobs":[{"title":"x","absolute_url":"http://x"}]}"#;
        let err = GreenhouseAdapter::parse_jobs(broken, &board(false)).unwrap_err();
        assert!(
            matches!(err, AdapterError::ParseDrift { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn workplace_inference_is_narrow() {
        assert_eq!(infer_workplace(Some("Remote, US")), WorkplaceType::Remote);
        assert_eq!(infer_workplace(Some("New York")), WorkplaceType::Unknown);
        assert_eq!(infer_workplace(None), WorkplaceType::Unknown);
    }
}
