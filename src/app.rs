use crate::{config::AppConfig, proxy, storage::Storage, tui, usage};
use anyhow::Result;
use std::sync::Arc;

/// High-level application orchestrator.
pub struct App {
    config: Arc<AppConfig>,
}

impl App {
    pub async fn new(config: AppConfig) -> Result<Self> {
        Ok(Self {
            config: Arc::new(config),
        })
    }

    pub async fn run(self) -> Result<()> {
        let storage = Storage::connect(&self.config.storage.database_path).await?;
        storage.ensure_schema().await?;

        let (aggregator_handle, usage_tx) =
            usage::spawn_aggregator(storage.clone(), self.config.display.recent_events_capacity);

        let proxy_handle = proxy::spawn(self.config.clone(), usage_tx.clone()).await?;

        tui::print_placeholder_overview(&self.config);
        tracing::info!(
            listen = %self.config.server.listen_addr,
            upstream = %self.config.server.upstream_base_url,
            "codex-usage-proxy scaffold ready. Press Ctrl+C to exit."
        );

        tokio::signal::ctrl_c().await?;

        drop(usage_tx);
        aggregator_handle.shutdown().await;
        proxy_handle.shutdown().await?;
        Ok(())
    }
}
