#[path = "task_status_clock.rs"]
mod status_clock;

use super::*;
use crate::types::ExecutionAdmissionProvenance;
use std::{future::Future, pin::Pin};

pub(super) struct MessageDispatchPlan {
    pub(super) prior_closure: ClosureDecision,
    pub(super) task: Result<Option<TaskRecord>>,
    pub(super) _task_budget: Option<Arc<input_evidence_budget::BudgetViewGuard>>,
    pub(super) continuation_trigger: Option<ContinuationTrigger>,
    pub(super) continuation_resolution: Option<ContinuationResolution>,
    pub(super) model_turn_allowed: bool,
    pub(super) execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
    pub(super) provider_admission: Option<Arc<super::provider_admission::ProviderTurnAdmission>>,
}

impl RuntimeHandle {
    #[cfg(test)]
    pub(super) fn build_message_dispatch_plan(
        &self,
        message: &MessageEnvelope,
        prior_closure: ClosureDecision,
        scheduler_state: &AgentState,
    ) -> Result<MessageDispatchPlan> {
        let waits = self
            .inner
            .storage
            .latest_wait_conditions_for_agent(&message.agent_id)?;
        let waits = waits.iter().collect::<Vec<_>>();
        let task = message
            .task_id
            .as_deref()
            .map(|id| self.inner.storage.latest_task_record(id))
            .transpose()?
            .flatten();
        self.build_message_dispatch_plan_with_waits(
            message,
            prior_closure,
            scheduler_state,
            &waits,
            task.as_ref(),
        )
    }

    /// The actual loaded dispatch supplies Input/Work/Task selections from its
    /// held owner. Existing low-level plan construction remains a pure helper.
    pub(super) fn build_message_dispatch_plan_from_captured(
        &self,
        message: &MessageEnvelope,
        prior_closure: ClosureDecision,
        scheduler_state: &AgentState,
        inputs: &super::resident_input_evidence::CapturedInputEvidence,
        work: &super::resident_work::CapturedWork,
        tasks: &super::resident_task::CapturedTasks,
    ) -> Result<MessageDispatchPlan> {
        let mut plan = self.build_message_dispatch_plan_from_work(
            message,
            prior_closure,
            scheduler_state,
            work,
            tasks,
        )?;
        if message.kind == MessageKind::TaskStatus {
            // The private live seal stays first. After restart only a qualified
            // canonical Task plus this exact accepted Message can supply birth.
            // Keep every proof error in the original deferred Task result.
            plan.task = plan.task.and_then(|mut parsed| {
                let exact_clock = self
                    .inner
                    .control_cancellations
                    .original_child_running_status_clock(message)?;
                if let Some(task) = parsed.as_mut() {
                    let seal = status_clock::OriginalRunningSeal::observe(
                        &self.inner.control_cancellations,
                        message,
                    )?;
                    if let Some(original) =
                        status_clock::qualify(inputs, tasks, message, task, seal.as_ref())?
                    {
                        original.preserve_rejoin_shape(task);
                        if exact_clock.is_none() && original.preserves_original_clock() {
                            task.created_at = original.created_at();
                        }
                    }
                }
                Ok(parsed)
            });
        }
        Ok(plan)
    }

