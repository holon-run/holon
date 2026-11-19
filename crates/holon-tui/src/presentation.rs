//! Presentation item model and reducer.
//!
//! Converts raw `ProjectionEventRecord` events into typed `PresentationItem`
//! values that render differently at each display level (info=3, verbose=4, debug=5).
//!
//! See `docs/rfcs/presentation-item-model-and-renderer.md`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::operator_event::OperatorEventCategory;
use crate::tui::projection::ProjectionEventRecord;
use crate::types::{
    BriefCreatedAuditEvent, BriefKind, BriefRecord, MessageBody, MessageEnvelope, MessageOrigin,
    WorkItemLifecycleAuditEvent, WorkItemRecord,
};

pub(crate) const LIVE_WORKING_ACTIVITY_DISPLAY_LEVEL: u8 = 4;

// ── Supporting types ───────────────────────────────────────────────────────

/// Summary of a diff hunk: start line and line count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkSummary {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub added_count: u32,
    pub removed_count: u32,
}

/// File action kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl FileAction {
    pub fn symbol(self) -> &'static str {
        match self {
            FileAction::Added => "A",
            FileAction::Modified => "M",
            FileAction::Deleted => "D",
            FileAction::Renamed => "R",
        }
    }
}

/// Token count from a provider round.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenCount {
    pub input: u64,
    pub output: u64,
}

/// Task lifecycle transition kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskTransition {
    Created,
    StatusUpdated,
    InputDelivered,
    ResultReceived,
    ChildSpawned,
    RecoveryFailed,
    RunnerFailed,
    Stopped,
}

impl TaskTransition {
    pub fn label(&self) -> &'static str {
        match self {
            TaskTransition::Created => "created",
            TaskTransition::StatusUpdated => "status updated",
            TaskTransition::InputDelivered => "input delivered",
            TaskTransition::ResultReceived => "result received",
            TaskTransition::ChildSpawned => "child spawned",
            TaskTransition::RecoveryFailed => "recovery failed",
            TaskTransition::RunnerFailed => "runner failed",
            TaskTransition::Stopped => "stopped",
        }
    }
}

/// Work item lifecycle transition (bookkeeping level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkItemTransition {
    Created,
    Updated,
    Completed,
    Picked,
    FocusReleased,
    EnqueueRequested,
    Waiting,
    WaitResolved,
    WaitCancelled,
    ReminderInjected,
    ReminderSkipped,
    DelegationCreated,
    DelegationCompleted,
    TurnEndCommitted,
    TurnEndSkipped,
}

impl WorkItemTransition {
    pub fn label(&self) -> &'static str {
        match self {
            WorkItemTransition::Created => "created",
            WorkItemTransition::Updated => "updated",
            WorkItemTransition::Completed => "completed",
            WorkItemTransition::Picked => "picked",
            WorkItemTransition::FocusReleased => "focus released",
            WorkItemTransition::EnqueueRequested => "enqueued",
            WorkItemTransition::Waiting => "waiting",
            WorkItemTransition::WaitResolved => "wait resolved",
            WorkItemTransition::WaitCancelled => "wait cancelled",
            WorkItemTransition::ReminderInjected => "reminder injected",
            WorkItemTransition::ReminderSkipped => "reminder skipped",
            WorkItemTransition::DelegationCreated => "delegated",
            WorkItemTransition::DelegationCompleted => "delegation completed",
            WorkItemTransition::TurnEndCommitted => "turn-end committed",
            WorkItemTransition::TurnEndSkipped => "turn-end skipped",
        }
    }
}

/// Outcome of an action — success, error, or neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    Neutral,
    Unknown,
}

impl Outcome {
    pub fn symbol(&self) -> &'static str {
        match self {
            Outcome::Success => "\u{2713}",
            Outcome::Failure => "\u{2717}",
            Outcome::Neutral => "\u{2022}",
            Outcome::Unknown => "?",
        }
    }
}

/// Command presentation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatus {
    Started,
    Completed,
    Failed,
    PromotedToTask,
    AlreadyRunning,
}

/// State of a presentation item: stable or still evolving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemState {
    Stable,
    Live,
}

// ── TimedItem: PresentationItem with timestamp ────────────────────────────

/// A `PresentationItem` paired with its originating event timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedItem {
    pub item: PresentationItem,
    pub ts: DateTime<Utc>,
    pub event_seq: u64,
    pub turn_index: Option<u64>,
    pub dedupe_key: String,
}

impl TimedItem {
    pub(crate) fn from_event(item: PresentationItem, event: &ProjectionEventRecord) -> Self {
        Self {
            item,
            ts: event.ts,
            event_seq: event.event_seq,
            turn_index: event.payload.get("turn_index").and_then(Value::as_u64),
            dedupe_key: event_dedupe_key(event),
        }
    }

    pub(crate) fn with_key(
        item: PresentationItem,
        ts: DateTime<Utc>,
        dedupe_key: impl Into<String>,
    ) -> Self {
        Self {
            item,
            ts,
            event_seq: 0,
            turn_index: None,
            dedupe_key: dedupe_key.into(),
        }
    }

    pub(crate) fn with_event_key(
        item: PresentationItem,
        event: &ProjectionEventRecord,
        dedupe_key: impl Into<String>,
    ) -> Self {
        Self {
            item,
            ts: event.ts,
            event_seq: event.event_seq,
            turn_index: event.payload.get("turn_index").and_then(Value::as_u64),
            dedupe_key: dedupe_key.into(),
        }
    }
}

// ── RenderedCell: surface-neutral intermediate ─────────────────────────────

/// A surface-neutral cell ready for final rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCell {
    pub speaker: String,
    pub body: String,
    pub body_lines: Vec<String>,
    pub is_live: bool,
    pub indent_level: u8,
}

impl RenderedCell {
    pub fn new(speaker: impl Into<String>, body: impl Into<String>) -> Self {
        let body: String = body.into();
        let body_lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
        Self {
            speaker: speaker.into(),
            body,
            body_lines,
            is_live: false,
            indent_level: 0,
        }
    }

    pub fn live(speaker: impl Into<String>, body: impl Into<String>) -> Self {
        let mut cell = Self::new(speaker, body);
        cell.is_live = true;
        cell
    }

    pub fn indented(mut self, level: u8) -> Self {
        self.indent_level = level;
        self
    }
}

// ── PresentationItem ───────────────────────────────────────────────────────

/// Typed user-facing activity item.
///
/// Each variant carries its own `min_display_level()` and renders differently
/// at info=3, verbose=4, debug=5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentationItem {
    // ── Level 3+ (Info): result-oriented ──────────────────────────────────
    UserMessage {
        text: String,
    },
    AssistantResult {
        brief_id: Option<String>,
        body: String,
        outcome: Outcome,
    },
    SystemAlert {
        title: String,
        body: String,
    },
    WaitingNotice {
        reason: String,
    },
    WorkItemCard {
        action: String,
        summary: String,
    },

    // ── Level 4+ (Verbose): compact activity ─────────────────────────────
    AssistantProgress {
        text: String,
        state: ItemState,
    },
    ResumeNotice {
        reason: String,
        details: Option<String>,
    },
    ActionGroup {
        heading: String,
        items: Vec<PresentationItem>,
        state: ItemState,
    },
    CommandExecuted {
        status: CommandStatus,
        cmd_preview: String,
        task_id: Option<String>,
        duration_ms: Option<u64>,
        exit_code: Option<i32>,
        stdout_summary: String,
        full_stdout: Option<String>,
        full_stderr: Option<String>,
        error_message: Option<String>,
    },
    FileRead {
        path: String,
        summary: String,
    },
    FileChange {
        path: String,
        action: FileAction,
        hunks: Vec<HunkSummary>,
        full_diff: Option<String>,
    },
    PatchFailure {
        summary: String,
        error_message: Option<String>,
    },
    ToolAction {
        summary: String,
    },
    PlanShown {
        plan_text: String,
    },

    // ── Level 5+ (Debug): curated internals ───────────────────────────────
    ProviderRound {
        model: String,
        stop_reason: String,
        tokens: TokenCount,
    },
    InternalTransition {
        what: String,
        from: String,
        to: String,
    },
    TaskLifecycle {
        task_id: String,
        transition: TaskTransition,
    },
    WorkItemBookkeeping {
        item_id: String,
        transition: WorkItemTransition,
        summary: String,
    },
    WorkspaceChange {
        path: String,
        action: String,
    },
    ContinuationDetail {
        trigger: String,
        outcome: String,
    },
    GenericEvent {
        kind: String,
        summary: String,
    },
}

