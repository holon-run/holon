use super::*;
use crate::types::WorkItemState;

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
        self.inner.storage.append_event(&AuditEvent::typed(
            RuntimeEventKind::MessageProcessingStarted,
            &MessageLifecycleAuditEvent::from_message(&message),
        )?)?;
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
                        guard.persist_state(&self.inner.storage)?;
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
                let task = task.ok_or_else(|| anyhow!("task status message should parse task"))?;
                self.reduce_task_status_message(task).await?;
            }
            MessageKind::TaskResult => {
                let task = task.ok_or_else(|| anyhow!("task result message should parse task"))?;
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
                guard.persist_state(&self.inner.storage)?;
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
        self.record_wait_reconciliation_signals(&message).await?;
        let final_closure = self.current_closure_decision().await?;
        {
            let mut guard = self.inner.agent.lock().await;
            let work_refs_changed = self
                .refresh_current_work_item_refs(&mut guard.state, &message)
                .await?;
            let memory_refresh =
                refresh_working_memory(&self.inner.storage, &mut guard.state, &final_closure)?;
            let episode_changed = refresh_episode_memory(
                &self.inner.storage,
                &mut guard.state,
                &message,
                &prior_closure,
                &final_closure,
                &memory_refresh.previous_snapshot,
                &memory_refresh.current_snapshot,
            )?;
            if work_refs_changed || memory_refresh.working_memory_updated || episode_changed {
                guard.persist_state(&self.inner.storage)?;
            }
        }
        self.inner.storage.append_event(&AuditEvent::legacy(
            "closure_decided",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "closure": final_closure,
            }),
        ))?;

        info!("processed message {}", message.id);
        Ok(())
    }

    async fn refresh_current_work_item_refs(
        &self,
        agent: &mut AgentState,
        message: &MessageEnvelope,
    ) -> Result<bool> {
        let Some(work_item_id) = agent
            .current_turn_work_item_id
            .as_deref()
            .or(agent.current_work_item_id.as_deref())
        else {
            return Ok(false);
        };
        let Some(mut record) = self.inner.storage.latest_work_item(work_item_id)? else {
            return Ok(false);
        };
        if record.state != WorkItemState::Open {
            return Ok(false);
        }

        let tools = self.inner.storage.read_recent_tool_executions(64)?;
        let mut additions = crate::work_item_refs::message_work_refs(message);
        additions.extend(crate::work_item_refs::current_turn_tool_refs(
            &tools,
            agent.current_turn_id.as_deref(),
            agent.turn_index,
            &record.id,
        ));
        if additions.is_empty() {
            return Ok(false);
        }

        let merged = crate::work_item_refs::merge_work_refs(&record.work_refs, additions);
        if merged == record.work_refs {
            return Ok(false);
        }
        let previous_count = record.work_refs.len();
        record.work_refs = merged;
        record.revision = record.revision.saturating_add(1);
        record.updated_at = Utc::now();
        let commit = self.inner.runtime_db.transitions().commit_work_item(
            &crate::runtime_db::transitions::WorkItemTransitionCommand {
                agent_id: agent.id.clone(),
                mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                    record: record.clone(),
                    expected_revision: record.revision - 1,
                },
                agent_state: None,
                audit_events: vec![AuditEvent::legacy(
                    "work_item_refs_updated",
                    serde_json::json!({
                        "agent_id": agent.id,
                        "work_item_id": record.id,
                        "revision": record.revision,
                        "previous_ref_count": previous_count,
                        "ref_count": record.work_refs.len(),
                    }),
                )],
                index_changes: self.inner.storage.index_changes_for_work_item(&record)?,
                notify_scheduler: false,
                fault: self.take_transition_fault(),
            },
        )?;
        self.apply_transition_commit(commit).await;
        Ok(true)
    }

    pub(super) fn record_incoming_transcript_entry(&self, message: &MessageEnvelope) -> Result<()> {
        self.persist_transcript_evidence(&TranscriptEntry::new(
            message.agent_id.clone(),
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(message.id.clone()),
            serde_json::json!({
                "authority_class": message.authority_class,
                "delivery_surface": message.delivery_surface,
                "admission_context": message.admission_context,
                "trigger_kind": message.trigger_kind,
                "work_item_id": message.work_item_id.clone(),
                "task_id": message.task_id.clone(),
                "source_refs": message.source_refs.clone(),
                "correlation_id": message.correlation_id.clone(),
                "causation_id": message.causation_id.clone(),
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
