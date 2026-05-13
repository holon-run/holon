#[cfg(test)]
use std::sync::Arc;
use std::{
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

        Ok(())
    }

    pub(crate) fn write_presentation_items(
        &self,
        reducer_events: &[ProjectionEventRecord],
        items: &[TimedItem],
    ) -> Result<()> {
        let path = self.root.join("presentation.jsonl");
        let mut writer = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("failed to open {}", path.display()))?,
        );
        for item in items {
            let line = serde_json::to_string(&PersistedPresentationLogRecord::from_timed_item(
                reducer_events,
                item,
            ))?;
            writeln!(writer, "{line}")?;
        }
        writer.flush()?;

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
    fn presentation_log_records_display_decisions_without_raw_conversation_log() {
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
        let writer = TuiLogWriter::new_temp().unwrap();
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
}
