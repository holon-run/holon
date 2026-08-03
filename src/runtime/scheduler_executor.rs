use super::message_dispatch::MessageDispatchPlan;
use super::*;
use crate::domain::scheduler_protocol::WaitState;
use crate::types::ExecutionAdmissionProvenance;

pub(super) enum RunLoopPoll {
    Shutdown,
    Stopped(AgentState, usize),
    Message(ScheduledMessage),
    Idle,
}

impl RunLoopPoll {
    fn outcome_name(&self) -> &'static str {
        match self {
            RunLoopPoll::Shutdown => "shutdown",
            RunLoopPoll::Stopped(_, _) => "stopped",
            RunLoopPoll::Message(_) => "message",
            RunLoopPoll::Idle => "idle",
        }
    }
}

pub(super) fn canonical_execution_origin(
    message: &MessageEnvelope,
) -> crate::domain::execution_protocol::ExecutionOrigin {
    use crate::domain::execution_protocol::ExecutionOrigin;
    match message.origin {
        MessageOrigin::Operator { .. } => ExecutionOrigin::Operator,
        MessageOrigin::Channel { .. } => ExecutionOrigin::Channel,
        MessageOrigin::Webhook { .. } => ExecutionOrigin::Webhook,
        MessageOrigin::Callback { .. } => ExecutionOrigin::Callback,
        MessageOrigin::Timer { .. } => ExecutionOrigin::Timer,
        MessageOrigin::System { .. } => ExecutionOrigin::System,
        MessageOrigin::Task { .. } => ExecutionOrigin::Task,
    }
}

pub(super) fn canonical_execution_trust(
    message: &MessageEnvelope,
) -> crate::domain::execution_protocol::ExecutionTrust {
    use crate::domain::execution_protocol::ExecutionTrust;
    match message.authority_class {
        crate::types::AuthorityClass::OperatorInstruction => ExecutionTrust::OperatorInstruction,
        crate::types::AuthorityClass::RuntimeInstruction => ExecutionTrust::RuntimeInstruction,
        crate::types::AuthorityClass::IntegrationSignal => ExecutionTrust::IntegrationSignal,
        crate::types::AuthorityClass::ExternalEvidence => ExecutionTrust::ExternalEvidence,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShutdownReason {
    DaemonShutdown,
}

impl ShutdownReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ShutdownReason::DaemonShutdown => "daemon_shutdown",
        }
    }
}

pub(super) struct ShutdownPostureOutcome {
    pub(super) status: AgentStatus,
    pub(super) current_run_id: Option<String>,
    pub(super) aborted_run_id: Option<String>,
}

pub(super) struct ControlPostureOutcome {
    pub(super) requested_action: ControlAction,
    pub(super) action: ControlAction,
    pub(super) status: AgentStatus,
    pub(super) current_run_id: Option<String>,
    pub(super) aborted_run_id: Option<String>,
    pub(super) occupancy_to_release: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SleepTransitionBoundary {
    LifecycleSleep,
    RunLoopIdle,
}

impl SleepTransitionBoundary {
    fn as_str(self) -> &'static str {
        match self {
            Self::LifecycleSleep => "lifecycle_sleep",
            Self::RunLoopIdle => "run_loop_idle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BootstrapRecoveryFacts {
    pub(super) queued_messages: usize,
}

pub(super) struct ScheduledMessage {
    pub(super) message: MessageEnvelope,
    pub(super) running_state: AgentState,
    pub(super) dispatch_plan: MessageDispatchPlan,
    pub(super) scheduler_decision: scheduler::SchedulerDecision,
}

struct CanonicalClaimPlan {
    activation_id: String,
    scenario_class: crate::domain::scheduler_protocol::SchedulerScenarioClass,
    execution_owner: crate::domain::scheduler_protocol::SchedulerOwner,
    scheduler_claim_work_item: Option<crate::types::WorkItemRecord>,
    bootstrap: Option<crate::domain::scheduler_protocol::Snapshot>,
    commands: Vec<crate::domain::scheduler_protocol::ProtocolCommand>,
    execution_protocol: crate::runtime_db::transitions::ExecutionProtocolTransition,
}

enum CanonicalClaimOutcome {
    ReduceOnly,
    Plan(CanonicalClaimPlan),
    RetainQueued {
        scenario_class: crate::domain::scheduler_protocol::SchedulerScenarioClass,
        reason: &'static str,
    },
    HardBlocker(CanonicalClaimHardBlocker),
}

fn projected_scheduler_dispatch_revision(
    snapshot: &crate::domain::scheduler_protocol::Snapshot,
    commands: &[crate::domain::scheduler_protocol::ProtocolCommand],
) -> Result<u64> {
    let mut projected = snapshot.clone();
    for command in commands {
        let outcome = crate::domain::scheduler_protocol::reduce_command(&projected, command);
        if outcome.outcome.decision == crate::domain::scheduler_protocol::Decision::Rejected {
            let diagnostic = outcome
                .conflict
                .as_ref()
                .map(|conflict| conflict.code.as_str())
                .or_else(|| outcome.outcome.diagnostics.first().map(String::as_str))
                .unwrap_or("rejected_without_diagnostic");
            anyhow::bail!(
                "canonical scheduler claim prefix rejected while projecting dispatch revision: {diagnostic}"
            );
        }
        projected = outcome.outcome.snapshot;
    }
    Ok(projected.dispatch_revision)
}

struct CanonicalClaimHardBlocker {
    scenario_class: crate::domain::scheduler_protocol::SchedulerScenarioClass,
    blocker_code: &'static str,
}

pub(super) struct SchedulerDecisionExecutor<'a> {
    runtime: &'a RuntimeHandle,
}

struct QueueCandidate {
    message: MessageEnvelope,
    prior_state: AgentState,
    queue_len: usize,
}

impl RuntimeHandle {
    pub(super) fn legacy_execution_admission_provenance(
        &self,
        message: &MessageEnvelope,
        continuation_resolution: Option<&ContinuationResolution>,
        task: Option<&TaskRecord>,
    ) -> Result<ExecutionAdmissionProvenance> {
        if !self.inner.scheduler_engine.is_canonical() {
            return Ok(ExecutionAdmissionProvenance::LegacyCompat {
                scenario_class: None,
                effective_mode: crate::domain::scheduler_protocol::ScenarioMode::Off,
            });
        }
        let scenario_class = if matches!(
            message.kind,
            crate::types::MessageKind::TaskStatus | crate::types::MessageKind::TaskResult
        ) && task.is_none()
        {
            None
        } else {
            scheduler::canonical_activation_candidate(message, continuation_resolution, task)?
                .map(|candidate| candidate.scenario_class())
        };
        Ok(ExecutionAdmissionProvenance::LegacyCompat {
            scenario_class,
            effective_mode: crate::domain::scheduler_protocol::ScenarioMode::Off,
        })
    }
}

impl<'a> SchedulerDecisionExecutor<'a> {
    pub(super) fn new(runtime: &'a RuntimeHandle) -> Self {
        Self { runtime }
    }

    pub(super) async fn apply_control(
        &self,
        requested_action: ControlAction,
    ) -> Result<ControlPostureOutcome> {
        let action = requested_action.canonical();
        let mut guard = self.runtime.inner.agent.lock().await;
        let previous_status = guard.state.status.clone();
        let previous_run_id = guard.state.current_run_id.clone();
        let previous_sleeping_until = guard.state.sleeping_until;
        let previous_pending_wake_hint = guard.state.pending_wake_hint.is_some();
        let mut aborted_run_id = None;
        let mut occupancy_to_release = None;

        match action {
            ControlAction::Start => {
                scheduler::apply_start_projection(&mut guard.state);
                scheduler::apply_idle_projection(&mut guard.state, &self.runtime.inner.storage)?;
            }
            ControlAction::Stop => {
                if let Some(handle) = guard.current_run_abort.as_ref() {
                    if let Ok(mut current_reason) = handle.reason.lock() {
                        *current_reason = "agent_stopped".into();
                    }
                    handle.token.cancel();
                    aborted_run_id = Some(handle.run_id.clone());
                }
                occupancy_to_release = guard
                    .state
                    .active_workspace_entry
                    .as_ref()
                    .and_then(|entry| entry.occupancy_id.clone());
                if occupancy_to_release.is_none() {
                    guard.state.active_workspace_entry = None;
                }
                scheduler::apply_stop_projection(&mut guard.state);
            }
        }

        self.append_posture_decision(
            "lifecycle_control",
            match action {
                ControlAction::Start => "start",
                ControlAction::Stop => "stop",
            },
            &previous_status,
            &guard.state.status,
            vec![
                format!("requested_action={requested_action:?}"),
                format!("canonical_action={action:?}"),
                format!("previous_run_id={previous_run_id:?}"),
                format!("next_run_id={:?}", guard.state.current_run_id),
                format!("previous_sleeping_until={previous_sleeping_until:?}"),
                format!("next_sleeping_until={:?}", guard.state.sleeping_until),
                format!("previous_pending_wake_hint={previous_pending_wake_hint}"),
                format!(
                    "next_pending_wake_hint={}",
                    guard.state.pending_wake_hint.is_some()
                ),
                format!("aborted_run_id={aborted_run_id:?}"),
                format!("occupancy_to_release={occupancy_to_release:?}"),
            ],
        )?;
        guard.persist_state(&self.runtime.inner.storage)?;

        Ok(ControlPostureOutcome {
            requested_action,
            action,
            status: guard.state.status.clone(),
            current_run_id: guard.state.current_run_id.clone(),
            aborted_run_id,
            occupancy_to_release,
        })
    }

