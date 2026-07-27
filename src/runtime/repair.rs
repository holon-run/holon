use super::*;
use anyhow::bail;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SchedulerRepairInspection {
    pub agent_id: String,
    pub active_waits: Vec<crate::types::WaitConditionRecord>,
    pub wake_only_queue_entries: Vec<QueueEntryRecord>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchedulerRepairOperation {
    CancelWait {
        wait_id: String,
        expected_status: crate::types::WaitConditionStatus,
        expected_updated_at: chrono::DateTime<Utc>,
    },
    DropWakeOnlyQueueEntry {
        message_id: String,
        expected_status: QueueEntryStatus,
        expected_updated_at: chrono::DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchedulerRepairRequest {
    #[serde(default)]
    pub dry_run: bool,
    pub reason: String,
    pub operation: SchedulerRepairOperation,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SchedulerRepairResult {
    pub agent_id: String,
    pub dry_run: bool,
    pub changed: bool,
    pub operation: &'static str,
    pub before: Value,
    pub after: Value,
}

impl RuntimeHandle {
    pub async fn inspect_scheduler_repair(&self) -> Result<SchedulerRepairInspection> {
        let agent_id = self.agent_id().await?;
        let active_waits = self
            .inner
            .storage
            .raw_active_wait_conditions_for_agent(&agent_id)?;
        let mut wake_only_queue_entries = Vec::new();
        for entry in self.inner.storage.latest_queue_entries()? {
            if entry.agent_id != agent_id
                || !matches!(
                    entry.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                )
            {
                continue;
            }
            if self
                .inner
                .storage
                .read_message_by_id(&entry.message_id)?
                .as_ref()
                .is_some_and(is_wake_only_message)
            {
                wake_only_queue_entries.push(entry);
            }
        }
        Ok(SchedulerRepairInspection {
            agent_id,
            active_waits,
            wake_only_queue_entries,
        })
    }

    pub async fn apply_scheduler_repair(
        &self,
        request: SchedulerRepairRequest,
    ) -> Result<SchedulerRepairResult> {
        let reason = request.reason.trim();
        if reason.is_empty() {
            bail!("scheduler repair reason may not be empty");
        }
        let agent_id = self.agent_id().await?;
        match request.operation {
            SchedulerRepairOperation::CancelWait {
                wait_id,
                expected_status,
                expected_updated_at,
            } => {
                let current = self
                    .inner
                    .storage
                    .latest_wait_conditions()?
                    .into_iter()
                    .find(|wait| wait.id == wait_id && wait.agent_id == agent_id)
                    .ok_or_else(|| anyhow!("wait condition {wait_id} not found"))?;
                if current.status != expected_status || current.updated_at != expected_updated_at {
                    bail!("wait condition {wait_id} changed before scheduler repair");
                }
                if current.status != crate::types::WaitConditionStatus::Active {
                    bail!("scheduler repair can only cancel an active wait");
                }
                let mut cancelled = current.clone();
                cancelled.status = crate::types::WaitConditionStatus::Cancelled;
                cancelled.updated_at = self.now();
                cancelled.cancelled_at = Some(cancelled.updated_at);
                let before = serde_json::to_value(&current)?;
                let after = serde_json::to_value(&cancelled)?;
                if !request.dry_run {
                    let commit = self.inner.runtime_db.transitions().commit_wait(
                        &crate::runtime_db::transitions::WaitTransitionCommand {
                            agent_id: agent_id.clone(),
                            work_items: Vec::new(),
                            expected_wait_conditions: vec![
                                crate::runtime_db::transitions::WaitConditionExpectation {
                                    id: current.id.clone(),
                                    agent_id: current.agent_id.clone(),
                                    status: current.status.clone(),
                                    updated_at: current.updated_at,
                                },
                            ],
                            wait_conditions: vec![cancelled],
                            agent_state: None,
                            audit_events: vec![AuditEvent::legacy(
                                "scheduler_wait_repaired",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "wait_id": wait_id,
                                    "action": "cancel",
                                    "reason": reason,
                                }),
                            )],
                            index_changes: Vec::new(),
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                        },
                    )?;
                    self.apply_transition_commit(commit).await;
                }
                Ok(SchedulerRepairResult {
                    agent_id,
                    dry_run: request.dry_run,
                    changed: true,
                    operation: "cancel_wait",
                    before,
                    after,
                })
            }
            SchedulerRepairOperation::DropWakeOnlyQueueEntry {
                message_id,
                expected_status,
                expected_updated_at,
            } => {
                let current = self
                    .inner
                    .storage
                    .latest_queue_entries()?
                    .into_iter()
                    .find(|entry| entry.message_id == message_id && entry.agent_id == agent_id)
                    .ok_or_else(|| anyhow!("queue entry {message_id} not found"))?;
                if current.status != expected_status || current.updated_at != expected_updated_at {
                    bail!("queue entry {message_id} changed before scheduler repair");
                }
                if !matches!(
                    current.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                ) {
                    bail!("scheduler repair can only drop queued or interrupted entries");
                }
                let message = self
                    .inner
                    .storage
                    .read_message_by_id(&message_id)?
                    .ok_or_else(|| anyhow!("message {message_id} not found"))?;
                if !is_wake_only_message(&message) {
                    bail!("scheduler repair refuses to drop a non-wake-only message");
                }
                let mut dropped = current.clone();
                dropped.status = QueueEntryStatus::Dropped;
                dropped.updated_at = self.now();
                let before = serde_json::to_value(&current)?;
                let after = serde_json::to_value(&dropped)?;
                if !request.dry_run {
                    let commit = self.inner.runtime_db.transitions().commit_queue(
                        &crate::runtime_db::transitions::QueueTransitionCommand {
                            agent_id: agent_id.clone(),
                            operation: crate::runtime_db::transitions::QueueOperation::RepairDrop,
                            mutation:
                                crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                    expected: current,
                                    record: dropped,
                                },
                            scheduler_claim_work_item: None,
                            scheduler_protocol_bootstrap: None,
                            scheduler_protocol_commands: Vec::new(),
                            scheduler_authority_scenarios: Vec::new(),
                            scheduler_rollout_expectations: Vec::new(),
                            agent_state: None,
                            message_evidence: Vec::new(),
                            transcript_entries: Vec::new(),
                            turn_record: None,
                            audit_events: vec![AuditEvent::legacy(
                                "scheduler_queue_entry_repaired",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "message_id": message_id,
                                    "action": "drop_wake_only",
                                    "reason": reason,
                                }),
                            )],
                            scheduler_shadow_comparison: None,
                            scheduler_wait_resume_shadow_comparison: None,
                            scheduler_delivery_shadow_comparison: None,
                            scheduler_semantic_shadow: None,
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        },
                    )?;
                    self.apply_transition_commit(commit).await;
                }
                Ok(SchedulerRepairResult {
                    agent_id,
                    dry_run: request.dry_run,
                    changed: true,
                    operation: "drop_wake_only_queue_entry",
                    before,
                    after,
                })
            }
        }
    }
}

fn is_wake_only_message(message: &MessageEnvelope) -> bool {
    matches!(
        (&message.kind, &message.origin),
        (MessageKind::SystemTick, MessageOrigin::System { subsystem })
            if subsystem == "wake_hint"
    )
}
