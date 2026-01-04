use crate::{
    config::AppConfig,
    storage::{IngestStateRow, SessionMeta, Storage},
};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::oneshot, task::JoinHandle, time};

const TITLE_MAX_CHARS: usize = 200;
const SUMMARY_MAX_CHARS: usize = 160;
const MESSAGE_DEDUPE_WINDOW_SECS: i64 = 2;

#[derive(Clone, Copy, Debug, Default)]
struct TokenTotals {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl TokenTotals {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            input_tokens: value.get("input_tokens")?.as_u64()?,
            cached_input_tokens: value
                .get("cached_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: value.get("output_tokens")?.as_u64()?,
            reasoning_output_tokens: value
                .get("reasoning_output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            total_tokens: value.get("total_tokens")?.as_u64()?,
        })
    }

    fn is_zero(self) -> bool {
        self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_output_tokens == 0
            && self.total_tokens == 0
    }

    fn any_decreased(self, previous: Self) -> bool {
        self.input_tokens < previous.input_tokens
            || self.cached_input_tokens < previous.cached_input_tokens
            || self.output_tokens < previous.output_tokens
            || self.reasoning_output_tokens < previous.reasoning_output_tokens
            || self.total_tokens < previous.total_tokens
    }

    fn saturating_sub(self, previous: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(previous.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(previous.cached_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(previous.output_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .saturating_sub(previous.reasoning_output_tokens),
            total_tokens: self.total_tokens.saturating_sub(previous.total_tokens),
        }
    }
}

#[derive(Clone)]
struct PendingMessage {
    timestamp: Option<DateTime<Utc>>,
    snippet: String,
    seq: u64,
}

#[derive(Clone)]
struct LastUserMessage {
    timestamp: Option<DateTime<Utc>>,
    snippet: String,
}

struct FileState {
    session_id: Option<String>,
    last_offset: u64,
    last_seen: TokenTotals,
    last_committed: TokenTotals,
    current_model: Option<String>,
    current_effort: Option<String>,
    current_message_id: Option<i64>,
    current_message_seq: u64,
    pending_note: Option<String>,
    pending_note_seq: u64,
    used_note_seq: u64,
    pending_title: Option<String>,
    pending_summary: Option<String>,
    pending_messages: Vec<PendingMessage>,
    last_user_message: Option<LastUserMessage>,
}

impl FileState {
    fn new() -> Self {
        Self {
            session_id: None,
            last_offset: 0,
            last_seen: TokenTotals::default(),
            last_committed: TokenTotals::default(),
            current_model: None,
            current_effort: None,
            current_message_id: None,
            current_message_seq: 0,
            pending_note: None,
            pending_note_seq: 0,
            used_note_seq: 0,
            pending_title: None,
            pending_summary: None,
            pending_messages: Vec::new(),
            last_user_message: None,
        }
    }

    fn from_state(state: &IngestStateRow) -> Self {
        Self {
            session_id: state.session_id.clone(),
            last_offset: state.last_offset,
            last_seen: TokenTotals {
                input_tokens: state.last_seen_input_tokens,
                cached_input_tokens: state.last_seen_cached_input_tokens,
                output_tokens: state.last_seen_output_tokens,
                reasoning_output_tokens: state.last_seen_reasoning_output_tokens,
                total_tokens: state.last_seen_total_tokens,
            },
            last_committed: TokenTotals {
                input_tokens: state.last_committed_input_tokens,
                cached_input_tokens: state.last_committed_cached_input_tokens,
                output_tokens: state.last_committed_output_tokens,
                reasoning_output_tokens: state.last_committed_reasoning_output_tokens,
                total_tokens: state.last_committed_total_tokens,
            },
            current_model: state.current_model.clone(),
            current_effort: state.current_effort.clone(),
            current_message_id: state.current_message_id,
            current_message_seq: state.current_message_seq,
            pending_note: None,
            pending_note_seq: 0,
            used_note_seq: 0,
            pending_title: None,
            pending_summary: None,
            pending_messages: Vec::new(),
            last_user_message: None,
        }
    }

    fn to_state_row(&self, path: &Path) -> IngestStateRow {
        IngestStateRow {
            path: path.to_path_buf(),
            session_id: self.session_id.clone(),
            last_offset: self.last_offset,
            last_seen_input_tokens: self.last_seen.input_tokens,
            last_seen_cached_input_tokens: self.last_seen.cached_input_tokens,
            last_seen_output_tokens: self.last_seen.output_tokens,
            last_seen_reasoning_output_tokens: self.last_seen.reasoning_output_tokens,
            last_seen_total_tokens: self.last_seen.total_tokens,
            last_committed_input_tokens: self.last_committed.input_tokens,
            last_committed_cached_input_tokens: self.last_committed.cached_input_tokens,
            last_committed_output_tokens: self.last_committed.output_tokens,
            last_committed_reasoning_output_tokens: self.last_committed.reasoning_output_tokens,
            last_committed_total_tokens: self.last_committed.total_tokens,
            current_message_id: self.current_message_id,
            current_message_seq: self.current_message_seq,
            current_model: self.current_model.clone(),
            current_effort: self.current_effort.clone(),
        }
    }
}

pub struct IngestHandle {
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<Result<()>>,
}

impl IngestHandle {
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

pub async fn spawn(config: Arc<AppConfig>, storage: Storage) -> Result<IngestHandle> {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let root = config.sessions.root_dir.clone();
    let poll_interval = Duration::from_secs(config.sessions.poll_interval_secs.max(1));

    let mut ingestor = SessionIngestor::new(root, storage).await?;

    let join = tokio::spawn(async move {
        let mut ticker = time::interval(poll_interval);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(err) = ingestor.scan_once().await {
                        tracing::warn!(error = %err, "session ingest scan failed");
                    }
                }
                _ = &mut shutdown_rx => {
                    break;
                }
            }
        }
        Ok(())
    });

