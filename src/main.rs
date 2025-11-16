mod app;
mod cli;
mod config;
mod proxy;
mod storage;
mod tokens;
mod tui;
mod usage;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = cli::Cli::parse();
    let config = config::AppConfig::load(cli.config_path.as_deref())?;
    let app = app::App::new(config).await?;
    app.run().await
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init();
}
