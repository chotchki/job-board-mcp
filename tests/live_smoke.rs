//! Live smoke tests against real public boards. Every test here is `#[ignore]`d, so
//! `cargo test` (and therefore normal CI) never touches the network — they run only
//! under `cargo test -- --ignored`, which the weekly scheduled workflow does.
//!
//! Their job is to catch API-shape DRIFT: if a board renames a field the adapter treats
//! as required, `list` returns `ParseDrift` and the assertion below fails loudly. That's
//! the failure an `#[ignore]`d test would otherwise hide by never running — which is why
//! the scheduled job exists.
//!
//! The boards (gitlab / ramp / gopuff) are large and stable, but they're third parties:
//! if one empties out, that's worth knowing too (it likely moved off the ATS).

use job_board_mcp::adapter::{
    Adapter, AshbyAdapter, GithubCareersAdapter, GreenhouseAdapter, LeverAdapter, RipplingAdapter,
    SmartRecruitersAdapter, WorkableAdapter,
};
use job_board_mcp::config::BoardConfig;
use job_board_mcp::http::{FetchCtx, HttpClient, HttpConfig};
use job_board_mcp::model::{Ats, AtsToken, BoardId, Posting};

fn board(id: &str, ats: Ats, token: &str) -> BoardConfig {
    BoardConfig {
        id: BoardId::new(id),
        ats,
        token: AtsToken::new(token),
        site: None,
        comp_site_only: false,
        updated_at_unreliable: false,
    }
}

fn assert_well_formed(postings: &[Posting], board_id: &str) {
    assert!(
        !postings.is_empty(),
        "{board_id}: returned zero postings — the board may have moved off this ATS"
    );
    let p = &postings[0];
    assert!(!p.title.is_empty(), "{board_id}: empty title");
    assert!(
        p.url.starts_with("http"),
        "{board_id}: url is not a URL: {}",
        p.url
    );
    assert!(!p.req_id.as_str().is_empty(), "{board_id}: empty req_id");
}

#[tokio::test]
#[ignore = "hits the live greenhouse API; run with --ignored"]
async fn greenhouse_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("gitlab", Ats::Greenhouse, "gitlab");
    let postings = GreenhouseAdapter
        .list(&http, &board)
        .await
        .unwrap()
        .postings;
    assert_well_formed(&postings, "gitlab");

    // Exercise the detail path too.
    let detail = GreenhouseAdapter
        .detail(&http, &board, &postings[0].req_id)
        .await
        .unwrap();
    assert_eq!(detail.posting.req_id, postings[0].req_id);
}

#[tokio::test]
#[ignore = "hits the live ashby API; run with --ignored"]
async fn ashby_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("ramp", Ats::Ashby, "ramp");
    let postings = AshbyAdapter.list(&http, &board).await.unwrap().postings;
    assert_well_formed(&postings, "ramp");
}

#[tokio::test]
#[ignore = "hits the live lever API; run with --ignored"]
async fn lever_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("gopuff", Ats::Lever, "gopuff");
    let postings = LeverAdapter.list(&http, &board).await.unwrap().postings;
    assert_well_formed(&postings, "gopuff");
}

#[tokio::test]
#[ignore = "hits the live workday API; run with --ignored"]
async fn workday_live() {
    // A single page is the drift check — the full nvidia board is ~2000 postings across
    // ~100 paginated requests, too much to fetch just to confirm the shape. If Workday
    // renames these fields the adapter breaks, and this catches it cheaply.
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let url = "https://nvidia.wd5.myworkdayjobs.com/wday/cxs/nvidia/NVIDIAExternalCareerSite/jobs";
    let ctx = FetchCtx {
        board_id: BoardId::new("nvidia"),
        ats: Ats::Workday,
    };
    let body = http
        .post_json(
            url,
            &serde_json::json!({ "appliedFacets": {}, "limit": 5, "offset": 0, "searchText": "" }),
            &ctx,
        )
        .await
        .unwrap();
    for marker in ["jobPostings", "bulletFields", "externalPath", "title"] {
        assert!(body.contains(marker), "workday response missing {marker}");
    }
}

#[tokio::test]
#[ignore = "hits the live smartrecruiters API; run with --ignored"]
async fn smartrecruiters_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("visa", Ats::SmartRecruiters, "Visa");
    let postings = SmartRecruitersAdapter
        .list(&http, &board)
        .await
        .unwrap()
        .postings;
    assert_well_formed(&postings, "Visa");
}

#[tokio::test]
#[ignore = "hits the live rippling API + a job page; run with --ignored"]
async fn rippling_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("rippling", Ats::Rippling, "rippling");
    let postings = RipplingAdapter.list(&http, &board).await.unwrap().postings;
    assert_well_formed(&postings, "rippling");
    // Exercise the __NEXT_DATA__ detail path against a real job page.
    let detail = RipplingAdapter
        .detail(&http, &board, &postings[0].req_id)
        .await
        .unwrap();
    assert_eq!(detail.posting.req_id, postings[0].req_id);
}

#[tokio::test]
#[ignore = "hits the live github.careers API; run with --ignored"]
async fn github_careers_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("github", Ats::GithubCareers, "github");
    let postings = GithubCareersAdapter
        .list(&http, &board)
        .await
        .unwrap()
        .postings;
    assert_well_formed(&postings, "github");
}

#[tokio::test]
#[ignore = "hits the live workable API; run with --ignored"]
async fn workable_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("futureplc", Ats::Workable, "futureplc");
    let postings = WorkableAdapter.list(&http, &board).await.unwrap().postings;
    assert_well_formed(&postings, "futureplc");
    let detail = WorkableAdapter
        .detail(&http, &board, &postings[0].req_id)
        .await
        .unwrap();
    assert_eq!(detail.posting.req_id, postings[0].req_id);
}
