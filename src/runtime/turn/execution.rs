//! Turn execution: the agent loop that drives provider rounds, tool calls,
//! checkpointing, context projection, and completion.

use std::{collections::HashSet, time::Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::config::ModelRef;
use crate::prompt::EffectivePrompt;
use crate::provider::{
    provider_attempt_timeline, provider_error_is_context_length_exceeded, AgentProvider,
    ModelBlock, ProviderAttemptTimeline, ProviderTurnRequest, ProviderTurnResponse,
    ToolResultBlock,
};
use crate::runtime::provider_turn::{
    build_continuation_request, build_provider_prompt_frame, build_provider_turn_request,
};
use crate::storage::to_json_value;
use crate::tool::{ToolCall, ToolError};
use crate::types::{
    AdmissionContext, AuditEvent, AuthorityClass, MessageBody, MessageDeliverySurface,
    MessageEnvelope, MessageKind, MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus,
    TokenUsage, ToolExecutionAuditEvent, TranscriptEntry, TranscriptEntryKind, TurnTerminalKind,
    TurnTerminalRecord,
};

use super::checkpoint::{
    build_checkpoint_resume_round, checkpoint_state_from_last_terminal,
    terminal_checkpoint_from_state, PendingCheckpointRequest, TurnLocalCheckpointRecord,
    TurnLocalCheckpointState,
};
use super::completion::{
    command_batch_preview_field, command_cost_field, command_display_field, command_preview_field,
    envelope_completes_work_item, exec_command_disposition_field, exec_command_exit_status_field,
    exec_command_task_handle_field, rejects_truncated_mutation_tool_call, result_work_item_id,
    round_has_post_completion_non_completion_tool_call, truncated_mutation_recovery_hint,
};
use super::context_management::context_management_diagnostic;
use super::projection::{
    build_round_estimated_tokens, build_turn_local_projection_with_runtime_reminder,
    normalize_provider_attempt_timing, provider_attempt_model_state, TurnLocalProjectionOutcome,
};
use super::reminders::{
    build_work_item_stale_reminder, maybe_reset_work_item_stale_reminder_cooldown,
    round_invalidates_checkpoint_anchor, round_updated_work_item, runtime_reminder_fits_baseline,
    work_item_plan_status_label, work_item_stale_reminder_cooldown_rounds,
    work_item_stale_reminder_rounds,
};
use super::{
    append_follow_up_user_texts, render_operator_interjection_text, AgentLoopOutcome,
    LoopControlOptions, TurnPromotedCompletionReport, TurnRoundRecord, TurnTerminalDelivery,
    MAX_OUTPUT_RECOVERY_ATTEMPTS, ROUND_TEXT_PREVIEW_LIMIT,
    WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS,
};
use super::{truncate_preview, CHECKPOINT_RESUME_PROMPT};
use crate::runtime::{
    combine_text_history, is_max_output_stop_reason, message_dispatch::message_text, scheduler,
    CurrentRunAborted, RuntimeHandle,
};

impl RuntimeHandle {
    pub(super) async fn maybe_handle_context_length_exceeded(
        &self,
        agent_id: &str,
        round: usize,
        error: &anyhow::Error,
        duration_ms: u64,
    ) -> Result<Option<AgentLoopOutcome>> {
        if !provider_error_is_context_length_exceeded(error) {
            return Ok(None);
        }

        self.inner.storage.append_event(&AuditEvent::new(
            "turn_context_length_exceeded",
            serde_json::json!({
                "agent_id": agent_id,
                "round": round,
                "error": error.to_string(),
                "token_usage": provider_attempt_timeline(error)
                    .and_then(|timeline| timeline.aggregated_token_usage.clone()),
                "provider_attempt_timeline": provider_attempt_timeline(error),
            }),
        ))?;
        let final_text = "Turn stopped because the provider rejected the request with context_length_exceeded. This usually means the configured model context window or prompt budget is too large for the current provider path.".to_string();
        let terminal = self
            .persist_turn_terminal_record(
                TurnTerminalKind::Aborted,
                Some(final_text.clone()),
                duration_ms,
                None,
            )
            .await?;
        Ok(Some(AgentLoopOutcome {
            final_text,
            final_text_source_assistant_round_id: None,
            turn_index: terminal.turn_index,
            terminal,
            should_sleep: false,
            sleep_duration_ms: None,
            allow_sleep_runnable_work_override: false,
            terminal_kind: TurnTerminalKind::Aborted,
            terminal_delivery: TurnTerminalDelivery::normal(),
        }))
    }

    pub(super) async fn persist_turn_terminal_record(
        &self,
        kind: TurnTerminalKind,
        last_assistant_message: Option<String>,
        duration_ms: u64,
        checkpoint_state: Option<&TurnLocalCheckpointState>,
    ) -> Result<TurnTerminalRecord> {
        let record = {
            let mut guard = self.inner.agent.lock().await;
            let checkpoint = if kind == TurnTerminalKind::Completed {
                checkpoint_state
                    .and_then(|state| terminal_checkpoint_from_state(state, guard.state.turn_index))
            } else {
                None
            };
            let turn_id = guard
                .state
                .current_turn_id
                .clone()
                .filter(|turn_id| !turn_id.trim().is_empty())
                .unwrap_or_else(crate::ids::turn_id);
            guard.state.current_turn_id = Some(turn_id.clone());
            let record = TurnTerminalRecord {
                turn_id,
                turn_index: guard.state.turn_index,
                kind,
                reason: None,
                last_assistant_message,
                checkpoint,
                completed_at: chrono::Utc::now(),
                duration_ms,
            };
            guard.state.last_turn_terminal = Some(record.clone());
            guard.persist_state(&self.inner.storage)?;
            record
        };
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal",
            serde_json::to_value(&record)?,
        ))?;
        Ok(record)
    }
    pub(super) async fn maybe_defer_provider_lineage_failure(
        &self,
        agent_id: &str,
        round: usize,
        error: &anyhow::Error,
        last_assistant_message: Option<String>,
        duration_ms: u64,
        side_effect_boundary_crossed: bool,
    ) -> Result<Option<AgentLoopOutcome>> {
        let Some(timeline) = provider_attempt_timeline(error).cloned() else {
            return Ok(None);
        };
        let Some(fallback_ref) = timeline.pending_fallback_model_ref.as_deref() else {
            return Ok(None);
        };
        let Ok(fallback_model) = ModelRef::parse(fallback_ref) else {
            return Ok(None);
        };
        let terminal_kind = if side_effect_boundary_crossed {
            TurnTerminalKind::ProviderFailedNeedsRecovery
        } else {
            TurnTerminalKind::DeferredToFallback
        };
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.pending_fallback_model = Some(fallback_model.clone());
            guard.persist_state(&self.inner.storage)?;
        }
        let error_text = error.to_string();
        let provider_failure_text = provider_lineage_failure_text(&error_text);
        let operator_message = provider_lineage_operator_message(
            fallback_ref,
            side_effect_boundary_crossed,
            &provider_failure_text,
        );
        self.inner.storage.append_event(&AuditEvent::new(
            "lineage_retry_exhausted",
            serde_json::json!({
                "agent_id": agent_id,
                "round": round,
                "error": error_text.clone(),
                "operator_message": operator_message.clone(),
                "requested_model_ref": timeline.requested_model_ref,
                "active_model_ref": timeline.active_model_ref,
                "pending_fallback_model_ref": fallback_ref,
                "side_effect_boundary_crossed": side_effect_boundary_crossed,
                "provider_attempt_timeline": timeline,
            }),
        ))?;
        let final_text = operator_message.clone();
        let terminal = self
            .persist_turn_terminal_record(
                terminal_kind,
                last_assistant_message
                    .clone()
                    .or_else(|| Some(final_text.clone())),
                duration_ms,
                None,
            )
            .await?;
        let event_kind = if side_effect_boundary_crossed {
            "provider_failed_needs_recovery"
        } else {
            "deferred_to_fallback"
        };
        self.inner.storage.append_event(&AuditEvent::new(
            event_kind,
            serde_json::json!({
                "agent_id": agent_id,
                "round": round,
                "error": error_text,
                "operator_message": operator_message,
                "fallback_model_ref": fallback_ref,
                "side_effect_boundary_crossed": side_effect_boundary_crossed,
                "last_assistant_preview": last_assistant_message
                    .as_deref()
                    .map(|text| truncate_preview(text, ROUND_TEXT_PREVIEW_LIMIT)),
            }),
        ))?;

        let mut message = MessageEnvelope::new(
            agent_id.to_string(),
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "model_lineage_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "Runtime recovery: the previous turn stopped after the active provider failed. Continue from the persisted transcript, current work item, and workspace state. Do not assume hidden provider continuation state is still available. Do not repeat completed tool work unless current evidence shows it is necessary.".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(serde_json::json!({
            "fallback_model_ref": fallback_ref,
            "source_terminal_kind": terminal_kind,
            "source_round": round,
            "side_effect_boundary_crossed": side_effect_boundary_crossed,
        }));
        let queued = self.enqueue(message).await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "recovery_turn_started",
            serde_json::json!({
                "agent_id": agent_id,
                "message_id": queued.id,
                "fallback_model_ref": fallback_ref,
                "source_terminal_kind": terminal_kind,
            }),
        ))?;
        Ok(Some(AgentLoopOutcome {
            final_text,
            final_text_source_assistant_round_id: None,
            turn_index: terminal.turn_index,
            terminal,
            should_sleep: false,
            sleep_duration_ms: None,
            allow_sleep_runnable_work_override: false,
            terminal_kind,
            terminal_delivery: TurnTerminalDelivery::normal(),
        }))
    }
    pub(super) async fn complete_turn_with_abort(
        &self,
        provider: std::sync::Arc<dyn AgentProvider>,
        request: ProviderTurnRequest,
    ) -> Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)> {
        if let Some(snapshot) = self.current_run_abort_token().await {
            tokio::select! {
                result = provider.complete_turn_with_diagnostics(request) => result,
                _ = snapshot.token.cancelled() => Err(CurrentRunAborted {
                    run_id: snapshot.run_id.clone(),
                    reason: snapshot.reason(),
                }.into()),
            }
        } else {
            provider.complete_turn_with_diagnostics(request).await
        }
    }

    pub(super) async fn complete_turn_with_timing(
        &self,
        provider: std::sync::Arc<dyn AgentProvider>,
        request: ProviderTurnRequest,
    ) -> (
        Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)>,
        DateTime<Utc>,
        DateTime<Utc>,
        u64,
    ) {
        let started_at = Utc::now();
        let started = Instant::now();
        let result = self.complete_turn_with_abort(provider, request).await;
        let completed_at = Utc::now();
        let duration_ms = started.elapsed().as_millis() as u64;
        (
            result.map(|(response, timeline)| {
                (
                    response,
                    normalize_provider_attempt_timing(
                        timeline,
                        started_at,
                        completed_at,
                        duration_ms,
                    ),
                )
            }),
            started_at,
            completed_at,
            duration_ms,
        )
    }

    pub(super) async fn ensure_not_aborted(&self) -> Result<()> {
        if let Some(snapshot) = self.current_run_abort_token().await {
            if snapshot.token.is_cancelled() {
                return Err(CurrentRunAborted {
                    run_id: snapshot.run_id.clone(),
                    reason: snapshot.reason(),
                }
                .into());
            }
        }
        Ok(())
    }

    pub(super) async fn drain_operator_interjections(
        &self,
        agent_id: &str,
        round: usize,
        boundary: &str,
    ) -> Result<Vec<String>> {
        let mut messages = Vec::new();
        {
            let mut guard = self.inner.agent.lock().await;
            while let Some(message) = guard
                .queue
                .pop_next_matching(scheduler::is_operator_interjection_message)
            {
                guard.state.pending = guard.queue.len();
                messages.push(message);
            }
            if !messages.is_empty() {
                if let Err(err) = guard.persist_state(&self.inner.storage) {
                    for message in messages.iter().rev().cloned() {
                        guard.queue.push_front(message);
                    }
                    guard.state.pending = guard.queue.len();
                    return Err(err);
                }
            }
        }

        let mut follow_up_texts = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let persist_result = (|| -> Result<String> {
                self.inner.storage.append_queue_entry(&QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: message.agent_id.clone(),
                    priority: message.priority.clone(),
                    status: QueueEntryStatus::Interjected,
                    created_at: message.created_at,
                    updated_at: chrono::Utc::now(),
                })?;
                self.record_incoming_transcript_entry(message)?;
                let text = render_operator_interjection_text(message);
                self.inner.storage.append_event(&AuditEvent::new(
                    "operator_interjection_admitted",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "round": round,
                        "boundary": boundary,
                        "message_id": message.id,
                        "origin": message.origin,
                        "authority_class": message.authority_class,
                        "priority": message.priority,
                        "delivery_surface": message.delivery_surface,
                        "admission_context": message.admission_context,
                        "text_preview": truncate_preview(&message_text(&message.body), ROUND_TEXT_PREVIEW_LIMIT),
                    }),
                ))?;
                Ok(text)
            })();

            match persist_result {
                Ok(text) => follow_up_texts.push(text),
                Err(err) => {
                    let mut guard = self.inner.agent.lock().await;
                    for message in messages[index..].iter().rev().cloned() {
                        guard.queue.push_front(message);
                    }
                    guard.state.pending = guard.queue.len();
                    let _ = guard.persist_state(&self.inner.storage);
                    return Err(err);
                }
            }
        }
        Ok(follow_up_texts)
    }

    pub(super) async fn append_operator_interjections_to_last_round(
        &self,
        agent_id: &str,
        round: usize,
        boundary: &str,
        completed_rounds: &mut [TurnRoundRecord],
    ) -> Result<bool> {
        let interjections = self
            .drain_operator_interjections(agent_id, round, boundary)
            .await?;
        let admitted = !interjections.is_empty();
        if let Some(last_round) = completed_rounds.last_mut() {
            append_follow_up_user_texts(last_round, interjections);
        }
        Ok(admitted)
    }

    pub(crate) async fn run_agent_loop(
        &self,
        agent_id: &str,
        authority_class: AuthorityClass,
        effective_prompt: EffectivePrompt,
        loop_control: LoopControlOptions,
    ) -> Result<AgentLoopOutcome> {
        TurnExecution {
            runtime: self,
            agent_id,
            authority_class,
            effective_prompt,
            loop_control,
        }
        .run()
        .await
    }
}

