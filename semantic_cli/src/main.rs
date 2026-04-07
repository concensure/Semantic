use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    semantic_app::render::run_cli().await
}
