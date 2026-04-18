use anyhow::Result;
use semantic_app::{mcp_server, AppRuntime, RuntimeOptions};
use std::net::SocketAddr;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let repo_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let bridge_token =
        std::env::var("MCP_BRIDGE_TOKEN").unwrap_or_else(|_| "semantic-local-token".to_string());

    let runtime = AppRuntime::bootstrap(
        repo_root,
        RuntimeOptions {
            start_watcher: true,
            ensure_config: true,
            ..RuntimeOptions::default()
        },
    )?;
    let addr: SocketAddr = "127.0.0.1:4321".parse()?;
    mcp_server::serve(runtime, bridge_token, addr).await
}
