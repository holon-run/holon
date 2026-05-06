use std::collections::BTreeSet;

use serde_json::Value;

use crate::types::{BriefRecord, WorkItemState};

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
    pub const DEFAULT_DISPLAY_LEVEL: Self = Self::TurnResult;

    pub fn from_display_level(level: u8) -> Option<Self> {
        match level {
            3 => Some(Self::TurnResult),
            4 => Some(Self::Progress),
            5 => Some(Self::Trace),
            _ => None,
        }
    }

    pub fn display_level(self) -> u8 {
        self as u8
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
        "work_item_written" | "work_item_delegation_created" | "work_item_delegation_completed" => {
            OperatorEventCategory::WorkItem
        }
        "task_created"
        | "task_status_updated"
        | "task_result_received"
        | "task_child_spawned"
        | "task_input_delivered" => OperatorEventCategory::Task,
        "waiting_intent_created" | "waiting_intent_cancelled" | "callback_delivered" => {
            OperatorEventCategory::Waiting
        }
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
        "provider_round_completed" | "text_only_round_observed" => {
            OperatorEventCategory::AssistantProgress
        }
        "tool_executed" | "tool_execution_failed" => OperatorEventCategory::Tool,
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

fn event_text(
    kind: &str,
    payload: &Value,
    fallback_summary: &str,
    category: OperatorEventCategory,
) -> (String, Option<String>, String) {
    match kind {
        "operator_notification_requested" => operator_notification_text(payload, fallback_summary),
        "task_child_spawned" => task_child_spawned_text(payload, fallback_summary),
        "task_input_delivered" => task_input_delivered_text(payload, fallback_summary),
        "work_item_delegation_created" => work_item_delegation_text(payload, false),
        "work_item_delegation_completed" => work_item_delegation_text(payload, true),
        "provider_round_completed" => provider_round_text(payload),
        "text_only_round_observed" => (
            "Assistant progress".into(),
            None,
            "Text-only model round observed".into(),
        ),
        "tool_executed" | "tool_execution_failed" => tool_text(kind, payload, fallback_summary),
        _ => {
            let title = category_title(category, kind);
            (title, None, fallback_summary.to_string())
        }
    }
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

fn provider_round_text(payload: &Value) -> (String, Option<String>, String) {
    let text_preview = payload
        .get("text_preview")
        .and_then(Value::as_str)
        .map(collapse_whitespace)
        .filter(|text| !text.is_empty());
    if let Some(text_preview) = text_preview.as_deref() {
        let body = trim_summary(text_preview);
        return (
            "Assistant progress".into(),
            Some(body.clone()),
            format!("Assistant progress: {body}"),
        );
    }

    let tool_names = payload
        .get("tool_names")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !tool_names.is_empty() {
        let tools = tool_names.join(", ");
        return (
            "Model requested tools".into(),
            Some(tools.clone()),
            format!("Model requested tools: {tools}"),
        );
    }

    (
        "Assistant progress".into(),
        None,
        "Provider round completed".into(),
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
    fn provider_round_distinguishes_text_from_tool_requests() {
        let context = OperatorPresentationContext::default();
        let text = present_operator_event(
            "provider_round_completed",
            &json!({ "text_preview": "thinking", "tool_names": ["ExecCommand"] }),
            "fallback",
            &context,
        );
        assert_eq!(text.visibility, OperatorVisibility::Progress);
        assert_eq!(text.category, OperatorEventCategory::AssistantProgress);
        assert_eq!(text.summary, "Assistant progress: thinking");

        let multiline = present_operator_event(
            "provider_round_completed",
            &json!({ "text_preview": "thinking\n\nabout\ttools  now" }),
            "fallback",
            &context,
        );
        assert_eq!(
            multiline.summary,
            "Assistant progress: thinking about tools now"
        );

        let tools = present_operator_event(
            "provider_round_completed",
            &json!({ "text_preview": null, "tool_names": ["ExecCommand", "ReadFile"] }),
            "fallback",
            &context,
        );
        assert_eq!(
            tools.summary,
            "Model requested tools: ExecCommand, ReadFile"
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
}
