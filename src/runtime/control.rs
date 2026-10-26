//! Runtime preparation for the focused atomic Control family.
//!
//! No side effect occurs until a proven canonical ACK is installed/published.
//! Stable command identity and authorization are supplied by the admitted entry.

use super::{
    command_task::ManagedTaskHandle,
    control_cancellation::{ControlJoinLease, ControlRunLease},
    *,
};
use crate::{
    domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding},
    runtime_db::control_transition::{
        self, ControlCancellationKind, ControlCancellationTarget, ControlInvocation, ControlKey,
        ControlPlanInput, ControlRunningProof, ControlSnapshot, ControlSource, PreparedControl,
    },
};

pub(super) struct PreparedRuntimeControl {
    pub(super) command: PreparedControl,
    pub(super) cancellation: Arc<PreparedControlCancellation>,
}

pub(super) enum OriginalControlLease {
    Run(Arc<ControlRunLease>),
    Join(Arc<ControlJoinLease>),
    Worktree {
        original: TaskRecord,
        terminal: TaskRecord,
        metadata: Mutex<Option<super::task_status_metadata::OriginalControlMetadataCapsule>>,
    },
    /// An unloaded/legacy producer has no local process lease. The durable
    /// obligation remains explicit; missing memory is never a join receipt.
    Absent {
        registration: Option<Arc<super::control_reconcile::ControlRegistrationRecord>>,
    },
}
pub(super) struct FrozenControlCancellation {
    pub(super) target: ControlCancellationTarget,
    pub(super) lease: OriginalControlLease,
}

pub(super) struct PreparedControlCancellation {
    pub(super) targets: Vec<FrozenControlCancellation>,
    signalled: AtomicBool,
    worker_started: AtomicBool,
    local_responsibility_finished: Arc<AtomicBool>,
}
impl std::fmt::Debug for PreparedControlCancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedControlCancellation")
            .field(
                "obligation_ids",
                &self
                    .targets
                    .iter()
                    .map(|target| target.target.obligation_id.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("signalled", &self.signalled.load(Ordering::Acquire))
            .finish()
    }
}
impl PreparedControlCancellation {
    fn new(targets: Vec<FrozenControlCancellation>) -> Arc<Self> {
        Arc::new(Self {
            targets,
            signalled: AtomicBool::new(false),
            worker_started: AtomicBool::new(false),
            local_responsibility_finished: Arc::new(AtomicBool::new(false)),
        })
    }
    /// Invoked only after exact known state installation/publication. Recovery
    /// clones this same Arc; repeating the notification cannot hit newer leases.
    pub(super) fn signal(&self) {
        if self.signalled.swap(true, Ordering::AcqRel) {
            return;
        }
        for frozen in &self.targets {
            match &frozen.lease {
                OriginalControlLease::Run(lease) => lease.cancel(),
                OriginalControlLease::Join(lease) => lease.cancel(),
                OriginalControlLease::Worktree { .. } | OriginalControlLease::Absent { .. } => {}
            }
        }
    }
    /// Registration is synchronous and precedes the first await. The registry
    /// keeps this worker's JoinHandle if its API caller/Host drainer is cancelled.
    pub(super) fn start_completion(
        self: &Arc<Self>,
        runtime: &RuntimeHandle,
        key: &ControlKey,
        digest: &str,
        owner_epoch: u64,
    ) {
        if self.worker_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let facts = self.clone();
        let runtime_clone = runtime.clone();
        let key = key.clone();
        let digest = digest.to_string();
        let handle = tokio::spawn(async move {
            let result = runtime_clone
                .complete_original_control_cancellation(&key, &facts)
                .await;
            if result.is_ok() {
                facts
                    .local_responsibility_finished
                    .store(true, Ordering::Release);
                let _ = runtime_clone.inner.control_cancellations.release_prepared(
                    &key,
                    &digest,
                    owner_epoch,
                );
            } else {
                // Keep the saved plan and durable pending obligations for the
                // next exact reconciler; never retry against current handles.
                facts.worker_started.store(false, Ordering::Release);
            }
            result
        });
        runtime
            .inner
            .control_cancellations
            .track_control_completion(handle, self.local_responsibility_finished.clone());
    }
}

