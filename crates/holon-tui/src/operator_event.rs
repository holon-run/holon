use std::collections::BTreeSet;

use holon_domain::tool_names as tn;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{
    BriefCreatedAuditEvent, BriefRecord, TaskRecord, TaskStatus, TimerRecord,
    WorkItemLifecycleAuditEvent, WorkItemRecord, WorkItemState, WorktreeSession,
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OperatorDisplayMode {
    /// Result-oriented operator view.
    Info = 3,
    /// Compact activity view.
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
    Skill,
    Configuration,
    Control,
    Context,
    Delivery,
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
                | OperatorEventCategory::Skill
                | OperatorEventCategory::Configuration
                | OperatorEventCategory::Control
                | OperatorEventCategory::Context
                | OperatorEventCategory::Delivery
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
            "runtime_error"
                | "turn_terminal"
                | "deferred_to_fallback"
                | "provider_failed_needs_recovery"
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

pub fn is_operator_event_in_display_mode(
    kind: &str,
    payload: &Value,
    fallback_summary: &str,
    context: &OperatorPresentationContext,
    display_mode: OperatorDisplayMode,
) -> bool {
    let presentation = present_operator_event(kind, payload, fallback_summary, context);
    match display_mode {
        OperatorDisplayMode::Info => is_info_event(kind, payload, &presentation),
        OperatorDisplayMode::Verbose => {
            is_info_event(kind, payload, &presentation)
                || presentation.visibility.display_level() <= display_mode.display_level()
                || is_verbose_event(kind, payload)
        }
        OperatorDisplayMode::Debug => {
            is_info_event(kind, payload, &presentation)
                || presentation.visibility.display_level() <= display_mode.display_level()
                || is_verbose_event(kind, payload)
                || is_debug_event(kind, payload)
        }
    }
}

fn is_info_event(kind: &str, payload: &Value, presentation: &OperatorEventPresentation) -> bool {
    is_operator_message_event(kind, payload)
        || presentation.is_conversation_candidate()
            && matches!(
                presentation.visibility,
                OperatorVisibility::ActionRequired
                    | OperatorVisibility::TurnResult
                    | OperatorVisibility::WorkDone
            )
}

fn is_operator_message_event(kind: &str, payload: &Value) -> bool {
    if kind != "message_enqueued" {
        return false;
    }
    payload
        .get("origin")
        .and_then(|origin| origin.get("kind"))
        .and_then(Value::as_str)
        == Some("operator")
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
            | OperatorEventCategory::Skill
            | OperatorEventCategory::Configuration
            | OperatorEventCategory::Control
            | OperatorEventCategory::Context
            | OperatorEventCategory::Delivery
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
            | "deferred_to_fallback"
            | "provider_failed_needs_recovery"
    )
}

fn event_category(kind: &str) -> OperatorEventCategory {
    match kind {
        "operator_notification_requested" => OperatorEventCategory::OperatorNotification,
        "brief_created" => OperatorEventCategory::Brief,
        "operator_interjection_admitted"
        | "message_acknowledged"
        | "message_enqueued"
        | "message_admitted"
        | "message_processing_started"
        | "message_processing_aborted"
        | "turn_started" => OperatorEventCategory::Message,
        "work_item_written"
        | "work_item_picked"
        | "work_item_focus_released"
        | "work_item_enqueue_requested"
        | "work_item_turn_end_committed"
        | "work_item_turn_end_commit_skipped"
        | "work_item_delegation_created"
        | "work_item_delegation_completed"
        | "work_item_completion_report_promoted"
        | "work_item_completion_report_candidate_promoted"
        | "work_item_stale_reminder_injected"
        | "work_item_stale_reminder_skipped"
        | "missing_current_work_item_before_wait"
        | "work_queue_redispatch_stalled" => OperatorEventCategory::WorkItem,
        "task_created"
        | "task_status_updated"
        | "task_result_received"
        | "task_child_spawned"
        | "task_input_delivered"
        | "task_create_requested"
        | "supervised_child_task_monitor_reattached"
        | "supervised_child_task_recovery_failed"
        | "command_task_runner_failed"
        | "command_task_result_enqueue_failed" => OperatorEventCategory::Task,
        "callback_delivered"
        | "timer_create_requested"
        | "timer_created"
        | "timer_fired"
        | "timer_fire_failed" => OperatorEventCategory::Waiting,
        "workspace_attach_requested"
        | "workspace_attached"
        | "workspace_entered"
        | "workspace_exit_requested"
        | "workspace_exited"
        | "workspace_detach_requested"
        | "workspace_detached"
        | "workspace_used"
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
        "skill_activated" | "skill_installed" | "skill_uninstalled" => OperatorEventCategory::Skill,
        "agent_created"
        | "agent_model_override_requested"
        | "agent_model_override_set"
        | "agent_model_override_clear_requested"
        | "agent_model_override_cleared" => OperatorEventCategory::Configuration,
        "agent_state_changed" | "state_changed" | "session_state_changed" => {
            OperatorEventCategory::StateSync
        }
        "control_request_admitted"
        | "control_applied"
        | "current_run_aborted"
        | "wake_requested"
        | "continuation_trigger_received"
        | "continuation_resolved"
        | "closure_decided"
        | "scheduler_decision"
        | "system_tick_emitted"
        | "system_tick_suppressed"
        | "runtime_service_shutdown_requested" => OperatorEventCategory::Control,
        "debug_prompt_requested"
        | "turn_context_built"
        | "turn_context_length_exceeded"
        | "turn_local_baseline_over_budget"
        | "turn_local_compaction_applied"
        | "turn_local_checkpoint_requested"
        | "turn_local_checkpoint_recorded"
        | "turn_local_checkpoint_resume_requested"
        | "episode_memory_finalized"
        | "working_memory_updated"
        | "recovery_cleared_missing_worktree_session"
        | "max_output_tokens_recovery" => OperatorEventCategory::Context,
        "operator_delivery_submitted"
        | "operator_delivery_completed"
        | "operator_notification_mirror_failed"
        | "operator_transport_binding_upserted" => OperatorEventCategory::Delivery,
        "runtime_error"
        | "turn_terminal"
        | "deferred_to_fallback"
        | "provider_failed_needs_recovery" => OperatorEventCategory::Runtime,
        "assistant_round_recorded" | "provider_round_completed" | "text_only_round_observed" => {
            OperatorEventCategory::AssistantProgress
        }
        "process_execution_requested"
        | "tool_executed"
        | "tool_execution_failed"
        | "truncated_mutation_tool_call_rejected" => OperatorEventCategory::Tool,
        _ => OperatorEventCategory::Trace,
    }
}

fn event_visibility(
    kind: &str,
    payload: &Value,
    category: OperatorEventCategory,
    context: &OperatorPresentationContext,
) -> OperatorVisibility {
    if matches!(
        kind,
        "assistant_round_recorded" | "provider_round_completed" | "text_only_round_observed"
    ) && is_runtime_checkpoint_round(payload)
    {
        return OperatorVisibility::Trace;
    }
    match (kind, category) {
        ("operator_notification_requested", _) => OperatorVisibility::ActionRequired,
        ("brief_created", _) => brief_visibility(payload, context),
        ("work_item_written", _) if work_item_completed(payload) => OperatorVisibility::WorkDone,
        ("work_item_written", _) if work_item_created(payload) => OperatorVisibility::TurnResult,
        ("work_queue_redispatch_stalled", OperatorEventCategory::WorkItem) => {
            OperatorVisibility::TurnResult
        }
        (_, OperatorEventCategory::WorkItem) => OperatorVisibility::Progress,
        ("timer_fire_failed", OperatorEventCategory::Waiting) => OperatorVisibility::Progress,
        ("runtime_error", _) => OperatorVisibility::TurnResult,
        ("deferred_to_fallback" | "provider_failed_needs_recovery", _) => {
            OperatorVisibility::TurnResult
        }
        ("turn_terminal", _) if turn_terminal_completed(payload) => OperatorVisibility::Trace,
        ("turn_terminal", _) if turn_terminal_provider_lineage(payload) => {
            OperatorVisibility::Trace
        }
        (_, OperatorEventCategory::AssistantProgress) => OperatorVisibility::Progress,
        (_, OperatorEventCategory::Tool)
        | (_, OperatorEventCategory::Task)
        | (_, OperatorEventCategory::Waiting)
        | (_, OperatorEventCategory::Workspace)
        | (_, OperatorEventCategory::Skill)
        | (_, OperatorEventCategory::Configuration)
        | (_, OperatorEventCategory::Control)
        | (_, OperatorEventCategory::Context)
        | (_, OperatorEventCategory::Delivery)
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
    let work_item_id = decode_value::<BriefRecord>(payload.clone())
        .and_then(|brief| brief.work_item_id)
        .or_else(|| {
            decode_value::<BriefCreatedAuditEvent>(payload.clone())
                .and_then(|brief| brief.work_item_id)
        });
    if work_item_id
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
        || payload.get("action").and_then(Value::as_str) == Some("completed")
}

fn work_item_created(payload: &Value) -> bool {
    payload.get("action").and_then(Value::as_str) == Some("created")
}

fn turn_terminal_completed(payload: &Value) -> bool {
    payload
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "completed")
}

fn turn_terminal_provider_lineage(payload: &Value) -> bool {
    payload
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            matches!(
                kind,
                "deferred_to_fallback" | "provider_failed_needs_recovery"
            )
        })
}

