//! Turn execution: the agent loop that drives provider rounds, tool calls,
//! checkpointing, context projection, and completion.

use std::{collections::HashSet, time::Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::config::ModelRouteRef;
use crate::prompt::EffectivePrompt;
use crate::provider::{
    provider_attempt_timeline, provider_error_is_context_length_exceeded, AgentProvider,
    ModelBlock, ProviderAttemptTimeline, ProviderTurnRequest, ProviderTurnResponse,
    ToolResultBlock,
};
use crate::runtime::provider_turn::{
    build_continuation_request, build_initial_provider_turn_request, build_provider_prompt_frame,
};
use crate::storage::to_json_value;
use crate::tool::{ToolCall, ToolError, ToolSpec};
use crate::types::{
    AdmissionContext, AssistantRoundPurpose, AuditEvent, AuthorityClass, Citation, MessageBody,
    MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    QueueEntryRecord, QueueEntryStatus, TokenUsage, ToolExecutionAuditEvent, ToolExecutionRecord,
    ToolExecutionStatus, TranscriptEntry, TranscriptEntryKind, TurnTerminalKind,
    TurnTerminalRecord, WorkItemExecutionBinding,
};

use super::checkpoint::{
    build_checkpoint_resume_round, checkpoint_state_from_last_terminal,
    terminal_checkpoint_from_state, PendingCheckpointRequest, TurnLocalCheckpointRecord,
    TurnLocalCheckpointState,
};
use super::completion::{
    command_batch_preview_field, command_cost_field, command_display_field, command_preview_field,
    completion_report_texts_by_tool_id, envelope_completes_work_item,
    exec_command_disposition_field, exec_command_exit_status_field, exec_command_task_handle_field,
    rejects_truncated_mutation_tool_call, result_work_item_id, truncated_mutation_recovery_hint,
};
use super::context_management::context_management_diagnostic;
use super::projection::{
    build_round_estimated_tokens, build_turn_local_projection_with_runtime_reminder,
    normalize_provider_attempt_timing, provider_attempt_model_state, TurnLocalProjectionOutcome,
};
use super::reminders::{
    build_turn_budget_warning, build_work_item_stale_reminder,
    maybe_reset_work_item_stale_reminder_cooldown, round_invalidates_checkpoint_anchor,
    round_updated_work_item, runtime_reminder_fits_baseline, work_item_plan_status_label,
    work_item_stale_reminder_cooldown_rounds, work_item_stale_reminder_rounds,
};
use super::{
    append_follow_up_user_texts, render_operator_interjection_text, AgentLoopOutcome,
    LoopControlOptions, ProviderRecoveryDirective, TurnModelSelection, TurnRoundRecord,
    MAX_OUTPUT_RECOVERY_ATTEMPTS, ROUND_TEXT_PREVIEW_LIMIT,
    WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS,
};
use super::{truncate_preview, CHECKPOINT_RESUME_PROMPT};
use crate::runtime::{
    combine_text_history, is_max_output_stop_reason, message_dispatch::message_text, scheduler,
    CurrentRunAborted, RuntimeHandle,
};

enum OperatorInterjectionPlan {
    Admit,
    LegacyTurnDeferred {
        scenario_class: Option<crate::domain::scheduler::SchedulerScenarioClass>,
        effective_mode: crate::domain::scheduler::ScenarioMode,
    },
}

struct PendingCompletionReport {
    request_id: String,
    work_item_id: String,
    expected_work_revision: u64,
    execution_binding: WorkItemExecutionBinding,
    request_turn_index: u64,
    request_round: usize,
    request_assistant_round_id: String,
    request_tool_call_id: String,
    tool_execution: ToolExecutionRecord,
    warnings: Vec<Value>,
    corrective_retry_attempted: bool,
}

fn tool_capability_projection_fingerprint(tools: &[ToolSpec]) -> String {
    let encoded = serde_json::to_vec(tools).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(encoded))
}

impl TurnModelSelection {
    pub(crate) fn message_has_provider_recovery_provenance(message: &MessageEnvelope) -> bool {
        message.kind == MessageKind::InternalFollowup
            && message.authority_class == AuthorityClass::RuntimeInstruction
            && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
            && message.admission_context == Some(AdmissionContext::RuntimeOwned)
            && matches!(
                &message.origin,
                MessageOrigin::System { subsystem } if subsystem == "model_lineage_recovery"
            )
    }

    pub(crate) fn message_has_valid_provider_recovery(message: &MessageEnvelope) -> bool {
        Self::from_message(message).is_ok_and(|selection| selection.recovery.is_some())
    }

    pub(crate) fn from_message(message: &MessageEnvelope) -> Result<Self> {
        let trusted_recovery = Self::message_has_provider_recovery_provenance(message);
        if !trusted_recovery {
            return Ok(Self::default());
        }
        let directive = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("provider_recovery"))
            .ok_or_else(|| {
                anyhow::anyhow!("model lineage recovery message is missing its directive")
            })
            .and_then(|value| {
                serde_json::from_value::<ProviderRecoveryDirective>(value.clone())
                    .map_err(anyhow::Error::from)
            })?;
        Ok(Self {
            recovery: Some(directive),
        })
    }
}