    Ok(IngestHandle {
        shutdown: Some(shutdown_tx),
        join,
    })
}

struct SessionIngestor {
    root: PathBuf,
    storage: Storage,
    files: HashMap<PathBuf, FileState>,
}

impl SessionIngestor {
    async fn new(root: PathBuf, storage: Storage) -> Result<Self> {
        let mut files = HashMap::new();
        let states = storage.load_ingest_state().await?;
        for state in states {
            files.insert(state.path.clone(), FileState::from_state(&state));
        }
        Ok(Self {
            root,
            storage,
            files,
        })
    }

    async fn scan_once(&mut self) -> Result<()> {
        if !self.root.exists() {
            tracing::debug!(root = %self.root.display(), "session root not found");
            return Ok(());
        }

        let mut paths = Vec::new();
        collect_jsonl_files(&self.root, &mut paths)?;

        for path in paths {
            self.process_file(&path).await?;
        }

        Ok(())
    }

    async fn process_file(&mut self, path: &Path) -> Result<()> {
        let metadata = match path.metadata() {
            Ok(meta) => meta,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "failed to stat session file");
                return Ok(());
            }
        };
        let len = metadata.len();
        let mut state = self.files.remove(path).unwrap_or_else(FileState::new);

        if len == state.last_offset {
            self.files.insert(path.to_path_buf(), state);
            return Ok(());
        }

        if len < state.last_offset {
            state.last_offset = 0;
            state.last_seen = TokenTotals::default();
            state.last_committed = TokenTotals::default();
            state.current_model = None;
            state.pending_note = None;
            state.pending_note_seq = 0;
            state.used_note_seq = 0;
            state.current_message_id = None;
            state.current_message_seq = 0;
            state.pending_messages.clear();
            state.last_user_message = None;
        }

        let (lines, new_offset) = read_new_lines(path, state.last_offset)?;
        if lines.is_empty() {
            self.files.insert(path.to_path_buf(), state);
            return Ok(());
        }

        let mut tx = self.storage.begin_tx().await?;

        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Err(err) = process_event(&self.storage, &mut tx, &mut state, &value).await {
                tracing::warn!(error = %err, path = %path.display(), "failed to process session event");
            }
        }

        state.last_offset = new_offset;
        let ingest_state = state.to_state_row(path);
        self.storage
            .update_ingest_activity_tx(&mut tx, Utc::now())
            .await?;
        self.storage.upsert_ingest_state_tx(&mut tx, &ingest_state).await?;
        tx.commit().await?;
        self.files.insert(path.to_path_buf(), state);
        Ok(())
    }
}

