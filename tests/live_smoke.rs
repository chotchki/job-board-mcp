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

use job_board_mcp::adapter::{Adapter, AshbyAdapter, GreenhouseAdapter, LeverAdapter};
use job_board_mcp::config::BoardConfig;
use job_board_mcp::http::{HttpClient, HttpConfig};
use job_board_mcp::model::{Ats, AtsToken, BoardId, Posting};

fn board(id: &str, ats: Ats, token: &str) -> BoardConfig {
    BoardConfig {
        id: BoardId::new(id),
        ats,
        token: AtsToken::new(token),
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
    let postings = GreenhouseAdapter.list(&http, &board).await.unwrap();
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
    let postings = AshbyAdapter.list(&http, &board).await.unwrap();
    assert_well_formed(&postings, "ramp");
}

#[tokio::test]
#[ignore = "hits the live lever API; run with --ignored"]
async fn lever_live() {
    let http = HttpClient::new(HttpConfig::default()).unwrap();
    let board = board("gopuff", Ats::Lever, "gopuff");
    let postings = LeverAdapter.list(&http, &board).await.unwrap();
    assert_well_formed(&postings, "gopuff");
}