    pub(super) fn build_message_dispatch_plan_from_work(
        &self,
        message: &MessageEnvelope,
        prior_closure: ClosureDecision,
        scheduler_state: &AgentState,
        work: &super::resident_work::CapturedWork,
        tasks: &super::resident_task::CapturedTasks,
    ) -> Result<MessageDispatchPlan> {
        anyhow::ensure!(
            work.scope.agent_id == message.agent_id,
            "dispatch Wait scope differs"
        );
        anyhow::ensure!(
            tasks.scope == work.scope,
            "dispatch Task/Work scopes differ"
        );
        let waits = work.waits_triggered_by(&message.id)?;
        let task = if message.kind == MessageKind::TaskResult
            && !super::task_reads::dispatch_result_is_invalid(tasks, message)?
        {
            message
                .task_id
                .as_deref()
                .map(|id| super::task_reads::dispatch_task_record(tasks, id))
                .transpose()?
                .flatten()
        } else {
            None
        };
        // The plan's Task copy outlives this selected capture in ScheduledMessage.
        // Reserve before parsing/copying either original metadata or fallback.
        let budget = if matches!(
            message.kind,
            MessageKind::TaskStatus | MessageKind::TaskResult
        ) {
            let limits = self.inner.input_evidence_budget.limits();
            let (bytes, records) = super::task_reads::dispatch_plan_material(message, task)?;
            anyhow::ensure!(
                records <= limits.record_limit
                    && bytes
                        <= limits
                            .history_per_agent_bytes
                            .checked_add(limits.pending_per_agent_bytes)
                            .context("dispatch Task plan limits overflow")?,
                super::resident_task::TaskReadError::ResourceExhausted(
                    "dispatch Task plan material"
                )
            );
            Some(self.inner.input_evidence_budget.register_owned_bytes(
                &tasks.scope,
                input_evidence_budget::InputMemoryCategory::Pins,
                input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: bytes,
                    metadata_estimate_bytes: 0,
                },
            )?)
        } else {
            None
        };
        let mut plan = self.build_message_dispatch_plan_with_waits(
            message,
            prior_closure,
            scheduler_state,
            &waits,
            task,
        )?;
        plan._task_budget = budget;
        Ok(plan)
    }

    fn build_message_dispatch_plan_with_waits(
        &self,
        message: &MessageEnvelope,
        prior_closure: ClosureDecision,
        scheduler_state: &AgentState,
        waits: &[&WaitConditionRecord],
        durable_task: Option<&TaskRecord>,
    ) -> Result<MessageDispatchPlan> {
        let task = match message.kind {
            MessageKind::TaskStatus => {
                tasks::task_from_message(message, &message.agent_id).and_then(|mut task| {
                    // Only this private original producer's exact accepted
                    // Message carries its clock; malformed status still stays
                    // an original deferred parse error in the dispatch plan.
                    if let Some(clock) = self
                        .inner
                        .control_cancellations
                        .original_child_running_status_clock(message)?
                    {
                        task.created_at = clock;
                    }
                    Ok(Some(task))
                })
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
                        .and_then(|task_id| durable_task.filter(|task| task.id == task_id))
                        .filter(|task| {
                            task.agent_id == message.agent_id
                                && task.work_item_id == message.work_item_id
                                && task.parent_message_id.as_deref() == Some(message.id.as_str())
                        });
                    let Some(task) = durable_task else {
                        return Err(error);
                    };
                    let wait_authority = if let Some(work_item_id) = task.work_item_id.as_deref() {
                        exact_dispatch_task_wait(
                            waits,
                            message,
                            &task.id,
                            Some(Some(work_item_id)),
                            false,
                        )
                    } else {
                        false
                    };
                    let terminal_with_explicit_work_item = matches!(
                        task.status,
                        TaskStatus::Completed
                            | TaskStatus::Failed
                            | TaskStatus::Cancelled
                            | TaskStatus::Interrupted
                    ) && task.effective_work_item_id()
                        == Some(message.work_item_id.as_deref().unwrap_or_default());
                    if terminal_with_explicit_work_item || task.terminal_reentry() || wait_authority
                    {
                        Ok(task.clone())
                    } else {
                        Err(error)
                    }
                })
                .map(Some),
            _ => Ok(None),
        };
        let mut continuation_trigger =
            ContinuationTrigger::from_message(message, task.as_ref().ok().and_then(Option::as_ref));
        if let Some(trigger) = continuation_trigger.as_mut() {
            if trigger.kind == crate::types::ContinuationTriggerKind::SystemTick {
                trigger.exact_wait_recheck = exact_dispatch_wait_recheck(waits, message);
            }
            // Only an exact durable task wait is correlated as the expected
            // wait. Captured ownership independently authorizes the terminal
            // result to request canonical admission.
            if trigger.kind == crate::types::ContinuationTriggerKind::TaskResult
                && trigger.task_result_outcome.is_some()
            {
                if let Some(task_id) = message.task_id.as_deref() {
                    let owner_scoped = exact_dispatch_task_wait(
                        waits,
                        message,
                        task_id,
                        Some(trigger.task_work_item_id.as_deref()),
                        false,
                    );
                    trigger.exact_task_wait = owner_scoped
                        || exact_dispatch_task_wait(waits, message, task_id, None, true);
                }
            }
        }
        let continuation_resolution = continuation_trigger
            .as_ref()
            .map(|trigger| resolve_continuation(&prior_closure, trigger));
        let model_turn_allowed = !matches!(scheduler_state.status, AgentStatus::Stopped);
        Ok(MessageDispatchPlan {
            prior_closure,
            task,
            _task_budget: None,
            continuation_trigger,
            continuation_resolution,
            model_turn_allowed,
            execution_admission_provenance: None,
            provider_admission: None,
        })
    }

    // Tests and direct runtime probes still exercise the per-message entrypoint.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn process_message(
        &self,
        message: MessageEnvelope,
        prior_closure: ClosureDecision,
    ) -> Result<()> {
        let mut mutation = self.begin_mutation().await?;
        let read = mutation
            .prepare_scheduler_read(
                Some(&message),
                Default::default(),
                Default::default(),
                self.now(),
            )
            .await?;
        let inputs = mutation.capture_input_evidence()?;
        let plan = self.build_message_dispatch_plan_from_captured(
            &message,
            prior_closure,
            &read.state,
            &inputs,
            &read.work,
            &read.tasks,
        )?;
        let scheduler_projection = &read.projection;
        drop(mutation);
        let scheduler_decision = scheduler::decide_next_action(
            scheduler_projection,
            scheduler::SchedulerBoundary::MessageProcessing,
            scheduler::SchedulerInput::Message {
                message: &message,
                model_turn_allowed: plan.model_turn_allowed,
                continuation_resolution: plan.continuation_resolution.as_ref(),
            },
        );
        self.append_scheduler_decision(&scheduler_decision).await?;
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

    pub(super) fn process_message_with_plan_deferred<'a>(
        &'a self,
        message: MessageEnvelope,
        plan: MessageDispatchPlan,
        scheduler_decision: &'a scheduler::SchedulerDecision,
    ) -> Pin<Box<impl Future<Output = Result<turn::TurnTerminalTransition>> + Send + 'a>> {
        // Box at this synchronous boundary so construction frames unwind before
        // polling and the runtime loop retains only a heap pointer.
        Box::pin(async move {
            let _provider_admission =
                self.activate_provider_admission(plan.provider_admission.clone())?;
            let trace_context = message.trace_context.clone();
            let attributes = crate::observability::TraceAttributes {
                agent_id: Some(message.agent_id.clone()),
                message_id: Some(message.id.clone()),
                turn_id: message.turn_id.clone(),
                work_item_id: message.work_item_id.clone(),
                task_id: message.task_id.clone(),
                ..Default::default()
            };
            if let Some(parent) = trace_context.as_ref() {
                let queue_context = parent.child();
                crate::observability::record_span(
                    &queue_context,
                    crate::observability::completed_span(
                        "holon.scheduler.queue_wait",
                        &queue_context,
                        Some(parent.span_id.clone()),
                        message.created_at,
                        crate::observability::TraceSpanStatus::Ok,
                        attributes.clone(),
                    ),
                );
            }
            let turn_context = trace_context
                .as_ref()
                .map(crate::observability::TraceContext::child);
            let turn_started_at = chrono::Utc::now();
            let result = self
                .process_message_with_plan_deferred_in_turn(
                    message,
                    plan,
                    scheduler_decision,
                    turn_context.clone(),
                )
                .await;
            if let Some(context) = turn_context.as_ref() {
                let status = if result.is_ok() {
                    crate::observability::TraceSpanStatus::Ok
                } else {
                    crate::observability::TraceSpanStatus::Error
                };
                crate::observability::record_span(
                    context,
                    crate::observability::completed_span(
                        "holon.turn",
                        context,
                        trace_context.as_ref().map(|parent| parent.span_id.clone()),
                        turn_started_at,
                        status,
                        attributes,
                    ),
                );
            }
            result
        })
    }

    #[inline(never)]
    fn process_message_with_plan_deferred_in_turn<'a>(
        &'a self,
        message: MessageEnvelope,
        plan: MessageDispatchPlan,
        scheduler_decision: &'a scheduler::SchedulerDecision,
        turn_trace_context: Option<crate::observability::TraceContext>,
    ) -> Pin<Box<impl Future<Output = Result<turn::TurnTerminalTransition>> + Send + 'a>> {
        // Construct before entering the nested provider/tool polling stack.
        Box::pin(self.process_message_with_plan_deferred_in_turn_inner(
            message,
            plan,
            scheduler_decision,
            turn_trace_context,
        ))
    }

    async fn process_message_with_plan_deferred_in_turn_inner(
        &self,
        mut message: MessageEnvelope,
        plan: MessageDispatchPlan,
        scheduler_decision: &scheduler::SchedulerDecision,
        turn_trace_context: Option<crate::observability::TraceContext>,
    ) -> Result<turn::TurnTerminalTransition> {
        message.normalize_admission_fields();
        self.append_original_audit_event(AuditEvent::typed(
            RuntimeEventKind::MessageProcessingStarted,
            &MessageLifecycleAuditEvent::from_message(&message),
        )?)
        .await?;
        let MessageDispatchPlan {
            prior_closure,
            task,
            continuation_trigger,
            continuation_resolution,
            execution_admission_provenance,
            _task_budget,
            ..
        } = plan;
        let model_reentry = scheduler_decision.model_reentry;
        let reducer_only_dispatch = match message.kind {
            MessageKind::OperatorPrompt
            | MessageKind::WebhookEvent
            | MessageKind::CallbackEvent
            | MessageKind::TimerTick
            | MessageKind::SystemTick
            | MessageKind::ChannelEvent
            | MessageKind::InternalFollowup => !model_reentry,
            MessageKind::TaskStatus
            | MessageKind::Control
            | MessageKind::BriefAck
            | MessageKind::BriefResult => true,
            MessageKind::TaskResult => false,
        };
        let reducer_dispatch = if reducer_only_dispatch {
            Some(
                self.begin_reducer_only_turn(&message, Some(&prior_closure))
                    .await?,
            )
        } else {
            None
        };
        let task = task?;
        let mut terminal_transition = None;
        let mut reducer_only_reason = None;
        if let Some(trigger) = continuation_trigger.as_ref() {
            self.record_continuation_trigger_received(&message, trigger, &prior_closure)
                .await?;
        }

        // Reconciliation is audit-only. The writer already triggered an exact
        // wait atomically with enqueue, and scheduler claim already resolved
        // it atomically with canonical admission.
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
                    let execution_admission_provenance =
                        execution_admission_provenance.clone().ok_or_else(|| {
                            anyhow!("model turn requires canonical execution admission provenance")
                        })?;
                    if let Some(work_item_id) = message.work_item_id.as_deref() {
                        let mut guard = self.begin_mutation().await?;
                        guard.state.current_turn_work_item_id = Some(work_item_id.to_string());
                        guard.persist_state(&self.inner.storage).await?;
                    }
                    terminal_transition = Some(
                        self.process_interactive_dispatch_with_cleanup(
                            &message,
                            continuation_resolution.as_ref(),
                            Some(execution_admission_provenance.clone()),
                            LoopControlOptions {
                                max_tool_rounds: None,
                            },
                            turn_trace_context,
                            Some(&prior_closure),
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
                    self.reduce_task_result_for_dispatch(
                        &message,
                        task,
                        model_reentry,
                        continuation_resolution.as_ref(),
                        execution_admission_provenance,
                        Some(&prior_closure),
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
                self.control_message(&message, action).await?;
                reducer_only_reason = Some("reducer_only/control");
            }
            MessageKind::BriefAck | MessageKind::BriefResult => {
                reducer_only_reason = Some("reducer_only/brief_notification");
            }
        }

        terminal_transition = match terminal_transition {
            Some(transition)
                if transition.prepared_work_item_completion.is_some()
                    || transition.prepared_wait_for.is_some() =>
            {
                // Prepared settlements own all terminal writes in the queue transaction.
                return Ok(transition);
            }
            transition => transition,
        };

        let actual_dispatch = terminal_transition
            .as_ref()
            .map(|transition| {
                (
                    transition.turn_record.turn_id.clone(),
                    transition.turn_record.turn_index,
                )
            })
            .or(reducer_dispatch)
            .ok_or_else(|| anyhow!("post-dispatch memory requires an actual Turn binding"))?;

        if let Some(resolution) = continuation_resolution.as_ref() {
            self.persist_last_continuation(resolution).await?;
            self.record_continuation_resolution_event(&message, resolution)
                .await?;
        }

        {
            let mut guard = self.begin_mutation().await?;
            let status_mutable = !matches!(
                guard.state.status,
                AgentStatus::Asleep | AgentStatus::Stopped
            );
            if status_mutable {
                // This producer still owns its original Started dispatch until
                // QueueTerminal writes Turn, memory Done and posture together.
                let dispatch_run =
                    guard.state.current_run_id.clone().filter(|run_id| {
                        guard
                            .current_run_abort
                            .as_ref()
                            .is_some_and(|handle| &handle.run_id == run_id)
                            && guard.state.current_turn_id.as_deref()
                                == Some(actual_dispatch.0.as_str())
                            && guard.state.turn_index == actual_dispatch.1
                            && guard.state.current_execution_binding.as_ref().is_some_and(
                                |binding| {
                                    binding.turn_id == actual_dispatch.0
                                        && binding.source_message_id == message.id
                                },
                            )
                    });
                scheduler::apply_idle_projection(&mut guard.state, &self.inner.storage)?;
                if dispatch_run.is_some() {
                    guard.state.current_run_id = dispatch_run;
                }
            }
            if status_mutable || matches!(message.kind, MessageKind::TaskResult) {
                guard.persist_state(&self.inner.storage).await?;
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
        // Reducer/Brief ACKs can invalidate Input. This original episode
        // planning phase prepares its own view; ordinary closure reads do not.
        let final_closure = {
            let mut owner = self.begin_mutation().await?;
            let read = owner
                .prepare_scheduler_read(None, Default::default(), Default::default(), self.now())
                .await?;
            Self::closure_decision_from_projection(
                &read.state,
                &read.projection,
                None,
                &read.acquisition,
            )?
        };
        {
            use crate::memory::episode_transition::{
                plan_episode_memory, EpisodeMemoryPlanInput, FrozenEpisodeMemoryValues,
            };
            use crate::runtime_db::episode_memory::{
                EpisodeMemoryInvocation, EpisodeMemoryKey, EpisodeMemoryTransitionCommand,
            };
            let key = EpisodeMemoryKey {
                agent_id: message.agent_id.clone(),
                identity_incarnation: self.inner.identity_incarnation,
                actual_dispatch_turn_id: actual_dispatch.0.clone(),
            };
            let invocation = EpisodeMemoryInvocation {
                message_id: message.id.clone(),
                turn_index: actual_dispatch.1,
            };
            let mut guard = self.begin_mutation().await?;
            let refs_commit = self
                .refresh_current_work_item_refs(&mut guard, &message)
                .await?;
            let legacy_phase = self.inner.runtime_db.transaction(|tx| {
                crate::runtime_db::dispatch_memory::is_legacy_dispatch_tx(tx, &key, &invocation)
            })?;
            // Only fixed pre-D4 dispatches retain the old standalone phase.
            // New dispatches finish memory inside their original terminal TX.
            // Original-outcome lookup comes before refresh and ID allocation.
            // Replaying a phase cannot recompute memory or replace current state.
            if legacy_phase
                && self
                    .inner
                    .runtime_db
                    .episode_memory_receipt(&key, None, &invocation)?
                    .is_none()
            {
                anyhow::ensure!(
                    guard.state.turn_index == actual_dispatch.1,
                    "post-dispatch memory cannot advance another Turn"
                );
                let memory_refresh =
                    refresh_working_memory(&self.inner.storage, &mut guard.state, &final_closure)?;
                let frozen = FrozenEpisodeMemoryValues {
                    timestamp: Utc::now(),
                    initial_builder_id: crate::ids::episode_id(),
                    fallback_builder_id: crate::ids::episode_id(),
                    next_builder_id: crate::ids::episode_id(),
                    audit_event_id: crate::ids::audit_event_id(),
                };
                let plan = plan_episode_memory(
                    EpisodeMemoryPlanInput {
                        agent: &guard.state,
                        actual_dispatch_turn_id: &actual_dispatch.0,
                        trigger: &message,
                        prior_closure: &prior_closure,
                        current_closure: &final_closure,
                        previous_snapshot: &memory_refresh.previous_snapshot,
                        current_snapshot: &memory_refresh.current_snapshot,
                    },
                    &frozen,
                );
                let command = EpisodeMemoryTransitionCommand::new(
                    key,
                    invocation,
                    guard.last_persisted_state.clone(),
                    plan,
                    frozen.timestamp,
                )?;
                guard.commit_episode_memory(&command).await?;
            }
            drop(guard);
            if let Some(commit) = refs_commit {
                self.apply_transition_commit(commit).await;
            }
        }
        self.append_original_audit_event(AuditEvent::legacy(
            "closure_decided",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "closure": final_closure,
            }),
        ))
        .await?;

        info!("processed message {}", message.id);
        match terminal_transition {
            Some(transition) => Ok(transition),
            None => {
                self.build_reducer_only_terminal_for_dispatch(
                    reducer_only_reason.unwrap_or("reducer_only/message_consumed"),
                    false,
                    Some(&actual_dispatch),
                )
                .await
            }
        }
    }

    async fn refresh_current_work_item_refs(
        &self,
        mutation: &mut super::state_mutation::RuntimeMutation,
        message: &MessageEnvelope,
    ) -> Result<Option<TransitionCommit>> {
        let agent = &mutation.state;
        let Some(work_item_id) = agent
            .current_turn_work_item_id
            .as_deref()
            .or(agent.current_work_item_id.as_deref())
        else {
            return Ok(None);
        };
        let mut request = super::resident_work::WorkCaptureRequest::current();
        request.include_live = false;
        request.nonopen_limit = 0;
        request
            .mandatory_roots
            .insert(super::resident_work::WorkKey::new(
                super::resident_work::WorkFamily::Work,
                work_item_id,
            ));
        let work = mutation.capture_work(&request)?;
        let Some(selected) = work.work_item(work_item_id)? else {
            return Ok(None);
        };
        let _owned = self.inner.input_evidence_budget.register_owned_bytes(
            &work.scope,
            super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: super::input_evidence_budget::serialized_bytes(selected)?
                    .saturating_mul(3),
                metadata_estimate_bytes: 0,
            },
        )?;
        let mut record = selected.clone();
        if record.state != WorkItemState::Open {
            return Ok(None);
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
            return Ok(None);
        }

        let merged = crate::work_item_refs::merge_work_refs(&record.work_refs, additions);
        if merged == record.work_refs {
            return Ok(None);
        }
        let previous_count = record.work_refs.len();
        record.work_refs = merged;
        record.revision = record.revision.saturating_add(1);
        record.updated_at = Utc::now();
        let command = crate::runtime_db::transitions::WorkItemTransitionCommand {
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
        };
        let operation_id = command.audit_events[0].id.clone();
        let commit = mutation
            .commit_work_transition(
                operation_id,
                command,
                crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
                true,
            )
            .await?;
        Ok(Some(commit))
    }

    pub(super) async fn record_incoming_transcript_entry(
        &self,
        message: &MessageEnvelope,
    ) -> Result<()> {
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
        .await
    }
}