impl RuntimeHandle {
    pub(super) async fn prepare_runtime_control(
        &self,
        mutation: &state_mutation::RuntimeMutation,
        key: ControlKey,
        invocation: ControlInvocation,
    ) -> Result<PreparedRuntimeControl> {
        self.prepare_runtime_control_with_audit(mutation, key, invocation, Vec::new())
            .await
    }

    pub(super) async fn prepare_runtime_control_with_audit(
        &self,
        mutation: &state_mutation::RuntimeMutation,
        key: ControlKey,
        invocation: ControlInvocation,
        audit_events: Vec<AuditEvent>,
    ) -> Result<PreparedRuntimeControl> {
        anyhow::ensure!(
            key.agent_id == mutation.state.id
                && key.agent_id == self.inner.mutation_owner.agent_id()
                && key.identity_incarnation == self.inner.identity_incarnation,
            "Control runtime scope mismatch"
        );
        let snapshot = self.inner.runtime_db.load_control_snapshot(&key)?;
        anyhow::ensure!(
            snapshot.state == mutation.last_persisted_state
                && mutation.state == mutation.last_persisted_state,
            "Control preparation requires the exact published durable baseline"
        );
        let prepared_at = self.now();
        let protected_control_attempt = protected_control_attempt(&snapshot, &invocation)?;
        let protected_control_run_id = self.protected_control_run_id(
            &snapshot,
            &invocation,
            protected_control_attempt.as_deref(),
        );
        let is_stop = invocation.action.canonical() == ControlAction::Stop;
        let healthy_running = if is_stop {
            None
        } else {
            self.healthy_control_running_proof(mutation, &snapshot)?
        };
        let task_results = if is_stop {
            snapshot
                .tasks
                .iter()
                .map(|task| tasks::prepare_lifecycle_stop_task_result(task, prepared_at))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let frozen = if is_stop {
            self.freeze_control_cancellation(
                mutation,
                &key,
                &snapshot,
                protected_control_run_id.as_deref(),
                &task_results,
            )
            .await?
        } else {
            Vec::new()
        };
        let cancellation_targets = frozen.iter().map(|frozen| frozen.target.clone()).collect();
        let command = control_transition::prepare_control(ControlPlanInput {
            key,
            invocation,
            snapshot,
            prepared_at,
            task_results,
            healthy_running,
            protected_control_attempt,
            protected_control_run_id,
            cancellation_targets,
            audit_events,
            index_changes: Vec::new(),
        })?;
        Ok(PreparedRuntimeControl {
            command,
            cancellation: PreparedControlCancellation::new(frozen),
        })
    }

    fn protected_control_run_id(
        &self,
        snapshot: &ControlSnapshot,
        invocation: &ControlInvocation,
        protected_attempt: Option<&str>,
    ) -> Option<String> {
        let run_id = snapshot.state.current_run_id.as_ref()?;
        let lease = self.inner.control_cancellations.run_for(run_id)?;
        if lease.run_id() != run_id {
            return None;
        }
        if let Some(attempt_id) = protected_attempt {
            let attempt = snapshot
                .execution_protocol
                .as_ref()?
                .attempts
                .get(attempt_id)?;
            return (lease.activation_id() == Some(attempt_id)
                && attempt.state == ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(lease.source_message_id()))
            .then(|| run_id.clone());
        }
        let ControlSource::ControlMessage { message_id } = &invocation.source else {
            return None;
        };
        (snapshot.reducer_control_source_message_id.as_deref() == Some(message_id.as_str())
            && lease.activation_id().is_none()
            && lease.source_message_id() == message_id)
            .then(|| run_id.clone())
    }

    fn healthy_control_running_proof(
        &self,
        mutation: &state_mutation::RuntimeMutation,
        snapshot: &ControlSnapshot,
    ) -> Result<Option<ControlRunningProof>> {
        if snapshot.state.status != AgentStatus::AwakeRunning {
            return Ok(None);
        }
        let run_id = snapshot
            .state
            .current_run_id
            .as_deref()
            .ok_or_else(|| anyhow!("Running Control posture has no run identity"))?;
        let handle = mutation
            .current_run_abort
            .as_ref()
            .filter(|handle| handle.run_id == run_id && !handle.token.is_cancelled())
            .ok_or_else(|| anyhow!("Running Control posture has no healthy original handle"))?;
        let lease = self
            .inner
            .control_cancellations
            .run_for(run_id)
            .filter(|lease| !lease.is_complete())
            .ok_or_else(|| anyhow!("Running Control posture has no active completion lease"))?;
        let _ = handle;
        let Some(attempt_id) = lease.activation_id() else {
            anyhow::ensure!(
                snapshot.reducer_source_message_id.as_deref() == Some(lease.source_message_id()),
                "Running Control has no qualified reducer-only execution"
            );
            let binding = snapshot
                .state
                .current_execution_binding
                .as_ref()
                .ok_or_else(|| anyhow!("Running reducer has no current execution binding"))?;
            return Ok(Some(ControlRunningProof {
                run_id: run_id.into(),
                attempt_id: None,
                source_message_id: lease.source_message_id().into(),
                turn_id: Some(binding.turn_id.clone()),
            }));
        };
        let attempt = snapshot
            .execution_protocol
            .as_ref()
            .and_then(|protocol| protocol.attempts.get(attempt_id))
            .filter(|attempt| {
                attempt.state == ExecutionAttemptState::Open
                    && attempt.source_message_id.as_deref() == Some(lease.source_message_id())
            })
            .ok_or_else(|| anyhow!("Running Control posture has no current Open claim"))?;
        let binding = snapshot
            .state
            .current_execution_binding
            .as_ref()
            .filter(|binding| binding.source_message_id == lease.source_message_id());
        let turn_id = binding
            .and_then(|binding| {
                snapshot.attempt_turns.iter().find(|turn| {
                    turn.turn_id == binding.turn_id
                        && turn.run_id.as_deref() == Some(run_id)
                        && turn.terminal.is_none()
                })
            })
            .map(|turn| turn.turn_id.clone());
        anyhow::ensure!(
            turn_id.is_some()
                || snapshot.queue_entries.iter().any(|entry| entry.message_id
                    == lease.source_message_id()
                    && matches!(
                        entry.status,
                        QueueEntryStatus::Dequeued | QueueEntryStatus::Interjected
                    )),
            "Running Control has no current source claim"
        );
        let _ = attempt;
        Ok(Some(ControlRunningProof {
            run_id: run_id.into(),
            attempt_id: Some(attempt_id.into()),
            source_message_id: lease.source_message_id().into(),
            turn_id,
        }))
    }

    async fn freeze_control_cancellation(
        &self,
        mutation: &state_mutation::RuntimeMutation,
        key: &ControlKey,
        snapshot: &ControlSnapshot,
        protected_control_run_id: Option<&str>,
        task_results: &[control_transition::ControlTaskResult],
    ) -> Result<Vec<FrozenControlCancellation>> {
        let owner_epoch = self.inner.mutation_owner.owner_epoch();
        let mut frozen = Vec::new();
        let own_command_run = protected_control_run_id == snapshot.state.current_run_id.as_deref()
            && protected_control_run_id.is_some();
        if !own_command_run {
            if let Some(run_id) = snapshot.state.current_run_id.as_deref() {
                if let Some(handle) = mutation.current_run_abort.as_ref() {
                    anyhow::ensure!(
                        handle.run_id == run_id,
                        "Control run handle differs from State"
                    );
                }
                let lease = self.inner.control_cancellations.run_for(run_id);
                let (activation_id, source_message_id) = if let Some(lease) = &lease {
                    (
                        lease.activation_id().map(str::to_owned),
                        Some(lease.source_message_id().to_owned()),
                    )
                } else {
                    absent_run_canonical_claim(snapshot, run_id)
                };
                frozen.push(FrozenControlCancellation {
                    target: cancellation_target(key, ControlCancellationKind::Run, run_id,
                        serde_json::json!({"activation_id":activation_id, "source_message_id":source_message_id,
                            "owner_epoch":owner_epoch,"runtime_lease_id":lease.as_ref().map(|lease|lease.lease_id())}),
                        serde_json::json!({"reason":"agent_stopped"})),
                    lease: lease.map(OriginalControlLease::Run).unwrap_or(OriginalControlLease::Absent {registration:None}),
                });
            }
        }
        let handles = self.inner.task_handles.lock().await;
        for task in &snapshot.tasks {
            let expected_generation = serde_json::json!({"created_at": task.created_at, "rejoin": task.rejoin_fence().ok()});
            let lease = handles
                .get(&task.id)
                .and_then(ManagedTaskHandle::control_lease);
            if let Some(lease) = &lease {
                anyhow::ensure!(
                    lease.matches_task_generation(task),
                    "Control task handle belongs to another generation"
                );
            }
            let terminal = &task_results
                .iter()
                .find(|result| result.task.id == task.id)
                .context("Control original terminal Task missing")?
                .task;
            let raw_original = super::task_generation::TaskRejoinGeneration::of(task)
                .context("Control original Task has an invalid rejoin generation")?;
            let raw_terminal = super::task_generation::TaskRejoinGeneration::of(terminal)
                .context("Control terminal Task has an invalid rejoin generation")?;
            let mut generation = expected_generation;
            generation[super::task_generation::CONTROL_ORIGINAL_REJOIN] =
                serde_json::to_value(raw_original)?;
            generation[super::task_generation::CONTROL_TERMINAL_REJOIN] =
                serde_json::to_value(raw_terminal)?;
            generation["owner_epoch"] = serde_json::json!(owner_epoch);
            generation["runtime_lease_id"] =
                serde_json::json!(lease.as_ref().map(|lease| lease.lease_id()));
            frozen.push(FrozenControlCancellation { target: cancellation_target(key, ControlCancellationKind::Task, &task.id,
                generation.clone(), serde_json::json!({"kind": task.kind, "recovery": task.recovery, "work_item_id": task.effective_work_item_id()})),
                lease: lease.map(OriginalControlLease::Join).unwrap_or_else(|| OriginalControlLease::Absent {
                    registration: self.inner.control_cancellations.registration_for_task(task) }) });
            if task.is_worktree_child_agent_task() {
                generation["artifact"] = task
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("worktree"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                frozen.push(FrozenControlCancellation {
                    target: cancellation_target(
                        key,
                        ControlCancellationKind::Worktree,
                        &task.id,
                        generation,
                        serde_json::json!({"reason": "agent_lifecycle_stop"}),
                    ),
                    lease: OriginalControlLease::Worktree {
                        original: task.clone(),
                        terminal: task_results
                            .iter()
                            .find(|result| result.task.id == task.id)
                            .ok_or_else(|| anyhow!("Control worktree task result missing"))?
                            .task
                            .clone(),
                        metadata: Mutex::new(None),
                    },
                });
            }
        }
        drop(handles);
        for timer in &snapshot.timers {
            let leases = self.inner.control_cancellations.timer_for(&timer.id);
            anyhow::ensure!(
                leases.len() <= 1,
                "Control timer has duplicate active local loops"
            );
            let lease = leases.into_iter().next();
            if let Some(lease) = &lease {
                anyhow::ensure!(
                    lease.domain_generation()
                        == &serde_json::json!({"created_at": timer.created_at}),
                    "Control timer loop belongs to another generation"
                );
            }
            frozen.push(FrozenControlCancellation { target: cancellation_target(key, ControlCancellationKind::TimerLoop, &timer.id,
                serde_json::json!({"created_at": timer.created_at, "fire_count": timer.fire_count, "next_fire_at": timer.next_fire_at,
                    "owner_epoch": owner_epoch, "runtime_lease_id": lease.as_ref().map(|lease| lease.lease_id())}),
                serde_json::json!({"interval_ms": timer.interval_ms, "repeat": timer.repeat})),
                lease: lease.map(OriginalControlLease::Join).unwrap_or(OriginalControlLease::Absent { registration: None }) });
        }
        Ok(frozen)
    }

    async fn complete_original_control_cancellation(
        &self,
        key: &ControlKey,
        facts: &PreparedControlCancellation,
    ) -> Result<()> {
        for frozen in &facts.targets {
            let evidence = match &frozen.lease {
                OriginalControlLease::Run(lease) => {
                    lease.join().await;
                    self.finish_original_stopped_dispatch(lease.clone()).await?;
                    serde_json::json!({"runtime_lease_id": lease.lease_id(), "joined": true})
                }
                OriginalControlLease::Join(lease) => {
                    lease.join().await?;
                    if frozen.target.target_kind == ControlCancellationKind::Task {
                        self.release_completed_task_handle_exact(
                            &frozen.target.target_identity,
                            lease.lease_id(),
                        )
                        .await;
                    }
                    serde_json::json!({"runtime_lease_id": lease.lease_id(), "joined": true})
                }
                OriginalControlLease::Worktree {
                    original,
                    terminal,
                    metadata,
                } => {
                    let limits = self.inner.input_evidence_budget.limits();
                    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                        query: Default::default(),
                        capture: super::resident_task::TaskCaptureRequest::current(),
                        record_limit: usize::try_from(limits.record_limit)?,
                        byte_limit: usize::try_from(
                            limits
                                .history_per_agent_bytes
                                .checked_add(limits.pending_per_agent_bytes)
                                .context("Control metadata limit overflow")?,
                        )?,
                    };
                    let mut metadata = metadata.lock().await;
                    // A completed source may still have an unpublished original
                    // whole ACK. Resolve that capsule before considering FS again.
                    if let Some(capsule) = metadata.as_mut() {
                        let committed = self
                            .commit_control_worktree_metadata(capsule, &request)
                            .await?;
                        anyhow::ensure!(
                            committed.ack.key == *key
                                && committed.ack.obligation_id == frozen.target.obligation_id,
                            "Control metadata ACK differs from its original target"
                        );
                        let failed = capsule.physical_failed();
                        *metadata = None;
                        anyhow::ensure!(
                            !failed,
                            "Control worktree cleanup failed; original obligation retained"
                        );
                        continue;
                    }
                    let scope = super::resident_input_evidence::InputEvidenceScope {
                        agent_id: key.agent_id.clone(),
                        identity_incarnation: key.identity_incarnation,
                    };
                    let initial = super::input_evidence_budget::serialized_bytes(&(
                        key,
                        &frozen.target,
                        original,
                        terminal,
                    ))?
                    .checked_add(1024)
                    .and_then(|n| n.checked_mul(8))
                    .context("Control completion reservation overflow")?;
                    let completion =
                        Arc::new(self.inner.input_evidence_budget.reserve_owned_completion(
                            &scope,
                            super::input_evidence_budget::InputMemoryCategory::Candidate,
                            super::input_evidence_budget::OwnedBytesEstimate {
                                payload_bytes: initial,
                                metadata_estimate_bytes: 0,
                            },
                        )?);
                    let producer = facts.targets.iter().find(|candidate| {
                        candidate.target.target_kind == ControlCancellationKind::Task
                            && candidate.target.target_identity == frozen.target.target_identity
                    });
                    let producer_completed=producer.map(|producer|self.inner.runtime_db.transaction(|tx|crate::runtime_db::task_status_metadata::original_target_completed_tx(tx,key,&producer.target))).transpose()?.unwrap_or(false);
                    if !producer_completed
                        || (self.inner.host_bridge.is_some() && original.is_child_agent_task())
                    {
                        // A parent monitor is not the independent child's exact
                        // execution lease. Never remove its workspace on that proof.
                        self.handoff_control_worktree(
                            key,
                            &frozen.target,
                            "original_worktree_producer_not_proven_joined",
                        )?;
                        continue;
                    }
                    // Joining original producers happened in earlier task/run
                    // obligations, without any owner permit. Keep this permit
                    // only across the exact artifact check + Git/FS cleanup, so
                    // a later Resume cannot reuse the artifact in between.
                    let _artifact_reservations = self.reserve_control_obligation_scope(key).await?;
                    let owner = self.inner.runtime_db.transaction(|tx| {
                        crate::runtime_db::command_task_terminal::observe_original_owner_tx(
                            tx,
                            &scope,
                            &request,
                            &self.inner.input_evidence_budget,
                        )
                    })?;
                    if !owner.identity.as_ref().is_some_and(|identity| {
                        identity.incarnation == key.identity_incarnation
                            && identity.status == crate::types::AgentRegistryStatus::Active
                    }) {
                        self.handoff_control_worktree(
                            key,
                            &frozen.target,
                            "original_identity_retired_artifact_retained",
                        )?;
                        continue;
                    }
                    let keys = BTreeSet::from([super::resident_task::TaskKey::new(
                        super::resident_task::TaskFamily::Task,
                        &original.id,
                    )]);
                    let (current,_task_material)=self.inner.runtime_db.transaction(|tx|{
                        let estimate=crate::runtime_db::task_snapshot::task_maintenance_material_tx(tx,&scope,&keys)?;
                        let guard=crate::runtime_db::command_task_terminal::charge_material(&scope,estimate.bytes,estimate.records,&request,&self.inner.input_evidence_budget)?;
                        use rusqlite::OptionalExtension;
                        let own:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND owner_agent_id=?2 AND CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.agent_id')=owner_agent_id AND json_extract(payload_json,'$.id')=task_id ELSE 0 END)",rusqlite::params![original.id,scope.agent_id],|r|r.get(0))?;
                        let raw:Option<String>=if own {tx.query_row("SELECT payload_json FROM tasks WHERE task_id=?1",[&original.id],|r|r.get(0)).optional()?}else{None};
                        let current=raw.as_deref().map(crate::runtime_db::repositories::decode_task_payload).transpose()?;
                        Ok((current,guard))
                    })?;
                    let mut audits = Vec::new();
                    let mut physical_failed = false;
                    let evidence = if current.as_ref() != Some(terminal) {
                        serde_json::json!({"cleanup": "retained", "reason": "original_task_replaced_or_advanced",
                            "original_created_at": original.created_at, "original_rejoin": original.rejoin_fence().ok()})
                    } else {
                        let mut detail = terminal
                            .detail
                            .clone()
                            .unwrap_or_else(|| serde_json::json!({}));
                        anyhow::ensure!(detail.get("worktree") == original.detail.as_ref().and_then(|detail| detail.get("worktree")),
                            "Control original worktree artifact differs from committed terminal Task");
                        let outcome = self
                            .cleanup_original_control_worktree_in_detail(
                                &original.id,
                                &mut detail,
                                "agent_lifecycle_stop",
                                completion.clone(),
                                &request,
                            )
                            .await?;
                        let outcome = outcome.map(|observed| {
                            audits = observed.audits;
                            observed.cleanup
                        });
                        physical_failed = outcome.as_ref().is_some_and(|outcome| {
                            outcome.status == worktree::TaskOwnedWorktreeCleanupStatus::Failed
                        });
                        let output_bytes = super::input_evidence_budget::serialized_bytes(&detail)
                            .unwrap_or(u64::MAX)
                            .saturating_add(outcome.as_ref().map_or(0, |outcome| {
                                outcome.changed_files.iter().fold(0u64, |n, path| {
                                    n.saturating_add(path.len() as u64).saturating_add(128)
                                })
                            }))
                            .saturating_add(1024)
                            .saturating_mul(8);
                        completion.observe_material(output_bytes, 0);
                        serde_json::json!({"cleanup": outcome.map(|outcome| format!("{outcome:?}")), "detail": detail})
                    };
                    *metadata = Some(
                        super::task_status_metadata::OriginalControlMetadataCapsule::observed(
                            scope,
                            key,
                            &frozen.target,
                            evidence,
                            audits,
                            physical_failed,
                            self.now(),
                            completion,
                        ),
                    );
                    drop(_task_material);
                    drop(_artifact_reservations);
                    let committed = self
                        .commit_control_worktree_metadata(
                            metadata.as_mut().expect("frozen above"),
                            &request,
                        )
                        .await?;
                    anyhow::ensure!(
                        committed.ack.key == *key
                            && committed.ack.obligation_id == frozen.target.obligation_id,
                        "Control metadata ACK differs from its original target"
                    );
                    *metadata = None;
                    anyhow::ensure!(
                        !physical_failed,
                        "Control worktree cleanup failed; original obligation retained"
                    );
                    continue;
                }
                OriginalControlLease::Absent { registration } => {
                    self.reconcile_absent_control_target(
                        key,
                        &frozen.target,
                        registration.as_ref(),
                    )
                    .await?;
                    // Completed or durably handed off, never inferred joined.
                    continue;
                }
            };
            self.inner.runtime_db.complete_control_obligation(
                key,
                &frozen.target,
                self.now(),
                evidence,
            )?;
        }
        Ok(())
    }
}

/// Without the original run lease, only an actually started canonical Turn
/// can link this administrative run to a claim. A stale binding is not proof.
fn absent_run_canonical_claim(
    snapshot: &ControlSnapshot,
    run_id: &str,
) -> (Option<String>, Option<String>) {
    let Some(binding) = snapshot.state.current_execution_binding.as_ref() else {
        return (None, None);
    };
    if !snapshot.attempt_turns.iter().any(|turn| {
        turn.turn_id == binding.turn_id
            && turn.run_id.as_deref() == Some(run_id)
            && turn.terminal.is_none()
    }) {
        return (None, None);
    }
    let candidates = snapshot
        .execution_protocol
        .as_ref()
        .into_iter()
        .flat_map(|protocol| protocol.attempts.values())
        .filter(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(binding.source_message_id.as_str())
                && (binding.activation_id.as_deref() == Some(attempt.attempt_id.as_str())
                    || (binding.activation_id.is_none()
                        && matches!(attempt.binding, ExecutionBinding::Command)))
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        (
            Some(candidates[0].attempt_id.clone()),
            Some(binding.source_message_id.clone()),
        )
    } else {
        (None, None)
    }
}

fn protected_control_attempt(
    snapshot: &ControlSnapshot,
    invocation: &ControlInvocation,
) -> Result<Option<String>> {
    let ControlSource::ControlMessage { message_id } = &invocation.source else {
        return Ok(None);
    };
    let attempts = snapshot
        .execution_protocol
        .as_ref()
        .into_iter()
        .flat_map(|protocol| protocol.attempts.values())
        .filter(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(message_id.as_str())
        })
        .collect::<Vec<_>>();
    if attempts.is_empty()
        && snapshot.reducer_control_source_message_id.as_deref() == Some(message_id.as_str())
    {
        return Ok(None);
    }
    anyhow::ensure!(
        attempts.len() == 1
            && matches!(&attempts[0].binding, ExecutionBinding::Command)
            && snapshot
                .queue_entries
                .iter()
                .any(|entry| entry.message_id == *message_id
                    && entry.status == QueueEntryStatus::Dequeued),
        "Control message has no exact consuming Command attempt"
    );
    Ok(Some(attempts[0].attempt_id.clone()))
}

