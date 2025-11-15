use crate::{
    config::AppConfig,
    usage::{UsageEvent, UsageEventSender},
};
use anyhow::{anyhow, Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    routing::any,
    Router,
};
use chrono::Utc;
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

#[derive(Clone)]
struct ProxyState {
    config: Arc<AppConfig>,
    usage_tx: UsageEventSender,
}

pub struct ProxyHandle {
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<Result<()>>,
}

pub async fn spawn(config: Arc<AppConfig>, usage_tx: UsageEventSender) -> Result<ProxyHandle> {
    let addr: SocketAddr = config
        .server
        .listen_addr
        .parse()
        .with_context(|| "failed to parse listen_addr")?;
    let state = Arc::new(ProxyState { config, usage_tx });

    let router = Router::new()
        .fallback(any(proxy_placeholder))
        .with_state(state.clone());

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| "failed to bind proxy listener")?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let join = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .map_err(|err| anyhow!(err))
    });

    tracing::info!(listen = %addr, "proxy listener (scaffold) started");

    Ok(ProxyHandle {
        shutdown: Some(shutdown_tx),
        join,
    })
}

impl ProxyHandle {
    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        match self.join.await {
            Ok(result) => result,
            Err(err) => Err(anyhow!(err)),
        }
    }
}

async fn proxy_placeholder(
    State(state): State<Arc<ProxyState>>,
    req: Request<Body>,
) -> (StatusCode, String) {
    tracing::warn!(
        method = %req.method(),
        path = %req.uri().path(),
        "proxy placeholder invoked; upstream forwarding not yet implemented"
    );

    let event = UsageEvent {
        timestamp: Utc::now(),
        model: "proxy-placeholder".to_string(),
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        cost_usd: 0.0,
    };
    let _ = state.usage_tx.try_send(event);

    (
        StatusCode::NOT_IMPLEMENTED,
        format!(
            "codex-usage-proxy scaffold running. Upstream target: {}",
            state.config.server.upstream_base_url
        ),
    )
}