    pub(super) async fn request_shutdown(
        &self,
        reason: ShutdownReason,
    ) -> Result<ShutdownPostureOutcome> {
        let mut guard = self.runtime.inner.agent.lock().await;
        let mut aborted_run_id = None;
        let mut should_write = false;

        if let Some(handle) = guard.current_run_abort.as_ref() {
            if let Ok(mut current_reason) = handle.reason.lock() {
                *current_reason = reason.as_str().into();
            }
            handle.token.cancel();
            aborted_run_id = Some(handle.run_id.clone());
            if matches!(guard.state.status, AgentStatus::AwakeRunning) {
                scheduler::apply_idle_projection(&mut guard.state, &self.runtime.inner.storage)?;
            } else {
                guard.state.current_run_id = None;
            }
            should_write = true;
        } else if guard.state.current_run_id.is_some() {
            guard.state.current_run_id = None;
            should_write = true;
        }

        if should_write {
            guard.persist_state(&self.runtime.inner.storage)?;
        }

        Ok(ShutdownPostureOutcome {
            status: guard.state.status.clone(),
            current_run_id: guard.state.current_run_id.clone(),
            aborted_run_id,
        })
    }

    pub(super) async fn bootstrap_recovered(&self) -> Result<AgentState> {
        let mut guard = self.runtime.inner.agent.lock().await;
        let facts = BootstrapRecoveryFacts {
            queued_messages: guard.queue.len(),
        };
        if apply_bootstrap_recovered_projection(&mut guard.state, facts) {
            guard.persist_state(&self.runtime.inner.storage)?;
        }
        Ok(guard.state.clone())
    }

    pub(super) async fn transition_to_sleep(
        &self,
        sleeping_until: Option<chrono::DateTime<chrono::Utc>>,
        boundary: SleepTransitionBoundary,
    ) -> Result<AgentState> {
        let mut guard = self.runtime.inner.agent.lock().await;
        let previous_status = guard.state.status.clone();
        let previous_run_id = guard.state.current_run_id.clone();
        scheduler::apply_sleep_projection(&mut guard.state, sleeping_until);
        self.append_posture_decision(
            boundary.as_str(),
            "sleep",
            &previous_status,
            &guard.state.status,
            vec![
                format!("previous_run_id={previous_run_id:?}"),
                format!("sleeping_until={:?}", guard.state.sleeping_until),
            ],
        )?;
        guard.persist_state(&self.runtime.inner.storage)?;
        Ok(guard.state.clone())
    }

