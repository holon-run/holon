use super::*;

pub(super) struct MessageDispatchPlan {
    pub(super) prior_closure: ClosureDecision,
    pub(super) task: Result<Option<TaskRecord>>,
    pub(super) continuation_trigger: Option<ContinuationTrigger>,
    pub(super) continuation_resolution: Option<ContinuationResolution>,
    pub(super) model_turn_allowed: bool,
}

impl RuntimeHandle {
    pub(super) fn build_message_dispatch_plan(
        &self,
        message: &MessageEnvelope,
        prior_closure: ClosureDecision,
        scheduler_state: &AgentState,
    ) -> Result<MessageDispatchPlan> {
        let task = match message.kind {
            MessageKind::TaskStatus | MessageKind::TaskResult => {
                tasks::task_from_message(message, &message.agent_id).map(Some)
            }
            _ => Ok(None),
        };
        let continuation_trigger =
            ContinuationTrigger::from_message(message, task.as_ref().ok().and_then(Option::as_ref));
        let continuation_resolution = continuation_trigger.as_ref().map(|trigger| {
            resolve_continuation(
                &prior_closure,
                trigger,
                scheduler_state.current_work_item_id.as_deref(),
            )
        });
        let model_turn_allowed = !matches!(scheduler_state.status, AgentStatus::Stopped);
        Ok(MessageDispatchPlan {
            prior_closure,
            task,
            continuation_trigger,
            continuation_resolution,
            model_turn_allowed,
        })
    }

