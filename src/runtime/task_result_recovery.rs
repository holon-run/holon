use super::*;
use crate::domain::execution_protocol::WorkItemExecutionState;
use crate::runtime_db::TaskResultSettlementDisposition;
use sha2::{Digest, Sha256};

const RESULT_RECHECK_SECONDS: i64 = 30;
const RESULT_RECHECK_LIMIT: usize = 8;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn recovery_owner_key(record: &crate::runtime_db::TaskResultSettlementRecord) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.task-result-recovery-owner.v1\0");
    hasher.update(record.agent_id.as_bytes());
    hasher.update([0]);
    hasher.update(record.work_item_id.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    hasher.update(record.rejoin_generation.to_be_bytes());
    hasher.update([0]);
    hasher.update(record.parent_turn_id.as_bytes());
    format!("recovery_owner_{}", hex(&hasher.finalize()[..16]))
}

fn recovery_message_id(owner_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.task-result-recovery-message.v1\0");
    hasher.update(owner_key.as_bytes());
    format!("msg_task_result_recovery_{}", hex(&hasher.finalize()[..16]))
}

impl RuntimeHandle {
    /// Rechecks are eligibility probes, not permission to consume a wait.
    pub(super) async fn emit_due_task_result_recovery(&self) -> Result<bool> {
        let state = self.agent_state().await?;
        if state.status == AgentStatus::Stopped {
            return Ok(false);
        }
        let now = self.now();
        let records = self
            .inner
            .runtime_db
            .task_result_settlements()
            .due_deferred(&state.id, now, RESULT_RECHECK_LIMIT)?;
        if records.is_empty() {
            return Ok(false);
        }
        let execution = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&state.id)?;
        let waits = self
            .inner
            .storage
            .active_wait_conditions_for_agent(&state.id)?;
        let mut queued_owners = std::collections::BTreeSet::new();
        let mut enqueued = false;
        for record in records {
            let mut reason = "awaiting_canonical_admission";
            let mut eligible = true;
            if execution
                .as_ref()
                .is_some_and(|execution| execution.open_attempt().is_some())
            {
                eligible = false;
                reason = "execution_lane_busy";
            }
            if let Some(work_item_id) = record.work_item_id.as_deref() {
                let owner = self.inner.runtime_db.work_items().latest(work_item_id)?;
                let unavailable = match owner.as_ref() {
                    None => Some(TaskResultSettlementDisposition::OwnerMissing),
                    Some(owner) if owner.state != WorkItemState::Open => {
                        Some(TaskResultSettlementDisposition::OwnerClosed)
                    }
                    Some(_) => None,
                };
                if let Some(disposition) = unavailable {
                    self.inner
                        .runtime_db
                        .task_result_settlements()
                        .settle_owner_unavailable(&record.message_id, disposition, now)?;
                    continue;
                }
                if owner.as_ref().is_some_and(|owner| !owner.is_runnable())
                    || execution.as_ref().is_some_and(|execution| {
                        execution.work_items.get(work_item_id).is_none_or(|work| {
                            !matches!(work.state, WorkItemExecutionState::Runnable { .. })
                        })
                    })
                {
                    eligible = false;
                    reason = "owner_not_runnable";
                }
            }
            if waits
                .iter()
                .any(|wait| wait.work_item_id == record.work_item_id)
            {
                eligible = false;
                reason = "owner_has_unresolved_wait";
            }
            // Advance first: a crash or enqueue failure still leaves a bounded
            // retry, while an ineligible owner cannot create a hot loop.
            self.inner
                .runtime_db
                .task_result_settlements()
                .mark_deferred(
                    &record.message_id,
                    reason,
                    now,
                    now + chrono::Duration::seconds(RESULT_RECHECK_SECONDS),
                )?;
            let owner_key = recovery_owner_key(&record);
            if !eligible || !queued_owners.insert(owner_key.clone()) {
                continue;
            }
            let mut message = MessageEnvelope::new(
                &state.id,
                MessageKind::InternalFollowup,
                MessageOrigin::System {
                    subsystem: "task_result_recovery".into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Background,
                MessageBody::Text {
                    text: "Handle the pending task results for this execution owner.".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::RuntimeSystem,
                AdmissionContext::RuntimeOwned,
            );
            message.id = recovery_message_id(&owner_key);
            message.work_item_id = record.work_item_id;
            message
                .source_refs
                .insert("task_result_owner_key".into(), owner_key);
            message.source_refs.insert(
                "task_result_rejoin_generation".into(),
                record.rejoin_generation.to_string(),
            );
            message.source_refs.insert(
                "task_result_parent_turn_id".into(),
                record.parent_turn_id.clone(),
            );
            message
                .source_refs
                .insert("task_result_message_id".into(), record.message_id);
            message
                .source_refs
                .insert("task_result_identity".into(), record.result_identity);
            self.enqueue(message).await?;
            enqueued = true;
        }
        Ok(enqueued)
    }
}