fn is_verbose_event(kind: &str, payload: &Value) -> bool {
    if is_runtime_checkpoint_round(payload) {
        return false;
    }
    match kind {
        "assistant_round_recorded" => payload_text_present(payload, "text_preview"),
        "text_only_round_observed" => {
            payload_text_present(payload, "text_preview")
                || payload
                    .get("triggered_recovery")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        }
        "max_output_tokens_recovery"
        | "turn_local_compaction_applied"
        | "turn_local_checkpoint_resume_requested"
        | "turn_local_baseline_over_budget" => true,
        "process_execution_requested" => {
            payload_text_present(payload, "cmd_preview")
                || payload
                    .get("command_cost")
                    .and_then(|value| value.get("cmd_preview"))
                    .and_then(Value::as_str)
                    .is_some_and(|cmd| !cmd.trim().is_empty())
        }
        "tool_executed" | "tool_execution_failed" => true,
        "truncated_mutation_tool_call_rejected" => true,
        "task_result_received"
        | "task_child_spawned"
        | "supervised_child_task_recovery_failed"
        | "command_task_runner_failed"
        | "command_task_result_enqueue_failed" => true,
        "task_status_updated" => task_status_is_terminal(payload),
        "work_item_written"
        | "work_item_picked"
        | "work_item_focus_released"
        | "work_item_enqueue_requested"
        | "work_item_turn_end_committed"
        | "work_item_turn_end_commit_skipped"
        | "work_item_delegation_created"
        | "work_item_delegation_completed"
        | "work_item_completion_report_promoted"
        | "work_item_completion_report_candidate_promoted"
        | "missing_current_work_item_before_wait"
        | "work_item_stale_reminder_injected"
        | "work_item_stale_reminder_skipped"
        | "timer_fire_failed"
        | "workspace_attached"
        | "workspace_entered"
        | "workspace_exited"
        | "workspace_detached"
        | "worktree_entered"
        | "worktree_exited"
        | "worktree_created_for_task"
        | "worktree_retained_for_review"
        | "worktree_auto_cleaned_up"
        | "worktree_auto_cleanup_failed"
        | "task_worktree_cleanup_failed"
        | "skill_installed"
        | "skill_uninstalled"
        | "agent_created"
        | "agent_model_override_set"
        | "agent_model_override_cleared"
        | "current_run_aborted"
        | "control_applied"
        | "runtime_service_shutdown_requested"
        | "turn_context_length_exceeded"
        | "recovery_cleared_missing_worktree_session"
        | "operator_notification_mirror_failed" => true,
        "callback_delivered" => callback_disposition_is_triggered(payload),
        "timer_fired" => true,
        "continuation_trigger_received" => continuation_trigger_explains_resume(payload),
        _ => false,
    }
}

