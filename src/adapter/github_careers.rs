//! github.careers adapter — GitHub's own careers board (a board-specific adapter, not a
//! multi-tenant platform), so the config `token` is ignored.
//!
//! Endpoint: `www.github.careers/api/jobs?page={n}&limit=100`, returning
//! `{jobs: [{data: {...}}], totalCount}`.
//!
//! Quirks this adapter owns:
//! - **The `?query=` param is ignored server-side**, so client-side filtering would be a
//!   lie — we just fetch every page and diff the whole board. That also means the
//!   companion trap — the rendered HTML page embeds a no-results i18n string that defeats
//!   naive grepping — never touches us, because we read the JSON API, not the page.
//! - **The list already carries the description**, so it feeds `content_hash` (a JD edit
//!   is a real CHANGED) and `fetch_posting` filters the list rather than fetching again.
//! - **`posted_date` uses a colon-less offset** (`+0000`), handled by the lenient parser.

use serde::Deserialize;

use super::parse;
use super::{Adapter, AdapterError};
use crate::config::BoardConfig;
use crate::http::{FetchCtx, HttpClient};
use crate::model::{
    Comp, CompInterval, CompSource, Currency, Equity, Posting, PostingDetail, ReqId, WorkplaceType,
    content_hash,
};

const PAGE_LIMIT: i64 = 100;
const MAX_POSTINGS: usize = 10_000;

#[derive(Deserialize)]
struct JobsResponse {
    #[serde(default)]
    jobs: Vec<Job>,
    #[serde(rename = "totalCount", default)]
    total_count: i64,
}

#[derive(Deserialize)]
struct Job {
    data: JobData,
}

#[derive(Deserialize)]
struct JobData {
    slug: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    location_name: Option<String>,
    #[serde(default)]
    posted_date: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    apply_url: Option<String>,
    #[serde(default)]
    salary_min_value: Option<serde_json::Number>,
    #[serde(default)]
    salary_max_value: Option<serde_json::Number>,
}

pub struct GithubCareersAdapter;

impl GithubCareersAdapter {
    fn jobs_url(page: i64) -> String {
        format!("https://www.github.careers/api/jobs?page={page}&limit={PAGE_LIMIT}")
    }

    fn parse_page(body: &str, board: &BoardConfig) -> Result<(Vec<Posting>, i64), AdapterError> {
        let parsed: JobsResponse = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("github.careers jobs", e.to_string()))?;
        let total = parsed.total_count;
        let postings = parsed
            .jobs
            .into_iter()
            .map(|j| Self::to_posting(j.data, board))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((postings, total))
    }

    fn to_posting(data: JobData, board: &BoardConfig) -> Result<Posting, AdapterError> {
        let locations: Vec<String> = data.location_name.clone().into_iter().collect();
        let workplace_type = infer_workplace(&locations);
        let comp = github_comp(
            data.salary_min_value.as_ref(),
            data.salary_max_value.as_ref(),
        )?;
        // The description is in the list, so it counts toward the change key.
        let description = data.description.clone().unwrap_or_default();
        let hash = content_hash(
            &data.title,
            &locations,
            workplace_type,
            &comp,
            Equity::None,
            &description,
        );

        Ok(Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(data.slug.clone()),
            title: data.title,
            url: data
                .apply_url
                .unwrap_or_else(|| format!("https://www.github.careers/jobs/{}", data.slug)),
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            equity: Equity::None,
            posted_at: parse::datetime_lenient(
                "github.careers posted_date",
                data.posted_date.as_deref(),
            )?,
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: data.department.filter(|d| !d.is_empty()),
            employment_type: None,
            content_hash: hash,
        })
    }
}

