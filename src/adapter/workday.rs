//! Workday job-board adapter.
//!
//! Endpoint: `{host}/wday/cxs/{tenant}/{site}/jobs`, a POST search that pages through
//! `{appliedFacets, limit, offset, searchText}`. The board's config `token` is the API
//! HOST (e.g. `nvidia.wd5.myworkdayjobs.com`) and `site` is the career-site id; the
//! tenant is the host's first label.
//!
//! Quirks this adapter owns:
//! - **The list is thin.** It carries title, a req_id (in `bulletFields`), an
//!   `externalPath`, and a `locationsText` that is often a SUMMARY ("3 Locations") rather
//!   than a place. The real locations, `startDate` and description live in the detail —
//!   so a list-derived posting hashes over what the list actually gives, and comp /
//!   workplace / a precise post date only come from `fetch_posting`.
//! - **`postedOn` is a relative human string** ("Posted Today"), useless as a date and
//!   un-parseable without a clock. `posted_at` is left `None` in the list; the detail's
//!   `startDate` is the real post date.
//! - **Detail is keyed by path, not req_id.** `fetch_posting` searches for the req_id to
//!   recover its `externalPath`, then fetches the detail — two requests, but stateless.
//! - **Maintenance mode** returns a non-200 or non-JSON body, which surfaces as
//!   `BoardUnreachable` / `ParseDrift` — never a spuriously empty board.

use serde::Deserialize;
use serde_json::json;

use super::parse;
use super::{Adapter, AdapterError, ListResult};
use crate::config::BoardConfig;
use crate::http::{FetchCtx, HttpClient};
use crate::model::{Comp, Equity, Posting, PostingDetail, ReqId, WorkplaceType, content_hash};

const PAGE_LIMIT: i64 = 20;
// A board with more than this many postings gets truncated rather than looping forever;
// the truncation is logged so it can't masquerade as full coverage.
const MAX_POSTINGS: usize = 10_000;

#[derive(Deserialize)]
struct JobsResponse {
    total: i64,
    #[serde(rename = "jobPostings", default)]
    job_postings: Vec<JobPosting>,
}

#[derive(Deserialize)]
struct JobPosting {
    // Optional because Workday emits STUB entries mid-publish — `{"bulletFields":["…"]}`
    // with no title or path yet. Requiring them here would fail serde on the whole page
    // and make a board with even one in-flight posting unfetchable; instead a stub is
    // tolerated and skipped (see `parse_page`).
    #[serde(default)]
    title: Option<String>,
    #[serde(rename = "externalPath", default)]
    external_path: Option<String>,
    #[serde(rename = "locationsText", default)]
    locations_text: Option<String>,
    #[serde(rename = "bulletFields", default)]
    bullet_fields: Vec<String>,
}

#[derive(Deserialize)]
struct DetailResponse {
    #[serde(rename = "jobPostingInfo")]
    job_posting_info: JobPostingInfo,
}

#[derive(Deserialize)]
struct JobPostingInfo {
    title: String,
    #[serde(rename = "jobReqId")]
    job_req_id: String,
    #[serde(rename = "jobDescription", default)]
    job_description: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(rename = "additionalLocations", default)]
    additional_locations: Vec<String>,
    #[serde(rename = "startDate", default)]
    start_date: Option<String>,
    #[serde(rename = "timeType", default)]
    time_type: Option<String>,
    #[serde(rename = "externalUrl", default)]
    external_url: Option<String>,
}

pub struct WorkdayAdapter;

impl WorkdayAdapter {
    fn tenant(host: &str) -> &str {
        host.split('.').next().unwrap_or(host)
    }

    fn jobs_url(host: &str, site: &str) -> String {
        let tenant = Self::tenant(host);
        format!("https://{host}/wday/cxs/{tenant}/{site}/jobs")
    }

    fn detail_url(host: &str, site: &str, external_path: &str) -> String {
        format!(
            "https://{host}/wday/cxs/{}/{site}{external_path}",
            Self::tenant(host)
        )
    }

