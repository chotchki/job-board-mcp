//! The MCP tool surface: the six tools SPEC promises, wired to the store and the
//! adapters. This layer holds the server's state (store, HTTP client, the configured
//! boards) and is the ONE place in the process that reads the wall clock — every
//! timestamp the store records flows from [`now`], so the store itself stays
//! deterministic.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rmcp::{
    ErrorData as McpError, Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::adapter::{self, AdapterError};
use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{BoardId, ObitKind, ReqId};
use crate::store::{Store, StoreError};

/// The one wall-clock read in the whole process. Every recorded timestamp — a snapshot's
/// `taken_at`, an obit's `marked_at` — comes from here, so the store never reads a clock
/// and its diffs stay reproducible. The `#[expect]` is the single sanctioned exception to
/// the determinism ban, and if it ever stops being needed (no `now()` call remains) the
/// lint says so.
#[expect(
    clippy::disallowed_methods,
    reason = "the MCP handler is the sole clock reader; the store takes time as a parameter"
)]
fn now() -> DateTime<Utc> {
    Utc::now()
}

struct Inner {
    store: Store,
    http: HttpClient,
    boards: HashMap<BoardId, BoardConfig>,
}

#[derive(Clone)]
pub struct JobBoardServer {
    tool_router: ToolRouter<JobBoardServer>,
    inner: Arc<Inner>,
}