impl Adapter for GithubCareersAdapter {
    async fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> Result<Vec<Posting>, AdapterError> {
        let mut postings = Vec::new();
        let mut page: i64 = 1;
        loop {
            let body = http
                .get_text(&Self::jobs_url(page), &FetchCtx::from_board(board))
                .await?;
            let (batch, total) = Self::parse_page(&body, board)?;
            let batch_len = batch.len();
            postings.extend(batch);
            if batch_len == 0 || postings.len() as i64 >= total {
                break;
            }
            page += 1;
            if postings.len() >= MAX_POSTINGS {
                tracing::warn!(
                    collected = postings.len(),
                    "github.careers exceeds MAX_POSTINGS; truncating"
                );
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
        // The list carries descriptions and there's no per-job endpoint worth a second
        // shape, so page through and filter to the req.
        let mut page: i64 = 1;
        loop {
            let body = http
                .get_text(&Self::jobs_url(page), &FetchCtx::from_board(board))
                .await?;
            let parsed: JobsResponse = serde_json::from_str(&body)
                .map_err(|e| AdapterError::drift("github.careers jobs", e.to_string()))?;
            let total = parsed.total_count;
            let mut seen = 0;
            for job in parsed.jobs {
                seen += 1;
                if job.data.slug == req_id.as_str() {
                    let description_html = job.data.description.clone();
                    let posting = Self::to_posting(job.data, board)?;
                    return Ok(PostingDetail {
                        posting,
                        description_text: description_html.as_deref().map(parse::strip_tags),
                        description_html,
                    });
                }
            }
            if seen == 0 || (page * PAGE_LIMIT) >= total {
                return Err(AdapterError::PostingNotFound(req_id.clone()));
            }
            page += 1;
        }
    }
}

/// github.careers exposes `salary_min_value`/`salary_max_value` but no currency or
/// interval. This is GitHub's OWN single US board, so those are annual USD — a documented
/// board-specific fact, not a guess in the wild. Zero means "not published" → `None`.
fn github_comp(
    min: Option<&serde_json::Number>,
    max: Option<&serde_json::Number>,
) -> Result<Comp, AdapterError> {
    let usd = Currency::new("USD").expect("USD is valid");
    let min = parse::number_to_minor("github salary_min_value", min, usd.minor_unit_exponent())?
        .filter(|&v| v > 0);
    let max = parse::number_to_minor("github salary_max_value", max, usd.minor_unit_exponent())?
        .filter(|&v| v > 0);
    let comp = match (min, max) {
        (Some(lo), Some(hi)) => Comp::band(
            usd,
            lo.min(hi),
            lo.max(hi),
            CompInterval::Year,
            CompSource::Api,
        ),
        (Some(v), None) | (None, Some(v)) => {
            Comp::point(usd, v, CompInterval::Year, CompSource::Api)
        }
        (None, None) => return Ok(Comp::None),
    };
    comp.map_err(|e| AdapterError::drift("github.careers salary", e.to_string()))
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
            id: BoardId::new("github"),
            ats: Ats::GithubCareers,
            token: AtsToken::new("github"),
            site: None,
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    #[test]
    fn parses_the_real_fixture() {
        let (postings, total) = GithubCareersAdapter::parse_page(
            include_str!("fixtures/github_careers_jobs.json"),
            &board(),
        )
        .unwrap();
        assert_eq!(total, 73); // paginate: this page is only part of the board
        assert_eq!(postings.len(), 2);
        let p = &postings[0];
        assert_eq!(p.ats, Ats::GithubCareers);
        assert_eq!(p.req_id, ReqId::new("5572"));
        assert!(p.title.starts_with("Sr. Mgr"));
        assert_eq!(p.locations, vec!["US Remote".to_owned()]);
        // "US Remote" contains "remote".
        assert_eq!(p.workplace_type, WorkplaceType::Remote);
        // posted_date has a colon-less offset (+0000) and still parses.
        assert!(p.posted_at.is_some());
    }

    #[test]
    fn a_changed_shape_is_parse_drift() {
        let broken = r#"{"jobs":[{"data":{"title":"x"}}],"totalCount":1}"#; // no slug
        assert!(matches!(
            GithubCareersAdapter::parse_page(broken, &board()).unwrap_err(),
            AdapterError::ParseDrift { .. }
        ));
    }

    #[test]
    fn salary_is_harvested_when_present_and_zero_is_absent() {
        use crate::model::{CompInterval, CompSource, Currency};
        let n = |v: &str| serde_json::from_str::<serde_json::Number>(v).unwrap();
        assert_eq!(
            github_comp(Some(&n("150000")), Some(&n("200000"))).unwrap(),
            Comp::band(
                Currency::new("USD").unwrap(),
                15_000_000,
                20_000_000,
                CompInterval::Year,
                CompSource::Api
            )
            .unwrap()
        );
        // The board's current values are all 0 → not published → None, never a $0 band.
        assert_eq!(
            github_comp(Some(&n("0")), Some(&n("0"))).unwrap(),
            Comp::None
        );
        assert_eq!(github_comp(None, None).unwrap(), Comp::None);
    }
}