    fn site(board: &BoardConfig) -> Result<&str, AdapterError> {
        board.site.as_deref().ok_or_else(|| {
            AdapterError::drift(
                "workday config",
                format!("board {} is missing the `site` setting", board.id),
            )
        })
    }

    /// Parse one search page into postings, the board `total`, and the req_ids of any
    /// STUB entries skipped. A stub is a posting mid-publish — Workday returns it with
    /// `bulletFields` (so it has a req id) but no `title`/`externalPath` yet. It is NOT a
    /// parse error: skipping it and recording its req keeps a live board fetchable while
    /// still surfacing the in-flight req (often a NEW about to land).
    fn parse_page(
        body: &str,
        board: &BoardConfig,
        host: &str,
        site: &str,
    ) -> Result<(Vec<Posting>, i64, Vec<String>), AdapterError> {
        let parsed: JobsResponse = serde_json::from_str(body)
            .map_err(|e| AdapterError::drift("workday jobs response", e.to_string()))?;
        let mut postings = Vec::new();
        let mut skipped = Vec::new();
        for jp in parsed.job_postings {
            let req_id = jp.bullet_fields.into_iter().next();
            let (Some(title), Some(external_path)) = (jp.title, jp.external_path) else {
                // Stub: no title/path yet. Keep the req id (it names what's being touched).
                let req = req_id.unwrap_or_else(|| "<unknown>".to_owned());
                tracing::warn!(
                    board = %board.id,
                    req = %req,
                    "workday stub posting (no title/externalPath yet, mid-publish); skipping"
                );
                skipped.push(req);
                continue;
            };
            // A titled posting with no req id is genuinely broken, not a stub — stay loud.
            let Some(req_id) = req_id else {
                return Err(AdapterError::drift(
                    "workday jobPosting",
                    "titled posting with no bulletFields (req id)",
                ));
            };
            let locations: Vec<String> = jp.locations_text.into_iter().collect();
            let comp = Comp::None;
            let hash = content_hash(
                &title,
                &locations,
                WorkplaceType::Unknown,
                &comp,
                Equity::None,
                "",
            );
            postings.push(Posting {
                ats: board.ats,
                board_id: board.id.clone(),
                req_id: ReqId::new(req_id),
                title,
                url: format!("https://{host}/{site}{external_path}"),
                locations,
                workplace_type: WorkplaceType::Unknown,
                remote_scope: None,
                comp,
                equity: Equity::None,
                posted_at: None, // list `postedOn` is relative text; the real date is in detail
                updated_at: None,
                updated_at_unreliable: board.updated_at_unreliable,
                department: None,
                employment_type: None,
                content_hash: hash,
            });
        }
        Ok((postings, parsed.total, skipped))
    }

    fn detail_from(
        info: JobPostingInfo,
        board: &BoardConfig,
    ) -> Result<PostingDetail, AdapterError> {
        let mut locations = Vec::new();
        locations.extend(info.location.clone());
        locations.extend(info.additional_locations.clone());
        let workplace_type = infer_workplace(&locations);
        let comp = Comp::None;
        let description = info.job_description.clone().unwrap_or_default();
        let hash = content_hash(
            &info.title,
            &locations,
            workplace_type,
            &comp,
            Equity::None,
            &description,
        );

        let posting = Posting {
            ats: board.ats,
            board_id: board.id.clone(),
            req_id: ReqId::new(info.job_req_id),
            title: info.title,
            // A detail with no externalUrl is a posting you can't apply to — drift or a
            // pulled req, not a usable posting. Stay loud rather than return url: "" (the
            // list path is already hard on a titled posting missing its req id).
            url: info.external_url.ok_or_else(|| {
                AdapterError::drift(
                    "workday jobPostingInfo",
                    "detail response has no externalUrl",
                )
            })?,
            locations,
            workplace_type,
            remote_scope: None,
            comp,
            equity: Equity::None,
            posted_at: parse::date("workday startDate", info.start_date.as_deref())?,
            updated_at: None,
            updated_at_unreliable: board.updated_at_unreliable,
            department: None,
            employment_type: info.time_type,
            content_hash: hash,
        };
        let description_html = info.job_description;
        Ok(PostingDetail {
            description_text: description_html.as_deref().map(parse::strip_tags),
            description_html,
            posting,
        })
    }
}