impl PresentationItem {
    /// Minimum display level at which this item should appear.
    pub fn min_display_level(&self) -> u8 {
        match self {
            PresentationItem::UserMessage { .. } => 3,
            PresentationItem::AssistantResult { .. } => 3,
            PresentationItem::SystemAlert { .. } => 3,
            PresentationItem::WaitingNotice { .. } => 3,
            PresentationItem::WorkItemCard { .. } => 3,

            PresentationItem::AssistantProgress { .. } => 4,
            PresentationItem::ResumeNotice { .. } => 4,
            PresentationItem::ActionGroup { .. } => 4,
            PresentationItem::CommandExecuted { .. } => 4,
            PresentationItem::FileRead { .. } => 4,
            PresentationItem::FileChange { .. } => 4,
            PresentationItem::PatchFailure { .. } => 4,
            PresentationItem::ToolAction { .. } => 4,
            PresentationItem::PlanShown { .. } => 4,
            PresentationItem::WorkItemBookkeeping { .. } => 4,

            PresentationItem::ProviderRound { .. } => 5,
            PresentationItem::InternalTransition { .. } => 5,
            PresentationItem::TaskLifecycle { .. } => 5,
            PresentationItem::WorkspaceChange { .. } => 5,
            PresentationItem::ContinuationDetail { .. } => 5,
            PresentationItem::GenericEvent { .. } => 5,
        }
    }

    pub fn is_visible_at(&self, level: u8) -> bool {
        level >= self.min_display_level()
    }

    pub fn is_live(&self) -> bool {
        matches!(
            self,
            PresentationItem::AssistantProgress {
                state: ItemState::Live,
                ..
            } | PresentationItem::ActionGroup {
                state: ItemState::Live,
                ..
            }
        )
    }

    pub(crate) fn is_live_working_activity_item(&self) -> bool {
        matches!(
            self,
            PresentationItem::AssistantProgress { .. }
                | PresentationItem::ActionGroup { .. }
                | PresentationItem::CommandExecuted { .. }
                | PresentationItem::FileRead { .. }
                | PresentationItem::FileChange { .. }
                | PresentationItem::PatchFailure { .. }
                | PresentationItem::ToolAction { .. }
                | PresentationItem::PlanShown { .. }
        )
    }
}

pub(crate) fn render_live_working_activity_text(event: &ProjectionEventRecord) -> Option<String> {
    let mut reducer = PresentationReducer::new();
    let empty_lookup = BriefTextLookup(&std::collections::BTreeMap::new());
    let items = reducer.reduce(&[event.clone()], &empty_lookup);
    items.iter().find_map(|timed| {
        if timed.item.is_live_working_activity_item()
            && timed
                .item
                .is_visible_at(LIVE_WORKING_ACTIVITY_DISPLAY_LEVEL)
        {
            timed
                .item
                .render(LIVE_WORKING_ACTIVITY_DISPLAY_LEVEL)
                .into_iter()
                .next()
                .map(|cell| cell.body)
        } else {
            None
        }
    })
}

// ── Renderable trait ───────────────────────────────────────────────────────

pub trait Renderable {
    fn min_display_level(&self) -> u8;
    fn render(&self, level: u8) -> Vec<RenderedCell>;
}

impl Renderable for PresentationItem {
    fn min_display_level(&self) -> u8 {
        PresentationItem::min_display_level(self)
    }

