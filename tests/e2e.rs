//! Drives the built binary as a child process over stdio with a real MCP client.
//! Phase E grows this into full tool coverage; today it proves the plumbing.

use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use tokio::process::Command;

#[tokio::test]
async fn ping_round_trips_through_a_real_mcp_client() -> anyhow::Result<()> {
    // `()` is the no-op ClientHandler. `.serve(..)` performs the initialize handshake and
    // only returns once it completes.
    let client = ()
        .serve(TokioChildProcess::new(
            Command::new(env!("CARGO_BIN_EXE_job-board-mcp")).configure(|cmd| {
                cmd.env("RUST_LOG", "error");
            }),
        )?)
        .await?;

    // Without an explicit `with_server_info`, rmcp reports its OWN name and version here.
    // Assert our identity so that regression can never ship silently.
    let server_info = client.peer_info().expect("peer_info set after handshake");
    assert_eq!(server_info.server_info.name, "job-board-mcp");
    assert_eq!(server_info.server_info.version, env!("CARGO_PKG_VERSION"));

    let tools = client.list_tools(Default::default()).await?;
    assert!(
        tools.tools.iter().any(|t| t.name == "ping"),
        "server did not advertise the `ping` tool: {:#?}",
        tools.tools
    );

    let result = client
        .call_tool(
            CallToolRequestParams::new("ping").with_arguments(
                serde_json::json!({ "message": "hello" })
                    .as_object()
                    .expect("object literal")
                    .clone(),
            ),
        )
        .await?;

    // `Json<T>` returns route through `CallToolResult::structured`, which sets
    // is_error = Some(false), fills structured_content, and mirrors the JSON into a text
    // content block. Pin all three — rmcp's own docstring claims content is left empty.
    assert_eq!(result.is_error, Some(false));
    let structured = result
        .structured_content
        .clone()
        .expect("Json<T> populates structured_content");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["echo"], serde_json::json!("hello"));
    assert!(!result.content.is_empty());

    client.cancel().await?;
    Ok(())
}
