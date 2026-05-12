use super::message_dispatch::MessageDispatchPlan;
use super::*;

pub(super) enum RunLoopPoll {
    Shutdown,
    Stopped(AgentState, usize),
    Message(ScheduledMessage),
    Idle,
}

pub(super) struct ScheduledMessage {
    pub(super) message: MessageEnvelope,
    pub(super) running_state: AgentState,
    pub(super) dispatch_plan: MessageDispatchPlan,
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

    pub(super) async fn poll(&self) -> Result<RunLoopPoll> {
        let candidate = {
            let guard = self.runtime.inner.agent.lock().await;
            if self.runtime.inner.shutdown_requested.load(Ordering::SeqCst) {
                return self.shutdown(guard);
            }
            if guard.state.status == AgentStatus::Stopped {
                return Ok(RunLoopPoll::Stopped(guard.state.clone(), guard.queue.len()));
            }
            if guard.state.status == AgentStatus::Paused {
                return Ok(RunLoopPoll::Idle);
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
                model_visible: dispatch_plan.model_visible,
            },
        );

        let (message, running_state) = {
            let mut guard = self.runtime.inner.agent.lock().await;
            if self.runtime.inner.shutdown_requested.load(Ordering::SeqCst) {
                return self.shutdown(guard);
            }
            if matches!(
                guard.state.status,
                AgentStatus::Paused | AgentStatus::Stopped
            ) {
                return Ok(if guard.state.status == AgentStatus::Stopped {
                    RunLoopPoll::Stopped(guard.state.clone(), guard.queue.len())
                } else {
                    RunLoopPoll::Idle
                });
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
            let interrupt_token = CancellationToken::new();
            guard.state.pending = guard.queue.len();
            scheduler::apply_running_projection(&mut guard.state, run_id.clone());
            guard.current_run_interrupt = Some(CurrentRunInterruptHandle {
                run_id: run_id.clone(),
                token: interrupt_token,
                reason: Arc::new(StdMutex::new("operator_interrupted".into())),
            });
            guard.state.last_wake_reason = Some(format!("{:?}", message.kind));
            self.runtime.inner.storage.write_agent(&guard.state)?;
            (message, guard.state.clone())
        };

        Ok(RunLoopPoll::Message(ScheduledMessage {
            message,
            running_state,
            dispatch_plan,
        }))
    }
}
