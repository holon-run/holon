use super::*;
use crate::domain::execution_protocol::WorkItemExecutionState;
use crate::runtime_db::task_result_settlement::TaskResultObligation;
use crate::runtime_db::TaskResultSettlementDisposition;
use sha2::{Digest, Sha256};

const RESULT_RECHECK_SECONDS: i64 = 30;
const RESULT_RECHECK_LIMIT: usize = 8;

#[cfg(test)]
pub(super) mod budget_tests;

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
    recovery_owner_key_for(&record.agent_id, record.work_item_id.as_deref())
}

fn recovery_owner_key_for(agent_id: &str, work_item_id: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.task-result-recovery-owner.v1\0");
    hasher.update(agent_id.as_bytes());
    hasher.update([0]);
    hasher.update(work_item_id.unwrap_or("").as_bytes());
    format!("recovery_owner_{}", hex(&hasher.finalize()[..16]))
}

fn recovery_message_id() -> String {
    crate::ids::runtime_id("msg_task_result_recovery")
}

mod read;
#[cfg(test)]
pub(super) use read::due_results as due_recovery_results;
pub(super) use read::prepare_outstanding_recovery_message_id;

/// Compatibility for exact-wait repair's synchronous planner. Only active
/// Queue sources are decoded, after a same-view material preflight.
pub(super) fn outstanding_recovery_message_id(
    runtime: &RuntimeHandle,
    agent_id: &str,
    work_item_id: Option<&str>,
    owner_key: &str,
) -> Result<Option<String>> {
    use super::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
    anyhow::ensure!(
        agent_id == runtime.inner.mutation_owner.agent_id(),
        "recovery Queue observation belongs to another agent"
    );
    let scope = super::resident_input_evidence::InputEvidenceScope {
        agent_id: agent_id.to_owned(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let limits = runtime.inner.input_evidence_budget.limits();
    runtime.inner.runtime_db.transaction(|tx| {
        let owner_live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_identities
             WHERE agent_id=?1 AND status='active'
               AND CASE WHEN json_valid(payload_json)
                   THEN json_extract(payload_json,'$.incarnation')=?2
                   ELSE 0 END)",
            rusqlite::params![agent_id, scope.identity_incarnation],
            |row| row.get(0),
        )?;
        anyhow::ensure!(owner_live, "recovery Queue owner incarnation is unavailable");
        let (count, bytes): (u64, u64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(m.payload_json AS BLOB))),0)
             FROM messages m JOIN queue_entries q ON q.message_id=m.message_id
             WHERE m.agent_id=?1 AND q.agent_id=?1
               AND q.status NOT IN ('processed','aborted','dropped','quarantined')",
            [agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let bytes = count.checked_mul(512)
            .and_then(|n| n.checked_add(bytes))
            .and_then(|n| n.checked_mul(8))
            .context("recovery active source material overflow")?;
        anyhow::ensure!(
            count.checked_mul(8).context("recovery active source count overflow")? <= limits.record_limit
                && bytes <= limits.history_per_agent_bytes.checked_add(limits.pending_per_agent_bytes)
                    .context("recovery active source limit overflow")?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "recovery active Queue sources"
            )
        );
        let _guard = runtime.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate { payload_bytes: bytes, metadata_estimate_bytes: 128 },
        )?;
        let mut query = tx.prepare(
            "SELECT m.message_id,m.payload_json FROM messages m
             JOIN queue_entries q ON q.message_id=m.message_id
             WHERE m.agent_id=?1 AND q.agent_id=?1
               AND q.status NOT IN ('processed','aborted','dropped','quarantined')
             ORDER BY m.message_seq IS NULL ASC,m.message_seq DESC,m.created_at DESC,m.message_id DESC",
        )?;
        let mut rows = query.query([agent_id])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let payload: String = row.get(1)?;
            let message: MessageEnvelope = serde_json::from_str(&payload)?;
            anyhow::ensure!(
                message.id == id && message.agent_id == agent_id,
                "recovery active source projection differs"
            );
            if matches!(&message.origin, MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
                && message.authority_class == AuthorityClass::RuntimeInstruction
                && message.work_item_id.as_deref() == work_item_id
                && message.source_refs.get("task_result_owner_key").map(String::as_str) == Some(owner_key)
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    })
}