pub(super) fn message_text(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => value.to_string(),
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

fn exact_dispatch_task_wait(
    waits: &[&WaitConditionRecord],
    message: &MessageEnvelope,
    task_id: &str,
    owner: Option<Option<&str>>,
    any_owner: bool,
) -> bool {
    let count = waits
        .iter()
        .filter(|wait| {
            owner.is_none_or(|owner| wait.work_item_id.as_deref() == owner)
                && matches!(
                    wait.status,
                    WaitConditionStatus::Triggered | WaitConditionStatus::Resolved
                )
                && wait.kind == crate::types::WaitConditionKind::Task
                && wait.trigger_message_id() == Some(message.id.as_str())
                && wait.wake_sources.iter().any(|source| {
                    matches!(source,
                crate::types::WakeSource::TaskResult { task_id: expected } if expected == task_id)
                })
        })
        .count();
    if any_owner {
        count > 0
    } else {
        count == 1
    }
}

fn exact_dispatch_wait_recheck(waits: &[&WaitConditionRecord], message: &MessageEnvelope) -> bool {
    if !matches!(&message.origin, MessageOrigin::System { subsystem } if subsystem == "wait_condition_recheck")
        || message.kind != MessageKind::SystemTick
        || message.authority_class != AuthorityClass::RuntimeInstruction
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
        || message.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
    {
        return false;
    }
    let Some(id) = message.source_refs.get("wait_id") else {
        return false;
    };
    waits
        .iter()
        .filter(|wait| {
            wait.id == *id
                && wait.work_item_id.is_none()
                && matches!(
                    wait.status,
                    WaitConditionStatus::Triggered | WaitConditionStatus::Resolved
                )
                && wait.trigger_message_id() == Some(message.id.as_str())
        })
        .count()
        == 1
}