impl RuntimeHandle {
    pub(super) async fn maybe_handle_context_length_exceeded(
        &self,
        agent_id: &str,
        round: usize,
        error: &anyhow::Error,
        duration_ms: u64,
        persist_terminal: bool,
    ) -> Result<Option<AgentLoopOutcome>> {
        if !provider_error_is_context_length_exceeded(error) {
            return Ok(None);
        }

        self.inner.storage.append_event(&AuditEvent::legacy(
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
                persist_terminal,
            )
            .await?;
        Ok(Some(AgentLoopOutcome {
            final_text,
            final_citations: Vec::new(),
            final_text_source_assistant_round_id: None,
            turn_index: terminal.turn_index,
            terminal,
            should_sleep: false,
            sleep_duration_ms: None,
            allow_sleep_runnable_work_override: false,
            terminal_kind: TurnTerminalKind::Aborted,
            prepared_work_item_completion: None,
            terminal_tool_executions: Vec::new(),
        }))
    }

    pub(super) async fn persist_turn_terminal_record(
        &self,
        kind: TurnTerminalKind,
        last_assistant_message: Option<String>,
        duration_ms: u64,
        checkpoint_state: Option<&TurnLocalCheckpointState>,
        persist: bool,
    ) -> Result<TurnTerminalRecord> {
        self.persist_turn_terminal_record_with_tool_executions(
            kind,
            last_assistant_message,
            duration_ms,
            checkpoint_state,
            persist,
            Vec::new(),
        )
        .await
    }

    pub(super) async fn persist_turn_terminal_record_with_tool_executions(
        &self,
        kind: TurnTerminalKind,
        last_assistant_message: Option<String>,
        duration_ms: u64,
        checkpoint_state: Option<&TurnLocalCheckpointState>,
        persist: bool,
        terminal_tool_executions: Vec<ToolExecutionRecord>,
    ) -> Result<TurnTerminalRecord> {
        let record = {
            let guard = self.inner.agent.lock().await;
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
            TurnTerminalRecord {
                turn_id,
                turn_index: guard.state.turn_index,
                kind,
                reason: None,
                last_assistant_message,
                checkpoint,
                completed_at: chrono::Utc::now(),
                duration_ms,
            }
        };
        if persist {
            let transition = super::TurnTerminalTransition {
                turn_record: self.build_turn_record(&record).await?,
                terminal: record.clone(),
                prepared_work_item_completion: None,
                terminal_tool_executions,
            };
            self.persist_terminal_transition(&transition).await?;
        }
        Ok(record)
    }

    async fn interrupt_completion_report_protocol(
        &self,
        pending: &mut PendingCompletionReport,
        reason: &str,
        summary: &str,
        last_assistant_message: Option<String>,
        final_citations: Vec<Citation>,
        final_text_source_assistant_round_id: Option<String>,
        duration_ms: u64,
        persist_terminal: bool,
    ) -> Result<AgentLoopOutcome> {
        let completed_at = Utc::now();
        pending.tool_execution.status = ToolExecutionStatus::Interrupted;
        pending.tool_execution.completed_at = Some(completed_at);
        pending.tool_execution.duration_ms = completed_at
            .signed_duration_since(pending.tool_execution.created_at)
            .num_milliseconds()
            .max(0) as u64;
        pending.tool_execution.summary = summary.into();
        pending.tool_execution.output = serde_json::json!({
            "disposition": "interrupted",
            "reason": reason,
            "completion_request_id": pending.request_id,
        });
        let terminal = self
            .persist_turn_terminal_record_with_tool_executions(
                TurnTerminalKind::Aborted,
                last_assistant_message.clone(),
                duration_ms,
                None,
                persist_terminal,
                vec![pending.tool_execution.clone()],
            )
            .await?;
        Ok(AgentLoopOutcome {
            final_text: last_assistant_message.unwrap_or_default(),
            final_citations,
            final_text_source_assistant_round_id,
            turn_index: terminal.turn_index,
            terminal,
            should_sleep: false,
            sleep_duration_ms: None,
            allow_sleep_runnable_work_override: false,
            terminal_kind: TurnTerminalKind::Aborted,
            prepared_work_item_completion: None,
            terminal_tool_executions: vec![pending.tool_execution.clone()],
        })
    }

    pub(super) async fn maybe_defer_provider_lineage_failure(
        &self,
        agent_id: &str,
        round: usize,
        error: &anyhow::Error,
        last_assistant_message: Option<String>,
        duration_ms: u64,
        side_effect_boundary_crossed: bool,
        persist_terminal: bool,
    ) -> Result<Option<AgentLoopOutcome>> {
        let Some(timeline) = provider_attempt_timeline(error).cloned() else {
            return Ok(None);
        };
        let Some(fallback_ref) = timeline.pending_fallback_model_ref.as_deref() else {
            return Ok(None);
        };
        let Ok(fallback_model) = ModelRouteRef::parse_compatible(fallback_ref) else {
            return Ok(None);
        };
        let terminal_kind = if side_effect_boundary_crossed {
            TurnTerminalKind::ProviderFailedNeedsRecovery
        } else {
            TurnTerminalKind::DeferredToFallback
        };
        let error_text = error.to_string();
        let provider_failure_text = provider_lineage_failure_text(&error_text);
        let operator_message = provider_lineage_operator_message(
            fallback_ref,
            side_effect_boundary_crossed,
            &provider_failure_text,
        );
        self.inner.storage.append_event(&AuditEvent::legacy(
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
                persist_terminal,
            )
            .await?;
        let event_kind = if side_effect_boundary_crossed {
            "provider_failed_needs_recovery"
        } else {
            "deferred_to_fallback"
        };
        self.inner.storage.append_event(&AuditEvent::legacy(
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
        message.work_item_id = {
            let guard = self.inner.agent.lock().await;
            guard
                .state
                .current_execution_binding
                .as_ref()
                .and_then(|binding| binding.work_item_id.clone())
                .or_else(|| guard.state.current_turn_work_item_id.clone())
                .or_else(|| guard.state.current_work_item_id.clone())
        };
        let source_message_id = {
            let guard = self.inner.agent.lock().await;
            guard
                .state
                .current_execution_binding
                .as_ref()
                .map(|binding| binding.source_message_id.clone())
                .unwrap_or_else(|| terminal.turn_id.clone())
        };
        message.causation_id = Some(source_message_id.clone());
        message
            .source_refs
            .insert("source_turn_id".into(), terminal.turn_id.clone());
        message
            .source_refs
            .insert("source_message_id".into(), source_message_id.clone());
        message.metadata = Some(serde_json::json!({
            "provider_recovery": ProviderRecoveryDirective {
                fallback_model_ref: fallback_model,
                source_turn_id: terminal.turn_id.clone(),
                source_message_id,
                source_terminal_kind: terminal_kind,
                source_round: round,
            },
            "side_effect_boundary_crossed": side_effect_boundary_crossed,
        }));
        let queued = self.enqueue(message).await?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "recovery_enqueued",
            serde_json::json!({
                "agent_id": agent_id,
                "message_id": queued.id,
                "fallback_model_ref": fallback_ref,
                "source_terminal_kind": terminal_kind,
            }),
        ))?;
        Ok(Some(AgentLoopOutcome {
            final_text,
            final_citations: Vec::new(),
            final_text_source_assistant_round_id: None,
            turn_index: terminal.turn_index,
            terminal,
            should_sleep: false,
            sleep_duration_ms: None,
            allow_sleep_runnable_work_override: false,
            terminal_kind,
            prepared_work_item_completion: None,
            terminal_tool_executions: Vec::new(),
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

    pub(in crate::runtime) async fn drain_operator_interjections(
        &self,
        agent_id: &str,
        round: usize,
        boundary: scheduler::InterjectionBoundary,
    ) -> Result<Vec<String>> {
        let boundary_str = boundary.as_str();
        let mut follow_up_texts = Vec::new();
        'outer: loop {
            let committed = {
                let mut attempt = 0;
                loop {
                    if attempt >= crate::runtime::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
                        return Err(anyhow::anyhow!(
                            "interjection OCC retry exhausted for agent {}",
                            agent_id
                        ));
                    }
                    let mut guard = self.inner.agent.lock().await;
                    let Some(message) = guard
                        .queue
                        .peek_next_matching(scheduler::is_operator_interjection_message)
                        .cloned()
                    else {
                        break 'outer;
                    };
                    let expected_state = guard.state.clone();
                    match self.operator_interjection_plan(
                        agent_id,
                        &expected_state,
                        &message,
                        round,
                        boundary_str,
                    )? {
                        OperatorInterjectionPlan::Admit => {}
                        OperatorInterjectionPlan::LegacyTurnDeferred {
                            scenario_class,
                            effective_mode,
                        } => {
                            drop(guard);
                            self.record_deferred_operator_interjection(
                                agent_id,
                                &expected_state,
                                &message,
                                round,
                                boundary_str,
                                scenario_class,
                                effective_mode,
                            )?;
                            break 'outer;
                        }
                    }
                    let mut committed_state = expected_state.clone();
                    committed_state.pending = guard.queue.len().saturating_sub(1);
                    let text = render_operator_interjection_text(&message);
                    let transcript = TranscriptEntry::new(
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
                    );
                    let queue_record = QueueEntryRecord {
                        message_id: message.id.clone(),
                        agent_id: message.agent_id.clone(),
                        priority: message.priority.clone(),
                        status: QueueEntryStatus::Interjected,
                        created_at: message.created_at,
                        updated_at: chrono::Utc::now(),
                    };
                    let audit_event = AuditEvent::legacy(
                        "operator_interjection_admitted",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "round": round,
                            "boundary": boundary_str,
                            "message_id": message.id,
                            "origin": message.origin,
                            "authority_class": message.authority_class,
                            "priority": message.priority,
                            "delivery_surface": message.delivery_surface,
                            "admission_context": message.admission_context,
                            "text_preview": truncate_preview(
                                &message_text(&message.body),
                                ROUND_TEXT_PREVIEW_LIMIT
                            ),
                        }),
                    );
                    let commit_result = self.inner.runtime_db.transitions().commit_queue(
                        &crate::runtime_db::transitions::QueueTransitionCommand {
                            agent_id: message.agent_id.clone(),
                            operation: crate::runtime_db::transitions::QueueOperation::Interject,
                            mutation: crate::runtime_db::transitions::QueueMutation::Consume(
                                queue_record,
                            ),
                            scheduler_claim_work_item: None,
                            agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                                expected: Some(Box::new(expected_state)),
                                record: Box::new(committed_state.clone()),
                            }),
                            message_evidence: Vec::new(),
                            transcript_entries: vec![transcript],
                            turn_record: None,
                            audit_events: vec![audit_event],
                            notify_scheduler: false,
                            fault: self.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        },
                    );
                    let mut commit = match commit_result {
                        Ok(commit) => commit,
                        Err(error) => {
                            let can_retry = attempt + 1
                                < crate::runtime::ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                                && crate::runtime::retryable_enqueue_conflict(&error, agent_id);
                            if !can_retry {
                                return Err(error);
                            }
                            drop(guard);
                            if !self.refresh_enqueue_agent_state_baseline(agent_id).await? {
                                return Err(error);
                            }
                            attempt += 1;
                            continue;
                        }
                    };
                    if !commit.applied {
                        return Err(anyhow::anyhow!(
                            "interjection settlement made no durable progress"
                        ));
                    }
                    guard
                        .queue
                        .pop_next_matching(|candidate| candidate.id == message.id)
                        .expect("peeked interjection remains queued while agent lock is held");
                    guard.state = committed_state.clone();
                    guard.last_persisted_state = committed_state;
                    commit.effects.agent_state = None;
                    break (text, commit);
                }
            };
            self.apply_transition_commit(committed.1).await;
            follow_up_texts.push(committed.0);
        }
        Ok(follow_up_texts)
    }

