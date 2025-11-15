use crate::storage::Storage;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
}

pub type UsageEventSender = mpsc::Sender<UsageEvent>;
pub type UsageEventReceiver = mpsc::Receiver<UsageEvent>;

pub fn spawn_aggregator(
    storage: Storage,
    recent_capacity: usize,
) -> (UsageAggregatorHandle, UsageEventSender) {
    let (tx, rx) = mpsc::channel(recent_capacity);
    let join = tokio::spawn(async move {
        let aggregator = UsageAggregator::new(storage, recent_capacity);
        if let Err(err) = aggregator.run(rx).await {
            tracing::error!(error = %err, "usage aggregator exited with error");
        }
    });

    (UsageAggregatorHandle { join }, tx)
}

pub struct UsageAggregatorHandle {
    join: JoinHandle<()>,
}

impl UsageAggregatorHandle {
    pub async fn shutdown(self) {
        self.join.abort();
        let _ = self.join.await;
    }
}

struct UsageAggregator {
    storage: Storage,
    recent_capacity: usize,
    #[allow(dead_code)]
    recent_events: VecDeque<UsageEvent>,
}

impl UsageAggregator {
    fn new(storage: Storage, recent_capacity: usize) -> Self {
        Self {
            storage,
            recent_capacity,
            recent_events: VecDeque::with_capacity(recent_capacity),
        }
    }

    async fn run(mut self, mut rx: UsageEventReceiver) -> Result<()> {
        while let Some(event) = rx.recv().await {
            self.handle_event(event).await?;
        }
        Ok(())
    }

    async fn handle_event(&mut self, event: UsageEvent) -> Result<()> {
        let date = event.timestamp.date_naive();
        self.storage
            .record_daily_stat(
                date,
                &event.model,
                event.prompt_tokens,
                event.completion_tokens,
                event.total_tokens,
                event.cost_usd,
            )
            .await?;

        if self.recent_events.len() == self.recent_capacity {
            self.recent_events.pop_back();
        }
        self.recent_events.push_front(event);
        Ok(())
    }
}