    // Tests and direct runtime probes still exercise the per-message entrypoint.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn process_message(
        &self,
        message: MessageEnvelope,
        prior_closure: ClosureDecision,
    ) -> Result<()> {
        let scheduler_state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let plan = self.build_message_dispatch_plan(&message, prior_closure, &scheduler_state)?;
        let scheduler_projection =
            scheduler::SchedulerProjection::from_state(&self.inner.storage, &scheduler_state)?;
        let scheduler_decision = scheduler::decide_next_action(
            &scheduler_projection,
            scheduler::SchedulerBoundary::MessageProcessing,
            scheduler::SchedulerInput::Message {
                message: &message,
                model_turn_allowed: plan.model_turn_allowed,
                continuation_resolution: plan.continuation_resolution.as_ref(),
            },
        );
        scheduler::append_scheduler_decision(&self.inner.storage, &scheduler_decision)?;
        self.process_message_with_plan(message, plan, &scheduler_decision)
            .await
    }

    pub(super) async fn process_message_with_plan(
        &self,
        mut message: MessageEnvelope,
        plan: MessageDispatchPlan,
        scheduler_decision: &scheduler::SchedulerDecision,
    ) -> Result<()> {
        message.normalize_admission_fields();
        self.inner.storage.append_event(&AuditEvent::new(
            "message_processing_started",
            to_json_value(&message),
        ))?;
        let MessageDispatchPlan {
            prior_closure,
            task,
            continuation_trigger,
            continuation_resolution,
            ..
        } = plan;
        let model_reentry = scheduler_decision.model_reentry;
        let task = task?;
        if let Some(trigger) = continuation_trigger.as_ref() {
            self.record_continuation_trigger_received(&message, trigger, &prior_closure)
                .await?;
        }

        match message.kind {
            MessageKind::OperatorPrompt
            | MessageKind::WebhookEvent
            | MessageKind::CallbackEvent
            | MessageKind::TimerTick
            | MessageKind::SystemTick
            | MessageKind::ChannelEvent
            | MessageKind::InternalFollowup => {
                if model_reentry {
                    if let Some(work_item_id) = message.work_item_id.as_deref() {
                        let mut guard = self.inner.agent.lock().await;
                        guard.state.current_turn_work_item_id = Some(work_item_id.to_string());
                        self.inner.storage.write_agent(&guard.state)?;
                    }
                    self.process_interactive_message(
                        &message,
                        continuation_resolution.as_ref(),
                        LoopControlOptions {
                            max_tool_rounds: None,
                        },
                    )
                    .await?;
                }
            }
            MessageKind::TaskStatus => {
                let task = task.expect("task status message should parse task");
                self.reduce_task_status_message(task).await?;
            }
            MessageKind::TaskResult => {
                let task = task.expect("task result message should parse task");
                self.reduce_task_result_message(
                    &message,
                    task,
                    model_reentry,
                    continuation_resolution.as_ref(),
                )
                .await?;
            }
            MessageKind::Control => {
                let action = match &message.body {
                    MessageBody::Text { text } if text == "start" => ControlAction::Start,
                    MessageBody::Text { text } if text == "stop" => ControlAction::Stop,
                    _ => return Err(anyhow!("unknown control action")),
                };
                self.control(action).await?;
            }
            MessageKind::BriefAck | MessageKind::BriefResult => {}
        }

        if let Some(resolution) = continuation_resolution.as_ref() {
            self.persist_last_continuation(resolution).await?;
            self.record_continuation_resolution_event(&message, resolution)
                .await?;
        }

        {
            let mut guard = self.inner.agent.lock().await;
            let status_mutable = !matches!(
                guard.state.status,
                AgentStatus::Asleep | AgentStatus::Stopped
            );
            if status_mutable {
                scheduler::apply_idle_projection(&mut guard.state, &self.inner.storage)?;
            }
            if status_mutable || matches!(message.kind, MessageKind::TaskResult) {
                self.inner.storage.write_agent(&guard.state)?;
            }
        }

        self.maybe_commit_turn_end_work_item_transition().await?;
        self.maybe_emit_pending_system_tick(continuation_resolution.as_ref())
            .await?;
        if continuation_resolution
            .as_ref()
            .is_some_and(|resolution| resolution.model_reentry)
        {
            self.arm_continue_active_suppression().await;
        }
        let pre_cleanup_closure = self.current_closure_decision().await?;
        self.reconcile_waiting_contract(&message, &pre_cleanup_closure)
            .await?;
        let final_closure = self.current_closure_decision().await?;
        {
            let mut guard = self.inner.agent.lock().await;
            let memory_refresh = refresh_working_memory(
                &self.inner.storage,
                &mut guard.state,
                &message,
                &prior_closure,
                &final_closure,
            )?;
            let episode_changed = refresh_episode_memory(
                &self.inner.storage,
                &mut guard.state,
                &message,
                &prior_closure,
                &final_closure,
                &memory_refresh.previous_snapshot,
                &memory_refresh.current_snapshot,
                &memory_refresh.turn_memory_delta,
            )?;
            if memory_refresh.working_memory_updated || episode_changed {
                self.inner.storage.write_agent(&guard.state)?;
            }
        }
        self.inner.storage.append_event(&AuditEvent::new(
            "closure_decided",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "closure": final_closure,
            }),
        ))?;

        info!("processed message {}", message.id);
        Ok(())
    }

    pub(super) fn record_incoming_transcript_entry(&self, message: &MessageEnvelope) -> Result<()> {
        self.persist_transcript_evidence(&TranscriptEntry::new(
            message.agent_id.clone(),
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(message.id.clone()),
            serde_json::json!({
                "kind": message.kind,
                "origin": message.origin,
                "authority_class": message.authority_class,
                "delivery_surface": message.delivery_surface,
                "admission_context": message.admission_context,
                "trigger_kind": message.trigger_kind,
                "work_item_id": message.work_item_id.clone(),
                "task_id": message.task_id.clone(),
                "source_refs": message.source_refs.clone(),
                "priority": message.priority,
                "body": message.body,
                "metadata": message.metadata,
                "correlation_id": message.correlation_id,
                "causation_id": message.causation_id,
            }),
        ))
    }
}

pub(super) fn message_text(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => value.to_string(),
        MessageBody::Brief { text, .. } => text.clone(),
    }
}
