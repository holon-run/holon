//! Presentation item model and reducer.
//!
//! Converts raw `ProjectionEventRecord` events into typed `PresentationItem`
//! values that render differently at each display level (info=3, verbose=4, debug=5).
//!
//! See `docs/rfcs/presentation-item-model-and-renderer.md`.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::operator_event::OperatorEventCategory;
use crate::tui::projection::ProjectionEventRecord;
use crate::types::{BriefKind, BriefRecord, MessageBody, MessageEnvelope, MessageOrigin};

// ── Supporting types ───────────────────────────────────────────────────────

/// Summary of a diff hunk: start line and line count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkSummary {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
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
    Completed,
    Picked,
    EnqueueRequested,
    DelegationCreated,
    DelegationCompleted,
    TurnEndCommitted,
}

impl WorkItemTransition {
    pub fn label(&self) -> &'static str {
        match self {
            WorkItemTransition::Created => "created",
            WorkItemTransition::Completed => "completed",
            WorkItemTransition::Picked => "picked",
            WorkItemTransition::EnqueueRequested => "enqueued",
            WorkItemTransition::DelegationCreated => "delegated",
            WorkItemTransition::DelegationCompleted => "delegation completed",
            WorkItemTransition::TurnEndCommitted => "turn-end committed",
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

    // ── Level 4+ (Verbose): Codex-like activity ───────────────────────────
    AssistantProgress {
        text: String,
        state: ItemState,
    },
    ActionGroup {
        heading: String,
        items: Vec<PresentationItem>,
        state: ItemState,
    },
    CommandExecuted {
        cmd_preview: String,
        duration_ms: Option<u64>,
        exit_code: Option<i32>,
        stdout_summary: String,
        full_stdout: Option<String>,
        full_stderr: Option<String>,
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
            PresentationItem::ActionGroup { .. } => 4,
            PresentationItem::CommandExecuted { .. } => 4,
            PresentationItem::FileRead { .. } => 4,
            PresentationItem::FileChange { .. } => 4,
            PresentationItem::PlanShown { .. } => 4,

            PresentationItem::ProviderRound { .. } => 5,
            PresentationItem::InternalTransition { .. } => 5,
            PresentationItem::TaskLifecycle { .. } => 5,
            PresentationItem::WorkItemBookkeeping { .. } => 5,
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
                cmd_preview,
                duration_ms,
                exit_code,
                stdout_summary,
                full_stdout,
                full_stderr,
            } => {
                let outcome = match exit_code {
                    Some(0) => Outcome::Success,
                    Some(_) => Outcome::Failure,
                    None => Outcome::Unknown,
                };
                let mut body = format!("{} {}", outcome.symbol(), cmd_preview);
                if let Some(duration_ms) = duration_ms {
                    let duration_s = *duration_ms as f64 / 1000.0;
                    body.push_str(&format!(" ({:.1}s)", duration_s));
                }

                if level >= 5 {
                    if let Some(stdout) = full_stdout {
                        if !stdout.trim().is_empty() {
                            body.push_str("\n\u{2502} stdout:\n");
                            for line in stdout.lines().take(20) {
                                body.push_str(&format!("\u{2502} {}\n", line));
                            }
                        }
                    }
                    if let Some(stderr) = full_stderr {
                        if !stderr.trim().is_empty() {
                            body.push_str("\n\u{2502} stderr:\n");
                            for line in stderr.lines().take(10) {
                                body.push_str(&format!("\u{2502} {}\n", line));
                            }
                        }
                    }
                } else if !stdout_summary.is_empty() {
                    body.push_str(&format!("\n\u{2502} {}", stdout_summary));
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
                let added: u32 = hunks.iter().map(|h| h.new_count).sum();
                let removed: u32 = hunks.iter().map(|h| h.old_count).sum();
                let hunk_info = if hunks.is_empty() {
                    String::new()
                } else {
                    format!(" (+{}, -{})", added, removed)
                };
                let mut body = format!("{} {}{}", action.symbol(), path, hunk_info);

                if level >= 5 {
                    if let Some(diff) = full_diff {
                        body.push('\n');
                        for line in diff.lines().take(40) {
                            body.push_str(&format!("\u{2502} {}\n", line));
                        }
                    }
                }
                vec![RenderedCell::new("Holon", body).indented(1)]
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
            } => {
                vec![RenderedCell::new(
                    "System",
                    format!("work-item {} {}", item_id, transition.label()),
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
    observed_assistant_text_keys: HashSet<String>,
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

    pub(crate) fn reduce(&mut self, events: &[ProjectionEventRecord]) -> Vec<TimedItem> {
        let mut items: Vec<TimedItem> = Vec::new();
        let final_brief_texts = final_brief_texts(events);

        let mut i = 0;
        while i < events.len() {
            let event = &events[i];

            match event.kind.as_str() {
                "process_execution_requested" => {
                    let exec_preview =
                        exec_command_preview(event).unwrap_or_else(|| event.summary.clone());

                    if let Some(next) = events.get(i + 1) {
                        if matches!(
                            next.kind.as_str(),
                            "tool_executed" | "tool_execution_failed"
                        ) && next.summary.contains(&exec_preview)
                        {
                            let duration_ms = tool_duration_ms(next);
                            let exit_code =
                                tool_exit_code(next).or_else(|| match next.kind.as_str() {
                                    "tool_execution_failed" => None,
                                    _ => Some(0),
                                });
                            let stdout_summary = tool_output_summary(next);
                            let full_stdout = tool_full_output(next);
                            let full_stderr = tool_full_stderr(next);

                            items.push(TimedItem {
                                item: PresentationItem::CommandExecuted {
                                    cmd_preview: exec_preview,
                                    duration_ms,
                                    exit_code,
                                    stdout_summary,
                                    full_stdout,
                                    full_stderr,
                                },
                                ts: next.ts,
                            });
                            i += 2;
                            continue;
                        }
                    }
                }

                "message_enqueued" => {
                    if let Some((key, text)) = operator_message_item(event) {
                        if self.observed_user_message_keys.insert(key) {
                            items.push(TimedItem {
                                item: PresentationItem::UserMessage { text },
                                ts: event.ts,
                            });
                        }
                    }
                }

                "brief_created" => {
                    if let Some((key, item)) = brief_result_item(event) {
                        if self.observed_brief_keys.insert(key) {
                            items.push(TimedItem { item, ts: event.ts });
                        }
                    }
                }

                "tool_executed" | "tool_execution_failed" => {
                    if is_sleep_tool_event(event) {
                        items.push(TimedItem {
                            item: PresentationItem::InternalTransition {
                                what: "Sleep".into(),
                                from: "tool".into(),
                                to: event.summary.clone(),
                            },
                            ts: event.ts,
                        });
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

                    items.push(TimedItem {
                        item: PresentationItem::CommandExecuted {
                            cmd_preview,
                            duration_ms: tool_duration_ms(event),
                            exit_code,
                            stdout_summary,
                            full_stdout,
                            full_stderr,
                        },
                        ts: event.ts,
                    });
                }

                "assistant_round_recorded" | "text_only_round_observed" => {
                    if let Some(text) = round_text_preview(event) {
                        let text_key = normalized_event_text_key(event, &text);
                        if !text.trim().is_empty()
                            && !matches_final_brief_text(event, &text, &final_brief_texts)
                            && self.observed_assistant_text_keys.insert(text_key)
                        {
                            items.push(TimedItem {
                                item: PresentationItem::AssistantProgress {
                                    text,
                                    state: ItemState::Stable,
                                },
                                ts: event.ts,
                            });
                        }
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
                    items.push(TimedItem {
                        item: PresentationItem::TaskLifecycle {
                            task_id: task_id.to_string(),
                            transition,
                        },
                        ts: event.ts,
                    });
                }

                "task_status_updated" => {
                    let task_id = event
                        .payload
                        .get("task_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    items.push(TimedItem {
                        item: PresentationItem::TaskLifecycle {
                            task_id: task_id.to_string(),
                            transition: TaskTransition::StatusUpdated,
                        },
                        ts: event.ts,
                    });
                }

                "provider_round_completed" => {
                    if let Some(item) = provider_round_item(event) {
                        items.push(TimedItem { item, ts: event.ts });
                    }
                }

                "continuation_resolved" | "closure_decided" => {
                    let what = match event.kind.as_str() {
                        "continuation_resolved" => "continuation",
                        _ => "closure",
                    };
                    items.push(TimedItem {
                        item: PresentationItem::InternalTransition {
                            what: what.to_string(),
                            from: "?".to_string(),
                            to: event.summary.clone(),
                        },
                        ts: event.ts,
                    });
                }

                "work_item_written" => {
                    let item_id = event
                        .payload
                        .get("work_item_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let action = event
                        .payload
                        .get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("updated");
                    let summary = event.summary.clone();
                    if action == "created" || action == "completed" {
                        items.push(TimedItem {
                            item: PresentationItem::WorkItemCard {
                                action: action.to_string(),
                                summary,
                            },
                            ts: event.ts,
                        });
                    } else {
                        items.push(TimedItem {
                            item: PresentationItem::WorkItemBookkeeping {
                                item_id: item_id.to_string(),
                                transition: WorkItemTransition::Created,
                            },
                            ts: event.ts,
                        });
                    }
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
                    items.push(TimedItem {
                        item: PresentationItem::WorkspaceChange {
                            path: path.to_string(),
                            action: action.to_string(),
                        },
                        ts: event.ts,
                    });
                }

                kind if is_suppressed_known_runtime_event(kind) => {}

                _ => {
                    items.push(TimedItem {
                        item: self.event_to_presentation(event),
                        ts: event.ts,
                    });
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
        self.live_group.as_ref().map(|group| TimedItem {
            item: PresentationItem::ActionGroup {
                heading: group.heading.clone(),
                items: group.items.clone(),
                state: ItemState::Live,
            },
            ts: self.last_ts.unwrap_or_else(Utc::now),
        })
    }

    pub(crate) fn flush(&mut self) -> Vec<TimedItem> {
        let mut items: Vec<TimedItem> = Vec::new();
        if let Some(group) = self.live_group.take() {
            items.push(TimedItem {
                item: PresentationItem::ActionGroup {
                    heading: group.heading,
                    items: group.items,
                    state: ItemState::Stable,
                },
                ts: self.last_ts.unwrap_or_else(Utc::now),
            });
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
            OperatorEventCategory::Waiting => PresentationItem::WaitingNotice {
                reason: event.summary.clone(),
            },
            OperatorEventCategory::Runtime => PresentationItem::InternalTransition {
                what: event.kind.clone(),
                from: "".to_string(),
                to: event.summary.clone(),
            },
            _ => PresentationItem::GenericEvent {
                kind: event.kind.clone(),
                summary: event.summary.clone(),
            },
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn brief_result_item(event: &ProjectionEventRecord) -> Option<(String, PresentationItem)> {
    match serde_json::from_value::<BriefRecord>(event.payload.clone()) {
        Ok(brief) if is_operator_queue_ack(&brief) => None,
        Ok(brief) => {
            let key = format!("id:{}", brief.id);
            Some((
                key,
                PresentationItem::AssistantResult {
                    brief_id: Some(brief.id),
                    body: brief.text,
                    outcome: match brief.kind {
                        BriefKind::Failure => Outcome::Failure,
                        BriefKind::Result => Outcome::Success,
                        BriefKind::Ack => Outcome::Neutral,
                    },
                },
            ))
        }
        Err(_) => Some((
            format!("summary:{}", normalize_text_key(&event.summary)),
            PresentationItem::AssistantResult {
                brief_id: None,
                body: event.summary.clone(),
                outcome: Outcome::Neutral,
            },
        )),
    }
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

fn is_operator_queue_ack(brief: &BriefRecord) -> bool {
    // This matches the canonical operator-input acknowledgement from
    // `brief::make_ack`; arbitrary Ack briefs should still render normally.
    brief.kind == BriefKind::Ack
        && brief.related_message_id.is_some()
        && brief
            .text
            .trim_start()
            .starts_with(crate::brief::QUEUED_WORK_ACK_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalBriefText {
    agent_id: String,
    text: String,
}

fn final_brief_texts(events: &[ProjectionEventRecord]) -> Vec<FinalBriefText> {
    events
        .iter()
        .filter(|event| event.kind == "brief_created")
        .filter_map(|event| serde_json::from_value::<BriefRecord>(event.payload.clone()).ok())
        .filter(|brief| !is_operator_queue_ack(brief))
        .filter(|brief| !brief.text.trim().is_empty())
        .map(|brief| FinalBriefText {
            agent_id: brief.agent_id,
            text: normalized_text(brief.text.as_str()),
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
    event.payload.get("tool_name").and_then(Value::as_str) == Some("Sleep")
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
    event
        .payload
        .get("exec_command_cmd")
        .and_then(|v| v.as_str())
        .or_else(|| event.payload.get("cmd").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn tool_exit_code(event: &ProjectionEventRecord) -> Option<i32> {
    event
        .payload
        .get("exit_status")
        .and_then(|v: &Value| v.as_i64())
        .map(|c| c as i32)
}

fn tool_duration_ms(event: &ProjectionEventRecord) -> Option<u64> {
    event.payload.get("duration_ms").and_then(Value::as_u64)
}

fn tool_output_summary(event: &ProjectionEventRecord) -> String {
    event
        .payload
        .get("stdout_preview")
        .or_else(|| event.payload.get("output_preview"))
        .and_then(|v: &Value| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn tool_full_output(event: &ProjectionEventRecord) -> Option<String> {
    event
        .payload
        .get("stdout")
        .and_then(|v: &Value| v.as_str())
        .map(|s| s.to_string())
}

fn tool_full_stderr(event: &ProjectionEventRecord) -> Option<String> {
    event
        .payload
        .get("stderr")
        .and_then(|v: &Value| v.as_str())
        .map(|s| s.to_string())
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
mod tests {
    use super::*;

    #[test]
    fn min_display_levels() {
        assert_eq!(
            PresentationItem::UserMessage { text: "hi".into() }.min_display_level(),
            3
        );
        assert_eq!(
            PresentationItem::AssistantProgress {
                text: "...".into(),
                state: ItemState::Stable
            }
            .min_display_level(),
            4
        );
        assert_eq!(
            PresentationItem::ProviderRound {
                model: "gpt-4".into(),
                stop_reason: "end_turn".into(),
                tokens: TokenCount::default()
            }
            .min_display_level(),
            5
        );
    }

    #[test]
    fn is_visible_at() {
        let item = PresentationItem::CommandExecuted {
            cmd_preview: "cargo test".into(),
            duration_ms: Some(1000),
            exit_code: Some(0),
            stdout_summary: "".into(),
            full_stdout: None,
            full_stderr: None,
        };
        assert!(!item.is_visible_at(3));
        assert!(item.is_visible_at(4));
        assert!(item.is_visible_at(5));
    }

    #[test]
    fn command_render_level_4() {
        let item = PresentationItem::CommandExecuted {
            cmd_preview: "cargo test --lib".into(),
            duration_ms: Some(2300),
            exit_code: Some(0),
            stdout_summary: "5 passed".into(),
            full_stdout: Some("running 5 tests\ntest result: ok".into()),
            full_stderr: None,
        };
        let cells = item.render(4);
        assert_eq!(cells.len(), 1);
        assert!(cells[0].body.contains("cargo test --lib"));
        assert!(cells[0].body.contains("2.3s"));
        assert!(cells[0].body.contains("5 passed"));
        assert!(!cells[0].body.contains("running 5 tests"));
    }

    #[test]
    fn command_render_level_5() {
        let item = PresentationItem::CommandExecuted {
            cmd_preview: "cargo test --lib".into(),
            duration_ms: Some(2300),
            exit_code: Some(0),
            stdout_summary: "5 passed".into(),
            full_stdout: Some("running 5 tests\ntest result: ok".into()),
            full_stderr: None,
        };
        let cells = item.render(5);
        assert_eq!(cells.len(), 1);
        assert!(cells[0].body.contains("running 5 tests"));
        assert!(cells[0].body.contains("test result: ok"));
    }

    #[test]
    fn file_change_render_level_4() {
        let item = PresentationItem::FileChange {
            path: "src/foo.rs".into(),
            action: FileAction::Modified,
            hunks: vec![HunkSummary {
                old_start: 12,
                old_count: 5,
                new_start: 12,
                new_count: 7,
            }],
            full_diff: Some("@@ -12,5 +12,7 @@\n-old\n+new".into()),
        };
        let cells = item.render(4);
        assert_eq!(cells.len(), 1);
        assert!(cells[0].body.contains("M src/foo.rs"));
        assert!(cells[0].body.contains("(+7, -5)"));
        assert!(!cells[0].body.contains("@@"));
    }

    #[test]
    fn file_change_render_level_5() {
        let item = PresentationItem::FileChange {
            path: "src/foo.rs".into(),
            action: FileAction::Modified,
            hunks: vec![HunkSummary {
                old_start: 12,
                old_count: 5,
                new_start: 12,
                new_count: 7,
            }],
            full_diff: Some("@@ -12,5 +12,7 @@\n-old\n+new".into()),
        };
        let cells = item.render(5);
        assert_eq!(cells.len(), 1);
        assert!(cells[0].body.contains("M src/foo.rs"));
        assert!(cells[0].body.contains("@@"));
        assert!(cells[0].body.contains("-old"));
        assert!(cells[0].body.contains("+new"));
    }

    #[test]
    fn render_suppresses_below_min_level() {
        let item = PresentationItem::ProviderRound {
            model: "gpt-4".into(),
            stop_reason: "end_turn".into(),
            tokens: TokenCount::default(),
        };
        assert!(item.render(4).is_empty());
    }

    // ── Reducer snapshot tests ──────────────────────────────────────────

    use crate::operator_event::{present_operator_event, OperatorPresentationContext};
    use crate::tui::projection::ProjectionEventRecord;
    use chrono::Utc;
    use serde_json::json;

    fn make_event(kind: &str, summary: &str, payload: serde_json::Value) -> ProjectionEventRecord {
        let presentation = present_operator_event(
            kind,
            &payload,
            summary,
            &OperatorPresentationContext::default(),
        );
        ProjectionEventRecord {
            id: "evt-1".into(),
            event_seq: 1,
            ts: Utc::now(),
            lane: crate::tui::projection::ProjectionEventLane::Debug,
            kind: kind.into(),
            summary: presentation.summary.clone(),
            presentation,
            payload,
        }
    }

    #[test]
    fn reducer_merges_command_start_and_finish() {
        let start = make_event(
            "process_execution_requested",
            "command started: cargo test",
            json!({"exec_command_cmd": "cargo test --lib", "cmd": "cargo test --lib"}),
        );
        let finish = make_event(
            "tool_executed",
            "tool executed: cargo test --lib",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "cargo test --lib",
                "exit_status": 0,
                "stdout_preview": "5 passed"
            }),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[start.clone(), finish.clone()]);

        // Should produce exactly 1 CommandExecuted (merged)
        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::CommandExecuted {
                cmd_preview,
                exit_code,
                stdout_summary,
                ..
            } => {
                assert!(cmd_preview.contains("cargo test"));
                assert_eq!(*exit_code, Some(0));
                assert_eq!(stdout_summary, "5 passed");
            }
            other => panic!("expected CommandExecuted, got {:?}", other),
        }
    }

    #[test]
    fn reducer_standalone_tool_executed_becomes_command() {
        let event = make_event(
            "tool_executed",
            "tool executed: rg pattern",
            json!({
                "tool_name": "ExecCommand",
                "exec_command_cmd": "rg pattern src/",
                "exit_status": 0
            }),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[event]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::CommandExecuted { cmd_preview, .. } => {
                assert!(cmd_preview.contains("rg pattern"));
            }
            other => panic!("expected CommandExecuted, got {:?}", other),
        }
    }

    #[test]
    fn reducer_produces_assistant_progress() {
        let event = make_event(
            "assistant_round_recorded",
            "assistant round",
            json!({"text_preview": "Let me analyze the code..."}),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[event]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::AssistantProgress { text, .. } => {
                assert_eq!(text, "Let me analyze the code...");
            }
            other => panic!("expected AssistantProgress, got {:?}", other),
        }
    }

    #[test]
    fn reducer_provider_round_at_level_5() {
        let event = make_event(
            "provider_round_completed",
            "provider round",
            json!({
                "active_model": "deepseek-v4",
                "stop_reason": "end_turn",
                "input_tokens": 5000,
                "output_tokens": 200
            }),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[event]);

        assert_eq!(items.len(), 1);
        let item = &items[0].item;
        assert_eq!(item.min_display_level(), 5);
        match item {
            PresentationItem::ProviderRound { model, tokens, .. } => {
                assert_eq!(model, "deepseek-v4");
                assert_eq!(tokens.input, 5000);
                assert_eq!(tokens.output, 200);
            }
            other => panic!("expected ProviderRound, got {:?}", other),
        }
    }

    #[test]
    fn reducer_brief_created_uses_brief_text_without_event_label() {
        let brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "completed the task",
            None,
            None,
        );
        let event = make_event("brief_created", "Brief: completed the task", json!(brief));

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[event]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::AssistantResult { body, outcome, .. } => {
                assert_eq!(body, "completed the task");
                assert_eq!(*outcome, Outcome::Success);
                assert!(!body.contains("Brief:"));
            }
            other => panic!("expected AssistantResult, got {:?}", other),
        }
    }

    #[test]
    fn reducer_deduplicates_repeated_brief_created_by_brief_id() {
        let brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "completed the task",
            None,
            None,
        );
        let first = make_event("brief_created", "Brief: completed the task", json!(brief));
        let mut second = first.clone();
        second.id = "evt-2".into();
        second.event_seq = 2;

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[first, second]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::AssistantResult { brief_id, body, .. } => {
                assert!(brief_id.is_some());
                assert_eq!(body, "completed the task");
            }
            other => panic!("expected AssistantResult, got {:?}", other),
        }
    }

    #[test]
    fn reducer_deduplicates_repeated_operator_message_by_message_id() {
        let mut message = MessageEnvelope::new(
            "default",
            crate::types::MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            crate::types::TrustLevel::TrustedOperator,
            crate::types::Priority::Normal,
            MessageBody::Text {
                text: "same operator input".into(),
            },
        );
        message.id = "message-1".into();
        let first = make_event("message_enqueued", "message enqueued", json!(message));
        let mut second = first.clone();
        second.id = "evt-2".into();
        second.event_seq = 2;

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[first, second]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::UserMessage { text } => {
                assert_eq!(text, "same operator input");
            }
            other => panic!("expected UserMessage, got {:?}", other),
        }
    }

    #[test]
    fn reducer_filters_canonical_operator_queue_ack_briefs() {
        let message = crate::types::MessageEnvelope::new(
            "default",
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator { actor_id: None },
            crate::types::TrustLevel::TrustedOperator,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "duplicate operator input".into(),
            },
        );
        let brief = crate::brief::make_ack("default", &message);
        assert!(brief.text.starts_with(crate::brief::QUEUED_WORK_ACK_PREFIX));
        let event = make_event("brief_created", "Queued work: duplicate", json!(brief));

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[event]);

        assert!(items.is_empty());
    }

    #[test]
    fn reducer_deduplicates_assistant_observations_against_final_brief() {
        let assistant = make_event(
            "assistant_round_recorded",
            "assistant round",
            json!({
                "agent_id": "default",
                "text_preview": "Issue recorded: #1128..."
            }),
        );
        let text_only = make_event(
            "text_only_round_observed",
            "text only round",
            json!({
                "agent_id": "default",
                "text_preview": "Issue recorded: #1128..."
            }),
        );
        let brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Issue recorded: #1128 with complete details",
            None,
            None,
        );
        let brief_event = make_event(
            "brief_created",
            "Issue recorded: #1128 with complete details",
            json!(brief),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[assistant, text_only, brief_event]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::AssistantResult { body, .. } => {
                assert_eq!(body, "Issue recorded: #1128 with complete details");
            }
            other => panic!("expected AssistantResult, got {:?}", other),
        }
    }

    #[test]
    fn reducer_deduplicates_repeated_assistant_text_observations() {
        let assistant = make_event(
            "assistant_round_recorded",
            "assistant round",
            json!({
                "agent_id": "default",
                "text_preview": "Analyzing the issue"
            }),
        );
        let text_only = make_event(
            "text_only_round_observed",
            "text only round",
            json!({
                "agent_id": "default",
                "text_preview": "Analyzing   the issue"
            }),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[assistant, text_only]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::AssistantProgress { text, .. } => {
                assert_eq!(text, "Analyzing the issue");
            }
            other => panic!("expected AssistantProgress, got {:?}", other),
        }
    }

    #[test]
    fn reducer_deduplicates_same_round_text_despite_agent_id_mismatch() {
        let assistant = make_event(
            "assistant_round_recorded",
            "assistant round",
            json!({
                "agent_id": "default",
                "turn_index": 12,
                "round": 3,
                "text_preview": "Rendering once"
            }),
        );
        let text_only = make_event(
            "text_only_round_observed",
            "text only round",
            json!({
                "turn_index": 12,
                "round": 3,
                "text_preview": "Rendering once"
            }),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[assistant, text_only]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::AssistantProgress { text, .. } => {
                assert_eq!(text, "Rendering once");
            }
            other => panic!("expected AssistantProgress, got {:?}", other),
        }
    }

    #[test]
    fn reducer_deduplicates_same_round_text_across_incremental_reductions() {
        let assistant = make_event(
            "assistant_round_recorded",
            "assistant round",
            json!({
                "agent_id": "default",
                "turn_index": 12,
                "round": 3,
                "text_preview": "Rendering once"
            }),
        );
        let text_only = make_event(
            "text_only_round_observed",
            "text only round",
            json!({
                "agent_id": "default",
                "turn_index": 12,
                "round": 3,
                "text_preview": "Rendering once"
            }),
        );

        let mut reducer = PresentationReducer::new();
        let first = reducer.reduce(&[assistant]);
        let second = reducer.reduce(&[text_only]);

        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }

    #[test]
    fn reducer_downgrades_sleep_tool_to_debug_internal_transition() {
        let event = make_event(
            "tool_executed",
            "Slept: sleep requested",
            json!({
                "tool_name": "Sleep"
            }),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[event]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::InternalTransition { what, .. } => {
                assert_eq!(what, "Sleep");
                assert_eq!(items[0].item.min_display_level(), 5);
                assert!(items[0].item.render(4).is_empty());
                assert!(!items[0].item.render(5).is_empty());
            }
            other => panic!("expected InternalTransition, got {:?}", other),
        }
    }

    #[test]
    fn reducer_failure_brief_preserves_failure_outcome() {
        let brief = BriefRecord::new(
            "default",
            BriefKind::Failure,
            "provider transport failed",
            None,
            None,
        );
        let event = make_event(
            "brief_created",
            "Brief: provider transport failed",
            json!(brief),
        );

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[event]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::AssistantResult { body, outcome, .. } => {
                assert_eq!(body, "provider transport failed");
                assert_eq!(*outcome, Outcome::Failure);
            }
            other => panic!("expected AssistantResult, got {:?}", other),
        }
    }

    #[test]
    fn reducer_suppresses_known_runtime_noise_but_keeps_unknown_debug_fallback() {
        let scheduler = make_event(
            "scheduler_decision",
            "Scheduler decision: sleep",
            json!({"decision": "sleep"}),
        );
        let unknown = make_event("unknown_runtime_event", "unknown runtime detail", json!({}));

        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[scheduler, unknown]);

        assert_eq!(items.len(), 1);
        match &items[0].item {
            PresentationItem::GenericEvent { kind, summary } => {
                assert_eq!(kind, "unknown_runtime_event");
                assert_eq!(summary, "unknown runtime detail");
                assert_eq!(items[0].item.min_display_level(), 5);
            }
            other => panic!("expected GenericEvent fallback, got {:?}", other),
        }
    }

    #[test]
    fn reducer_empty_events_returns_empty() {
        let mut reducer = PresentationReducer::new();
        let items = reducer.reduce(&[]);
        assert!(items.is_empty());

        let flushed = reducer.flush();
        assert!(flushed.is_empty());
    }
}
