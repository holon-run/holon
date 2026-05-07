use std::collections::BTreeSet;

use serde_json::Value;

use crate::types::{
    BriefRecord, TaskRecord, TaskStatus, TimerRecord, WaitingIntentRecord, WorkItemRecord,
    WorkItemState, WorktreeSession,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperatorVisibility {
    /// Operator attention is required before the agent can continue.
    ActionRequired = 1,
    /// A work item reached a durable completion point.
    WorkDone = 2,
    /// Default operator-facing turn result or durable conversation event.
    TurnResult = 3,
    /// In-turn assistant progress that is useful while the agent is active.
    Progress = 4,
    /// Tool and internal trace events for detailed inspection.
    Trace = 5,
}

impl OperatorVisibility {
    pub fn display_level(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperatorDisplayMode {
    /// Result-oriented operator view.
    Info = 3,
    /// Codex-like activity view.
    Verbose = 4,
    /// Detailed but still curated operator view.
    Debug = 5,
}

impl OperatorDisplayMode {
    pub const DEFAULT: Self = Self::Info;

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "3" | "info" => Some(Self::Info),
            "4" | "verbose" => Some(Self::Verbose),
            "5" | "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    pub fn display_level(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Verbose => "verbose",
            Self::Debug => "debug",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorEventCategory {
    OperatorNotification,
    Brief,
    Message,
    WorkItem,
    Task,
    Waiting,
    Workspace,
    Runtime,
    AssistantProgress,
    Tool,
    StateSync,
    Trace,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OperatorEventPresentation {
    pub visibility: OperatorVisibility,
    pub category: OperatorEventCategory,
    pub title: String,
    pub body: Option<String>,
    pub summary: String,
    pub source_event_kind: String,
}

#[derive(Debug, Default)]
pub struct OperatorPresentationContext {
    pub awaiting_operator_input: bool,
    pub completed_work_item_ids: BTreeSet<String>,
}

impl OperatorEventPresentation {
    pub fn is_conversation_candidate(&self) -> bool {
        !matches!(
            self.category,
            OperatorEventCategory::AssistantProgress
                | OperatorEventCategory::Tool
                | OperatorEventCategory::StateSync
                | OperatorEventCategory::Trace
        )
    }

    pub fn is_current_activity_candidate(&self) -> bool {
        matches!(
            self.category,
            OperatorEventCategory::AssistantProgress
                | OperatorEventCategory::Tool
                | OperatorEventCategory::Trace
                | OperatorEventCategory::Task
                | OperatorEventCategory::WorkItem
                | OperatorEventCategory::Waiting
                | OperatorEventCategory::Workspace
                | OperatorEventCategory::Runtime
                | OperatorEventCategory::OperatorNotification
                | OperatorEventCategory::Brief
                | OperatorEventCategory::Message
        )
    }

    pub fn is_loggable(&self) -> bool {
        !matches!(self.category, OperatorEventCategory::StateSync)
    }

    pub fn is_error_loggable(&self) -> bool {
        matches!(
            self.source_event_kind.as_str(),
            "runtime_error" | "turn_terminal"
        )
    }
}

pub fn present_operator_event(
    kind: &str,
    payload: &Value,
    fallback_summary: &str,
    context: &OperatorPresentationContext,
) -> OperatorEventPresentation {
    let category = event_category(kind);
    let visibility = event_visibility(kind, payload, category, context);
    let (title, body, summary) = event_text(kind, payload, fallback_summary, category);

    OperatorEventPresentation {
        visibility,
        category,
        title,
        body,
        summary,
        source_event_kind: kind.to_string(),
    }
}

pub fn is_durable_operator_event_kind(kind: &str) -> bool {
    matches!(
        event_category(kind),
        OperatorEventCategory::OperatorNotification
            | OperatorEventCategory::Brief
            | OperatorEventCategory::Message
            | OperatorEventCategory::WorkItem
            | OperatorEventCategory::Task
            | OperatorEventCategory::Waiting
            | OperatorEventCategory::Workspace
            | OperatorEventCategory::Runtime
    )
}

pub fn is_activity_reset_event_kind(kind: &str) -> bool {
    matches!(
        kind,
        "turn_started"
            | "message_processing_started"
            | "operator_interjection_admitted"
            | "brief_created"
            | "turn_terminal"
            | "runtime_error"
    )
}

fn event_category(kind: &str) -> OperatorEventCategory {
    match kind {
        "operator_notification_requested" => OperatorEventCategory::OperatorNotification,
        "brief_created" => OperatorEventCategory::Brief,
        "message_enqueued" | "turn_started" | "operator_interjection_admitted" => {
            OperatorEventCategory::Message
        }
        "work_item_written"
        | "work_item_delegation_created"
        | "work_item_delegation_completed"
        | "work_item_stale_reminder_injected"
        | "work_item_stale_reminder_skipped" => OperatorEventCategory::WorkItem,
        "task_created"
        | "task_status_updated"
        | "task_result_received"
        | "task_child_spawned"
        | "task_input_delivered"
        | "command_task_runner_failed"
        | "command_task_running_persisted"
        | "command_task_result_enqueue_failed" => OperatorEventCategory::Task,
        "waiting_intent_created"
        | "waiting_intent_cancelled"
        | "callback_delivered"
        | "timer_created"
        | "timer_fired" => OperatorEventCategory::Waiting,
        "workspace_entered"
        | "workspace_exited"
        | "workspace_detached"
        | "worktree_entered"
        | "worktree_exited"
        | "worktree_created_for_task"
        | "task_worktree_metadata_recorded"
        | "worktree_retained_for_review"
        | "worktree_auto_cleaned_up"
        | "worktree_auto_cleanup_failed"
        | "task_worktree_cleanup_already_removed"
        | "task_worktree_cleanup_retained"
        | "task_worktree_cleanup_failed"
        | "task_worktree_branch_cleanup_retained" => OperatorEventCategory::Workspace,
        "runtime_error" | "turn_terminal" => OperatorEventCategory::Runtime,
        "assistant_round_recorded"
        | "provider_round_completed"
        | "text_only_round_observed"
        | "max_output_tokens_recovery"
        | "turn_local_compaction_applied"
        | "turn_local_checkpoint_requested"
        | "turn_local_checkpoint_recorded"
        | "turn_local_checkpoint_resume_requested"
        | "turn_local_baseline_over_budget" => OperatorEventCategory::AssistantProgress,
        "process_execution_requested" | "tool_executed" | "tool_execution_failed" => {
            OperatorEventCategory::Tool
        }
        "agent_state_changed" | "session_state_changed" => OperatorEventCategory::StateSync,
        _ => OperatorEventCategory::Trace,
    }
}

fn event_visibility(
    kind: &str,
    payload: &Value,
    category: OperatorEventCategory,
    context: &OperatorPresentationContext,
) -> OperatorVisibility {
    match (kind, category) {
        ("operator_notification_requested", _) => OperatorVisibility::ActionRequired,
        ("brief_created", _) => brief_visibility(payload, context),
        ("work_item_written", _) if work_item_completed(payload) => OperatorVisibility::WorkDone,
        ("work_item_written", _) => OperatorVisibility::Trace,
        ("runtime_error", _) => OperatorVisibility::TurnResult,
        ("turn_terminal", _) if turn_terminal_completed(payload) => OperatorVisibility::Trace,
        (_, OperatorEventCategory::AssistantProgress) => OperatorVisibility::Progress,
        (_, OperatorEventCategory::Tool)
        | (_, OperatorEventCategory::Task)
        | (_, OperatorEventCategory::Waiting)
        | (_, OperatorEventCategory::Workspace)
        | (_, OperatorEventCategory::Message)
        | (_, OperatorEventCategory::StateSync)
        | (_, OperatorEventCategory::Trace) => OperatorVisibility::Trace,
        _ if is_durable_operator_event_kind(kind) => OperatorVisibility::TurnResult,
        _ => OperatorVisibility::Trace,
    }
}

fn brief_visibility(payload: &Value, context: &OperatorPresentationContext) -> OperatorVisibility {
    if context.awaiting_operator_input {
        return OperatorVisibility::ActionRequired;
    }
    let Some(brief) = decode_value::<BriefRecord>(payload.clone()) else {
        return OperatorVisibility::TurnResult;
    };
    if brief
        .work_item_id
        .as_deref()
        .is_some_and(|id| context.completed_work_item_ids.contains(id))
    {
        OperatorVisibility::WorkDone
    } else {
        OperatorVisibility::TurnResult
    }
}

fn work_item_completed(payload: &Value) -> bool {
    payload
        .get("record")
        .cloned()
        .and_then(decode_value::<crate::types::WorkItemRecord>)
        .is_some_and(|record| record.state == WorkItemState::Completed)
}

fn turn_terminal_completed(payload: &Value) -> bool {
    payload
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "completed")
}

fn event_text(
    kind: &str,
    payload: &Value,
    fallback_summary: &str,
    category: OperatorEventCategory,
) -> (String, Option<String>, String) {
    match kind {
        "operator_notification_requested" => operator_notification_text(payload, fallback_summary),
        "task_created" | "task_status_updated" | "task_result_received" => {
            task_record_text(kind, payload, fallback_summary)
        }
        "command_task_runner_failed" => command_task_runner_failed_text(payload, fallback_summary),
        "command_task_running_persisted" => command_task_running_persisted_text(payload),
        "command_task_result_enqueue_failed" => {
            command_task_result_enqueue_failed_text(payload, fallback_summary)
        }
        "task_child_spawned" => task_child_spawned_text(payload, fallback_summary),
        "task_input_delivered" => task_input_delivered_text(payload, fallback_summary),
        "waiting_intent_created" => waiting_intent_text(payload, fallback_summary, false),
        "waiting_intent_cancelled" => waiting_intent_text(payload, fallback_summary, true),
        "callback_delivered" => callback_delivered_text(payload, fallback_summary),
        "timer_created" => timer_text(payload, fallback_summary, false),
        "timer_fired" => timer_text(payload, fallback_summary, true),
        "work_item_written" => work_item_text(payload, fallback_summary),
        "work_item_stale_reminder_injected" => work_item_stale_reminder_text(payload, false),
        "work_item_stale_reminder_skipped" => work_item_stale_reminder_text(payload, true),
        "work_item_delegation_created" => work_item_delegation_text(payload, false),
        "work_item_delegation_completed" => work_item_delegation_text(payload, true),
        "workspace_entered" => workspace_text(payload, fallback_summary, "Entered workspace"),
        "workspace_exited" => workspace_text(payload, fallback_summary, "Exited workspace"),
        "workspace_detached" => workspace_text(payload, fallback_summary, "Detached workspace"),
        "worktree_entered" => worktree_text(payload, fallback_summary, true),
        "worktree_exited" => worktree_text(payload, fallback_summary, false),
        "turn_terminal" => turn_terminal_text(payload),
        "assistant_round_recorded" => assistant_round_recorded_text(payload),
        "provider_round_completed" => provider_round_text(payload),
        "text_only_round_observed" => text_only_round_text(payload),
        "max_output_tokens_recovery" => max_output_recovery_text(payload),
        "turn_local_compaction_applied" => turn_local_compaction_text(payload),
        "turn_local_checkpoint_requested" => turn_local_checkpoint_requested_text(payload),
        "turn_local_checkpoint_recorded" => turn_local_checkpoint_recorded_text(payload),
        "turn_local_checkpoint_resume_requested" => turn_local_checkpoint_resume_text(payload),
        "turn_local_baseline_over_budget" => turn_local_baseline_over_budget_text(payload),
        "process_execution_requested" => process_execution_text(payload, fallback_summary),
        "tool_executed" | "tool_execution_failed" => tool_text(kind, payload, fallback_summary),
        _ => {
            let title = category_title(category, kind);
            (title, None, fallback_summary.to_string())
        }
    }
}

fn task_record_text(
    kind: &str,
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let Some(task) = decode_value::<TaskRecord>(payload.clone()) else {
        return ("Task".into(), None, fallback_summary.to_string());
    };
    let summary = task
        .summary
        .as_deref()
        .unwrap_or_else(|| task.kind.as_str())
        .trim();
    let label = match kind {
        "task_created" => "Task queued",
        "task_status_updated" => task_status_update_label(&task.status),
        "task_result_received" => task_result_label(&task.status),
        _ => "Task updated",
    };
    let body = (!summary.is_empty()).then(|| summary.to_string());
    let summary = if summary.is_empty() {
        format!("{label}: {}", task.id)
    } else {
        format!("{label}: {summary}")
    };
    (label.into(), body, summary)
}

fn task_status_update_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "Task queued",
        TaskStatus::Running => "Task running",
        TaskStatus::Cancelling => "Task cancelling",
        TaskStatus::Completed => "Task completed",
        TaskStatus::Failed => "Task failed",
        TaskStatus::Cancelled => "Task cancelled",
        TaskStatus::Interrupted => "Task interrupted",
    }
}

fn task_result_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Completed => "Task completed",
        TaskStatus::Failed => "Task failed",
        TaskStatus::Cancelled => "Task cancelled",
        TaskStatus::Interrupted => "Task interrupted",
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling => "Task result received",
    }
}