    fn operator_interjection_plan(
        &self,
        agent_id: &str,
        expected_state: &crate::types::AgentState,
        message: &MessageEnvelope,
        round: usize,
        boundary: &str,
    ) -> Result<OperatorInterjectionPlan> {
        use crate::domain::scheduler::ScenarioMode;
        use crate::types::ExecutionAdmissionProvenance;

        let execution_binding = expected_state
            .current_execution_binding
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("operator interjection requires a current execution binding")
            })?;
        if execution_binding.source_message_id.is_empty()
            || execution_binding.turn_id.is_empty()
            || expected_state.current_turn_id.as_deref() != Some(execution_binding.turn_id.as_str())
        {
            return Err(anyhow::anyhow!(
                "operator interjection execution binding disagrees with the current turn"
            ));
        }
        let activation_id = match (
            execution_binding.activation_id.as_deref(),
            execution_binding.admission_provenance.as_ref(),
        ) {
            (
                Some(activation_id),
                Some(ExecutionAdmissionProvenance::Canonical {
                    activation_id: provenance_activation_id,
                    ..
                }),
            ) if activation_id == provenance_activation_id => activation_id,
            (
                None,
                Some(ExecutionAdmissionProvenance::LegacyCompat {
                    scenario_class,
                    effective_mode,
                }),
            ) if matches!(effective_mode, ScenarioMode::Off | ScenarioMode::Shadow) => {
                return Ok(OperatorInterjectionPlan::LegacyTurnDeferred {
                    scenario_class: *scenario_class,
                    effective_mode: *effective_mode,
                });
            }
            (None, Some(ExecutionAdmissionProvenance::Canonical { .. })) => {
                return Err(anyhow::anyhow!(
                    "canonical operator interjection admission is missing its activation"
                ));
            }
            (_, None) => {
                return Err(anyhow::anyhow!(
                    "operator interjection requires typed execution admission provenance"
                ));
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "operator interjection execution admission provenance disagrees with activation"
                ));
            }
        };
        let execution = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(agent_id)?
            .ok_or_else(|| {
                anyhow::anyhow!("operator interjection requires unified execution authority")
            })?;
        let attempt = execution.attempts.get(activation_id).ok_or_else(|| {
            anyhow::anyhow!("operator interjection references an unknown execution attempt")
        })?;
        if attempt.state != crate::domain::execution_protocol::ExecutionAttemptState::Open
            || attempt.source_message_id.as_deref()
                != Some(execution_binding.source_message_id.as_str())
        {
            return Err(anyhow::anyhow!(
                "operator interjection attempt disagrees with the current source message"
            ));
        }
        match &attempt.binding {
            crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id }
                if execution_binding.work_item_id.as_deref() == Some(work_item_id.as_str()) => {}
            crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle {
                agent_id: owner_agent_id,
            } if owner_agent_id == agent_id && execution_binding.work_item_id.is_none() => {}
            crate::domain::execution_protocol::ExecutionBinding::Conversation { .. }
                if execution_binding.work_item_id.is_none() => {}
            _ => {
                return Err(anyhow::anyhow!(
                    "operator interjection execution binding disagrees with attempt owner"
                ));
            }
        }

        let _ = (message, round, boundary);
        Ok(OperatorInterjectionPlan::Admit)
    }

    fn record_deferred_operator_interjection(
        &self,
        agent_id: &str,
        expected_state: &crate::types::AgentState,
        message: &MessageEnvelope,
        round: usize,
        boundary: &str,
        scenario_class: Option<crate::domain::scheduler::SchedulerScenarioClass>,
        effective_mode: crate::domain::scheduler::ScenarioMode,
    ) -> Result<()> {
        let turn_id = expected_state
            .current_turn_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("deferred interjection requires a current turn"))?;
        let mut event = AuditEvent::legacy(
            "operator_interjection_deferred_no_canonical_activation",
            serde_json::json!({
                "agent_id": agent_id,
                "turn_id": turn_id,
                "message_id": message.id,
                "round": round,
                "boundary": boundary,
                "scenario_class": scenario_class.map(|scenario| scenario.as_str()),
                "effective_mode": effective_mode,
            }),
        );
        event.id = format!(
            "operator_interjection_deferred:{}:{}:{}",
            turn_id, message.id, boundary
        );
        event.created_at = message.created_at;
        self.inner.storage.append_event(&event)?;
        Ok(())
    }

    pub(super) async fn append_operator_interjections_to_last_round(
        &self,
        agent_id: &str,
        round: usize,
        boundary: scheduler::InterjectionBoundary,
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

    #[cfg(test)]
    pub(crate) async fn run_agent_loop(
        &self,
        agent_id: &str,
        authority_class: AuthorityClass,
        effective_prompt: EffectivePrompt,
        loop_control: LoopControlOptions,
    ) -> Result<AgentLoopOutcome> {
        self.reconfigure_provider_for_turn(None).await?;
        Box::pin(
            TurnExecution {
                runtime: self,
                agent_id,
                authority_class,
                effective_prompt,
                model_selection: TurnModelSelection::default(),
                loop_control,
                persist_terminal: true,
            }
            .run(),
        )
        .await
    }

    pub(crate) async fn run_agent_loop_deferred(
        &self,
        agent_id: &str,
        authority_class: AuthorityClass,
        effective_prompt: EffectivePrompt,
        model_selection: TurnModelSelection,
        loop_control: LoopControlOptions,
    ) -> Result<AgentLoopOutcome> {
        Box::pin(
            TurnExecution {
                runtime: self,
                agent_id,
                authority_class,
                effective_prompt,
                model_selection,
                loop_control,
                persist_terminal: false,
            }
            .run(),
        )
        .await
    }
}

pub(super) const TOOL_AUDIT_INPUT_STRING_LIMIT: usize = 4_096;
pub(super) const TOOL_AUDIT_INPUT_PREVIEW_LIMIT: usize = 240;
const TOOL_AUDIT_REDACTED: &str = "[REDACTED]";
const RECENT_TURNS_RETRY_LIMIT: usize = 3;
const RECENT_TURNS_RETRY_SAFETY_MARGIN_TOKENS: usize = 128;

pub(super) fn tool_audit_input_field(call: &ToolCall) -> Option<Value> {
    if call.name == crate::tool::names::APPLY_PATCH {
        return Some(audit_large_input(&call.input));
    }
    Some(sanitize_audit_input(&call.input, None))
}

fn sanitize_audit_input(value: &Value, key: Option<&str>) -> Value {
    if key.is_some_and(is_sensitive_audit_input_key) {
        return Value::String(TOOL_AUDIT_REDACTED.into());
    }
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), sanitize_audit_input(value, Some(key.as_str()))))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| sanitize_audit_input(value, None))
                .collect(),
        ),
        Value::String(value) => Value::String(truncate_audit_input_string(
            value,
            TOOL_AUDIT_INPUT_STRING_LIMIT,
        )),
        _ => value.clone(),
    }
}

fn audit_large_input(value: &Value) -> Value {
    let serialized = value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string());
    let mut audit = Map::new();
    audit.insert(
        "preview".into(),
        Value::String(truncate_audit_input_string(
            &serialized,
            TOOL_AUDIT_INPUT_PREVIEW_LIMIT,
        )),
    );
    audit.insert("bytes".into(), Value::from(serialized.len() as u64));
    audit.insert(
        "truncated".into(),
        Value::Bool(serialized.chars().count() > TOOL_AUDIT_INPUT_PREVIEW_LIMIT),
    );
    Value::Object(audit)
}

fn is_sensitive_audit_input_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', ' '], "_");
    [
        "secret",
        "token",
        "api_key",
        "apikey",
        "authorization",
        "password",
        "material",
        "capability",
    ]
    .iter()
    .any(|sensitive| normalized.contains(sensitive))
}

fn truncate_audit_input_string(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
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
    pub(super) model_selection: TurnModelSelection,
    pub(super) loop_control: LoopControlOptions,
    pub(super) persist_terminal: bool,
}

