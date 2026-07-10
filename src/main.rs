use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use job_board_mcp::config::{self, Config};
use job_board_mcp::http::{HttpClient, HttpConfig};
use job_board_mcp::server::JobBoardServer;
use job_board_mcp::store::Store;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

const HELP: &str = "\
job-board-mcp — an MCP server exposing typed, deterministic job-board tools over stdio.

USAGE:
    job-board-mcp --config <path>

OPTIONS:
    --config <path>    Path to the TOML config (or set JOB_BOARD_MCP_CONFIG)
    -h, --help         Print this help
    -V, --version      Print version

Normally launched by an MCP client, not run directly. See docs/USAGE.md.";

#[tokio::main]
async fn main() -> Result<()> {
    // Answer --help/--version BEFORE touching config, so an unconfigured binary can still
    // introduce itself. These go to stdout because we exit before entering server mode.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return Ok(());
    }

    // stdout is the JSON-RPC wire — every byte of logging goes to stderr, and a stray
    // `println!` anywhere in this process corrupts the protocol stream. A bare `fmt()`
    // subscriber ignores RUST_LOG and pins INFO; EnvFilter is what makes RUST_LOG honored.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path =
        config::resolve_config_path(std::env::args()).context("resolving the config path")?;
    let config = Config::load(&config_path).context("loading config")?;

    let db_path = expand_tilde(&config.db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating the store directory {}", parent.display()))?;
    }

    let store = Store::open(&db_path)
        .await
        .with_context(|| format!("opening the store at {}", db_path.display()))?;
    // Mirror the config's boards so snapshots/postings have something to reference. The
    // config file remains the source of truth.
    for board in &config.boards {
        store
            .upsert_board(board)
            .await
            .context("mirroring a board")?;
    }

    let http = HttpClient::new(HttpConfig::default()).context("building the HTTP client")?;

    let service = JobBoardServer::new(store, http, config.boards)
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

/// Expand a leading `~/` against the home directory. The config keeps `db_path` verbatim;
/// this is where it becomes a real path.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(path)
}