    fn render(&self, level: u8) -> Vec<RenderedCell> {
        if level < self.min_display_level() {
            return vec![];
        }

        match self {
            PresentationItem::UserMessage { text } => {
                vec![RenderedCell {
                    speaker: "You".into(),
                    body: text.clone(),
                    body_lines: text.lines().map(|l| l.to_string()).collect(),
                    is_live: false,
                    indent_level: 0,
                }]
            }

            PresentationItem::AssistantResult { body, outcome, .. } => {
                vec![RenderedCell::new(
                    "Holon",
                    format!("{} {}", outcome.symbol(), body),
                )]
            }

            PresentationItem::SystemAlert { title, body } => {
                vec![RenderedCell::new(
                    "System",
                    format!("\u{26a0} {} \u{2014} {}", title, body),
                )]
            }

            PresentationItem::WaitingNotice { reason } => {
                vec![RenderedCell::new("Holon", format!("\u{23f3} {}", reason))]
            }

            PresentationItem::WorkItemCard { action, summary } => {
                vec![RenderedCell::new(
                    "Holon",
                    format!("\u{2022} {}: {}", action, summary),
                )]
            }

            // ── Level 4+ ────────────────────────────────────────────────
            PresentationItem::AssistantProgress { text, state } => {
                let cell = if *state == ItemState::Live {
                    RenderedCell::live("Holon", text.clone())
                } else {
                    RenderedCell::new("Holon", text.clone())
                };
                vec![cell]
            }

            PresentationItem::ResumeNotice { reason, details } => {
                let mut body = format!("\u{21bb} {}", reason);
                if level >= 5 {
                    if let Some(details) = details {
                        if !details.trim().is_empty() && details != reason {
                            body.push_str(&format!("\n\u{2502} {}", details));
                        }
                    }
                }
                vec![RenderedCell::new("Holon", body)]
            }

            PresentationItem::ActionGroup {
                heading,
                items,
                state,
            } => {
                let mut cells = Vec::new();
                let header = if *state == ItemState::Live {
                    RenderedCell::live("Holon", heading.clone())
                } else {
                    RenderedCell::new("Holon", heading.clone())
                };
                cells.push(header);
                for item in items {
                    for cell in item.render(level) {
                        cells.push(cell.indented(1));
                    }
                }
                cells
            }

            PresentationItem::CommandExecuted {
                status,
                cmd_preview,
                task_id,
                duration_ms,
                exit_code,
                stdout_summary: _,
                full_stdout,
                full_stderr,
                error_message,
            } => {
                let symbol = match status {
                    CommandStatus::Started => "\u{2026}",
                    CommandStatus::PromotedToTask | CommandStatus::AlreadyRunning => "\u{21bb}",
                    CommandStatus::Failed => Outcome::Failure.symbol(),
                    CommandStatus::Completed => match exit_code {
                        Some(0) => Outcome::Success.symbol(),
                        Some(_) => Outcome::Failure.symbol(),
                        None => Outcome::Unknown.symbol(),
                    },
                };
                let mut body = format!("{} {}", symbol, cmd_preview);
                if let Some(duration_ms) = duration_ms {
                    let duration_s = *duration_ms as f64 / 1000.0;
                    body.push_str(&format!(" ({:.1}s)", duration_s));
                }
                match status {
                    CommandStatus::Started => body.push_str(" — started"),
                    CommandStatus::PromotedToTask => {
                        body.push_str(" — running in background");
                        if let Some(task_id) = task_id {
                            body.push_str(&format!(" ({task_id})"));
                        }
                    }
                    CommandStatus::AlreadyRunning => {
                        body.push_str(" — already running");
                        if let Some(task_id) = task_id {
                            body.push_str(&format!(" ({task_id})"));
                        }
                    }
                    CommandStatus::Failed if exit_code.is_none() => body.push_str(" — failed"),
                    CommandStatus::Completed | CommandStatus::Failed => {}
                }

                if level >= 5 {
                    if let Some(error) = error_message {
                        if !error.trim().is_empty() {
                            body.push_str("\n\u{2502} error:\n");
                            for line in error.lines() {
                                body.push_str(&format!("\u{2502} {}\n", line));
                            }
                        }
                    }
                    if let Some(stdout) = full_stdout {
                        if !stdout.trim().is_empty() {
                            body.push_str("\n\u{2502} stdout:\n");
                            for line in stdout.lines() {
                                body.push_str(&format!("\u{2502} {}\n", line));
                            }
                        }
                    }
                    if let Some(stderr) = full_stderr {
                        if !stderr.trim().is_empty() {
                            body.push_str("\n\u{2502} stderr:\n");
                            for line in stderr.lines() {
                                body.push_str(&format!("\u{2502} {}\n", line));
                            }
                        }
                    }
                }

                vec![RenderedCell::new("Holon", body)]
            }

            PresentationItem::FileRead { path, summary } => {
                let body = if summary.is_empty() {
                    format!("Read {}", path)
                } else {
                    format!("Read {} \u{2014} {}", path, summary)
                };
                vec![RenderedCell::new("Holon", body).indented(1)]
            }

            PresentationItem::FileChange {
                path,
                action,
                hunks,
                full_diff,
            } => {
                let added: u32 = hunks.iter().map(|h| h.added_count).sum();
                let removed: u32 = hunks.iter().map(|h| h.removed_count).sum();
                let hunk_info = if hunks.is_empty() {
                    String::new()
                } else {
                    format!(" (+{}, -{})", added, removed)
                };
                let mut body = format!("{} {}{}", action.symbol(), path, hunk_info);

                if level >= 5 {
                    if let Some(diff) = full_diff {
                        body.push('\n');
                        for line in diff.lines() {
                            body.push_str(&format!("\u{2502} {}\n", line));
                        }
                    }
                }
                vec![RenderedCell::new("Holon", body).indented(1)]
            }

            PresentationItem::PatchFailure {
                summary,
                error_message,
            } => {
                let mut body = format!("{} {}", Outcome::Failure.symbol(), summary);
                if level >= 5 {
                    if let Some(error) = error_message {
                        if !error.trim().is_empty() {
                            body.push_str("\n\u{2502} error:\n");
                            for line in error.lines() {
                                body.push_str(&format!("\u{2502} {}\n", line));
                            }
                        }
                    }
                }
                vec![RenderedCell::new("Holon", body)]
            }

            PresentationItem::ToolAction { summary } => {
                vec![RenderedCell::new("Holon", summary.clone()).indented(1)]
            }

            PresentationItem::PlanShown { plan_text } => {
                let body = if level >= 5 {
                    format!("Plan:\n{}", plan_text)
                } else {
                    format!("Plan: {}", truncate_text(plan_text, 200))
                };
                vec![RenderedCell::new("Holon", body)]
            }

            // ── Level 5+ ────────────────────────────────────────────────
            PresentationItem::ProviderRound {
                model,
                stop_reason,
                tokens,
            } => {
                vec![RenderedCell::new(
                    "Provider",
                    format!(
                        "{} \u{2014} {} stop \u{2014} {}\u{2193} {}\u{2191} tokens",
                        model, stop_reason, tokens.input, tokens.output
                    ),
                )]
            }

            PresentationItem::InternalTransition { what, from, to } => {
                vec![RenderedCell::new(
                    "Runtime",
                    format!("{}: {} \u{2192} {}", what, from, to),
                )]
            }

            PresentationItem::TaskLifecycle {
                task_id,
                transition,
            } => {
                vec![RenderedCell::new(
                    "System",
                    format!("task {} {}", task_id, transition.label()),
                )]
            }

            PresentationItem::WorkItemBookkeeping {
                item_id,
                transition,
                summary,
            } => {
                let detail = if summary.trim().is_empty() {
                    item_id.clone()
                } else {
                    summary.clone()
                };
                vec![RenderedCell::new(
                    "System",
                    format!("work item {}: {}", transition.label(), detail),
                )]
            }

            PresentationItem::WorkspaceChange { path, action } => {
                vec![RenderedCell::new(
                    "System",
                    format!("workspace {} {}", path, action),
                )]
            }

            PresentationItem::ContinuationDetail { trigger, outcome } => {
                vec![RenderedCell::new(
                    "Runtime",
                    format!("continuation: {} \u{2014} {}", trigger, outcome),
                )]
            }

            PresentationItem::GenericEvent { kind, summary } => {
                vec![RenderedCell::new(
                    "System",
                    format!("[{}] {}", kind, summary),
                )]
            }
        }
    }
}

// ── Reducer ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub(crate) struct PresentationReducer {
    live_group: Option<LiveGroup>,
    last_ts: Option<DateTime<Utc>>,
    open_command_keys: HashSet<String>,
    observed_assistant_text_keys: HashSet<String>,
    observed_assistant_texts: HashSet<String>,
    observed_user_message_keys: HashSet<String>,
    observed_brief_keys: HashSet<String>,
}

#[derive(Debug, Clone)]
struct LiveGroup {
    heading: String,
    items: Vec<PresentationItem>,
}