impl Adapter for WorkdayAdapter {
    async fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> Result<ListResult, AdapterError> {
        let host = board.token.as_str();
        let site = Self::site(board)?;
        let url = Self::jobs_url(host, site);

        let mut postings = Vec::new();
        let mut skipped = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let body = http
                .post_json(
                    &url,
                    &json!({ "appliedFacets": {}, "limit": PAGE_LIMIT, "offset": offset, "searchText": "" }),
                    &FetchCtx::from_board(board),
                )
                .await?;
            let (page, total, page_skipped) = Self::parse_page(&body, board, host, site)?;
            // Decide "another page?" on the RAW page size (postings + skipped stubs) — a
            // page that is ALL stubs still has entries, and more real ones may follow.
            let page_raw = page.len() + page_skipped.len();
            postings.extend(page);
            skipped.extend(page_skipped);
            offset += PAGE_LIMIT;
            if page_raw == 0 || offset >= total {
                break;
            }
            if postings.len() >= MAX_POSTINGS {
                tracing::warn!(
                    board = %board.id,
                    total,
                    collected = postings.len(),
                    "workday board exceeds MAX_POSTINGS; truncating"
                );
                break;
            }
        }
        Ok(ListResult { postings, skipped })
    }

    async fn detail(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
        req_id: &ReqId,
    ) -> Result<PostingDetail, AdapterError> {
        let host = board.token.as_str();
        let site = Self::site(board)?;

        // Detail is keyed by externalPath, not req_id — search for the req to recover it.
        let search = http
            .post_json(
                &Self::jobs_url(host, site),
                &json!({ "appliedFacets": {}, "limit": PAGE_LIMIT, "offset": 0, "searchText": req_id.as_str() }),
                &FetchCtx::from_board(board),
            )
            .await?;
        let found: JobsResponse = serde_json::from_str(&search)
            .map_err(|e| AdapterError::drift("workday search response", e.to_string()))?;
        let external_path = found
            .job_postings
            .into_iter()
            .find(|jp| jp.bullet_fields.first().map(String::as_str) == Some(req_id.as_str()))
            .and_then(|jp| jp.external_path)
            .ok_or_else(|| AdapterError::PostingNotFound(req_id.clone()))?;

        let body = http
            .get_text(
                &Self::detail_url(host, site, &external_path),
                &FetchCtx::from_board(board),
            )
            .await?;
        let parsed: DetailResponse = serde_json::from_str(&body)
            .map_err(|e| AdapterError::drift("workday job detail", e.to_string()))?;
        Self::detail_from(parsed.job_posting_info, board)
    }
}