fn is_debug_event(kind: &str, payload: &Value) -> bool {
    match kind {
        "provider_round_completed" => provider_round_has_useful_telemetry(payload),
        "message_processing_aborted"
        | "operator_interjection_admitted"
        | "task_created"
        | "task_status_updated"
        | "task_input_delivered"
        | "task_create_requested"
        | "supervised_child_task_monitor_reattached"
        | "callback_delivered"
        | "timer_create_requested"
        | "timer_created"
        | "timer_fired"
        | "workspace_attach_requested"
        | "workspace_exit_requested"
        | "workspace_detach_requested"
        | "workspace_used"
        | "task_worktree_metadata_recorded"
        | "task_worktree_cleanup_already_removed"
        | "task_worktree_cleanup_retained"
        | "task_worktree_branch_cleanup_retained"
        | "skill_activated"
        | "agent_model_override_requested"
        | "agent_model_override_clear_requested"
        | "control_request_admitted"
        | "wake_requested"
        | "continuation_trigger_received"
        | "continuation_resolved"
        | "closure_decided"
        | "debug_prompt_requested"
        | "turn_context_built"
        | "turn_local_checkpoint_requested"
        | "turn_local_checkpoint_recorded"
        | "episode_memory_finalized"
        | "working_memory_updated"
        | "operator_delivery_submitted"
        | "operator_delivery_completed"
        | "operator_transport_binding_upserted" => true,
        _ => false,
    }
}

fn payload_text_present(payload: &Value, field: &str) -> bool {
    payload
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

fn is_runtime_checkpoint_round(payload: &Value) -> bool {
    payload.get("round_purpose").and_then(Value::as_str) == Some("runtime_checkpoint")
}

fn provider_round_has_useful_telemetry(payload: &Value) -> bool {
    let model = payload
        .get("active_model")
        .and_then(Value::as_str)
        .or_else(|| payload.get("requested_model").and_then(Value::as_str))
        .is_some_and(|model| {
            let model = model.trim();
            !model.is_empty() && model != "model"
        });
    let stop = payload
        .get("stop_reason")
        .and_then(Value::as_str)
        .is_some_and(|stop| {
            let stop = stop.trim();
            !stop.is_empty() && stop != "unknown"
        });
    let tokens = payload
        .get("input_tokens")
        .and_then(Value::as_u64)
        .is_some()
        || payload
            .get("output_tokens")
            .and_then(Value::as_u64)
            .is_some();
    let tools = payload
        .get("tool_call_count")
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0);
    model || stop || tokens || tools
}

fn task_status_is_terminal(payload: &Value) -> bool {
    payload
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| {
            matches!(
                status,
                "Completed"
                    | "completed"
                    | "Failed"
                    | "failed"
                    | "Cancelled"
                    | "cancelled"
                    | "Interrupted"
                    | "interrupted"
            )
        })
}

fn callback_disposition_is_triggered(payload: &Value) -> bool {
    payload
        .get("disposition")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("triggered"))
}

