use super::message_dispatch::MessageDispatchPlan;
use super::*;
use crate::types::ExecutionAdmissionProvenance;

const QUEUE_HEAD_NO_PROGRESS_MAX_ATTEMPTS: u32 = 3;

pub(super) enum RunLoopPoll {
    Shutdown,
    Stopped(AgentState, usize),
    Message(ScheduledMessage),
    Idle,
    AuthorityBlocked,
}

impl RunLoopPoll {
    fn outcome_name(&self) -> &'static str {
        match self {
            RunLoopPoll::Shutdown => "shutdown",
            RunLoopPoll::Stopped(_, _) => "stopped",
            RunLoopPoll::Message(_) => "message",
            RunLoopPoll::Idle => "idle",
            RunLoopPoll::AuthorityBlocked => "authority_blocked",
        }
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
    scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
    work_item_id: Option<String>,
    work_item_expectation: Option<crate::types::WorkItemRecord>,
    execution_protocol: crate::runtime_db::transitions::ExecutionProtocolTransition,
}

enum CanonicalClaimOutcome {
    ReduceOnly,
    Plan(CanonicalClaimPlan),
    RejectQueued {
        scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
        reason: &'static str,
    },
    RetainQueued {
        scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
        reason: &'static str,
    },
    HardBlocker(CanonicalClaimHardBlocker),
}

struct CanonicalClaimHardBlocker {
    scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
    blocker_code: &'static str,
}

enum QueueHeadNoProgressCause {
    RetainedAuthority {
        scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
        reason: &'static str,
    },
    HardBlocker(CanonicalClaimHardBlocker),
    AmbiguousWait,
    ClaimContended {
        scenario_class: Option<crate::domain::scheduler::SchedulerScenarioClass>,
    },
    ReplanExhausted,
}

impl QueueHeadNoProgressCause {
    fn scenario_class(&self) -> Option<crate::domain::scheduler::SchedulerScenarioClass> {
        match self {
            Self::RetainedAuthority { scenario_class, .. } => Some(*scenario_class),
            Self::HardBlocker(blocker) => Some(blocker.scenario_class),
            Self::ClaimContended { scenario_class } => *scenario_class,
            Self::AmbiguousWait | Self::ReplanExhausted => None,
        }
    }

    fn reason(&self) -> &'static str {
        match self {
            Self::RetainedAuthority { reason, .. } => reason,
            Self::HardBlocker(blocker) => blocker.blocker_code,
            Self::AmbiguousWait => "canonical_wait_ambiguous",
            Self::ClaimContended { .. } => "canonical_claim_contended",
            Self::ReplanExhausted => "canonical_claim_replan_exhausted",
        }
    }
}

pub(super) struct SchedulerDecisionExecutor<'a> {
    runtime: &'a RuntimeHandle,
}

#[derive(Clone)]
struct QueueCandidate {
    message: MessageEnvelope,
    prior_state: AgentState,
    queue_len: usize,
}

