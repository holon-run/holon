//! Recovery snapshot and active wait/task recovery helpers.

use crate::types::{
    AgentState, AuditEvent, ExternalWaitRecoverability, MessageEnvelope, TaskRecord, TimerRecord,
    WaitConditionRecord, WorkItemDelegationRecord, WorkItemRecord,
};

#[derive(Debug, Clone)]
pub struct RecoverySnapshot {
    pub agent: Option<AgentState>,
    pub replay_messages: Vec<MessageEnvelope>,
    pub active_tasks: Vec<TaskRecord>,
    pub active_timers: Vec<TimerRecord>,
    pub work_items: Vec<WorkItemRecord>,
    pub work_item_delegations: Vec<WorkItemDelegationRecord>,
}

pub(crate) fn external_wait_recoverability_event(
    record: &WaitConditionRecord,
) -> Option<AuditEvent> {
    match record.external_recoverability()? {
        ExternalWaitRecoverability::Weak => Some(AuditEvent::legacy(
            "external_wait_without_recovery",
            serde_json::json!({
                "wait_condition_id": record.id,
                "work_item_id": record.work_item_id,
                "source": record.source,
                "subject_ref": record.subject_ref,
                "waiting_for": record.waiting_for,
                "external_recoverability": "weak",
                "wake_sources": record.wake_sources,
            }),
        )),
        ExternalWaitRecoverability::ExplicitNoFallback => Some(AuditEvent::legacy(
            "external_wait_without_recovery",
            serde_json::json!({
                "wait_condition_id": record.id,
                "work_item_id": record.work_item_id,
                "source": record.source,
                "subject_ref": record.subject_ref,
                "waiting_for": record.waiting_for,
                "external_recoverability": "explicit_no_fallback",
                "no_fallback_reason": record.no_fallback_reason(),
                "wake_sources": record.wake_sources,
            }),
        )),
        ExternalWaitRecoverability::Recoverable => None,
    }
}
