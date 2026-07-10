//! The one trait every ATS implementation satisfies, plus the error taxonomy that
//! keeps failures loud. Concrete adapters (greenhouse, Ashby, Lever) land in Phase C
//! as submodules here; dispatch is an exhaustive `match` on [`Ats`](crate::model::Ats),
//! so a wave-2 platform can't be added without the compiler forcing it to be wired.

use std::future::Future;

use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{Ats, BoardId, Posting, PostingDetail, ReqId};

mod ashby;
mod greenhouse;
mod lever;
mod parse;

pub use ashby::AshbyAdapter;
pub use greenhouse::GreenhouseAdapter;
pub use lever::LeverAdapter;

/// Why an adapter call failed. Every variant is loud and typed — especially
/// [`ParseDrift`](AdapterError::ParseDrift), which an adapter returns *instead of*
/// guessing a field. A wrong location or comp band silently propagating into a
/// decision is the exact failure this project exists to kill, so a changed feed shape
/// stops the world rather than inventing data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// The board isn't one this build knows how to fetch.
    #[error("unknown board: {0}")]
    UnknownBoard(BoardId),

    /// The board answered, but not with a usable listing — an HTTP non-success. This
    /// must NEVER be recorded as an empty snapshot (a tenant in maintenance mode
    /// returning a 200-with-empty-body is the trap): treating it as a real fetch would
    /// mark every posting DEAD and poison the next diff.
    #[error("board unreachable: HTTP {status}")]
    BoardUnreachable { status: u16 },

    /// A network-level failure with no HTTP status — timeout, connection refused, DNS.
    /// Same consequence as `BoardUnreachable`: no snapshot.
    #[error("transport error: {0}")]
    Transport(String),

    /// The feed's shape changed out from under the parser. `context` names what was
    /// being parsed and `detail` says what was missing or wrong — enough to fix the
    /// adapter, and a hard stop rather than a guess.
    #[error("parse drift while reading {context}: {detail}")]
    ParseDrift { context: String, detail: String },
}

impl AdapterError {
    /// Convenience for the common `ParseDrift` construction at a parse site.
    pub fn drift(context: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::ParseDrift {
            context: context.into(),
            detail: detail.into(),
        }
    }
}

/// One ATS's read-only view of a board. Async, and the futures are `Send` because they
/// run inside the tokio server; the explicit `impl Future + Send` (rather than bare
/// `async fn`) is what guarantees that at the trait boundary.
/// Fetch a board's listing through the adapter for its ATS. This `match` is the whole
/// dispatch — and because `Ats` is closed, a wave-2 platform can't be added without the
/// compiler forcing a new arm here.
pub async fn list_for(
    http: &HttpClient,
    board: &BoardConfig,
) -> Result<Vec<Posting>, AdapterError> {
    match board.ats {
        Ats::Greenhouse => GreenhouseAdapter.list(http, board).await,
        Ats::Ashby => AshbyAdapter.list(http, board).await,
        Ats::Lever => LeverAdapter.list(http, board).await,
    }
}

/// Fetch one posting's detail through the adapter for its ATS.
pub async fn detail_for(
    http: &HttpClient,
    board: &BoardConfig,
    req_id: &ReqId,
) -> Result<PostingDetail, AdapterError> {
    match board.ats {
        Ats::Greenhouse => GreenhouseAdapter.detail(http, board, req_id).await,
        Ats::Ashby => AshbyAdapter.detail(http, board, req_id).await,
        Ats::Lever => LeverAdapter.detail(http, board, req_id).await,
    }
}

pub trait Adapter {
    /// Fetch the board's current listing, normalized. A successful return is the ONLY
    /// thing that may become a snapshot. The `http` client is passed in so adapters stay
    /// stateless (zero-size) and their parse logic stays testable against fixtures.
    fn list(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
    ) -> impl Future<Output = Result<Vec<Posting>, AdapterError>> + Send;

    /// Fetch one posting's full detail, including description text, for JD capture at
    /// apply time.
    fn detail(
        &self,
        http: &HttpClient,
        board: &BoardConfig,
        req_id: &ReqId,
    ) -> impl Future<Output = Result<PostingDetail, AdapterError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, Comp, ContentHash, WorkplaceType};

    // Proves the trait is implementable with a plain `async fn` body and that its
    // futures are Send (they're awaited across a spawn below).
    struct StubAdapter;

    impl Adapter for StubAdapter {
        async fn list(
            &self,
            _http: &HttpClient,
            board: &BoardConfig,
        ) -> Result<Vec<Posting>, AdapterError> {
            Ok(vec![Posting {
                ats: board.ats,
                board_id: board.id.clone(),
                req_id: ReqId::new("1"),
                title: "Engineer".to_owned(),
                url: "https://example.test/1".to_owned(),
                locations: vec![],
                workplace_type: WorkplaceType::Unknown,
                remote_scope: None,
                comp: Comp::None,
                posted_at: None,
                updated_at: None,
                updated_at_unreliable: board.updated_at_unreliable,
                department: None,
                employment_type: None,
                content_hash: ContentHash::from_bytes([0; 32]),
            }])
        }

        async fn detail(
            &self,
            _http: &HttpClient,
            board: &BoardConfig,
            req_id: &ReqId,
        ) -> Result<PostingDetail, AdapterError> {
            let _ = req_id;
            Err(AdapterError::UnknownBoard(board.id.clone()))
        }
    }

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("stub"),
            ats: Ats::Greenhouse,
            token: AtsToken::new("stub"),
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    #[tokio::test]
    async fn trait_is_implementable_and_futures_are_send() {
        // Spawning forces the future to be Send — a compile-time proof of the bound.
        let http = crate::http::HttpClient::new(crate::http::HttpConfig::default()).unwrap();
        let postings = tokio::spawn(async move { StubAdapter.list(&http, &board()).await })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].title, "Engineer");
    }

    #[test]
    fn errors_display_loudly() {
        assert_eq!(
            AdapterError::BoardUnreachable { status: 503 }.to_string(),
            "board unreachable: HTTP 503"
        );
        assert_eq!(
            AdapterError::drift("greenhouse jobs[3].location", "field absent").to_string(),
            "parse drift while reading greenhouse jobs[3].location: field absent"
        );
    }
}