fn command_task_runner_failed_text(
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let task_id = payload.get("task_id").and_then(Value::as_str);
    let error = payload
        .get("error")
        .and_then(Value::as_str)
        .map(trim_summary);
    let body = error.clone().or_else(|| task_id.map(ToString::to_string));
    let summary = match (task_id, error) {
        (Some(task_id), Some(error)) => format!("Command task runner failed: {task_id}: {error}"),
        (Some(task_id), None) => format!("Command task runner failed: {task_id}"),
        (None, Some(error)) => format!("Command task runner failed: {error}"),
        (None, None) => fallback_summary.to_string(),
    };
    ("Command task runner failed".into(), body, summary)
}

fn command_task_running_persisted_text(payload: &Value) -> (String, Option<String>, String) {
    let task_id = payload.get("task_id").and_then(Value::as_str);
    let body = task_id.map(ToString::to_string);
    let summary = task_id
        .map(|task_id| format!("Command task running: {task_id}"))
        .unwrap_or_else(|| "Command task running".into());
    ("Command task running".into(), body, summary)
}

fn command_task_result_enqueue_failed_text(
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let task_id = payload.get("task_id").and_then(Value::as_str);
    let error = payload
        .get("error")
        .and_then(Value::as_str)
        .map(trim_summary);
    let body = error.clone().or_else(|| task_id.map(ToString::to_string));
    let summary = match (task_id, error) {
        (Some(task_id), Some(error)) => {
            format!("Command task result enqueue failed: {task_id}: {error}")
        }
        (Some(task_id), None) => format!("Command task result enqueue failed: {task_id}"),
        (None, Some(error)) => format!("Command task result enqueue failed: {error}"),
        (None, None) => fallback_summary.to_string(),
    };
    ("Command task result enqueue failed".into(), body, summary)
}