enum PrepareMessageOutcome {
    Poll(RunLoopPoll),
    Replan,
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
                effective_mode: crate::domain::scheduler::ScenarioMode::Off,
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
            effective_mode: crate::domain::scheduler::ScenarioMode::Off,
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
        let mut replans = 0;
        loop {
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

            match self.prepare_message(candidate.clone()).await? {
                PrepareMessageOutcome::Poll(poll) => {
                    crate::diagnostics::record_scheduler_poll(
                        poll.outcome_name(),
                        started_at.elapsed(),
                    );
                    return Ok(poll);
                }
                PrepareMessageOutcome::Replan
                    if replans + 1 < super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS =>
                {
                    replans += 1;
                }
                PrepareMessageOutcome::Replan => {
                    let poll = self
                        .defer_or_quarantine_queue_head(
                            &candidate,
                            QueueHeadNoProgressCause::ReplanExhausted,
                        )
                        .await?;
                    crate::diagnostics::record_scheduler_poll(
                        poll.outcome_name(),
                        started_at.elapsed(),
                    );
                    return Ok(poll);
                }
            }
        }
    }

    fn shutdown(
        &self,
        mut guard: tokio::sync::MutexGuard<'_, RuntimeAgent>,
    ) -> Result<RunLoopPoll> {
        guard.state.current_run_id = None;
        guard.persist_state(&self.runtime.inner.storage)?;
        Ok(RunLoopPoll::Shutdown)
    }

    async fn prepare_message(&self, candidate: QueueCandidate) -> Result<PrepareMessageOutcome> {
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
        let replay_source_turn_id = self
            .runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&candidate.message.id)?
            .filter(|entry| entry.status == QueueEntryStatus::Interrupted)
            .and_then(|_| persisted_message.turn_id.clone());
        let canonical_claim = if self.runtime.inner.scheduler_engine.is_canonical() {
            match self.canonical_activation_plan(
                &projection,
                &persisted_message,
                &dispatch_plan,
                legacy_decision.model_reentry,
            ) {
                Ok(CanonicalClaimOutcome::ReduceOnly) => None,
                Ok(CanonicalClaimOutcome::Plan(plan)) => Some(plan),
                Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason,
                }) => {
                    self.terminalize_rejected_queue_head(
                        &candidate,
                        &persisted_message,
                        scenario_class,
                        reason,
                    )
                    .await?;
                    // Terminalization notifies the scheduler, so the next poll can
                    // advance past the rejected queue head in the same session.
                    return Ok(PrepareMessageOutcome::Poll(RunLoopPoll::Idle));
                }
                Ok(CanonicalClaimOutcome::RetainQueued {
                    scenario_class,
                    reason,
                }) => {
                    return Ok(PrepareMessageOutcome::Poll(
                        self.defer_or_quarantine_queue_head(
                            &candidate,
                            QueueHeadNoProgressCause::RetainedAuthority {
                                scenario_class,
                                reason,
                            },
                        )
                        .await?,
                    ));
                }
                Ok(CanonicalClaimOutcome::HardBlocker(blocker)) => {
                    return Ok(PrepareMessageOutcome::Poll(
                        self.defer_or_quarantine_queue_head(
                            &candidate,
                            QueueHeadNoProgressCause::HardBlocker(blocker),
                        )
                        .await?,
                    ));
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
                        return Ok(PrepareMessageOutcome::Poll(
                            self.defer_or_quarantine_queue_head(
                                &candidate,
                                QueueHeadNoProgressCause::AmbiguousWait,
                            )
                            .await?,
                        ));
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
                if let Some(work_item_id) = plan.work_item_id.as_deref() {
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
        #[cfg(test)]
        self.runtime
            .apply_claim_work_item_plan_status_before_commit()
            .await?;

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
                    return Ok(PrepareMessageOutcome::Poll(self.shutdown(guard)?));
                }
                if matches!(guard.state.status, AgentStatus::Stopped) {
                    return Ok(PrepareMessageOutcome::Poll(RunLoopPoll::Stopped(
                        guard.state.clone(),
                        guard.queue.len(),
                    )));
                }
                if !guard
                    .queue
                    .peek()
                    .is_some_and(|message| message.id == candidate.message.id)
                {
                    return Ok(PrepareMessageOutcome::Poll(RunLoopPoll::Idle));
                }
                let mut running_state = guard.state.clone();
                running_state.pending = guard.queue.len().saturating_sub(1);
                scheduler::apply_running_projection(&mut running_state, run_id.clone());
                running_state.last_wake_reason = Some(format!("{:?}", candidate.message.kind));
                let mut execution_protocol = canonical_claim
                    .as_ref()
                    .map(|plan| plan.execution_protocol.clone())
                    .unwrap_or_default();
                let wait_transition = canonical_claim
                    .as_ref()
                    .map(|_| {
                        self.runtime
                            .wait_resolution_transition_for_message(&persisted_message)
                    })
                    .transpose()?
                    .flatten();
                align_execution_claim_with_wait_transition(
                    &mut execution_protocol,
                    wait_transition.as_ref(),
                )?;
                let mut attempt_audit_events = claim_audit_events.clone();
                if let Some(wait_transition) = wait_transition.as_ref() {
                    attempt_audit_events.push(AuditEvent::legacy(
                        "wait_conditions_resolved",
                        serde_json::json!({
                            "agent_id": persisted_message.agent_id,
                            "message_id": persisted_message.id,
                            "reason": "execution_admission",
                            "wait_condition_ids": [wait_transition.record.id],
                        }),
                    ));
                }
                let commit_result = self
                    .runtime
                    .inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_execution_protocol_and_wait_transition(
                        &crate::runtime_db::transitions::QueueTransitionCommand {
                            agent_id: agent_id.clone(),
                            operation: crate::runtime_db::transitions::QueueOperation::Claim,
                            mutation: crate::runtime_db::transitions::QueueMutation::Consume(
                                queue_record.clone(),
                            ),
                            scheduler_claim_work_item: canonical_claim
                                .as_ref()
                                .and_then(|plan| plan.work_item_expectation.clone()),
                            scheduler_protocol_bootstrap: None,
                            scheduler_protocol_commands: Vec::new(),
                            agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                                expected: Some(Box::new(guard.state.clone())),
                                record: Box::new(running_state.clone()),
                            }),
                            message_evidence: Vec::new(),
                            transcript_entries: Vec::new(),
                            turn_record: None,
                            audit_events: attempt_audit_events,
                            notify_scheduler: false,
                            fault: self.runtime.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        },
                        &execution_protocol,
                        wait_transition.as_ref(),
                    );
                let mut commit = match commit_result {
                    Ok(commit) => commit,
                    Err(error) => {
                        if scheduler_work_item_claim_conflict(&error) {
                            drop(guard);
                            return Ok(PrepareMessageOutcome::Replan);
                        }
                        let can_retry = attempt + 1 < super::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                            && super::retryable_enqueue_conflict(&error, &agent_id);
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
                    return Ok(PrepareMessageOutcome::Poll(
                        self.defer_or_quarantine_queue_head(
                            &candidate,
                            QueueHeadNoProgressCause::ClaimContended {
                                scenario_class: canonical_claim
                                    .as_ref()
                                    .map(|plan| plan.scenario_class),
                            },
                        )
                        .await?,
                    ));
                }
                if !commit.applied {
                    let _ = guard.queue.pop_if_next(&candidate.message.id);
                    guard.state.pending = guard.queue.len();
                    guard.persist_state(&self.runtime.inner.storage)?;
                    return Ok(PrepareMessageOutcome::Poll(RunLoopPoll::Idle));
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
                let mut claimed_message = persisted_message.clone();
                if let Some(source_turn_id) = replay_source_turn_id.as_ref() {
                    claimed_message
                        .source_refs
                        .insert("replay_source_turn_id".into(), source_turn_id.clone());
                    claimed_message.source_refs.insert(
                        "replay_reason".into(),
                        "interrupted_queue_claim_reentry".into(),
                    );
                }
                break (claimed_message, running_state, commit);
            }
        };
        self.runtime
            .apply_transition_commit(transition_commit)
            .await;

        Ok(PrepareMessageOutcome::Poll(RunLoopPoll::Message(
            ScheduledMessage {
                message,
                running_state,
                dispatch_plan,
                scheduler_decision: effective_decision,
            },
        )))
    }

    fn provably_stale_correlated_wait(
        &self,
        candidate: &scheduler::CanonicalActivationCandidate,
        message: &MessageEnvelope,
    ) -> Result<bool> {
        let scheduler::CanonicalActivationCandidate::ExactWaitResume {
            expected_work_item_id,
            correlated_wait: Some(wait_id),
        } = candidate
        else {
            return Ok(false);
        };
        let durable_wait = self
            .runtime
            .inner
            .storage
            .latest_wait_conditions()?
            .into_iter()
            .find(|condition| {
                condition.id == *wait_id
                    && condition.agent_id == message.agent_id
                    && condition.work_item_id.as_deref() == expected_work_item_id.as_deref()
            });
        if durable_wait.as_ref().is_none_or(|condition| {
            !matches!(
                condition.status,
                crate::types::WaitConditionStatus::Active
                    | crate::types::WaitConditionStatus::Triggered
            ) || (condition.status == crate::types::WaitConditionStatus::Triggered
                && condition.trigger_message_id() != Some(message.id.as_str()))
        }) {
            return Ok(true);
        }
        if let Some(work_item_id) = expected_work_item_id {
            if let Some(execution) = self
                .runtime
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized(&message.agent_id)?
            {
                if execution.work_items.get(work_item_id).is_some_and(|work| {
                    !matches!(
                        &work.state,
                        crate::domain::execution_protocol::WorkItemExecutionState::Waiting {
                            wait,
                            ..
                        } if wait.wait_id == *wait_id
                    )
                }) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn provably_stale_task_rejoin(
        &self,
        candidate: &scheduler::CanonicalActivationCandidate,
        message: &MessageEnvelope,
    ) -> Result<bool> {
        let scheduler::CanonicalActivationCandidate::ExactTaskRejoin {
            task_id,
            work_item_id,
        } = candidate
        else {
            return Ok(false);
        };
        let Some(execution) = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&message.agent_id)?
        else {
            return Ok(false);
        };
        let Some(work) = execution.work_items.get(work_item_id) else {
            // Once the unified partition exists, an exact task rejoin can only
            // resume an open WorkItem owned by that partition. A completing or
            // completed WorkItem cannot regain execution authority even when a
            // pre-cutover resolved task wait still references it.
            if self
                .runtime
                .inner
                .runtime_db
                .work_items()
                .latest(work_item_id)?
                .is_none_or(|work_item| work_item.state != WorkItemState::Open)
            {
                return Ok(true);
            }
            // Open pre-cutover WorkItems may still reference a legacy scheduler
            // Waiting mirror, but they are only provably orphaned when no exact
            // durable task wait remains for the same WorkItem.
            let exact_wait_exists = self
                .runtime
                .inner
                .storage
                .latest_wait_conditions()?
                .into_iter()
                .any(|condition| {
                    condition.agent_id == message.agent_id
                        && condition.work_item_id.as_deref() == Some(work_item_id.as_str())
                        && matches!(
                            condition.status,
                            crate::types::WaitConditionStatus::Active
                                | crate::types::WaitConditionStatus::Triggered
                                | crate::types::WaitConditionStatus::Resolved
                        )
                        && condition.kind == crate::types::WaitConditionKind::Task
                        && condition.wake_sources.iter().any(|source| {
                            matches!(
                                source,
                                crate::types::WakeSource::TaskResult {
                                    task_id: expected_task_id,
                                } if expected_task_id == task_id
                            )
                        })
                });
            return Ok(!exact_wait_exists);
        };
        let crate::domain::execution_protocol::WorkItemExecutionState::Waiting { wait, .. } =
            &work.state
        else {
            return Ok(false);
        };
        let current_wait = self
            .runtime
            .inner
            .storage
            .latest_wait_conditions()?
            .into_iter()
            .find(|condition| {
                condition.id == wait.wait_id
                    && condition.agent_id == message.agent_id
                    && condition.work_item_id.as_deref() == Some(work_item_id.as_str())
            });
        Ok(current_wait.is_none_or(|condition| {
            !matches!(
                condition.status,
                crate::types::WaitConditionStatus::Active
                    | crate::types::WaitConditionStatus::Triggered
                    | crate::types::WaitConditionStatus::Resolved
            ) || condition.kind != crate::types::WaitConditionKind::Task
                || !condition.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        crate::types::WakeSource::TaskResult {
                            task_id: expected_task_id,
                        } if expected_task_id == task_id
                    )
                })
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
            Err(_) => return Ok(CanonicalClaimOutcome::ReduceOnly),
        };
        let Some(candidate) = scheduler::canonical_activation_candidate(
            message,
            dispatch_plan.continuation_resolution.as_ref(),
            task,
        )?
        else {
            return Ok(if model_reentry {
                CanonicalClaimOutcome::RejectQueued {
                    scenario_class:
                        crate::domain::scheduler::SchedulerScenarioClass::ReducerOnlyCandidates,
                    reason: "canonical_model_reentry_candidate_unclassified",
                }
            } else {
                CanonicalClaimOutcome::ReduceOnly
            });
        };
        let scenario_class = candidate.scenario_class();
        let stale_correlated_wait = self.provably_stale_correlated_wait(&candidate, message)?;
        let stale_task_rejoin = self.provably_stale_task_rejoin(&candidate, message)?;
        if let scheduler::CanonicalActivationCandidate::ExactTaskRejoin { task_id, .. } = &candidate
        {
            let durable_task = self.runtime.inner.storage.latest_task_record(task_id)?;
            if durable_task
                .as_ref()
                .and_then(|task| tasks::task_rejoin_fence(task).ok())
                .is_none()
            {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_task_rejoin_contract_missing_or_invalid",
                });
            }
        }
        let original_candidate = candidate.clone();
        let mut scenario =
            scheduler::resolve_canonical_activation_scenario(projection, message, candidate)?;
        if scenario.is_none() && task.is_some_and(TaskRecord::terminal_reentry) {
            if let scheduler::CanonicalActivationCandidate::ExactTaskRejoin {
                task_id,
                work_item_id,
            } = &original_candidate
            {
                scenario = Some(scheduler::CanonicalActivationScenario::ExactTaskRejoin {
                    task_id: task_id.clone(),
                    work_item_id: work_item_id.clone(),
                    wait_id: None,
                });
            }
        }
        let Some(mut scenario) = scenario else {
            if original_candidate
                == scheduler::CanonicalActivationCandidate::UnboundTaskResultWaitOrReduce
            {
                return Ok(CanonicalClaimOutcome::ReduceOnly);
            }
            if stale_task_rejoin {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_task_rejoin_stale",
                });
            }
            if stale_correlated_wait {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_correlated_wait_stale",
                });
            }
            if let scheduler::CanonicalActivationCandidate::ExactTaskRejoin {
                work_item_id, ..
            } = &original_candidate
            {
                if self
                    .runtime
                    .inner
                    .runtime_db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&message.agent_id)?
                    .is_some_and(|execution| !execution.work_items.contains_key(work_item_id))
                {
                    return Ok(CanonicalClaimOutcome::RejectQueued {
                        scenario_class,
                        reason: "canonical_wait_execution_authority_missing",
                    });
                }
            }
            return Ok(canonical_claim_hard_blocker(
                scenario_class,
                "canonical_activation_scenario_unresolved",
            ));
        };

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
                existing_execution,
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
        let Some(work_item) = self.runtime.inner.storage.latest_work_item(work_item_id)? else {
            return Ok(
                if matches!(
                    scenario,
                    scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput { .. }
                ) {
                    CanonicalClaimOutcome::RetainQueued {
                        scenario_class,
                        reason: "explicit_binding_work_item_missing",
                    }
                } else {
                    CanonicalClaimOutcome::RejectQueued {
                        scenario_class,
                        reason: "canonical_work_item_missing",
                    }
                },
            );
        };
        if work_item.agent_id != message.agent_id {
            return Ok(CanonicalClaimOutcome::RejectQueued {
                scenario_class,
                reason: "canonical_work_item_not_open_same_agent",
            });
        }
        if work_item.state != crate::types::WorkItemState::Open {
            if matches!(
                scenario,
                scheduler::CanonicalActivationScenario::ExactTaskRejoin { .. }
            ) && task.is_some_and(|task| {
                crate::runtime::task_state_reducer::is_terminal_task_status(&task.status)
            }) {
                return Ok(CanonicalClaimOutcome::ReduceOnly);
            }
            return Ok(CanonicalClaimOutcome::RejectQueued {
                scenario_class,
                reason: "canonical_work_item_not_open_same_agent",
            });
        }
        if let scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation {
            expected_work_item_revision,
            ..
        } = &scenario
        {
            if work_item.revision != *expected_work_item_revision {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_autonomous_work_item_revision_stale",
                });
            }
        }
        let work_queue = self.runtime.inner.storage.work_queue_prompt_projection()?;
        let Some(work_projection) = work_queue
            .items
            .iter()
            .find(|candidate| candidate.id == work_item.id)
        else {
            return Ok(canonical_claim_hard_blocker(
                scenario_class,
                "canonical_work_item_projection_missing",
            ));
        };
        if matches!(
            scenario,
            scheduler::CanonicalActivationScenario::ProviderRecovery { .. }
        ) {
            if !crate::runtime::turn::TurnModelSelection::message_has_valid_provider_recovery(
                message,
            ) {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_provider_recovery_directive_invalid",
                });
            }
            if !matches!(
                work_projection.scheduling_state,
                crate::types::WorkItemSchedulingState::Runnable
                    | crate::types::WorkItemSchedulingState::WaitingOperator
                    | crate::types::WorkItemSchedulingState::WaitingTask
                    | crate::types::WorkItemSchedulingState::WaitingExternal
                    | crate::types::WorkItemSchedulingState::WaitingTimer
                    | crate::types::WorkItemSchedulingState::WaitingSystem
            ) {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_provider_recovery_work_item_not_recoverable",
                });
            }
        }
        if matches!(
            scenario,
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
                | scheduler::CanonicalActivationScenario::InternalFollowup { .. }
        ) && work_projection.scheduling_state != crate::types::WorkItemSchedulingState::Runnable
        {
            return Ok(CanonicalClaimOutcome::RejectQueued {
                scenario_class,
                reason: match scenario {
                    scheduler::CanonicalActivationScenario::InternalFollowup { .. } => {
                        "canonical_internal_followup_work_item_not_runnable"
                    }
                    _ => "canonical_autonomous_work_item_not_runnable",
                },
            });
        }

        let authoritative_work = existing_execution
            .as_ref()
            .and_then(|state| state.work_items.get(work_item_id));
        if let scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation {
            expected_work_item_revision,
            ..
        } = &scenario
        {
            let Some(authoritative_work) = authoritative_work else {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_autonomous_execution_authority_missing",
                });
            };
            if authoritative_work.source_revision != *expected_work_item_revision {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_autonomous_execution_revision_stale",
                });
            }
        }
        let recovery_of_attempt_id = if matches!(
            scenario,
            scheduler::CanonicalActivationScenario::ProviderRecovery { .. }
        ) {
            let Some(attempt_id) =
                self.provider_recovery_source_attempt_id(message, existing_execution.as_ref())?
            else {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_provider_recovery_source_invalid",
                });
            };
            Some(attempt_id)
        } else {
            None
        };
        if matches!(
            scenario,
            scheduler::CanonicalActivationScenario::ProviderRecovery { .. }
        ) {
            let Some(authoritative_work) = authoritative_work else {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_provider_recovery_execution_authority_missing",
                });
            };
            if !matches!(
                authoritative_work.state,
                crate::domain::execution_protocol::WorkItemExecutionState::Runnable { .. }
                    | crate::domain::execution_protocol::WorkItemExecutionState::Waiting { .. }
            ) {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_provider_recovery_execution_not_recoverable",
                });
            }
        }
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
            | scheduler::CanonicalActivationScenario::ProviderRecovery { .. }
            | scheduler::CanonicalActivationScenario::InternalFollowup { .. } => None,
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. } => {
                unreachable!("lifecycle scenario is planned before WorkItem lookup")
            }
        };
        if let Some(wait_id) = wait_id {
            let Some(authoritative_work) = authoritative_work else {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_wait_execution_authority_missing",
                });
            };
            if !matches!(
                &authoritative_work.state,
                crate::domain::execution_protocol::WorkItemExecutionState::Waiting { wait, .. }
                    if wait.wait_id == wait_id
            ) {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_wait_execution_authority_mismatch",
                });
            }
        } else if !matches!(
            scenario,
            scheduler::CanonicalActivationScenario::ProviderRecovery { .. }
        ) && authoritative_work.is_some_and(|record| {
            !matches!(
                record.state,
                crate::domain::execution_protocol::WorkItemExecutionState::Runnable { .. }
            )
        }) {
            return Ok(CanonicalClaimOutcome::RejectQueued {
                scenario_class,
                reason: "canonical_work_item_execution_not_runnable",
            });
        }

        let scheduling_generation =
            authoritative_work.map_or(work_item.revision.max(1), |record| record.generation());
        let activation_id =
            canonical_execution_attempt_id_for_message(existing_execution.as_ref(), &message.id);
        if let Some(existing_attempt) = existing_execution
            .as_ref()
            .and_then(|state| state.attempts.get(&activation_id))
        {
            if execution_attempt_matches_scenario(existing_attempt, message, &scenario) {
                return Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
                    activation_id,
                    scenario_class,
                    work_item_id: Some(work_item.id),
                    work_item_expectation: None,
                    execution_protocol:
                        crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
                }));
            }
            return Ok(CanonicalClaimOutcome::RejectQueued {
                scenario_class,
                reason: "canonical_execution_attempt_replay_conflict",
            });
        }
        let execution_protocol = self.plan_execution_protocol_claim(
            message,
            &scenario,
            &activation_id,
            Some((&work_item, scheduling_generation)),
            wait_id,
            recovery_of_attempt_id,
        )?;
        let work_item_expectation = matches!(
            scenario,
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
                | scheduler::CanonicalActivationScenario::InternalFollowup { .. }
        )
        .then_some(work_item.clone());

        Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
            activation_id,
            scenario_class,
            work_item_id: Some(work_item.id),
            work_item_expectation,
            execution_protocol,
        }))
    }

    fn plan_canonical_lifecycle_activation_claim(
        &self,
        message: &MessageEnvelope,
        scenario: scheduler::CanonicalActivationScenario,
        existing_execution: Option<crate::domain::execution_protocol::ExecutionProtocolState>,
        scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
    ) -> Result<CanonicalClaimOutcome> {
        let wait_id = match &scenario {
            scheduler::CanonicalActivationScenario::ExactWaitResume { owner, wait_id }
                if owner
                    == &(crate::domain::scheduler::SchedulerOwner::AgentLifecycle {
                        agent_id: message.agent_id.clone(),
                    }) =>
            {
                let matching = self
                    .runtime
                    .inner
                    .storage
                    .latest_wait_conditions()?
                    .into_iter()
                    .find(|condition| {
                        condition.id == *wait_id
                            && condition.agent_id == message.agent_id
                            && condition.work_item_id.is_none()
                            && matches!(
                                condition.status,
                                crate::types::WaitConditionStatus::Triggered
                                    | crate::types::WaitConditionStatus::Resolved
                            )
                            && condition.trigger_message_id() == Some(message.id.as_str())
                            && scheduler::message_matches_wait_condition(message, condition)
                    });
                if matching.is_none() {
                    return Ok(CanonicalClaimOutcome::RejectQueued {
                        scenario_class,
                        reason: "canonical_lifecycle_wait_stale",
                    });
                }
                Some(wait_id.as_str())
            }
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge { agent_id }
                if agent_id == &message.agent_id =>
            {
                None
            }
            _ => {
                return Ok(CanonicalClaimOutcome::RejectQueued {
                    scenario_class,
                    reason: "canonical_lifecycle_binding_mismatch",
                })
            }
        };
        let activation_id =
            canonical_execution_attempt_id_for_message(existing_execution.as_ref(), &message.id);
        if let Some(existing_attempt) = existing_execution
            .as_ref()
            .and_then(|state| state.attempts.get(&activation_id))
        {
            if execution_attempt_matches_scenario(existing_attempt, message, &scenario) {
                return Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
                    activation_id,
                    scenario_class,
                    work_item_id: None,
                    work_item_expectation: None,
                    execution_protocol:
                        crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
                }));
            }
            return Ok(CanonicalClaimOutcome::RejectQueued {
                scenario_class,
                reason: "canonical_execution_attempt_replay_conflict",
            });
        }
        let execution_protocol = self.plan_execution_protocol_claim(
            message,
            &scenario,
            &activation_id,
            None,
            wait_id,
            None,
        )?;
        Ok(CanonicalClaimOutcome::Plan(CanonicalClaimPlan {
            activation_id,
            scenario_class,
            work_item_id: None,
            work_item_expectation: None,
            execution_protocol,
        }))
    }

    async fn terminalize_rejected_queue_head(
        &self,
        candidate: &QueueCandidate,
        message: &MessageEnvelope,
        scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
        reason: &'static str,
    ) -> Result<()> {
        let expected = self
            .runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest_all()?
            .into_iter()
            .find(|entry| entry.message_id == message.id)
            .ok_or_else(|| anyhow!("rejected queue entry is missing durable state"))?;
        if !matches!(
            expected.status,
            QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
        ) {
            return Ok(());
        }
        let mut dropped = expected.clone();
        dropped.status = QueueEntryStatus::Dropped;
        dropped.updated_at = self.runtime.now();
        let invariant_event = scheduler::scheduler_invariant_diagnostic_event(
            &message.agent_id,
            reason,
            message.work_item_id.clone(),
            Some(message.id.clone()),
            vec![
                format!("message_kind={:?}", message.kind),
                format!("message_origin={:?}", message.origin),
                format!("authority_class={:?}", message.authority_class),
                format!("delivery_surface={:?}", message.delivery_surface),
                format!("admission_context={:?}", message.admission_context),
                "queue_disposition=dropped".into(),
            ],
        )?;
        let mut guard = self.runtime.inner.agent.lock().await;
        if !guard
            .queue
            .peek()
            .is_some_and(|queued| queued.id == message.id)
        {
            return Ok(());
        }
        let mut next_state = guard.state.clone();
        next_state.pending = candidate.queue_len.saturating_sub(1);
        let mut commit = self.runtime.inner.runtime_db.transitions().commit_queue(
            &crate::runtime_db::transitions::QueueTransitionCommand {
                agent_id: message.agent_id.clone(),
                operation: crate::runtime_db::transitions::QueueOperation::Settle,
                mutation: crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                    expected,
                    record: dropped,
                },
                scheduler_claim_work_item: None,
                scheduler_protocol_bootstrap: None,
                scheduler_protocol_commands: Vec::new(),
                agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(guard.state.clone())),
                    record: Box::new(next_state.clone()),
                }),
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: vec![
                    AuditEvent::legacy(
                        "scheduler_authority_input_rejected",
                        serde_json::json!({
                            "message_id": message.id,
                            "agent_id": message.agent_id,
                            "scenario_class": scenario_class.as_str(),
                            "reason": reason,
                            "queue_disposition": "dropped",
                            "message_kind": message.kind,
                            "message_origin": message.origin,
                            "authority_class": message.authority_class,
                            "delivery_surface": message.delivery_surface,
                            "admission_context": message.admission_context,
                        }),
                    ),
                    invariant_event,
                ],
                notify_scheduler: true,
                fault: self.runtime.take_transition_fault(),
                brief_evidence: Vec::new(),
            },
        )?;
        if !commit.applied {
            return Ok(());
        }
        let _ = guard.queue.pop_if_next(&message.id);
        guard.state = next_state.clone();
        guard.last_persisted_state = next_state;
        commit.effects.agent_state = None;
        drop(guard);
        self.runtime.apply_transition_commit(commit).await;
        Ok(())
    }

    fn plan_execution_protocol_claim(
        &self,
        message: &MessageEnvelope,
        scenario: &scheduler::CanonicalActivationScenario,
        attempt_id: &str,
        work_item: Option<(&crate::types::WorkItemRecord, u64)>,
        admitted_wait_id: Option<&str>,
        recovery_of_attempt_id: Option<String>,
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
                ..
            } => ExecutionSourceIdentity::WorkItemContinuation {
                work_item_id: work_item_id.clone(),
            },
            scheduler::CanonicalActivationScenario::ProviderRecovery { .. } => {
                ExecutionSourceIdentity::RuntimeRecovery {
                    recovery_id: message.id.clone(),
                }
            }
            scheduler::CanonicalActivationScenario::ExactTaskRejoin { task_id, .. } => {
                ExecutionSourceIdentity::TaskResult {
                    task_id: task_id.clone(),
                    result_message_id: message.id.clone(),
                }
            }
            scheduler::CanonicalActivationScenario::ExactWaitResume { wait_id, .. } => {
                if admitted_wait_id != Some(wait_id.as_str()) {
                    return Err(anyhow!(
                        "wait resume requires the exact admitted wait identity"
                    ));
                }
                ExecutionSourceIdentity::TriggeredWait {
                    wait_id: wait_id.clone(),
                    trigger_message_id: message.id.clone(),
                }
            }
            scheduler::CanonicalActivationScenario::InternalFollowup { .. } => {
                ExecutionSourceIdentity::InternalFollowup {
                    message_id: message.id.clone(),
                }
            }
            scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                wait_id: Some(wait_id),
                ..
            } => ExecutionSourceIdentity::TriggeredWait {
                wait_id: wait_id.clone(),
                trigger_message_id: message.id.clone(),
            },
            scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                wait_id: None,
                ..
            } => ExecutionSourceIdentity::QueueMessage {
                message_id: message.id.clone(),
            },
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. }
                if scheduler::runtime_owned_internal_followup(message) =>
            {
                ExecutionSourceIdentity::InternalFollowup {
                    message_id: message.id.clone(),
                }
            }
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. } => {
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
                    let state = if let Some(wait_id) = admitted_wait_id {
                        WorkItemExecutionState::Waiting {
                            generation: scheduling_generation,
                            wait: crate::domain::execution_protocol::WaitReference {
                                wait_id: wait_id.to_string(),
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
                    origin: canonical_execution_origin_for_scenario(message, scenario),
                    trust: canonical_execution_trust_for_scenario(message, scenario),
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
                recovery_of_attempt_id,
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

    fn provider_recovery_source_attempt_id(
        &self,
        message: &MessageEnvelope,
        execution: Option<&crate::domain::execution_protocol::ExecutionProtocolState>,
    ) -> Result<Option<String>> {
        use crate::domain::execution_protocol::ExecutionAttemptState;

        let selection = crate::runtime::turn::TurnModelSelection::from_message(message)?;
        let Some(recovery) = selection.recovery.as_ref() else {
            return Ok(None);
        };
        if !matches!(
            recovery.source_terminal_kind,
            crate::types::TurnTerminalKind::DeferredToFallback
                | crate::types::TurnTerminalKind::ProviderFailedNeedsRecovery
        ) || message.source_refs.get("source_turn_id") != Some(&recovery.source_turn_id)
            || message.source_refs.get("source_message_id") != Some(&recovery.source_message_id)
            || message.causation_id.as_deref() != Some(recovery.source_message_id.as_str())
        {
            return Ok(None);
        }
        let Some(source_message) = self
            .runtime
            .inner
            .storage
            .read_message_by_id(&recovery.source_message_id)?
        else {
            return Ok(None);
        };
        if source_message.agent_id != message.agent_id
            || source_message.turn_id.as_deref() != Some(recovery.source_turn_id.as_str())
        {
            return Ok(None);
        }
        let Some(source_turn) = self
            .runtime
            .inner
            .storage
            .read_turn_by_id(&recovery.source_turn_id)?
        else {
            return Ok(None);
        };
        if source_turn.agent_id != message.agent_id
            || source_turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(recovery.source_message_id.as_str())
            || source_turn.terminal.as_ref().map(|terminal| terminal.kind)
                != Some(recovery.source_terminal_kind)
        {
            return Ok(None);
        }
        let Some(execution) = execution else {
            return Ok(None);
        };
        let matching_attempts = execution
            .attempts
            .values()
            .filter(|attempt| {
                attempt.agent_id == message.agent_id
                    && attempt.source_message_id.as_deref()
                        == Some(recovery.source_message_id.as_str())
                    && attempt.turn_id.as_deref() == Some(recovery.source_turn_id.as_str())
                    && matches!(
                        attempt.state,
                        ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
                    )
                    && attempt.terminal_outcome_id.is_some()
            })
            .map(|attempt| attempt.attempt_id.clone())
            .collect::<Vec<_>>();
        Ok(match matching_attempts.as_slice() {
            [attempt_id] => Some(attempt_id.clone()),
            _ => None,
        })
    }

    async fn defer_or_quarantine_queue_head(
        &self,
        candidate: &QueueCandidate,
        cause: QueueHeadNoProgressCause,
    ) -> Result<RunLoopPoll> {
        let scenario_class = cause.scenario_class();
        let reason = cause.reason();
        let expected = self
            .runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&candidate.message.id)?
            .ok_or_else(|| anyhow!("deferred queue head is missing durable state"))?;
        if !matches!(
            expected.status,
            QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
        ) {
            return Ok(RunLoopPoll::Idle);
        }
        let mut quarantined = expected.clone();
        quarantined.status = QueueEntryStatus::Quarantined;
        quarantined.updated_at = self.runtime.now();

        let mut guard = self.runtime.inner.agent.lock().await;
        if !guard
            .queue
            .peek()
            .is_some_and(|queued| queued.id == candidate.message.id)
        {
            return Ok(RunLoopPoll::Idle);
        }
        let mut next_state = guard.state.clone();
        next_state.pending = guard.queue.len().saturating_sub(1);
        let Some(mut result) = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue_head_no_progress(
                &crate::runtime_db::transitions::QueueHeadNoProgressCommand {
                    agent_id: candidate.message.agent_id.clone(),
                    expected,
                    quarantined,
                    agent_state: crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(guard.state.clone())),
                        record: Box::new(next_state.clone()),
                    },
                    reason: reason.into(),
                    scenario_class: scenario_class.map(|scenario| scenario.as_str().to_string()),
                    max_attempts: QUEUE_HEAD_NO_PROGRESS_MAX_ATTEMPTS,
                    fault: self.runtime.take_transition_fault(),
                },
            )?
        else {
            return Ok(RunLoopPoll::Idle);
        };
        let quarantined = matches!(
            result.outcome,
            crate::runtime_db::transitions::QueueHeadNoProgressOutcome::Quarantined { .. }
        );
        if quarantined {
            let _ = guard.queue.pop_if_next(&candidate.message.id);
            guard.state = next_state.clone();
            guard.last_persisted_state = next_state;
            result.commit.effects.agent_state = None;
        }
        drop(guard);
        self.runtime.apply_transition_commit(result.commit).await;
        Ok(if quarantined {
            RunLoopPoll::Idle
        } else {
            RunLoopPoll::AuthorityBlocked
        })
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

fn scheduler_work_item_claim_conflict(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some_and(|conflict| {
                conflict.retryable() && conflict.domain() == "scheduler_claim_work_item"
            })
    })
}

