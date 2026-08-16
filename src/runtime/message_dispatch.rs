use super::*;
use crate::types::{ExecutionAdmissionProvenance, WorkItemState};

pub(super) struct MessageDispatchPlan {
    pub(super) prior_closure: ClosureDecision,
    pub(super) task: Result<Option<TaskRecord>>,
    pub(super) continuation_trigger: Option<ContinuationTrigger>,
    pub(super) continuation_resolution: Option<ContinuationResolution>,
    pub(super) model_turn_allowed: bool,
    pub(super) execution_admission_provenance: ExecutionAdmissionProvenance,
}

impl RuntimeHandle {
    pub(super) fn build_message_dispatch_plan(
        &self,
        message: &MessageEnvelope,
        prior_closure: ClosureDecision,
        scheduler_state: &AgentState,
    ) -> Result<MessageDispatchPlan> {
        let task = match message.kind {
            MessageKind::TaskStatus => {
                tasks::task_from_message(message, &message.agent_id).map(Some)
            }
            MessageKind::TaskResult => tasks::task_from_message(message, &message.agent_id)
                .or_else(|error| {
                    let durable_task = message
                        .task_id
                        .as_deref()
                        .filter(|task_id| {
                            message.authority_class == AuthorityClass::RuntimeInstruction
                                && message.admission_context == Some(AdmissionContext::RuntimeOwned)
                                && message.delivery_surface
                                    == Some(MessageDeliverySurface::TaskRejoin)
                                && matches!(
                                    &message.origin,
                                    MessageOrigin::Task {
                                        task_id: origin_task_id
                                    } if origin_task_id == *task_id
                                )
                        })
                        .map(|task_id| self.inner.storage.latest_task_record(task_id))
                        .transpose()?
                        .flatten()
                        .filter(|task| {
                            task.agent_id == message.agent_id
                                && task.work_item_id == message.work_item_id
                                && task.parent_message_id.as_deref() == Some(message.id.as_str())
                        });
                    let Some(task) = durable_task else {
                        return Err(error);
                    };
                    let wait_authority = if let Some(work_item_id) = task.work_item_id.as_deref() {
                        exact_triggered_or_resolved_task_result_wait(
                            &self.inner.storage,
                            message,
                            &task.id,
                            work_item_id,
                        )?
                        .is_some()
                    } else {
                        false
                    };
                    if task.terminal_reentry() || wait_authority {
                        Ok(task)
                    } else {
                        Err(error)
                    }
                })
                .map(Some),
            _ => Ok(None),
        };
        let continuation_trigger =
            ContinuationTrigger::from_message(message, task.as_ref().ok().and_then(Option::as_ref));
        let matching_wait_work_item_id = if continuation_trigger.is_some() {
            let matching_waits = self
                .inner
                .storage
                .active_wait_conditions_for_agent(&message.agent_id)?
                .into_iter()
                .filter(|condition| scheduler::message_matches_wait_condition(message, condition))
                .collect::<Vec<_>>();
            (matching_waits.len() == 1)
                .then(|| matching_waits[0].work_item_id.clone())
                .flatten()
        } else {
            None
        };
        let message_work_item_id = match message.work_item_id.as_deref() {
            Some(work_item_id)
                if self
                    .inner
                    .storage
                    .latest_work_item(work_item_id)?
                    .is_some_and(|work_item| work_item.state != WorkItemState::Open) =>
            {
                None
            }
            work_item_id => work_item_id,
        };
        let continuation_work_item_id = matching_wait_work_item_id
            .as_deref()
            .or(message_work_item_id)
            .or(scheduler_state.current_turn_work_item_id.as_deref())
            .or(scheduler_state.current_work_item_id.as_deref());
        let continuation_resolution = continuation_trigger.as_ref().map(|trigger| {
            resolve_continuation(&prior_closure, trigger, continuation_work_item_id)
        });
        let model_turn_allowed = !matches!(scheduler_state.status, AgentStatus::Stopped);
        let execution_admission_provenance = self.legacy_execution_admission_provenance(
            message,
            continuation_resolution.as_ref(),
            task.as_ref().ok().and_then(Option::as_ref),
        )?;
        Ok(MessageDispatchPlan {
            prior_closure,
            task,
            continuation_trigger,
            continuation_resolution,
            model_turn_allowed,
            execution_admission_provenance,
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
        let scheduler_projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
            &self.inner.storage,
            &scheduler_state,
            scheduler_state.pending,
            self.now(),
        )?;
        let scheduler_projection = if self.inner.scheduler_engine.is_canonical() {
            scheduler_projection
        } else {
            scheduler_projection.without_canonical_authority()
        };
        let scheduler_decision = scheduler::decide_next_action(
            &scheduler_projection,
            scheduler::SchedulerBoundary::MessageProcessing,
            scheduler::SchedulerInput::Message {
                message: &message,
                model_turn_allowed: plan.model_turn_allowed,
                continuation_resolution: plan.continuation_resolution.as_ref(),
            },
        );
        scheduler::append_scheduler_decision(
            &self.inner.storage,
            &self.inner.default_agent_id,
            &scheduler_decision,
        )?;
        self.process_message_with_plan(message, plan, &scheduler_decision)
            .await
    }

    pub(super) async fn process_message_with_plan(
        &self,
        message: MessageEnvelope,
        plan: MessageDispatchPlan,
        scheduler_decision: &scheduler::SchedulerDecision,
    ) -> Result<()> {
        let transition = self
            .process_message_with_plan_deferred(message, plan, scheduler_decision)
            .await?;
        self.persist_terminal_transition(&transition).await?;
        Ok(())
    }

    pub(super) async fn process_message_with_plan_deferred(
        &self,
        mut message: MessageEnvelope,
        plan: MessageDispatchPlan,
        scheduler_decision: &scheduler::SchedulerDecision,
    ) -> Result<turn::TurnTerminalTransition> {
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
            execution_admission_provenance,
            ..
        } = plan;
        let model_reentry = scheduler_decision.model_reentry;
        let task = task?;
        let mut terminal_transition = None;
        let mut reducer_only_reason = None;
        if let Some(trigger) = continuation_trigger.as_ref() {
            self.record_continuation_trigger_received(&message, trigger, &prior_closure)
                .await?;
        }

        // Resolve wait conditions triggered by this message BEFORE the model
        // processes it.  If the message is an external trigger or operator
        // prompt that matches an active wait, the wait must transition to
        // Resolved and the WorkItem blocker cleared before the model starts
        // working.  Otherwise the model may complete the WorkItem first,
        // causing the wait to be Cancelled (work_item_completed) instead of
        // Resolved. Reconciliation failure intentionally blocks model
        // processing rather than allowing work to proceed from ambiguous wait
        // state.
        self.record_wait_reconciliation_signals(&message).await?;

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
                    terminal_transition = Some(
                        self.process_interactive_message_deferred_with_cleanup(
                            &message,
                            continuation_resolution.as_ref(),
                            execution_admission_provenance.clone(),
                            LoopControlOptions {
                                max_tool_rounds: None,
                            },
                        )
                        .await?,
                    );
                } else {
                    reducer_only_reason = Some("reducer_only/model_reentry_suppressed");
                }
            }
            MessageKind::TaskStatus => {
                let task = task.ok_or_else(|| anyhow!("task status message should parse task"))?;
                self.reduce_task_status_message(task).await?;
                reducer_only_reason = Some("reducer_only/task_status");
            }
            MessageKind::TaskResult => {
                let task = task.ok_or_else(|| anyhow!("task result message should parse task"))?;
                terminal_transition = Some(
                    self.reduce_task_result_message_deferred(
                        &message,
                        task,
                        model_reentry,
                        continuation_resolution.as_ref(),
                        execution_admission_provenance,
                    )
                    .await?,
                );
            }
            MessageKind::Control => {
                let action = match &message.body {
                    MessageBody::Text { text } if text == "start" => ControlAction::Start,
                    MessageBody::Text { text } if text == "stop" => ControlAction::Stop,
                    _ => return Err(anyhow!("unknown control action")),
                };
                self.control(action).await?;
                reducer_only_reason = Some("reducer_only/control");
            }
            MessageKind::BriefAck | MessageKind::BriefResult => {
                reducer_only_reason = Some("reducer_only/brief_notification");
            }
        }

        if terminal_transition
            .as_ref()
            .is_some_and(|transition| transition.prepared_work_item_completion.is_some())
        {
            return Ok(terminal_transition.expect("checked prepared completion"));
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
        match terminal_transition {
            Some(transition) => Ok(transition),
            None => {
                self.build_reducer_only_terminal_transition(
                    reducer_only_reason.unwrap_or("reducer_only/message_consumed"),
                )
                .await
            }
        }
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
        let commit = self.commit_work_item_transition(
            &crate::runtime_db::transitions::WorkItemTransitionCommand {
                agent_id: agent.id.clone(),
                mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                    record: record.clone(),
                    expected_revision: record.revision - 1,
                },
                agent_state: None,
                brief_evidence: Vec::new(),
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