fn operator_notification_text(
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let summary = payload
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .map(trim_summary)
        .unwrap_or_else(|| fallback_summary.to_string());
    let boundary = payload
        .get("target_operator_boundary")
        .and_then(Value::as_str)
        .unwrap_or("primary_operator");
    if boundary == "parent_supervisor" {
        let requested_by = payload
            .get("requested_by_agent_id")
            .and_then(Value::as_str)
            .unwrap_or("child");
        return (
            "Parent supervision needed".into(),
            Some(summary.clone()),
            format!("Child {requested_by} needs parent supervision: {summary}"),
        );
    }
    (
        "Operator attention".into(),
        Some(summary.clone()),
        format!("Operator attention needed: {summary}"),
    )
}

fn waiting_intent_text(
    payload: &Value,
    fallback_summary: &str,
    cancelled: bool,
) -> (String, Option<String>, String) {
    let Some(waiting) = decode_value::<WaitingIntentRecord>(payload.clone()) else {
        let title = if cancelled {
            "Stopped waiting"
        } else {
            "Waiting"
        };
        return (title.into(), None, fallback_summary.to_string());
    };
    let description = trim_summary(&waiting.description);
    if cancelled {
        return (
            "Stopped waiting".into(),
            Some(description.clone()),
            format!("Stopped waiting: {description}"),
        );
    }
    (
        "Waiting".into(),
        Some(description.clone()),
        format!("Waiting: {description}"),
    )
}