async fn process_event(
    storage: &Storage,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &mut FileState,
    value: &Value,
) -> Result<()> {
    let Some(kind) = value.get("type").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let timestamp = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(parse_timestamp);

    match kind {
        "session_meta" => {
            if let Some(meta) = parse_session_meta(value, timestamp) {
                state.session_id = Some(meta.session_id.clone());
                storage.upsert_session_meta_tx(tx, &meta).await?;
                if let Some(title) = state.pending_title.take() {
                    storage
                        .set_session_title_if_empty_tx(tx, &meta.session_id, &title)
                        .await?;
                }
                if let Some(summary) = state.pending_summary.take() {
                    storage
                        .set_session_summary_tx(tx, &meta.session_id, &summary, meta.last_event_at)
                        .await?;
                }
                if !state.pending_messages.is_empty() {
                    flush_pending_messages(
                        storage,
                        tx,
                        state,
                        &meta.session_id,
                        meta.last_event_at,
                    )
                        .await?;
                }
            }
        }
        "turn_context" => {
            if let Some(payload) = value.get("payload") {
                if let Some(model) = payload.get("model").and_then(|m| m.as_str()) {
                    state.current_model = Some(normalize_model_id(model));
                }
                state.current_effort = payload
                    .get("effort")
                    .and_then(|v| v.as_str())
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string());
            }
        }
        "event_msg" => {
            if let Some(payload) = value.get("payload") {
                match payload.get("type").and_then(|v| v.as_str()) {
                    Some("token_count") => {
                        handle_token_count(storage, tx, state, payload, timestamp).await?;
                    }
                    Some("user_message") => {
                        if let Some(message) = payload.get("message").and_then(|v| v.as_str()) {
                            handle_user_message(storage, tx, state, message, timestamp).await?;
                        }
                    }
                    Some("agent_message") => {
                        if let Some(message) = payload.get("message").and_then(|v| v.as_str()) {
                            if let Some(summary) = format_snippet(message, SUMMARY_MAX_CHARS) {
                                apply_summary(storage, tx, state, &summary, timestamp).await?;
                            }
                            if let Some(note) = format_snippet(message, SUMMARY_MAX_CHARS) {
                                state.pending_note = Some(note);
                                state.pending_note_seq = state.pending_note_seq.saturating_add(1);
                            }
                        }
                    }
                    Some("agent_reasoning") => {
                        if let Some(text) = payload.get("text").and_then(|v| v.as_str())
                            && let Some(note) = format_snippet(text, SUMMARY_MAX_CHARS)
                        {
                            state.pending_note = Some(format_reasoning_note(&note));
                            state.pending_note_seq = state.pending_note_seq.saturating_add(1);
                        }
                    }
                    _ => {}
                }
            }
        }
        "response_item" => {
            if let Some(payload) = value.get("payload") {
                match payload.get("type").and_then(|v| v.as_str()) {
                    Some("message") => {
                        if let Some(role) = payload.get("role").and_then(|v| v.as_str())
                            && let Some(text) = extract_message_text(payload)
                        {
                            if role.eq_ignore_ascii_case("user") {
                                handle_user_message(storage, tx, state, &text, timestamp).await?;
                            } else if role.eq_ignore_ascii_case("assistant") {
                                if let Some(summary) = format_snippet(&text, SUMMARY_MAX_CHARS) {
                                    apply_summary(storage, tx, state, &summary, timestamp).await?;
                                }
                                if let Some(note) = format_snippet(&text, SUMMARY_MAX_CHARS) {
                                    state.pending_note = Some(note);
                                    state.pending_note_seq =
                                        state.pending_note_seq.saturating_add(1);
                                }
                            }
                        }
                    }
                    Some("function_call") | Some("custom_tool_call") => {
                        if let Some(note) = tool_call_note(payload) {
                            state.pending_note = Some(note);
                            state.pending_note_seq = state.pending_note_seq.saturating_add(1);
                        }
                        if let Some(name) = payload.get("name").and_then(|v| v.as_str()) {
                            record_tool_event(storage, tx, state, timestamp, name).await?;
                        }
                    }
                    Some("web_search_call") => {
                        if let Some(note) = web_search_note(payload) {
                            state.pending_note = Some(note);
                            state.pending_note_seq = state.pending_note_seq.saturating_add(1);
                        }
                        record_tool_event(storage, tx, state, timestamp, "web_search").await?;
                    }
                    Some("local_shell_call") => {
                        if let Some(action_type) = payload
                            .get("action")
                            .and_then(|v| v.get("type"))
                            .and_then(|v| v.as_str())
                        {
                            record_tool_event(storage, tx, state, timestamp, action_type).await?;
                        } else {
                            record_tool_event(storage, tx, state, timestamp, "local_shell").await?;
                        }
                    }
                    Some("reasoning") => {
                        if let Some(note) = reasoning_summary_note(payload) {
                            state.pending_note = Some(note);
                            state.pending_note_seq = state.pending_note_seq.saturating_add(1);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    Ok(())
}

async fn apply_title(
    storage: &Storage,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &mut FileState,
    title: &str,
) -> Result<()> {
    let Some(session_id) = state.session_id.as_deref() else {
        state.pending_title = Some(title.to_string());
        return Ok(());
    };
    storage
        .set_session_title_if_empty_tx(tx, session_id, title)
        .await
}

async fn apply_summary(
    storage: &Storage,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &mut FileState,
    summary: &str,
    timestamp: Option<DateTime<Utc>>,
) -> Result<()> {
    let Some(session_id) = state.session_id.as_deref() else {
        state.pending_summary = Some(summary.to_string());
        return Ok(());
    };
    if let Some(ts) = timestamp {
        storage
            .set_session_summary_tx(tx, session_id, summary, ts)
            .await
    } else {
        storage
            .set_session_summary_only_tx(tx, session_id, summary)
            .await
    }
}

async fn handle_user_message(
    storage: &Storage,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &mut FileState,
    message: &str,
    timestamp: Option<DateTime<Utc>>,
) -> Result<()> {
    let Some(snippet) = format_snippet(message, TITLE_MAX_CHARS) else {
        return Ok(());
    };

    if should_skip_duplicate_message(state, &snippet, timestamp) {
        return Ok(());
    }

    state.current_message_seq = state.current_message_seq.saturating_add(1);
    let seq = state.current_message_seq;

    apply_title(storage, tx, state, &snippet).await?;

    if let Some(session_id) = state.session_id.as_deref() {
        let ts = timestamp.unwrap_or_else(Utc::now);
        let message_id = storage
            .record_user_message_tx(tx, session_id, ts, &snippet, seq)
            .await?;
        state.current_message_id = (message_id > 0).then_some(message_id);
    } else {
        state.pending_messages.push(PendingMessage {
            timestamp,
            snippet: snippet.clone(),
            seq,
        });
        state.current_message_id = None;
    }

    state.last_user_message = Some(LastUserMessage { timestamp, snippet });
    Ok(())
}

async fn flush_pending_messages(
    storage: &Storage,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &mut FileState,
    session_id: &str,
    fallback_timestamp: DateTime<Utc>,
) -> Result<()> {
    let pending = std::mem::take(&mut state.pending_messages);
    let mut latest_id = state.current_message_id;
    for message in pending {
        let ts = message.timestamp.unwrap_or(fallback_timestamp);
        let message_id = storage
            .record_user_message_tx(tx, session_id, ts, &message.snippet, message.seq)
            .await?;
        if message_id > 0 {
            latest_id = Some(message_id);
        }
    }
    state.current_message_id = latest_id;
    Ok(())
}

fn should_skip_duplicate_message(
    state: &FileState,
    snippet: &str,
    timestamp: Option<DateTime<Utc>>,
) -> bool {
    let Some(last) = state.last_user_message.as_ref() else {
        return false;
    };
    if last.snippet != snippet {
        return false;
    }
    match (last.timestamp, timestamp) {
        (Some(prev), Some(current)) => {
            let delta = current.signed_duration_since(prev).num_seconds().abs();
            delta <= MESSAGE_DEDUPE_WINDOW_SECS
        }
        _ => true,
    }
}

async fn handle_token_count(
    storage: &Storage,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &mut FileState,
    payload: &Value,
    timestamp: Option<DateTime<Utc>>,
) -> Result<()> {
    let Some(info) = payload.get("info") else {
        return Ok(());
    };
    let context_window = info
        .get("model_context_window")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            info.get("model_context_window")
                .and_then(|v| v.as_i64())
                .map(|v| v as u64)
        });
    let totals_value = info.get("total_token_usage");
    let Some(totals_value) = totals_value else {
        return Ok(());
    };
    let Some(totals) = TokenTotals::from_value(totals_value) else {
        return Ok(());
    };

    if totals.any_decreased(state.last_seen) {
        state.last_seen = totals;
        state.last_committed = totals;
        return Ok(());
    }

    if totals == state.last_seen {
        return Ok(());
    }

    state.last_seen = totals;

    let Some(model) = state.current_model.as_deref() else {
        return Ok(());
    };
    let session_id = match state.session_id.as_deref() {
        Some(id) => id,
        None => return Ok(()),
    };

    let delta = totals.saturating_sub(state.last_committed);
    if delta.is_zero() {
        return Ok(());
    }

    let Some(ts) = timestamp else {
        return Ok(());
    };
    let note = if state.pending_note_seq > state.used_note_seq {
        state.pending_note.as_deref()
    } else {
        None
    };
    storage
        .record_turn_tx(
            tx,
            session_id,
            ts,
            model,
            note,
            context_window,
            state.current_effort.as_deref(),
            delta.input_tokens,
            delta.cached_input_tokens,
            delta.output_tokens,
            delta.reasoning_output_tokens,
            delta.total_tokens,
            state.current_message_id,
        )
        .await?;

    state.last_committed = totals;
    if state.pending_note_seq > state.used_note_seq {
        state.used_note_seq = state.pending_note_seq;
    }
    Ok(())
}

fn collect_jsonl_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries = match path.read_dir() {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "failed to read session directory");
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(error = %err, "failed to read directory entry");
                    continue;
                }
            };
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn read_new_lines(path: &Path, offset: u64) -> Result<(Vec<String>, u64)> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open session file {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .with_context(|| "failed to seek session file")?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .with_context(|| "failed to read session file")?;
    if buf.is_empty() {
        return Ok((Vec::new(), offset));
    }

    let mut last_newline = None;
    for (idx, byte) in buf.iter().enumerate() {
        if *byte == b'\n' {
            last_newline = Some(idx);
        }
    }

    let Some(last_newline) = last_newline else {
        return Ok((Vec::new(), offset));
    };

    let slice = &buf[..=last_newline];
    let text = String::from_utf8_lossy(slice);
    let mut lines = Vec::new();
    for line in text.split_terminator('\n') {
        lines.push(line.to_string());
    }

    let new_offset = offset + (last_newline as u64) + 1;
    Ok((lines, new_offset))
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_session_meta(value: &Value, fallback_ts: Option<DateTime<Utc>>) -> Option<SessionMeta> {
    let payload = value.get("payload")?;
    let session_id = payload.get("id")?.as_str()?.to_string();
    let ts = payload
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(parse_timestamp)
        .or(fallback_ts)?;

    Some(SessionMeta {
        session_id,
        started_at: ts,
        last_event_at: ts,
        cwd: payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        repo_url: payload
            .get("git")
            .and_then(|v| v.get("repository_url"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        repo_branch: payload
            .get("git")
            .and_then(|v| v.get("branch"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        repo_commit: payload
            .get("git")
            .and_then(|v| v.get("commit_hash"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        model_provider: payload
            .get("model_provider")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        subagent: payload
            .get("source")
            .and_then(|v| v.get("subagent"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        last_model: None,
    })
}

fn extract_message_text(payload: &Value) -> Option<String> {
    let content = payload.get("content")?;
    if let Some(arr) = content.as_array() {
        let mut acc = String::new();
        for entry in arr {
            if let Some(text) = entry.get("text").and_then(|v| v.as_str())
                && !text.trim().is_empty()
            {
                if !acc.is_empty() {
                    acc.push(' ');
                }
                acc.push_str(text.trim());
            }
        }
        if acc.is_empty() { None } else { Some(acc) }
    } else {
        content.as_str().map(|text| text.to_string())
    }
}

fn format_snippet(text: &str, max_chars: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let filtered = filter_title_candidate(trimmed)?;
    let mut collapsed = String::new();
    for word in filtered.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= max_chars {
        return Some(collapsed);
    }
    let mut truncated = String::new();
    for ch in collapsed.chars().take(max_chars.saturating_sub(1)) {
        truncated.push(ch);
    }
    let trimmed = truncated.trim_end().to_string();
    let mut result = if trimmed.is_empty() {
        truncated
    } else {
        trimmed
    };
    result.push('…');
    Some(result)
}

fn normalize_model_id(model: &str) -> String {
    let trimmed = model.trim();
    if let Some(rest) = trimmed.strip_prefix("openai/") {
        rest.trim().to_string()
    } else if let Some(rest) = trimmed.strip_prefix("openai.") {
        rest.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn filter_title_candidate(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("<environment_context>")
        || lower.contains("<environment_context>")
        || lower.contains("# agents.md instructions")
        || lower.contains("<instructions>")
        || lower.contains("<user_instructions>")
        || lower.contains("<system_instructions>")
        || lower.contains("<developer_instructions>")
        || lower.contains("<system>")
    {
        return None;
    }
    Some(text.to_string())
}

fn tool_call_note(payload: &Value) -> Option<String> {
    let name = payload.get("name").and_then(|v| v.as_str())?;
    let mut detail = None;

    if let Some(args) = payload.get("arguments").and_then(|v| v.as_str()) {
        if let Ok(parsed) = serde_json::from_str::<Value>(args) {
            if let Some(command) = parsed.get("command").and_then(|v| v.as_str()) {
                detail = format_snippet(command, SUMMARY_MAX_CHARS);
            } else if let Some(query) = parsed.get("query").and_then(|v| v.as_str()) {
                detail = format_snippet(query, SUMMARY_MAX_CHARS);
            } else if let Some(path) = parsed.get("path").and_then(|v| v.as_str()) {
                detail = format_snippet(path, SUMMARY_MAX_CHARS);
            } else if let Some(input) = parsed.get("input").and_then(|v| v.as_str()) {
                detail = format_snippet(input, SUMMARY_MAX_CHARS);
            } else if let Some(code) = parsed.get("code").and_then(|v| v.as_str()) {
                detail = format_snippet(code, SUMMARY_MAX_CHARS);
            }
        } else {
            detail = format_snippet(args, SUMMARY_MAX_CHARS);
        }
    } else if let Some(input) = payload.get("input").and_then(|v| v.as_str()) {
        detail = format_snippet(input, SUMMARY_MAX_CHARS);
    }

    if let Some(detail) = detail {
        Some(format!("tool: {} ({})", name, detail))
    } else {
        Some(format!("tool: {}", name))
    }
}

fn web_search_note(payload: &Value) -> Option<String> {
    let query = payload
        .get("action")
        .and_then(|v| v.get("query"))
        .and_then(|v| v.as_str());
    let snippet = query.and_then(|q| format_snippet(q, SUMMARY_MAX_CHARS));
    if let Some(snippet) = snippet {
        Some(format!("web_search: {}", snippet))
    } else {
        Some("web_search".to_string())
    }
}

fn reasoning_summary_note(payload: &Value) -> Option<String> {
    if let Some(summary) = payload.get("summary").and_then(|v| v.as_array()) {
        for entry in summary {
            if let Some(text) = entry.get("text").and_then(|v| v.as_str())
                && let Some(snippet) = format_snippet(text, SUMMARY_MAX_CHARS)
            {
                return Some(format_reasoning_note(&snippet));
            } else if let Some(text) = entry.as_str()
                && let Some(snippet) = format_snippet(text, SUMMARY_MAX_CHARS)
            {
                return Some(format_reasoning_note(&snippet));
            }
        }
    }

    if let Some(content) = payload.get("content").and_then(|v| v.as_array()) {
        for entry in content {
            if let Some(text) = entry.get("text").and_then(|v| v.as_str())
                && let Some(snippet) = format_snippet(text, SUMMARY_MAX_CHARS)
            {
                return Some(format_reasoning_note(&snippet));
            }
        }
    }

    None
}

fn format_reasoning_note(text: &str) -> String {
    let trimmed = text.trim();
    let stripped = trimmed.trim_matches('*').trim_matches('_').trim();
    if stripped.is_empty() {
        format!("reasoning: {}", trimmed)
    } else {
        format!("reasoning: {}", stripped)
    }
}

async fn record_tool_event(
    storage: &Storage,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &FileState,
    timestamp: Option<DateTime<Utc>>,
    tool_name: &str,
) -> Result<()> {
    let Some(session_id) = state.session_id.as_deref() else {
        return Ok(());
    };
    let Some(ts) = timestamp else {
        return Ok(());
    };
    storage.record_tool_call_tx(tx, session_id, ts, tool_name).await
}

impl PartialEq for TokenTotals {
    fn eq(&self, other: &Self) -> bool {
        self.input_tokens == other.input_tokens
            && self.cached_input_tokens == other.cached_input_tokens
            && self.output_tokens == other.output_tokens
            && self.reasoning_output_tokens == other.reasoning_output_tokens
            && self.total_tokens == other.total_tokens
    }
}

impl Eq for TokenTotals {}