impl RuntimeHandle {
    async fn enqueue_exact_recovery_continuation(
        &self,
        record: crate::runtime_db::TaskResultSettlementRecord,
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

        let mut mutation = self.begin_mutation().await?;
        let repository = self.inner.runtime_db.task_result_settlements();
        let result_message_id = record.message_id.clone();
        if repository
            .exact_waiter_recovery_continuation(&record, now)?
            .as_ref()
            != Some(predecessor)
        {
            return Ok(false);
        }
        let state = mutation.state.clone();
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
        let task_request = super::resident_task::TaskCaptureRequest {
            include_active: false,
            include_unsettled: false,
            terminal_limit: 0,
            recovery_source_ids: Default::default(),
            required_keys: std::collections::BTreeSet::from([super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::ResultSettlement,
                &record.result_identity,
            )]),
        };
        mutation.prepare_owner_task_request(&task_request).await?;
        let mut work_request = super::work_reads::live_work_request();
        work_request
            .required_trigger_message_ids
            .insert(source_id.to_owned());
        work_request
            .mandatory_roots
            .insert(super::resident_work::WorkKey::new(
                super::resident_work::WorkFamily::Wait,
                &predecessor.id,
            ));
        if let Some(owner_id) = successor.work_item_id.as_deref() {
            work_request
                .mandatory_roots
                .insert(super::resident_work::WorkKey::new(
                    super::resident_work::WorkFamily::Work,
                    owner_id,
                ));
        }
        mutation.prepare_owner_work_request(&work_request).await?;
        use super::resident_input_evidence::{
            InputEvidenceFamily, InputEvidenceKey, ObservedWorkExecution,
        };
        let (mut roots, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        roots.insert(InputEvidenceKey::new(
            InputEvidenceFamily::Message,
            source_id,
        ));
        if let Some(owner_id) = successor.work_item_id.as_deref() {
            roots.insert(InputEvidenceKey::new(
                InputEvidenceFamily::WorkExecution,
                owner_id,
            ));
        }
        let input_request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: roots,
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        let episode_limit = self
            .inner
            .config_snapshot
            .load_full()
            .base_context_config
            .recent_episode_candidates;
        mutation
            .prepare_owner_input_request(&input_request, episode_limit)
            .await?;
        let (tasks, work, inputs) = {
            let root = mutation.capture_published_root()?;
            (
                root.tasks
                    .as_ref()
                    .context("exact recovery Task domain missing")?
                    .capture(&task_request)?,
                root.work
                    .as_ref()
                    .context("exact recovery Work domain missing")?
                    .capture(&work_request)?,
                root.input_evidence
                    .as_ref()
                    .context("exact recovery Input domain missing")?
                    .capture_for_request(&input_request)?,
            )
        };
        if work.wait_condition(&predecessor.id)? != Some(predecessor)
            || !matches!(
                &tasks.result_dependency(&record.result_identity)?.relation,
                super::resident_task::TaskResultDependencyRelation::QualifiedCurrent { .. }
            )
            || !inputs.execution.initialized
            || !inputs.execution.open_attempt_ids.is_empty()
        {
            return Ok(false);
        }
        let selected = tasks
            .lookup(&super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::ResultSettlement,
                &record.result_identity,
            ))?
            .context("exact recovery Result is absent")?;
        anyhow::ensure!(
            matches!(&selected.fact, super::resident_task::CanonicalTaskFact::ResultSettlement(current) if current == &record),
            "exact recovery Result differs from the selected current record"
        );
        let (wait_witness, _wait_observation) = self.capture_task_result_wait_witness(
            &mutation,
            &work,
            Some(&predecessor.id),
            &[source_id],
        )?;
        let mut execution_protocol = ExecutionProtocolTransition::default();
        let mut work_items = Vec::new();
        let mut index_changes = Vec::new();
        if let Some(owner_id) = successor.work_item_id.as_deref() {
            let Some(owner) = work.work_item(owner_id)? else {
                return Ok(false);
            };
            let ObservedWorkExecution::Present(expected) = inputs.execution_work_item(owner_id)?
            else {
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
                        work_item_id: owner.id.clone(),
                        record: WorkItemExecutionRecord {
                            source_revision: updated.revision,
                            state: WorkItemExecutionState::Waiting {
                                generation: expected.generation() + 1,
                                wait: WaitReference {
                                    wait_id: successor.id.clone(),
                                },
                            },
                        },
                        expected: Some(expected.clone()),
                    },
                )));
            work_items.push(WorkItemMutation::Update {
                expected_revision: owner.revision,
                record: updated,
            });
        }
        let mut queued_state = state.clone();
        queued_state.pending = mutation.queue.len().saturating_add(1);
        queued_state.last_wake_reason = Some("TaskResult".into());
        scheduler::apply_message_wake_projection(&mut queued_state);
        let commit = mutation
            .commit_wait_transition(
                crate::ids::audit_event_id(),
                WaitTransitionCommand {
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
                            dropped_reply_source: None,
                            wait_witness,
                        },
                    }),
                    agent_state: Some(AgentStateMutation {
                        expected: Some(Box::new(state)),
                        record: Box::new(queued_state),
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
                execution_protocol,
                None,
            )
            .await?;
        let applied = commit.applied;
        drop(mutation);
        self.apply_transition_commit(commit).await;
        Ok(applied)
    }

    /// The original admission transaction checks this Work revision, exact
    /// bodies and the live frontier before publishing a successor wait.
    pub(super) fn capture_task_result_wait_witness(
        &self,
        mutation: &super::state_mutation::RuntimeMutation,
        work: &super::resident_work::CapturedWork,
        predecessor_id: Option<&str>,
        source_ids: &[&str],
    ) -> Result<(
        crate::runtime_db::transitions::WaitTaskResultWitness,
        Arc<super::input_evidence_budget::BudgetViewGuard>,
    )> {
        use super::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
        let scope = mutation.input_evidence_scope();
        let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        anyhow::ensure!(
            work.scope == scope,
            "exact recovery Wait witness scope differs"
        );
        let live_waits = work.unresolved_wait_conditions()?;
        let mut exact_waits = Vec::new();
        for source_id in source_ids {
            exact_waits.extend(work.waits_triggered_by(source_id)?);
        }
        if let Some(predecessor_id) = predecessor_id {
            if let Some(predecessor) = work.wait_condition(predecessor_id)? {
                exact_waits.push(predecessor);
            }
        }
        exact_waits.retain(|wait| {
            !matches!(
                wait.status,
                WaitConditionStatus::Active | WaitConditionStatus::Triggered
            )
        });
        exact_waits.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        exact_waits.dedup_by(|a, b| a.id == b.id);
        let count = live_waits
            .len()
            .checked_add(exact_waits.len())
            .context("exact recovery Wait witness count overflow")?;
        let bytes = super::input_evidence_budget::serialized_bytes(&(
            &scope,
            work.revision,
            &live_waits,
            &exact_waits,
        ))?
        .checked_add(
            u64::try_from(count)?
                .checked_mul(512)
                .context("exact recovery Wait witness metadata overflow")?,
        )
        .and_then(|bytes| bytes.checked_mul(8))
        .context("exact recovery Wait witness material overflow")?;
        anyhow::ensure!(
            count
                .checked_mul(8)
                .context("exact recovery Wait witness count overflow")?
                <= record_limit
                && bytes <= u64::try_from(byte_limit)?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "exact recovery Wait witness"
            )
        );
        let guard = self.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 128,
            },
        )?;
        Ok((
            crate::runtime_db::transitions::WaitTaskResultWitness {
                scope,
                work_revision: work.revision,
                live_waits: live_waits.into_iter().cloned().collect(),
                exact_waits: exact_waits.into_iter().cloned().collect(),
            },
            guard,
        ))
    }

    async fn publish_paused_task_result_diagnostics(&self, agent_id: &str) -> Result<()> {
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
                     root={}, reason={}, attempts={}/{}. \
                     The result remains pending. Operator repair: \
                     holon debug scheduler-recovery --agent {} --apply --rearm-task-result {}",
                        record.task_id,
                        record.result_identity,
                        record.work_item_id.as_deref().unwrap_or("agent_lifecycle"),
                        record
                            .last_recovery_root_attempt_id
                            .as_deref()
                            .unwrap_or("unknown"),
                        record
                            .deferred_reason
                            .as_deref()
                            .unwrap_or("recovery_budget_exhausted"),
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
                let now = self.now();
                self.apply_task_result_operation(
                    (&record.result_identity, record.recovery_epoch, now),
                    || crate::runtime_db::transitions::TaskResultOperation::MarkRecoveryDiagnostic {
                        result_identity: record.result_identity.clone(),
                        epoch: record.recovery_epoch,
                        now,
                    },
                )
                .await?
                .unit()?;
            }
        }
    }

    /// Rechecks are eligibility probes, not permission to consume a wait.
    pub(super) async fn emit_due_task_result_recovery(&self) -> Result<bool> {
        let state = self.agent_state().await?;
        if state.status == AgentStatus::Stopped {
            return Ok(false);
        }
        #[cfg(feature = "recovery-acceptance")]
        let (acceptance, acceptance_enqueued) = super::recovery_acceptance::service(self).await?;
        self.publish_paused_task_result_diagnostics(&state.id)
            .await?;
        let now = self.now();
        let read = {
            let mut mutation = self.begin_mutation().await?;
            mutation.prepare_task_recovery_read(self, now).await?
        };
        let Some(read) = read else {
            #[cfg(feature = "recovery-acceptance")]
            return Ok(acceptance_enqueued);
            #[cfg(not(feature = "recovery-acceptance"))]
            return Ok(false);
        };
        let state = &read.state;
        let mut queued_owners = std::collections::BTreeSet::new();
        let mut enqueued = false;
        #[cfg(feature = "recovery-acceptance")]
        {
            enqueued |= acceptance_enqueued;
        }
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
            #[cfg(feature = "recovery-acceptance")]
            if super::recovery_acceptance::hold_scanner(self, acceptance.as_ref(), &record)? {
                continue;
            }
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
                continue;
            }
            // A lifecycle-owned settlement whose result message already exists
            // can only be delivered through its anchor turn, a live exact
            // waiter, a work-item wake root, or its own terminal re-entry
            // queue root. When the anchor turn is terminal with no open bound
            // attempt, no exact waiter claims the result, and the durable task
            // no longer carries terminal re-entry authority, every admission
            // channel is unreachable: converge to the same terminal
            // disposition as an unavailable owner instead of rechecking
            // forever. Reply-anchored records keep their distinct dispositions
            // below.
            if record.work_item_id.is_none()
                && exact_continuation.is_none()
                && match record.source.parent_turn_id() {
                    Some(parent_turn_id) => !self
                        .inner
                        .runtime_db
                        .task_result_settlements()
                        .anchor_turn_live(&record.agent_id, parent_turn_id)?,
                    None => false,
                }
                && crate::wake_contract::agent_message_reply_reference(
                    read.source_message(&record.message_id)?,
                )
                .is_none()
                && !read.terminal_reentry(&record.result_identity)?
            {
                tracing::debug!(
                    agent_id = %state.id,
                    result_identity = %record.result_identity,
                    parent_turn_id = ?record.source.parent_turn_id(),
                    "converging lifecycle task-result settlement with a terminal anchor turn"
                );
                self.apply_task_result_operation(
                    (&record.message_id, TaskResultSettlementDisposition::InvalidOrStale, now),
                    || crate::runtime_db::transitions::TaskResultOperation::SettleOwnerUnavailable {
                        message_id: record.message_id.clone(),
                        disposition: TaskResultSettlementDisposition::InvalidOrStale,
                        at: now,
                    },
                )
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
                if exact_continuation.is_none()
                    && (owner.is_some_and(|owner| !owner.is_runnable())
                        || (read.inputs.execution.initialized
                            && !matches!(
                                read.inputs.execution_work_item(work_item_id)?,
                                crate::runtime::resident_input_evidence::ObservedWorkExecution::Present(work)
                                    if matches!(work.state, WorkItemExecutionState::Runnable { .. })
                            )))
                {
                    eligible = false;
                    reason = "owner_not_runnable";
                }
            }
            if read.owner_has_unresolved_wait(consuming_owner)? {
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
                let recoverable = read.reply_recoverable(&record.result_identity);
                if (read.reply_processed(reply_id)
                    || self
                        .inner
                        .runtime_db
                        .task_result_settlements()
                        .reply_source_is_consumed(record)?)
                    && !recoverable
                {
                    // Content was consumed by another execution owner. Preserve
                    // that distinct outcome instead of claiming caller admission.
                    self.apply_task_result_operation((&record.message_id, TaskResultSettlementDisposition::ReplyConsumedElsewhere, now), || crate::runtime_db::transitions::TaskResultOperation::SettleOwnerUnavailable {
                        message_id:record.message_id.clone(),disposition:TaskResultSettlementDisposition::ReplyConsumedElsewhere,
                        at:now}).await?.changed()?;
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
            let deferred = self
                .apply_task_result_operation(
                    (&record.message_id, reason, now, RESULT_RECHECK_SECONDS),
                    || crate::runtime_db::transitions::TaskResultOperation::Defer {
                        message_id: record.message_id.clone(),
                        reason: reason.to_owned(),
                        at: now,
                        deadline: now + chrono::Duration::seconds(RESULT_RECHECK_SECONDS),
                    },
                )
                .await?;
            let Some(record) = deferred.record()? else {
                continue;
            };
            let owner_key = recovery_owner_key_for(&record.agent_id, consuming_owner);
            if !eligible || !queued_owners.insert(owner_key.clone()) {
                continue;
            }
            if let Some(message_id) = read.outstanding(consuming_owner, &owner_key)? {
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
            let (source_key, source_id) = record.source.result_source_ref();
            message
                .source_refs
                .insert(source_key.into(), source_id.into());
            message
                .source_refs
                .insert("task_result_message_id".into(), record.message_id.clone());
            message.source_refs.insert(
                "task_result_identity".into(),
                record.result_identity.clone(),
            );
            #[cfg(feature = "recovery-acceptance")]
            let acceptance_message = message.clone();
            if let Some(predecessor) = exact_continuation {
                enqueued |= self
                    .enqueue_exact_recovery_continuation(record.clone(), &predecessor, message, now)
                    .await?;
            } else {
                self.enqueue(message).await?;
                enqueued = true;
            }
            #[cfg(feature = "recovery-acceptance")]
            super::recovery_acceptance::scanner_enqueued(
                self,
                acceptance.as_ref(),
                &acceptance_message,
            )?;
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
            source: crate::domain::execution_protocol::TaskRejoinSource::ParentTurn {
                parent_turn_id: "turn-1".into(),
            },
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
        record.source = crate::domain::execution_protocol::TaskRejoinSource::ParentTurn {
            parent_turn_id: "turn-2".into(),
        };
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
                query: None,
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