fn infer_workplace(locations: &[String]) -> WorkplaceType {
    if locations
        .iter()
        .any(|l| l.to_lowercase().contains("remote"))
    {
        WorkplaceType::Remote
    } else {
        WorkplaceType::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, BoardId};

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("nvidia"),
            ats: Ats::Workday,
            token: AtsToken::new("nvidia.wd5.myworkdayjobs.com"),
            site: Some("NVIDIAExternalCareerSite".to_owned()),
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    /// A titled detail with no externalUrl used to become a `url: ""` posting flowed into the
    /// store (H.3). It must be `ParseDrift` — a posting you can't apply to is drift or a pulled
    /// req, not usable data.
    #[test]
    fn a_detail_with_no_external_url_is_drift_not_an_empty_url() {
        let info = JobPostingInfo {
            title: "Staff Engineer".to_owned(),
            job_req_id: "JR123".to_owned(),
            job_description: None,
            location: None,
            additional_locations: Vec::new(),
            start_date: None,
            time_type: None,
            external_url: None,
        };
        let err = WorkdayAdapter::detail_from(info, &board())
            .expect_err("a detail with no externalUrl must be drift, not a url:\"\" posting");
        assert!(
            matches!(err, AdapterError::ParseDrift { .. }),
            "expected ParseDrift, got {err:?}"
        );
    }

    #[test]
    fn parses_a_real_list_page() {
        let (postings, total, skipped) = WorkdayAdapter::parse_page(
            include_str!("fixtures/workday_jobs.json"),
            &board(),
            "nvidia.wd5.myworkdayjobs.com",
            "NVIDIAExternalCareerSite",
        )
        .unwrap();
        assert_eq!(total, 2000); // paginate: the list is only a page of the whole board
        assert_eq!(postings.len(), 2);
        assert!(skipped.is_empty(), "the real fixture has no stubs");
        let p = &postings[0];
        assert_eq!(p.ats, Ats::Workday);
        assert_eq!(p.req_id, ReqId::new("JR1998928"));
        assert_eq!(p.title, "ASIC Design Efficiency Engineer");
        assert_eq!(
            p.url,
            "https://nvidia.wd5.myworkdayjobs.com/NVIDIAExternalCareerSite/job/US-CA-Santa-Clara/ASIC-Design-Efficiency-Engineer_JR1998928"
        );
        // locationsText is a summary, carried verbatim; the real dates/locations are detail.
        assert_eq!(p.locations, vec!["3 Locations".to_owned()]);
        assert_eq!(p.posted_at, None);
    }

    #[test]
    fn detail_carries_start_date_and_real_locations() {
        let detail = WorkdayAdapter::detail_from(
            serde_json::from_str::<DetailResponse>(include_str!("fixtures/workday_detail.json"))
                .unwrap()
                .job_posting_info,
            &board(),
        )
        .unwrap();
        assert_eq!(detail.posting.req_id, ReqId::new("JR1998928"));
        // startDate is the post date.
        assert!(detail.posting.posted_at.is_some());
        // location + additionalLocations, not the "3 Locations" summary.
        assert_eq!(detail.posting.locations.len(), 3);
        assert_eq!(detail.posting.employment_type.as_deref(), Some("Full time"));
        assert!(detail.description_html.is_some());
    }

    #[test]
    fn a_missing_site_is_a_loud_config_error() {
        let mut b = board();
        b.site = None;
        assert!(matches!(
            WorkdayAdapter::site(&b),
            Err(AdapterError::ParseDrift { .. })
        ));
    }

    #[test]
    fn a_changed_shape_is_parse_drift() {
        // A per-entry stub (no title) is now tolerated, so drift is a broken RESPONSE
        // shape — here `total` is the wrong type, which no page should ever carry.
        let broken = r#"{"total":"lots","jobPostings":[]}"#;
        assert!(matches!(
            WorkdayAdapter::parse_page(broken, &board(), "h", "s").unwrap_err(),
            AdapterError::ParseDrift { .. }
        ));
    }

    #[test]
    fn a_stub_posting_is_skipped_not_fatal() {
        // Disney's real failure: a page with a live posting AND a mid-publish stub
        // (`{"bulletFields":["10151065"]}`, no title/path). The real one parses, the stub
        // is skipped with its req surfaced, and the board stays fetchable.
        let body = r#"{"total":2,"jobPostings":[
            {"title":"Real Role","externalPath":"/job/Real_JR1","bulletFields":["JR1"]},
            {"bulletFields":["10151065"]}
        ]}"#;
        let (postings, total, skipped) =
            WorkdayAdapter::parse_page(body, &board(), "h.wd5.myworkdayjobs.com", "Site").unwrap();
        assert_eq!(total, 2);
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].req_id, ReqId::new("JR1"));
        assert_eq!(skipped, vec!["10151065".to_owned()]);
    }
}