fn align_execution_claim_with_wait_transition(
    execution_protocol: &mut crate::runtime_db::transitions::ExecutionProtocolTransition,
    wait_transition: Option<&crate::runtime_db::transitions::QueueWaitTransition>,
) -> Result<()> {
    let Some(crate::runtime_db::transitions::WorkItemMutation::Update { record, .. }) =
        wait_transition.and_then(|transition| transition.work_item.as_ref())
    else {
        return Ok(());
    };
    let Some(admit_index) = execution_protocol.commands.iter().position(|command| {
        matches!(
            command,
            crate::domain::execution_protocol::ExecutionProtocolCommand::Admit(_)
        )
    }) else {
        return Ok(());
    };
    let crate::domain::execution_protocol::ExecutionProtocolCommand::Admit(admit) =
        &mut execution_protocol.commands[admit_index]
    else {
        unreachable!("admit index was selected by variant");
    };
    let crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } =
        &admit.attempt.binding
    else {
        return Ok(());
    };
    if work_item_id != &record.id {
        return Err(anyhow!(
            "wait transition WorkItem does not match execution admission binding"
        ));
    }
    let Some(expected_source_revision) = admit.attempt.admitted_fences.work_item_source_revision
    else {
        return Err(anyhow!(
            "WorkItem wait admission is missing its source revision fence"
        ));
    };
    if record.revision == expected_source_revision {
        return Ok(());
    }
    if record.revision < expected_source_revision {
        return Err(anyhow!(
            "wait transition WorkItem revision precedes execution admission fence"
        ));
    }
    admit.attempt.admitted_fences.work_item_source_revision = Some(record.revision);
    let command_id = format!(
        "advance:{}:work_item_source_revision",
        admit.attempt.attempt_id
    );
    execution_protocol.commands.insert(
        admit_index,
        crate::domain::execution_protocol::ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(
            crate::domain::execution_protocol::AdvanceWorkItemSourceRevision {
                command_id,
                work_item_id: record.id.clone(),
                expected_source_revision,
                source_revision: record.revision,
            },
        ),
    );
    Ok(())
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

