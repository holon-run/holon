use super::*;
use crate::domain::execution_protocol::WorkItemExecutionState;
use crate::runtime_db::task_result_settlement::TaskResultObligation;
use crate::runtime_db::TaskResultSettlementDisposition;
use sha2::{Digest, Sha256};

const RESULT_RECHECK_SECONDS: i64 = 30;
const RESULT_RECHECK_LIMIT: usize = 8;

#[cfg(test)]
mod budget_tests;

#[cfg(test)]
mod terminal_anchor_tests;

#[cfg(test)]
mod repeated_failure_tests;

#[cfg(test)]
pub(super) mod stale_source_tests;

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
    async fn enqueue_exact_recovery_continuation(
        &self,
        result_message_id: &str,
        predecessor: &WaitConditionRecord,
        mut message: MessageEnvelope,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        use crate::domain::execution_protocol::{
            ExecutionProtocolCommand, SetWorkItemWaiting, WaitReference, WorkItemExecutionRecord,
        };
        use crate::runtime_db::transitions::{
            AgentStateMutation, ExecutionProtocolTransition, WaitTaskResultAdmission,
            WaitTaskResultObligation, WaitTransitionCommand, WorkItemMutation,
        };

        let repository = self.inner.runtime_db.task_result_settlements();
        let Some(record) = repository.latest_for_message(result_message_id)? else {
            return Ok(false);
        };
        if repository
            .exact_waiter_recovery_continuation(&record, now)?
            .as_ref()
            != Some(predecessor)
        {
            return Ok(false);
        }
        let state = self.agent_state().await?;
        if state.status == AgentStatus::Stopped {
            return Ok(false);
        }
        let mut successor = predecessor.clone();
        successor.id = crate::ids::wait_condition_id();
        successor.status = crate::types::WaitConditionStatus::Triggered;
        successor.created_at = now;
        successor.updated_at = now;
        successor.resolved_at = None;
        successor.cancelled_at = None;
        successor.turn_id = None;
        successor.trigger_message_id = Some(message.id.clone());
        successor.triggered_at = Some(now);
        successor.continuation = Some(serde_json::json!({
            "created_by": "WaitFor",
            "wake": "task_result",
            "resource": record.task_id,
            "task_result_recovery_predecessor_wait_id": predecessor.id,
            "task_result_recovery_predecessor_activation_id": record.activation_id,
            "clear_blocker_on_task_result": false,
        }));
        message
            .source_refs
            .insert("wait_id".into(), successor.id.clone());
        let source_id = predecessor
            .trigger_message_id()
            .expect("verified exact trigger");
        let Some(execution) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&record.agent_id)?
        else {
            return Ok(false);
        };
        if execution.open_attempt().is_some() {
            return Ok(false);
        }
        let mut execution_protocol = ExecutionProtocolTransition::default();
        let mut work_items = Vec::new();
        let mut index_changes = Vec::new();
        if let Some(owner_id) = successor.work_item_id.as_deref() {
            let Some(owner) = self.inner.runtime_db.work_items().latest(owner_id)? else {
                return Ok(false);
            };
            let Some(expected) = execution.work_items.get(owner_id).cloned() else {
                return Ok(false);
            };
            if owner.state != WorkItemState::Open
                || matches!(
                    expected.state,
                    WorkItemExecutionState::InFlight { .. }
                        | WorkItemExecutionState::Terminal { .. }
                )
            {
                return Ok(false);
            }
            let updated = WorkItemRecord {
                revision: owner.revision + 1,
                updated_at: now,
                ..owner.clone()
            };
            index_changes.extend(self.inner.storage.index_changes_for_work_item(&updated)?);
            execution_protocol
                .commands
                .push(ExecutionProtocolCommand::SetWorkItemWaiting(Box::new(
                    SetWorkItemWaiting {
                        command_id: format!("task-result-recovery-wait:{}", successor.id),
                        work_item_id: owner.id,
                        record: WorkItemExecutionRecord {
                            source_revision: updated.revision,
                            state: WorkItemExecutionState::Waiting {
                                generation: expected.generation() + 1,
                                wait: WaitReference {
                                    wait_id: successor.id.clone(),
                                },
                            },
                        },
                        expected: Some(expected),
                    },
                )));
            work_items.push(WorkItemMutation::Update {
                expected_revision: owner.revision,
                record: updated,
            });
        }
        let commit = self
            .inner
            .runtime_db
            .transitions()
            .commit_wait_with_execution_protocol(
                &WaitTransitionCommand {
                    agent_id: record.agent_id.clone(),
                    work_items,
                    expected_wait_conditions: Vec::new(),
                    wait_conditions: vec![successor.clone()],
                    timer_wake: None,
                    task_result_admission: Some(WaitTaskResultAdmission {
                        expected: None,
                        record: QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: QueueEntryStatus::Queued,
                            created_at: message.created_at,
                            updated_at: now,
                        },
                        message,
                        obligation: WaitTaskResultObligation {
                            expected: record,
                            current_activation_id: None,
                            waiter_work_item_id: predecessor.work_item_id.clone(),
                            canonical_recovery: true,
                            source_message_id: source_id.into(),
                            source_queue: self
                                .inner
                                .runtime_db
                                .queue_entries()
                                .latest(source_id)?,
                            wait_conditions: self
                                .inner
                                .storage
                                .latest_wait_conditions_for_agent(&state.id)?,
                        },
                    }),
                    agent_state: Some(AgentStateMutation {
                        expected: Some(Box::new(state.clone())),
                        record: Box::new(state),
                    }),
                    audit_events: vec![AuditEvent::legacy(
                        "task_result_exact_continuation_recovery_queued",
                        serde_json::json!({
                            "wait_condition_id": successor.id,
                            "predecessor_wait_condition_id": predecessor.id,
                            "work_item_id": predecessor.work_item_id,
                            "result_message_id": result_message_id,
                        }),
                    )],
                    index_changes,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                },
                &execution_protocol,
            )?;
        let applied = commit.applied;
        self.apply_transition_commit(commit).await;
        Ok(applied)
    }

    fn publish_paused_task_result_diagnostics(&self, agent_id: &str) -> Result<()> {
        loop {
            let records = self
                .inner
                .runtime_db
                .task_result_settlements()
                .unreported_paused(agent_id, RESULT_RECHECK_LIMIT)?;
            if records.is_empty() {
                return Ok(());
            }
            for record in records {
                let Some(paused_at) = record.recovery_paused_at else {
                    continue;
                };
                let mut brief = BriefRecord::new(
                    agent_id,
                    crate::types::BriefKind::Failure,
                    format!(
                        "Automatic task-result recovery paused: task={}, result={}, owner={}, \
                     root={}, reason=recovery_budget_exhausted, attempts={}/{}. \
                     The result remains pending. Operator repair: \
                     holon debug scheduler-recovery --agent {} --apply --rearm-task-result {}",
                        record.task_id,
                        record.result_identity,
                        record.work_item_id.as_deref().unwrap_or("agent_lifecycle"),
                        record
                            .last_recovery_root_attempt_id
                            .as_deref()
                            .unwrap_or("unknown"),
                        record.recovery_attempts,
                        crate::runtime_db::task_result_settlement::TASK_RESULT_RECOVERY_LIMIT,
                        agent_id,
                        record.result_identity,
                    ),
                    Some(record.message_id.clone()),
                    Some(record.task_id.clone()),
                );
                brief.id = format!(
                    "brief_task_result_recovery_{}_{}",
                    record.result_identity, record.recovery_epoch
                );
                brief.created_at = paused_at;
                brief.work_item_id = record.work_item_id.clone();
                // No turn binding or model input: this is an operator diagnostic,
                // not a completion report or another recovery wake.
                let event = crate::types::brief_created_event_for(&brief)?;
                self.inner
                    .storage
                    .append_brief_with_created_event(&brief, &event)?;
                self.inner
                    .runtime_db
                    .task_result_settlements()
                    .mark_recovery_diagnostic(
                        &record.result_identity,
                        record.recovery_epoch,
                        self.now(),
                    )?;
            }
        }
    }

    /// Rechecks are eligibility probes, not permission to consume a wait.
    pub(super) async fn emit_due_task_result_recovery(&self) -> Result<bool> {
        let state = self.agent_state().await?;
        if state.status == AgentStatus::Stopped {
            return Ok(false);
        }
        let now = self.now();
        self.publish_paused_task_result_diagnostics(&state.id)?;
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
            let exact_continuation = self
                .inner
                .runtime_db
                .task_result_settlements()
                .exact_waiter_recovery_continuation(&record, now)?;
            let consuming_owner = exact_continuation
                .as_ref()
                .map_or(record.work_item_id.as_deref(), |wait| {
                    wait.work_item_id.as_deref()
                });
            let deferred_reason = match self
                .inner
                .runtime_db
                .task_result_settlements()
                .classify_obligation(&record, None, consuming_owner)?
            {
                TaskResultObligation::Recoverable => None,
                TaskResultObligation::AlreadyAdmitted { .. }
                | TaskResultObligation::InFlight { .. } => Some("canonical_admission_in_flight"),
                TaskResultObligation::ClaimedByOtherWaiter { .. } => {
                    Some("claimed_by_other_waiter")
                }
                TaskResultObligation::InvalidOrStale => Some("invalid_or_stale_obligation"),
                TaskResultObligation::Settled { .. } | TaskResultObligation::Paused => continue,
            };
            if let Some(reason) = deferred_reason {
                self.inner
                    .runtime_db
                    .task_result_settlements()
                    .mark_deferred(
                        &record.message_id,
                        reason,
                        now,
                        now + chrono::Duration::seconds(RESULT_RECHECK_SECONDS),
                    )?;
                continue;
            }
            // A lifecycle-owned settlement whose result message already exists
            // can only be delivered through its anchor turn, a live exact
            // waiter, or a work-item wake root. When the anchor turn is
            // terminal with no open bound attempt and no exact waiter claims
            // the result, every admission channel is unreachable: converge to
            // the same terminal disposition as an unavailable owner instead of
            // rechecking forever. Reply-anchored records keep their distinct
            // dispositions below.
            if record.work_item_id.is_none()
                && exact_continuation.is_none()
                && !self
                    .inner
                    .runtime_db
                    .task_result_settlements()
                    .anchor_turn_live(&record.agent_id, &record.parent_turn_id)?
                && self
                    .inner
                    .storage
                    .read_message_by_id(&record.message_id)?
                    .as_ref()
                    .and_then(crate::wake_contract::agent_message_reply_reference)
                    .is_none()
            {
                tracing::debug!(
                    agent_id = %state.id,
                    result_identity = %record.result_identity,
                    parent_turn_id = %record.parent_turn_id,
                    "converging lifecycle task-result settlement with a terminal anchor turn"
                );
                self.inner
                    .runtime_db
                    .task_result_settlements()
                    .settle_owner_unavailable(
                        &record.message_id,
                        TaskResultSettlementDisposition::InvalidOrStale,
                        now,
                    )?;
                continue;
            }
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
                if exact_continuation.is_none()
                    && (owner.as_ref().is_some_and(|owner| !owner.is_runnable())
                        || execution.as_ref().is_some_and(|execution| {
                            execution.work_items.get(work_item_id).is_none_or(|work| {
                                !matches!(work.state, WorkItemExecutionState::Runnable { .. })
                            })
                        }))
                {
                    eligible = false;
                    reason = "owner_not_runnable";
                }
            }
            if waits
                .iter()
                .any(|wait| wait.work_item_id.as_deref() == consuming_owner)
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
                let consumed = self
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(reply_id)?
                    .is_some_and(|entry| entry.status == QueueEntryStatus::Processed)
                    || self
                        .inner
                        .runtime_db
                        .task_result_settlements()
                        .reply_source_is_consumed(&record)?;
                let recoverable = self
                    .inner
                    .runtime_db
                    .task_result_settlements()
                    .reply_source_is_recoverable(&record)?;
                if consumed && !recoverable {
                    // Content was consumed by another execution owner. Preserve
                    // that distinct outcome instead of claiming caller admission.
                    self.inner
                        .runtime_db
                        .task_result_settlements()
                        .settle_reply_consumed_elsewhere(&record.message_id, now)?;
                    continue;
                }
                if !recoverable {
                    // The original queue source still owns first delivery.
                    eligible = false;
                    reason = "awaiting_original_agent_reply_admission";
                }
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
            let mut consuming_record = record.clone();
            consuming_record.work_item_id = consuming_owner.map(str::to_owned);
            let owner_key = recovery_owner_key(&consuming_record);
            if !eligible || !queued_owners.insert(owner_key.clone()) {
                continue;
            }
            if let Some(message_id) =
                outstanding_recovery_message_id(self, &state.id, consuming_owner, &owner_key)?
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
            message.work_item_id = consuming_owner.map(str::to_owned);
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
            message
                .source_refs
                .insert("task_result_identity".into(), record.result_identity);
            if let Some(predecessor) = exact_continuation {
                enqueued |= self
                    .enqueue_exact_recovery_continuation(
                        &record.message_id,
                        &predecessor,
                        message,
                        now,
                    )
                    .await?;
            } else {
                self.enqueue(message).await?;
                enqueued = true;
            }
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
            source_projection: None,
            reply_projection: None,
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
            recovery_epoch: 0,
            recovery_attempts: 0,
            last_recovery_root_attempt_id: None,
            last_recovery_admitted_at: None,
            recovery_paused_at: None,
            recovery_diagnostic_at: None,
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
