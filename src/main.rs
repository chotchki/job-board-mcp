use anyhow::Result;
use job_board_mcp::server::JobBoardServer;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
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

    let service = JobBoardServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