fn callback_delivered_text(
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let waiting_id = payload.get("waiting_intent_id").and_then(Value::as_str);
    let source = payload.get("source").and_then(Value::as_str);
    let summary = match (source, waiting_id) {
        (Some(source), Some(waiting_id)) => {
            format!("External event received from {source} for wait {waiting_id}")
        }
        (Some(source), None) => format!("External event received from {source}"),
        (None, Some(waiting_id)) => format!("External event received for wait {waiting_id}"),
        (None, None) => fallback_summary.to_string(),
    };
    (
        "External event received".into(),
        source.map(str::to_string),
        summary,
    )
}

fn timer_text(
    payload: &Value,
    fallback_summary: &str,
    fired: bool,
) -> (String, Option<String>, String) {
    let Some(timer) = decode_value::<TimerRecord>(payload.clone()) else {
        let title = if fired {
            "Timer fired"
        } else {
            "Timer scheduled"
        };
        return (title.into(), None, fallback_summary.to_string());
    };
    let body = timer.summary.clone();
    let label = if fired {
        "Timer fired"
    } else {
        "Timer scheduled"
    };
    let summary = body
        .as_deref()
        .map(|summary| format!("{label}: {}", trim_summary(summary)))
        .unwrap_or_else(|| format!("{label}: {}", timer.id));
    (label.into(), body, summary)
}

fn work_item_text(payload: &Value, fallback_summary: &str) -> (String, Option<String>, String) {
    let Some(record) = payload
        .get("record")
        .cloned()
        .and_then(decode_value::<WorkItemRecord>)
    else {
        return ("Work item".into(), None, fallback_summary.to_string());
    };
    let objective = trim_summary(&record.objective);
    let state = format!("{:?}", record.state);
    let title = if record.state == WorkItemState::Completed {
        "Work completed"
    } else {
        "Work item updated"
    };
    let summary = if record.state == WorkItemState::Completed {
        record
            .result_summary
            .as_deref()
            .map(trim_summary)
            .map(|result| format!("Work completed: {result}"))
            .unwrap_or_else(|| format!("Work completed: {objective}"))
    } else {
        format!("Work item {state}: {objective}")
    };
    (title.into(), Some(objective), summary)
}

fn work_item_stale_reminder_text(
    payload: &Value,
    skipped: bool,
) -> (String, Option<String>, String) {
    let work_item_id = payload.get("work_item_id").and_then(Value::as_str);
    let reason = payload.get("reason").and_then(Value::as_str);
    if skipped {
        let summary = match (work_item_id, reason) {
            (Some(work_item_id), Some(reason)) => {
                format!("Work reminder skipped: {work_item_id} ({reason})")
            }
            (Some(work_item_id), None) => format!("Work reminder skipped: {work_item_id}"),
            (None, Some(reason)) => format!("Work reminder skipped: {reason}"),
            (None, None) => "Work reminder skipped".into(),
        };
        return (
            "Work reminder skipped".into(),
            reason.map(ToString::to_string),
            summary,
        );
    }

    let text_preview = payload
        .get("text_preview")
        .and_then(Value::as_str)
        .map(trim_summary);
    let summary = match (work_item_id, text_preview.clone()) {
        (Some(work_item_id), Some(text)) => {
            format!("Work reminder injected: {work_item_id}: {text}")
        }
        (Some(work_item_id), None) => format!("Work reminder injected: {work_item_id}"),
        (None, Some(text)) => format!("Work reminder injected: {text}"),
        (None, None) => "Work reminder injected".into(),
    };
    ("Work reminder injected".into(), text_preview, summary)
}