    pub(super) async fn transition_run_loop_idle_to_sleep(
        &self,
        sleeping_until: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Option<AgentState>> {
        let mut guard = self.runtime.inner.agent.lock().await;
        if matches!(guard.state.status, AgentStatus::Stopped) || !guard.queue.is_empty() {
            return Ok(None);
        }

        let previous_status = guard.state.status.clone();
        let previous_run_id = guard.state.current_run_id.clone();
        let next_sleeping_until = if matches!(previous_status, AgentStatus::Asleep) {
            match (guard.state.sleeping_until, sleeping_until) {
                (Some(current), Some(proposed)) => Some(current.min(proposed)),
                (Some(current), None) => Some(current),
                (None, proposed) => proposed,
            }
        } else {
            sleeping_until
        };
        scheduler::apply_sleep_projection(&mut guard.state, next_sleeping_until);
        self.append_posture_decision(
            SleepTransitionBoundary::RunLoopIdle.as_str(),
            "sleep",
            &previous_status,
            &guard.state.status,
            vec![
                format!("previous_run_id={previous_run_id:?}"),
                format!("sleeping_until={:?}", guard.state.sleeping_until),
            ],
        )?;
        guard.persist_state(&self.runtime.inner.storage)?;
        Ok(Some(guard.state.clone()))
    }

    pub(super) async fn poll(&self) -> Result<RunLoopPoll> {
        let started_at = std::time::Instant::now();
        let candidate = {
            let guard = self.runtime.inner.agent.lock().await;
            if self.runtime.inner.shutdown_requested.load(Ordering::SeqCst) {
                let poll = self.shutdown(guard)?;
                crate::diagnostics::record_scheduler_poll(
                    poll.outcome_name(),
                    started_at.elapsed(),
                );
                return Ok(poll);
            }
            if guard.state.status == AgentStatus::Stopped {
                let poll = RunLoopPoll::Stopped(guard.state.clone(), guard.queue.len());
                crate::diagnostics::record_scheduler_poll(
                    poll.outcome_name(),
                    started_at.elapsed(),
                );
                return Ok(poll);
            }
            let Some(message) = guard.queue.peek().cloned() else {
                let poll = RunLoopPoll::Idle;
                crate::diagnostics::record_scheduler_poll(
                    poll.outcome_name(),
                    started_at.elapsed(),
                );
                return Ok(poll);
            };
            QueueCandidate {
                message,
                prior_state: guard.state.clone(),
                queue_len: guard.queue.len(),
            }
        };

        let poll = self.prepare_message(candidate).await?;
        crate::diagnostics::record_scheduler_poll(poll.outcome_name(), started_at.elapsed());
        Ok(poll)
    }

    fn shutdown(
        &self,
        mut guard: tokio::sync::MutexGuard<'_, RuntimeAgent>,
    ) -> Result<RunLoopPoll> {
        guard.state.current_run_id = None;
        guard.persist_state(&self.runtime.inner.storage)?;
        Ok(RunLoopPoll::Shutdown)
    }

    async fn prepare_message(&self, candidate: QueueCandidate) -> Result<RunLoopPoll> {
        let prior_closure = self
            .runtime
            .closure_decision_for_state(&candidate.prior_state, None)
            .await?;
        let mut dispatch_plan = self.runtime.build_message_dispatch_plan(
            &candidate.message,
            prior_closure,
            &candidate.prior_state,
        )?;
        let projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
            &self.runtime.inner.storage,
            &candidate.prior_state,
            candidate.queue_len,
            self.runtime.now(),
        )?;
        let projection = if self.runtime.inner.scheduler_engine.is_canonical() {
            projection
        } else {
            projection.without_canonical_authority()
        };
        let legacy_decision = scheduler::decide_next_action(
            &projection,
            scheduler::SchedulerBoundary::RunLoop,
            scheduler::SchedulerInput::Message {
                message: &candidate.message,
                model_turn_allowed: dispatch_plan.model_turn_allowed,
                continuation_resolution: dispatch_plan.continuation_resolution.as_ref(),
            },
        );
        let scheduler_decision_events =
            scheduler::scheduler_decision_events(&candidate.message.agent_id, &legacy_decision)?;
        let persisted_message = self
            .runtime
            .inner
            .storage
            .read_message_by_id(&candidate.message.id)?
            .ok_or_else(|| anyhow!("claimed message is missing persisted ingress evidence"))?;
        let canonical_claim = if self.runtime.inner.scheduler_engine.is_canonical() {
            match self.canonical_activation_plan(
                &projection,
                &persisted_message,
                &dispatch_plan,
                legacy_decision.model_reentry,
            ) {
                Ok(CanonicalClaimOutcome::ReduceOnly) => None,
                Ok(CanonicalClaimOutcome::Plan(plan)) => Some(plan),
                Ok(CanonicalClaimOutcome::RetainQueued {
                    scenario_class,
                    reason,
                }) => {
                    self.runtime
                        .inner
                        .storage
                        .append_event(&AuditEvent::legacy(
                            "scheduler_authority_input_rejected",
                            serde_json::json!({
                                "message_id": persisted_message.id,
                                "agent_id": persisted_message.agent_id,
                                "scenario_class": scenario_class.as_str(),
                                "reason": reason,
                                "queue_disposition": "retained_queued",
                            }),
                        ))?;
                    self.runtime.inner.notify.notify_one();
                    return Ok(RunLoopPoll::Idle);
                }
                Ok(CanonicalClaimOutcome::HardBlocker(blocker)) => {
                    self.report_canonical_claim_hard_blocker(&persisted_message, blocker)?;
                    return Ok(RunLoopPoll::Idle);
                }
                Err(error) => {
                    if let Some(ambiguous) =
                        error.downcast_ref::<scheduler::AmbiguousCanonicalWaits>()
                    {
                        scheduler::append_ambiguous_wait_advisory(
                            &self.runtime.inner.storage,
                            &persisted_message,
                            &ambiguous.wait_condition_ids,
                        )?;
                        scheduler::append_scheduling_advisories(
                            &self.runtime.inner.storage,
                            &candidate.prior_state,
                            candidate.queue_len,
                        )?;
                        return Ok(RunLoopPoll::Idle);
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };
        if let Some(plan) = canonical_claim.as_ref() {
            dispatch_plan.execution_admission_provenance =
                ExecutionAdmissionProvenance::Canonical {
                    scenario_class: plan.scenario_class,
                    activation_id: plan.activation_id.clone(),
                };
        }
        let effective_decision = canonical_claim
            .as_ref()
            .map(|plan| {
                let mut decision = scheduler::SchedulerDecision::new(
                    scheduler::SchedulerDecisionKind::StartModelTurn,
                    "canonical_activation_admitted",
                )
                .message(&persisted_message)
                .model_reentry(true)
                .evidence(format!("canonical_activation={}", plan.activation_id));
                if let Some(work_item_id) = plan.execution_owner.work_item_id() {
                    decision = decision.work_item_id(work_item_id);
                }
                decision
            })
            .unwrap_or(legacy_decision);
        scheduler::append_scheduling_advisories(
            &self.runtime.inner.storage,
            &candidate.prior_state,
            candidate.queue_len,
        )?;

        let (message, running_state, transition_commit) = {
            let queue_record = QueueEntryRecord {
                message_id: candidate.message.id.clone(),
                agent_id: candidate.message.agent_id.clone(),
                priority: candidate.message.priority.clone(),
                status: QueueEntryStatus::Dequeued,
                created_at: candidate.message.created_at,
                updated_at: Utc::now(),
            };
            let run_id = crate::ids::run_id();
            let abort_token = CancellationToken::new();
            let claim_audit_events = vec![
                scheduler_decision_events[0].clone(),
                scheduler_decision_events[1].clone(),
                AuditEvent::legacy(
                    "queue_entry_claimed",
                    serde_json::json!({
                        "message_id": queue_record.message_id,
                        "agent_id": queue_record.agent_id,
                        "status": QueueEntryStatus::Dequeued,
                        "run_id": run_id,
                    }),
                ),
            ];
            let agent_id = candidate.message.agent_id.clone();
            let mut attempt = 0;
            loop {
                if attempt >= super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
                    return Err(anyhow!("claim OCC retry exhausted for agent {}", agent_id));
                }
                let mut guard = self.runtime.inner.agent.lock().await;
                if self.runtime.inner.shutdown_requested.load(Ordering::SeqCst) {
                    return self.shutdown(guard);
                }
                if matches!(guard.state.status, AgentStatus::Stopped) {
                    return Ok(RunLoopPoll::Stopped(guard.state.clone(), guard.queue.len()));
                }
                if !guard
                    .queue
                    .peek()
                    .is_some_and(|message| message.id == candidate.message.id)
                {
                    return Ok(RunLoopPoll::Idle);
                }
                let mut running_state = guard.state.clone();
                running_state.pending = guard.queue.len().saturating_sub(1);
                scheduler::apply_running_projection(&mut running_state, run_id.clone());
                running_state.last_wake_reason = Some(format!("{:?}", candidate.message.kind));
                let execution_protocol = canonical_claim
                    .as_ref()
                    .map(|plan| plan.execution_protocol.clone())
                    .unwrap_or_default();
                let commit_result = self
                    .runtime
                    .inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_execution_protocol(
                        &crate::runtime_db::transitions::QueueTransitionCommand {
                            agent_id: agent_id.clone(),
                            operation: crate::runtime_db::transitions::QueueOperation::Claim,
                            mutation: crate::runtime_db::transitions::QueueMutation::Consume(
                                queue_record.clone(),
                            ),
                            scheduler_claim_work_item: canonical_claim
                                .as_ref()
                                .and_then(|plan| plan.scheduler_claim_work_item.clone()),
                            scheduler_protocol_bootstrap: canonical_claim
                                .as_ref()
                                .and_then(|plan| plan.bootstrap.clone()),
                            scheduler_protocol_commands: canonical_claim
                                .as_ref()
                                .map(|plan| plan.commands.clone())
                                .unwrap_or_default(),
                            agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                                expected: Some(Box::new(guard.state.clone())),
                                record: Box::new(running_state.clone()),
                            }),
                            message_evidence: Vec::new(),
                            transcript_entries: Vec::new(),
                            turn_record: None,
                            audit_events: claim_audit_events.clone(),
                            notify_scheduler: false,
                            fault: self.runtime.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        },
                        &execution_protocol,
                    );
                let mut commit = match commit_result {
                    Ok(commit) => commit,
                    Err(error) => {
                        let can_retry = attempt + 1 < super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                            && super::retryable_enqueue_agent_state_conflict(&error, &agent_id);
                        if !can_retry {
                            return Err(error);
                        }
                        drop(guard);
                        if !self
                            .runtime
                            .refresh_enqueue_agent_state_baseline(&agent_id)
                            .await?
                        {
                            return Err(error);
                        }
                        attempt += 1;
                        continue;
                    }
                };
                if commit.scheduler_authority_blocked {
                    commit.effects.agent_state = None;
                    drop(guard);
                    self.runtime.apply_transition_commit(commit).await;
                    return Ok(RunLoopPoll::Idle);
                }
                if !commit.applied {
                    let _ = guard.queue.pop_if_next(&candidate.message.id);
                    guard.state.pending = guard.queue.len();
                    guard.persist_state(&self.runtime.inner.storage)?;
                    return Ok(RunLoopPoll::Idle);
                }
                let queued_message = guard
                    .queue
                    .pop_if_next(&candidate.message.id)
                    .expect("queue head was just checked");
                debug_assert_eq!(queued_message.id, persisted_message.id);
                guard.state = running_state.clone();
                guard.last_persisted_state = running_state.clone();
                guard.current_run_abort = Some(CurrentRunAbortHandle {
                    run_id: run_id.clone(),
                    token: abort_token,
                    reason: Arc::new(StdMutex::new("operator_aborted".into())),
                });
                commit.effects.agent_state = None;
                break (persisted_message.clone(), running_state, commit);
            }
        };
        self.runtime
            .apply_transition_commit(transition_commit)
            .await;

        Ok(RunLoopPoll::Message(ScheduledMessage {
            message,
            running_state,
            dispatch_plan,
            scheduler_decision: effective_decision,
        }))
    }

    fn canonical_activation_plan(
        &self,
        projection: &scheduler::SchedulerProjection,
        message: &MessageEnvelope,
        dispatch_plan: &MessageDispatchPlan,
        model_reentry: bool,
    ) -> Result<CanonicalClaimOutcome> {
        let task = match &dispatch_plan.task {
            Ok(task) => task.as_ref(),
            Err(_) => {
                return Ok(CanonicalClaimOutcome::ReduceOnly);
            }
        };
        let Some(candidate) = scheduler::canonical_activation_candidate(
            message,
            dispatch_plan.continuation_resolution.as_ref(),
            task,
        )?
        else {
            return Ok(if model_reentry {
                CanonicalClaimOutcome::RetainQueued {
                    scenario_class:
                        crate::domain::scheduler_protocol::SchedulerScenarioClass::ReducerOnlyCandidates,
                    reason: "canonical_model_reentry_candidate_unclassified",
                }
            } else {
                CanonicalClaimOutcome::ReduceOnly
            });
        };
        let scenario_class = candidate.scenario_class();
        if let scheduler::CanonicalActivationCandidate::ExactTaskRejoin { task_id, .. } = &candidate
        {
            let durable_task = self.runtime.inner.storage.latest_task_record(task_id)?;
            if durable_task
                .as_ref()
                .and_then(|task| tasks::task_rejoin_fence(task).ok())
                .is_none()
            {
                return Ok(canonical_claim_hard_blocker(
                    scenario_class,
                    "canonical_task_rejoin_contract_missing_or_invalid",
                ));
            }
        }
        let Some(mut scenario) =
            scheduler::resolve_canonical_activation_scenario(projection, message, candidate)?
        else {
            return Ok(canonical_claim_hard_blocker(
                scenario_class,
                "canonical_activation_scenario_unresolved",
            ));
        };

        let existing = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot_if_initialized(&message.agent_id)?;
        let existing_execution = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&message.agent_id)?;
        if scenario.work_item_id().is_none() {
            return self.plan_canonical_lifecycle_activation_claim(
                message,
                scenario,
                existing,
                scenario_class,
            );
        }
        if let scheduler::CanonicalActivationScenario::ExactTaskRejoin {
            work_item_id,
            wait_id,
            ..
        } = &mut scenario
        {
            if wait_id.is_none() {
                let matching_waits = self
                    .runtime
                    .inner
                    .storage
                    .latest_wait_conditions()?
                    .into_iter()
                    .filter(|condition| {
                        condition.agent_id == message.agent_id
                            && condition.work_item_id.as_deref() == Some(work_item_id.as_str())
                            && condition.status == crate::types::WaitConditionStatus::Resolved
                            && condition.kind == crate::types::WaitConditionKind::Task
                            && scheduler::message_matches_wait_condition(message, condition)
                    })
                    .map(|condition| condition.id)
                    .collect::<Vec<_>>();
                if let [authoritative_wait_id] = matching_waits.as_slice() {
                    *wait_id = Some(authoritative_wait_id.clone());
                }
            }
        }

