//! End-to-end: spawn the real binary and drive it as an MCP client over stdio, the way
//! an actual client would. Proves the tool surface is wired and the offline tools
//! (list_boards, mark_obit, list_obits) round-trip through the process. Tools that fetch
//! (fetch_board, diff after a fetch) need the network and are covered by the #[ignore]d
//! live smoke tests and the store/adapter unit tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::DateTime;
use job_board_mcp::config::{BoardConfig, Config};
use job_board_mcp::model::{
    Ats, AtsToken, BoardId, Comp, Equity, Posting, ReqId, WorkplaceType, content_hash,
};
use job_board_mcp::store::{RawCapture, Store};
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
            raw_capture_days: 7,
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
        "list_captures",
        "dump_captures",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }

    client.cancel().await?;
    Ok(())
}

/// Asserts every emitted tool schema stays inside the subset a strict-but-incomplete MCP
/// client validator accepts — the failure class that has bitten this server twice, where a
/// spec-legal construct one validator dislikes fails the ENTIRE `tools/list` (one bad tool
/// sinks them all). Two bans:
///
///   1. No BOOLEAN subschema in a `properties`/`items` position. schemars derives a bare
///      `serde_json::Value` as the boolean schema `true`; Claude Code's validator rejects it
///      there (found live: fetch_posting's `posting: Value`). `additionalProperties` is
///      exempt — boolean is the conventional strictness switch there and clients take it.
///   2. No `$ref`/`$defs`/`definitions`. A `$ref` is spec-legal but a weak client validator
///      may not resolve it (G.3: mark_obit's `ObitKind` enum shipped one). Named types are
///      inlined via `#[schemars(inline)]` / a hand-written `JsonSchema` instead.
///
/// This is a client-compat conformance check, not a JSON-Schema meta-validation: the point
/// is what a real MCP client's validator REJECTS, and those constructs are all valid schema.
fn assert_client_safe_schema(path: &str, node: &serde_json::Value) {
    match node {
        serde_json::Value::Object(obj) => {
            for (key, val) in obj {
                let child = format!("{path}/{key}");
                assert!(
                    !matches!(key.as_str(), "$ref" | "$defs" | "definitions"),
                    "client-unsafe `{key}` at {child}: a weak validator may not resolve it and \
                     one rejected tool sinks the whole listing — inline the schema instead"
                );
                if key == "properties" {
                    if let Some(props) = val.as_object() {
                        for (name, schema) in props {
                            assert!(
                                schema.is_object(),
                                "boolean subschema at {child}/{name}: {schema}"
                            );
                        }
                    }
                } else if key == "items" {
                    assert!(val.is_object(), "boolean subschema at {child}: {val}");
                }
                assert_client_safe_schema(&child, val);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                assert_client_safe_schema(&format!("{path}/{i}"), v);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn tool_schemas_are_client_safe() -> anyhow::Result<()> {
    let fixture = Fixture::new("schemas");
    let client = connect(&fixture).await?;

    let tools = client.list_tools(Default::default()).await?;
    for tool in &tools.tools {
        let input = serde_json::to_value(tool.input_schema.as_ref())?;
        assert_client_safe_schema(&format!("{}/inputSchema", tool.name), &input);
        if let Some(out) = &tool.output_schema {
            let output = serde_json::to_value(out.as_ref())?;
            assert_client_safe_schema(&format!("{}/outputSchema", tool.name), &output);
        }
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
async fn dump_captures_writes_a_sample_file_to_the_chosen_dir() -> anyhow::Result<()> {
    let fixture = Fixture::new("dump");
    // Pre-seed a capture straight into the store the binary will open — the e2e can't hit
    // a live board, so we seed the ledger the way a real fetch would have. Drop the handle
    // to release the SQLite connection before the child process opens the same file.
    {
        let store = Store::open(&fixture.dir.join("store.sqlite")).await?;
        store
            .record_capture(
                &RawCapture {
                    board_id: &BoardId::new("testco"),
                    ats: Ats::Greenhouse,
                    url: "https://boards-api.greenhouse.io/v1/boards/testco/jobs",
                    method: "GET",
                    request_body: None,
                    status: 200,
                    body: r#"{"jobs":[{"id":1}]}"#,
                },
                DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                7,
            )
            .await?;
    }

    let client = connect(&fixture).await?;

    // The ledger surfaces the seeded capture — metadata only, no body inline.
    let listed = client
        .call_tool(
            CallToolRequestParams::new("list_captures").with_arguments(
                serde_json::json!({ "board_id": "testco" })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    let listed = listed.structured_content.expect("structured content");
    assert_eq!(listed["captures"][0]["status"], serde_json::json!(200));

    // Dump to a caller-chosen directory and confirm a real file lands there with the body.
    let out_dir = fixture.dir.join("samples");
    let dumped = client
        .call_tool(
            CallToolRequestParams::new("dump_captures").with_arguments(
                serde_json::json!({ "out_dir": out_dir.to_string_lossy() })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    let dumped = dumped.structured_content.expect("structured content");
    let path = dumped["dumped"][0]["path"]
        .as_str()
        .expect("a dumped file path");
    assert_eq!(std::fs::read_to_string(path)?, r#"{"jobs":[{"id":1}]}"#);
    // The body is on disk, never echoed inline — the whole point of dumping to files.
    assert!(dumped["dumped"][0].get("body").is_none());

    client.cancel().await?;
    Ok(())
}

fn seed_posting(req: &str, title: &str) -> Posting {
    let comp = Comp::None;
    Posting {
        ats: Ats::Greenhouse,
        board_id: BoardId::new("testco"),
        req_id: ReqId::new(req),
        title: title.to_owned(),
        url: format!("https://boards.greenhouse.io/testco/jobs/{req}"),
        locations: vec!["Remote".to_owned()],
        workplace_type: WorkplaceType::Remote,
        remote_scope: None,
        comp: comp.clone(),
        equity: Equity::None,
        posted_at: None,
        updated_at: None,
        updated_at_unreliable: false,
        department: None,
        employment_type: None,
        content_hash: content_hash(
            title,
            &["Remote".to_owned()],
            WorkplaceType::Remote,
            &comp,
            Equity::None,
            "",
        ),
    }
}

/// Seed a snapshot straight into the store (so a NEW row exists without a live fetch), then
/// call `diff_boards --include_summary` through the server and assert the NEW row is enriched.
#[tokio::test]
async fn diff_boards_include_summary_enriches_new_rows() -> anyhow::Result<()> {
    let fixture = Fixture::new("summary");
    // Seed then DROP the store handle before the server opens the same SQLite file.
    {
        let store = Store::open(&fixture.dir.join("store.sqlite")).await?;
        let board = BoardConfig {
            id: BoardId::new("testco"),
            ats: Ats::Greenhouse,
            token: AtsToken::new("testco"),
            site: None,
            comp_site_only: false,
            updated_at_unreliable: false,
        };
        store.upsert_board(&board).await?;
        store
            .record_snapshot(
                &BoardId::new("testco"),
                job_board_mcp::clock::now(),
                &[seed_posting("R1", "Staff Engineer")],
            )
            .await?;
    }

    let client = connect(&fixture).await?;
    let out = client
        .call_tool(
            CallToolRequestParams::new("diff_boards").with_arguments(
                serde_json::json!({ "include_summary": true })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    let body = out.structured_content.expect("structured content");
    let new0 = &body["diffs"][0]["diff"]["new"][0];
    assert_eq!(new0["req_id"], serde_json::json!("R1"));
    assert_eq!(new0["title"], serde_json::json!("Staff Engineer"));
    assert_eq!(new0["locations"][0], serde_json::json!("Remote"));

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