pub(super) fn provider_lineage_failure_text(text: &str) -> String {
    if text.trim().is_empty() {
        "provider failed".into()
    } else {
        truncate_preview(text, ROUND_TEXT_PREVIEW_LIMIT)
    }
}

pub(super) fn provider_lineage_operator_message(
    fallback_ref: &str,
    side_effect_boundary_crossed: bool,
    failure: &str,
) -> String {
    let prefix = if side_effect_boundary_crossed {
        "Turn stopped after the active provider lineage failed"
    } else {
        "Turn stopped before provider output was accepted"
    };
    let queued = if side_effect_boundary_crossed {
        "Queued recovery turn"
    } else {
        "Queued fallback turn"
    };
    format!("{prefix}: {failure} {queued} on {fallback_ref}.")
}

pub(super) struct TurnExecution<'a> {
    pub(super) runtime: &'a RuntimeHandle,
    pub(super) agent_id: &'a str,
    pub(super) authority_class: AuthorityClass,
    pub(super) effective_prompt: EffectivePrompt,
    pub(super) loop_control: LoopControlOptions,
}

impl TurnExecution<'_> {
    pub(super) async fn run(self) -> Result<AgentLoopOutcome> {
        let TurnExecution {
            runtime,
            agent_id,
            authority_class,
            effective_prompt,
            loop_control,
        } = self;
        let mut completed_rounds = Vec::<TurnRoundRecord>::new();
        let turn_started_at = Instant::now();
        let mut sleep_duration_ms = None;
        let mut promoted_completion_reports = Vec::<TurnPromotedCompletionReport>::new();
        let mut post_completion_continuation_action = false;
        let mut completed_work_item_this_turn = false;
        let mut round = 0usize;
        let mut truncated_text_history = Vec::new();
        let mut last_assistant_message: Option<String> = None;
        let mut last_assistant_round_id: Option<String>;
        let mut max_output_recovery_count = 0usize;
        let mut rounds_since_work_item_update = 0usize;
        let mut rounds_since_work_item_reminder = work_item_stale_reminder_cooldown_rounds();
        let mut checkpoint_state = {
            let guard = runtime.inner.agent.lock().await;
            checkpoint_state_from_last_terminal(guard.state.last_turn_terminal.as_ref())
        };
        let (turn_model_override, turn_pending_fallback_model, turn_model_state) = {
            let guard = runtime.inner.agent.lock().await;
            (
                guard.state.model_override.clone(),
                guard.state.pending_fallback_model.clone(),
                runtime.model_state_for(&guard.state),
            )
        };
        runtime.reconfigure_provider_for_current_state().await?;
        let identity = runtime.agent_identity_view().await?;
        let (
            provider,
            available_tools,
            _apply_patch_surface,
            native_web_search,
            builtin_web_search_selection,
        ) = runtime.provider_tool_selection(&identity).await?;
        let allowed_tool_names = available_tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<HashSet<_>>();
        runtime.inner.storage.append_event(&AuditEvent::new(
            "lineage_selected",
            serde_json::json!({
                "agent_id": agent_id,
                "model_override": turn_model_override,
                "pending_fallback_model": turn_pending_fallback_model,
                "model": turn_model_state,
                "builtin_web_search_selection": builtin_web_search_selection,
            }),
        ))?;
        if let Some(pending) = turn_pending_fallback_model.as_ref() {
            runtime.inner.storage.append_event(&AuditEvent::new(
                "pending_model_promoted",
                serde_json::json!({
                    "agent_id": agent_id,
                    "fallback_model": pending,
                }),
            ))?;
        }

        loop {
            if let Err(err) = runtime.ensure_not_aborted().await {
                if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                    runtime
                        .persist_turn_aborted_record(
                            &aborted.run_id,
                            &aborted.reason,
                            last_assistant_message.clone(),
                            turn_started_at.elapsed().as_millis() as u64,
                        )
                        .await?;
                }
                return Err(err);
            }
            round += 1;
            if let Some(max_tool_rounds) = loop_control.max_tool_rounds {
                if round > max_tool_rounds {
                    let final_text = format!(
                        "Stopped after reaching the maximum tool loop depth ({max_tool_rounds})."
                    );
                    let terminal = runtime
                        .persist_turn_terminal_record(
                            TurnTerminalKind::Aborted,
                            Some(final_text.clone()),
                            turn_started_at.elapsed().as_millis() as u64,
                            None,
                        )
                        .await?;
                    return Ok(AgentLoopOutcome {
                        final_text,
                        final_text_source_assistant_round_id: None,
                        turn_index: terminal.turn_index,
                        terminal,
                        should_sleep: false,
                        sleep_duration_ms: None,
                        allow_sleep_runnable_work_override: false,
                        terminal_kind: TurnTerminalKind::Aborted,
                        terminal_delivery: TurnTerminalDelivery::normal(),
                    });
                }
            }
            if round > 1 {
                runtime
                    .append_operator_interjections_to_last_round(
                        agent_id,
                        round,
                        "before_provider_continuation",
                        &mut completed_rounds,
                    )
                    .await?;
            }

            let context_build_started = Instant::now();

            let provider_round_started = std::time::Instant::now();
            let (
                response,
                attempt_timeline,
                context_management,
                context_build_ms,
                provider_started_at,
                provider_completed_at,
                provider_round_ms,
            ) = if round == 1 {
                let request_build_started = std::time::Instant::now();
                let request = build_provider_turn_request(
                    &effective_prompt,
                    available_tools.clone(),
                    native_web_search.clone(),
                );
                crate::diagnostics::record_provider_request_build(request_build_started.elapsed());
                let context_management = context_management_diagnostic(provider.as_ref(), &request);
                let context_build_ms = context_build_started.elapsed().as_millis() as u64;
                let (result, provider_started_at, provider_completed_at, provider_round_ms) =
                    runtime
                        .complete_turn_with_timing(provider.clone(), request)
                        .await;
                match result {
                    Ok((response, attempt_timeline)) => (
                        response,
                        attempt_timeline,
                        context_management,
                        context_build_ms,
                        provider_started_at,
                        provider_completed_at,
                        provider_round_ms,
                    ),
                    Err(err) => {
                        if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                            runtime
                                .persist_turn_aborted_record(
                                    &aborted.run_id,
                                    &aborted.reason,
                                    last_assistant_message.clone(),
                                    turn_started_at.elapsed().as_millis() as u64,
                                )
                                .await?;
                            return Err(err);
                        }
                        if let Some(outcome) = runtime
                            .maybe_handle_context_length_exceeded(
                                agent_id,
                                round,
                                &err,
                                turn_started_at.elapsed().as_millis() as u64,
                            )
                            .await?
                        {
                            return Ok(outcome);
                        }
                        if let Some(outcome) = runtime
                            .maybe_defer_provider_lineage_failure(
                                agent_id,
                                round,
                                &err,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                                !completed_rounds.is_empty() || last_assistant_message.is_some(),
                            )
                            .await?
                        {
                            return Ok(outcome);
                        }
                        {
                            let mut guard = runtime.inner.agent.lock().await;
                            if guard.state.pending_fallback_model.is_some() {
                                guard.state.pending_fallback_model = None;
                                guard.persist_state(&runtime.inner.storage)?;
                            }
                        }
                        runtime
                            .persist_turn_terminal_record(
                                TurnTerminalKind::Aborted,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                                None,
                            )
                            .await?;
                        return Err(err);
                    }
                }
            } else {
                let context_config = runtime.current_context_config().await;
                let turn_index = {
                    let guard = runtime.inner.agent.lock().await;
                    guard.state.turn_index
                };
                let checkpoint_request_id =
                    Some(format!("turn-{turn_index}-round-{round}-checkpoint"));
                let prompt_frame = build_provider_prompt_frame(&effective_prompt);
                let reminder_rounds = work_item_stale_reminder_rounds();
                let reminder_cooldown_rounds = work_item_stale_reminder_cooldown_rounds();
                let stale_work_item_reminder = if rounds_since_work_item_update >= reminder_rounds
                    && rounds_since_work_item_reminder >= reminder_cooldown_rounds
                {
                    let current_work_item_id = {
                        let guard = runtime.inner.agent.lock().await;
                        guard.state.current_work_item_id.clone()
                    };
                    current_work_item_id
                        .as_deref()
                        .and_then(|id| runtime.inner.storage.latest_work_item(id).ok().flatten())
                        .map(|work_item| {
                            let reminder = build_work_item_stale_reminder(
                                &work_item,
                                rounds_since_work_item_update,
                            );
                            (work_item, reminder)
                        })
                } else {
                    None
                };
                let stale_work_item_reminder = if let Some((work_item, reminder)) =
                    stale_work_item_reminder
                {
                    let turn_projection_budget = context_config.turn_projection_budget();
                    if runtime_reminder_fits_baseline(
                        &prompt_frame,
                        &available_tools,
                        turn_projection_budget,
                        &reminder,
                    ) {
                        Some((work_item, reminder))
                    } else {
                        let event = AuditEvent::new(
                            "work_item_stale_reminder_skipped",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "round": round,
                                "work_item_id": work_item.id,
                                "plan_status": work_item_plan_status_label(work_item.plan_status),
                                "rounds_since_work_item_update": rounds_since_work_item_update,
                                "cooldown_rounds": reminder_cooldown_rounds,
                                "reason": "baseline_budget",
                            }),
                        );
                        runtime.inner.storage.append_event(&event)?;
                        None
                    }
                } else {
                    None
                };
                if let Some((work_item, reminder)) = stale_work_item_reminder.as_ref() {
                    runtime.inner.storage.append_event(&AuditEvent::new(
                        "work_item_stale_reminder_injected",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "round": round,
                            "work_item_id": work_item.id,
                            "plan_status": work_item_plan_status_label(work_item.plan_status),
                            "rounds_since_work_item_update": rounds_since_work_item_update,
                            "cooldown_rounds": reminder_cooldown_rounds,
                            "text_preview": truncate_preview(reminder, ROUND_TEXT_PREVIEW_LIMIT),
                        }),
                    ))?;
                }
                maybe_reset_work_item_stale_reminder_cooldown(
                    &mut rounds_since_work_item_reminder,
                    stale_work_item_reminder.is_some(),
                );
                let projection = match build_turn_local_projection_with_runtime_reminder(
                    &prompt_frame,
                    &completed_rounds,
                    &available_tools,
                    &checkpoint_state,
                    checkpoint_request_id,
                    context_config.turn_projection_budget(),
                    context_config.compaction_keep_recent_estimated_tokens,
                    stale_work_item_reminder
                        .as_ref()
                        .map(|(_, reminder)| reminder.as_str()),
                ) {
                    TurnLocalProjectionOutcome::Projection(projection) => projection,
                    TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                        runtime.inner.storage.append_event(&AuditEvent::new(
                            "turn_local_baseline_over_budget",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "round": round,
                                "reason": &diagnostics.reason,
                                "estimated_baseline_tokens": diagnostics.estimated_baseline_tokens,
                                "minimum_exact_round_estimated_tokens": diagnostics.minimum_exact_round_estimated_tokens,
                                "minimum_projection_estimated_tokens": diagnostics.minimum_projection_estimated_tokens,
                                "effective_budget_estimated_tokens": diagnostics.effective_budget_estimated_tokens,
                                "tool_overhead_estimated_tokens": diagnostics.tool_overhead_estimated_tokens,
                                "system_prompt_estimated_tokens": diagnostics.system_prompt_estimated_tokens,
                                "context_attachment_estimated_tokens": diagnostics.context_attachment_estimated_tokens,
                            }),
                        ))?;
                        let final_text = format!(
                            "Turn stopped because the continuation baseline exceeded the prompt budget (reason={}, estimated_baseline_tokens={}, minimum_projection_estimated_tokens={}, effective_budget_estimated_tokens={}, tool_overhead_estimated_tokens={}).",
                            diagnostics.reason,
                            diagnostics.estimated_baseline_tokens,
                            diagnostics.minimum_projection_estimated_tokens,
                            diagnostics.effective_budget_estimated_tokens,
                            diagnostics.tool_overhead_estimated_tokens,
                        );
                        let terminal = runtime
                            .persist_turn_terminal_record(
                                TurnTerminalKind::BaselineOverBudget,
                                Some(final_text.clone()),
                                turn_started_at.elapsed().as_millis() as u64,
                                None,
                            )
                            .await?;
                        return Ok(AgentLoopOutcome {
                            final_text,
                            final_text_source_assistant_round_id: None,
                            turn_index: terminal.turn_index,
                            terminal,
                            should_sleep: false,
                            sleep_duration_ms: None,
                            allow_sleep_runnable_work_override: false,
                            terminal_kind: TurnTerminalKind::BaselineOverBudget,
                            terminal_delivery: TurnTerminalDelivery::normal(),
                        });
                    }
                };
                if let Some(compaction) = projection.compaction.as_ref() {
                    runtime.inner.storage.append_event(&AuditEvent::new(
                        "turn_local_compaction_applied",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "round": round,
                            "compacted_rounds": compaction.compacted_rounds,
                            "exact_tail_rounds": compaction.exact_tail_rounds,
                            "projected_estimated_tokens": compaction.projected_estimated_tokens,
                            "effective_budget_estimated_tokens": compaction.effective_budget_estimated_tokens,
                            "tool_overhead_estimated_tokens": compaction.tool_overhead_estimated_tokens,
                            "strict_fallback_applied": compaction.strict_fallback_applied,
                            "checkpoint_request_id": compaction.checkpoint_request_id,
                            "checkpoint_mode": compaction.checkpoint_mode.map(|mode| mode.as_str()),
                            "checkpoint_anchor_generation": compaction.checkpoint_anchor_generation,
                            "checkpoint_base_round": compaction.checkpoint_base_round,
                            "previous_checkpoint_round": compaction.previous_checkpoint_round,
                            "anchor_changed_since_checkpoint": compaction.anchor_changed_since_checkpoint,
                            "last_round_degraded": compaction.last_round_degraded,
                        }),
                    ))?;
                    if let (Some(request_id), Some(mode), Some(anchor_generation)) = (
                        compaction.checkpoint_request_id.clone(),
                        compaction.checkpoint_mode,
                        compaction.checkpoint_anchor_generation,
                    ) {
                        checkpoint_state.pending = Some(PendingCheckpointRequest {
                            request_id: request_id.clone(),
                            mode,
                            requested_at_round: round,
                            anchor_generation,
                            base_round: compaction.checkpoint_base_round,
                            text_fragments: Vec::new(),
                        });
                        runtime.inner.storage.append_event(&AuditEvent::new(
                            "turn_local_checkpoint_requested",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "round": round,
                                "checkpoint_request_id": request_id,
                                "checkpoint_mode": mode.as_str(),
                                "checkpoint_anchor_generation": anchor_generation,
                                "checkpoint_base_round": compaction.checkpoint_base_round,
                            }),
                        ))?;
                    }
                }
                let request = build_continuation_request(
                    prompt_frame,
                    projection.conversation,
                    available_tools.clone(),
                    native_web_search.clone(),
                );
                let context_management = context_management_diagnostic(provider.as_ref(), &request);
                let context_build_ms = context_build_started.elapsed().as_millis() as u64;
                let (result, provider_started_at, provider_completed_at, provider_round_ms) =
                    runtime
                        .complete_turn_with_timing(provider.clone(), request)
                        .await;
                match result {
                    Ok((response, attempt_timeline)) => (
                        response,
                        attempt_timeline,
                        context_management,
                        context_build_ms,
                        provider_started_at,
                        provider_completed_at,
                        provider_round_ms,
                    ),
                    Err(err) => {
                        if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                            runtime
                                .persist_turn_aborted_record(
                                    &aborted.run_id,
                                    &aborted.reason,
                                    last_assistant_message.clone(),
                                    turn_started_at.elapsed().as_millis() as u64,
                                )
                                .await?;
                            return Err(err);
                        }
                        if let Some(outcome) = runtime
                            .maybe_handle_context_length_exceeded(
                                agent_id,
                                round,
                                &err,
                                turn_started_at.elapsed().as_millis() as u64,
                            )
                            .await?
                        {
                            return Ok(outcome);
                        }
                        if let Some(outcome) = runtime
                            .maybe_defer_provider_lineage_failure(
                                agent_id,
                                round,
                                &err,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                                !completed_rounds.is_empty() || last_assistant_message.is_some(),
                            )
                            .await?
                        {
                            return Ok(outcome);
                        }
                        {
                            let mut guard = runtime.inner.agent.lock().await;
                            if guard.state.pending_fallback_model.is_some() {
                                guard.state.pending_fallback_model = None;
                                guard.persist_state(&runtime.inner.storage)?;
                            }
                        }
                        runtime
                            .persist_turn_terminal_record(
                                TurnTerminalKind::Aborted,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                                None,
                            )
                            .await?;
                        return Err(err);
                    }
                }
            };
            let stop_reason = response.stop_reason.clone();
            let cache_usage = response.cache_usage.clone();
            let request_diagnostics = response.request_diagnostics.clone();
            let model_attempt_state = provider_attempt_model_state(attempt_timeline.as_ref());

            let (turn_index, run_id, round_work_item_id) = {
                let mut guard = runtime.inner.agent.lock().await;
                guard.state.total_input_tokens += response.input_tokens;
                guard.state.total_output_tokens += response.output_tokens;
                guard.state.total_model_rounds += 1;
                guard.state.last_turn_token_usage = Some(TokenUsage::new(
                    response.input_tokens,
                    response.output_tokens,
                ));
                guard.state.last_requested_model = model_attempt_state.requested_model.clone();
                guard.state.last_active_model = model_attempt_state.active_model.clone();
                guard.state.pending_fallback_model = None;
                guard.persist_state(&runtime.inner.storage)?;
                (
                    guard.state.turn_index,
                    guard.state.current_run_id.clone(),
                    guard
                        .state
                        .current_turn_work_item_id
                        .clone()
                        .or_else(|| guard.state.current_work_item_id.clone()),
                )
            };

            let assistant_blocks = response.blocks.clone();
            let mut tool_calls = Vec::new();
            let mut text_blocks = Vec::new();

            for block in &assistant_blocks {
                match block {
                    ModelBlock::Text { text } => {
                        if !text.trim().is_empty() {
                            text_blocks.push(text.clone());
                        }
                    }
                    ModelBlock::ToolUse { id, name, input } => {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => {}
                }
            }

            crate::diagnostics::record_turn_provider_round(provider_round_started.elapsed());
            crate::diagnostics::record_provider_round_total(provider_round_started.elapsed());
            let completed_round_assistant_blocks = assistant_blocks.clone();
            let only_sleep_tools =
                !tool_calls.is_empty() && tool_calls.iter().all(|call| call.name == "Sleep");
            let legacy_sleep_duration_ms = if only_sleep_tools {
                tool_calls
                    .iter()
                    .filter_map(|call| call.input.get("duration_ms").and_then(Value::as_u64))
                    .filter(|duration| *duration > 0)
                    .last()
            } else {
                None
            };
            let combined_text = text_blocks
                .iter()
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            let aggregated_text = combine_text_history(&truncated_text_history, &text_blocks)
                .into_iter()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !aggregated_text.is_empty() {
                last_assistant_message = Some(aggregated_text.clone());
            } else if !truncated_text_history.is_empty() {
                // If current round has no text, preserve text history from previous rounds
                // This ensures that summaries before tool calls are not lost
                let history_text = truncated_text_history
                    .iter()
                    .map(|text| text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !history_text.is_empty() {
                    last_assistant_message = Some(history_text);
                }
            }
            let token_usage = TokenUsage::new(response.input_tokens, response.output_tokens);

            runtime.inner.storage.append_event(&AuditEvent::new(
                "provider_round_completed",
                serde_json::json!({
                    "agent_id": agent_id,
                    "turn_index": turn_index,
                    "run_id": run_id,
                    "round": round,
                    "work_item_id": round_work_item_id.clone(),
                    "stop_reason": stop_reason,
                    "context_build_ms": context_build_ms,
                    "provider_round_ms": provider_round_ms,
                    "provider_started_at": provider_started_at,
                    "provider_completed_at": provider_completed_at,
                    "input_tokens": response.input_tokens,
                    "output_tokens": response.output_tokens,
                    "token_usage": token_usage,
                    "tool_call_count": tool_calls.len(),
                    "tool_names": tool_calls.iter().map(|call| call.name.clone()).collect::<Vec<_>>(),
                    "text_block_count": text_blocks.len(),
                    "text_char_count": combined_text.chars().count(),
                    "only_sleep_tools": only_sleep_tools,
                    "provider_cache_usage": cache_usage,
                    "prompt_cache_key": effective_prompt.cache_identity.prompt_cache_key.clone(),
                    "context_fingerprint": effective_prompt.cache_identity.context_fingerprint.clone(),
                    "compression_epoch": effective_prompt.cache_identity.compression_epoch,
                    "requested_model": model_attempt_state.requested_model.clone(),
                    "active_model": model_attempt_state.active_model.clone(),
                    "fallback_active": model_attempt_state.fallback_active,
                    "context_management": context_management,
                    "provider_request_diagnostics": request_diagnostics.clone(),
                    "provider_attempt_timeline": attempt_timeline,
                }),
            ))?;
            let max_output_recovery_pending = tool_calls.is_empty()
                && is_max_output_stop_reason(stop_reason.as_deref())
                && max_output_recovery_count < MAX_OUTPUT_RECOVERY_ATTEMPTS;
            let should_record_pending_checkpoint =
                checkpoint_state
                    .pending
                    .as_mut()
                    .is_some_and(|pending_checkpoint| {
                        if !combined_text.trim().is_empty() {
                            pending_checkpoint
                                .text_fragments
                                .push(combined_text.clone());
                        }
                        !max_output_recovery_pending
                            && (!combined_text.trim().is_empty() || tool_calls.is_empty())
                    });
            let mut checkpoint_recorded_this_round = false;
            if should_record_pending_checkpoint {
                let pending_checkpoint = checkpoint_state
                    .pending
                    .take()
                    .expect("pending checkpoint should exist when record flag is set");
                let checkpoint_text = pending_checkpoint
                    .text_fragments
                    .iter()
                    .map(|text| text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let checkpoint_recorded = !checkpoint_text.is_empty();
                checkpoint_recorded_this_round = checkpoint_recorded;
                if checkpoint_recorded {
                    checkpoint_state.latest = Some(TurnLocalCheckpointRecord {
                        request_id: pending_checkpoint.request_id.clone(),
                        requested_at_round: pending_checkpoint.requested_at_round,
                        response_round: Some(round),
                        source_turn_index: None,
                        mode: pending_checkpoint.mode,
                        text: checkpoint_text.clone(),
                        anchor_generation: pending_checkpoint.anchor_generation,
                    });
                }
                runtime.inner.storage.append_event(&AuditEvent::new(
                    "turn_local_checkpoint_recorded",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "round": round,
                        "checkpoint_request_id": pending_checkpoint.request_id,
                        "requested_at_round": pending_checkpoint.requested_at_round,
                        "checkpoint_mode": pending_checkpoint.mode.as_str(),
                        "checkpoint_anchor_generation": pending_checkpoint.anchor_generation,
                        "checkpoint_response_round": round,
                        "checkpoint_recorded": checkpoint_recorded,
                        "text_char_count": checkpoint_text.chars().count(),
                        "text_preview": if checkpoint_text.is_empty() {
                            None::<String>
                        } else {
                            Some(truncate_preview(&checkpoint_text, ROUND_TEXT_PREVIEW_LIMIT))
                        },
                        "checkpoint_base_round": pending_checkpoint.base_round,
                    }),
                ))?;
            }
            let assistant_round_transcript_entry = TranscriptEntry {
                stop_reason: stop_reason.clone(),
                input_tokens: Some(response.input_tokens),
                output_tokens: Some(response.output_tokens),
                ..TranscriptEntry::new(
                    agent_id.to_string(),
                    TranscriptEntryKind::AssistantRound,
                    Some(round),
                    None,
                    serde_json::json!({
                        "blocks": &completed_round_assistant_blocks,
                        "work_item_id": round_work_item_id.clone(),
                        "token_usage": token_usage,
                        "provider_cache_usage": cache_usage,
                        "prompt_cache_key": effective_prompt.cache_identity.prompt_cache_key.clone(),
                        "context_fingerprint": effective_prompt.cache_identity.context_fingerprint.clone(),
                        "compression_epoch": effective_prompt.cache_identity.compression_epoch,
                        "requested_model": model_attempt_state.requested_model,
                        "active_model": model_attempt_state.active_model,
                        "fallback_active": model_attempt_state.fallback_active,
                        "context_management": context_management,
                        "provider_request_diagnostics": request_diagnostics,
                        "provider_attempt_timeline": attempt_timeline,
                    }),
                )
            };
            let assistant_round_id = assistant_round_transcript_entry.id.clone();
            runtime.persist_transcript_evidence(&assistant_round_transcript_entry)?;
            last_assistant_round_id = Some(assistant_round_id.clone());
            runtime.inner.storage.append_event(&AuditEvent::new(
                "assistant_round_recorded",
                serde_json::json!({
                    "assistant_round_id": assistant_round_id,
                    "agent_id": agent_id,
                    "turn_index": turn_index,
                    "run_id": run_id,
                    "round": round,
                    "work_item_id": round_work_item_id.clone(),
                    "stop_reason": stop_reason,
                    "text_block_count": text_blocks.len(),
                    "text_char_count": combined_text.chars().count(),
                    "tool_call_count": tool_calls.len(),
                    "tool_names": tool_calls.iter().map(|call| call.name.clone()).collect::<Vec<_>>(),
                    "has_text": !combined_text.is_empty(),
                    "has_tool_calls": !tool_calls.is_empty(),
                }),
            ))?;

            if tool_calls.is_empty() {
                runtime.inner.storage.append_event(&AuditEvent::new(
                    "text_only_round_observed",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "turn_index": turn_index,
                        "run_id": run_id,
                        "round": round,
                        "stop_reason": stop_reason,
                        "has_text": !combined_text.is_empty(),
                        "text_preview": if combined_text.is_empty() {
                            None::<String>
                        } else {
                            Some(truncate_preview(&combined_text, ROUND_TEXT_PREVIEW_LIMIT))
                        },
                        "triggered_recovery": is_max_output_stop_reason(stop_reason.as_deref()),
                        "recovery_attempt": max_output_recovery_count,
                    }),
                ))?;
            }

            if tool_calls.is_empty() {
                let interjections = runtime
                    .drain_operator_interjections(agent_id, round, "after_provider_round")
                    .await?;
                if !interjections.is_empty() {
                    let mut round_record = TurnRoundRecord {
                        round,
                        estimated_tokens: build_round_estimated_tokens(
                            &completed_round_assistant_blocks,
                            &[],
                            &[],
                        ),
                        assistant_blocks: completed_round_assistant_blocks,
                        text_blocks,
                        tool_calls: Vec::new(),
                        tool_results: Vec::new(),
                        tool_result_envelopes: Vec::new(),
                        follow_up_user_texts: Vec::new(),
                    };
                    append_follow_up_user_texts(&mut round_record, interjections);
                    if round_invalidates_checkpoint_anchor(&round_record) {
                        checkpoint_state.anchor_generation =
                            checkpoint_state.anchor_generation.saturating_add(1);
                    }
                    completed_rounds.push(round_record);
                    continue;
                }
            }

            let mut before_tool_execution_interjections = Vec::new();
            if !tool_calls.is_empty() {
                before_tool_execution_interjections = runtime
                    .drain_operator_interjections(agent_id, round, "before_tool_execution")
                    .await?;
            }

            if tool_calls.is_empty() && is_max_output_stop_reason(stop_reason.as_deref()) {
                if max_output_recovery_count < MAX_OUTPUT_RECOVERY_ATTEMPTS {
                    if !combined_text.is_empty() {
                        truncated_text_history.push(combined_text.clone());
                    }
                    max_output_recovery_count += 1;
                    let continuation_text =
                        "Output token limit hit. Continue exactly where you left off. Do not restart from the top, repeat analysis, or re-read context already provided. Finish the remaining report directly.".to_string();
                    completed_rounds.push(TurnRoundRecord {
                        round,
                        estimated_tokens: build_round_estimated_tokens(
                            &completed_round_assistant_blocks,
                            &[],
                            std::slice::from_ref(&continuation_text),
                        ),
                        assistant_blocks: completed_round_assistant_blocks,
                        text_blocks,
                        tool_calls: Vec::new(),
                        tool_results: Vec::new(),
                        tool_result_envelopes: Vec::new(),
                        follow_up_user_texts: vec![continuation_text.clone()],
                    });
                    runtime.persist_transcript_evidence(&TranscriptEntry::new(
                        agent_id.to_string(),
                        TranscriptEntryKind::ContinuationPrompt,
                        Some(round),
                        None,
                        serde_json::json!({
                            "text": continuation_text,
                            "reason": "max_output_tokens",
                        }),
                    ))?;
                    runtime.inner.storage.append_event(&AuditEvent::new(
                        "max_output_tokens_recovery",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "attempt": max_output_recovery_count,
                        }),
                    ))?;
                    continue;
                }
            }

            if tool_calls.is_empty() && checkpoint_recorded_this_round {
                completed_rounds.push(build_checkpoint_resume_round(
                    round,
                    completed_round_assistant_blocks,
                    text_blocks,
                ));
                runtime.persist_transcript_evidence(&TranscriptEntry::new(
                    agent_id.to_string(),
                    TranscriptEntryKind::ContinuationPrompt,
                    Some(round),
                    None,
                    serde_json::json!({
                        "text": CHECKPOINT_RESUME_PROMPT,
                        "reason": "turn_local_checkpoint",
                    }),
                ))?;
                runtime.inner.storage.append_event(&AuditEvent::new(
                    "turn_local_checkpoint_resume_requested",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "round": round,
                    }),
                ))?;
                continue;
            }

            if tool_calls.is_empty() {
                let final_text = last_assistant_message.clone().unwrap_or_default();
                let terminal = runtime
                    .persist_turn_terminal_record(
                        TurnTerminalKind::Completed,
                        last_assistant_message.clone(),
                        turn_started_at.elapsed().as_millis() as u64,
                        Some(&checkpoint_state),
                    )
                    .await?;
                return Ok(AgentLoopOutcome {
                    final_text,
                    final_text_source_assistant_round_id: last_assistant_round_id.clone(),
                    turn_index: terminal.turn_index,
                    terminal,
                    should_sleep: true,
                    sleep_duration_ms,
                    allow_sleep_runnable_work_override: completed_work_item_this_turn,
                    terminal_kind: TurnTerminalKind::Completed,
                    terminal_delivery: if !promoted_completion_reports.is_empty() {
                        TurnTerminalDelivery::completion_reports(
                            promoted_completion_reports,
                            !post_completion_continuation_action,
                        )
                    } else {
                        TurnTerminalDelivery::normal()
                    },
                });
            }

            if !promoted_completion_reports.is_empty() && !tool_calls.is_empty() {
                post_completion_continuation_action = true;
            }
            let round_tool_calls = tool_calls.clone();
            let mut tool_results = Vec::new();
            let mut tool_result_envelopes = Vec::new();
            let mut tool_execution_refs: Vec<(String, String)> = Vec::new();
            for call in tool_calls {
                if let Err(err) = runtime.ensure_not_aborted().await {
                    if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                        runtime
                            .persist_turn_aborted_record(
                                &aborted.run_id,
                                &aborted.reason,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                            )
                            .await?;
                    }
                    return Err(err);
                }
                let tool_call_id = call.id.clone();
                let tool_name = call.name.clone();
                if !allowed_tool_names.contains(&call.name) && call.name != "Sleep" {
                    let error = ToolError::new(
                        "tool_not_exposed_for_round",
                        format!("tool {tool_name} was not exposed in this round"),
                    )
                    .with_details(serde_json::json!({
                        "tool_name": tool_name,
                    }))
                    .with_recovery_hint(
                        "request the current tool list again and call only tools exposed in this round",
                    )
                    .with_retryable(false);
                    let audit_error = error.render();
                    let result = crate::tool::ToolResult::error(&tool_name, error.clone());
                    let result_content = crate::tool::tools::render_tool_result_for_model(&result)?;
                    let (turn_index, run_id) = {
                        let guard = runtime.inner.agent.lock().await;
                        (guard.state.turn_index, guard.state.current_run_id.clone())
                    };
                    runtime.inner.storage.append_event(&AuditEvent::new(
                        "tool_execution_failed",
                        serde_json::json!({
                            "tool_call_id": tool_call_id,
                            "tool_name": tool_name,
                            "turn_index": turn_index,
                            "run_id": run_id,
                            "exec_command_cmd": command_preview_field(&call),
                            "exec_command_display": command_display_field(&call),
                            "exec_command_batch_items": command_batch_preview_field(&call),
                            "exec_command_cost": command_cost_field(
                                &call,
                                {
                                    let snap = runtime.inner.config_snapshot.load();
                                    snap.default_tool_output_tokens
                                },
                                {
                                    let snap = runtime.inner.config_snapshot.load();
                                    snap.max_tool_output_tokens
                                },
                            ),
                            "error": audit_error,
                            "error_kind": error.kind.clone(),
                            "tool_error": error.clone(),
                            "reason": "tool_not_exposed_for_round",
                        }),
                    ))?;
                    tool_results.push(ToolResultBlock {
                        tool_use_id: tool_call_id.clone(),
                        content: result_content,
                        is_error: true,
                        error: Some(error.clone()),
                    });
                    tool_result_envelopes.push(result.envelope);
                    continue;
                }
                if is_max_output_stop_reason(stop_reason.as_deref())
                    && rejects_truncated_mutation_tool_call(&call.name)
                {
                    let stop_reason_label = stop_reason.as_deref().unwrap_or("an output limit");
                    let error = ToolError::new(
                        "truncated_mutation_tool_call",
                        format!(
                            "{tool_name} was not executed because the provider stopped with {stop_reason_label}; mutation tool arguments may be incomplete"
                        ),
                    )
                    .with_details(serde_json::json!({
                        "tool_name": tool_name.clone(),
                        "stop_reason": stop_reason.clone(),
                        "round": round,
                    }))
                    .with_recovery_hint(truncated_mutation_recovery_hint(&tool_name))
                    .with_retryable(true);
                    let result = crate::tool::ToolResult::error(&tool_name, error.clone());
                    let result_content = crate::tool::tools::render_tool_result_for_model(&result)?;
                    runtime.inner.storage.append_event(&AuditEvent::new(
                        "truncated_mutation_tool_call_rejected",
                        serde_json::json!({
                            "tool_call_id": tool_call_id.clone(),
                            "tool_name": tool_name.clone(),
                            "stop_reason": stop_reason.clone(),
                            "round": round,
                            "error_kind": error.kind.clone(),
                            "tool_error": error.clone(),
                        }),
                    ))?;
                    tool_results.push(ToolResultBlock {
                        tool_use_id: tool_call_id.clone(),
                        content: result_content,
                        is_error: true,
                        error: Some(error.clone()),
                    });
                    tool_result_envelopes.push(result.envelope);
                    continue;
                }
                let pre_tool_work_item_id = {
                    let guard = runtime.inner.agent.lock().await;
                    guard
                        .state
                        .current_turn_work_item_id
                        .clone()
                        .or_else(|| guard.state.current_work_item_id.clone())
                };
                let tool_exec_started = std::time::Instant::now();
                let tool_execution = if let Some(snapshot) = runtime.current_run_abort_token().await
                {
                    tokio::select! {
                        result = runtime.inner.tools.execute(runtime, agent_id, &authority_class, &call) => result,
                        _ = snapshot.token.cancelled() => Err(CurrentRunAborted {
                            run_id: snapshot.run_id.clone(),
                            reason: snapshot.reason(),
                        }.into()),
                    }
                } else {
                    runtime
                        .inner
                        .tools
                        .execute(runtime, agent_id, &authority_class, &call)
                        .await
                };
                crate::diagnostics::record_turn_tool_execution(tool_exec_started.elapsed());
                match tool_execution {
                    Ok((result, mut record)) => {
                        let result_content =
                            crate::tool::tools::render_tool_result_for_model(&result)?;
                        let duration_ms = record.duration_ms;
                        let (turn_index, turn_id, run_id, current_work_item_id) = {
                            let guard = runtime.inner.agent.lock().await;
                            (
                                guard.state.turn_index,
                                guard.state.current_turn_id.clone(),
                                guard.state.current_run_id.clone(),
                                guard
                                    .state
                                    .current_turn_work_item_id
                                    .clone()
                                    .or_else(|| guard.state.current_work_item_id.clone()),
                            )
                        };
                        record.turn_index = turn_index;
                        record.turn_id = turn_id;
                        if record.work_item_id.is_none() {
                            record.work_item_id = pre_tool_work_item_id
                                .clone()
                                .or(current_work_item_id)
                                .or_else(|| result_work_item_id(&result.envelope));
                        }

                        if result.should_sleep {
                            sleep_duration_ms = result.sleep_duration_ms;
                        }
                        tool_execution_refs.push((tool_call_id.clone(), record.id.clone()));
                        runtime.persist_tool_execution_evidence(&record)?;
                        if matches!(record.status, crate::types::ToolExecutionStatus::Success) {
                            runtime
                                .record_skill_tool_activation(
                                    &record.tool_name,
                                    &record.input,
                                    &result,
                                )
                                .await?;
                        }

                        runtime.inner.storage.append_event(&AuditEvent::new(
                            "tool_executed",
                            to_json_value(&ToolExecutionAuditEvent {
                                tool_call_id: tool_call_id.clone(),
                                tool_execution_id: record.id.clone(),
                                agent_id: record.agent_id.clone(),
                                tool_name: tool_name.clone(),
                                turn_index,
                                turn_id: record.turn_id.clone(),
                                run_id,
                                work_item_id: record.work_item_id.clone(),
                                status: record.status.clone(),
                                duration_ms,
                                summary: record.summary.clone(),
                                exec_command_cmd: command_preview_field(&call),
                                exec_command_display: command_display_field(&call),
                                exec_command_batch_items: command_batch_preview_field(&call),
                                exec_command_cost: command_cost_field(
                                    &call,
                                    runtime
                                        .inner
                                        .config_snapshot
                                        .load()
                                        .default_tool_output_tokens,
                                    runtime.inner.config_snapshot.load().max_tool_output_tokens,
                                ),
                                exec_command_disposition: exec_command_disposition_field(
                                    &call,
                                    &result.envelope,
                                ),
                                exit_status: exec_command_exit_status_field(
                                    &call,
                                    &result.envelope,
                                ),
                                task_handle: exec_command_task_handle_field(
                                    &call,
                                    &result.envelope,
                                ),
                                error: result.tool_error().map(|error| error.render()),
                                error_kind: result.tool_error().map(|error| error.kind.clone()),
                            }),
                        ))?;
                        tool_result_envelopes.push(result.envelope.clone());
                        tool_results.push(ToolResultBlock {
                            tool_use_id: tool_call_id,
                            content: result_content.clone(),
                            is_error: result.is_error(),
                            error: result.tool_error().cloned(),
                        });
                    }
                    Err(err) => {
                        if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                            runtime
                                .persist_turn_aborted_record(
                                    &aborted.run_id,
                                    &aborted.reason,
                                    last_assistant_message.clone(),
                                    turn_started_at.elapsed().as_millis() as u64,
                                )
                                .await?;
                            return Err(err);
                        }
                        let error = ToolError::from_anyhow(&err);
                        let audit_error = error.render();
                        let result = crate::tool::ToolResult::error(&tool_name, error.clone());
                        let result_content =
                            crate::tool::tools::render_tool_result_for_model(&result)?;
                        let (turn_index, run_id) = {
                            let guard = runtime.inner.agent.lock().await;
                            (guard.state.turn_index, guard.state.current_run_id.clone())
                        };
                        runtime.inner.storage.append_event(&AuditEvent::new(
                            "tool_execution_failed",
                            serde_json::json!({
                                "tool_call_id": tool_call_id,
                                "tool_name": tool_name,
                                "turn_index": turn_index,
                                "run_id": run_id,
                                "exec_command_cmd": command_preview_field(&call),
                                "exec_command_display": command_display_field(&call),
                                "exec_command_batch_items": command_batch_preview_field(&call),
                                "exec_command_cost": command_cost_field(
                                    &call,
                                    runtime.inner.config_snapshot.load().default_tool_output_tokens,
                                    runtime.inner.config_snapshot.load().max_tool_output_tokens
                                ),
                                "error": audit_error,
                                "error_kind": error.kind.clone(),
                                "tool_error": error.clone(),
                            }),
                        ))?;
                        tool_results.push(ToolResultBlock {
                            tool_use_id: tool_call_id,
                            content: result_content,
                            is_error: true,
                            error: Some(error.clone()),
                        });
                        tool_result_envelopes.push(result.envelope);
                    }
                }
            }
            let completion_promotions = runtime
                .promote_round_completion_report_if_present(
                    agent_id,
                    round,
                    turn_index,
                    &completed_round_assistant_blocks,
                    &mut tool_results,
                    &mut tool_result_envelopes,
                )
                .await?;
            if tool_result_envelopes
                .iter()
                .any(envelope_completes_work_item)
            {
                completed_work_item_this_turn = true;
            }
            if !completion_promotions.is_empty()
                && round_has_post_completion_non_completion_tool_call(
                    &completed_round_assistant_blocks,
                )
            {
                post_completion_continuation_action = true;
            }
            // Build ref-backed tool result metadata for transcript
            use crate::types::{ToolResultData, ToolResultRef};
            let refs: Vec<ToolResultRef> = tool_results
                .iter()
                .map(|result| {
                    let tool_call_id = &result.tool_use_id;
                    let tool_execution_id = tool_execution_refs
                        .iter()
                        .find(|(id, _)| id == tool_call_id)
                        .map(|(_, exec_id)| exec_id);
                    // Store full content for now - truncation breaks structured JSON receipts
                    let (provider_visible_text, content_truncated) =
                        (Some(result.content.clone()), false);
                    ToolResultRef {
                        tool_call_id: tool_call_id.clone(),
                        tool_execution_id: tool_execution_id.map(|id| id.clone()),
                        provider_visible_text,
                        content_truncated,
                        is_error: result.is_error,
                    }
                })
                .collect();
            runtime.persist_transcript_evidence(&TranscriptEntry::new(
                agent_id.to_string(),
                TranscriptEntryKind::ToolResults,
                Some(round),
                None,
                to_json_value(&ToolResultData::RefsWithWrapper { refs }),
            ))?;
            let after_tool_results_interjections = runtime
                .drain_operator_interjections(agent_id, round, "after_tool_results")
                .await?;
            let mut interjections = before_tool_execution_interjections;
            interjections.extend(after_tool_results_interjections);
            let has_operator_interjections = !interjections.is_empty();
            if (!promoted_completion_reports.is_empty() || !completion_promotions.is_empty())
                && has_operator_interjections
            {
                post_completion_continuation_action = true;
            }
            let round_record = TurnRoundRecord {
                round,
                estimated_tokens: build_round_estimated_tokens(
                    &completed_round_assistant_blocks,
                    &tool_results,
                    &interjections,
                ),
                assistant_blocks: completed_round_assistant_blocks,
                text_blocks,
                tool_calls: round_tool_calls,
                tool_results,
                tool_result_envelopes,
                follow_up_user_texts: interjections,
            };
            if round_invalidates_checkpoint_anchor(&round_record) {
                checkpoint_state.anchor_generation =
                    checkpoint_state.anchor_generation.saturating_add(1);
            }
            if round_updated_work_item(&round_record) {
                rounds_since_work_item_update = 0;
                rounds_since_work_item_reminder = WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS;
            } else {
                rounds_since_work_item_update = rounds_since_work_item_update.saturating_add(1);
                rounds_since_work_item_reminder = rounds_since_work_item_reminder.saturating_add(1);
            }
            completed_rounds.push(round_record);

            promoted_completion_reports.extend(completion_promotions.into_iter().map(
                |promotion| TurnPromotedCompletionReport {
                    work_item_id: promotion.record.id,
                    brief_id: promotion.brief_id,
                },
            ));

            if only_sleep_tools && !has_operator_interjections {
                let final_text = last_assistant_message.clone().unwrap_or_default();
                let terminal = runtime
                    .persist_turn_terminal_record(
                        TurnTerminalKind::Completed,
                        last_assistant_message.clone(),
                        turn_started_at.elapsed().as_millis() as u64,
                        Some(&checkpoint_state),
                    )
                    .await?;
                return Ok(AgentLoopOutcome {
                    final_text,
                    final_text_source_assistant_round_id: last_assistant_round_id.clone(),
                    turn_index: terminal.turn_index,
                    terminal,
                    should_sleep: true,
                    sleep_duration_ms: sleep_duration_ms.or(legacy_sleep_duration_ms),
                    allow_sleep_runnable_work_override: completed_work_item_this_turn,
                    terminal_kind: TurnTerminalKind::Completed,
                    terminal_delivery: if !promoted_completion_reports.is_empty() {
                        TurnTerminalDelivery::completion_reports(
                            promoted_completion_reports,
                            !post_completion_continuation_action,
                        )
                    } else {
                        TurnTerminalDelivery::normal()
                    },
                });
            }
        }
    }
}
