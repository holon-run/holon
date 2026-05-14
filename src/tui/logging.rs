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
        let mut writer = Self::new_temp()?;
        writer.presentation_logging_enabled = true;
        writer.presentation_log_max_bytes = max_bytes;
        Ok(writer)
    }

    #[cfg(test)]
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
    seq: u64,
    kind: &'a str,
    summary: &'a str,
    payload: &'a Value,
}

impl<'a> PersistedTuiEventLogRecord<'a> {
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
struct PersistedPresentationLogRecord<'a> {
    ts: DateTime<Utc>,
    item_kind: &'static str,
    min_display_level: u8,
    reducer_event_ids: Vec<&'a str>,
    reducer_event_kinds: Vec<&'a str>,
    reducer_event_seqs: Vec<u64>,
    reducer_event_summaries: Vec<&'a str>,
    displays: Vec<PersistedDisplayRecord>,
}

impl<'a> PersistedPresentationLogRecord<'a> {
    fn from_timed_item(reducer_events: &'a [ProjectionEventRecord], item: &TimedItem) -> Self {
        Self {
            ts: item.ts,
            item_kind: presentation_item_kind(&item.item),
            min_display_level: item.item.min_display_level(),
            reducer_event_ids: reducer_events
                .iter()
                .map(|event| event.id.as_str())
                .collect(),
            reducer_event_kinds: reducer_events
                .iter()
                .map(|event| event.kind.as_str())
                .collect(),
            reducer_event_seqs: reducer_events.iter().map(|event| event.seq).collect(),
            reducer_event_summaries: reducer_events
                .iter()
                .map(|event| event.summary.as_str())
                .collect(),
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
        PresentationItem::ActionGroup { .. } => "action_group",
        PresentationItem::CommandExecuted { .. } => "command_executed",
        PresentationItem::FileRead { .. } => "file_read",
        PresentationItem::FileChange { .. } => "file_change",
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
mod tests {
    use super::*;
    use crate::operator_event::{
        OperatorEventCategory, OperatorEventPresentation, OperatorVisibility,
    };
    use crate::presentation::Outcome;
    use crate::tui::projection::{ProjectionEventLane, ProjectionEventRecord};
    use serde_json::json;

    #[test]
    fn new_uses_log_root_not_agent_root() {
        let tempdir = tempfile::tempdir().unwrap();
        let log_root = tempdir.path().join("logs");

        let writer = TuiLogWriter::new(&log_root).unwrap();

        assert_eq!(writer.root, log_root.join("tui"));
        assert!(writer.root.exists());
        assert!(
            !tempdir
                .path()
                .join("agents")
                .join("logs")
                .join("tui")
                .exists(),
            "TUI diagnostics must not create a pseudo agent under agents/logs"
        );
    }

    #[test]
    fn presentation_log_is_disabled_by_default() {
        let writer = TuiLogWriter::new_temp().unwrap();
        let event = ProjectionEventRecord {
            id: "evt_1".into(),
            seq: 7,
            ts: Utc::now(),
            kind: "brief_created".into(),
            lane: ProjectionEventLane::Timeline,
            summary: "completed work".into(),
            presentation: OperatorEventPresentation {
                visibility: OperatorVisibility::TurnResult,
                category: OperatorEventCategory::Brief,
                title: "Holon".into(),
                body: Some("completed work".into()),
                summary: "completed work".into(),
                source_event_kind: "brief_created".into(),
            },
            payload: json!({ "id": "brief_1", "text": "completed work" }),
        };
        let item = TimedItem {
            ts: event.ts,
            item: PresentationItem::AssistantResult {
                brief_id: Some("brief_1".into()),
                body: "completed work".into(),
                outcome: Outcome::Success,
            },
        };

        writer
            .write_event(&event)
            .and_then(|_| writer.write_presentation_items(std::slice::from_ref(&event), &[item]))
            .unwrap();

        assert!(
            !writer.root.join("presentation.jsonl").exists(),
            "presentation logging is debug instrumentation and must default off"
        );
    }

    #[test]
    fn presentation_log_records_display_decisions_without_raw_conversation_log_when_enabled() {
        let writer = TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();
        let event = ProjectionEventRecord {
            id: "evt_1".into(),
            seq: 7,
            ts: Utc::now(),
            kind: "brief_created".into(),
            lane: ProjectionEventLane::Timeline,
            summary: "completed work".into(),
            presentation: OperatorEventPresentation {
                visibility: OperatorVisibility::TurnResult,
                category: OperatorEventCategory::Brief,
                title: "Holon".into(),
                body: Some("completed work".into()),
                summary: "completed work".into(),
                source_event_kind: "brief_created".into(),
            },
            payload: json!({ "id": "brief_1", "text": "completed work" }),
        };
        let item = TimedItem {
            ts: event.ts,
            item: PresentationItem::AssistantResult {
                brief_id: Some("brief_1".into()),
                body: "completed work".into(),
                outcome: Outcome::Success,
            },
        };

        writer
            .write_event(&event)
            .and_then(|_| writer.write_presentation_items(std::slice::from_ref(&event), &[item]))
            .unwrap();

        let presentation_path = writer.root.join("presentation.jsonl");
        let line = fs::read_to_string(&presentation_path).unwrap();
        let record: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["item_kind"], "assistant_result");
        assert_eq!(record["min_display_level"], 3);
        assert_eq!(record["reducer_event_ids"], json!(["evt_1"]));
        assert_eq!(record["displays"][0]["display_level"], 3);
        assert_eq!(record["displays"][0]["decision"], "shown");
        assert_eq!(record["displays"][0]["cells"][0]["speaker"], "Holon");
        assert_eq!(
            record["displays"][0]["cells"][0]["body_preview"],
            "✓ completed work"
        );
        assert!(
            !writer.root.join("conversation.jsonl").exists(),
            "TUI should not duplicate raw conversation events"
        );
    }

    #[test]
    fn presentation_log_marks_lower_display_levels_hidden() {
        let writer = TuiLogWriter::new_temp_with_presentation_logging(4096).unwrap();
        let event = ProjectionEventRecord {
            id: "evt_debug".into(),
            seq: 9,
            ts: Utc::now(),
            kind: "provider_round_completed".into(),
            lane: ProjectionEventLane::Debug,
            summary: "provider completed".into(),
            presentation: OperatorEventPresentation {
                visibility: OperatorVisibility::Trace,
                category: OperatorEventCategory::Trace,
                title: "Provider".into(),
                body: None,
                summary: "provider completed".into(),
                source_event_kind: "provider_round_completed".into(),
            },
            payload: json!({ "model": "test-model" }),
        };
        let item = TimedItem {
            ts: event.ts,
            item: PresentationItem::GenericEvent {
                kind: "provider_round_completed".into(),
                summary: "provider completed".into(),
            },
        };

        writer
            .write_presentation_items(std::slice::from_ref(&event), &[item])
            .unwrap();

        let line = fs::read_to_string(writer.root.join("presentation.jsonl")).unwrap();
        let record: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["min_display_level"], 5);
        assert_eq!(record["displays"][0]["decision"], "hidden");
        assert_eq!(record["displays"][1]["decision"], "hidden");
        assert_eq!(record["displays"][2]["decision"], "shown");
    }

    #[test]
    fn presentation_log_rotates_when_enabled_debug_log_reaches_limit() {
        let writer = TuiLogWriter::new_temp_with_presentation_logging(1800).unwrap();
        let event = ProjectionEventRecord {
            id: "evt_debug".into(),
            seq: 9,
            ts: Utc::now(),
            kind: "provider_round_completed".into(),
            lane: ProjectionEventLane::Debug,
            summary: "provider completed".into(),
            presentation: OperatorEventPresentation {
                visibility: OperatorVisibility::Trace,
                category: OperatorEventCategory::Trace,
                title: "Provider".into(),
                body: None,
                summary: "provider completed".into(),
                source_event_kind: "provider_round_completed".into(),
            },
            payload: json!({ "model": "test-model" }),
        };
        let item = TimedItem {
            ts: event.ts,
            item: PresentationItem::GenericEvent {
                kind: "provider_round_completed".into(),
                summary: "provider completed".into(),
            },
        };

        for _ in 0..8 {
            writer
                .write_presentation_items(std::slice::from_ref(&event), std::slice::from_ref(&item))
                .unwrap();
        }

        let path = writer.root.join("presentation.jsonl");
        let rotated = writer.root.join("presentation.jsonl.1");
        assert!(path.exists());
        assert!(rotated.exists());
        assert!(path.metadata().unwrap().len() <= writer.presentation_log_max_bytes);
        assert!(rotated.metadata().unwrap().len() <= writer.presentation_log_max_bytes);
    }

    #[test]
    fn presentation_log_drops_single_records_larger_than_limit() {
        let writer = TuiLogWriter::new_temp_with_presentation_logging(128).unwrap();
        let event = ProjectionEventRecord {
            id: "evt_large".into(),
            seq: 9,
            ts: Utc::now(),
            kind: "provider_round_completed".into(),
            lane: ProjectionEventLane::Debug,
            summary: "provider completed".repeat(100),
            presentation: OperatorEventPresentation {
                visibility: OperatorVisibility::Trace,
                category: OperatorEventCategory::Trace,
                title: "Provider".into(),
                body: None,
                summary: "provider completed".repeat(100),
                source_event_kind: "provider_round_completed".into(),
            },
            payload: json!({ "model": "test-model" }),
        };
        let item = TimedItem {
            ts: event.ts,
            item: PresentationItem::GenericEvent {
                kind: "provider_round_completed".into(),
                summary: "provider completed".repeat(100),
            },
        };

        writer
            .write_presentation_items(std::slice::from_ref(&event), std::slice::from_ref(&item))
            .unwrap();

        assert!(!writer.root.join("presentation.jsonl").exists());
    }

    #[test]
    fn turn_terminal_records_are_routed_to_turns_not_errors() {
        let writer = TuiLogWriter::new_temp().unwrap();
        let event = ProjectionEventRecord {
            id: "evt_terminal".into(),
            seq: 10,
            ts: Utc::now(),
            kind: "turn_terminal".into(),
            lane: ProjectionEventLane::Debug,
            summary: "turn completed".into(),
            presentation: OperatorEventPresentation {
                visibility: OperatorVisibility::Trace,
                category: OperatorEventCategory::Trace,
                title: "Turn".into(),
                body: None,
                summary: "turn completed".into(),
                source_event_kind: "turn_terminal".into(),
            },
            payload: json!({ "kind": "completed" }),
        };

        writer.write_event(&event).unwrap();

        assert!(!writer.root.join("errors.jsonl").exists());
        let line = fs::read_to_string(writer.root.join("turns.jsonl")).unwrap();
        let record: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["kind"], "turn_terminal");
    }

    #[test]
    fn runtime_errors_are_routed_to_errors_not_turns() {
        let writer = TuiLogWriter::new_temp().unwrap();
        let event = ProjectionEventRecord {
            id: "evt_error".into(),
            seq: 11,
            ts: Utc::now(),
            kind: "runtime_error".into(),
            lane: ProjectionEventLane::Debug,
            summary: "runtime failed".into(),
            presentation: OperatorEventPresentation {
                visibility: OperatorVisibility::Trace,
                category: OperatorEventCategory::Trace,
                title: "Runtime".into(),
                body: None,
                summary: "runtime failed".into(),
                source_event_kind: "runtime_error".into(),
            },
            payload: json!({ "error": "failed" }),
        };

        writer.write_event(&event).unwrap();

        assert!(!writer.root.join("turns.jsonl").exists());
        let line = fs::read_to_string(writer.root.join("errors.jsonl")).unwrap();
        let record: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["kind"], "runtime_error");
    }
}