fn continuation_trigger_explains_resume(payload: &Value) -> bool {
    payload
        .get("contentful")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || payload
            .get("task_terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || payload
            .get("wake_hint_source")
            .and_then(Value::as_str)
            .is_some_and(|source| !source.trim().is_empty())
}

fn event_text(
    kind: &str,
    payload: &Value,
    fallback_summary: &str,
    category: OperatorEventCategory,
) -> (String, Option<String>, String) {
    match kind {
        "operator_notification_requested" => operator_notification_text(payload, fallback_summary),
        "brief_created" => brief_text(payload),
        "message_acknowledged" => simple_event_text("Message acknowledged", ack_body(payload)),
        "message_enqueued" => simple_event_text("Message queued", message_body_preview(payload)),
        "message_admitted" => simple_event_text("Message admitted", message_id_body(payload)),
        "message_processing_started" => {
            simple_event_text("Message processing started", message_id_body(payload))
        }
        "message_processing_aborted" => {
            simple_event_text("Message aborted", error_or_message_body(payload))
        }
        "operator_interjection_admitted" => {
            simple_event_text("Operator message admitted", message_id_body(payload))
        }
        "turn_started" => simple_event_text("Turn started", turn_body(payload)),
        "task_created" | "task_status_updated" | "task_result_received" => {
            task_record_text(kind, payload, fallback_summary)
        }
        "task_create_requested" => simple_event_text("Task creation requested", task_body(payload)),
        "supervised_child_task_monitor_reattached" => {
            simple_event_text("Child task monitor reattached", task_body(payload))
        }
        "supervised_child_task_recovery_failed" => {
            simple_event_text("Child task recovery failed", error_or_task_body(payload))
        }
        "command_task_runner_failed" => command_task_runner_failed_text(payload, fallback_summary),
        "command_task_result_enqueue_failed" => {
            command_task_result_enqueue_failed_text(payload, fallback_summary)
        }
        "task_child_spawned" => task_child_spawned_text(payload, fallback_summary),
        "task_input_delivered" => task_input_delivered_text(payload, fallback_summary),
        "work_item_picked" => simple_event_text("Work item picked", work_item_body(payload)),
        "work_item_focus_released" => {
            simple_event_text("Work item focus released", work_item_body(payload))
        }
        "work_item_enqueue_requested" => {
            simple_event_text("Work item enqueue requested", work_item_body(payload))
        }
        "work_item_turn_end_committed" => {
            simple_event_text("Work item turn committed", work_item_body(payload))
        }
        "work_item_turn_end_commit_skipped" => simple_event_text(
            "Work item turn commit skipped",
            reason_or_work_item_body(payload),
        ),
        "callback_delivered" => callback_delivered_text(payload, fallback_summary),
        "timer_create_requested" => simple_event_text("Timer requested", timer_body(payload)),
        "timer_created" => timer_text(payload, fallback_summary, false),
        "timer_fired" => timer_text(payload, fallback_summary, true),
        "timer_fire_failed" => simple_event_text("Timer failed", error_or_timer_body(payload)),
        "work_item_written" => work_item_text(payload, fallback_summary),
        "work_queue_redispatch_stalled" => queued_redispatch_stalled_text(payload),
        "work_item_stale_reminder_injected" => work_item_stale_reminder_text(payload, false),
        "work_item_stale_reminder_skipped" => work_item_stale_reminder_text(payload, true),
        "missing_current_work_item_before_wait" => simple_event_text(
            "Missing current work item before wait",
            reason_or_work_item_body(payload),
        ),
        "work_item_delegation_created" => work_item_delegation_text(payload, false),
        "work_item_delegation_completed" => work_item_delegation_text(payload, true),
        "workspace_attach_requested" => {
            workspace_text(payload, fallback_summary, "Workspace attach requested")
        }
        "workspace_attached" => workspace_text(payload, fallback_summary, "Workspace attached"),
        "workspace_entered" => workspace_text(payload, fallback_summary, "Entered workspace"),
        "workspace_exit_requested" => {
            workspace_text(payload, fallback_summary, "Workspace exit requested")
        }
        "workspace_exited" => workspace_text(payload, fallback_summary, "Exited workspace"),
        "workspace_detach_requested" => {
            workspace_text(payload, fallback_summary, "Workspace detach requested")
        }
        "workspace_detached" => workspace_text(payload, fallback_summary, "Detached workspace"),
        "workspace_used" => workspace_text(payload, fallback_summary, "Workspace used"),
        "worktree_entered" => worktree_text(payload, fallback_summary, true),
        "worktree_exited" => worktree_text(payload, fallback_summary, false),
        "worktree_created_for_task" => {
            worktree_event_text(payload, "Worktree created for task", fallback_summary)
        }
        "task_worktree_metadata_recorded" => {
            worktree_event_text(payload, "Task worktree recorded", fallback_summary)
        }
        "worktree_retained_for_review" => {
            worktree_event_text(payload, "Worktree retained for review", fallback_summary)
        }
        "worktree_auto_cleaned_up" => {
            worktree_event_text(payload, "Worktree cleaned up", fallback_summary)
        }
        "worktree_auto_cleanup_failed" => {
            worktree_event_text(payload, "Worktree cleanup failed", fallback_summary)
        }
        "task_worktree_cleanup_already_removed" => {
            worktree_event_text(payload, "Task worktree already removed", fallback_summary)
        }
        "task_worktree_cleanup_retained" => {
            worktree_event_text(payload, "Task worktree retained", fallback_summary)
        }
        "task_worktree_cleanup_failed" => {
            worktree_event_text(payload, "Task worktree cleanup failed", fallback_summary)
        }
        "task_worktree_branch_cleanup_retained" => {
            worktree_event_text(payload, "Task worktree branch retained", fallback_summary)
        }
        "skill_activated" => skill_text(payload, "Loaded skill"),
        "skill_installed" => skill_text(payload, "Installed skill"),
        "skill_uninstalled" => skill_text(payload, "Uninstalled skill"),
        "agent_created" => simple_event_text("Agent created", agent_body(payload)),
        "agent_model_override_requested" => {
            simple_event_text("Model override requested", model_body(payload))
        }
        "agent_model_override_set" => simple_event_text("Model override set", model_body(payload)),
        "agent_model_override_clear_requested" => {
            simple_event_text("Model override clear requested", agent_body(payload))
        }
        "agent_model_override_cleared" => {
            simple_event_text("Model override cleared", agent_body(payload))
        }
        "agent_state_changed" | "state_changed" => {
            simple_event_text("Agent state updated", state_body(payload))
        }
        "session_state_changed" => simple_event_text("Session state updated", state_body(payload)),
        "control_request_admitted" => {
            simple_event_text("Control request admitted", control_body(payload))
        }
        "control_applied" => simple_event_text("Control applied", control_body(payload)),
        "current_run_aborted" => {
            simple_event_text("Current run aborted", reason_or_message_body(payload))
        }
        "wake_requested" => simple_event_text("Wake requested", reason_or_message_body(payload)),
        "continuation_trigger_received" => {
            simple_event_text("Continuation trigger received", continuation_body(payload))
        }
        "continuation_resolved" => {
            simple_event_text("Continuation resolved", continuation_body(payload))
        }
        "closure_decided" => simple_event_text("Closure updated", closure_body(payload)),
        "scheduler_decision" => simple_event_text("Scheduler decision", scheduler_body(payload)),
        "system_tick_emitted" => {
            simple_event_text("System tick emitted", reason_or_message_body(payload))
        }
        "system_tick_suppressed" => {
            simple_event_text("System tick suppressed", reason_or_message_body(payload))
        }
        "runtime_service_shutdown_requested" => simple_event_text(
            "Runtime shutdown requested",
            reason_or_message_body(payload),
        ),
        "debug_prompt_requested" => {
            simple_event_text("Debug prompt requested", reason_or_message_body(payload))
        }
        "turn_context_built" => simple_event_text("Turn context built", context_body(payload)),
        "turn_context_length_exceeded" => {
            simple_event_text("Context length exceeded", context_body(payload))
        }
        "runtime_error" => runtime_error_text(payload),
        "deferred_to_fallback" | "provider_failed_needs_recovery" => {
            provider_lineage_failure_text(kind, payload)
        }
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
        "episode_memory_finalized" => {
            simple_event_text("Episode memory finalized", memory_body(payload))
        }
        "working_memory_updated" => {
            simple_event_text("Working memory updated", memory_body(payload))
        }
        "recovery_cleared_missing_worktree_session" => {
            simple_event_text("Recovered missing worktree session", worktree_body(payload))
        }
        "operator_delivery_submitted" => {
            simple_event_text("Operator delivery submitted", delivery_body(payload))
        }
        "operator_delivery_completed" => {
            simple_event_text("Operator delivery completed", delivery_body(payload))
        }
        "operator_notification_mirror_failed" => simple_event_text(
            "Operator notification mirror failed",
            error_or_delivery_body(payload),
        ),
        "operator_transport_binding_upserted" => {
            simple_event_text("Operator transport binding updated", delivery_body(payload))
        }
        "process_execution_requested" => process_execution_text(payload, fallback_summary),
        "tool_executed" | "tool_execution_failed" => tool_text(kind, payload, fallback_summary),
        "truncated_mutation_tool_call_rejected" => simple_event_text(
            "Mutation tool call rejected",
            error_or_message_body(payload),
        ),
        _ => {
            let title = category_title(category, kind);
            (title, None, fallback_summary.to_string())
        }
    }
}

fn simple_event_text(
    title: &'static str,
    body: Option<String>,
) -> (String, Option<String>, String) {
    let summary = body
        .as_deref()
        .map(|body| format!("{title}: {body}"))
        .unwrap_or_else(|| title.to_string());
    (title.into(), body, summary)
}

fn brief_text(payload: &Value) -> (String, Option<String>, String) {
    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .map(collapse_whitespace)
        .filter(|text| !text.is_empty())
        .map(|text| trim_summary(&text));
    simple_event_text("Brief", text)
}

fn string_field(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn nested_string_field(payload: &Value, object: &str, field: &str) -> Option<String> {
    payload
        .get(object)
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn numeric_field(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
}

fn first_string_field(payload: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| string_field(payload, field))
}

fn message_id_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["message_id", "id"]).map(|id| format!("message {id}"))
}

