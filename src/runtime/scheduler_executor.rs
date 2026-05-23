use super::message_dispatch::MessageDispatchPlan;
use super::*;

pub(super) enum RunLoopPoll {
    Shutdown,
    Stopped(AgentState, usize),
    Message(ScheduledMessage),
    Idle,
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
        self.runtime.inner.storage.write_agent(&guard.state)?;

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
            self.runtime.inner.storage.write_agent(&guard.state)?;
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
            self.runtime.inner.storage.write_agent(&guard.state)?;
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
        self.runtime.inner.storage.write_agent(&guard.state)?;
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
        scheduler::apply_sleep_projection(&mut guard.state, sleeping_until);
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
        self.runtime.inner.storage.write_agent(&guard.state)?;
        Ok(Some(guard.state.clone()))
    }

    pub(super) async fn admit_message_wake(
        &self,
        message: &MessageEnvelope,
    ) -> Result<Option<AgentState>> {
        let mut guard = self.runtime.inner.agent.lock().await;
        let previous_status = guard.state.status.clone();
        let previous_sleeping_until = guard.state.sleeping_until;
        if !scheduler::apply_message_wake_projection(&mut guard.state) {
            return Ok(None);
        }
        self.append_posture_decision(
            "message_admission",
            "message_admission_wake",
            &previous_status,
            &guard.state.status,
            vec![
                format!("message_id={}", message.id),
                format!("message_kind={:?}", message.kind),
                format!("previous_sleeping_until={previous_sleeping_until:?}"),
            ],
        )?;
        self.runtime.inner.storage.write_agent(&guard.state)?;
        Ok(Some(guard.state.clone()))
    }

    pub(super) async fn poll(&self) -> Result<RunLoopPoll> {
        let candidate = {
            let guard = self.runtime.inner.agent.lock().await;
            if self.runtime.inner.shutdown_requested.load(Ordering::SeqCst) {
                return self.shutdown(guard);
            }
            if guard.state.status == AgentStatus::Stopped {
                return Ok(RunLoopPoll::Stopped(guard.state.clone(), guard.queue.len()));
            }
            let Some(message) = guard.queue.peek().cloned() else {
                return Ok(RunLoopPoll::Idle);
            };
            QueueCandidate {
                message,
                prior_state: guard.state.clone(),
                queue_len: guard.queue.len(),
            }
        };

        self.prepare_message(candidate).await
    }

    fn shutdown(
        &self,
        mut guard: tokio::sync::MutexGuard<'_, RuntimeAgent>,
    ) -> Result<RunLoopPoll> {
        guard.state.current_run_id = None;
        self.runtime.inner.storage.write_agent(&guard.state)?;
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
        let projection = scheduler::SchedulerProjection::from_state_with_queue_len(
            &self.runtime.inner.storage,
            &candidate.prior_state,
            candidate.queue_len,
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
        scheduler::append_scheduling_diagnostics(
            &self.runtime.inner.storage,
            &candidate.prior_state,
            candidate.queue_len,
        )?;

        let (message, running_state) = {
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

            scheduler::append_scheduler_decision(&self.runtime.inner.storage, &decision)?;
            let message = guard
                .queue
                .pop_if_next(&candidate.message.id)
                .expect("queue head was just checked");
            let run_id = Uuid::new_v4().to_string();
            let abort_token = CancellationToken::new();
            guard.state.pending = guard.queue.len();
            scheduler::apply_running_projection(&mut guard.state, run_id.clone());
            guard.current_run_abort = Some(CurrentRunAbortHandle {
                run_id: run_id.clone(),
                token: abort_token,
                reason: Arc::new(StdMutex::new("operator_aborted".into())),
            });
            guard.state.last_wake_reason = Some(format!("{:?}", message.kind));
            self.runtime.inner.storage.write_agent(&guard.state)?;
            (message, guard.state.clone())
        };

        Ok(RunLoopPoll::Message(ScheduledMessage {
            message,
            running_state,
            dispatch_plan,
            scheduler_decision: decision,
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
        self.runtime.inner.storage.append_event(&AuditEvent::new(
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
}
