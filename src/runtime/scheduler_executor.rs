use super::message_dispatch::MessageDispatchPlan;
use super::*;

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
    scheduler_claim_work_item: Option<crate::types::WorkItemRecord>,
    bootstrap: Option<crate::domain::scheduler_protocol::Snapshot>,
    commands: Vec<crate::domain::scheduler_protocol::ProtocolCommand>,
    rollout_expectations: Vec<
        crate::runtime_db::transitions::scheduler_protocol_repository::SchedulerRolloutExpectation,
    >,
}

pub(super) struct SchedulerDecisionExecutor<'a> {
    runtime: &'a RuntimeHandle,
}

struct QueueCandidate {
    message: MessageEnvelope,
    prior_state: AgentState,
    queue_len: usize,
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
        let dispatch_plan = self.runtime.build_message_dispatch_plan(
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
        let decision = scheduler::decide_next_action(
            &projection,
            scheduler::SchedulerBoundary::RunLoop,
            scheduler::SchedulerInput::Message {
                message: &candidate.message,
                model_turn_allowed: dispatch_plan.model_turn_allowed,
                continuation_resolution: dispatch_plan.continuation_resolution.as_ref(),
            },
        );
        let scheduler_authority_scenarios = scheduler::authority_scenarios_for_message_claim(
            &projection,
            &candidate.message,
            dispatch_plan.continuation_resolution.as_ref(),
        );
        let production_commands_enabled = self
            .runtime
            .scheduler_protocol_production_commands_enabled();
        let mut scheduler_rollout_expectations = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .scheduler_rollout_expectations(
                &scheduler_authority_scenarios,
                production_commands_enabled,
            )?;
        let shadow_comparison = scheduler::shadow_comparison_for_message_admission(
            &projection,
            &candidate.message,
            &decision,
            dispatch_plan.continuation_resolution.as_ref(),
        )
        .or_else(|| {
            scheduler::shadow_comparison_for_wait_resume(&projection, &candidate.message, &decision)
        });
        let scheduler_decision_events = scheduler::scheduler_decision_events(
            &candidate.message.agent_id,
            &decision,
            shadow_comparison.as_ref(),
        )?;
        let shadow_comparison = shadow_comparison
            .map(scheduler_shadow_comparison_command)
            .transpose()?;
        #[cfg(test)]
        let shadow_comparison = if self
            .runtime
            .inner
            .omit_next_scheduler_claim_shadow_comparison
            .swap(false, Ordering::SeqCst)
        {
            None
        } else {
            shadow_comparison
        };
        let persisted_message = self
            .runtime
            .inner
            .storage
            .read_message_by_id(&candidate.message.id)?
            .ok_or_else(|| anyhow!("claimed message is missing persisted ingress evidence"))?;
        let semantic_shadow = scheduler::semantic_shadow_decision_for_message_admission(
            &projection,
            &persisted_message,
        )?
        .map(scheduler_semantic_shadow_command);
        let canonical_claim = match self.canonical_activation_plan(
            &projection,
            &persisted_message,
            &dispatch_plan,
            production_commands_enabled,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                if let Some(ambiguous) = error.downcast_ref::<scheduler::AmbiguousCanonicalWaits>()
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
        };
        if let Some(plan) = canonical_claim.as_ref() {
            for expectation in &plan.rollout_expectations {
                if !scheduler_rollout_expectations
                    .iter()
                    .any(|candidate| candidate.scenario_class == expectation.scenario_class)
                {
                    scheduler_rollout_expectations.push(expectation.clone());
                }
            }
        }
        scheduler::append_scheduling_advisories(
            &self.runtime.inner.storage,
            &candidate.prior_state,
            candidate.queue_len,
        )?;

        let (message, running_state, transition_commit) = {
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
            let mut running_state = guard.state.clone();
            running_state.pending = guard.queue.len().saturating_sub(1);
            scheduler::apply_running_projection(&mut running_state, run_id.clone());
            running_state.last_wake_reason = Some(format!("{:?}", candidate.message.kind));
            let mut commit = self.runtime.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: candidate.message.agent_id.clone(),
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
                    scheduler_authority_scenarios,
                    scheduler_rollout_expectations,
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(guard.state.clone())),
                        record: Box::new(running_state.clone()),
                    }),
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: vec![
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
                    ],
                    scheduler_shadow_comparison: shadow_comparison,
                    scheduler_delivery_shadow_comparison: None,
                    scheduler_semantic_shadow: semantic_shadow,
                    notify_scheduler: false,
                    fault: self.runtime.take_transition_fault(),
                    brief_evidence: Vec::new(),
                },
            )?;
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
            let message = guard
                .queue
                .pop_if_next(&candidate.message.id)
                .expect("queue head was just checked");
            guard.state = running_state.clone();
            guard.last_persisted_state = running_state.clone();
            guard.current_run_abort = Some(CurrentRunAbortHandle {
                run_id: run_id.clone(),
                token: abort_token,
                reason: Arc::new(StdMutex::new("operator_aborted".into())),
            });
            commit.effects.agent_state = None;
            (message, running_state, commit)
        };
        self.runtime
            .apply_transition_commit(transition_commit)
            .await;

        Ok(RunLoopPoll::Message(ScheduledMessage {
            message,
            running_state,
            dispatch_plan,
            scheduler_decision: decision,
        }))
    }

    fn canonical_activation_plan(
        &self,
        projection: &scheduler::SchedulerProjection,
        message: &MessageEnvelope,
        dispatch_plan: &MessageDispatchPlan,
        production_commands_enabled: bool,
    ) -> Result<Option<CanonicalClaimPlan>> {
        if !production_commands_enabled {
            return Ok(None);
        }
        let task = dispatch_plan.task.as_ref().ok().and_then(Option::as_ref);
        let Some(mut scenario) = scheduler::canonical_activation_scenario(
            projection,
            message,
            dispatch_plan.continuation_resolution.as_ref(),
            task,
        )?
        else {
            return Ok(None);
        };
        let rollout_expectations = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .scheduler_rollout_expectations(
                &[scenario.scenario_class(), scheduler::SETTLEMENT_SCENARIO],
                production_commands_enabled,
            )?;
        if rollout_expectations.iter().any(|expectation| {
            expectation.mode != crate::domain::scheduler_protocol::ScenarioMode::Authoritative
        }) {
            return Ok(None);
        }

        use crate::domain::scheduler_protocol::WorkStatus;

        let existing = self
            .runtime
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot_if_initialized(&message.agent_id)?;
        if let scheduler::CanonicalActivationScenario::ExactTaskRejoin {
            work_item_id,
            wait_id,
            ..
        } = &mut scenario
        {
            if wait_id.is_none() {
                let authoritative_wait_id = existing
                    .as_ref()
                    .and_then(|snapshot| snapshot.work.get(work_item_id))
                    .and_then(|demand| match &demand.status {
                        WorkStatus::Waiting { wait_id } => Some(wait_id),
                        _ => None,
                    });
                if let Some(authoritative_wait_id) = authoritative_wait_id {
                    let resolved_legacy_wait_matches = self
                        .runtime
                        .inner
                        .storage
                        .latest_wait_conditions()?
                        .into_iter()
                        .any(|condition| {
                            condition.id == *authoritative_wait_id
                                && condition.agent_id == message.agent_id
                                && condition.work_item_id.as_deref() == Some(work_item_id.as_str())
                                && condition.status == crate::types::WaitConditionStatus::Resolved
                                && condition.kind == crate::types::WaitConditionKind::Task
                                && scheduler::message_matches_wait_condition(message, &condition)
                        });
                    if resolved_legacy_wait_matches {
                        *wait_id = Some(authoritative_wait_id.clone());
                    }
                }
            }
        }

        let work_item_id = scenario.work_item_id();
        let work_item = self
            .runtime
            .inner
            .storage
            .latest_work_item(work_item_id)?
            .ok_or_else(|| anyhow!("canonical activation references unknown WorkItem"))?;
        let work_queue = self.runtime.inner.storage.work_queue_prompt_projection()?;
        let work_projection = work_queue
            .items
            .iter()
            .find(|candidate| candidate.id == work_item.id)
            .ok_or_else(|| anyhow!("canonical activation has no WorkItem scheduling projection"))?;
        if work_item.agent_id != message.agent_id
            || work_item.state != crate::types::WorkItemState::Open
        {
            return Err(anyhow!(
                "canonical activation requires an open same-agent WorkItem"
            ));
        }
        if matches!(
            scenario,
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
        ) && work_projection.scheduling_state != crate::types::WorkItemSchedulingState::Runnable
        {
            return Err(anyhow!(
                "canonical autonomous activation requires a runnable WorkItem"
            ));
        }
        let activation_id = canonical_activation_id(&message.id);

        use crate::domain::scheduler_protocol::{
            ActivationBinding, ActivationCause, ActivationLifecycleState, ActivationOrigin,
            ActivationPriority, ActivationProvenance, ActivationSlot, ActivationTrust,
            AdmitActivationCommand, AgentActivation, AgentDispatchState,
            IssueActivationAuthorityCommand, PreemptionPolicy, ProtocolCommand,
            RegisterWorkDemandCommand, RolloutState, Snapshot, TriggerWaitCommand, WaitResumeClaim,
            WorkDemand,
        };

        if let Some(snapshot) = existing.as_ref() {
            if let Some(activation) = snapshot.activations.get(&activation_id) {
                let slot_matches = matches!(
                    &snapshot.slot,
                    ActivationSlot::Running {
                        activation_id: running_activation_id,
                        work_item_id: running_work_item_id,
                        ..
                    } if running_activation_id == &activation_id
                        && running_work_item_id == work_item_id
                );
                if activation.state == crate::domain::scheduler_protocol::ActivationState::Running
                    && activation.work_item_id == work_item_id
                    && slot_matches
                    && snapshot
                        .activation_admissions
                        .get(&activation_id)
                        .is_some_and(|admission| {
                            canonical_admission_matches_scenario(admission, message, &scenario)
                        })
                {
                    return Ok(Some(CanonicalClaimPlan {
                        scheduler_claim_work_item: matches!(
                            scenario,
                            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation {
                                ..
                            }
                        )
                        .then_some(work_item),
                        bootstrap: None,
                        commands: Vec::new(),
                        rollout_expectations,
                    }));
                }
                return Err(anyhow!(
                    "canonical work queue replay references a non-running activation"
                ));
            }
        }
        let new_demand = || WorkDemand {
            metadata_revision: work_item.revision.max(1),
            scheduling_generation: work_item.revision.max(1),
            status: WorkStatus::Runnable,
            capabilities: Default::default(),
            locks: Default::default(),
            locality: "runtime".into(),
            cost_class: "default".into(),
        };
        let wait_id = match &scenario {
            scheduler::CanonicalActivationScenario::ExactTaskRejoin { wait_id, .. }
            | scheduler::CanonicalActivationScenario::ExplicitlyBoundOperatorInput {
                wait_id,
                ..
            } => wait_id.as_deref(),
            scheduler::CanonicalActivationScenario::ExactWaitResume { wait_id, .. } => {
                Some(wait_id.as_str())
            }
            scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. } => None,
        };
        let (bootstrap, expected_dispatch_revision, scheduling_generation, register) =
            if let Some(snapshot) = existing.as_ref() {
                if let Some(demand) = snapshot.work.get(work_item_id) {
                    if wait_id.is_none() && demand.status != WorkStatus::Runnable {
                        return Err(anyhow!(
                            "canonical WorkItem demand is not runnable for activation"
                        ));
                    }
                    (
                        None,
                        snapshot.dispatch_revision,
                        demand.scheduling_generation,
                        None,
                    )
                } else {
                    if wait_id.is_some() {
                        return Err(anyhow!(
                            "canonical wait resume requires an existing WorkItem demand"
                        ));
                    }
                    let demand = new_demand();
                    (
                        None,
                        snapshot.dispatch_revision,
                        demand.scheduling_generation,
                        Some(ProtocolCommand::RegisterWorkDemand(
                            RegisterWorkDemandCommand {
                                work_item_id: work_item.id.clone(),
                                demand,
                            },
                        )),
                    )
                }
            } else {
                if wait_id.is_some() {
                    return Err(anyhow!(
                        "canonical wait resume requires an initialized protocol partition"
                    ));
                }
                let demand = new_demand();
                (
                    Some(Snapshot {
                        slot: ActivationSlot::Idle,
                        dispatch: AgentDispatchState::Open,
                        dispatch_revision: 0,
                        focus: None,
                        work: Default::default(),
                        waits: Default::default(),
                        activations: Default::default(),
                        activation_authorities: Default::default(),
                        activation_admissions: Default::default(),
                        settlements: Default::default(),
                        missing_settlements: Default::default(),
                        rollout: RolloutState::default(),
                        admitted_generations: Default::default(),
                        continuation_admissions: Default::default(),
                        activation_inputs: Default::default(),
                    }),
                    0,
                    demand.scheduling_generation,
                    Some(ProtocolCommand::RegisterWorkDemand(
                        RegisterWorkDemandCommand {
                            work_item_id: work_item.id.clone(),
                            demand,
                        },
                    )),
                )
            };

        let resume = wait_id
            .map(|wait_id| -> Result<WaitResumeClaim> {
                let snapshot = existing.as_ref().ok_or_else(|| {
                    anyhow!("canonical wait resume requires an initialized snapshot")
                })?;
                let wait = snapshot
                    .waits
                    .get(wait_id)
                    .ok_or_else(|| anyhow!("canonical activation references unknown wait"))?;
                let generation = wait
                    .generations
                    .get(&wait.current_generation)
                    .ok_or_else(|| anyhow!("canonical wait has no current generation"))?;
                if generation.owner_work_item_id != work_item_id {
                    return Err(anyhow!(
                        "canonical wait owner does not match WorkItem binding"
                    ));
                }
                let trigger_generation = message.message_seq.ok_or_else(|| {
                    anyhow!("canonical activation requires persisted message sequence")
                })?;
                Ok(WaitResumeClaim {
                    wait_id: wait_id.to_string(),
                    wait_generation: wait.current_generation,
                    trigger_id: canonical_wait_trigger_id(message),
                    trigger_generation,
                })
            })
            .transpose()?;
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
                format!("work-queue-message:{}", message.id),
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
                format!("task-rejoin:{task_id}"),
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
                        owner_work_item_id: work_item.id.clone(),
                    },
                    canonical_activation_origin(message),
                    canonical_activation_trust(message),
                    format!("wait-resume:{}:{}", wait_id, resume.wait_generation),
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
                format!("operator-message:{}", message.id),
            ),
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
        let authority_id = format!("authority:{activation_id}");
        let authority = IssueActivationAuthorityCommand {
            authority_id: authority_id.clone(),
            activation: activation.clone(),
            expected_scheduling_generation: scheduling_generation,
            expected_dispatch_revision,
        };
        let admission = AdmitActivationCommand {
            authority_id,
            activation,
            expected_scheduling_generation: scheduling_generation,
            expected_dispatch_revision,
        };
        let mut commands = Vec::with_capacity(4);
        commands.extend(register);
        if let Some(resume) = resume {
            commands.push(ProtocolCommand::TriggerWait(TriggerWaitCommand {
                wait_id: resume.wait_id,
                wait_generation: resume.wait_generation,
                trigger_id: resume.trigger_id,
                trigger_generation: resume.trigger_generation,
            }));
        }
        commands.push(ProtocolCommand::IssueActivationAuthority(authority));
        commands.push(ProtocolCommand::AdmitActivation(admission));

        Ok(Some(CanonicalClaimPlan {
            scheduler_claim_work_item: matches!(
                scenario,
                scheduler::CanonicalActivationScenario::WorkItemAutonomousContinuation { .. }
            )
            .then_some(work_item),
            bootstrap,
            commands,
            rollout_expectations,
        }))
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