impl PresentationReducer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn reduce(
        &mut self,
        events: &[ProjectionEventRecord],
        brief_texts: &BriefTextLookup,
    ) -> Vec<TimedItem> {
        let mut items: Vec<TimedItem> = Vec::new();
        let final_brief_texts = final_brief_texts(events, brief_texts);
        let mut local_open_command_items: HashMap<String, usize> = HashMap::new();

        let mut i = 0;
        while i < events.len() {
            let event = &events[i];

            match event.kind.as_str() {
                "process_execution_requested" => {}

                "message_enqueued" => {
                    if let Some((key, text)) = operator_message_item(event) {
                        if self.observed_user_message_keys.insert(key) {
                            items.push(TimedItem::from_event(
                                PresentationItem::UserMessage { text },
                                event,
                            ));
                        }
                    }
                }

                "brief_created" => {
                    if let Some((key, item)) = brief_result_item(event, brief_texts) {
                        let duplicates_observed_preview = match &item {
                            PresentationItem::AssistantResult { body, .. } => self
                                .observed_assistant_texts
                                .contains(&strip_preview_ellipsis(normalized_text(body).as_str())),
                            _ => false,
                        };
                        if !duplicates_observed_preview && self.observed_brief_keys.insert(key) {
                            items.push(TimedItem::from_event(item, event));
                        }
                    }
                }

                "tool_executed" | "tool_execution_failed" => {
                    if is_sleep_tool_event(event) {
                        items.push(TimedItem::from_event(
                            PresentationItem::InternalTransition {
                                what: "Sleep".into(),
                                from: "tool".into(),
                                to: event.summary.clone(),
                            },
                            event,
                        ));
                        i += 1;
                        continue;
                    }
                    if is_successful_work_item_tool_event(event) {
                        i += 1;
                        continue;
                    }
                    if let Some(item) = list_work_items_snapshot_item(event) {
                        items.push(TimedItem::from_event(item, event));
                        i += 1;
                        continue;
                    }
                    if is_apply_patch_tool_event(event) {
                        for item in apply_patch_items(event) {
                            items.push(TimedItem::from_event(item, event));
                        }
                        i += 1;
                        continue;
                    }
                    if !is_exec_command_tool_event(event) {
                        items.push(TimedItem::from_event(
                            self.event_to_presentation(event),
                            event,
                        ));
                        i += 1;
                        continue;
                    }
                    let cmd_preview =
                        exec_command_preview(event).unwrap_or_else(|| event.summary.clone());
                    let exit_code = tool_exit_code(event).or_else(|| match event.kind.as_str() {
                        "tool_execution_failed" => None,
                        _ => Some(0),
                    });
                    let stdout_summary = tool_output_summary(event);
                    let full_stdout = tool_full_output(event);
                    let full_stderr = tool_full_stderr(event);

                    let item = PresentationItem::CommandExecuted {
                        status: command_status_from_tool_event(event),
                        cmd_preview: cmd_preview.clone(),
                        task_id: command_task_id(event),
                        duration_ms: tool_duration_ms(event),
                        exit_code,
                        stdout_summary,
                        full_stdout,
                        full_stderr,
                        error_message: tool_error_message(event),
                    };
                    if let Some(key) = command_presentation_key(event, &cmd_preview) {
                        if let Some(index) = local_open_command_items.remove(&key) {
                            items[index] = TimedItem::with_event_key(item, event, key);
                            self.open_command_keys.remove(&items[index].dedupe_key);
                        } else if self.open_command_keys.remove(&key) {
                            items.push(TimedItem::with_event_key(item, event, key));
                        } else {
                            items.push(TimedItem::with_event_key(item, event, key));
                        }
                    } else {
                        items.push(TimedItem::from_event(item, event));
                    }
                }

                "assistant_round_recorded" | "text_only_round_observed" => {
                    let runtime_checkpoint =
                        event.payload.get("round_purpose").and_then(Value::as_str)
                            == Some("runtime_checkpoint");
                    if !runtime_checkpoint {
                        if let Some(text) = round_text_preview(event) {
                            let text_key = normalized_event_text_key(event, &text);
                            if !text.trim().is_empty()
                                && !matches_final_brief_text(event, &text, &final_brief_texts)
                                && self.observed_assistant_text_keys.insert(text_key)
                            {
                                self.observed_assistant_texts.insert(strip_preview_ellipsis(
                                    normalized_text(&text).as_str(),
                                ));
                                items.push(TimedItem::from_event(
                                    PresentationItem::AssistantProgress {
                                        text,
                                        state: ItemState::Stable,
                                    },
                                    event,
                                ));
                            }
                        }
                    }
                }

                "callback_delivered" | "timer_fired" | "continuation_trigger_received" => {
                    if let Some(item) = resume_notice_item(event) {
                        items.push(TimedItem::from_event(item, event));
                    } else {
                        items.push(TimedItem::from_event(
                            self.event_to_presentation(event),
                            event,
                        ));
                    }
                }

                "task_result_received"
                | "task_child_spawned"
                | "supervised_child_task_recovery_failed"
                | "command_task_runner_failed"
                | "command_task_result_enqueue_failed" => {
                    let task_id = event
                        .payload
                        .get("task_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let transition = match event.kind.as_str() {
                        "task_result_received" => TaskTransition::ResultReceived,
                        "task_child_spawned" => TaskTransition::ChildSpawned,
                        "supervised_child_task_recovery_failed" => TaskTransition::RecoveryFailed,
                        "command_task_runner_failed" => TaskTransition::RunnerFailed,
                        _ => TaskTransition::StatusUpdated,
                    };
                    items.push(TimedItem::from_event(
                        PresentationItem::TaskLifecycle {
                            task_id: task_id.to_string(),
                            transition,
                        },
                        event,
                    ));
                }

                "task_status_updated" => {
                    let task_id = event
                        .payload
                        .get("task_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    items.push(TimedItem::from_event(
                        PresentationItem::TaskLifecycle {
                            task_id: task_id.to_string(),
                            transition: TaskTransition::StatusUpdated,
                        },
                        event,
                    ));
                }

                "provider_round_completed" => {
                    if let Some(item) = provider_round_item(event) {
                        items.push(TimedItem::from_event(item, event));
                    }
                }

                "continuation_resolved" | "closure_decided" => {
                    let what = match event.kind.as_str() {
                        "continuation_resolved" => "continuation",
                        _ => "closure",
                    };
                    items.push(TimedItem::from_event(
                        PresentationItem::InternalTransition {
                            what: what.to_string(),
                            from: "?".to_string(),
                            to: event.summary.clone(),
                        },
                        event,
                    ));
                }

                "work_item_written" => {
                    let action = event
                        .payload
                        .get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("updated");
                    if action == "created" || action == "completed" {
                        items.push(TimedItem::from_event(
                            PresentationItem::WorkItemCard {
                                action: work_item_card_action(action).to_string(),
                                summary: work_item_lifecycle_summary(event),
                            },
                            event,
                        ));
                    } else {
                        let (item_id, summary) = work_item_bookkeeping_parts(event, events);
                        items.push(TimedItem::from_event(
                            PresentationItem::WorkItemBookkeeping {
                                item_id,
                                transition: work_item_written_transition(action),
                                summary,
                            },
                            event,
                        ));
                    }
                }

                "work_item_picked"
                | "work_item_focus_released"
                | "work_item_enqueue_requested"
                | "work_item_turn_end_committed"
                | "work_item_turn_end_commit_skipped"
                | "work_item_stale_reminder_injected"
                | "work_item_stale_reminder_skipped"
                | "work_item_delegation_created"
                | "work_item_delegation_completed"
                | "missing_current_work_item_before_wait" => {
                    let (item_id, summary) = work_item_bookkeeping_parts(event, events);
                    items.push(TimedItem::from_event(
                        PresentationItem::WorkItemBookkeeping {
                            item_id,
                            transition: work_item_event_transition(&event.kind),
                            summary,
                        },
                        event,
                    ));
                }

                "workspace_attached" | "workspace_entered" | "workspace_exited"
                | "workspace_detached" | "worktree_entered" | "worktree_exited" => {
                    let path = event
                        .payload
                        .get("path")
                        .and_then(|v| v.as_str())
                        .or_else(|| event.payload.get("workspace_path").and_then(|v| v.as_str()))
                        .unwrap_or("?");
                    let action = match event.kind.as_str() {
                        "workspace_attached" => "attached",
                        "workspace_entered" => "entered",
                        "workspace_exited" => "exited",
                        "workspace_detached" => "detached",
                        "worktree_entered" => "entered worktree",
                        "worktree_exited" => "exited worktree",
                        _ => "changed",
                    };
                    items.push(TimedItem::from_event(
                        PresentationItem::WorkspaceChange {
                            path: path.to_string(),
                            action: action.to_string(),
                        },
                        event,
                    ));
                }

                kind if is_suppressed_known_runtime_event(kind) => {}

                _ => {
                    items.push(TimedItem::from_event(
                        self.event_to_presentation(event),
                        event,
                    ));
                }
            }

            i += 1;
        }

        if let Some(last) = events.last() {
            self.last_ts = Some(last.ts);
        }

        items
    }

    /// Return the current live group as a `TimedItem`, if one is accumulating.
    pub(crate) fn current_live_item(&self) -> Option<TimedItem> {
        self.live_group.as_ref().map(|group| {
            TimedItem::with_key(
                PresentationItem::ActionGroup {
                    heading: group.heading.clone(),
                    items: group.items.clone(),
                    state: ItemState::Live,
                },
                self.last_ts.unwrap_or_else(Utc::now),
                "live-group",
            )
        })
    }

    pub(crate) fn flush(&mut self) -> Vec<TimedItem> {
        let mut items: Vec<TimedItem> = Vec::new();
        if let Some(group) = self.live_group.take() {
            items.push(TimedItem::with_key(
                PresentationItem::ActionGroup {
                    heading: group.heading,
                    items: group.items,
                    state: ItemState::Stable,
                },
                self.last_ts.unwrap_or_else(Utc::now),
                "flushed-live-group",
            ));
        }
        items
    }

    fn event_to_presentation(&self, event: &ProjectionEventRecord) -> PresentationItem {
        match event.presentation.category {
            OperatorEventCategory::OperatorNotification => PresentationItem::SystemAlert {
                title: event.presentation.title.clone(),
                body: event.summary.clone(),
            },
            OperatorEventCategory::Brief => PresentationItem::AssistantResult {
                brief_id: None,
                body: event.summary.clone(),
                outcome: Outcome::Neutral,
            },
            OperatorEventCategory::Message => PresentationItem::InternalTransition {
                what: event.kind.clone(),
                from: "message".to_string(),
                to: event.summary.clone(),
            },
            OperatorEventCategory::Waiting => {
                if matches!(
                    event.kind.as_str(),
                    "callback_delivered" | "timer_create_requested" | "timer_created"
                ) {
                    PresentationItem::GenericEvent {
                        kind: event.kind.clone(),
                        summary: event.summary.clone(),
                    }
                } else {
                    PresentationItem::WaitingNotice {
                        reason: event.summary.clone(),
                    }
                }
            }
            OperatorEventCategory::Runtime => PresentationItem::InternalTransition {
                what: event.kind.clone(),
                from: "".to_string(),
                to: event.summary.clone(),
            },
            OperatorEventCategory::Tool => PresentationItem::ToolAction {
                summary: event.summary.clone(),
            },
            _ => PresentationItem::GenericEvent {
                kind: event.kind.clone(),
                summary: event.summary.clone(),
            },
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Brief text resolved from transcript hydration, keyed by transcript entry_id.
pub(crate) struct BriefTextLookup<'a>(pub(crate) &'a std::collections::BTreeMap<String, String>);

impl<'a> BriefTextLookup<'a> {
    pub(crate) fn resolve_for_record(&self, brief: &BriefRecord) -> Option<&str> {
        match brief.content_source {
            crate::types::BriefContentSource::TranscriptEntry { ref entry_id, .. } => {
                self.0.get(entry_id).map(String::as_str)
            }
            crate::types::BriefContentSource::Inline => self.0.get(&brief.id).map(String::as_str),
        }
    }

    pub(crate) fn resolve_for_event(&self, event: &ProjectionEventRecord) -> Option<&str> {
        let brief = serde_json::from_value::<BriefCreatedAuditEvent>(event.payload.clone()).ok()?;
        match brief.content_source {
            crate::types::BriefContentSource::TranscriptEntry { ref entry_id, .. } => {
                self.0.get(entry_id).map(String::as_str)
            }
            crate::types::BriefContentSource::Inline => {
                self.0.get(&brief.brief_id).map(String::as_str)
            }
        }
    }
}

fn brief_result_item(
    event: &ProjectionEventRecord,
    brief_texts: &BriefTextLookup,
) -> Option<(String, PresentationItem)> {
    match serde_json::from_value::<BriefRecord>(event.payload.clone()) {
        Ok(brief) => {
            if brief.kind == BriefKind::Ack {
                return None;
            }
            let key = format!("id:{}", brief.id);
            Some((
                key,
                PresentationItem::AssistantResult {
                    brief_id: Some(brief.id.clone()),
                    body: brief_texts
                        .resolve_for_record(&brief)
                        .map(str::to_string)
                        .unwrap_or_else(|| brief.text.clone()),
                    outcome: match brief.kind {
                        BriefKind::Failure => Outcome::Failure,
                        BriefKind::Result => Outcome::Success,
                        BriefKind::Ack => Outcome::Neutral,
                    },
                },
            ))
        }
        Err(_) => {
            if let Ok(brief) =
                serde_json::from_value::<BriefCreatedAuditEvent>(event.payload.clone())
            {
                if brief.kind == BriefKind::Ack {
                    return None;
                }
                return Some((
                    format!("id:{}", brief.brief_id),
                    PresentationItem::AssistantResult {
                        brief_id: Some(brief.brief_id),
                        body: brief_texts
                            .resolve_for_event(event)
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                format!(
                                    "{:?} brief ({} chars)",
                                    brief.kind, brief.content_char_count
                                )
                            }),
                        outcome: match brief.kind {
                            BriefKind::Failure => Outcome::Failure,
                            BriefKind::Result => Outcome::Success,
                            BriefKind::Ack => Outcome::Neutral,
                        },
                    },
                ));
            }
            Some((
                format!("summary:{}", normalize_text_key(&event.summary)),
                PresentationItem::AssistantResult {
                    brief_id: None,
                    body: event.summary.clone(),
                    outcome: Outcome::Neutral,
                },
            ))
        }
    }
}

