use super::*;

impl RuntimeHandle {
    pub(super) async fn process_message(
        &self,
        message: MessageEnvelope,
        prior_closure: ClosureDecision,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::new(
            "message_processing_started",
            to_json_value(&message),
        ))?;

        let task = match message.kind {
            MessageKind::TaskStatus | MessageKind::TaskResult => {
                Some(tasks::task_from_message(&message, &message.agent_id)?)
            }
            _ => None,
        };
        let continuation_trigger = ContinuationTrigger::from_message(&message, task.as_ref());
        if let Some(trigger) = continuation_trigger.as_ref() {
            self.record_continuation_trigger_received(&message, trigger, &prior_closure)
                .await?;
        }
        let continuation_resolution = continuation_trigger
            .as_ref()
            .map(|trigger| resolve_continuation(&prior_closure, trigger));
        let model_visible = continuation_resolution
            .as_ref()
            .is_some_and(|resolution| resolution.model_visible);

        match message.kind {
            MessageKind::OperatorPrompt
            | MessageKind::WebhookEvent
            | MessageKind::CallbackEvent
            | MessageKind::TimerTick
            | MessageKind::SystemTick
            | MessageKind::ChannelEvent
            | MessageKind::InternalFollowup => {
                if model_visible {
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
                    model_visible,
                    continuation_resolution.as_ref(),
                )
                .await?;
            }
            MessageKind::Control => {
                let action = match &message.body {
                    MessageBody::Text { text } if text == "pause" => ControlAction::Pause,
                    MessageBody::Text { text } if text == "resume" => ControlAction::Resume,
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
                AgentStatus::Asleep | AgentStatus::Paused | AgentStatus::Stopped
            );
            if status_mutable {
                guard.state.status = if task_state_reducer::has_blocking_active_tasks(
                    &self.inner.storage,
                    &guard.state.active_task_ids,
                )? {
                    AgentStatus::AwaitingTask
                } else {
                    AgentStatus::AwakeIdle
                };
                guard.state.current_run_id = None;
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
            .is_some_and(|resolution| resolution.model_visible)
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
        self.inner
            .storage
            .append_transcript_entry(&TranscriptEntry::new(
                message.agent_id.clone(),
                TranscriptEntryKind::IncomingMessage,
                None,
                Some(message.id.clone()),
                serde_json::json!({
                    "kind": message.kind,
                    "origin": message.origin,
                    "trust": message.trust,
                    "authority_class": message.authority_class,
                    "delivery_surface": message.delivery_surface,
                    "admission_context": message.admission_context,
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
