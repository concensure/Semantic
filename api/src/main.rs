use anyhow::Result;
use semantic_app::{api_server, AppRuntime, RuntimeOptions};
use std::net::SocketAddr;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .compact()
        .init();

    let repo_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);

    let runtime = AppRuntime::bootstrap(
        repo_root,
        RuntimeOptions {
            start_watcher: true,
            ensure_config: true,
        },
    )?;
    let addr: SocketAddr = "127.0.0.1:4317".parse()?;
    api_server::serve(runtime, addr).await
}