        let work_item_id = scenario
            .work_item_id()
            .expect("WorkItem scenario has a WorkItem owner");
        let work_item = self
            .runtime
            .inner
            .storage
            .latest_work_item(work_item_id)?
            .ok_or_else(|| anyhow!("canonical activation references unknown WorkItem"));
        let work_item = match work_item {
            Ok(work_item) => work_item,
            Err(_) => {
                if matches!(
                    scenario,
                    scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput { .. }
                ) {
                    return Ok(CanonicalClaimOutcome::RetainQueued {
                        scenario_class,
                        reason: "explicit_binding_work_item_missing",
                    });
                }
                return Ok(canonical_claim_hard_blocker(
                    scenario_class,
                    "canonical_work_item_missing",
                ));
            }
        };
        let work_queue = self.runtime.inner.storage.work_queue_prompt_projection()?;
        let work_projection = work_queue
            .items
            .iter()
            .find(|candidate| candidate.id == work_item.id)
            .ok_or_else(|| anyhow!("canonical activation has no WorkItem scheduling projection"));
        let work_projection = match work_projection {
            Ok(work_projection) => work_projection,
            Err(_) => {
                return Ok(canonical_claim_hard_blocker(
                    scenario_class,
                    "canonical_work_item_projection_missing",
                ));
            }
        };
        if work_item.agent_id != message.agent_id
            || work_item.state != crate::types::WorkItemState::Open
        {
            return Ok(canonical_claim_hard_blocker(
                scenario_class,
                "canonical_work_item_not_open_same_agent",
            ));
        }
        if matches!(
            scenario,
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
        ) && work_projection.scheduling_state != crate::types::WorkItemSchedulingState::Runnable
        {
            return Ok(canonical_claim_hard_blocker(
                scenario_class,
                "canonical_autonomous_work_item_not_runnable",
            ));
        }
        let activation_id = canonical_activation_id_for_attempt(existing.as_ref(), &message.id);

        use crate::domain::scheduler_protocol::{
            ActivationBinding, ActivationCause, ActivationLifecycleState, ActivationOrigin,
            ActivationPriority, ActivationProvenance, ActivationSlot, ActivationTrust,
            AdmitActivationCommand, AdoptLegacyWorkStateCommand, AgentActivation,
            AgentDispatchState, LegacyWaitAdoption, PreemptionPolicy, ProtocolCommand,
            RegisterWorkDemandCommand, Snapshot, TriggerWaitCommand, WaitResumeClaim, WaitState,
            WorkDemand, WorkStatus,
        };