fn event_dedupe_key(event: &ProjectionEventRecord) -> String {
    if !event.id.is_empty() {
        return format!("event-id:{}", event.id);
    }
    format!("event-seq:{}:{}", event.event_seq, event.kind)
}

fn work_item_record(event: &ProjectionEventRecord) -> Option<WorkItemRecord> {
    event
        .payload
        .get("record")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn work_item_lifecycle_event(event: &ProjectionEventRecord) -> Option<WorkItemLifecycleAuditEvent> {
    serde_json::from_value(event.payload.clone()).ok()
}

fn work_item_id_from_event(event: &ProjectionEventRecord) -> Option<String> {
    work_item_record(event)
        .map(|record| record.id)
        .or_else(|| work_item_lifecycle_event(event).map(|record| record.work_item_id))
        .or_else(|| {
            event
                .payload
                .get("work_item_id")
                .or_else(|| event.payload.get("current_work_item_id"))
                .or_else(|| event.payload.get("id"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn work_item_objective_from_events(
    work_item_id: &str,
    events: &[ProjectionEventRecord],
) -> Option<String> {
    events
        .iter()
        .rev()
        .filter(|event| event.kind == "work_item_written")
        .find_map(|event| {
            if let Some(record) = work_item_record(event) {
                return (record.id == work_item_id).then_some(record.objective);
            }
            let record = work_item_lifecycle_event(event)?;
            (record.work_item_id == work_item_id).then_some(record.objective_preview)
        })
}

fn work_item_lifecycle_summary(event: &ProjectionEventRecord) -> String {
    if let Some(record) = work_item_record(event) {
        if event.payload.get("action").and_then(Value::as_str) == Some("completed") {
            return record
                .result_summary
                .filter(|summary| !summary.trim().is_empty())
                .unwrap_or(record.objective);
        }
        return record.objective;
    }
    if let Some(record) = work_item_lifecycle_event(event) {
        if event.payload.get("action").and_then(Value::as_str) == Some("completed") {
            return record
                .result_summary_preview
                .filter(|summary| !summary.trim().is_empty())
                .unwrap_or(record.objective_preview);
        }
        return record.objective_preview;
    }
    event.summary.clone()
}

fn work_item_card_action(action: &str) -> &'static str {
    match action {
        "created" => "tracking started",
        "completed" => "completed",
        _ => "updated",
    }
}

fn work_item_bookkeeping_parts(
    event: &ProjectionEventRecord,
    events: &[ProjectionEventRecord],
) -> (String, String) {
    let item_id = work_item_id_from_event(event).unwrap_or_else(|| "?".into());
    let summary = if let Some(record) = work_item_record(event) {
        record.objective
    } else if let Some(record) = work_item_lifecycle_event(event) {
        record.objective_preview
    } else if item_id != "?" {
        work_item_objective_from_events(&item_id, events).unwrap_or_else(|| event.summary.clone())
    } else {
        event.summary.clone()
    };
    (item_id, summary)
}

fn work_item_written_transition(action: &str) -> WorkItemTransition {
    match action {
        "wait_for_blocked" => WorkItemTransition::Waiting,
        "wait_for_task_resolved" => WorkItemTransition::WaitResolved,
        "completed" => WorkItemTransition::Completed,
        "created" => WorkItemTransition::Created,
        _ => WorkItemTransition::Updated,
    }
}

fn work_item_event_transition(kind: &str) -> WorkItemTransition {
    match kind {
        "work_item_picked" => WorkItemTransition::Picked,
        "work_item_focus_released" => WorkItemTransition::FocusReleased,
        "work_item_enqueue_requested" => WorkItemTransition::EnqueueRequested,
        "work_item_turn_end_committed" => WorkItemTransition::TurnEndCommitted,
        "work_item_turn_end_commit_skipped" => WorkItemTransition::TurnEndSkipped,
        "work_item_stale_reminder_injected" => WorkItemTransition::ReminderInjected,
        "work_item_stale_reminder_skipped" => WorkItemTransition::ReminderSkipped,
        "work_item_delegation_created" => WorkItemTransition::DelegationCreated,
        "work_item_delegation_completed" => WorkItemTransition::DelegationCompleted,
        "missing_current_work_item_before_wait" => WorkItemTransition::Waiting,
        _ => WorkItemTransition::Updated,
    }
}

fn command_presentation_key(event: &ProjectionEventRecord, cmd_preview: &str) -> Option<String> {
    if cmd_preview.trim().is_empty() {
        return None;
    }
    let workdir = event
        .payload
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(format!(
        "command:{}:{}",
        workdir,
        normalize_text_key(cmd_preview)
    ))
}

fn normalize_text_key(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn operator_message_item(event: &ProjectionEventRecord) -> Option<(String, String)> {
    let message = serde_json::from_value::<MessageEnvelope>(event.payload.clone()).ok()?;
    if !matches!(message.origin, MessageOrigin::Operator { .. }) {
        return None;
    }
    let text = match message.body {
        MessageBody::Text { text } | MessageBody::Brief { text, .. } => Some(text),
        MessageBody::Json { value } => serde_json::to_string(&value).ok(),
    }?;
    let key = if message.id.is_empty() {
        format!("text:{}", normalize_text_key(&text))
    } else {
        format!("message:{}", message.id)
    };
    Some((key, text))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalBriefText {
    agent_id: String,
    text: String,
}

fn final_brief_texts(
    events: &[ProjectionEventRecord],
    brief_texts: &BriefTextLookup,
) -> Vec<FinalBriefText> {
    events
        .iter()
        .filter(|event| event.kind == "brief_created")
        .filter_map(|event| {
            if let Ok(brief) = serde_json::from_value::<BriefRecord>(event.payload.clone()) {
                let text = brief_texts
                    .resolve_for_record(&brief)
                    .unwrap_or(brief.text.as_str());
                return (!text.trim().is_empty()).then_some(FinalBriefText {
                    agent_id: brief.agent_id,
                    text: normalized_text(text),
                });
            }
            let brief =
                serde_json::from_value::<BriefCreatedAuditEvent>(event.payload.clone()).ok()?;
            // Resolve text from the hydration cache for TranscriptEntry briefs;
            // Inline briefs have no text in slimmed audit events.
            brief_texts.resolve_for_event(event).and_then(|text| {
                (!text.trim().is_empty()).then_some(FinalBriefText {
                    agent_id: brief.agent_id,
                    text: normalized_text(text),
                })
            })
        })
        .collect()
}

fn matches_final_brief_text(
    event: &ProjectionEventRecord,
    text: &str,
    final_brief_texts: &[FinalBriefText],
) -> bool {
    let Some(agent_id) = event.payload.get("agent_id").and_then(Value::as_str) else {
        return false;
    };
    let observed = strip_preview_ellipsis(normalized_text(text).as_str());
    if observed.is_empty() {
        return false;
    }
    final_brief_texts
        .iter()
        .filter(|brief| brief.agent_id == agent_id)
        .any(|brief| brief.text == observed || brief.text.starts_with(&observed))
}

fn normalized_event_text_key(event: &ProjectionEventRecord, text: &str) -> String {
    let normalized = normalized_text(text);
    match (
        event.payload.get("turn_index").and_then(Value::as_u64),
        event.payload.get("round").and_then(Value::as_u64),
    ) {
        (Some(turn_index), Some(round)) => format!("turn:{turn_index}:round:{round}::{normalized}"),
        _ => normalized,
    }
}

fn normalized_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_preview_ellipsis(text: &str) -> String {
    let mut observed = text.trim().to_string();
    loop {
        let trimmed = observed.trim_end();
        if let Some(stripped) = trimmed.strip_suffix("...") {
            observed = stripped.trim_end().to_string();
            continue;
        }
        if let Some(stripped) = trimmed.strip_suffix('\u{2026}') {
            observed = stripped.trim_end().to_string();
            continue;
        }
        return trimmed.to_string();
    }
}

fn is_sleep_tool_event(event: &ProjectionEventRecord) -> bool {
    event.payload.get("tool_name").and_then(Value::as_str) == Some(holon_domain::tool_names::SLEEP)
}

fn is_exec_command_tool_event(event: &ProjectionEventRecord) -> bool {
    matches!(
        event.payload.get("tool_name").and_then(Value::as_str),
        Some(name) if name == holon_domain::tool_names::EXEC_COMMAND || name == holon_domain::tool_names::EXEC_COMMAND_BATCH
    )
}

fn is_apply_patch_tool_event(event: &ProjectionEventRecord) -> bool {
    event.payload.get("tool_name").and_then(Value::as_str)
        == Some(holon_domain::tool_names::APPLY_PATCH)
}

fn list_work_items_snapshot_item(event: &ProjectionEventRecord) -> Option<PresentationItem> {
    if event.kind != "tool_executed" {
        return None;
    }
    if event.payload.get("tool_name").and_then(Value::as_str)
        != Some(holon_domain::tool_names::LIST_WORK_ITEMS)
    {
        return None;
    }
    if event.payload.get("status").and_then(Value::as_str) != Some("success") {
        return None;
    }
    let result = event
        .payload
        .get("tool_result")
        .or_else(|| event.payload.get("result"))?;
    Some(PresentationItem::ToolAction {
        summary: list_work_items_snapshot_summary(result),
    })
}

fn list_work_items_snapshot_summary(result: &Value) -> String {
    let filter = result
        .get("filter")
        .and_then(Value::as_str)
        .unwrap_or("all");
    let returned = result.get("returned").and_then(Value::as_u64).unwrap_or(0);
    let total = result
        .get("total_matching")
        .and_then(Value::as_u64)
        .unwrap_or(returned);
    let limit = result.get("limit").and_then(Value::as_u64);
    let mut lines = vec![match limit {
        Some(limit) => {
            format!("Work items: filter={filter} returned={returned} total={total} limit={limit}")
        }
        None => format!("Work items: filter={filter} returned={returned} total={total}"),
    }];

    let Some(items) = result.get("work_items").and_then(Value::as_array) else {
        return lines.join("\n");
    };
    if items.is_empty() {
        lines.push(format!("No work items matched filter={filter}."));
        if let Some(current_id) = result
            .get("context")
            .and_then(|context| context.get("current_work_item_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            lines.push(format!("current: {}", short_work_item_id(current_id)));
        }
        return lines.join("\n");
    }

    for item in items.iter().take(5) {
        if let Some(line) = list_work_item_line(item) {
            lines.push(line);
        }
    }
    let hidden = returned.saturating_sub(items.len().min(5) as u64);
    if hidden > 0 {
        lines.push(format!("… {hidden} more not shown"));
    }
    lines.join("\n")
}

fn list_work_item_line(item: &Value) -> Option<String> {
    let id = item.get("id").and_then(Value::as_str)?;
    let objective = item
        .get("objective")
        .and_then(Value::as_str)
        .map(|value| truncate_text(value, 96))
        .unwrap_or_else(|| "(no objective)".into());
    let focus = item.get("focus").and_then(Value::as_str).unwrap_or("open");
    let readiness = item
        .get("readiness")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut line = format!(
        "- [{}][{}] {} :: {}",
        focus,
        readiness,
        short_work_item_id(id),
        objective
    );
    if let Some(detail) = list_work_item_detail(item) {
        line.push_str(&format!("\n  {detail}"));
    }
    Some(line)
}

fn list_work_item_detail(item: &Value) -> Option<String> {
    if let Some(blocked_by) = item
        .get("blocked_by")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(format!("blocked_by: {}", truncate_text(blocked_by, 140)));
    }
    if let Some(waiting_for) = item
        .get("active_wait_conditions")
        .and_then(Value::as_array)
        .and_then(|waits| waits.first())
        .and_then(|wait| wait.get("waiting_for"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(format!("waiting_for: {}", truncate_text(waiting_for, 140)));
    }
    if let Some(todo) = item
        .get("todo_list")
        .and_then(Value::as_array)
        .and_then(|todos| {
            todos
                .iter()
                .find(|todo| todo.get("state").and_then(Value::as_str) == Some("in_progress"))
                .or_else(|| todos.first())
        })
        .and_then(|todo| todo.get("text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(format!("todo: {}", truncate_text(todo, 140)));
    }
    item.get("completion_report")
        .and_then(|report| report.get("text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|report| format!("completed: {}", truncate_text(report, 140)))
}

fn short_work_item_id(id: &str) -> String {
    const PREFIX_LEN: usize = 17;
    if id.len() <= PREFIX_LEN {
        id.to_string()
    } else {
        id.chars().take(PREFIX_LEN).collect()
    }
}

fn is_successful_work_item_tool_event(event: &ProjectionEventRecord) -> bool {
    if event.kind != "tool_executed" {
        return false;
    }
    if event.payload.get("status").and_then(Value::as_str) != Some("success") {
        return false;
    }
    matches!(
        event.payload.get("tool_name").and_then(Value::as_str),
        Some(name)
            if name == holon_domain::tool_names::CREATE_WORK_ITEM
            || name == holon_domain::tool_names::UPDATE_WORK_ITEM
            || name == holon_domain::tool_names::PICK_WORK_ITEM
            || name == holon_domain::tool_names::COMPLETE_WORK_ITEM
    )
}

fn apply_patch_items(event: &ProjectionEventRecord) -> Vec<PresentationItem> {
    if event.kind == "tool_execution_failed" {
        return vec![PresentationItem::PatchFailure {
            summary: patch_failure_summary(event),
            error_message: tool_raw_error_message(event),
        }];
    }

    let mut changes = apply_patch_result(event)
        .map(apply_patch_file_changes_from_result)
        .unwrap_or_default();
    if changes.is_empty() {
        changes = apply_patch_file_changes_from_summary(
            event
                .payload
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or(&event.summary),
        );
    }

    if changes.is_empty() {
        vec![PresentationItem::GenericEvent {
            kind: event.kind.clone(),
            summary: event.summary.clone(),
        }]
    } else {
        changes
    }
}

fn patch_failure_summary(event: &ProjectionEventRecord) -> String {
    let summary = tool_error_message(event)
        .and_then(|error| {
            error
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| format!("Patch failed: {line}"))
        })
        .unwrap_or_else(|| {
            event
                .summary
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        });
    if summary.is_empty() {
        "Patch failed".to_string()
    } else if summary.starts_with("Patch failed") {
        summary
    } else {
        format!("Patch failed: {summary}")
    }
}

fn apply_patch_result(event: &ProjectionEventRecord) -> Option<&Value> {
    event
        .payload
        .get("apply_patch_result")
        .or_else(|| event.payload.get("result"))
}

fn apply_patch_file_changes_from_result(result: &Value) -> Vec<PresentationItem> {
    let changes = result
        .get("changed_files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(apply_patch_file_change_from_value)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !changes.is_empty() {
        return changes;
    }

    result
        .get("changed_paths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .map(|path| PresentationItem::FileChange {
                    path: path.to_string(),
                    action: FileAction::Modified,
                    hunks: Vec::new(),
                    full_diff: None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn apply_patch_file_change_from_value(value: &Value) -> Option<PresentationItem> {
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .and_then(file_action_from_apply_patch_action)
        .unwrap_or(FileAction::Modified);
    let path = value.get("path").and_then(Value::as_str)?.trim();
    if path.is_empty() {
        return None;
    }
    let path = if action == FileAction::Renamed {
        match value.get("from_path").and_then(Value::as_str) {
            Some(from_path) if !from_path.trim().is_empty() => {
                format!("{} -> {}", from_path.trim(), path)
            }
            _ => path.to_string(),
        }
    } else {
        path.to_string()
    };
    Some(PresentationItem::FileChange {
        path,
        action,
        hunks: apply_patch_hunks_from_value(value),
        full_diff: value
            .get("diff_preview")
            .and_then(Value::as_str)
            .filter(|diff| !diff.trim().is_empty())
            .map(ToString::to_string),
    })
}

fn apply_patch_hunks_from_value(value: &Value) -> Vec<HunkSummary> {
    value
        .get("hunks")
        .and_then(Value::as_array)
        .map(|hunks| {
            hunks
                .iter()
                .filter_map(|hunk| {
                    Some(HunkSummary {
                        old_start: value_u32(hunk.get("old_start"))?,
                        old_count: value_u32(hunk.get("old_count"))?,
                        new_start: value_u32(hunk.get("new_start"))?,
                        new_count: value_u32(hunk.get("new_count"))?,
                        added_count: value_u32(hunk.get("added_count")).unwrap_or(0),
                        removed_count: value_u32(hunk.get("removed_count")).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn value_u32(value: Option<&Value>) -> Option<u32> {
    value?.as_u64()?.try_into().ok()
}

fn file_action_from_apply_patch_action(action: &str) -> Option<FileAction> {
    match action {
        "add" => Some(FileAction::Added),
        "modify" => Some(FileAction::Modified),
        "delete" => Some(FileAction::Deleted),
        "move" => Some(FileAction::Renamed),
        _ => None,
    }
}

fn apply_patch_file_changes_from_summary(summary: &str) -> Vec<PresentationItem> {
    let Some((_, changed)) = summary.split_once("patched ") else {
        return Vec::new();
    };
    changed
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let (marker, path) = entry.split_once(':')?;
            let action = file_action_from_apply_patch_marker(marker.trim())?;
            let path = path.trim();
            if path.is_empty() {
                return None;
            }
            let path = if action == FileAction::Renamed {
                path.split_once("->")
                    .map(|(from, to)| format!("{} -> {}", from.trim(), to.trim()))
                    .unwrap_or_else(|| path.to_string())
            } else {
                path.to_string()
            };
            Some(PresentationItem::FileChange {
                path,
                action,
                hunks: Vec::new(),
                full_diff: None,
            })
        })
        .collect()
}

fn file_action_from_apply_patch_marker(marker: &str) -> Option<FileAction> {
    match marker {
        "A" => Some(FileAction::Added),
        "M" => Some(FileAction::Modified),
        "D" => Some(FileAction::Deleted),
        "R" => Some(FileAction::Renamed),
        _ => None,
    }
}

fn resume_notice_item(event: &ProjectionEventRecord) -> Option<PresentationItem> {
    let reason = match event.kind.as_str() {
        "callback_delivered" => callback_resume_reason(event),
        "timer_fired" => timer_resume_reason(event),
        "continuation_trigger_received" => continuation_resume_reason(event),
        _ => None,
    }?;
    Some(PresentationItem::ResumeNotice {
        reason,
        details: Some(event.summary.clone()),
    })
}

fn callback_resume_reason(event: &ProjectionEventRecord) -> Option<String> {
    if !callback_disposition_is_triggered(&event.payload) {
        return None;
    }
    let source = event.payload.get("source").and_then(Value::as_str);
    let resource = event.payload.get("resource").and_then(Value::as_str);
    let reason = match (source, resource) {
        (Some(source), Some(resource)) => {
            format!("External event from {source} for {resource}; resuming agent")
        }
        (Some(source), None) => format!("External event from {source}; resuming agent"),
        (None, Some(resource)) => format!("External event for {resource}; resuming agent"),
        (None, None) => "External event received; resuming agent".to_string(),
    };
    Some(reason)
}

fn timer_resume_reason(event: &ProjectionEventRecord) -> Option<String> {
    let reason = event
        .payload
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .map(|summary| format!("Timer fired: {}; resuming agent", summary.trim()))
        .unwrap_or_else(|| "Timer fired; resuming agent".to_string());
    Some(reason)
}

fn continuation_resume_reason(event: &ProjectionEventRecord) -> Option<String> {
    let trigger = event
        .payload
        .get("trigger_kind")
        .and_then(Value::as_str)
        .filter(|trigger| !trigger.trim().is_empty())?;
    Some(format!(
        "Continuation triggered by {trigger}; resuming agent"
    ))
}

fn callback_disposition_is_triggered(payload: &Value) -> bool {
    payload
        .get("disposition")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("triggered"))
}

fn is_suppressed_known_runtime_event(kind: &str) -> bool {
    matches!(
        kind,
        "scheduler_decision" | "message_admitted" | "message_processing_started" | "turn_started"
    )
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars - 1).collect();
    format!("{}\u{2026}", truncated)
}

fn exec_command_preview(event: &ProjectionEventRecord) -> Option<String> {
    if let Some(cmd) = event
        .payload
        .get("exec_command_display")
        .and_then(Value::as_str)
        .or_else(|| event.payload.get("cmd_display").and_then(Value::as_str))
    {
        return Some(cmd.to_string());
    }
    if let Some(items) = event
        .payload
        .get("exec_command_batch_items")
        .and_then(Value::as_array)
    {
        let cmds = items
            .iter()
            .filter_map(|item| {
                item.get("cmd_display")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("cmd").and_then(Value::as_str))
            })
            .collect::<Vec<_>>();
        if !cmds.is_empty() {
            return Some(cmds.join("\n"));
        }
    }
    exec_command_result(event)
        .and_then(|result| result.get("cmd_display"))
        .and_then(Value::as_str)
        .or_else(|| {
            exec_command_result(event)
                .and_then(|result| result.get("cmd"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .payload
                .get("exec_command_cmd")
                .and_then(Value::as_str)
        })
        .or_else(|| event.payload.get("cmd").and_then(Value::as_str))
        .or_else(|| event.payload.get("cmd_preview").and_then(Value::as_str))
        .or_else(|| {
            event
                .payload
                .get("command_cost")
                .and_then(|v| v.get("cmd_preview"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .payload
                .get("exec_command_cost")
                .and_then(|v| v.get("cmd_preview"))
                .and_then(Value::as_str)
        })
        .or_else(|| command_preview_from_summary(&event.summary))
        .map(ToString::to_string)
}

fn command_preview_from_summary(summary: &str) -> Option<&str> {
    [
        "Command started:",
        "Command finished:",
        "Command failed:",
        "Command batch started:",
        "Command batch finished:",
        "Command batch failed:",
    ]
    .iter()
    .find_map(|prefix| summary.strip_prefix(prefix))
    .map(str::trim)
    .filter(|cmd| !cmd.is_empty())
}

fn tool_exit_code(event: &ProjectionEventRecord) -> Option<i32> {
    event
        .payload
        .get("exit_status")
        .or_else(|| exec_command_result(event).and_then(|result| result.get("exit_status")))
        .and_then(|v: &Value| v.as_i64())
        .map(|c| c as i32)
}

fn tool_duration_ms(event: &ProjectionEventRecord) -> Option<u64> {
    event
        .payload
        .get("duration_ms")
        .or_else(|| exec_command_result(event).and_then(|result| result.get("duration_ms")))
        .and_then(Value::as_u64)
}

fn tool_output_summary(event: &ProjectionEventRecord) -> String {
    event
        .payload
        .get("stdout_preview")
        .or_else(|| event.payload.get("output_preview"))
        .or_else(|| exec_command_result(event).and_then(|result| result.get("stdout_preview")))
        .or_else(|| {
            exec_command_result(event).and_then(|result| result.get("initial_output_preview"))
        })
        .and_then(|v: &Value| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn tool_full_output(event: &ProjectionEventRecord) -> Option<String> {
    event
        .payload
        .get("stdout")
        .or_else(|| exec_command_result(event).and_then(|result| result.get("stdout_preview")))
        .and_then(|v: &Value| v.as_str())
        .map(|s| s.to_string())
}

fn tool_full_stderr(event: &ProjectionEventRecord) -> Option<String> {
    event
        .payload
        .get("stderr")
        .or_else(|| exec_command_result(event).and_then(|result| result.get("stderr_preview")))
        .and_then(|v: &Value| v.as_str())
        .map(|s| s.to_string())
}

fn tool_error_message(event: &ProjectionEventRecord) -> Option<String> {
    if let Some(message) = event
        .payload
        .get("tool_error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        return Some(message);
    }

    let raw_error = event.payload.get("error").and_then(Value::as_str)?;
    error_json_message(raw_error).or_else(|| Some(raw_error.to_string()))
}

fn error_json_message(error: &str) -> Option<String> {
    serde_json::from_str::<Value>(error)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| value.get("kind").and_then(Value::as_str))
                .map(str::to_string)
        })
        .filter(|message| !message.trim().is_empty())
}

fn tool_raw_error_message(event: &ProjectionEventRecord) -> Option<String> {
    event
        .payload
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .payload
                .get("tool_error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .map(|s| s.to_string())
}

fn command_status_from_tool_event(event: &ProjectionEventRecord) -> CommandStatus {
    if event.kind == "tool_execution_failed" {
        return CommandStatus::Failed;
    }
    if let Some(disposition) = event
        .payload
        .get("exec_command_disposition")
        .and_then(Value::as_str)
        .or_else(|| {
            exec_command_result(event)
                .and_then(|result| result.get("disposition"))
                .and_then(Value::as_str)
        })
    {
        return match disposition {
            "promoted_to_task" => CommandStatus::PromotedToTask,
            "already_running" => CommandStatus::AlreadyRunning,
            "completed" => match tool_exit_code(event) {
                Some(0) | None => CommandStatus::Completed,
                Some(_) => CommandStatus::Failed,
            },
            _ => CommandStatus::Completed,
        };
    }
    match tool_exit_code(event) {
        Some(0) | None => CommandStatus::Completed,
        Some(_) => CommandStatus::Failed,
    }
}

fn command_task_id(event: &ProjectionEventRecord) -> Option<String> {
    event
        .payload
        .get("task_handle")
        .or_else(|| exec_command_result(event).and_then(|result| result.get("task_handle")))
        .and_then(|handle| handle.get("task_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn exec_command_result(event: &ProjectionEventRecord) -> Option<&Value> {
    event.payload.get("exec_command_result")
}

fn round_text_preview(event: &ProjectionEventRecord) -> Option<String> {
    event
        .payload
        .get("text_preview")
        .and_then(|v: &Value| v.as_str())
        .map(|s| s.to_string())
}

fn provider_round_item(event: &ProjectionEventRecord) -> Option<PresentationItem> {
    let model = event
        .payload
        .get("active_model")
        .and_then(|v: &Value| v.as_str())?;
    let stop_reason = event
        .payload
        .get("stop_reason")
        .and_then(|v: &Value| v.as_str())
        .unwrap_or("?");
    let input = event
        .payload
        .get("input_tokens")
        .and_then(|v: &Value| v.as_u64())
        .unwrap_or(0);
    let output = event
        .payload
        .get("output_tokens")
        .and_then(|v: &Value| v.as_u64())
        .unwrap_or(0);

    Some(PresentationItem::ProviderRound {
        model: model.to_string(),
        stop_reason: stop_reason.to_string(),
        tokens: TokenCount { input, output },
    })
}

#[cfg(test)]
mod tests;
