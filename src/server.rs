//! The MCP tool surface: the six tools SPEC promises, wired to the store and the
//! adapters. This layer holds the server's state (store, HTTP client, the configured
//! boards) and is the ONE place in the process that reads the wall clock — every
//! timestamp the store records flows from [`now`], so the store itself stays
//! deterministic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

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
use crate::clock;
use crate::config::BoardConfig;
use crate::http::HttpClient;
use crate::model::{Ats, BoardId, ObitKind, ReqId};
use crate::store::{Store, StoreError};

struct Inner {
    store: Arc<Store>,
    http: HttpClient,
    boards: HashMap<BoardId, BoardConfig>,
    /// Fallback root for `dump_captures` when the caller doesn't pass an `out_dir` — the
    /// store's own directory, so samples land next to the database by default.
    db_dir: PathBuf,
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
    /// Echo the full postings array back. Default false: a morning scan (fetch → diff)
    /// never needs the postings in context, and a big board is hundreds of KB. Use
    /// `diff_boards` for what changed, `fetch_posting` for one JD.
    #[serde(default)]
    pub full: bool,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListCapturesArgs {
    /// Restrict to one board; omit for every board.
    #[serde(default)]
    pub board_id: Option<String>,
    /// Max rows to return, newest first. Default 50.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DumpCapturesArgs {
    /// Directory to write the sample files into (created if missing). Defaults to a
    /// `captures` directory beside the store when omitted. A leading `~/` is expanded.
    #[serde(default)]
    pub out_dir: Option<String>,
    /// Restrict to one board; omit for every board.
    #[serde(default)]
    pub board_id: Option<String>,
    /// Max samples to dump, newest first. Default 20.
    #[serde(default)]
    pub limit: Option<i64>,
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
    /// Non-fatal notes from this fetch: stub postings skipped mid-publish, or a board
    /// that went non-empty → empty (a possible migration). Empty when there's nothing to
    /// flag. Surfaced here because an MCP client never sees the server's stderr log.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    /// Present only when `full` was set on the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    postings: Option<Vec<Value>>,
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

#[derive(Serialize, JsonSchema)]
struct CapturesResponse {
    captures: Vec<Value>,
}

#[derive(Serialize, JsonSchema)]
struct DumpResponse {
    /// Where the samples were written.
    out_dir: String,
    /// One entry per file: its path, plus enough metadata to describe the sample without
    /// carrying the body.
    dumped: Vec<Value>,
}

#[tool_router]
impl JobBoardServer {
    pub fn new(
        store: Arc<Store>,
        http: HttpClient,
        boards: Vec<BoardConfig>,
        db_dir: PathBuf,
    ) -> Self {
        let boards = boards.into_iter().map(|b| (b.id.clone(), b)).collect();
        Self {
            tool_router: Self::tool_router(),
            inner: Arc::new(Inner {
                store,
                http,
                boards,
                db_dir,
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
        let result = adapter::list_for(&self.inner.http, board)
            .await
            .map_err(adapter_err)?;
        // Read the prior count BEFORE recording overwrites it — the migration signal.
        let prev_count = self
            .inner
            .store
            .previous_posting_count(&board.id)
            .await
            .map_err(store_err)?;
        let postings = result.postings;
        let snapshot_id = self
            .inner
            .store
            .record_snapshot(&board.id, clock::now(), &postings)
            .await
            .map_err(store_err)?;

        let mut warnings = Vec::new();
        if !result.skipped.is_empty() {
            warnings.push(format!(
                "{} posting(s) skipped mid-publish (no title/path yet): {}",
                result.skipped.len(),
                result.skipped.join(", ")
            ));
        }
        if postings.is_empty() && prev_count.unwrap_or(0) > 0 {
            warnings.push(format!(
                "board returned 0 postings after a snapshot of {} — possible migration off this ATS",
                prev_count.unwrap_or(0)
            ));
        }

        let postings_echo = args.full.then(|| postings.iter().map(to_value).collect());
        Ok(Json(FetchBoardResponse {
            board_id: board.id.to_string(),
            snapshot_id,
            posting_count: postings.len(),
            warnings,
            postings: postings_echo,
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
            .mark_obit(&board.id, &args.key, args.kind, &args.reason, clock::now())
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

    #[tool(
        description = "List the raw request/response capture ledger — metadata only, no \
                          bodies — newest first. Optionally scoped to one board."
    )]
    async fn list_captures(
        &self,
        Parameters(args): Parameters<ListCapturesArgs>,
    ) -> Result<Json<CapturesResponse>, McpError> {
        let board = args
            .board_id
            .as_deref()
            .map(|id| self.board(id))
            .transpose()?;
        let limit = args.limit.unwrap_or(50).clamp(1, 1000);
        let metas = self
            .inner
            .store
            .list_captures(board.map(|b| &b.id), limit)
            .await
            .map_err(store_err)?;
        Ok(Json(CapturesResponse {
            captures: metas.iter().map(to_value).collect(),
        }))
    }

    #[tool(
        description = "Dump raw captured response bodies to sample files on disk and return \
                          their PATHS (never the bodies inline — a big board is hundreds of \
                          KB). Hand a returned file back to have an adapter built or fixed \
                          against the real shape. Pass out_dir to choose where they land."
    )]
    async fn dump_captures(
        &self,
        Parameters(args): Parameters<DumpCapturesArgs>,
    ) -> Result<Json<DumpResponse>, McpError> {
        let board = args
            .board_id
            .as_deref()
            .map(|id| self.board(id))
            .transpose()?;
        let limit = args.limit.unwrap_or(20).clamp(1, 1000);
        let out_dir = match args.out_dir.as_deref() {
            Some(dir) => expand_tilde(dir),
            None => self.inner.db_dir.join("captures"),
        };
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            McpError::internal_error(format!("creating {}: {e}", out_dir.display()), None)
        })?;

        let records = self
            .inner
            .store
            .dump_captures(board.map(|b| &b.id), limit)
            .await
            .map_err(store_err)?;

        let mut dumped = Vec::with_capacity(records.len());
        for rec in records {
            let filename = format!(
                "{}-{}-{}.{}",
                rec.board_id,
                ats_slug(rec.ats),
                rec.id,
                sample_ext(&rec.body),
            );
            let path = out_dir.join(&filename);
            std::fs::write(&path, &rec.body).map_err(|e| {
                McpError::internal_error(format!("writing {}: {e}", path.display()), None)
            })?;
            dumped.push(json!({
                "path": path.to_string_lossy(),
                "board_id": rec.board_id,
                "url": rec.url,
                "captured_at": rec.captured_at,
                "bytes": rec.body.len(),
            }));
        }
        Ok(Json(DumpResponse {
            out_dir: out_dir.to_string_lossy().into_owned(),
            dumped,
        }))
    }
}

/// The ATS as a bare slug for a filename (`greenhouse`, not `"greenhouse"`).
fn ats_slug(ats: Ats) -> String {
    serde_json::to_value(ats)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "ats".to_owned())
}

/// A file extension guessed from the body's first non-space byte — so a JSON sample
/// opens as JSON and Rippling's HTML detail opens as HTML.
fn sample_ext(body: &str) -> &'static str {
    match body.trim_start().as_bytes().first() {
        Some(b'{' | b'[') => "json",
        Some(b'<') => "html",
        _ => "txt",
    }
}

/// Expand a leading `~/` against the home directory; everything else is verbatim. Mirrors
/// the binary's own db_path expansion so a `dump_captures out_dir` behaves the same way.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::Path::new(&home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn to_value<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).expect("model types serialize")
}

fn store_err(e: StoreError) -> McpError {
    McpError::internal_error(format!("store error: {e}"), None)
}

fn adapter_err(e: AdapterError) -> McpError {
    match e {
        // Bad user input (wrong board or wrong req) is invalid_params, not internal_error —
        // it points the caller at what they passed, not at the server.
        AdapterError::UnknownBoard(_) | AdapterError::PostingNotFound(_) => {
            McpError::invalid_params(e.to_string(), None)
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
