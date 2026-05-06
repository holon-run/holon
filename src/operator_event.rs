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
    pub debug_payload: Option<Value>,
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
    let debug_payload = if matches!(
        category,
        OperatorEventCategory::StateSync | OperatorEventCategory::Trace
    ) {
        Some(payload.clone())
    } else {
        None
    };

    OperatorEventPresentation {
        visibility,
        category,
        title,
        body,
        summary,
        source_event_kind: kind.to_string(),
        debug_payload,
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
        "work_item_written" => OperatorEventCategory::WorkItem,
        "task_created" | "task_status_updated" | "task_result_received" => {
            OperatorEventCategory::Task
        }
        "waiting_intent_created" | "waiting_intent_cancelled" | "callback_delivered" => {
            OperatorEventCategory::Waiting
        }
        "workspace_entered" | "workspace_exited" | "workspace_detached" | "worktree_entered"
        | "worktree_exited" => OperatorEventCategory::Workspace,
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

fn provider_round_text(payload: &Value) -> (String, Option<String>, String) {
    let text_preview = payload
        .get("text_preview")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if let Some(text_preview) = text_preview {
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
}
