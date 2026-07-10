//! End-to-end: spawn the real binary and drive it as an MCP client over stdio, the way
//! an actual client would. Proves the tool surface is wired and the offline tools
//! (list_boards, mark_obit, list_obits) round-trip through the process. Tools that fetch
//! (fetch_board, diff after a fetch) need the network and are covered by the #[ignore]d
//! live smoke tests and the store/adapter unit tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use job_board_mcp::config::{BoardConfig, Config};
use job_board_mcp::model::{Ats, AtsToken, BoardId};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use tokio::process::Command;

// pid + a per-test counter — collision-safe without a wall clock (which the determinism
// ban forbids anyway).
static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Fixture {
    dir: PathBuf,
    config_path: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jbmcp-e2e-{}-{tag}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Build the config THROUGH the serializer, never format! — a Windows db_path holds
        // backslashes that are TOML escapes, and hand-rolling the string would emit invalid
        // TOML only on the Windows runner. serde makes escaping the code's job.
        let config = Config {
            db_path: dir.join("store.sqlite").to_string_lossy().into_owned(),
            boards: vec![BoardConfig {
                id: BoardId::new("testco"),
                ats: Ats::Greenhouse,
                token: AtsToken::new("testco"),
                site: None,
                comp_site_only: false,
                updated_at_unreliable: false,
            }],
        };
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
        Self { dir, config_path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn connect(
    fixture: &Fixture,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    let client = ()
        .serve(TokioChildProcess::new(
            Command::new(env!("CARGO_BIN_EXE_job-board-mcp")).configure(|cmd| {
                cmd.arg("--config").arg(&fixture.config_path);
                cmd.env("RUST_LOG", "error");
            }),
        )?)
        .await?;
    Ok(client)
}

#[tokio::test]
async fn server_advertises_its_identity_and_the_full_tool_surface() -> anyhow::Result<()> {
    let fixture = Fixture::new("surface");
    let client = connect(&fixture).await?;

    let info = client.peer_info().expect("peer_info after handshake");
    assert_eq!(info.server_info.name, "job-board-mcp");

    let tools = client.list_tools(Default::default()).await?;
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in [
        "list_boards",
        "fetch_board",
        "fetch_posting",
        "diff_boards",
        "mark_obit",
        "list_obits",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn list_boards_reflects_the_config() -> anyhow::Result<()> {
    let fixture = Fixture::new("boards");
    let client = connect(&fixture).await?;

    let result = client
        .call_tool(CallToolRequestParams::new("list_boards"))
        .await?;
    let value = result.structured_content.expect("structured content");
    // One configured board, never fetched → null last_snapshot_at.
    assert_eq!(value["boards"][0]["id"], serde_json::json!("testco"));
    assert_eq!(value["boards"][0]["ats"], serde_json::json!("greenhouse"));
    assert_eq!(
        value["boards"][0]["last_snapshot_at"],
        serde_json::Value::Null
    );

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn mark_obit_then_list_obits_round_trips_through_the_server() -> anyhow::Result<()> {
    let fixture = Fixture::new("obit");
    let client = connect(&fixture).await?;

    client
        .call_tool(
            CallToolRequestParams::new("mark_obit").with_arguments(
                serde_json::json!({
                    "board_id": "testco",
                    "key": "req-99",
                    "kind": "rejected",
                    "reason": "applied and closed",
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await?;

    let listed = client
        .call_tool(
            CallToolRequestParams::new("list_obits").with_arguments(
                serde_json::json!({ "board_id": "testco" })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    let obits = listed.structured_content.expect("structured content");
    assert_eq!(obits["obits"][0]["key"], serde_json::json!("req-99"));
    assert_eq!(obits["obits"][0]["kind"], serde_json::json!("rejected"));
    assert_eq!(
        obits["obits"][0]["reason"],
        serde_json::json!("applied and closed")
    );

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn an_unknown_board_is_a_clean_error() -> anyhow::Result<()> {
    let fixture = Fixture::new("unknown");
    let client = connect(&fixture).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("diff_boards").with_arguments(
                serde_json::json!({ "board_ids": ["nope"] })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;
    assert!(
        result.is_err(),
        "an unknown board must be an error, not empty data"
    );

    client.cancel().await?;
    Ok(())
}