fn task_child_spawned_text(
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let task_id = payload.get("id").and_then(Value::as_str);
    let child_agent_id = payload
        .get("detail")
        .and_then(|detail| detail.get("child_agent_id"))
        .and_then(Value::as_str);
    let workspace_mode = payload
        .get("detail")
        .and_then(|detail| detail.get("workspace_mode"))
        .and_then(Value::as_str);
    let summary = match (child_agent_id, task_id) {
        (Some(child), Some(task)) => {
            let mut text = format!("Delegated child {child} started under supervision task {task}");
            if let Some(mode) = workspace_mode {
                text.push_str(&format!(" ({mode})"));
            }
            text
        }
        _ => fallback_summary.to_string(),
    };
    (
        "Delegated child started".into(),
        child_agent_id.map(ToString::to_string),
        summary,
    )
}

fn task_input_delivered_text(
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let child_agent_id = payload.get("child_agent_id").and_then(Value::as_str);
    let task_id = payload.get("task_id").and_then(Value::as_str);
    let input_target = payload.get("input_target").and_then(Value::as_str);
    if input_target == Some("child_followup") {
        if let (Some(child), Some(task)) = (child_agent_id, task_id) {
            return (
                "Parent follow-up delivered".into(),
                Some(child.to_string()),
                format!("Parent follow-up delivered to child {child} via supervision task {task}"),
            );
        }
    }
    (
        "Task input delivered".into(),
        task_id.map(ToString::to_string),
        fallback_summary.to_string(),
    )
}

fn workspace_text(
    payload: &Value,
    fallback_summary: &str,
    label: &'static str,
) -> (String, Option<String>, String) {
    let workspace_id = payload.get("workspace_id").and_then(Value::as_str);
    if let Some(workspace_id) = workspace_id {
        return (
            label.into(),
            Some(workspace_id.to_string()),
            format!("{label}: {workspace_id}"),
        );
    }
    (label.into(), None, fallback_summary.to_string())
}

fn worktree_text(
    payload: &Value,
    fallback_summary: &str,
    entered: bool,
) -> (String, Option<String>, String) {
    let label = if entered {
        "Entered worktree"
    } else {
        "Exited worktree"
    };
    let branch = payload
        .get("worktree")
        .cloned()
        .and_then(decode_value::<WorktreeSession>)
        .map(|worktree| worktree.worktree_branch)
        .or_else(|| {
            payload
                .get("worktree_branch")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
    if let Some(branch) = branch {
        return (
            label.into(),
            Some(branch.clone()),
            format!("{label}: {branch}"),
        );
    }
    (label.into(), None, fallback_summary.to_string())
}

fn work_item_delegation_text(payload: &Value, completed: bool) -> (String, Option<String>, String) {
    let parent = payload
        .get("parent_work_item_id")
        .and_then(Value::as_str)
        .unwrap_or("parent work item");
    let child = payload
        .get("child_work_item_id")
        .and_then(Value::as_str)
        .unwrap_or("child work item");
    let child_agent = payload
        .get("child_agent_id")
        .and_then(Value::as_str)
        .unwrap_or("child");
    if completed {
        (
            "Delegated work completed".into(),
            Some(child_agent.to_string()),
            format!("Delegated work from {parent} completed by child {child_agent} ({child})"),
        )
    } else {
        (
            "Delegated work linked".into(),
            Some(child_agent.to_string()),
            format!("Delegated work from {parent} linked to child {child_agent} ({child})"),
        )
    }
}

fn process_execution_text(
    payload: &Value,
    _fallback_summary: &str,
) -> (String, Option<String>, String) {
    let surface = payload
        .get("surface")
        .and_then(Value::as_str)
        .unwrap_or("process");
    let cmd_preview = payload
        .get("cmd_preview")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("command_cost")
                .and_then(|value| value.get("cmd_preview"))
                .and_then(Value::as_str)
        })
        .map(collapse_whitespace)
        .filter(|cmd| !cmd.is_empty());
    let label = match surface {
        "ExecCommand" | "ExecCommandBatch" => "Command started",
        "command_task" => "Background command started",
        _ => "Process started",
    };
    if let Some(cmd_preview) = cmd_preview {
        return (
            label.into(),
            Some(cmd_preview.clone()),
            format!("{label}: {cmd_preview}"),
        );
    }
    let body = match surface {
        "ExecCommand" | "ExecCommandBatch" => "Command details are available in the event log.",
        "command_task" => "Background command details are available in the task and event logs.",
        _ => "Process details are available in the event log.",
    };
    (label.into(), Some(body.into()), label.into())
}

fn assistant_round_recorded_text(payload: &Value) -> (String, Option<String>, String) {
    let text = payload
        .get("text_preview")
        .and_then(Value::as_str)
        .map(collapse_whitespace)
        .filter(|text| !text.is_empty());
    if let Some(text) = text {
        let body = trim_summary(&text);
        return (
            "Assistant round".into(),
            Some(body.clone()),
            format!("Assistant round: {body}"),
        );
    }

    let tool_names = tool_names(payload);
    if !tool_names.is_empty() {
        let tools = tool_names.join(", ");
        let body = format!("requested tools: {tools}");
        return (
            "Assistant requested tools".into(),
            Some(body),
            format!("Assistant requested tools: {tools}"),
        );
    }

    let stop_reason = payload
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    (
        "Assistant round".into(),
        Some(format!("Stop reason: {stop_reason}")),
        format!("Assistant round completed without text (stop={stop_reason})"),
    )
}

