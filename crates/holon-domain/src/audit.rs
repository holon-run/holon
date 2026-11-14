use super::brief::{BriefContentSource, BriefKind, BriefRecord};
use super::message::{
    AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageOrigin,
    Priority,
};
use super::model::{AgentModelSource, AgentModelState};
use super::task::{TaskKind, TaskRecord, TaskStatus};
use super::tool::ToolExecutionStatus;
use crate::agent_records::AgentMessageRoutingContext;
use crate::ids;
use crate::model_values::ModelRouteRef;
use crate::tool_error::ToolError;
use crate::work_item::{WorkItemPlanStatus, WorkItemReadiness, WorkItemRecord, WorkItemState};
use crate::{ContinuationTriggerKind, MessageKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageLifecycleAuditEvent {
    pub message_id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub kind: MessageKind,
    pub origin: MessageOrigin,
    pub authority_class: AuthorityClass,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_kind: Option<ContinuationTriggerKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_refs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_surface: Option<MessageDeliverySurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_context: Option<AdmissionContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_context: Option<AgentMessageRoutingContext>,
}

impl MessageLifecycleAuditEvent {
    pub fn from_message(message: &MessageEnvelope) -> Self {
        Self {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            message_seq: message.message_seq,
            turn_id: message.turn_id.clone(),
            kind: message.kind.clone(),
            origin: message.origin.clone(),
            authority_class: message.authority_class,
            priority: message.priority.clone(),
            trigger_kind: message.trigger_kind,
            work_item_id: message.work_item_id.clone(),
            task_id: message.task_id.clone(),
            source_refs: message.source_refs.clone(),
            delivery_surface: message.delivery_surface,
            admission_context: message.admission_context,
            correlation_id: message.correlation_id.clone(),
            causation_id: message.causation_id.clone(),
            routing_context: message.routing_context.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefCreatedAuditEvent {
    pub brief_id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub kind: BriefKind,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalizes_assistant_round_id: Option<String>,
    #[serde(default)]
    pub content_source: BriefContentSource,
    #[serde(default)]
    pub content_char_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_task_id: Option<String>,
}

impl BriefCreatedAuditEvent {
    pub fn from_brief(brief: &BriefRecord) -> Self {
        Self {
            brief_id: brief.id.clone(),
            agent_id: brief.agent_id.clone(),
            workspace_id: brief.workspace_id.clone(),
            work_item_id: brief.work_item_id.clone(),
            turn_index: brief.turn_index,
            turn_id: brief.turn_id.clone(),
            kind: brief.kind,
            created_at: brief.created_at,
            finalizes_assistant_round_id: brief.finalizes_assistant_round_id.clone(),
            content_source: brief.content_source.clone(),
            content_char_count: brief.text.chars().count(),
            related_message_id: brief.related_message_id.clone(),
            related_task_id: brief.related_task_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkItemLifecycleAuditEvent {
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub work_item_id: String,
    pub workspace_id: String,
    pub revision: u64,
    pub action: String,
    pub state: WorkItemState,
    pub plan_status: WorkItemPlanStatus,
    pub readiness: WorkItemReadiness,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub objective_preview: String,
    pub objective_len: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_brief_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_at: Option<DateTime<Utc>>,
}

impl WorkItemLifecycleAuditEvent {
    pub fn from_work_item(action: impl Into<String>, record: &WorkItemRecord) -> Self {
        Self::from_work_item_with_readiness(action, record, record.readiness())
    }

    pub fn from_work_item_with_readiness(
        action: impl Into<String>,
        record: &WorkItemRecord,
        readiness: WorkItemReadiness,
    ) -> Self {
        Self {
            agent_id: record.agent_id.clone(),
            work_item_id: record.id.clone(),
            workspace_id: record.workspace_id.clone(),
            revision: record.revision,
            action: action.into(),
            state: record.state.clone(),
            plan_status: record.plan_status,
            readiness,
            updated_at: record.updated_at,
            turn_id: record.turn_id.clone(),
            objective_preview: truncate_audit_preview(&record.objective, 600),
            objective_len: record.objective.chars().count(),
            result_brief_id: record.result_brief_id.clone(),
            result_summary_preview: record
                .result_summary
                .as_deref()
                .map(|value| truncate_audit_preview(value, 600)),
            blocked_by_preview: record
                .blocked_by
                .as_deref()
                .map(|value| truncate_audit_preview(value, 600)),
            recheck_at: record.recheck_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskLifecycleAuditEvent {
    pub task_id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_summary_preview: Option<String>,
}

impl TaskLifecycleAuditEvent {
    pub fn from_task(task: &TaskRecord) -> Self {
        let detail = task.detail.as_ref();
        Self {
            task_id: task.id.clone(),
            agent_id: task.agent_id.clone(),
            kind: task.kind.clone(),
            status: task.status.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
            parent_message_id: task.parent_message_id.clone(),
            work_item_id: task.work_item_id.clone(),
            summary: task.summary.clone(),
            task_status: detail_string(detail, "task_status"),
            exit_status: detail_i64(detail, "exit_status"),
            error: detail_string(detail, "error").map(|value| truncate_audit_preview(&value, 512)),
            output_path: detail_string(detail, "output_path"),
            output_summary_preview: detail_string(detail, "output_summary")
                .map(|value| truncate_audit_preview(&value, 512)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolExecutionAuditEvent {
    pub tool_call_id: String,
    pub tool_execution_id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub tool_name: String,
    pub turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub status: ToolExecutionStatus,
    pub duration_ms: u64,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_command_cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_command_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_command_batch_items: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_command_cost: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_command_disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_handle: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_error: Option<ToolError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentModelOverrideAuditEvent {
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub source: AgentModelSource,
    pub effective_model: ModelRouteRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_service_tier: Option<crate::model_values::ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_service_tier: Option<crate::model_values::ServiceTier>,
    #[serde(default)]
    pub fallback_active: bool,
    #[serde(default)]
    pub pending_next_turn: bool,
}

impl AgentModelOverrideAuditEvent {
    pub fn from_model_state(
        agent_id: impl Into<String>,
        model: &AgentModelState,
        pending_next_turn: bool,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            source: model.source.clone(),
            effective_model: model.effective_model.clone(),
            requested_model: model.requested_model.clone(),
            active_model: model.active_model.clone(),
            override_model: model.override_model.clone(),
            override_reasoning_effort: model.override_reasoning_effort.clone(),
            override_service_tier: model.override_service_tier,
            effective_service_tier: model.effective_service_tier,
            fallback_active: model.fallback_active,
            pending_next_turn,
        }
    }
}

/// Public diagnostic event for scheduler decisions and invariant violations.
///
/// Extends scheduler observability from internal audit-only records to a typed,
/// publicly addressable event stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerDiagnosticAuditEvent {
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub decision: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
}

fn detail_string(detail: Option<&Value>, field: &str) -> Option<String> {
    detail
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn detail_i64(detail: Option<&Value>, field: &str) -> Option<i64> {
    detail
        .and_then(|value| value.get(field))
        .and_then(Value::as_i64)
}

pub fn truncate_audit_preview(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut preview = value.chars().take(max_chars).collect::<String>();
    preview.push_str("...");
    preview
}

/// Deterministic idempotency identity for a brief's `brief_created` audit
/// event. Retry of a Brief publication must reuse the same event id (and
/// brief-sourced `created_at`) so the transactional append sees one event,
/// not one per attempt.
pub fn stable_brief_created_event_id(agent_id: &str, brief_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update("brief_created".as_bytes());
    hasher.update([0]);
    hasher.update(agent_id.as_bytes());
    hasher.update([0]);
    hasher.update(brief_id.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("event_{}", &digest[..15])
}

/// Builds the deterministic `brief_created` audit event for a Brief record.
/// The event id and `created_at` are derived from the Brief so retries are
/// content-identical and deduplicate at the storage layer.
pub fn brief_created_event_for(brief: &BriefRecord) -> anyhow::Result<AuditEvent> {
    let mut event = AuditEvent::typed(
        crate::runtime_event::RuntimeEventKind::BriefCreated,
        &BriefCreatedAuditEvent::from_brief(brief),
    )?;
    event.id = stable_brief_created_event_id(&brief.agent_id, &brief.id);
    event.created_at = brief.created_at;
    Ok(event)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: String,
    #[serde(default)]
    pub event_seq: u64,
    #[serde(default)]
    pub event_log_epoch: String,
    pub created_at: DateTime<Utc>,
    pub kind: String,
    #[serde(default = "crate::runtime_event::legacy_contract_version")]
    pub contract_version: u32,
    #[serde(default = "crate::runtime_event::legacy_payload_schema")]
    pub payload_schema: String,
    #[serde(default = "crate::runtime_event::legacy_payload_schema_version")]
    pub payload_schema_version: u32,
    pub data: Value,
}

impl AuditEvent {
    pub fn legacy(kind: impl Into<String>, data: Value) -> Self {
        Self {
            id: ids::audit_event_id(),
            event_seq: 0,
            event_log_epoch: String::new(),
            created_at: Utc::now(),
            kind: kind.into(),
            contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
            payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.to_string(),
            payload_schema_version: 1,
            data,
        }
    }

    pub fn typed<P>(
        kind: crate::runtime_event::RuntimeEventKind,
        payload: &P,
    ) -> anyhow::Result<Self>
    where
        P: crate::runtime_event::RuntimeEventPayload,
    {
        let descriptor = kind.descriptor();
        anyhow::ensure!(
            descriptor.payload_schema == P::SCHEMA_ID
                && descriptor.payload_schema_version == P::SCHEMA_VERSION,
            "runtime event kind {} requires payload schema {}@{}, got {}@{}",
            descriptor.wire_name,
            descriptor.payload_schema,
            descriptor.payload_schema_version,
            P::SCHEMA_ID,
            P::SCHEMA_VERSION
        );
        Ok(Self {
            id: ids::audit_event_id(),
            event_seq: 0,
            event_log_epoch: String::new(),
            created_at: Utc::now(),
            kind: descriptor.wire_name.to_string(),
            contract_version: crate::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION,
            payload_schema: descriptor.payload_schema.to_string(),
            payload_schema_version: descriptor.payload_schema_version,
            data: serde_json::to_value(payload)?,
        })
    }
}