fn ack_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["summary"]).or_else(|| message_id_body(payload))
}

fn message_body_preview(payload: &Value) -> Option<String> {
    payload
        .get("body")
        .and_then(|body| {
            body.get("text")
                .or_else(|| body.get("message"))
                .or_else(|| body.get("summary"))
        })
        .and_then(Value::as_str)
        .map(collapse_whitespace)
        .filter(|text| !text.is_empty())
        .map(|text| trim_summary(&text))
        .or_else(|| message_id_body(payload))
}

fn task_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["task_id", "id"])
        .map(|id| format!("task {id}"))
        .or_else(|| first_string_field(payload, &["summary", "task_kind", "kind"]))
}

fn work_item_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["work_item_id", "current_work_item_id", "id"])
        .map(|id| format!("work item {id}"))
        .or_else(|| {
            payload
                .get("record")
                .and_then(|record| record.get("objective"))
                .and_then(Value::as_str)
                .map(trim_summary)
        })
}

fn queued_redispatch_stalled_text(payload: &Value) -> (String, Option<String>, String) {
    let work_item = string_field(payload, "work_item_id")
        .map(|id| format!("Work item {id}"))
        .unwrap_or_else(|| "A queued work item".into());
    let attempts = payload
        .get("no_progress_attempts")
        .and_then(Value::as_u64)
        .map(|attempts| format!(" after {attempts} redispatch attempts without progress"))
        .unwrap_or_default();
    simple_event_text(
        "Queued work stalled",
        Some(format!(
            "{work_item} is stalled{attempts}. It will wait for new external input or a WorkItem update."
        )),
    )
}

fn timer_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["timer_id", "id", "summary"])
        .map(|value| format!("timer {value}"))
}

fn worktree_body(payload: &Value) -> Option<String> {
    first_string_field(
        payload,
        &[
            "worktree_branch",
            "branch",
            "worktree_path",
            "path",
            "task_id",
        ],
    )
    .or_else(|| nested_string_field(payload, "worktree", "worktree_branch"))
    .or_else(|| nested_string_field(payload, "worktree", "worktree_path"))
}

fn error_or_message_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["error", "message", "summary", "reason"]).map(|text| {
        let text = collapse_whitespace(&text);
        trim_summary(&text)
    })
}

fn reason_or_message_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["reason", "message", "summary"]).map(|text| {
        let text = collapse_whitespace(&text);
        trim_summary(&text)
    })
}

fn reason_or_work_item_body(payload: &Value) -> Option<String> {
    reason_or_message_body(payload).or_else(|| work_item_body(payload))
}

fn error_or_task_body(payload: &Value) -> Option<String> {
    error_or_message_body(payload).or_else(|| task_body(payload))
}

fn error_or_timer_body(payload: &Value) -> Option<String> {
    error_or_message_body(payload).or_else(|| timer_body(payload))
}

fn error_or_delivery_body(payload: &Value) -> Option<String> {
    error_or_message_body(payload).or_else(|| delivery_body(payload))
}

fn turn_body(payload: &Value) -> Option<String> {
    let turn = numeric_field(payload, "turn_index").map(|turn| format!("turn {turn}"));
    let run = string_field(payload, "run_id").map(|run| format!("run {run}"));
    match (turn, run) {
        (Some(turn), Some(run)) => Some(format!("{turn}, {run}")),
        (Some(turn), None) => Some(turn),
        (None, Some(run)) => Some(run),
        (None, None) => message_id_body(payload),
    }
}

fn agent_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["agent_id", "id", "agent"]).map(|id| format!("agent {id}"))
}

fn model_body(payload: &Value) -> Option<String> {
    first_string_field(
        payload,
        &[
            "model",
            "override_model",
            "requested_model",
            "active_model",
            "effective_model",
        ],
    )
    .map(|model| format!("model {model}"))
    .or_else(|| agent_body(payload))
}