fn canonical_execution_attempt_id_for_message(
    state: Option<&crate::domain::execution_protocol::ExecutionProtocolState>,
    message_id: &str,
) -> String {
    let base = canonical_activation_id(message_id);
    let matching_attempts = state.map_or(0, |state| {
        state
            .attempts
            .values()
            .filter(|attempt| attempt.source_message_id.as_deref() == Some(message_id))
            .count()
    });
    if matching_attempts == 0 {
        base
    } else {
        format!("{base}:attempt:{}", matching_attempts + 1)
    }
}

fn execution_attempt_matches_scenario(
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    message: &MessageEnvelope,
    scenario: &scheduler::CanonicalActivationScenario,
) -> bool {
    use crate::domain::execution_protocol::{ExecutionBinding, ExecutionSourceIdentity};

    if attempt.agent_id != message.agent_id
        || attempt.source_message_id.as_deref() != Some(message.id.as_str())
        || attempt.provenance.origin != canonical_execution_origin_for_scenario(message, scenario)
        || attempt.provenance.trust != canonical_execution_trust_for_scenario(message, scenario)
    {
        return false;
    }
    match (&attempt.source.identity, &attempt.binding, scenario) {
        (
            ExecutionSourceIdentity::WorkItemContinuation {
                work_item_id: source_work_item_id,
            },
            ExecutionBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation {
                work_item_id: expected,
                ..
            },
        ) => source_work_item_id == expected && work_item_id == expected,
        (
            ExecutionSourceIdentity::RuntimeRecovery { recovery_id },
            ExecutionBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::ProviderRecovery {
                work_item_id: expected,
            },
        ) => recovery_id == &message.id && work_item_id == expected,
        (
            ExecutionSourceIdentity::InternalFollowup { message_id },
            ExecutionBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::InternalFollowup {
                work_item_id: expected,
            },
        ) => message_id == &message.id && work_item_id == expected,
        (
            ExecutionSourceIdentity::TaskResult {
                task_id,
                result_message_id,
            },
            ExecutionBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::ExactTaskRejoin {
                task_id: expected_task,
                work_item_id: expected_work_item,
                ..
            },
        ) => {
            task_id == expected_task
                && result_message_id == &message.id
                && work_item_id == expected_work_item
        }
        (
            ExecutionSourceIdentity::TriggeredWait {
                wait_id,
                trigger_message_id,
            },
            ExecutionBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::ExactWaitResume {
                owner:
                    crate::domain::scheduler::SchedulerOwner::WorkItem {
                        work_item_id: expected_work_item,
                    },
                wait_id: expected_wait,
            },
        ) => {
            wait_id == expected_wait
                && trigger_message_id == &message.id
                && work_item_id == expected_work_item
        }
        (
            ExecutionSourceIdentity::TriggeredWait {
                wait_id,
                trigger_message_id,
            },
            ExecutionBinding::AgentLifecycle { agent_id },
            scheduler::CanonicalActivationScenario::ExactWaitResume {
                owner:
                    crate::domain::scheduler::SchedulerOwner::AgentLifecycle {
                        agent_id: expected_agent,
                    },
                wait_id: expected_wait,
            },
        ) => {
            wait_id == expected_wait
                && trigger_message_id == &message.id
                && agent_id == expected_agent
        }
        (
            ExecutionSourceIdentity::QueueMessage { message_id }
            | ExecutionSourceIdentity::InternalFollowup { message_id },
            ExecutionBinding::AgentLifecycle { agent_id },
            scheduler::CanonicalActivationScenario::LifecycleExternalNudge {
                agent_id: expected_agent,
            },
        ) => message_id == &message.id && agent_id == expected_agent,
        (
            ExecutionSourceIdentity::QueueMessage { message_id },
            ExecutionBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                work_item_id: expected,
                wait_id: None,
            },
        ) => message_id == &message.id && work_item_id == expected,
        (
            ExecutionSourceIdentity::TriggeredWait {
                wait_id,
                trigger_message_id,
            },
            ExecutionBinding::WorkItem { work_item_id },
            scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                work_item_id: expected,
                wait_id: Some(expected_wait),
            },
        ) => {
            wait_id == expected_wait
                && trigger_message_id == &message.id
                && work_item_id == expected
        }
        _ => false,
    }
}

