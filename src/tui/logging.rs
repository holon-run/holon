#[cfg(test)]
use std::sync::Arc;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use super::chat::{conversation_event_body, is_chat_visible_conversation_event};
use super::projection::ProjectionEventRecord;

#[derive(Debug, Clone)]
pub(crate) struct TuiLogWriter {
    root: PathBuf,
    #[cfg(test)]
    _tempdir: Option<Arc<tempfile::TempDir>>,
}

impl TuiLogWriter {
    pub(crate) fn new(agent_home: impl Into<PathBuf>) -> Result<Self> {
        let root = agent_home.into().join("logs").join("tui");
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        Ok(Self {
            root,
            #[cfg(test)]
            _tempdir: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_temp() -> Result<Self> {
        let tempdir = Arc::new(tempfile::tempdir()?);
        let root = tempdir.path().join("logs").join("tui");
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        Ok(Self {
            root,
            _tempdir: Some(tempdir),
        })
    }

    pub(crate) fn write_event(&self, event: &ProjectionEventRecord) -> Result<()> {
        if is_error_log_event(&event.kind) {
            append_jsonl(
                &self.root.join("errors.jsonl"),
                &PersistedErrorLogRecord::from_event(event),
            )?;
        }

        if is_chat_visible_conversation_event(&event.kind) {
            append_jsonl(
                &self.root.join("conversation.jsonl"),
                &PersistedConversationLogRecord::from_event(event),
            )?;
        }

        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct PersistedErrorLogRecord<'a> {
    ts: DateTime<Utc>,
    event_id: &'a str,
    seq: u64,
    kind: &'a str,
    summary: &'a str,
    payload: &'a Value,
}

impl<'a> PersistedErrorLogRecord<'a> {
    fn from_event(event: &'a ProjectionEventRecord) -> Self {
        Self {
            ts: event.ts,
            event_id: &event.id,
            seq: event.seq,
            kind: &event.kind,
            summary: &event.summary,
            payload: &event.payload,
        }
    }
}

#[derive(Debug, Serialize)]
struct PersistedConversationLogRecord<'a> {
    ts: DateTime<Utc>,
    event_id: &'a str,
    seq: u64,
    kind: &'a str,
    speaker: String,
    body: String,
    payload: &'a Value,
}

impl<'a> PersistedConversationLogRecord<'a> {
    fn from_event(event: &'a ProjectionEventRecord) -> Self {
        Self {
            ts: event.ts,
            event_id: &event.id,
            seq: event.seq,
            kind: &event.kind,
            speaker: conversation_log_speaker(&event.kind),
            body: conversation_event_body(event),
            payload: &event.payload,
        }
    }
}

fn conversation_log_speaker(kind: &str) -> String {
    match kind {
        "runtime_error" | "turn_terminal" => "System (runtime)".into(),
        "provider_round_completed" | "text_only_round_observed" | "max_output_tokens_recovery" => {
            "System".into()
        }
        _ => "System".into(),
    }
}

fn is_error_log_event(kind: &str) -> bool {
    matches!(kind, "runtime_error" | "turn_terminal")
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let line = serde_json::to_string(value)?;
    writeln!(file, "{line}")?;
    Ok(())
}
