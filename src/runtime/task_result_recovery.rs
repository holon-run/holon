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

mod read;
#[cfg(test)]
pub(super) use read::due_results as due_recovery_results;
pub(super) use read::prepare_outstanding_recovery_message_id;

impl RuntimeHandle {
    /// Rechecks are eligibility probes, not permission to consume a wait.
    pub(super) async fn emit_due_task_result_recovery(&self) -> Result<bool> {
        let now = self.now();
        let read = {
            let mut mutation = self.begin_mutation().await?;
            mutation.prepare_task_recovery_read(self, now).await?
        };
        let Some(read) = read else {
            return Ok(false);
        };
        let state = &read.state;
        let mut queued_owners = std::collections::BTreeSet::new();
        let mut enqueued = false;
        for metadata in read::due_results(&read.tasks, now)?.into_iter().flatten() {
            let record = read.record(metadata)?;
            if read.invalid_result(&record.result_identity)? {
                self.apply_task_result_operation((&record.message_id, "invalid_only", now), || {
                    crate::runtime_db::transitions::TaskResultOperation::SettleInvalidOnly {
                        message_id: record.message_id.clone(),
                        at: now,
                    }
                })
                .await?
                .changed()?;
                continue;
            }
            let mut reason = "awaiting_canonical_admission";
            let mut eligible = true;
            if read.execution_lane_busy()? {
                eligible = false;
                reason = "execution_lane_busy";
            }
            if let Some(work_item_id) = record.work_item_id.as_deref() {
                let owner = read.work.work_item(work_item_id)?;
                let unavailable = match owner {
                    None => Some(TaskResultSettlementDisposition::OwnerMissing),
                    Some(owner) if owner.state != WorkItemState::Open => {
                        Some(TaskResultSettlementDisposition::OwnerClosed)
                    }
                    Some(_) => None,
                };
                if let Some(disposition) = unavailable {
                    self.apply_task_result_operation((&record.message_id, disposition, now), || crate::runtime_db::transitions::TaskResultOperation::SettleOwnerUnavailable {
                        message_id:record.message_id.clone(),disposition,at:now}).await?.changed()?;
                    continue;
                }
                if owner.is_some_and(|owner| !owner.is_runnable())
                    || (read.inputs.execution.initialized
                        && !matches!(
                            read.inputs.execution_work_item(work_item_id)?,
                            crate::runtime::resident_input_evidence::ObservedWorkExecution::Present(work)
                                if matches!(work.state, WorkItemExecutionState::Runnable { .. })
                        ))
                {
                    eligible = false;
                    reason = "owner_not_runnable";
                }
            }
            if read.owner_has_unresolved_wait(record.work_item_id.as_deref())? {
                eligible = false;
                reason = "owner_has_unresolved_wait";
            }
            let source = read.source_message(&record.message_id)?;
            anyhow::ensure!(
                crate::wake_contract::agent_message_reply_reference(source)
                    == metadata.reply_message_id.as_deref(),
                "recovery Result reply observation differs from qualified source"
            );
            if let Some(reply_id) = metadata.reply_message_id.as_deref() {
                if read.reply_processed(reply_id) {
                    // Content was consumed by another execution owner. Preserve
                    // that distinct outcome instead of claiming caller admission.
                    self.apply_task_result_operation((&record.message_id, TaskResultSettlementDisposition::ReplyConsumedElsewhere, now), || crate::runtime_db::transitions::TaskResultOperation::SettleOwnerUnavailable {
                        message_id:record.message_id.clone(),disposition:TaskResultSettlementDisposition::ReplyConsumedElsewhere,
                        at:now}).await?.changed()?;
                    continue;
                }
                // The delivery ledger and original message own reply recovery.
                // A reference observation must not create a second model wake.
                eligible = false;
                reason = "awaiting_original_agent_reply_admission";
            }
            // Advance first: a crash or enqueue failure still leaves a bounded
            // retry, while an ineligible owner cannot create a hot loop.
            self.apply_task_result_operation(
                (&record.message_id, reason, now, RESULT_RECHECK_SECONDS),
                || crate::runtime_db::transitions::TaskResultOperation::Defer {
                    message_id: record.message_id.clone(),
                    reason: reason.to_owned(),
                    at: now,
                    deadline: now + chrono::Duration::seconds(RESULT_RECHECK_SECONDS),
                },
            )
            .await?
            .record()?;
            let owner_key = recovery_owner_key(record);
            if !eligible || !queued_owners.insert(owner_key.clone()) {
                continue;
            }
            if let Some(message_id) =
                read.outstanding(record.work_item_id.as_deref(), &owner_key)?
            {
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
            message.work_item_id = record.work_item_id.clone();
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
                .insert("task_result_message_id".into(), record.message_id.clone());
            message.source_refs.insert(
                "task_result_identity".into(),
                record.result_identity.clone(),
            );
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
            {
                let mut mutation = runtime.begin_mutation().await.unwrap();
                prepare_outstanding_recovery_message_id(&runtime, &mut mutation, None, owner_key)
                    .await
                    .unwrap()
                    .map(|observation| observation.id().to_owned())
            },
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
            {
                let mut mutation = runtime.begin_mutation().await.unwrap();
                prepare_outstanding_recovery_message_id(&runtime, &mut mutation, None, owner_key)
                    .await
                    .unwrap()
                    .map(|observation| observation.id().to_owned())
            },
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
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        assert!(reply.is_runtime_owned_internal_followup());
        // The original reply was actually consumed by another dispatch.
        // Retain its real qualification/assignment rather than a raw Processed row.
        {
            runtime.enqueue(reply.clone()).await.unwrap();
            let scheduler_executor::RunLoopPoll::Message(consumed) =
                scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                    .poll()
                    .await
                    .unwrap()
            else {
                panic!("actual original reply claim")
            };
            assert_eq!(consumed.message.id, reply.id);
            let (turn, _) = runtime
                .begin_dispatch_turn(
                    &consumed.message,
                    None,
                    None,
                    consumed
                        .dispatch_plan
                        .execution_admission_provenance
                        .clone(),
                    &consumed.dispatch_plan.prior_closure,
                )
                .await
                .unwrap();
            let mut source = consumed.message.clone();
            source.turn_id = Some(turn);
            let terminal = crate::runtime::tests::support::terminal_transition_from_started_turn(
                runtime, &source,
            );
            runtime
                .finish_scheduled_message_processing(
                    &consumed.message,
                    consumed.running_state.current_run_id.as_deref().unwrap(),
                    std::time::Instant::now(),
                    Ok(terminal),
                )
                .await
                .unwrap();
            drop(consumed);
            assert_eq!(
                runtime
                    .runtime_db()
                    .queue_entries()
                    .latest(&reply.id)
                    .unwrap()
                    .unwrap()
                    .status,
                QueueEntryStatus::Processed
            );
        }
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
        let mut parent = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original task producer".into(),
            },
        );
        parent.work_item_id = Some(work.id.clone());
        runtime.enqueue(parent).await.unwrap();
        let scheduler_executor::RunLoopPoll::Message(scheduled) =
            scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                .poll()
                .await
                .unwrap()
        else {
            panic!("actual parent claim")
        };
        let (parent_turn, _) = runtime
            .begin_dispatch_turn(
                &scheduled.message,
                None,
                None,
                scheduled
                    .dispatch_plan
                    .execution_admission_provenance
                    .clone(),
                &scheduled.dispatch_plan.prior_closure,
            )
            .await
            .unwrap();
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
                serde_json::json!({"rejoin_obligation_id":"observer-task", "rejoin_generation":1, "parent_turn_id":parent_turn,
                    "message_id":reply.id,"reply_content_source":"original_message"}),
            ),
            recovery: None,
        };
        signal.metadata.as_mut().unwrap()["task_detail"] = task.detail.clone().unwrap();
        runtime
            .persist_task_transition_with_message(&task, "task_status_updated", &signal)
            .await
            .unwrap();
        // The original Task writer allocated the canonical source sequence.
        // Freeze EnsurePending from that qualified ACK, not its pre-write input.
        let signal_capture = {
            let mut mutation = runtime.begin_mutation().await.unwrap();
            let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy().unwrap();
            let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
                mandatory_roots: std::collections::BTreeSet::from([
                    crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                        crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                        &signal.id,
                    ),
                    crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                        crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                        &reply.id,
                    ),
                ]),
                windows: Vec::new(),
                interrupted_sources: Default::default(),
                selected_completion_brief_ids: Default::default(),
                record_limit,
                byte_limit,
            };
            mutation
                .prepare_owner_input_request(&request, 0)
                .await
                .unwrap();
            mutation.capture_input_evidence().unwrap()
        };
        let row = signal_capture
            .lookup(
                &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    &signal.id,
                ),
            )
            .unwrap()
            .unwrap();
        let crate::runtime::resident_input_evidence::PreparedInputEvidence::Message(actual_signal) =
            &row.record
        else {
            panic!("qualified source family")
        };
        assert_eq!(actual_signal.id, signal.id);
        assert!(actual_signal.message_seq.is_some());
        let signal = actual_signal;
        let at = harness.now() - chrono::Duration::seconds(31);
        runtime
            .apply_task_result_operation((&task.id, signal, at), || {
                crate::runtime_db::transitions::TaskResultOperation::EnsurePending {
                    task_id: task.id.clone(),
                    message: Box::new((*signal).clone()),
                    at,
                }
            })
            .await
            .unwrap()
            .record()
            .unwrap()
            .as_ref()
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
        let mut started_source = scheduled.message.clone();
        started_source.turn_id = Some(parent_turn);
        let terminal = crate::runtime::tests::support::terminal_transition_from_started_turn(
            runtime,
            &started_source,
        );
        runtime
            .finish_scheduled_message_processing(
                &scheduled.message,
                scheduled.running_state.current_run_id.as_deref().unwrap(),
                std::time::Instant::now(),
                Ok(terminal),
            )
            .await
            .unwrap();
        drop(scheduled);
    }
}
