#[cfg(test)]
use std::sync::Arc;
use std::{
    env,
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use super::projection::ProjectionEventRecord;
use crate::presentation::{PresentationItem, Renderable, RenderedCell, TimedItem};

const PRESENTATION_LOG_ENV: &str = "HOLON_TUI_PRESENTATION_LOG";
const PRESENTATION_LOG_MAX_BYTES_ENV: &str = "HOLON_TUI_PRESENTATION_LOG_MAX_BYTES";
const DEFAULT_PRESENTATION_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Maximum number of reducer event summaries to include in a single
/// `presentation.jsonl` record before truncation.
const MAX_REDUCER_EVENT_SUMMARIES: usize = 100;

#[derive(Debug, Clone)]
pub(crate) struct TuiLogWriter {
    root: PathBuf,
    presentation_logging_enabled: bool,
    presentation_log_max_bytes: u64,
    #[cfg(test)]
    _tempdir: Option<Arc<tempfile::TempDir>>,
}

impl TuiLogWriter {
    pub(crate) fn new(log_root: impl Into<PathBuf>) -> Result<Self> {
        let root = log_root.into().join("tui");
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        Ok(Self {
            root,
            presentation_logging_enabled: presentation_logging_enabled_from_env(),
            presentation_log_max_bytes: presentation_log_max_bytes_from_env(),
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
            presentation_logging_enabled: false,
            presentation_log_max_bytes: DEFAULT_PRESENTATION_LOG_MAX_BYTES,
            _tempdir: Some(tempdir),
        })
    }

    #[cfg(test)]
    pub(crate) fn new_temp_with_presentation_logging(max_bytes: u64) -> Result<Self> {
        Ok(Self::new_temp()?.with_presentation_logging(max_bytes))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn with_presentation_logging(mut self, max_bytes: u64) -> Self {
        self.presentation_logging_enabled = true;
        self.presentation_log_max_bytes = max_bytes;
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn write_event(&self, event: &ProjectionEventRecord) -> Result<()> {
        if is_error_log_event(&event.kind) {
            append_jsonl(
                &self.root.join("errors.jsonl"),
                &PersistedTuiEventLogRecord::from_event(event),
            )?;
        }
        if is_turn_log_event(&event.kind) {
            append_jsonl(
                &self.root.join("turns.jsonl"),
                &PersistedTuiEventLogRecord::from_event(event),
            )?;
        }

        Ok(())
    }

    pub(crate) fn write_presentation_items(
        &self,
        reducer_events: &[ProjectionEventRecord],
        items: &[TimedItem],
    ) -> Result<()> {
        if !self.presentation_logging_enabled || items.is_empty() {
            return Ok(());
        }
        let path = self.root.join("presentation.jsonl");
        let lines = items
            .iter()
            .map(|item| {
                serde_json::to_string(&PersistedPresentationLogRecord::from_timed_item(
                    reducer_events,
                    item,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        append_bounded_jsonl_lines(&path, lines.as_slice(), self.presentation_log_max_bytes)?;

        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct PersistedTuiEventLogRecord<'a> {
    ts: DateTime<Utc>,
    event_id: &'a str,
    event_seq: u64,
    kind: &'a str,
    summary: &'a str,
    payload: &'a Value,
}

impl<'a> PersistedTuiEventLogRecord<'a> {
    fn from_event(event: &'a ProjectionEventRecord) -> Self {
        Self {
            ts: event.ts,
            event_id: &event.id,
            event_seq: event.event_seq,
            kind: &event.kind,
            summary: &event.summary,
            payload: &event.payload,
        }
    }
}

#[derive(Debug, Serialize)]
struct PersistedPresentationLogRecord<'a> {
    ts: DateTime<Utc>,
    item_kind: &'static str,
    min_display_level: u8,
    reducer_event_ids: Vec<&'a str>,
    reducer_event_kinds: Vec<&'a str>,
    reducer_event_seqs: Vec<u64>,
    reducer_event_summaries: Vec<&'a str>,
    displays: Vec<PersistedDisplayRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reducer_event_summaries_truncated: Option<String>,
}

impl<'a> PersistedPresentationLogRecord<'a> {
    fn from_timed_item(reducer_events: &'a [ProjectionEventRecord], item: &TimedItem) -> Self {
        Self {
            ts: item.ts,
            item_kind: presentation_item_kind(&item.item),
            min_display_level: item.item.min_display_level(),
            reducer_event_ids: if MAX_REDUCER_EVENT_SUMMARIES == 0 {
                Vec::new()
            } else {
                reducer_events
                    .iter()
                    .take(MAX_REDUCER_EVENT_SUMMARIES)
                    .map(|e| e.id.as_str())
                    .collect()
            },
            reducer_event_kinds: if MAX_REDUCER_EVENT_SUMMARIES == 0 {
                Vec::new()
            } else {
                reducer_events
                    .iter()
                    .take(MAX_REDUCER_EVENT_SUMMARIES)
                    .map(|e| e.kind.as_str())
                    .collect()
            },
            reducer_event_seqs: if MAX_REDUCER_EVENT_SUMMARIES == 0 {
                Vec::new()
            } else {
                reducer_events
                    .iter()
                    .take(MAX_REDUCER_EVENT_SUMMARIES)
                    .map(|e| e.event_seq)
                    .collect()
            },
            reducer_event_summaries: if MAX_REDUCER_EVENT_SUMMARIES == 0 {
                Vec::new()
            } else {
                reducer_events
                    .iter()
                    .take(MAX_REDUCER_EVENT_SUMMARIES)
                    .map(|e| e.summary.as_str())
                    .collect()
            },
            reducer_event_summaries_truncated: if MAX_REDUCER_EVENT_SUMMARIES > 0
                && reducer_events.len() > MAX_REDUCER_EVENT_SUMMARIES
            {
                Some(format!(
                    "...and {} more",
                    reducer_events.len() - MAX_REDUCER_EVENT_SUMMARIES
                ))
            } else {
                None
            },
            displays: [3, 4, 5]
                .into_iter()
                .map(|display_level| PersistedDisplayRecord::from_item(display_level, &item.item))
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PersistedDisplayRecord {
    display_level: u8,
    decision: &'static str,
    cells: Vec<PersistedRenderedCell>,
}

impl PersistedDisplayRecord {
    fn from_item(display_level: u8, item: &PresentationItem) -> Self {
        let cells = item.render(display_level);
        Self {
            display_level,
            decision: if cells.is_empty() { "hidden" } else { "shown" },
            cells: cells
                .iter()
                .map(PersistedRenderedCell::from_rendered_cell)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PersistedRenderedCell {
    speaker: String,
    body_preview: String,
    body_char_count: usize,
    body_line_count: usize,
    is_live: bool,
    indent_level: u8,
}

impl PersistedRenderedCell {
    fn from_rendered_cell(cell: &RenderedCell) -> Self {
        Self {
            speaker: cell.speaker.clone(),
            body_preview: preview_text(&cell.body, 512),
            body_char_count: cell.body.chars().count(),
            body_line_count: cell.body_lines.len(),
            is_live: cell.is_live,
            indent_level: cell.indent_level,
        }
    }
}

fn preview_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut preview = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    preview.push('…');
    preview
}

fn presentation_item_kind(item: &PresentationItem) -> &'static str {
    match item {
        PresentationItem::UserMessage { .. } => "user_message",
        PresentationItem::AssistantResult { .. } => "assistant_result",
        PresentationItem::SystemAlert { .. } => "system_alert",
        PresentationItem::WaitingNotice { .. } => "waiting_notice",
        PresentationItem::WorkItemCard { .. } => "work_item_card",
        PresentationItem::AssistantProgress { .. } => "assistant_progress",
        PresentationItem::ResumeNotice { .. } => "resume_notice",
        PresentationItem::ActionGroup { .. } => "action_group",
        PresentationItem::CommandExecuted { .. } => "command_executed",
        PresentationItem::FileRead { .. } => "file_read",
        PresentationItem::FileChange { .. } => "file_change",
        PresentationItem::PatchFailure { .. } => "patch_failure",
        PresentationItem::ToolAction { .. } => "tool_action",
        PresentationItem::PlanShown { .. } => "plan_shown",
        PresentationItem::ProviderRound { .. } => "provider_round",
        PresentationItem::InternalTransition { .. } => "internal_transition",
        PresentationItem::TaskLifecycle { .. } => "task_lifecycle",
        PresentationItem::WorkItemBookkeeping { .. } => "work_item_bookkeeping",
        PresentationItem::WorkspaceChange { .. } => "workspace_change",
        PresentationItem::ContinuationDetail { .. } => "continuation_detail",
        PresentationItem::GenericEvent { .. } => "generic_event",
    }
}

fn is_error_log_event(kind: &str) -> bool {
    matches!(kind, "runtime_error")
}

fn is_turn_log_event(kind: &str) -> bool {
    matches!(kind, "turn_terminal")
}

fn presentation_logging_enabled_from_env() -> bool {
    env::var(PRESENTATION_LOG_ENV)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on" | "debug"))
}

fn presentation_log_max_bytes_from_env() -> u64 {
    env::var(PRESENTATION_LOG_MAX_BYTES_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PRESENTATION_LOG_MAX_BYTES)
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

fn append_bounded_jsonl_lines(path: &Path, lines: &[String], max_bytes: u64) -> Result<()> {
    let mut current_size = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let mut writer: Option<BufWriter<std::fs::File>> = None;

    for line in lines {
        let incoming_bytes = line.len() as u64 + 1;
        if incoming_bytes > max_bytes {
            continue;
        }
        if current_size.saturating_add(incoming_bytes) > max_bytes {
            if let Some(mut open_writer) = writer.take() {
                open_writer.flush()?;
            }
            rotate_jsonl(path)?;
            current_size = 0;
        }
        if writer.is_none() {
            writer = Some(BufWriter::new(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("failed to open {}", path.display()))?,
            ));
        }
        if let Some(open_writer) = writer.as_mut() {
            writeln!(open_writer, "{line}")?;
        }
        current_size += incoming_bytes;
    }

    if let Some(mut open_writer) = writer {
        open_writer.flush()?;
    }
    Ok(())
}

fn rotate_jsonl(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let rotated = path.with_extension("jsonl.1");
    if rotated.exists() {
        fs::remove_file(&rotated)
            .with_context(|| format!("failed to remove {}", rotated.display()))?;
    }
    fs::rename(path, &rotated).with_context(|| {
        format!(
            "failed to rotate {} to {}",
            path.display(),
            rotated.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