        if let Some(snapshot) = existing.as_ref() {
            if let Some(activation) = snapshot.activations.get(&activation_id) {
                if activation.state == crate::domain::scheduler_protocol::ActivationState::Running
                    && activation.owner.work_item_id() == Some(work_item_id)
                    && snapshot
                        .activation_admissions
                        .get(&activation_id)
                        .is_some_and(|admission| {
                            canonical_admission_matches_scenario(admission, message, &scenario)
                        })
                {
                    return Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
                        activation_id,
                        scenario_class,
                        execution_owner: activation.owner.clone(),
                        scheduler_claim_work_item: matches!(
                            scenario,
                            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation {
                                ..
                            }
                        )
                        .then_some(work_item),
                        bootstrap: None,
                        commands: Vec::new(),
                        execution_protocol:
                            crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
                    }));
                }
                return Ok(canonical_claim_hard_blocker(
                    scenario_class,
                    "canonical_activation_replay_conflict",
                ));
            }
        }
        let authoritative_work = existing_execution
            .as_ref()
            .and_then(|state| state.work_items.get(work_item_id));
        let scheduling_generation =
            authoritative_work.map_or(work_item.revision.max(1), |record| record.generation());
        let wait_id = match &scenario {
            scheduler::CanonicalActivationScenario::ExactTaskRejoin { wait_id, .. }
            | scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                wait_id,
                ..
            } => wait_id.as_deref(),
            scheduler::CanonicalActivationScenario::ExactWaitResume { wait_id, .. } => {
                Some(wait_id.as_str())
            }
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
            | scheduler::CanonicalActivationScenario::InternalFollowup { .. } => None,
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. } => {
                unreachable!("lifecycle scenario is planned before WorkItem lookup")
            }
        };
        let compatibility_wait = if let Some(wait_id) = wait_id {
            if let Some(authoritative_work) = authoritative_work {
                let Some(authoritative_wait) = (match &authoritative_work.state {
                    crate::domain::execution_protocol::WorkItemExecutionState::Waiting {
                        generation,
                        wait,
                    } if generation == &scheduling_generation && wait.wait_id == wait_id => {
                        Some(wait)
                    }
                    _ => None,
                }) else {
                    return Ok(canonical_claim_hard_blocker(
                        scenario_class,
                        "canonical_wait_execution_authority_mismatch",
                    ));
                };
                if authoritative_wait.generation != scheduling_generation {
                    return Ok(canonical_claim_hard_blocker(
                        scenario_class,
                        "canonical_wait_generation_authority_mismatch",
                    ));
                }
            }
            let wait_condition = self
                .runtime
                .inner
                .storage
                .latest_wait_conditions()?
                .into_iter()
                .find(|condition| {
                    condition.id == wait_id
                        && condition.agent_id == message.agent_id
                        && condition.work_item_id.as_deref() == Some(work_item_id)
                })
                .ok_or_else(|| anyhow!("canonical wait resume references unknown durable wait"))?;
            Some(LegacyWaitAdoption {
                wait_id: wait_id.to_string(),
                generation: scheduling_generation,
                owner_work_item_id: work_item_id.to_string(),
                source_updated_at: crate::runtime_db::repositories::timestamp(
                    wait_condition.updated_at,
                ),
            })
        } else {
            None
        };
        let demand = WorkDemand {
            metadata_revision: work_item.revision.max(1),
            scheduling_generation,
            status: compatibility_wait
                .as_ref()
                .map_or(WorkStatus::Runnable, |wait| WorkStatus::Waiting {
                    wait_id: wait.wait_id.clone(),
                }),
            capabilities: Default::default(),
            locks: Default::default(),
            locality: "runtime".into(),
            cost_class: "default".into(),
        };
        let (bootstrap, register) = if let Some(snapshot) = existing.as_ref() {
            let wait_projection_matches = compatibility_wait.as_ref().is_none_or(|wait| {
                snapshot
                    .waits
                    .get(&wait.wait_id)
                    .filter(|record| record.current_generation == wait.generation)
                    .and_then(|record| record.generations.get(&wait.generation))
                    .is_some_and(|generation| {
                        generation.owner.work_item_id() == Some(work_item_id)
                            && generation.state == WaitState::Active
                    })
            });
            match snapshot.work.get(work_item_id) {
                Some(existing_demand) if existing_demand == &demand && wait_projection_matches => {
                    (None, None)
                }
                Some(_) | None if compatibility_wait.is_some() => (
                    None,
                    Some(ProtocolCommand::AdoptLegacyWorkState(
                        AdoptLegacyWorkStateCommand {
                            work_item_id: work_item.id.clone(),
                            source_work_item_revision: work_item.revision.max(1),
                            demand: demand.clone(),
                            wait: compatibility_wait.clone(),
                            focus: false,
                            reserve_dispatch: false,
                            replace_completed_focus: None,
                        },
                    )),
                ),
                Some(_) => (
                    None,
                    Some(ProtocolCommand::AdoptLegacyWorkState(
                        AdoptLegacyWorkStateCommand {
                            work_item_id: work_item.id.clone(),
                            source_work_item_revision: work_item.revision.max(1),
                            demand: demand.clone(),
                            wait: None,
                            focus: false,
                            reserve_dispatch: false,
                            replace_completed_focus: None,
                        },
                    )),
                ),
                None => (
                    None,
                    Some(ProtocolCommand::RegisterWorkDemand(
                        RegisterWorkDemandCommand {
                            work_item_id: work_item.id.clone(),
                            demand: demand.clone(),
                        },
                    )),
                ),
            }
        } else {
            let bootstrap = Snapshot {
                slot: ActivationSlot::Idle,
                dispatch: AgentDispatchState::Open,
                dispatch_revision: 0,
                focus: None,
                work: Default::default(),
                waits: Default::default(),
                activations: Default::default(),
                activation_admissions: Default::default(),
                settlements: Default::default(),
                missing_settlements: Default::default(),
                admitted_generations: Default::default(),
                continuation_admissions: Default::default(),
                activation_inputs: Default::default(),
            };
            let register = compatibility_wait.as_ref().map_or_else(
                || {
                    ProtocolCommand::RegisterWorkDemand(RegisterWorkDemandCommand {
                        work_item_id: work_item.id.clone(),
                        demand: demand.clone(),
                    })
                },
                |_| {
                    ProtocolCommand::AdoptLegacyWorkState(AdoptLegacyWorkStateCommand {
                        work_item_id: work_item.id.clone(),
                        source_work_item_revision: work_item.revision.max(1),
                        demand: demand.clone(),
                        wait: compatibility_wait.clone(),
                        focus: false,
                        reserve_dispatch: false,
                        replace_completed_focus: None,
                    })
                },
            );
            (Some(bootstrap), Some(register))
        };

        let resume = if let Some(wait) = compatibility_wait.as_ref() {
            let Some(trigger_generation) = message.message_seq else {
                return Ok(canonical_claim_hard_blocker(
                    scenario_class,
                    "canonical_trigger_sequence_missing",
                ));
            };
            Some(WaitResumeClaim {
                wait_id: wait.wait_id.clone(),
                wait_generation: wait.generation,
                trigger_id: canonical_wait_trigger_id(message),
                trigger_generation,
            })
        } else {
            None
        };
        let (cause, binding, provenance_origin, provenance_trust, idempotency_key) = match &scenario
        {
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. } => (
                ActivationCause::WorkItemRunnable {
                    work_item_id: work_item.id.clone(),
                    scheduling_generation,
                },
                ActivationBinding::WorkItem {
                    work_item_id: work_item.id.clone(),
                },
                ActivationOrigin::System,
                ActivationTrust::RuntimeInstruction,
                format!("work-queue-attempt:{activation_id}"),
            ),
            scheduler::CanonicalActivationScenario::InternalFollowup { .. } => (
                ActivationCause::InternalFollowup {
                    message_id: message.id.clone(),
                },
                ActivationBinding::WorkItem {
                    work_item_id: work_item.id.clone(),
                },
                ActivationOrigin::System,
                ActivationTrust::RuntimeInstruction,
                format!("internal-followup:{activation_id}"),
            ),
            scheduler::CanonicalActivationScenario::ExactTaskRejoin { task_id, .. } => (
                ActivationCause::TaskRejoin {
                    task_id: task_id.clone(),
                    message_id: message.id.clone(),
                    resume: resume.clone(),
                },
                ActivationBinding::WorkItem {
                    work_item_id: work_item.id.clone(),
                },
                ActivationOrigin::Task,
                ActivationTrust::RuntimeInstruction,
                format!("task-rejoin:{task_id}:{activation_id}"),
            ),
            scheduler::CanonicalActivationScenario::ExactWaitResume { wait_id, .. } => {
                let resume = resume
                    .as_ref()
                    .expect("exact wait resume has a canonical wait claim");
                (
                    ActivationCause::WaitResume {
                        wait_id: wait_id.clone(),
                        wait_generation: resume.wait_generation,
                        trigger_id: resume.trigger_id.clone(),
                        trigger_generation: resume.trigger_generation,
                    },
                    ActivationBinding::WaitOwner {
                        wait_id: wait_id.clone(),
                        owner: crate::domain::scheduler_protocol::SchedulerOwner::WorkItem {
                            work_item_id: work_item.id.clone(),
                        },
                    },
                    canonical_activation_origin(message),
                    canonical_activation_trust(message),
                    format!(
                        "wait-resume:{}:{}:{activation_id}",
                        wait_id, resume.wait_generation
                    ),
                )
            }
            scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput { .. } => (
                ActivationCause::OperatorInput {
                    message_id: message.id.clone(),
                    resume: resume.clone(),
                },
                ActivationBinding::WorkItem {
                    work_item_id: work_item.id.clone(),
                },
                ActivationOrigin::Operator,
                ActivationTrust::OperatorInstruction,
                format!("operator-message:{activation_id}"),
            ),
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. } => {
                unreachable!("lifecycle scenario is planned before WorkItem lookup")
            }
        };
        let activation = AgentActivation {
            id: activation_id.clone(),
            agent_id: message.agent_id.clone(),
            state: ActivationLifecycleState::Admitted,
            cause,
            binding,
            priority: match message.priority {
                Priority::Interject => ActivationPriority::Interject,
                Priority::Next => ActivationPriority::Next,
                Priority::Normal => ActivationPriority::Normal,
                Priority::Background => ActivationPriority::Background,
            },
            preemption: PreemptionPolicy::AllowOperatorInterjection,
            source_revision: Some(work_item.revision),
            idempotency_key,
            provenance: ActivationProvenance {
                origin: provenance_origin,
                trust: provenance_trust,
                source_id: message.id.clone(),
                correlation_id: message.correlation_id.clone(),
                causation_id: message.causation_id.clone(),
            },
        };
        let mut commands = Vec::with_capacity(4);
        commands.extend(register);
        if let Some(resume) = resume.as_ref() {
            commands.push(ProtocolCommand::TriggerWait(TriggerWaitCommand {
                wait_id: resume.wait_id.clone(),
                wait_generation: resume.wait_generation,
                trigger_id: resume.trigger_id.clone(),
                trigger_generation: resume.trigger_generation,
            }));
        }
        let expected_dispatch_revision = projected_scheduler_dispatch_revision(
            existing
                .as_ref()
                .or(bootstrap.as_ref())
                .expect("canonical claim has existing or bootstrap scheduler state"),
            &commands,
        )?;
        commands.push(ProtocolCommand::AdmitActivation(AdmitActivationCommand {
            authority_id: format!("authority:{activation_id}"),
            activation,
            expected_scheduling_generation: scheduling_generation,
            expected_dispatch_revision,
        }));
        let execution_protocol = self.plan_execution_protocol_claim(
            message,
            &scenario,
            &activation_id,
            Some((&work_item, scheduling_generation)),
            resume.as_ref(),
        )?;

        Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
            activation_id,
            scenario_class,
            execution_owner: crate::domain::scheduler_protocol::SchedulerOwner::WorkItem {
                work_item_id: work_item_id.to_string(),
            },
            scheduler_claim_work_item: matches!(
                scenario,
                scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
                    | scheduler::CanonicalActivationScenario::InternalFollowup { .. }
            )
            .then_some(work_item),
            bootstrap,
            commands,
            execution_protocol,
        }))
    }

    fn plan_canonical_lifecycle_activation_claim(
        &self,
        message: &MessageEnvelope,
        scenario: scheduler::CanonicalActivationScenario,
        existing: Option<crate::domain::scheduler_protocol::Snapshot>,
        scenario_class: crate::domain::scheduler_protocol::SchedulerScenarioClass,
    ) -> Result<CanonicalClaimOutcome> {
        use crate::domain::scheduler_protocol::{
            ActivationBinding, ActivationCause, ActivationLifecycleState, ActivationPriority,
            ActivationProvenance, ActivationSlot, AdmitActivationCommand, AgentActivation,
            AgentDispatchState, PreemptionPolicy, ProtocolCommand, SchedulerOwner, Snapshot,
            TriggerWaitCommand, WaitResumeClaim,
        };

        let owner = SchedulerOwner::AgentLifecycle {
            agent_id: message.agent_id.clone(),
        };
        let activation_id = canonical_activation_id_for_attempt(existing.as_ref(), &message.id);
        if let Some(snapshot) = existing.as_ref() {
            if let Some(activation) = snapshot.activations.get(&activation_id) {
                if activation.state == crate::domain::scheduler_protocol::ActivationState::Running
                    && activation.owner == owner
                    && snapshot
                        .activation_admissions
                        .get(&activation_id)
                        .is_some_and(|admission| {
                            canonical_admission_matches_scenario(admission, message, &scenario)
                        })
                {
                    return Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
                        activation_id,
                        scenario_class,
                        execution_owner: activation.owner.clone(),
                        scheduler_claim_work_item: None,
                        bootstrap: None,
                        commands: Vec::new(),
                        execution_protocol:
                            crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
                    }));
                }
                return Ok(canonical_claim_hard_blocker(
                    scenario_class,
                    "canonical_activation_replay_conflict",
                ));
            }
        }

        let bootstrap = existing.is_none().then(|| Snapshot {
            slot: ActivationSlot::Idle,
            dispatch: AgentDispatchState::Open,
            dispatch_revision: 0,
            focus: None,
            work: Default::default(),
            waits: Default::default(),
            activations: Default::default(),
            activation_admissions: Default::default(),
            settlements: Default::default(),
            missing_settlements: Default::default(),
            admitted_generations: Default::default(),
            continuation_admissions: Default::default(),
            activation_inputs: Default::default(),
        });
        let (expected_generation, scheduler_resume, execution_resume, cause, idempotency_key) =
            match &scenario {
                scheduler::CanonicalActivationScenario::ExactWaitResume {
                    owner: expected_owner,
                    wait_id,
                } if expected_owner == &owner => {
                    self.runtime
                        .inner
                        .storage
                        .latest_wait_conditions()?
                        .into_iter()
                        .find(|condition| {
                            condition.id == *wait_id
                                && condition.agent_id == message.agent_id
                                && condition.work_item_id.is_none()
                                && condition.status == crate::types::WaitConditionStatus::Active
                                && scheduler::message_matches_wait_condition(message, condition)
                        })
                        .ok_or_else(|| {
                            anyhow!("canonical lifecycle wait is not durable and active")
                        })?;
                    let Some(trigger_generation) = message.message_seq else {
                        return Ok(canonical_claim_hard_blocker(
                            scenario_class,
                            "canonical_trigger_sequence_missing",
                        ));
                    };
                    let authoritative_wait_generation = message
                        .source_refs
                        .get("wait_generation")
                        .and_then(|generation| generation.parse::<u64>().ok())
                        .filter(|generation| *generation > 0)
                        .or_else(|| {
                            existing
                                .as_ref()
                                .and_then(|snapshot| snapshot.waits.get(wait_id))
                                .map(|wait| wait.current_generation)
                        })
                        .unwrap_or(1);
                    let admission_generation = existing.as_ref().map_or(1, |snapshot| {
                        snapshot
                            .activations
                            .values()
                            .filter(|activation| activation.owner == owner)
                            .map(|activation| activation.admitted_generation)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(1)
                    });
                    let execution_resume = WaitResumeClaim {
                        wait_id: wait_id.clone(),
                        wait_generation: authoritative_wait_generation,
                        trigger_id: canonical_wait_trigger_id(message),
                        trigger_generation,
                    };
                    let scheduler_resume = existing.as_ref().and_then(|snapshot| {
                        let wait = snapshot.waits.get(wait_id)?;
                        let generation = wait.generations.get(&wait.current_generation)?;
                        (wait.current_generation == authoritative_wait_generation
                            && generation.owner == owner
                            && generation.state == WaitState::Active)
                            .then(|| execution_resume.clone())
                    });
                    let (cause, idempotency_key) = scheduler_resume.as_ref().map_or_else(
                        || {
                            (
                                ActivationCause::LifecycleExternalNudge {
                                    message_id: message.id.clone(),
                                },
                                format!("lifecycle-wait-message:{activation_id}"),
                            )
                        },
                        |resume| {
                            (
                                ActivationCause::WaitResume {
                                    wait_id: wait_id.clone(),
                                    wait_generation: resume.wait_generation,
                                    trigger_id: resume.trigger_id.clone(),
                                    trigger_generation: resume.trigger_generation,
                                },
                                format!(
                                    "wait-resume:{}:{}:{activation_id}",
                                    wait_id, resume.wait_generation
                                ),
                            )
                        },
                    );
                    (
                        admission_generation,
                        scheduler_resume,
                        Some(execution_resume),
                        cause,
                        idempotency_key,
                    )
                }
                scheduler::CanonicalActivationScenario::LifecycleExternalNudge { agent_id }
                    if agent_id == &message.agent_id =>
                {
                    let generation = existing.as_ref().map_or(1, |snapshot| {
                        let activation_generation = snapshot
                            .activations
                            .values()
                            .filter(|activation| activation.owner == owner)
                            .map(|activation| activation.admitted_generation)
                            .max()
                            .unwrap_or(0);
                        let wait_generation = snapshot
                            .waits
                            .values()
                            .filter(|wait| {
                                wait.generations
                                    .get(&wait.current_generation)
                                    .is_some_and(|generation| generation.owner == owner)
                            })
                            .map(|wait| wait.current_generation)
                            .max()
                            .unwrap_or(0);
                        activation_generation.max(wait_generation).saturating_add(1)
                    });
                    (
                        generation,
                        None,
                        None,
                        ActivationCause::LifecycleExternalNudge {
                            message_id: message.id.clone(),
                        },
                        format!("lifecycle-message:{activation_id}"),
                    )
                }
                _ => {
                    return Ok(canonical_claim_hard_blocker(
                        scenario_class,
                        "canonical_lifecycle_binding_mismatch",
                    ))
                }
            };
        let activation = AgentActivation {
            id: activation_id.clone(),
            agent_id: message.agent_id.clone(),
            state: ActivationLifecycleState::Admitted,
            cause,
            binding: ActivationBinding::Lifecycle {
                agent_id: message.agent_id.clone(),
            },
            priority: match message.priority {
                Priority::Interject => ActivationPriority::Interject,
                Priority::Next => ActivationPriority::Next,
                Priority::Normal => ActivationPriority::Normal,
                Priority::Background => ActivationPriority::Background,
            },
            preemption: PreemptionPolicy::AllowOperatorInterjection,
            source_revision: None,
            idempotency_key,
            provenance: ActivationProvenance {
                origin: canonical_activation_origin(message),
                trust: canonical_activation_trust(message),
                source_id: message.id.clone(),
                correlation_id: message.correlation_id.clone(),
                causation_id: message.causation_id.clone(),
            },
        };
        let mut commands = Vec::with_capacity(3);
        if let Some(resume) = scheduler_resume.as_ref() {
            commands.push(ProtocolCommand::TriggerWait(TriggerWaitCommand {
                wait_id: resume.wait_id.clone(),
                wait_generation: resume.wait_generation,
                trigger_id: resume.trigger_id.clone(),
                trigger_generation: resume.trigger_generation,
            }));
        }
        let expected_dispatch_revision = projected_scheduler_dispatch_revision(
            existing
                .as_ref()
                .or(bootstrap.as_ref())
                .expect("canonical claim has existing or bootstrap scheduler state"),
            &commands,
        )?;
        commands.push(ProtocolCommand::AdmitActivation(AdmitActivationCommand {
            authority_id: format!("authority:{activation_id}"),
            activation,
            expected_scheduling_generation: expected_generation,
            expected_dispatch_revision,
        }));
        let execution_protocol = self.plan_execution_protocol_claim(
            message,
            &scenario,
            &activation_id,
            None,
            execution_resume.as_ref(),
        )?;
        Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
            activation_id,
            scenario_class,
            execution_owner: owner,
            scheduler_claim_work_item: None,
            bootstrap,
            commands,
            execution_protocol,
        }))
    }

    fn plan_execution_protocol_claim(
        &self,
        message: &MessageEnvelope,
        scenario: &scheduler::CanonicalActivationScenario,
        attempt_id: &str,
        work_item: Option<(&crate::types::WorkItemRecord, u64)>,
        resume: Option<&crate::domain::scheduler_protocol::WaitResumeClaim>,
    ) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
        use crate::domain::execution_protocol::{
            AdmitExecution, AdmittedFences, ExecutionAttempt, ExecutionAttemptState,
            ExecutionBinding, ExecutionPriority, ExecutionProtocolCommand, ExecutionProtocolState,
            ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity,
            RegisterWorkItemExecution, WorkItemExecutionRecord, WorkItemExecutionState,
        };

        let existing = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&message.agent_id)?;
        if existing
            .as_ref()
            .is_some_and(|state| state.attempts.contains_key(attempt_id))
        {
            return Ok(crate::runtime_db::transitions::ExecutionProtocolTransition::default());
        }
        let source_revision = message
            .message_seq
            .filter(|revision| *revision > 0)
            .ok_or_else(|| anyhow!("canonical execution admission requires message sequence"))?;
        let authority_fences = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_authority_fences(&message.agent_id)?;
        let rejoin = match scenario {
            scheduler::CanonicalActivationScenario::ExactTaskRejoin { task_id, .. } => {
                let task = self
                    .runtime
                    .inner
                    .storage
                    .latest_task_record(task_id)?
                    .ok_or_else(|| anyhow!("task rejoin requires durable task record"))?;
                Some(tasks::task_rejoin_fence(&task)?)
            }
            _ => None,
        };
        let source_identity = match scenario {
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation {
                work_item_id,
            } => ExecutionSourceIdentity::WorkItemContinuation {
                work_item_id: work_item_id.clone(),
            },
            scheduler::CanonicalActivationScenario::ExactTaskRejoin { task_id, .. } => {
                ExecutionSourceIdentity::TaskResult {
                    task_id: task_id.clone(),
                    result_message_id: message.id.clone(),
                }
            }
            scheduler::CanonicalActivationScenario::ExactWaitResume { wait_id, .. } => {
                let resume =
                    resume.ok_or_else(|| anyhow!("wait resume requires admitted wait claim"))?;
                ExecutionSourceIdentity::TriggeredWait {
                    wait_id: wait_id.clone(),
                    wait_generation: resume.wait_generation,
                    trigger_id: resume.trigger_id.clone(),
                    trigger_generation: resume.trigger_generation,
                }
            }
            scheduler::CanonicalActivationScenario::InternalFollowup { .. }
            | scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput { .. }
            | scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. } => {
                ExecutionSourceIdentity::QueueMessage {
                    message_id: message.id.clone(),
                }
            }
        };
        let binding = work_item.map_or_else(
            || ExecutionBinding::AgentLifecycle {
                agent_id: message.agent_id.clone(),
            },
            |(work_item, _)| ExecutionBinding::WorkItem {
                work_item_id: work_item.id.clone(),
            },
        );
        let mut commands = Vec::with_capacity(2);
        let (work_item_source_revision, work_item_generation) =
            if let Some((work_item, scheduling_generation)) = work_item {
                let existing_record = existing
                    .as_ref()
                    .and_then(|state| state.work_items.get(&work_item.id));
                if existing_record.is_none() {
                    let state = if let Some(resume) = resume {
                        WorkItemExecutionState::Waiting {
                            generation: scheduling_generation,
                            wait: crate::domain::execution_protocol::WaitReference {
                                wait_id: resume.wait_id.clone(),
                                generation: resume.wait_generation,
                            },
                        }
                    } else {
                        WorkItemExecutionState::Runnable {
                            generation: scheduling_generation,
                            recovery_ref: None,
                        }
                    };
                    commands.push(ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                        RegisterWorkItemExecution {
                            work_item_id: work_item.id.clone(),
                            record: WorkItemExecutionRecord {
                                source_revision: work_item.revision.max(1),
                                state,
                            },
                        },
                    )));
                }
                (
                    Some(
                        existing_record
                            .map_or(work_item.revision.max(1), |record| record.source_revision),
                    ),
                    Some(
                        existing_record.map_or(scheduling_generation, |record| record.generation()),
                    ),
                )
            } else {
                (None, None)
            };
        commands.push(ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
            attempt: ExecutionAttempt {
                attempt_id: attempt_id.to_owned(),
                agent_id: message.agent_id.clone(),
                source_message_id: Some(message.id.clone()),
                source: ExecutionSource {
                    identity: source_identity,
                    generation: source_revision,
                },
                binding,
                provenance: ExecutionProvenance {
                    origin: canonical_execution_origin(message),
                    trust: canonical_execution_trust(message),
                    priority: match message.priority {
                        Priority::Interject => ExecutionPriority::Interject,
                        Priority::Next => ExecutionPriority::Next,
                        Priority::Normal => ExecutionPriority::Normal,
                        Priority::Background => ExecutionPriority::Background,
                    },
                    correlation_id: message.correlation_id.clone(),
                    causation_id: message.causation_id.clone(),
                },
                admitted_fences: AdmittedFences {
                    source_revision,
                    work_item_source_revision,
                    work_item_generation,
                    rejoin,
                    agent_control_revision: authority_fences.agent_control_revision,
                    host_registry_revision: authority_fences.host_registry_revision,
                },
                state: ExecutionAttemptState::Open,
                run_id: None,
                turn_id: message.turn_id.clone(),
                recovery_of_attempt_id: None,
                terminal_outcome_id: None,
                admitted_at: Utc::now().to_rfc3339(),
                terminal_at: None,
            },
        })));
        Ok(
            crate::runtime_db::transitions::ExecutionProtocolTransition {
                bootstrap: existing
                    .is_none()
                    .then(|| ExecutionProtocolState::empty(&message.agent_id)),
                commands,
            },
        )
    }

    fn report_canonical_claim_hard_blocker(
        &self,
        message: &MessageEnvelope,
        blocker: CanonicalClaimHardBlocker,
    ) -> Result<()> {
        let scenario_class = blocker.scenario_class.as_str();
        self.runtime
            .inner
            .storage
            .append_event(&AuditEvent::legacy(
                "scheduler_authority_hard_blocker",
                serde_json::json!({
                    "message_id": message.id,
                    "agent_id": message.agent_id,
                    "scenario_class": scenario_class,
                    "blocker_code": blocker.blocker_code,
                    "queue_disposition": "retained_queued",
                }),
            ))?;
        self.runtime.inner.notify.notify_one();
        Ok(())
    }

    fn append_posture_decision(
        &self,
        boundary: &'static str,
        reason: &'static str,
        previous_status: &AgentStatus,
        next_status: &AgentStatus,
        evidence: Vec<String>,
    ) -> Result<()> {
        self.runtime.inner.storage.append_event(&AuditEvent::legacy(
            "scheduler_posture_decision",
            serde_json::json!({
                "boundary": boundary,
                "reason": reason,
                "previous_status": previous_status,
                "next_status": next_status,
                "evidence": evidence,
            }),
        ))
    }
}