fn tool_names(payload: &Value) -> Vec<String> {
    payload
        .get("tool_names")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn provider_round_text(payload: &Value) -> (String, Option<String>, String) {
    let round = payload
        .get("round")
        .and_then(Value::as_u64)
        .map(|round| format!("round {round}"))
        .unwrap_or_else(|| "round".into());
    let model = payload
        .get("active_model")
        .and_then(Value::as_str)
        .or_else(|| payload.get("requested_model").and_then(Value::as_str))
        .filter(|model| !model.trim().is_empty())
        .unwrap_or("model");
    let stop = payload
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let input_tokens = payload.get("input_tokens").and_then(Value::as_u64);
    let output_tokens = payload.get("output_tokens").and_then(Value::as_u64);
    let tokens = match (input_tokens, output_tokens) {
        (Some(input), Some(output)) => format!("{input}/{output} tokens"),
        _ => "tokens unavailable".into(),
    };
    let tool_count = payload
        .get("tool_call_count")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| tool_names(payload).len() as u64);
    let body = format!("model={model}; stop={stop}; {tokens}; tools={tool_count}");
    (
        "Provider round completed".into(),
        Some(body.clone()),
        format!("Provider {round}: {body}"),
    )
}

fn turn_terminal_text(payload: &Value) -> (String, Option<String>, String) {
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("terminal");
    let duration = payload.get("duration_ms").and_then(Value::as_u64);
    let last_message = payload
        .get("last_assistant_message")
        .and_then(Value::as_str)
        .map(collapse_whitespace)
        .filter(|message| !message.is_empty())
        .map(|message| trim_summary(&message));
    let title = match kind {
        "completed" => "Turn completed",
        "aborted" => "Turn aborted",
        "baseline_over_budget" => "Turn stopped",
        _ => "Turn terminal",
    };
    let body = last_message.or_else(|| duration.map(|ms| format!("{ms} ms")));
    let summary = body
        .as_deref()
        .map(|body| format!("{title}: {body}"))
        .unwrap_or_else(|| title.to_string());
    (title.into(), body, summary)
}

fn text_only_round_text(payload: &Value) -> (String, Option<String>, String) {
    let text_preview = payload
        .get("text_preview")
        .and_then(Value::as_str)
        .map(collapse_whitespace)
        .filter(|text| !text.is_empty());
    if payload
        .get("triggered_recovery")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return (
            "Output limit recovery".into(),
            text_preview.clone(),
            text_preview
                .map(|text| format!("Output limit recovery: continuing after {text}"))
                .unwrap_or_else(|| "Output limit recovery: requesting continuation".into()),
        );
    }
    if let Some(text_preview) = text_preview {
        let body = trim_summary(&text_preview);
        return (
            "Model text observed".into(),
            Some(body.clone()),
            format!("Model text observed: {body}"),
        );
    }
    (
        "Model returned no content".into(),
        None,
        "Model returned no content".into(),
    )
}

fn max_output_recovery_text(payload: &Value) -> (String, Option<String>, String) {
    let attempt = payload
        .get("attempt")
        .and_then(Value::as_u64)
        .map(|attempt| format!("attempt {attempt}"));
    let summary = attempt
        .as_deref()
        .map(|attempt| format!("Output limit recovery: continuing ({attempt})"))
        .unwrap_or_else(|| "Output limit recovery: continuing".into());
    ("Output limit recovery".into(), attempt, summary)
}

fn turn_local_compaction_text(payload: &Value) -> (String, Option<String>, String) {
    let compacted = payload.get("compacted_rounds").and_then(Value::as_u64);
    let exact_tail = payload.get("exact_tail_rounds").and_then(Value::as_u64);
    let body = match (compacted, exact_tail) {
        (Some(compacted), Some(exact_tail)) => {
            format!("Compacted {compacted} rounds; keeping {exact_tail} recent rounds exact.")
        }
        (Some(compacted), None) => format!("Compacted {compacted} older rounds."),
        _ => "Compressed local conversation context.".into(),
    };
    (
        "Context compacted".into(),
        Some(body.clone()),
        format!("Context compacted: {body}"),
    )
}

fn turn_local_checkpoint_requested_text(payload: &Value) -> (String, Option<String>, String) {
    let mode = payload
        .get("checkpoint_mode")
        .and_then(Value::as_str)
        .unwrap_or("checkpoint");
    (
        "Context checkpoint requested".into(),
        Some(mode.to_string()),
        format!("Context checkpoint requested: {mode}"),
    )
}