fn canonical_wait_trigger_id(message: &MessageEnvelope) -> String {
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
                owner_work_item_id,
            },
            scheduler::CanonicalActivationScenario::ExactWaitResume {
                work_item_id,
                wait_id: expected_wait,
            },
        ) => {
            wait_id == expected_wait
                && bound_wait_id == expected_wait
                && owner_work_item_id == work_item_id
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

fn scheduler_semantic_shadow_command(
    decision: scheduler::SchedulerSemanticShadowDecision,
) -> crate::runtime_db::transitions::scheduler_protocol_repository::SchedulerSemanticShadowCommand {
    crate::runtime_db::transitions::scheduler_protocol_repository::SchedulerSemanticShadowCommand {
        input: decision.input,
        provider: decision.provider,
        response: decision.response,
        policy: decision.policy,
    }
}

pub(super) fn scheduler_shadow_comparison_command(
    comparison: scheduler::SchedulerShadowComparison,
) -> Result<
    crate::runtime_db::transitions::scheduler_protocol_repository::SchedulerShadowComparisonCommand,
> {
    Ok(
        crate::runtime_db::transitions::scheduler_protocol_repository::SchedulerShadowComparisonCommand {
            scenario_class: comparison.scenario_class.as_str().into(),
            comparison_identity: comparison.comparison_identity,
            boundary: comparison.boundary.into(),
            input_identity: comparison.input_identity,
            legacy_observation: serde_json::to_value(comparison.legacy_observation)?,
            shadow_candidate: serde_json::to_value(comparison.shadow_candidate)?,
            matched: comparison.matched,
            divergence_code: comparison.divergence_code.map(Into::into),
        },
    )
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