fn canonical_claim_hard_blocker(
    scenario_class: crate::domain::scheduler::SchedulerScenarioClass,
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

fn canonical_activation_origin_for_scenario(
    message: &MessageEnvelope,
    scenario: &scheduler::CanonicalActivationScenario,
) -> crate::domain::scheduler_protocol::ActivationOrigin {
    use crate::domain::scheduler_protocol::ActivationOrigin;
    match scenario {
        scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. } => {
            ActivationOrigin::System
        }
        scheduler::CanonicalActivationScenario::ProviderRecovery { .. } => {
            ActivationOrigin::RuntimeRecovery
        }
        scheduler::CanonicalActivationScenario::InternalFollowup { .. }
            if !scheduler::runtime_owned_internal_followup(message) =>
        {
            ActivationOrigin::System
        }
        scheduler::CanonicalActivationScenario::ExactTaskRejoin { .. } => ActivationOrigin::Task,
        scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput { .. } => {
            ActivationOrigin::Operator
        }
        scheduler::CanonicalActivationScenario::InternalFollowup { .. }
        | scheduler::CanonicalActivationScenario::ExactWaitResume { .. }
        | scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. } => {
            canonical_activation_origin(message)
        }
    }
}

fn canonical_activation_trust_for_scenario(
    message: &MessageEnvelope,
    scenario: &scheduler::CanonicalActivationScenario,
) -> crate::domain::scheduler_protocol::ActivationTrust {
    use crate::domain::scheduler_protocol::ActivationTrust;
    match scenario {
        scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
        | scheduler::CanonicalActivationScenario::ProviderRecovery { .. }
        | scheduler::CanonicalActivationScenario::ExactTaskRejoin { .. } => {
            ActivationTrust::RuntimeInstruction
        }
        scheduler::CanonicalActivationScenario::InternalFollowup { .. }
            if !scheduler::runtime_owned_internal_followup(message) =>
        {
            ActivationTrust::RuntimeInstruction
        }
        scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput { .. } => {
            ActivationTrust::OperatorInstruction
        }
        scheduler::CanonicalActivationScenario::InternalFollowup { .. }
        | scheduler::CanonicalActivationScenario::ExactWaitResume { .. }
        | scheduler::CanonicalActivationScenario::LifecycleExternalNudge { .. } => {
            canonical_activation_trust(message)
        }
    }
}

