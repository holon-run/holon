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
    format!("recovery_owner_{}", hex(&hasher.finalize()[..16]))
}

fn recovery_message_id() -> String {
    crate::ids::runtime_id("msg_task_result_recovery")
}

pub(super) fn outstanding_recovery_message_id(
    runtime: &RuntimeHandle,
    agent_id: &str,
    work_item_id: Option<&str>,
    owner_key: &str,
) -> Result<Option<String>> {
    let messages = runtime.inner.runtime_db.messages().all(Some(agent_id))?;
    let queue_entries = runtime.inner.runtime_db.queue_entries().latest_all()?;
    let queue_entries = queue_entries
        .into_iter()
        .map(|entry| (entry.message_id.clone(), entry))
        .collect::<std::collections::HashMap<_, _>>();

    Ok(messages.into_iter().rev().find_map(|message| {
        if !matches!(
            message.origin,
            MessageOrigin::System { ref subsystem } if subsystem == "task_result_recovery"
        ) || message.authority_class != AuthorityClass::RuntimeInstruction
            || message.work_item_id.as_deref() != work_item_id
            || message.source_refs.get("task_result_owner_key") != Some(&owner_key.to_owned())
        {
            return None;
        }
        let entry = queue_entries.get(&message.id)?;
        (!matches!(
            entry.status,
            QueueEntryStatus::Processed
                | QueueEntryStatus::Aborted
                | QueueEntryStatus::Dropped
                | QueueEntryStatus::Quarantined
        ))
        .then_some(message.id)
    }))
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
            if let Some(reply_id) = self
                .inner
                .storage
                .read_message_by_id(&record.message_id)?
                .as_ref()
                .and_then(crate::wake_contract::agent_message_reply_reference)
            {
                if self
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(reply_id)?
                    .is_some_and(|entry| entry.status == QueueEntryStatus::Processed)
                {
                    // Content was consumed by another execution owner. Preserve
                    // that distinct outcome instead of claiming caller admission.
                    self.inner
                        .runtime_db
                        .task_result_settlements()
                        .settle_reply_consumed_elsewhere(&record.message_id, now)?;
                    continue;
                }
                // The delivery ledger and original message own reply recovery.
                // A reference observation must not create a second model wake.
                eligible = false;
                reason = "awaiting_original_agent_reply_admission";
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
            if let Some(message_id) = outstanding_recovery_message_id(
                self,
                &state.id,
                record.work_item_id.as_deref(),
                &owner_key,
            )? {
                tracing::debug!(
                    agent_id = %state.id,
                    work_item_id = ?record.work_item_id,
                    owner_key = %owner_key,
                    message_id = %message_id,
                    "task-result recovery wake already outstanding"
                );
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
            message.id = recovery_message_id();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::context_config;
    use crate::runtime_db::task_result_settlement::TaskResultSettlementState;
    use crate::types::QueueEntryStatus;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn recovery_message(owner_key: &str) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "task_result_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Background,
            MessageBody::Text {
                text: "recovery".into(),
            },
        );
        message
            .source_refs
            .insert("task_result_owner_key".into(), owner_key.into());
        message
    }

    #[test]
    fn recovery_owner_key_ignores_result_fence_fields() {
        let mut record = crate::runtime_db::TaskResultSettlementRecord {
            result_identity: "result-1".into(),
            agent_id: "agent-a".into(),
            task_id: "task-1".into(),
            message_id: "message-1".into(),
            work_item_id: Some("work-a".into()),
            rejoin_generation: 1,
            parent_turn_id: "turn-1".into(),
            task_status: "completed".into(),
            state: TaskResultSettlementState::PersistedPending,
            activation_id: None,
            disposition: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            admitted_at: None,
            settled_at: None,
            deferred_reason: None,
            deferred_at: None,
            next_recheck_at: None,
        };
        let owner_key = recovery_owner_key(&record);
        record.rejoin_generation = 2;
        record.parent_turn_id = "turn-2".into();
        record.result_identity = "result-2".into();
        assert_eq!(owner_key, recovery_owner_key(&record));
    }

    #[tokio::test]
    async fn processed_recovery_wake_can_be_rearmed() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let owner_key = "recovery_owner_test";
        let first = recovery_message(owner_key);
        let first_id = first.id.clone();
        runtime.enqueue(first).await.unwrap();
        assert_eq!(
            outstanding_recovery_message_id(&runtime, "default", None, owner_key).unwrap(),
            Some(first_id.clone())
        );

        let mut entry = runtime
            .runtime_db()
            .queue_entries()
            .latest(&first_id)
            .unwrap()
            .unwrap();
        entry.status = QueueEntryStatus::Processed;
        entry.updated_at = Utc::now();
        runtime.runtime_db().queue_entries().upsert(&entry).unwrap();

        assert_eq!(
            outstanding_recovery_message_id(&runtime, "default", None, owner_key).unwrap(),
            None
        );
    }
    #[tokio::test(start_paused = true)]
    async fn consumed_reply_settles_other_owner_observations_without_rearming() {
        use crate::runtime::tests::support::LifecycleHarness;
        use crate::types::{TaskKind, TaskRecord, TaskStatus};
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let work = runtime
            .create_work_item("non-selected reply observer".into(), None, None, Vec::new())
            .await
            .unwrap();
        let reply = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "peer reply".into(),
            },
        );
        runtime.storage().append_message(&reply).unwrap();
        runtime
            .runtime_db()
            .queue_entries()
            .upsert(&QueueEntryRecord {
                message_id: reply.id.clone(),
                agent_id: "default".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Processed,
                created_at: harness.now(),
                updated_at: harness.now(),
            })
            .unwrap();
        let mut signal = MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "observer-task".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "reply received".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        signal.task_id = Some("observer-task".into());
        signal.work_item_id = Some(work.id.clone());
        signal.metadata = Some(
            serde_json::json!({"task_id":"observer-task", "task_kind":"agent_message_wait",
            "task_status":"completed", "task_detail":{"message_id":reply.id, "reply_content_source":"original_message"}}),
        );
        let task = TaskRecord {
            id: "observer-task".into(),
            agent_id: "default".into(),
            kind: TaskKind::AgentMessageWait,
            status: TaskStatus::Completed,
            created_at: harness.now(),
            updated_at: harness.now(),
            parent_message_id: Some(signal.id.clone()),
            work_item_id: Some(work.id),
            summary: None,
            detail: Some(
                serde_json::json!({"rejoin_obligation_id":"observer-task", "rejoin_generation":1, "parent_turn_id":"parent"}),
            ),
            recovery: None,
        };
        runtime.runtime_db().tasks().upsert(&task).unwrap();
        runtime.storage().append_message(&signal).unwrap();
        runtime
            .runtime_db()
            .task_result_settlements()
            .ensure_pending(
                &task,
                &signal,
                harness.now() - chrono::Duration::seconds(31),
            )
            .unwrap()
            .unwrap();
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        let settled = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&signal.id)
            .unwrap()
            .unwrap();
        assert_eq!(settled.state, TaskResultSettlementState::Settled);
        assert_eq!(
            settled.disposition,
            Some(TaskResultSettlementDisposition::ReplyConsumedElsewhere)
        );
        assert!(settled.next_recheck_at.is_none());
        harness.advance(std::time::Duration::from_secs(60)).await;
        assert!(!runtime.emit_due_task_result_recovery().await.unwrap());
        assert_eq!(
            runtime
                .runtime_db()
                .task_result_settlements()
                .latest_for_message(&signal.id)
                .unwrap()
                .unwrap()
                .updated_at,
            settled.updated_at
        );
    }
}
