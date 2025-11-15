use anyhow::{Context, Result};
use chrono::NaiveDate;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{
    convert::TryFrom,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub struct Storage {
    pool: Arc<SqlitePool>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl Storage {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let options = SqliteConnectOptions::new()
            .filename(&path_buf)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .with_context(|| "failed to connect to sqlite database")?;

        Ok(Self {
            pool: Arc::new(pool),
            path: path_buf,
        })
    }

    pub async fn ensure_schema(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS daily_stats (
                date TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL NOT NULL DEFAULT 0.0,
                PRIMARY KEY (date, model)
            );
            "#,
        )
        .execute(&*self.pool)
        .await
        .with_context(|| "failed to ensure daily_stats schema")?;

        Ok(())
    }

    pub async fn record_daily_stat(
        &self,
        date: NaiveDate,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        cost_usd: f64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO daily_stats (
                date, model, prompt_tokens, completion_tokens, total_tokens, cost_usd
            ) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(date, model) DO UPDATE SET
                prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                completion_tokens = completion_tokens + excluded.completion_tokens,
                total_tokens = total_tokens + excluded.total_tokens,
                cost_usd = cost_usd + excluded.cost_usd;
            "#,
        )
        .bind(date.to_string())
        .bind(model)
        .bind(i64::try_from(prompt_tokens).unwrap_or(i64::MAX))
        .bind(i64::try_from(completion_tokens).unwrap_or(i64::MAX))
        .bind(i64::try_from(total_tokens).unwrap_or(i64::MAX))
        .bind(cost_usd)
        .execute(&*self.pool)
        .await
        .with_context(|| "failed to upsert daily stat")?;

        Ok(())
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