fn canonical_execution_origin_for_scenario(
    message: &MessageEnvelope,
    scenario: &scheduler::CanonicalActivationScenario,
) -> crate::domain::execution_protocol::ExecutionOrigin {
    use crate::domain::execution_protocol::ExecutionOrigin;
    match canonical_activation_origin_for_scenario(message, scenario) {
        crate::domain::scheduler_protocol::ActivationOrigin::Operator => ExecutionOrigin::Operator,
        crate::domain::scheduler_protocol::ActivationOrigin::Channel => ExecutionOrigin::Channel,
        crate::domain::scheduler_protocol::ActivationOrigin::Webhook => ExecutionOrigin::Webhook,
        crate::domain::scheduler_protocol::ActivationOrigin::Callback => ExecutionOrigin::Callback,
        crate::domain::scheduler_protocol::ActivationOrigin::Timer => ExecutionOrigin::Timer,
        crate::domain::scheduler_protocol::ActivationOrigin::System => ExecutionOrigin::System,
        crate::domain::scheduler_protocol::ActivationOrigin::Task => ExecutionOrigin::Task,
        crate::domain::scheduler_protocol::ActivationOrigin::RuntimeRecovery => {
            ExecutionOrigin::RuntimeRecovery
        }
    }
}

fn canonical_execution_trust_for_scenario(
    message: &MessageEnvelope,
    scenario: &scheduler::CanonicalActivationScenario,
) -> crate::domain::execution_protocol::ExecutionTrust {
    use crate::domain::execution_protocol::ExecutionTrust;
    match canonical_activation_trust_for_scenario(message, scenario) {
        crate::domain::scheduler_protocol::ActivationTrust::OperatorInstruction => {
            ExecutionTrust::OperatorInstruction
        }
        crate::domain::scheduler_protocol::ActivationTrust::RuntimeInstruction => {
            ExecutionTrust::RuntimeInstruction
        }
        crate::domain::scheduler_protocol::ActivationTrust::IntegrationSignal => {
            ExecutionTrust::IntegrationSignal
        }
        crate::domain::scheduler_protocol::ActivationTrust::ExternalEvidence => {
            ExecutionTrust::ExternalEvidence
        }
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

    fn no_progress_cause(reason: &'static str) -> QueueHeadNoProgressCause {
        use crate::domain::scheduler::SchedulerScenarioClass;

        match reason {
            "explicit_binding_work_item_missing" => QueueHeadNoProgressCause::RetainedAuthority {
                scenario_class: SchedulerScenarioClass::ExplicitlyBoundOperatorInput,
                reason,
            },
            "canonical_activation_scenario_unresolved" => {
                QueueHeadNoProgressCause::HardBlocker(CanonicalClaimHardBlocker {
                    scenario_class: SchedulerScenarioClass::ExactWaitResume,
                    blocker_code: reason,
                })
            }
            "canonical_wait_ambiguous" => QueueHeadNoProgressCause::AmbiguousWait,
            "canonical_claim_contended" => QueueHeadNoProgressCause::ClaimContended {
                scenario_class: Some(SchedulerScenarioClass::WorkItemAutonomousContinuation),
            },
            "canonical_claim_replan_exhausted" => QueueHeadNoProgressCause::ReplanExhausted,
            other => panic!("unknown queue-head no-progress test cause: {other}"),
        }
    }

    fn operator_prompt(text: impl Into<String>) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("control".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        )
        .with_admission(
            MessageDeliverySurface::HttpControlPrompt,
            AdmissionContext::ControlAuthenticated,
        )
    }

    async fn queue_candidate(runtime: &RuntimeHandle) -> QueueCandidate {
        let guard = runtime.inner.agent.lock().await;
        QueueCandidate {
            message: guard.queue.peek().cloned().expect("queued test message"),
            prior_state: guard.state.clone(),
            queue_len: guard.queue.len(),
        }
    }

    #[test]
    fn queue_head_no_progress_causes_have_total_diagnostic_mapping() {
        use crate::domain::scheduler::SchedulerScenarioClass;

        let cases = [
            (
                QueueHeadNoProgressCause::RetainedAuthority {
                    scenario_class: SchedulerScenarioClass::ExplicitlyBoundOperatorInput,
                    reason: "explicit_binding_work_item_missing",
                },
                Some(SchedulerScenarioClass::ExplicitlyBoundOperatorInput),
                "explicit_binding_work_item_missing",
            ),
            (
                QueueHeadNoProgressCause::HardBlocker(CanonicalClaimHardBlocker {
                    scenario_class: SchedulerScenarioClass::ExactWaitResume,
                    blocker_code: "canonical_activation_scenario_unresolved",
                }),
                Some(SchedulerScenarioClass::ExactWaitResume),
                "canonical_activation_scenario_unresolved",
            ),
            (
                QueueHeadNoProgressCause::AmbiguousWait,
                None,
                "canonical_wait_ambiguous",
            ),
            (
                QueueHeadNoProgressCause::ClaimContended {
                    scenario_class: Some(SchedulerScenarioClass::WorkItemAutonomousContinuation),
                },
                Some(SchedulerScenarioClass::WorkItemAutonomousContinuation),
                "canonical_claim_contended",
            ),
            (
                QueueHeadNoProgressCause::ReplanExhausted,
                None,
                "canonical_claim_replan_exhausted",
            ),
        ];

        for (cause, scenario_class, reason) in cases {
            assert_eq!(cause.scenario_class(), scenario_class);
            assert_eq!(cause.reason(), reason);
        }
    }

    #[tokio::test]
    async fn queue_head_no_progress_matrix_quarantines_across_restart_and_advances_valid_next() {
        use crate::runtime::tests::support::{context_config, CountingProvider};
        use tempfile::tempdir;

        for reason in [
            "explicit_binding_work_item_missing",
            "canonical_activation_scenario_unresolved",
            "canonical_wait_ambiguous",
            "canonical_claim_contended",
            "canonical_claim_replan_exhausted",
        ] {
            let dir = tempdir().unwrap();
            let workspace = tempdir().unwrap();
            let runtime = RuntimeHandle::new(
                "default",
                dir.path().to_path_buf(),
                workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),
                Arc::new(CountingProvider {
                    calls: Mutex::new(0),
                    reply: "unused",
                }),
                "default".into(),
                context_config(),
            )
            .unwrap();
            let blocked = runtime
                .enqueue(operator_prompt(format!("blocked by {reason}")))
                .await
                .unwrap();
            let valid = runtime
                .enqueue(operator_prompt(format!("valid after {reason}")))
                .await
                .unwrap();
            let candidate = queue_candidate(&runtime).await;
            assert_eq!(candidate.message.id, blocked.id);
            assert!(matches!(
                SchedulerDecisionExecutor::new(&runtime)
                    .defer_or_quarantine_queue_head(&candidate, no_progress_cause(reason))
                    .await
                    .unwrap(),
                RunLoopPoll::AuthorityBlocked
            ));
            drop(runtime);

            let reopened = RuntimeHandle::new(
                "default",
                dir.path().to_path_buf(),
                workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),
                Arc::new(CountingProvider {
                    calls: Mutex::new(0),
                    reply: "unused",
                }),
                "default".into(),
                context_config(),
            )
            .unwrap();
            let candidate = queue_candidate(&reopened).await;
            assert!(matches!(
                SchedulerDecisionExecutor::new(&reopened)
                    .defer_or_quarantine_queue_head(&candidate, no_progress_cause(reason))
                    .await
                    .unwrap(),
                RunLoopPoll::AuthorityBlocked
            ));
            assert!(matches!(
                SchedulerDecisionExecutor::new(&reopened)
                    .defer_or_quarantine_queue_head(&candidate, no_progress_cause(reason))
                    .await
                    .unwrap(),
                RunLoopPoll::Idle
            ));
            assert_eq!(
                reopened
                    .inner
                    .runtime_db
                    .queue_entries()
                    .latest(&blocked.id)
                    .unwrap()
                    .map(|entry| entry.status),
                Some(QueueEntryStatus::Quarantined),
                "cause {reason}"
            );

            let poll = SchedulerDecisionExecutor::new(&reopened)
                .poll()
                .await
                .unwrap();
            let RunLoopPoll::Message(scheduled) = poll else {
                panic!("valid input should advance after quarantining {reason}");
            };
            assert_eq!(scheduled.message.id, valid.id, "cause {reason}");
            assert!(reopened
                .storage()
                .read_recent_events(usize::MAX)
                .unwrap()
                .iter()
                .any(|event| {
                    event.kind == "scheduler_queue_head_quarantined"
                        && event.data["message_id"] == blocked.id
                        && event.data["reason"] == reason
                        && event.data["attempt"] == QUEUE_HEAD_NO_PROGRESS_MAX_ATTEMPTS
                }));
        }
    }

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