fn turn_local_checkpoint_recorded_text(payload: &Value) -> (String, Option<String>, String) {
    let recorded = payload
        .get("checkpoint_recorded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text_preview = payload
        .get("text_preview")
        .and_then(Value::as_str)
        .map(trim_summary);
    if recorded {
        return (
            "Context checkpoint recorded".into(),
            text_preview.clone(),
            text_preview
                .map(|text| format!("Context checkpoint recorded: {text}"))
                .unwrap_or_else(|| "Context checkpoint recorded".into()),
        );
    }
    (
        "Context checkpoint empty".into(),
        None,
        "Context checkpoint produced no visible text".into(),
    )
}

fn turn_local_checkpoint_resume_text(_payload: &Value) -> (String, Option<String>, String) {
    (
        "Context checkpoint resume".into(),
        Some("Asking the model to continue after refreshing local context.".into()),
        "Refreshing local context; asking the model to continue".into(),
    )
}

fn turn_local_baseline_over_budget_text(payload: &Value) -> (String, Option<String>, String) {
    let reason = payload
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("prompt budget");
    (
        "Prompt budget exceeded".into(),
        Some(reason.to_string()),
        format!("Prompt budget exceeded before provider request: {reason}"),
    )
}

fn tool_text(
    kind: &str,
    payload: &Value,
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let title = if kind == "tool_execution_failed" {
        "Tool failed"
    } else {
        "Tool executed"
    };
    let Some(tool_name) = payload.get("tool_name").and_then(Value::as_str) else {
        return (title.into(), None, fallback_summary.to_string());
    };
    if tool_name == "ExecCommand" {
        if let Some(cmd) = payload.get("exec_command_cmd").and_then(Value::as_str) {
            let summary = if kind == "tool_execution_failed" {
                format!("ExecCommand failed: {cmd}")
            } else {
                format!("ExecCommand: {cmd}")
            };
            return (title.into(), Some(cmd.to_string()), summary);
        }
    }
    let summary = if kind == "tool_execution_failed" {
        format!("Tool failed: {tool_name}")
    } else {
        format!("Tool executed: {tool_name}")
    };
    (title.into(), Some(tool_name.to_string()), summary)
}

fn category_title(category: OperatorEventCategory, kind: &str) -> String {
    match category {
        OperatorEventCategory::OperatorNotification => "Operator attention".into(),
        OperatorEventCategory::Brief => "Brief".into(),
        OperatorEventCategory::Message => "Message".into(),
        OperatorEventCategory::WorkItem => "Work item".into(),
        OperatorEventCategory::Task => "Task".into(),
        OperatorEventCategory::Waiting => "External trigger".into(),
        OperatorEventCategory::Workspace => "Workspace".into(),
        OperatorEventCategory::Runtime => "Runtime".into(),
        OperatorEventCategory::AssistantProgress => "Assistant progress".into(),
        OperatorEventCategory::Tool => "Tool".into(),
        OperatorEventCategory::StateSync => "State sync".into(),
        OperatorEventCategory::Trace => kind.to_string(),
    }
}

fn decode_value<T: serde::de::DeserializeOwned>(value: Value) -> Option<T> {
    serde_json::from_value(value).ok()
}

fn trim_summary(value: &str) -> String {
    const LIMIT: usize = 120;
    if value.chars().count() <= LIMIT {
        value.to_string()
    } else {
        let mut trimmed = value
            .chars()
            .take(LIMIT.saturating_sub(1))
            .collect::<String>();
        trimmed.push('…');
        trimmed
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        present_operator_event, OperatorEventCategory, OperatorPresentationContext,
        OperatorVisibility,
    };
    use serde_json::json;

    #[test]
    fn assistant_round_recorded_distinguishes_text_from_tool_requests() {
        let context = OperatorPresentationContext::default();
        let text = present_operator_event(
            "assistant_round_recorded",
            &json!({ "text_preview": "thinking", "tool_names": ["ExecCommand"] }),
            "fallback",
            &context,
        );
        assert_eq!(text.visibility, OperatorVisibility::Progress);
        assert_eq!(text.category, OperatorEventCategory::AssistantProgress);
        assert_eq!(text.summary, "Assistant round: thinking");

        let multiline = present_operator_event(
            "assistant_round_recorded",
            &json!({ "text_preview": "thinking\n\nabout\ttools  now" }),
            "fallback",
            &context,
        );
        assert_eq!(
            multiline.summary,
            "Assistant round: thinking about tools now"
        );

        let tools = present_operator_event(
            "assistant_round_recorded",
            &json!({ "text_preview": null, "tool_names": ["ExecCommand", "ReadFile"] }),
            "fallback",
            &context,
        );
        assert_eq!(
            tools.summary,
            "Assistant requested tools: ExecCommand, ReadFile"
        );
        assert_eq!(
            tools.body.as_deref(),
            Some("requested tools: ExecCommand, ReadFile")
        );

        let empty = present_operator_event(
            "assistant_round_recorded",
            &json!({ "text_preview": null, "tool_names": [], "stop_reason": "end_turn" }),
            "fallback",
            &context,
        );
        assert_eq!(
            empty.summary,
            "Assistant round completed without text (stop=end_turn)"
        );
    }

    #[test]
    fn provider_round_completed_presents_provider_telemetry() {
        let context = OperatorPresentationContext::default();
        let provider = present_operator_event(
            "provider_round_completed",
            &json!({
                "round": 2,
                "active_model": "deepseek-chat",
                "stop_reason": "tool_use",
                "input_tokens": 12,
                "output_tokens": 7,
                "tool_call_count": 1
            }),
            "fallback",
            &context,
        );
        assert_eq!(provider.visibility, OperatorVisibility::Progress);
        assert_eq!(provider.category, OperatorEventCategory::AssistantProgress);
        assert_eq!(
            provider.summary,
            "Provider round 2: model=deepseek-chat; stop=tool_use; 12/7 tokens; tools=1"
        );
        assert_eq!(provider.title, "Provider round completed");
    }

    #[test]
    fn completed_turn_terminal_is_trace_but_failures_are_turn_results() {
        let context = OperatorPresentationContext::default();
        let completed = present_operator_event(
            "turn_terminal",
            &json!({ "kind": "completed", "duration_ms": 42 }),
            "turn completed",
            &context,
        );
        assert_eq!(completed.visibility, OperatorVisibility::Trace);
        assert_eq!(completed.summary, "Turn completed: 42 ms");

        let aborted = present_operator_event(
            "turn_terminal",
            &json!({ "kind": "aborted", "last_assistant_message": "need more input" }),
            "turn aborted",
            &context,
        );
        assert_eq!(aborted.visibility, OperatorVisibility::TurnResult);
        assert_eq!(aborted.summary, "Turn aborted: need more input");
    }

    #[test]
    fn turn_local_events_explain_context_management() {
        let context = OperatorPresentationContext::default();
        let checkpoint = present_operator_event(
            "turn_local_checkpoint_resume_requested",
            &json!({ "round": 3 }),
            "turn_local_checkpoint_resume_requested",
            &context,
        );
        assert_eq!(
            checkpoint.summary,
            "Refreshing local context; asking the model to continue"
        );

        let recovery = present_operator_event(
            "max_output_tokens_recovery",
            &json!({ "attempt": 2 }),
            "max_output_tokens_recovery",
            &context,
        );
        assert_eq!(
            recovery.summary,
            "Output limit recovery: continuing (attempt 2)"
        );
    }

    #[test]
    fn state_sync_is_not_conversation_presentation() {
        let presentation = present_operator_event(
            "agent_state_changed",
            &json!({ "status": "AwakeRunning" }),
            "agent_state_changed",
            &OperatorPresentationContext::default(),
        );
        assert_eq!(presentation.category, OperatorEventCategory::StateSync);
        assert_eq!(presentation.visibility, OperatorVisibility::Trace);
        assert!(!presentation.is_conversation_candidate());
    }

    #[test]
    fn delegated_child_events_use_supervision_vocabulary() {
        let context = OperatorPresentationContext::default();
        let spawned = present_operator_event(
            "task_child_spawned",
            &json!({
                "id": "task-1",
                "detail": {
                    "child_agent_id": "child-1",
                    "workspace_mode": "worktree"
                }
            }),
            "fallback",
            &context,
        );
        assert_eq!(spawned.category, OperatorEventCategory::Task);
        assert_eq!(spawned.title, "Delegated child started");
        assert_eq!(
            spawned.summary,
            "Delegated child child-1 started under supervision task task-1 (worktree)"
        );

        let followup = present_operator_event(
            "task_input_delivered",
            &json!({
                "task_id": "task-1",
                "child_agent_id": "child-1",
                "input_target": "child_followup"
            }),
            "fallback",
            &context,
        );
        assert_eq!(followup.title, "Parent follow-up delivered");
        assert_eq!(
            followup.summary,
            "Parent follow-up delivered to child child-1 via supervision task task-1"
        );

        let notification = present_operator_event(
            "operator_notification_requested",
            &json!({
                "requested_by_agent_id": "child-1",
                "target_operator_boundary": "parent_supervisor",
                "summary": "need parent decision"
            }),
            "fallback",
            &context,
        );
        assert_eq!(notification.visibility, OperatorVisibility::ActionRequired);
        assert_eq!(notification.title, "Parent supervision needed");
        assert_eq!(
            notification.summary,
            "Child child-1 needs parent supervision: need parent decision"
        );
    }

    #[test]
    fn process_execution_requested_uses_command_vocabulary() {
        let presentation = present_operator_event(
            "process_execution_requested",
            &json!({
                "surface": "ExecCommand",
                "cmd_preview": "cargo test -q tui::chat",
            }),
            "process_execution_requested",
            &OperatorPresentationContext::default(),
        );

        assert_eq!(presentation.category, OperatorEventCategory::Tool);
        assert_eq!(presentation.visibility, OperatorVisibility::Trace);
        assert_eq!(
            presentation.summary,
            "Command started: cargo test -q tui::chat"
        );

        let background = present_operator_event(
            "process_execution_requested",
            &json!({ "surface": "command_task" }),
            "process_execution_requested",
            &OperatorPresentationContext::default(),
        );
        assert_eq!(background.summary, "Background command started");
        assert_eq!(
            background.body.as_deref(),
            Some("Background command details are available in the task and event logs.")
        );
    }
}