fn state_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["status", "state", "agent_id", "session_id"])
}

fn control_body(payload: &Value) -> Option<String> {
    first_string_field(
        payload,
        &["action", "control", "request", "reason", "message"],
    )
}

fn continuation_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["kind", "reason", "trigger", "status"])
        .or_else(|| numeric_field(payload, "round").map(|round| format!("round {round}")))
}

fn closure_body(payload: &Value) -> Option<String> {
    payload
        .get("closure")
        .and_then(|closure| {
            closure
                .get("outcome")
                .or_else(|| closure.get("runtime_posture"))
                .or_else(|| closure.get("waiting_reason"))
        })
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| first_string_field(payload, &["outcome", "runtime_posture", "waiting_reason"]))
}

fn scheduler_body(payload: &Value) -> Option<String> {
    let decision = string_field(payload, "decision");
    let reason = string_field(payload, "reason");
    let work_item = string_field(payload, "work_item_id").map(|id| format!("work {id}"));
    let task = string_field(payload, "task_id").map(|id| format!("task {id}"));
    let mut parts = Vec::new();
    if let Some(decision) = decision {
        parts.push(decision);
    }
    if let Some(reason) = reason {
        parts.push(reason);
    }
    if let Some(work_item) = work_item {
        parts.push(work_item);
    }
    if let Some(task) = task {
        parts.push(task);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn context_body(payload: &Value) -> Option<String> {
    let tokens = numeric_field(payload, "estimated_tokens")
        .or_else(|| numeric_field(payload, "token_count"))
        .or_else(|| numeric_field(payload, "context_tokens"));
    let reason = first_string_field(payload, &["reason", "mode", "checkpoint_mode"]);
    match (tokens, reason) {
        (Some(tokens), Some(reason)) => Some(format!("{tokens} tokens; {reason}")),
        (Some(tokens), None) => Some(format!("{tokens} tokens")),
        (None, Some(reason)) => Some(reason),
        (None, None) => numeric_field(payload, "round").map(|round| format!("round {round}")),
    }
}

fn memory_body(payload: &Value) -> Option<String> {
    first_string_field(payload, &["summary", "memory_id", "work_item_id", "reason"])
}

fn delivery_body(payload: &Value) -> Option<String> {
    first_string_field(
        payload,
        &["target", "route", "transport", "status", "binding_id"],
    )
}

fn skill_text(payload: &Value, title: &'static str) -> (String, Option<String>, String) {
    let body = first_string_field(payload, &["name", "skill_name", "skill", "path", "scope"]);
    simple_event_text(title, body)
}

fn task_record_text(
    kind: &str,
    payload: &Value,
    _fallback_summary: &str,
) -> (String, Option<String>, String) {
    let Some(task) = decode_value::<TaskRecord>(payload.clone()) else {
        let label = match kind {
            "task_created" => "Task queued",
            "task_status_updated" => "Task updated",
            "task_result_received" => "Task result received",
            _ => "Task updated",
        };
        let body = task_body(payload);
        let summary = body
            .as_deref()
            .map(|body| format!("{label}: {body}"))
            .unwrap_or_else(|| label.to_string());
        return (label.into(), body, summary);
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
    _fallback_summary: &str,
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
        (None, None) => "Command task runner failed".into(),
    };
    ("Command task runner failed".into(), body, summary)
}

fn command_task_result_enqueue_failed_text(
    payload: &Value,
    _fallback_summary: &str,
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
        (None, None) => "Command task result enqueue failed".into(),
    };
    ("Command task result enqueue failed".into(), body, summary)
}

fn operator_notification_text(
    payload: &Value,
    _fallback_summary: &str,
) -> (String, Option<String>, String) {
    let summary = payload
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .map(trim_summary)
        .unwrap_or_else(|| "Operator attention requested".into());
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

fn callback_delivered_text(
    payload: &Value,
    _fallback_summary: &str,
) -> (String, Option<String>, String) {
    let source = payload.get("source").and_then(Value::as_str);
    let resource = payload.get("resource").and_then(Value::as_str);
    let triggered = callback_disposition_is_triggered(payload);
    let summary = match (source, resource) {
        (Some(source), Some(resource)) if triggered => {
            format!("External event received from {source} for {resource}; resuming agent")
        }
        (Some(source), None) if triggered => {
            format!("External event received from {source}; resuming agent")
        }
        (None, Some(resource)) if triggered => {
            format!("External event received for {resource}; resuming agent")
        }
        (None, None) if triggered => "External event received; resuming agent".into(),
        (Some(source), _) => format!("External event received from {source}"),
        (None, _) => "External event received".into(),
    };
    (
        "External event received".into(),
        source.map(str::to_string),
        summary,
    )
}

fn timer_text(
    payload: &Value,
    _fallback_summary: &str,
    fired: bool,
) -> (String, Option<String>, String) {
    let Some(timer) = decode_value::<TimerRecord>(payload.clone()) else {
        let title = if fired {
            "Timer fired"
        } else {
            "Timer scheduled"
        };
        return (title.into(), None, title.into());
    };
    let body = timer.content.instruction.clone();
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

fn work_item_text(payload: &Value, _fallback_summary: &str) -> (String, Option<String>, String) {
    if let Some(record) = payload
        .get("record")
        .cloned()
        .and_then(decode_value::<WorkItemRecord>)
    {
        return work_item_record_text(
            &record.objective,
            record.result_summary.as_deref(),
            &record.state,
        );
    }
    if let Some(record) = decode_value::<WorkItemLifecycleAuditEvent>(payload.clone()) {
        return work_item_record_text(
            &record.objective_preview,
            record.result_summary_preview.as_deref(),
            &record.state,
        );
    }
    ("Work item updated".into(), None, "Work item updated".into())
}

fn work_item_record_text(
    objective: &str,
    result_summary: Option<&str>,
    state: &WorkItemState,
) -> (String, Option<String>, String) {
    let objective = trim_summary(objective);
    let state_label = format!("{:?}", state);
    let title = if *state == WorkItemState::Completed {
        "Work completed"
    } else {
        "Work item updated"
    };
    let summary = if *state == WorkItemState::Completed {
        result_summary
            .map(trim_summary)
            .map(|result| format!("Work completed: {result}"))
            .unwrap_or_else(|| format!("Work completed: {objective}"))
    } else {
        format!("Work item {state_label}: {objective}")
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
    _fallback_summary: &str,
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
        (Some(child), None) => format!("Delegated child {child} started"),
        (None, Some(task)) => format!("Delegated child started under supervision task {task}"),
        (None, None) => "Delegated child started".into(),
    };
    (
        "Delegated child started".into(),
        child_agent_id.map(ToString::to_string),
        summary,
    )
}

fn task_input_delivered_text(
    payload: &Value,
    _fallback_summary: &str,
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
        task_id
            .map(|task_id| format!("Task input delivered: {task_id}"))
            .unwrap_or_else(|| "Task input delivered".into()),
    )
}

fn workspace_text(
    payload: &Value,
    _fallback_summary: &str,
    label: &'static str,
) -> (String, Option<String>, String) {
    let body = first_string_field(
        payload,
        &[
            "execution_root",
            "canonical_root",
            "workspace_anchor",
            "workspace_path",
            "cwd",
            "workspace_id",
        ],
    );
    if let Some(body) = body {
        return (label.into(), Some(body.clone()), format!("{label}: {body}"));
    }
    (label.into(), None, label.into())
}

fn worktree_text(
    payload: &Value,
    _fallback_summary: &str,
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
    (label.into(), None, label.into())
}

fn worktree_event_text(
    payload: &Value,
    label: &'static str,
    _fallback_summary: &str,
) -> (String, Option<String>, String) {
    simple_event_text(label, worktree_body(payload))
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
    fallback_summary: &str,
) -> (String, Option<String>, String) {
    let surface = payload
        .get("surface")
        .and_then(Value::as_str)
        .unwrap_or("process");
    let cmd_preview = payload
        .get("cmd_display")
        .and_then(Value::as_str)
        .or_else(|| payload.get("exec_command_display").and_then(Value::as_str))
        .or_else(|| payload.get("cmd_preview").and_then(Value::as_str))
        .or_else(|| {
            payload
                .get("command_cost")
                .and_then(|value| value.get("cmd_preview"))
                .and_then(Value::as_str)
        })
        .map(ToString::to_string)
        .filter(|cmd| !cmd.is_empty());
    let label = match surface {
        "ExecCommand" | "ExecCommandBatch" => "Command started",
        "command_task" => "Background command started",
        _ => "Process execution requested",
    };
    if let Some(cmd_preview) = cmd_preview {
        return (
            label.into(),
            Some(cmd_preview.clone()),
            format!("{label}: {cmd_preview}"),
        );
    }
    if let Some((legacy_label, legacy_cmd)) = legacy_command_summary(fallback_summary) {
        return (
            legacy_label.to_string(),
            Some(legacy_cmd.to_string()),
            fallback_summary.to_string(),
        );
    }
    (label.into(), None, label.into())
}

fn legacy_command_summary(summary: &str) -> Option<(&str, &str)> {
    [
        "Command started:",
        "Command finished:",
        "Command failed:",
        "Command batch started:",
        "Command batch finished:",
        "Command batch failed:",
    ]
    .iter()
    .find_map(|prefix| {
        summary
            .strip_prefix(prefix)
            .map(str::trim)
            .filter(|cmd| !cmd.is_empty())
            .map(|cmd| (prefix.trim_end_matches(':'), cmd))
    })
}

fn assistant_round_recorded_text(payload: &Value) -> (String, Option<String>, String) {
    if is_runtime_checkpoint_round(payload) {
        let mode = payload
            .get("checkpoint_mode")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let body = format!("Internal continuity evidence (mode={mode})");
        return (
            "Runtime checkpoint".into(),
            Some(body.clone()),
            format!("Runtime checkpoint: {body}"),
        );
    }
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
        return (
            "Assistant requested tools".into(),
            Some(tools.clone()),
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

fn provider_lineage_failure_text(kind: &str, payload: &Value) -> (String, Option<String>, String) {
    let title = match kind {
        "provider_failed_needs_recovery" => "Provider failed; recovery queued",
        _ => "Provider failed; fallback queued",
    };
    let body = payload
        .get("operator_message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("error").and_then(Value::as_str))
        .map(collapse_whitespace)
        .filter(|message| !message.is_empty())
        .map(|message| trim_summary(&message))
        .or_else(|| {
            payload
                .get("fallback_model_ref")
                .and_then(Value::as_str)
                .map(|fallback| format!("Queued fallback turn on {fallback}."))
        });
    let summary = body
        .as_deref()
        .map(|body| format!("{title}: {body}"))
        .unwrap_or_else(|| title.to_string());
    (title.into(), body, summary)
}

fn runtime_error_text(payload: &Value) -> (String, Option<String>, String) {
    let body = error_or_message_body(payload);
    simple_event_text("Runtime error", body)
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
    _fallback_summary: &str,
) -> (String, Option<String>, String) {
    let title = if kind == "tool_execution_failed" {
        "Tool failed"
    } else {
        "Tool finished"
    };
    let Some(tool_name) = payload.get("tool_name").and_then(Value::as_str) else {
        return (title.into(), None, title.into());
    };
    let failed = kind == "tool_execution_failed";
    let friendly = tool_friendly_label(tool_name, failed);
    if tool_name == "ExecCommand" {
        if let Some(cmd) = payload
            .get("exec_command_display")
            .and_then(Value::as_str)
            .or_else(|| payload.get("exec_command_cmd").and_then(Value::as_str))
        {
            let summary = if failed {
                format!("Command failed: {cmd}")
            } else {
                format!("Command finished: {cmd}")
            };
            return (friendly.into(), Some(cmd.to_string()), summary);
        }
    }
    if tool_name == "ExecCommandBatch" {
        if let Some(detail) = command_batch_detail(payload) {
            let summary = if failed {
                format!("Command batch failed: {detail}")
            } else {
                format!("Command batch finished: {detail}")
            };
            return (friendly.into(), Some(detail), summary);
        }
    }
    if let Some(detail) = tool_payload_detail(payload) {
        return (
            friendly.into(),
            Some(detail.clone()),
            format!("{friendly}: {detail}"),
        );
    }
    let summary = if tool_friendly_label_is_generic(tool_name) {
        format!("{friendly}: {tool_name}")
    } else {
        friendly.to_string()
    };
    (friendly.into(), Some(tool_name.to_string()), summary)
}

fn command_batch_detail(payload: &Value) -> Option<String> {
    let items = payload
        .get("exec_command_batch_items")
        .and_then(Value::as_array)?;
    let cmds = items
        .iter()
        .filter_map(|item| {
            item.get("cmd_display")
                .and_then(Value::as_str)
                .or_else(|| item.get("cmd").and_then(Value::as_str))
        })
        .map(|cmd| cmd.trim().to_string())
        .filter(|cmd| !cmd.is_empty())
        .take(2)
        .collect::<Vec<_>>();
    if cmds.is_empty() {
        return None;
    }
    let total = items.len();
    let mut detail = format!("{total} item");
    if total != 1 {
        detail.push('s');
    }
    detail.push_str(": ");
    detail.push_str(&cmds.join("; "));
    if total > cmds.len() {
        detail.push_str("; ...");
    }
    Some(trim_summary(&detail))
}

fn tool_payload_detail(payload: &Value) -> Option<String> {
    first_string_field(payload, &["error", "summary", "reason"])
        .map(|value| trim_summary(&collapse_whitespace(&value)))
        .filter(|value| !value.is_empty())
}

fn tool_friendly_label(tool_name: &str, failed: bool) -> &'static str {
    // Order doesn't matter: each tool name maps to a unique pair of labels.
    let (success, fail) = match tool_name {
        tn::APPLY_PATCH => ("Applied patch", "Patch failed"),
        tn::EXEC_COMMAND => ("Command finished", "Command failed"),
        tn::EXEC_COMMAND_BATCH => ("Command batch finished", "Command batch failed"),
        tn::LIST_WORK_ITEMS => ("Listed work items", "List work items failed"),
        tn::CREATE_WORK_ITEM => ("Created work item", "Create work item failed"),
        tn::UPDATE_WORK_ITEM => ("Updated work item", "Update work item failed"),
        tn::COMPLETE_WORK_ITEM => ("Completed work item", "Complete work item failed"),
        tn::LIST_TASKS => ("Listed tasks", "List tasks failed"),
        tn::TASK_LIST => ("Listed tasks", "List tasks failed"),
        tn::TASK_STATUS => ("Read task status", "Read task status failed"),
        tn::CREATE_AGENT => ("Created agent", "Create agent failed"),
        tn::INVOKE_AGENT => ("Invoked agent", "Invoke agent failed"),
        tn::ATTACH_WORKSPACE => ("Workspace attached", "Workspace attachment failed"),
        tn::DETACH_WORKSPACE => ("Workspace detached", "Workspace detachment failed"),
        tn::SWITCH_WORKSPACE => ("Workspace selected", "Workspace selection failed"),
        tn::CREATE_WORKTREE => ("Worktree created", "Worktree creation failed"),
        tn::REMOVE_WORKTREE => ("Worktree removed", "Worktree removal failed"),
        tn::USE_WORKSPACE => ("Workspace selected", "Workspace selection failed"),
        tn::SLEEP => ("Slept", "Sleep failed"),
        tn::WAIT_FOR => ("Waiting", "Wait failed"),
        "ReadSkill" => ("Read skill", "Read skill failed"),
        tn::WEB_FETCH => ("Fetched web page", "Fetch web page failed"),
        tn::WEB_SEARCH => ("Searched web", "Search web failed"),
        _ => ("Tool finished", "Tool failed"),
    };
    if failed {
        fail
    } else {
        success
    }
}

fn tool_friendly_label_is_generic(tool_name: &str) -> bool {
    // `ReadSkill` is not a builtin tool name constant — it is a dynamic
    // skill-read action.  Keep it as a literal here.
    const KNOWN_LABEL_TOOLS: &[&str] = &[
        tn::APPLY_PATCH,
        tn::EXEC_COMMAND,
        tn::EXEC_COMMAND_BATCH,
        tn::LIST_WORK_ITEMS,
        tn::CREATE_WORK_ITEM,
        tn::UPDATE_WORK_ITEM,
        tn::COMPLETE_WORK_ITEM,
        tn::LIST_TASKS,
        tn::TASK_LIST,
        tn::TASK_STATUS,
        tn::CREATE_AGENT,
        tn::INVOKE_AGENT,
        tn::ATTACH_WORKSPACE,
        tn::DETACH_WORKSPACE,
        tn::SWITCH_WORKSPACE,
        tn::CREATE_WORKTREE,
        tn::REMOVE_WORKTREE,
        tn::USE_WORKSPACE,
        tn::SLEEP,
        tn::WAIT_FOR,
        "ReadSkill",
        tn::WEB_FETCH,
        tn::WEB_SEARCH,
    ];
    !KNOWN_LABEL_TOOLS.contains(&tool_name)
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
        OperatorEventCategory::Skill => "Skill".into(),
        OperatorEventCategory::Configuration => "Configuration".into(),
        OperatorEventCategory::Control => "Control".into(),
        OperatorEventCategory::Context => "Context".into(),
        OperatorEventCategory::Delivery => "Operator delivery".into(),
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
mod tests;