impl TurnExecution<'_> {
    pub(super) async fn run(self) -> Result<AgentLoopOutcome> {
        let TurnExecution {
            runtime,
            agent_id,
            authority_class,
            mut effective_prompt,
            model_selection,
            loop_control,
            persist_terminal,
        } = self;
        let mut completed_rounds = Vec::<TurnRoundRecord>::new();
        let turn_started_at = Instant::now();
        let mut sleep_duration_ms = None;
        let mut completed_work_item_this_turn = false;
        let mut prepared_work_item_completion = None;
        let mut pending_completion_report: Option<PendingCompletionReport> = None;
        let mut round = 0usize;
        let mut truncated_text_history = Vec::new();
        let mut truncated_citation_history = Vec::<Citation>::new();
        let mut last_assistant_message: Option<String> = None;
        let mut last_assistant_citations = Vec::<Citation>::new();
        let mut last_assistant_round_id: Option<String> = None;
        let mut max_output_recovery_count = 0usize;
        let mut rounds_since_work_item_update = 0usize;
        let mut rounds_since_work_item_reminder = work_item_stale_reminder_cooldown_rounds();
        let mut checkpoint_state = {
            let guard = runtime.inner.agent.lock().await;
            checkpoint_state_from_last_terminal(guard.state.last_turn_terminal.as_ref())
        };
        let (turn_model_override, turn_model_state) = {
            let guard = runtime.inner.agent.lock().await;
            (
                guard.state.model_override.clone(),
                runtime.model_state_for_turn(&guard.state, model_selection.fallback_model()),
            )
        };
        let identity = runtime.agent_identity_view().await?;
        let (
            provider,
            available_tools,
            _apply_patch_surface,
            native_web_search,
            builtin_web_search_selection,
        ) = runtime
            .provider_tool_selection_for_turn(&identity, model_selection.fallback_model())
            .await?;
        let allowed_tool_names = available_tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<HashSet<_>>();
        let tool_schema_fingerprint = tool_capability_projection_fingerprint(&available_tools);
        runtime.inner.storage.append_event(&AuditEvent::legacy(
            "lineage_selected",
            serde_json::json!({
                "agent_id": agent_id,
                "model_override": turn_model_override,
                "recovery_fallback_model": model_selection.fallback_model(),
                "model": turn_model_state,
                "builtin_web_search_selection": builtin_web_search_selection,
                "tool_capability_projection": {
                    "policy": "static_agent_profile_runtime_route",
                    "pruning": "none",
                    "reason": "no provider-specific tool pruning declared",
                    "tool_count": available_tools.len(),
                    "tool_names": available_tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(),
                    "schema_fingerprint": tool_schema_fingerprint,
                },
            }),
        ))?;
        if let Some(pending) = model_selection.fallback_model() {
            runtime.inner.storage.append_event(&AuditEvent::legacy(
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
                            persist_terminal,
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
                            persist_terminal,
                        )
                        .await?;
                    return Ok(AgentLoopOutcome {
                        final_text,
                        final_citations: Vec::new(),
                        final_text_source_assistant_round_id: None,
                        turn_index: terminal.turn_index,
                        terminal,
                        should_sleep: false,
                        sleep_duration_ms: None,
                        allow_sleep_runnable_work_override: false,
                        terminal_kind: TurnTerminalKind::Aborted,
                        prepared_work_item_completion: None,
                        terminal_tool_executions: Vec::new(),
                    });
                }
            }
            if round > 1 && pending_completion_report.is_none() {
                runtime
                    .append_operator_interjections_to_last_round(
                        agent_id,
                        round,
                        scheduler::InterjectionBoundary::BeforeProviderContinuation,
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
                turn_local_compaction,
            ) = if round == 1 {
                let request_build_started = std::time::Instant::now();
                let request = build_initial_provider_turn_request(
                    provider.as_ref(),
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
                        None,
                    ),
                    Err(err) => {
                        if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                            runtime
                                .persist_turn_aborted_record(
                                    &aborted.run_id,
                                    &aborted.reason,
                                    last_assistant_message.clone(),
                                    turn_started_at.elapsed().as_millis() as u64,
                                    persist_terminal,
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
                                persist_terminal,
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
                                persist_terminal,
                            )
                            .await?
                        {
                            return Ok(outcome);
                        }
                        runtime
                            .persist_turn_terminal_record(
                                TurnTerminalKind::Aborted,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                                None,
                                persist_terminal,
                            )
                            .await?;
                        return Err(err);
                    }
                }
            } else {
                let context_config = runtime.current_context_config().await;
                let (turn_index, turn_budget) = {
                    let guard = runtime.inner.agent.lock().await;
                    (guard.state.turn_index, guard.state.turn_budget.clone())
                };
                let checkpoint_request_id =
                    Some(format!("turn-{turn_index}-round-{round}-checkpoint"));
                let mut prompt_frame = build_provider_prompt_frame(&effective_prompt);
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
                    // Continuation reminders are part of the complete provider request, so this
                    // check uses the model prompt budget rather than the recent-turns sub-budget.
                    let request_prompt_budget = context_config.prompt_budget_estimated_tokens;
                    if runtime_reminder_fits_baseline(
                        &prompt_frame,
                        &available_tools,
                        request_prompt_budget,
                        &reminder,
                    ) {
                        Some((work_item, reminder))
                    } else {
                        let event = AuditEvent::legacy(
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
                    runtime.inner.storage.append_event(&AuditEvent::legacy(
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
                let budget_warning = if let Some(budget) = turn_budget.as_ref() {
                    let turns_elapsed = turn_index.saturating_sub(budget.run_start_turn_index);

                    if turns_elapsed >= budget.max_turns.saturating_sub(1) {
                        let warning = build_turn_budget_warning(budget.max_turns, turns_elapsed);
                        runtime.inner.storage.append_event(&AuditEvent::legacy(
                            "turn_budget_warning_injected",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "round": round,
                                "max_turns": budget.max_turns,
                                "turns_elapsed": turns_elapsed,
                                "text_preview": truncate_preview(&warning, ROUND_TEXT_PREVIEW_LIMIT),
                            }),
                        ))?;
                        Some(warning)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let runtime_reminder: Option<String> = match (
                    stale_work_item_reminder
                        .as_ref()
                        .map(|(_, reminder)| reminder.as_str()),
                    budget_warning.as_deref(),
                ) {
                    (Some(work_item), Some(budget)) => Some(format!("{work_item}\n\n{budget}")),
                    (Some(reminder), None) => Some(reminder.to_string()),
                    (None, Some(budget)) => Some(budget.to_string()),
                    (None, None) => None,
                };
                let mut recent_turns_budget = effective_prompt.recent_turns_initial_budget();
                let mut recent_turns_retry_attempts = 0usize;
                let projection = loop {
                    match build_turn_local_projection_with_runtime_reminder(
                        &prompt_frame,
                        &completed_rounds,
                        &available_tools,
                        &checkpoint_state,
                        checkpoint_request_id.clone(),
                        // Turn-local continuation projection covers the complete provider request.
                        // The bounded turn projection budget only applies to initial recent_turns.
                        context_config.prompt_budget_estimated_tokens,
                        context_config.compaction_trigger_estimated_tokens,
                        context_config.compaction_keep_recent_estimated_tokens,
                        turn_model_state
                            .resolved_policy
                            .tool_output_truncation_estimated_tokens,
                        runtime_reminder.as_deref(),
                    ) {
                        TurnLocalProjectionOutcome::Projection(projection) => break projection,
                        TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics)
                            if diagnostics.reason == "minimum_exact_round_unfit"
                                && recent_turns_retry_attempts < RECENT_TURNS_RETRY_LIMIT
                                && recent_turns_budget.is_some() =>
                        {
                            let current_budget = recent_turns_budget.unwrap_or_default();
                            let deficit = diagnostics
                                .minimum_projection_estimated_tokens
                                .saturating_sub(diagnostics.effective_budget_estimated_tokens);
                            let next_budget = current_budget.saturating_sub(
                                deficit.saturating_add(RECENT_TURNS_RETRY_SAFETY_MARGIN_TOKENS),
                            );
                            if next_budget == current_budget {
                                recent_turns_budget = None;
                                continue;
                            }
                            let Some(reprojected_prompt) = effective_prompt.reproject_recent_turns(
                                &runtime.inner.storage,
                                next_budget,
                                &available_tools,
                            ) else {
                                recent_turns_budget = None;
                                continue;
                            };
                            recent_turns_retry_attempts += 1;
                            runtime.inner.storage.append_event(&AuditEvent::legacy(
                                "turn_local_recent_turns_retry",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "round": round,
                                    "attempt": recent_turns_retry_attempts,
                                    "reason": &diagnostics.reason,
                                    "previous_recent_turns_budget": current_budget,
                                    "next_recent_turns_budget": next_budget,
                                    "deficit_estimated_tokens": deficit,
                                    "previous_context_attachment_estimated_tokens": diagnostics.context_attachment_estimated_tokens,
                                }),
                            ))?;
                            effective_prompt = reprojected_prompt;
                            prompt_frame = build_provider_prompt_frame(&effective_prompt);
                            recent_turns_budget = Some(next_budget);
                        }
                        TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                            runtime.inner.storage.append_event(&AuditEvent::legacy(
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
                                    "recent_turns_retry_attempts": recent_turns_retry_attempts,
                                    "final_recent_turns_budget": recent_turns_budget,
                                }),
                            ))?;
                            let final_text = format!(
                                "Turn stopped because the continuation baseline exceeded the prompt budget after {} recent-turns recovery attempt(s) (reason={}, estimated_baseline_tokens={}, minimum_projection_estimated_tokens={}, effective_budget_estimated_tokens={}, tool_overhead_estimated_tokens={}).",
                                recent_turns_retry_attempts,
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
                                    persist_terminal,
                                )
                                .await?;
                            return Ok(AgentLoopOutcome {
                                final_text,
                                final_citations: Vec::new(),
                                final_text_source_assistant_round_id: None,
                                turn_index: terminal.turn_index,
                                terminal,
                                should_sleep: false,
                                sleep_duration_ms: None,
                                allow_sleep_runnable_work_override: false,
                                terminal_kind: TurnTerminalKind::BaselineOverBudget,
                                prepared_work_item_completion: None,
                                terminal_tool_executions: Vec::new(),
                            });
                        }
                    }
                };
                let turn_local_compaction = projection.compaction.as_ref().map(|compaction| {
                    serde_json::json!({
                        "trigger_reason": compaction.trigger_reason,
                        "prompt_budget_estimated_tokens": compaction.prompt_budget_estimated_tokens,
                        "compaction_trigger_estimated_tokens": compaction.compaction_trigger_estimated_tokens,
                        "compaction_keep_recent_estimated_tokens": compaction.keep_recent_estimated_tokens,
                        "tool_output_truncation_estimated_tokens": compaction.tool_output_budget_estimated_tokens,
                        "pre_compaction_estimated_tokens": compaction.pre_compaction_estimated_tokens,
                        "compacted_rounds": compaction.compacted_rounds,
                        "exact_tail_rounds": compaction.exact_tail_rounds,
                        "degraded_rounds": compaction.degraded_rounds,
                        "projected_estimated_tokens": compaction.projected_estimated_tokens,
                        "effective_budget_estimated_tokens": compaction.effective_budget_estimated_tokens,
                        "tool_overhead_estimated_tokens": compaction.tool_overhead_estimated_tokens,
                        "compacted_tool_results": compaction.compacted_tool_results,
                        "preserved_artifact_refs": compaction.preserved_artifact_refs,
                        "strict_fallback_applied": compaction.strict_fallback_applied,
                        "checkpoint_request_id": compaction.checkpoint_request_id,
                        "checkpoint_mode": compaction.checkpoint_mode.map(|mode| mode.as_str()),
                        "checkpoint_anchor_generation": compaction.checkpoint_anchor_generation,
                        "checkpoint_base_round": compaction.checkpoint_base_round,
                        "previous_checkpoint_round": compaction.previous_checkpoint_round,
                        "anchor_changed_since_checkpoint": compaction.anchor_changed_since_checkpoint,
                        "last_round_degraded": compaction.last_round_degraded,
                    })
                });
                if let Some(compaction) = projection.compaction.as_ref() {
                    runtime.inner.storage.append_event(&AuditEvent::legacy(
                        "turn_local_compaction_applied",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "round": round,
                            "trigger_reason": compaction.trigger_reason,
                            "prompt_budget_estimated_tokens": compaction.prompt_budget_estimated_tokens,
                            "compaction_trigger_estimated_tokens": compaction.compaction_trigger_estimated_tokens,
                            "compaction_keep_recent_estimated_tokens": compaction.keep_recent_estimated_tokens,
                            "tool_output_truncation_estimated_tokens": compaction.tool_output_budget_estimated_tokens,
                            "pre_compaction_estimated_tokens": compaction.pre_compaction_estimated_tokens,
                            "compacted_rounds": compaction.compacted_rounds,
                            "exact_tail_rounds": compaction.exact_tail_rounds,
                            "degraded_rounds": compaction.degraded_rounds,
                            "projected_estimated_tokens": compaction.projected_estimated_tokens,
                            "effective_budget_estimated_tokens": compaction.effective_budget_estimated_tokens,
                            "tool_overhead_estimated_tokens": compaction.tool_overhead_estimated_tokens,
                            "compacted_tool_results": compaction.compacted_tool_results,
                            "preserved_artifact_refs": compaction.preserved_artifact_refs,
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
                        runtime.inner.storage.append_event(&AuditEvent::legacy(
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
                    crate::provider::ContinuationScopeId::new(agent_id),
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
                        turn_local_compaction,
                    ),
                    Err(err) => {
                        if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                            runtime
                                .persist_turn_aborted_record(
                                    &aborted.run_id,
                                    &aborted.reason,
                                    last_assistant_message.clone(),
                                    turn_started_at.elapsed().as_millis() as u64,
                                    persist_terminal,
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
                                persist_terminal,
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
                                persist_terminal,
                            )
                            .await?
                        {
                            return Ok(outcome);
                        }
                        runtime
                            .persist_turn_terminal_record(
                                TurnTerminalKind::Aborted,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                                None,
                                persist_terminal,
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
            let pending_checkpoint_metadata = checkpoint_state.pending.as_ref().map(|pending| {
                (
                    pending.request_id.clone(),
                    pending.mode,
                    pending.requested_at_round,
                )
            });
            let round_purpose = if pending_checkpoint_metadata.is_some() {
                AssistantRoundPurpose::RuntimeCheckpoint
            } else {
                AssistantRoundPurpose::AgentResponse
            };
            let mut tool_calls = Vec::new();
            let mut text_blocks = Vec::new();
            let mut citation_blocks = Vec::<Citation>::new();
            let mut thinking_block_count = 0usize;

            for block in &assistant_blocks {
                match block {
                    ModelBlock::Text { text } => {
                        if !text.trim().is_empty() {
                            text_blocks.push(text.clone());
                        }
                    }
                    ModelBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    ModelBlock::Citations { citations } => {
                        extend_unique_citations(&mut citation_blocks, citations.iter().cloned());
                    }
                    ModelBlock::Thinking { .. }
                    | ModelBlock::ReasoningText { .. }
                    | ModelBlock::RedactedThinking { .. } => {
                        thinking_block_count += 1;
                    }
                }
            }

            crate::diagnostics::record_turn_provider_round(provider_round_started.elapsed());
            crate::diagnostics::record_provider_round_total(provider_round_started.elapsed());
            let completed_round_assistant_blocks = assistant_blocks.clone();
            let only_legacy_sleep_tool_calls = !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|call| call.name == crate::tool::names::SLEEP);
            let legacy_sleep_duration_ms = if only_legacy_sleep_tool_calls {
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
            let current_round_has_assistant_text = !combined_text.is_empty();
            if round_purpose == AssistantRoundPurpose::AgentResponse {
                let aggregated_text = combine_text_history(&truncated_text_history, &text_blocks)
                    .into_iter()
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !aggregated_text.is_empty() {
                    last_assistant_message = Some(aggregated_text);
                    last_assistant_citations = truncated_citation_history.clone();
                    extend_unique_citations(
                        &mut last_assistant_citations,
                        citation_blocks.iter().cloned(),
                    );
                } else if !truncated_text_history.is_empty() {
                    // If current round has no text, preserve text history from previous rounds.
                    let history_text = truncated_text_history
                        .iter()
                        .map(|text| text.trim())
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !history_text.is_empty() {
                        last_assistant_message = Some(history_text);
                        last_assistant_citations = truncated_citation_history.clone();
                    }
                }
            }
            let token_usage = TokenUsage::new(response.input_tokens, response.output_tokens);
            let checkpoint_request_id = pending_checkpoint_metadata
                .as_ref()
                .map(|(request_id, _, _)| request_id.clone());
            let checkpoint_mode = pending_checkpoint_metadata
                .as_ref()
                .map(|(_, mode, _)| mode.as_str());
            let checkpoint_requested_at_round = pending_checkpoint_metadata
                .as_ref()
                .map(|(_, _, requested_at_round)| *requested_at_round);

            runtime.inner.storage.append_event(&AuditEvent::legacy(
                "provider_round_completed",
                serde_json::json!({
                    "agent_id": agent_id,
                    "turn_index": turn_index,
                    "run_id": run_id,
                    "round": round,
                    "round_purpose": round_purpose.as_str(),
                    "visibility": if round_purpose == AssistantRoundPurpose::RuntimeCheckpoint {
                        "runtime_private"
                    } else {
                        "operator_visible"
                    },
                    "checkpoint_request_id": checkpoint_request_id.clone(),
                    "checkpoint_mode": checkpoint_mode,
                    "checkpoint_requested_at_round": checkpoint_requested_at_round,
                    "work_item_id": round_work_item_id.clone(),
                    "stop_reason": stop_reason,
                    "context_build_ms": context_build_ms,
                    "provider_round_ms": provider_round_ms,
                    "provider_started_at": provider_started_at,
                    "provider_completed_at": provider_completed_at,
                    "input_tokens": response.input_tokens,
                    "output_tokens": response.output_tokens,
                    "token_usage": token_usage,
                    "provider_message_id": response.provider_message_id,
                    "provider_request_id": response.provider_request_id,
                    "tool_call_count": tool_calls.len(),
                    "tool_names": tool_calls.iter().map(|call| call.name.clone()).collect::<Vec<_>>(),
                    "text_block_count": text_blocks.len(),
                    "text_char_count": combined_text.chars().count(),
                    "only_sleep_tools": only_legacy_sleep_tool_calls,
                    "provider_cache_usage": cache_usage,
                    "prompt_cache_key": effective_prompt.cache_identity.prompt_cache_key.clone(),
                    "context_fingerprint": effective_prompt.cache_identity.context_fingerprint.clone(),
                    "compression_epoch": effective_prompt.cache_identity.compression_epoch,
                    "requested_model": model_attempt_state.requested_model.clone(),
                    "active_model": model_attempt_state.active_model.clone(),
                    "fallback_active": model_attempt_state.fallback_active,
                    "context_management": context_management,
                    "turn_local_compaction": turn_local_compaction,
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
                    checkpoint_state.mark_operator_delivery_pending();
                }
                runtime.inner.storage.append_event(&AuditEvent::legacy(
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
                        "round_purpose": round_purpose.as_str(),
                        "visibility": if round_purpose == AssistantRoundPurpose::RuntimeCheckpoint {
                            "runtime_private"
                        } else {
                            "operator_visible"
                        },
                        "checkpoint_request_id": checkpoint_request_id.clone(),
                        "checkpoint_mode": checkpoint_mode,
                        "checkpoint_requested_at_round": checkpoint_requested_at_round,
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
            if round_purpose == AssistantRoundPurpose::AgentResponse
                && current_round_has_assistant_text
            {
                last_assistant_round_id = Some(assistant_round_id.clone());
            }
            runtime.inner.storage.append_event(&AuditEvent::legacy(
                "assistant_round_recorded",
                serde_json::json!({
                    "assistant_round_id": assistant_round_id,
                    "agent_id": agent_id,
                    "turn_index": turn_index,
                    "run_id": run_id,
                    "round": round,
                    "round_purpose": round_purpose.as_str(),
                    "visibility": if round_purpose == AssistantRoundPurpose::RuntimeCheckpoint {
                        "runtime_private"
                    } else {
                        "operator_visible"
                    },
                    "checkpoint_request_id": checkpoint_request_id.clone(),
                    "checkpoint_mode": checkpoint_mode,
                    "checkpoint_requested_at_round": checkpoint_requested_at_round,
                    "work_item_id": round_work_item_id.clone(),
                    "stop_reason": stop_reason,
                    "text_block_count": text_blocks.len(),
                    "text_char_count": combined_text.chars().count(),
                    "tool_call_count": tool_calls.len(),
                    "tool_names": tool_calls.iter().map(|call| call.name.clone()).collect::<Vec<_>>(),
                    "has_text": !combined_text.is_empty(),
                    "has_tool_calls": !tool_calls.is_empty(),
                    "thinking_block_count": thinking_block_count,
                    "has_thinking": thinking_block_count > 0,
                }),
            ))?;

            if let Some(mut pending) = pending_completion_report.take() {
                if !tool_calls.is_empty() || combined_text.trim().is_empty() {
                    let reason = if !tool_calls.is_empty() {
                        "tool_call_not_allowed"
                    } else {
                        "empty_completion_report"
                    };
                    if !pending.corrective_retry_attempted {
                        pending.corrective_retry_attempted = true;
                        let continuation_text = "Completion report expected. Reply with the final operator-facing completion report as text only. Do not call any tool.".to_string();
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
                                "reason": "completion_report_expected",
                                "completion_request_id": pending.request_id,
                            }),
                        ))?;
                        runtime.inner.storage.append_event(&AuditEvent::legacy(
                            "completion_report_request_corrective_retry",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "completion_request_id": pending.request_id,
                                "work_item_id": pending.work_item_id,
                                "reason": reason,
                            }),
                        ))?;
                        pending_completion_report = Some(pending);
                        continue;
                    }
                    runtime.inner.storage.append_event(&AuditEvent::legacy(
                        "completion_report_request_abandoned",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "completion_request_id": pending.request_id,
                            "work_item_id": pending.work_item_id,
                            "reason": reason,
                        }),
                    ))?;
                    return runtime
                        .interrupt_completion_report_protocol(
                            &mut pending,
                            "report_protocol_abandoned",
                            "Interrupted: completion report protocol abandoned",
                            last_assistant_message.clone(),
                            last_assistant_citations.clone(),
                            last_assistant_round_id.clone(),
                            turn_started_at.elapsed().as_millis() as u64,
                            persist_terminal,
                        )
                        .await;
                }

                let state = runtime.agent_state().await?;
                let current = runtime.latest_work_item(&pending.work_item_id).await?;
                let invalidation_reason = if state.current_execution_binding.as_ref()
                    != Some(&pending.execution_binding)
                {
                    Some("execution_binding_changed")
                } else if current.is_none() {
                    Some("work_item_missing")
                } else if current
                    .as_ref()
                    .is_some_and(|current| current.revision != pending.expected_work_revision)
                {
                    Some("work_item_revision_changed")
                } else {
                    None
                };
                if let Some(reason) = invalidation_reason {
                    runtime.inner.storage.append_event(&AuditEvent::legacy(
                        "completion_report_request_invalidated",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "completion_request_id": pending.request_id,
                            "work_item_id": pending.work_item_id,
                            "reason": reason,
                        }),
                    ))?;
                    return runtime
                        .interrupt_completion_report_protocol(
                            &mut pending,
                            reason,
                            "Interrupted: completion report binding invalidated",
                            last_assistant_message.clone(),
                            last_assistant_citations.clone(),
                            last_assistant_round_id.clone(),
                            turn_started_at.elapsed().as_millis() as u64,
                            persist_terminal,
                        )
                        .await;
                }
                let candidate = crate::tool::spec::CompletionReportCandidate {
                    text: combined_text.clone(),
                    citations: citation_blocks.clone(),
                    source_turn_index: turn_index,
                    source_round: round,
                    source_turn_id: Some(pending.execution_binding.turn_id.clone()),
                    source_message_id: Some(pending.execution_binding.source_message_id.clone()),
                    source_assistant_round_id: assistant_round_id.clone(),
                    source_tool_call_id: pending.request_tool_call_id.clone(),
                };
                let warnings = pending
                    .warnings
                    .iter()
                    .filter_map(|warning| serde_json::from_value(warning.clone()).ok())
                    .collect::<Vec<_>>();
                let mut result =
                    crate::tool::tools::complete_work_item::complete_with_report_candidate(
                        runtime,
                        pending.work_item_id.clone(),
                        crate::runtime::WorkItemCompletionAuthority::AgentExecution(
                            pending.execution_binding.clone(),
                        ),
                        Some(&candidate),
                        warnings,
                        "followup_final_text",
                    )
                    .await?;
                let result_envelope = result.envelope.clone();
                let mut success_record = pending.tool_execution.clone();
                let completed_at = Utc::now();
                success_record.completed_at = Some(completed_at);
                success_record.duration_ms = completed_at
                    .signed_duration_since(success_record.created_at)
                    .num_milliseconds()
                    .max(0) as u64;
                success_record.status = ToolExecutionStatus::Success;
                success_record.output = serde_json::json!({
                    "envelope": result_envelope,
                    "is_error": false,
                    "should_sleep": result.should_sleep,
                    "sleep_duration_ms": result.sleep_duration_ms,
                    "error": null,
                    "completion_request_id": pending.request_id,
                });
                success_record.summary =
                    crate::tool::summary::tool_result_summary(&result.envelope);
                let mut prepared =
                    result.prepared_work_item_completion.take().ok_or_else(|| {
                        anyhow::anyhow!("follow-up completion did not prepare commit")
                    })?;
                prepared.tool_execution = Some(success_record);
                prepared.audit_events.push(AuditEvent::legacy(
                    "completion_report_request_completed",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "completion_request_id": pending.request_id,
                        "work_item_id": pending.work_item_id,
                        "request_turn_index": pending.request_turn_index,
                        "request_round": pending.request_round,
                        "request_assistant_round_id": pending.request_assistant_round_id,
                        "report_assistant_round_id": assistant_round_id,
                        "source": "followup_final_text",
                    }),
                ));
                prepared_work_item_completion = Some(prepared);
                let final_text = combined_text;
                let terminal = TurnTerminalRecord {
                    turn_id: state
                        .current_turn_id
                        .clone()
                        .filter(|turn_id| !turn_id.trim().is_empty())
                        .unwrap_or_else(crate::ids::turn_id),
                    turn_index,
                    kind: TurnTerminalKind::Completed,
                    reason: None,
                    last_assistant_message: Some(final_text.clone()),
                    checkpoint: terminal_checkpoint_from_state(&checkpoint_state, turn_index),
                    completed_at: Utc::now(),
                    duration_ms: turn_started_at.elapsed().as_millis() as u64,
                };
                return Ok(AgentLoopOutcome {
                    final_text,
                    final_citations: citation_blocks,
                    final_text_source_assistant_round_id: Some(assistant_round_id),
                    turn_index,
                    terminal,
                    should_sleep: true,
                    sleep_duration_ms: None,
                    allow_sleep_runnable_work_override: true,
                    terminal_kind: TurnTerminalKind::Completed,
                    prepared_work_item_completion: prepared_work_item_completion.take(),
                    terminal_tool_executions: Vec::new(),
                });
            }

            if tool_calls.is_empty() {
                runtime.inner.storage.append_event(&AuditEvent::legacy(
                    "text_only_round_observed",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "turn_index": turn_index,
                        "run_id": run_id,
                        "round": round,
                        "round_purpose": round_purpose.as_str(),
                        "visibility": if round_purpose == AssistantRoundPurpose::RuntimeCheckpoint {
                            "runtime_private"
                        } else {
                            "operator_visible"
                        },
                        "checkpoint_request_id": checkpoint_request_id,
                        "checkpoint_mode": checkpoint_mode,
                        "checkpoint_requested_at_round": checkpoint_requested_at_round,
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
                    .drain_operator_interjections(
                        agent_id,
                        round,
                        scheduler::InterjectionBoundary::AfterProviderRound,
                    )
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
                    .drain_operator_interjections(
                        agent_id,
                        round,
                        scheduler::InterjectionBoundary::BeforeToolExecution,
                    )
                    .await?;
            }

            if tool_calls.is_empty() && is_max_output_stop_reason(stop_reason.as_deref()) {
                if max_output_recovery_count < MAX_OUTPUT_RECOVERY_ATTEMPTS {
                    if round_purpose == AssistantRoundPurpose::AgentResponse
                        && !combined_text.is_empty()
                    {
                        truncated_text_history.push(combined_text.clone());
                        extend_unique_citations(
                            &mut truncated_citation_history,
                            citation_blocks.iter().cloned(),
                        );
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
                    runtime.inner.storage.append_event(&AuditEvent::legacy(
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
                runtime.inner.storage.append_event(&AuditEvent::legacy(
                    "turn_local_checkpoint_resume_requested",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "round": round,
                    }),
                ))?;
                continue;
            }

            if tool_calls.is_empty() {
                if checkpoint_state.operator_delivery_pending() {
                    if combined_text.is_empty() {
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
                                "reason": "checkpoint_operator_delivery_pending",
                            }),
                        ))?;
                        runtime.inner.storage.append_event(&AuditEvent::legacy(
                            "checkpoint_operator_delivery_retry",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "round": round,
                            }),
                        ))?;
                        continue;
                    }
                    checkpoint_state.clear_operator_delivery_pending();
                }
                let final_text = last_assistant_message.clone().unwrap_or_default();
                let terminal = runtime
                    .persist_turn_terminal_record(
                        TurnTerminalKind::Completed,
                        last_assistant_message.clone(),
                        turn_started_at.elapsed().as_millis() as u64,
                        Some(&checkpoint_state),
                        persist_terminal,
                    )
                    .await?;
                return Ok(AgentLoopOutcome {
                    final_text,
                    final_citations: last_assistant_citations.clone(),
                    final_text_source_assistant_round_id: last_assistant_round_id.clone(),
                    turn_index: terminal.turn_index,
                    terminal,
                    should_sleep: true,
                    sleep_duration_ms,
                    allow_sleep_runnable_work_override: completed_work_item_this_turn,
                    terminal_kind: TurnTerminalKind::Completed,
                    prepared_work_item_completion: prepared_work_item_completion.take(),
                    terminal_tool_executions: Vec::new(),
                });
            }

            let round_tool_calls = tool_calls.clone();
            let completion_report_texts =
                completion_report_texts_by_tool_id(&completed_round_assistant_blocks);
            let mut tool_results = Vec::new();
            let mut tool_result_envelopes = Vec::new();
            let mut tool_execution_refs: Vec<(String, String)> = Vec::new();
            let mut all_tool_results_should_sleep = !round_tool_calls.is_empty();
            let mut terminal_tool_transition = false;
            for (tool_call_index, call) in tool_calls.into_iter().enumerate() {
                if let Err(err) = runtime.ensure_not_aborted().await {
                    if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                        runtime
                            .persist_turn_aborted_record(
                                &aborted.run_id,
                                &aborted.reason,
                                last_assistant_message.clone(),
                                turn_started_at.elapsed().as_millis() as u64,
                                persist_terminal,
                            )
                            .await?;
                    }
                    return Err(err);
                }
                let tool_call_id = call.id.clone();
                let tool_name = call.name.clone();
                if !allowed_tool_names.contains(&call.name)
                    && call.name != crate::tool::names::SLEEP
                {
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
                    let (turn_index, run_id, work_item_id, turn_id) = {
                        let guard = runtime.inner.agent.lock().await;
                        (
                            guard.state.turn_index,
                            guard.state.current_run_id.clone(),
                            guard
                                .state
                                .current_turn_work_item_id
                                .clone()
                                .or_else(|| guard.state.current_work_item_id.clone()),
                            guard.state.current_turn_id.clone(),
                        )
                    };
                    let failed_id = crate::ids::tool_execution_id();
                    let now = chrono::Utc::now();
                    let failed_record = crate::types::ToolExecutionRecord {
                        id: failed_id.clone(),
                        agent_id: agent_id.to_string(),
                        work_item_id: work_item_id.clone(),
                        turn_index,
                        turn_id: turn_id.clone(),
                        tool_name: tool_name.clone(),
                        created_at: now,
                        completed_at: Some(now),
                        duration_ms: 0,
                        authority_class: authority_class.clone(),
                        status: crate::types::ToolExecutionStatus::Error,
                        input: call.input.clone(),
                        output: serde_json::json!({
                            "error": audit_error,
                            "tool_error": &error,
                        }),
                        summary: format!("Failed: {tool_name} not exposed for round"),
                        invocation_surface: None,
                    };
                    runtime.persist_tool_execution_evidence(&failed_record)?;
                    tool_execution_refs.push((tool_call_id.clone(), failed_id.clone()));
                    runtime.inner.storage.append_event(&AuditEvent::legacy(
                        "tool_execution_failed",
                        to_json_value(&ToolExecutionAuditEvent {
                            tool_call_id: tool_call_id.clone(),
                            tool_execution_id: failed_id,
                            agent_id: failed_record.agent_id.clone(),
                            tool_name: tool_name.clone(),
                            turn_index,
                            turn_id,
                            run_id,
                            work_item_id,
                            status: failed_record.status.clone(),
                            duration_ms: failed_record.duration_ms,
                            summary: failed_record.summary.clone(),
                            input: tool_audit_input_field(&call),
                            exec_command_cmd: command_preview_field(&call),
                            exec_command_display: command_display_field(&call),
                            exec_command_batch_items: command_batch_preview_field(&call),
                            exec_command_cost: command_cost_field(
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
                            exec_command_disposition: None,
                            exit_status: None,
                            task_handle: None,
                            error: Some(audit_error),
                            error_kind: Some(error.kind.clone()),
                            tool_error: Some(error.clone()),
                            reason: Some("tool_not_exposed_for_round".into()),
                        }),
                    ))?;
                    tool_results.push(ToolResultBlock {
                        tool_use_id: tool_call_id.clone(),
                        content: result_content,
                        is_error: true,
                        error: Some(error.clone()),
                    });
                    tool_result_envelopes.push(result.envelope);
                    all_tool_results_should_sleep = false;
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
                    runtime.inner.storage.append_event(&AuditEvent::legacy(
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
                    all_tool_results_should_sleep = false;
                    continue;
                }
                let (pre_tool_work_item_id, execution_binding) = {
                    let guard = runtime.inner.agent.lock().await;
                    (
                        guard
                            .state
                            .current_turn_work_item_id
                            .clone()
                            .or_else(|| guard.state.current_work_item_id.clone()),
                        guard.state.current_execution_binding.clone(),
                    )
                };
                let tool_execution_context = crate::tool::spec::ToolExecutionContext {
                    completion_report_candidate: completion_report_texts
                        .iter()
                        .find(|(candidate_tool_call_id, _, _)| {
                            candidate_tool_call_id == &tool_call_id
                        })
                        .map(
                            |(_, text, citations)| crate::tool::spec::CompletionReportCandidate {
                                text: text.clone(),
                                citations: citations.clone(),
                                source_turn_index: turn_index,
                                source_round: round,
                                source_turn_id: execution_binding
                                    .as_ref()
                                    .map(|binding| binding.turn_id.clone()),
                                source_message_id: execution_binding
                                    .as_ref()
                                    .map(|binding| binding.source_message_id.clone()),
                                source_assistant_round_id: assistant_round_id.clone(),
                                source_tool_call_id: tool_call_id.clone(),
                            },
                        ),
                };
                let tool_exec_started = std::time::Instant::now();
                let tool_execution = if let Some(snapshot) = runtime.current_run_abort_token().await
                {
                    tokio::select! {
                        result = runtime.inner.tools.execute_with_context(runtime, agent_id, &authority_class, &call, &tool_execution_context) => result,
                        _ = snapshot.token.cancelled() => Err(CurrentRunAborted {
                            run_id: snapshot.run_id.clone(),
                            reason: snapshot.reason(),
                        }.into()),
                    }
                } else {
                    runtime
                        .inner
                        .tools
                        .execute_with_context(
                            runtime,
                            agent_id,
                            &authority_class,
                            &call,
                            &tool_execution_context,
                        )
                        .await
                };
                crate::diagnostics::record_turn_tool_execution(tool_exec_started.elapsed());
                match tool_execution {
                    Ok((mut result, mut record)) => {
                        let result_content =
                            crate::tool::tools::render_tool_result_for_model(&result)?;
                        let loop_directive = result.loop_directive.take();
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
                        } else {
                            all_tool_results_should_sleep = false;
                        }
                        tool_execution_refs.push((tool_call_id.clone(), record.id.clone()));
                        let tool_executed_event = AuditEvent::legacy(
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
                                input: tool_audit_input_field(&call),
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
                                tool_error: result.tool_error().cloned(),
                                reason: None,
                            }),
                        );
                        let stops_tool_batch = result.prepared_work_item_completion.is_some();
                        if let Some(mut prepared) = result.prepared_work_item_completion.take() {
                            prepared.tool_execution = Some(record.clone());
                            prepared.audit_events.push(tool_executed_event);
                            prepared_work_item_completion = Some(prepared);
                        } else {
                            runtime.persist_tool_execution_evidence(&record)?;
                            runtime.inner.storage.append_event(&tool_executed_event)?;
                        }
                        if let Some(crate::tool::spec::ToolLoopDirective::AwaitCompletionReport(
                            directive,
                        )) = loop_directive
                        {
                            let execution_binding = execution_binding.clone().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "completion report request lost its execution binding"
                                )
                            })?;
                            anyhow::ensure!(
                                record.status == ToolExecutionStatus::Deferred,
                                "completion report request must persist a deferred tool execution"
                            );
                            runtime.inner.storage.append_event(&AuditEvent::legacy(
                                "completion_report_request_created",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "completion_request_id": directive.request_id,
                                    "work_item_id": directive.work_item_id,
                                    "expected_work_revision": directive.expected_work_revision,
                                    "turn_index": turn_index,
                                    "round": round,
                                    "assistant_round_id": assistant_round_id,
                                    "tool_call_id": tool_call_id,
                                    "tool_execution_id": record.id,
                                }),
                            ))?;
                            pending_completion_report = Some(PendingCompletionReport {
                                request_id: directive.request_id,
                                work_item_id: directive.work_item_id,
                                expected_work_revision: directive.expected_work_revision,
                                execution_binding,
                                request_turn_index: turn_index,
                                request_round: round,
                                request_assistant_round_id: assistant_round_id.clone(),
                                request_tool_call_id: tool_call_id.clone(),
                                tool_execution: record.clone(),
                                warnings: directive.warnings,
                                corrective_retry_attempted: false,
                            });
                        }
                        if prepared_work_item_completion.is_none()
                            && matches!(record.status, crate::types::ToolExecutionStatus::Success)
                        {
                            runtime
                                .record_skill_tool_activation(
                                    &record.tool_name,
                                    &record.input,
                                    &result,
                                )
                                .await?;
                        }
                        tool_result_envelopes.push(result.envelope.clone());
                        tool_results.push(ToolResultBlock {
                            tool_use_id: tool_call_id.clone(),
                            content: result_content.clone(),
                            is_error: result.is_error(),
                            error: result.tool_error().cloned(),
                        });
                        if pending_completion_report.is_some() {
                            break;
                        }
                        if result.terminal_transition || stops_tool_batch {
                            terminal_tool_transition = result.terminal_transition
                                || tool_call_index + 1 < round_tool_calls.len();
                            break;
                        }
                    }
                    Err(err) => {
                        if let Some(aborted) = err.downcast_ref::<CurrentRunAborted>() {
                            runtime
                                .persist_turn_aborted_record(
                                    &aborted.run_id,
                                    &aborted.reason,
                                    last_assistant_message.clone(),
                                    turn_started_at.elapsed().as_millis() as u64,
                                    persist_terminal,
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
                        let (work_item_id, turn_id) = {
                            let guard = runtime.inner.agent.lock().await;
                            (
                                guard
                                    .state
                                    .current_turn_work_item_id
                                    .clone()
                                    .or_else(|| guard.state.current_work_item_id.clone()),
                                guard.state.current_turn_id.clone(),
                            )
                        };
                        let failed_id = crate::ids::tool_execution_id();
                        let failed_record = crate::types::ToolExecutionRecord {
                            id: failed_id.clone(),
                            agent_id: agent_id.to_string(),
                            work_item_id: work_item_id.clone(),
                            turn_index,
                            turn_id: turn_id.clone(),
                            tool_name: tool_name.clone(),
                            created_at: chrono::Utc::now(),
                            completed_at: Some(chrono::Utc::now()),
                            duration_ms: tool_exec_started.elapsed().as_millis() as u64,
                            authority_class: authority_class.clone(),
                            status: crate::types::ToolExecutionStatus::Error,
                            input: call.input.clone(),
                            output: serde_json::json!({
                                "error": audit_error,
                                "tool_error": &error,
                            }),
                            summary: format!("Failed: {tool_name}"),
                            invocation_surface: None,
                        };
                        runtime.persist_tool_execution_evidence(&failed_record)?;
                        tool_execution_refs.push((tool_call_id.clone(), failed_id.clone()));
                        runtime.inner.storage.append_event(&AuditEvent::legacy(
                            "tool_execution_failed",
                            to_json_value(&ToolExecutionAuditEvent {
                                tool_call_id: tool_call_id.clone(),
                                tool_execution_id: failed_id,
                                agent_id: failed_record.agent_id.clone(),
                                tool_name: tool_name.clone(),
                                turn_index,
                                turn_id,
                                run_id,
                                work_item_id,
                                status: failed_record.status.clone(),
                                duration_ms: failed_record.duration_ms,
                                summary: failed_record.summary.clone(),
                                input: tool_audit_input_field(&call),
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
                                exec_command_disposition: None,
                                exit_status: None,
                                task_handle: None,
                                error: Some(audit_error),
                                error_kind: Some(error.kind.clone()),
                                tool_error: Some(error.clone()),
                                reason: None,
                            }),
                        ))?;
                        tool_results.push(ToolResultBlock {
                            tool_use_id: tool_call_id,
                            content: result_content,
                            is_error: true,
                            error: Some(error.clone()),
                        });
                        tool_result_envelopes.push(result.envelope);
                        all_tool_results_should_sleep = false;
                    }
                }
            }
            let _completion_promotions = runtime
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
            let tool_results_transcript = TranscriptEntry::new(
                agent_id.to_string(),
                TranscriptEntryKind::ToolResults,
                Some(round),
                None,
                to_json_value(&ToolResultData::RefsWithWrapper { refs }),
            );
            if let Some(prepared) = prepared_work_item_completion.as_mut() {
                prepared.transcript_entries.push(tool_results_transcript);
            } else {
                runtime.persist_transcript_evidence(&tool_results_transcript)?;
            }
            let after_tool_results_interjections = if pending_completion_report.is_some() {
                Vec::new()
            } else {
                runtime
                    .drain_operator_interjections(
                        agent_id,
                        round,
                        scheduler::InterjectionBoundary::AfterToolResults,
                    )
                    .await?
            };
            let mut interjections = before_tool_execution_interjections;
            interjections.extend(after_tool_results_interjections);
            let has_operator_interjections = !interjections.is_empty();
            let terminal_wait_without_text =
                round_tool_calls.iter().any(|call| call.name == "WaitFor")
                    && !current_round_has_assistant_text;
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

            if (all_tool_results_should_sleep || terminal_tool_transition)
                && (!has_operator_interjections || terminal_tool_transition)
                && !checkpoint_state.operator_delivery_pending()
            {
                let terminal_assistant_message = if terminal_wait_without_text {
                    None
                } else {
                    last_assistant_message.clone()
                };
                let final_text = terminal_assistant_message.clone().unwrap_or_default();
                let terminal = runtime
                    .persist_turn_terminal_record(
                        TurnTerminalKind::Completed,
                        terminal_assistant_message,
                        turn_started_at.elapsed().as_millis() as u64,
                        Some(&checkpoint_state),
                        persist_terminal,
                    )
                    .await?;
                return Ok(AgentLoopOutcome {
                    final_text,
                    final_citations: if terminal_wait_without_text {
                        Vec::new()
                    } else {
                        last_assistant_citations.clone()
                    },
                    final_text_source_assistant_round_id: (!terminal_wait_without_text)
                        .then(|| last_assistant_round_id.clone())
                        .flatten(),
                    turn_index: terminal.turn_index,
                    terminal,
                    should_sleep: true,
                    sleep_duration_ms: sleep_duration_ms.or(legacy_sleep_duration_ms),
                    allow_sleep_runnable_work_override: completed_work_item_this_turn,
                    terminal_kind: TurnTerminalKind::Completed,
                    prepared_work_item_completion: prepared_work_item_completion.take(),
                    terminal_tool_executions: Vec::new(),
                });
            }
        }
    }
}

fn extend_unique_citations(
    target: &mut Vec<Citation>,
    citations: impl IntoIterator<Item = Citation>,
) {
    let mut seen = target
        .iter()
        .map(|citation| citation.url.clone())
        .collect::<HashSet<_>>();
    target.extend(
        citations
            .into_iter()
            .filter(|citation| seen.insert(citation.url.clone())),
    );
}