fn cancellation_target(
    key: &ControlKey,
    kind: ControlCancellationKind,
    identity: &str,
    generation: serde_json::Value,
    target: serde_json::Value,
) -> ControlCancellationTarget {
    // Scoped command identity disambiguates obligations; domain generation is
    // verified separately and retained in the immutable obligation payload.
    ControlCancellationTarget {
        obligation_id: format!(
            "control:{}:{}:{}:{kind:?}:{identity}",
            key.agent_id, key.identity_incarnation, key.command_id
        ),
        target_kind: kind,
        target_identity: identity.into(),
        target_generation: generation,
        target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::context_config;
    use tempfile::tempdir;

    #[tokio::test]
    async fn queued_stop_preserves_its_original_consuming_run_until_terminal_settlement() {
        assert_queued_control_settles_its_original_reducer_run(ControlAction::Stop).await;
    }

    #[tokio::test]
    async fn queued_start_preserves_its_original_consuming_run_until_terminal_settlement() {
        assert_queued_control_settles_its_original_reducer_run(ControlAction::Start).await;
    }

    async fn assert_queued_control_settles_its_original_reducer_run(action: ControlAction) {
        let is_stop = action == ControlAction::Stop;
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().into(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let message = runtime
            .enqueue(MessageEnvelope::new(
                "default",
                MessageKind::Control,
                MessageOrigin::System {
                    subsystem: "queued-control-regression".into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: if is_stop { "stop" } else { "start" }.into(),
                },
            ))
            .await
            .unwrap();
        let loop_task = tokio::spawn(runtime.clone().run());
        let settlement = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if runtime
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&message.id)
                    .unwrap()
                    .is_some_and(|entry| entry.status == QueueEntryStatus::Processed)
                {
                    break;
                }
                assert!(
                    !loop_task.is_finished(),
                    "queued Control failed before terminal settlement"
                );
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            settlement.is_ok(),
            "queued Control did not settle: queue={:?}, state={:?}",
            runtime.inner.runtime_db.queue_entries().latest(&message.id),
            runtime.inner.runtime_db.agent_states().latest("default"),
        );
        let key = ControlKey {
            agent_id: "default".into(),
            identity_incarnation: runtime.inner.identity_incarnation,
            command_id: format!("control-message/v1:{}", message.id),
        };
        let invocation = ControlInvocation {
            action,
            source: ControlSource::ControlMessage {
                message_id: message.id.clone(),
            },
            authority_class: message.authority_class,
        };
        let receipt = runtime
            .inner
            .runtime_db
            .control_receipt(&key, &invocation, None)
            .unwrap()
            .unwrap();
        assert!(receipt.outcome.original_current_run_id.is_some());
        assert!(receipt.outcome.aborted_run_id.is_none());
        assert!(receipt.outcome.interrupted_attempt_ids.is_empty());
        assert!(receipt.obligation_ids.is_empty());
        // Control is reducer-only: it settles its actual Turn/queue without
        // inventing a Command execution attempt.
        let protocol = runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap();
        assert!(!protocol.as_ref().is_some_and(|protocol| protocol
            .attempts
            .values()
            .any(|attempt| attempt.source_message_id.as_deref() == Some(message.id.as_str()))));
        let assignment: String = runtime.inner.runtime_db.connection().unwrap().query_row(
            "SELECT turn_id FROM conversation_input_assignments WHERE agent_id='default' AND message_id=?1",
            [&message.id], |row| row.get(0),
        ).unwrap();
        let turn: TurnRecord = runtime
            .inner
            .runtime_db
            .connection()
            .unwrap()
            .query_row(
                "SELECT payload_json FROM turn_records WHERE agent_id='default' AND turn_id=?1",
                [&assignment],
                |row| row.get::<_, String>(0),
            )
            .map(|payload| serde_json::from_str(&payload).unwrap())
            .unwrap();
        assert_eq!(turn.run_id, receipt.outcome.original_current_run_id);
        assert_eq!(
            turn.trigger.unwrap().message_id.as_deref(),
            Some(message.id.as_str())
        );
        assert!(turn.terminal.is_some());
        if is_stop {
            assert_eq!(
                runtime.agent_state().await.unwrap().status,
                AgentStatus::Stopped
            );
        } else {
            assert!(!receipt.outcome.changed);
            assert_ne!(
                runtime.agent_state().await.unwrap().status,
                AgentStatus::Stopped
            );
        }
        runtime.request_service_shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), loop_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        runtime.join_owned_mutations().await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.inner.control_cancellations.drain_retiring(),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn stop_keeps_a_durable_absence_obligation_for_a_run_without_local_memory() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().into(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let mut mutation = runtime.begin_mutation().await.unwrap();
        mutation.state.status = AgentStatus::AwakeRunning;
        mutation.state.current_run_id = Some("legacy-run-with-no-handle".into());
        mutation
            .persist_state(&runtime.inner.storage)
            .await
            .unwrap();
        drop(mutation);
        let key = ControlKey {
            agent_id: "default".into(),
            identity_incarnation: runtime.inner.identity_incarnation,
            command_id: "stop-absent-run".into(),
        };
        let invocation = ControlInvocation {
            action: ControlAction::Stop,
            source: ControlSource::DirectApi,
            authority_class: AuthorityClass::OperatorInstruction,
        };
        let mut mutation = runtime.begin_mutation().await.unwrap();
        let prepared = runtime
            .prepare_runtime_control(&mutation, key.clone(), invocation)
            .await
            .unwrap();
        let target = prepared
            .command
            .cancellation_targets()
            .iter()
            .find(|target| target.target_kind == ControlCancellationKind::Run)
            .unwrap()
            .clone();
        assert_eq!(target.target_identity, "legacy-run-with-no-handle");
        assert!(target.target_generation["runtime_lease_id"].is_null());
        assert!(target.target_generation["activation_id"].is_null());
        assert!(target.target_generation["source_message_id"].is_null());
        let receipt = mutation.commit_control(&prepared).await.unwrap();
        drop(mutation);
        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.inner.control_cancellations.drain(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            receipt.outcome.aborted_run_id.as_deref(),
            Some("legacy-run-with-no-handle")
        );
        assert_eq!(
            runtime.agent_state().await.unwrap().status,
            AgentStatus::Stopped
        );
        let obligation = runtime
            .inner
            .runtime_db
            .control_obligation(&key, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            obligation.state,
            crate::runtime_db::control_transition::ControlObligationState::NeedsAttention
        );
        assert_eq!(
            obligation.completion_evidence.as_ref().unwrap()["physical_completion_proven"],
            false
        );
    }
}