pub(super) fn canonical_activation_id(message_id: &str) -> String {
    format!("activation:message:{message_id}")
}

pub(super) fn canonical_open_activation_id(
    snapshot: &crate::domain::scheduler_protocol::Snapshot,
    message_id: &str,
) -> Option<String> {
    // Phase 4: derive the open activation from the `activations` authority map
    // rather than the `slot` mirror.  `assert_invariants` already guarantees
    // that a Running slot corresponds to exactly one Running activation, so
    // scanning activations is equivalent and removes the slot reader dependency.
    snapshot
        .activations
        .iter()
        .filter(|(_, activation)| {
            activation.state == crate::domain::scheduler_protocol::ActivationState::Running
        })
        .find_map(|(activation_id, _)| {
            snapshot
                .activation_admissions
                .get(activation_id)
                .filter(|admission| admission.activation.provenance.source_id == message_id)
                .map(|_| activation_id.clone())
        })
}

fn canonical_activation_id_for_attempt(
    snapshot: Option<&crate::domain::scheduler_protocol::Snapshot>,
    message_id: &str,
) -> String {
    let base = canonical_activation_id(message_id);
    let Some(snapshot) = snapshot else {
        return base;
    };
    // Admissions are an append-only identity ledger: settlement changes the
    // activation state but never removes its admission. Counting matching
    // admissions therefore remains monotonic across persistence reloads.
    let matching_attempts = snapshot
        .activation_admissions
        .values()
        .filter(|admission| admission.activation.provenance.source_id == message_id)
        .count();
    if matching_attempts == 0 {
        base
    } else {
        format!("{base}:attempt:{}", matching_attempts + 1)
    }
}