// ---- tool inputs --------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchBoardArgs {
    /// The configured board's id.
    pub board_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchPostingArgs {
    pub board_id: String,
    /// The requisition id, as returned by `fetch_board` / `diff_boards`.
    pub req_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffBoardsArgs {
    /// Boards to diff; omit to diff every configured board.
    #[serde(default)]
    pub board_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkObitArgs {
    pub board_id: String,
    /// A req_id, or a freeform key for a listing with no stable req_id.
    pub key: String,
    pub kind: ObitKind,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListObitsArgs {
    /// Restrict to one board; omit for the whole ledger.
    #[serde(default)]
    pub board_id: Option<String>,
}

// ---- tool outputs -------------------------------------------------------------------
//
// MCP requires an output schema rooted at `type: object`, so each tool returns a small
// struct (object root) whose heterogeneous payload is carried as `serde_json::Value` —
// which keeps the schema valid without deriving `JsonSchema` on every model type.

#[derive(Serialize, JsonSchema)]
struct BoardsResponse {
    boards: Vec<Value>,
}

#[derive(Serialize, JsonSchema)]
struct FetchBoardResponse {
    board_id: String,
    snapshot_id: i64,
    posting_count: usize,
    postings: Vec<Value>,
}

#[derive(Serialize, JsonSchema)]
struct PostingResponse {
    posting: Value,
}

#[derive(Serialize, JsonSchema)]
struct DiffResponse {
    diffs: Vec<Value>,
}

#[derive(Serialize, JsonSchema)]
struct MarkObitResponse {
    ok: bool,
    board_id: String,
    key: String,
}

#[derive(Serialize, JsonSchema)]
struct ObitsResponse {
    obits: Vec<Value>,
}

#[tool_router]
impl JobBoardServer {
    pub fn new(store: Store, http: HttpClient, boards: Vec<BoardConfig>) -> Self {
        let boards = boards.into_iter().map(|b| (b.id.clone(), b)).collect();
        Self {
            tool_router: Self::tool_router(),
            inner: Arc::new(Inner {
                store,
                http,
                boards,
            }),
        }
    }

    fn board(&self, id: &str) -> Result<&BoardConfig, McpError> {
        self.inner
            .boards
            .get(&BoardId::new(id))
            .ok_or_else(|| McpError::invalid_params(format!("unknown board: {id}"), None))
    }

    #[tool(
        description = "List configured boards with their ATS and the time of their last \
                          successful snapshot (null if never fetched)."
    )]
    async fn list_boards(&self) -> Result<Json<BoardsResponse>, McpError> {
        let mut boards = Vec::with_capacity(self.inner.boards.len());
        for board in self.inner.boards.values() {
            let last = self
                .inner
                .store
                .last_snapshot_at(&board.id)
                .await
                .map_err(store_err)?;
            boards.push(json!({
                "id": board.id,
                "ats": board.ats,
                "last_snapshot_at": last,
            }));
        }
        boards.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(Json(BoardsResponse { boards }))
    }

    #[tool(
        description = "Live-fetch a board, normalize its postings, and record a snapshot \
                          on success. A failed fetch records nothing."
    )]
    async fn fetch_board(
        &self,
        Parameters(args): Parameters<FetchBoardArgs>,
    ) -> Result<Json<FetchBoardResponse>, McpError> {
        let board = self.board(&args.board_id)?;
        // Only a successful fetch reaches record_snapshot — the invariant that a
        // maintenance-mode board is never recorded as empty and DEAD.
        let postings = adapter::list_for(&self.inner.http, board)
            .await
            .map_err(adapter_err)?;
        let snapshot_id = self
            .inner
            .store
            .record_snapshot(&board.id, now(), &postings)
            .await
            .map_err(store_err)?;
        Ok(Json(FetchBoardResponse {
            board_id: board.id.to_string(),
            snapshot_id,
            posting_count: postings.len(),
            postings: postings.iter().map(to_value).collect(),
        }))
    }

    #[tool(
        description = "Fetch one posting's full detail, including description text, for \
                          capturing a JD at apply time."
    )]
    async fn fetch_posting(
        &self,
        Parameters(args): Parameters<FetchPostingArgs>,
    ) -> Result<Json<PostingResponse>, McpError> {
        let board = self.board(&args.board_id)?;
        let detail = adapter::detail_for(&self.inner.http, board, &ReqId::new(args.req_id))
            .await
            .map_err(adapter_err)?;
        Ok(Json(PostingResponse {
            posting: to_value(&detail),
        }))
    }

    #[tool(
        description = "Report NEW / CHANGED / DEAD per board versus the previous \
                          snapshot, obit-suppressed rows excluded. Does not fetch."
    )]
    async fn diff_boards(
        &self,
        Parameters(args): Parameters<DiffBoardsArgs>,
    ) -> Result<Json<DiffResponse>, McpError> {
        let ids: Vec<String> = match args.board_ids {
            Some(ids) => ids,
            None => self.inner.boards.keys().map(|b| b.to_string()).collect(),
        };
        let mut diffs = Vec::with_capacity(ids.len());
        for id in ids {
            let board = self.board(&id)?;
            let diff = self
                .inner
                .store
                .diff_board(&board.id)
                .await
                .map_err(store_err)?;
            diffs.push(json!({ "board_id": board.id, "diff": diff }));
        }
        diffs.sort_by(|a, b| a["board_id"].as_str().cmp(&b["board_id"].as_str()));
        Ok(Json(DiffResponse { diffs }))
    }

    #[tool(
        description = "Suppress a posting (by req_id) or a freeform key from future NEW \
                          results, tagged dead | rejected | out_of_scope | ghost."
    )]
    async fn mark_obit(
        &self,
        Parameters(args): Parameters<MarkObitArgs>,
    ) -> Result<Json<MarkObitResponse>, McpError> {
        let board = self.board(&args.board_id)?;
        self.inner
            .store
            .mark_obit(&board.id, &args.key, args.kind, &args.reason, now())
            .await
            .map_err(store_err)?;
        Ok(Json(MarkObitResponse {
            ok: true,
            board_id: board.id.to_string(),
            key: args.key,
        }))
    }

    #[tool(description = "Read the obit ledger, for audit. Optionally scoped to one board.")]
    async fn list_obits(
        &self,
        Parameters(args): Parameters<ListObitsArgs>,
    ) -> Result<Json<ObitsResponse>, McpError> {
        let board = args
            .board_id
            .as_deref()
            .map(|id| self.board(id))
            .transpose()?;
        let obits = self
            .inner
            .store
            .list_obits(board.map(|b| &b.id))
            .await
            .map_err(store_err)?;
        Ok(Json(ObitsResponse {
            obits: obits.iter().map(to_value).collect(),
        }))
    }
}

fn to_value<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).expect("model types serialize")
}

fn store_err(e: StoreError) -> McpError {
    McpError::internal_error(format!("store error: {e}"), None)
}

fn adapter_err(e: AdapterError) -> McpError {
    match e {
        AdapterError::UnknownBoard(b) => {
            McpError::invalid_params(format!("unknown board: {b}"), None)
        }
        other => McpError::internal_error(other.to_string(), None),
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for JobBoardServer {
    fn get_info(&self) -> ServerInfo {
        // Without an explicit server_info, rmcp reports its OWN name/version (its env!s
        // expand at rmcp's compile time). The env!s below expand in THIS crate.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Typed, deterministic job-board tools: fetch, normalize, snapshot and diff \
                 postings from hosted ATS APIs. This server holds no opinions — fit scoring, \
                 ranking and 'should I apply' are the calling model's job."
                    .to_string(),
            )
    }
}
