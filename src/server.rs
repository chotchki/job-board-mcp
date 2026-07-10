//! The MCP tool surface. Today it is just `ping`; the real tools (`list_boards`,
//! `fetch_board`, `fetch_posting`, `diff_boards`, `mark_obit`, `list_obits`) land in Phase E.

use rmcp::{
    ErrorData as McpError, Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PingRequest {
    /// Optional text to echo back in the reply.
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PingReply {
    pub ok: bool,
    pub echo: Option<String>,
}

#[derive(Clone)]
pub struct JobBoardServer {
    tool_router: ToolRouter<JobBoardServer>,
}

impl Default for JobBoardServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl JobBoardServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Health check; echoes an optional message back as JSON")]
    async fn ping(
        &self,
        Parameters(PingRequest { message }): Parameters<PingRequest>,
    ) -> Result<Json<PingReply>, McpError> {
        Ok(Json(PingReply {
            ok: true,
            echo: message,
        }))
    }
}

// `router = self.tool_router` dispatches through the field built once in `new()`. A bare
// `#[tool_handler]` defaults to `Self::tool_router()`, rebuilding the router on every call
// and leaving the field unread.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for JobBoardServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo::new` fills server_info from `Implementation::from_build_env()`, whose
        // `env!()`s expand at RMCP's compile time — omit `with_server_info` and this server
        // introduces itself to every client as "rmcp"/"2.2.0". The `env!()`s below expand here.
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