fn canonical_claim_hard_blocker(
    scenario_class: crate::domain::scheduler_protocol::SchedulerScenarioClass,
    blocker_code: &'static str,
) -> CanonicalClaimOutcome {
    CanonicalClaimOutcome::HardBlocker(CanonicalClaimHardBlocker {
        scenario_class,
        blocker_code,
    })
}

pub(super) fn canonical_wait_trigger_id(message: &MessageEnvelope) -> String {
    for key in [
        "task_result_id",
        "callback_delivery_id",
        "external_trigger_id",
        "timer_id",
    ] {
        if let Some(value) = message.source_refs.get(key) {
            return format!("{key}:{value}");
        }
    }
    match &message.origin {
        MessageOrigin::Task { task_id } => format!("task:{task_id}"),
        MessageOrigin::Callback { descriptor_id, .. } => {
            format!("callback:{descriptor_id}")
        }
        MessageOrigin::Timer { timer_id } => format!("timer:{timer_id}"),
        _ => format!("message:{}", message.id),
    }
}

fn canonical_activation_origin(
    message: &MessageEnvelope,
) -> crate::domain::scheduler_protocol::ActivationOrigin {
    use crate::domain::scheduler_protocol::ActivationOrigin;
    match message.origin {
        MessageOrigin::Operator { .. } => ActivationOrigin::Operator,
        MessageOrigin::Channel { .. } => ActivationOrigin::Channel,
        MessageOrigin::Webhook { .. } => ActivationOrigin::Webhook,
        MessageOrigin::Callback { .. } => ActivationOrigin::Callback,
        MessageOrigin::Timer { .. } => ActivationOrigin::Timer,
        MessageOrigin::System { .. } => ActivationOrigin::System,
        MessageOrigin::Task { .. } => ActivationOrigin::Task,
    }
}

fn canonical_activation_trust(
    message: &MessageEnvelope,
) -> crate::domain::scheduler_protocol::ActivationTrust {
    use crate::domain::scheduler_protocol::ActivationTrust;
    match message.authority_class {
        crate::types::AuthorityClass::OperatorInstruction => ActivationTrust::OperatorInstruction,
        crate::types::AuthorityClass::RuntimeInstruction => ActivationTrust::RuntimeInstruction,
        crate::types::AuthorityClass::IntegrationSignal => ActivationTrust::IntegrationSignal,
        crate::types::AuthorityClass::ExternalEvidence => ActivationTrust::ExternalEvidence,
    }
}

fn canonical_admission_matches_scenario(
    admission: &crate::domain::scheduler_protocol::AdmitActivationCommand,
    message: &MessageEnvelope,
    scenario: &scheduler::CanonicalActivationScenario,
) -> bool {
    use crate::domain::scheduler_protocol::{ActivationBinding, ActivationCause};
    let activation = &admission.activation;
    if activation.agent_id != message.agent_id
        || activation.provenance.source_id != message.id
        || activation.provenance.origin != canonical_activation_origin(message)
        || activation.provenance.trust != canonical_activation_trust(message)
    {
        return false;
    }
    match (&activation.cause, &activation.binding, scenario) {
        (
            ActivationCause::WorkItemRunnable { work_item_id, .. },
            ActivationBinding::WorkItem {
                work_item_id: bound_work_item_id,
            },
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation {
                work_item_id: expected,
            },
        ) => work_item_id == expected && bound_work_item_id == expected,
        (
            ActivationCause::InternalFollowup { message_id },
            ActivationBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::InternalFollowup {
                work_item_id: expected,
            },
        ) => message_id == &message.id && work_item_id == expected,
        (
            ActivationCause::TaskRejoin {
                task_id,
                message_id,
                resume,
            },
            ActivationBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::ExactTaskRejoin {
                task_id: expected_task,
                work_item_id: expected_work_item,
                wait_id,
            },
        ) => {
            task_id == expected_task
                && message_id == &message.id
                && work_item_id == expected_work_item
                && resume.as_ref().map(|claim| claim.wait_id.as_str()) == wait_id.as_deref()
        }
        (
            ActivationCause::WaitResume { wait_id, .. },
            ActivationBinding::WaitOwner {
                wait_id: bound_wait_id,
                owner:
                    crate::domain::scheduler_protocol::SchedulerOwner::WorkItem {
                        work_item_id: owner_work_item_id,
                    },
            },
            scheduler::CanonicalActivationScenario::ExactWaitResume {
                owner: crate::domain::scheduler_protocol::SchedulerOwner::WorkItem { work_item_id },
                wait_id: expected_wait,
            },
        ) => {
            wait_id == expected_wait
                && bound_wait_id == expected_wait
                && owner_work_item_id == work_item_id
        }
        (
            ActivationCause::WaitResume { wait_id, .. },
            ActivationBinding::Lifecycle { agent_id },
            scheduler::CanonicalActivationScenario::ExactWaitResume {
                owner:
                    crate::domain::scheduler_protocol::SchedulerOwner::AgentLifecycle {
                        agent_id: expected_agent_id,
                    },
                wait_id: expected_wait,
            },
        ) => {
            wait_id == expected_wait
                && agent_id == expected_agent_id
                && agent_id == &message.agent_id
        }
        (
            ActivationCause::LifecycleExternalNudge { message_id },
            ActivationBinding::Lifecycle { agent_id },
            scheduler::CanonicalActivationScenario::ExactWaitResume {
                owner:
                    crate::domain::scheduler_protocol::SchedulerOwner::AgentLifecycle {
                        agent_id: expected_agent_id,
                    },
                ..
            },
        ) => {
            message_id == &message.id
                && agent_id == expected_agent_id
                && agent_id == &message.agent_id
        }
        (
            ActivationCause::LifecycleExternalNudge { message_id },
            ActivationBinding::Lifecycle { agent_id },
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge {
                agent_id: expected_agent_id,
            },
        ) => {
            message_id == &message.id
                && agent_id == expected_agent_id
                && agent_id == &message.agent_id
        }
        (
            ActivationCause::OperatorInput { message_id, resume },
            ActivationBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                work_item_id: expected_work_item,
                wait_id,
            },
        ) => {
            message_id == &message.id
                && work_item_id == expected_work_item
                && resume.as_ref().map(|claim| claim.wait_id.as_str()) == wait_id.as_deref()
        }
        _ => false,
    }
}

pub(super) fn apply_bootstrap_recovered_projection(
    state: &mut AgentState,
    facts: BootstrapRecoveryFacts,
) -> bool {
    if matches!(state.status, AgentStatus::Stopped) {
        return false;
    }

    let previous_status = state.status.clone();
    let previous_run_id = state.current_run_id.clone();
    state.current_run_id = None;

    if state.pending > 0 || facts.queued_messages > 0 || state.pending_wake_hint.is_some() {
        state.status = AgentStatus::AwakeIdle;
    } else if matches!(
        state.status,
        AgentStatus::Booting | AgentStatus::AwakeRunning | AgentStatus::AwaitingTask
    ) {
        state.status = AgentStatus::AwakeIdle;
    }

    state.status != previous_status || state.current_run_id != previous_run_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap_state(status: AgentStatus) -> AgentState {
        let mut state = AgentState::new("default");
        state.status = status;
        state
    }

    #[test]
    fn bootstrap_recovery_with_queued_messages_becomes_runnable_idle() {
        let mut state = bootstrap_state(AgentStatus::Asleep);
        state.pending = 1;
        assert!(apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 1 },
        ));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.current_run_id, None);
    }

    #[test]
    fn bootstrap_recovery_without_runnable_facts_becomes_idle() {
        let mut state = bootstrap_state(AgentStatus::Booting);
        assert!(apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 0 },
        ));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
    }

    #[test]
    fn bootstrap_recovery_preserves_stopped_gate() {
        let mut state = bootstrap_state(AgentStatus::Stopped);
        state.current_run_id = Some("run-1".into());
        assert!(!apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 1 },
        ));
        assert_eq!(state.status, AgentStatus::Stopped);
        assert_eq!(state.current_run_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn bootstrap_recovery_clears_non_durable_current_run() {
        let mut state = bootstrap_state(AgentStatus::AwakeRunning);
        state.current_run_id = Some("run-1".into());
        assert!(apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 0 },
        ));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.current_run_id, None);
    }

    #[test]
    fn bootstrap_recovery_with_pending_wake_hint_becomes_idle() {
        let mut state = bootstrap_state(AgentStatus::Asleep);
        state.pending_wake_hint = Some(crate::types::PendingWakeHint {
            reason: "external".into(),
            description: None,
            source: None,
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: chrono::Utc::now(),
        });
        assert!(apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 0 },
        ));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
    }

    #[test]
    fn bootstrap_recovery_awaiting_task_transitions_to_idle() {
        let mut state = bootstrap_state(AgentStatus::AwaitingTask);
        state.current_run_id = Some("run-task".into());
        assert!(apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 0 },
        ));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.current_run_id, None);
    }

    #[test]
    fn bootstrap_recovery_already_idle_clears_run_id() {
        let mut state = bootstrap_state(AgentStatus::AwakeIdle);
        state.current_run_id = Some("stale-run".into());
        assert!(apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 0 },
        ));
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert_eq!(state.current_run_id, None);
    }

    #[test]
    fn bootstrap_recovery_no_change_when_already_idle_without_pending() {
        let mut state = bootstrap_state(AgentStatus::AwakeIdle);
        // Already AwakeIdle, no run_id, no pending → returns false (no state change)
        assert!(!apply_bootstrap_recovered_projection(
            &mut state,
            BootstrapRecoveryFacts { queued_messages: 0 },
        ));
    }
}
