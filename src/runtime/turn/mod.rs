use std::{collections::HashSet, time::Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

#[allow(unused_imports)]
use crate::{
    config::ModelRef,
    prompt::EffectivePrompt,
    provider::{
        provider_attempt_timeline, provider_error_is_context_length_exceeded, AgentProvider,
        ConversationMessage, ModelBlock, PromptContentBlock, ProviderAttemptTimeline,
        ProviderPromptFrame, ProviderTurnRequest, ProviderTurnResponse, ToolResultBlock,
    },
    runtime::provider_turn::{
        build_continuation_request, build_provider_prompt_frame, build_provider_turn_request,
    },
    storage::to_json_value,
    tool::{
        spec::{ToolResultEnvelope, ToolResultStatus},
        ToolCall, ToolError, ToolSpec,
    },
    types::{
        AdmissionContext, AuditEvent, AuthorityClass, BriefKind, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        QueueEntryRecord, QueueEntryStatus, TodoItemState, TokenUsage, ToolExecutionAuditEvent,
        TranscriptEntry, TranscriptEntryKind, TurnRecord, TurnTerminalCheckpointRecord,
        TurnTerminalKind, TurnTerminalRecord, TurnTerminalSummary, TurnTriggerSummary,
        WorkItemPlanStatus, WorkItemRecord,
    },
};

use super::{
    combine_text_history, is_max_output_stop_reason, message_dispatch::message_text, scheduler,
    CurrentRunAborted, RuntimeHandle, WorkItemCompletionReportPromotion,
    WorkItemCompletionReportPromotionOutcome,
};

mod checkpoint;
mod completion;
mod context_management;
mod execution;
mod projection;
mod reminders;
mod tool_summary;

#[allow(unused_imports)]
use context_management::{
    context_management_diagnostic, estimate_context_management_eligible_tool_results,
};
#[allow(unused_imports)]
use reminders::{
    build_work_item_stale_reminder, maybe_reset_work_item_stale_reminder_cooldown,
    round_invalidates_checkpoint_anchor, round_updated_work_item, runtime_reminder_fits_baseline,
    tool_result_invalidates_checkpoint_anchor, work_item_plan_status_label,
    work_item_stale_reminder_cooldown_rounds, work_item_stale_reminder_rounds,
};
#[allow(unused_imports)]
use tool_summary::{build_compacted_round_recap, build_round_recap_line};

#[allow(unused_imports)]
use checkpoint::*;
#[allow(unused_imports)]
use completion::*;
use execution::*;
pub(crate) use projection::estimate_tool_specs_tokens;
#[allow(unused_imports)]
use projection::*;

pub(super) struct AgentLoopOutcome {
    pub(super) final_text: String,
    pub(super) final_text_source_assistant_round_id: Option<String>,
    pub(super) turn_index: u64,
    pub(super) terminal: TurnTerminalRecord,
    pub(super) should_sleep: bool,
    pub(super) sleep_duration_ms: Option<u64>,
    pub(super) allow_sleep_runnable_work_override: bool,
    pub(super) terminal_kind: TurnTerminalKind,
    pub(super) terminal_delivery: TurnTerminalDelivery,
}

#[derive(Debug, Clone)]
pub(super) struct TurnTerminalDelivery {
    /// Completion reports already promoted during this turn.
    pub(super) promoted_completion_reports: Vec<TurnPromotedCompletionReport>,
    /// Whether the terminal result brief should be suppressed because it would
    /// only duplicate a promoted completion report.
    pub(super) suppress_normal_brief: bool,
}

#[derive(Debug, Clone)]
pub(super) struct TurnPromotedCompletionReport {
    pub(super) work_item_id: String,
    pub(super) brief_id: String,
}

impl TurnTerminalDelivery {
    fn normal() -> Self {
        Self {
            promoted_completion_reports: Vec::new(),
            suppress_normal_brief: false,
        }
    }

    fn completion_reports(
        promoted_completion_reports: Vec<TurnPromotedCompletionReport>,
        suppress_normal_brief: bool,
    ) -> Self {
        Self {
            promoted_completion_reports,
            suppress_normal_brief,
        }
    }
}

pub(super) struct LoopControlOptions {
    pub(super) max_tool_rounds: Option<usize>,
}

pub(super) const MAX_OUTPUT_RECOVERY_ATTEMPTS: usize = 2;
pub(super) const ROUND_TEXT_PREVIEW_LIMIT: usize = 600;
pub(super) const RECAP_TEXT_PREVIEW_LIMIT: usize = 160;
pub(super) const MIN_EXACT_TAIL_ROUNDS: usize = 2;
pub(super) const CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS: usize = 256;
pub(super) const DEGRADED_ROUND_PROVENANCE_MARKER: &str = "[runtime: last turn content trimmed to fit prompt budget — truncated sections are marked inline]";
pub(super) const DEGRADED_ROUND_MINIMUM_CONTENT_CHARS: usize = 200;
pub(super) const WORK_ITEM_STALE_REMINDER_ROUNDS: usize = 10;
pub(super) const WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS: usize = 10;
pub(super) const WORK_ITEM_STALE_REMINDER_MAX_TOKENS: usize = 512;
pub(super) const WORK_ITEM_STALE_REMINDER_PLAN_LINE_LIMIT: usize = 8;
pub(super) const WORK_ITEM_STALE_REMINDER_PLAN_CHAR_LIMIT: usize = 1_200;
pub(super) const WORK_ITEM_STALE_REMINDER_TODO_LIMIT: usize = 8;
pub(super) const TURN_RECORD_SCAN_LIMIT: usize = 4096;
pub(super) const OPERATOR_INTERJECTION_HEADER: &str =
    "[Operator message received while this turn was in progress]";
pub(super) const COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT: &str = "\
[Runtime-generated full progress checkpoint request]
You are crossing a context compaction boundary. Before continuing, include a concise progress checkpoint for continuation in your next assistant message.

Include:
- current user goal
- current work item objective, plan_status, plan, and todo_list state
- files, commands, or sources already inspected
- key findings and ruled-out paths
- what remains unknown
- the next goal-aligned action

If continuing exploration, name the specific missing information and the next bounded command/query. If enough evidence already exists to act, make the next action the concrete mutation, verification, or delivery step instead of another read.
If the current todo item became complete through material progress, update the work item after that progress is recorded.
This is not a request to finish the task; after the checkpoint, continue with the next goal-aligned action when useful.
Do not assume the task requires code changes unless the user goal does.";

pub(super) const DELTA_CHECKPOINT_PREVIEW_LIMIT: usize = 1_200;
pub(super) const CHECKPOINT_RESUME_PROMPT: &str = "\
[Runtime-generated checkpoint continuation]
Continue from the checkpoint's next goal-aligned action now. Do not restate the checkpoint. If the checkpoint says enough evidence exists to act, call the concrete mutation, verification, or delivery tool next; otherwise run only the named bounded command/query.";

impl RuntimeHandle {
    async fn maybe_handle_context_length_exceeded(
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

    async fn persist_turn_terminal_record(
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

    pub(super) async fn persist_turn_record(&self, terminal: &TurnTerminalRecord) -> Result<()> {
        let (agent_id, run_id, current_work_item_id) = {
            let guard = self.inner.agent.lock().await;
            (
                guard.state.id.clone(),
                guard.state.current_run_id.clone(),
                guard
                    .state
                    .current_turn_work_item_id
                    .clone()
                    .or_else(|| guard.state.current_work_item_id.clone()),
            )
        };
        let turn_id = terminal.turn_id.trim();
        if turn_id.is_empty() {
            return Ok(());
        }

        let messages = self.inner.storage.read_all_messages()?;
        let briefs = self
            .inner
            .storage
            .read_recent_briefs(TURN_RECORD_SCAN_LIMIT)?;
        let tools = self
            .inner
            .storage
            .read_recent_tool_executions(TURN_RECORD_SCAN_LIMIT)?;
        let wait_conditions = self
            .inner
            .storage
            .read_recent_wait_conditions(TURN_RECORD_SCAN_LIMIT)?;

        let input_messages = messages
            .iter()
            .filter(|message| {
                turn_optional_id_matches(message.turn_id.as_deref(), turn_id)
                    || message.message_seq == Some(terminal.turn_index)
            })
            .collect::<Vec<_>>();

        let mut record = TurnRecord::new(agent_id, turn_id, terminal.turn_index);
        record.run_id = run_id;
        record.current_work_item_id = current_work_item_id;
        record.trigger = input_messages
            .first()
            .map(|message| TurnTriggerSummary::from_message(message));
        record.input_message_ids = input_messages
            .iter()
            .map(|message| message.id.clone())
            .collect();
        record.tool_execution_ids = tools
            .iter()
            .filter(|tool| {
                turn_optional_id_matches(tool.turn_id.as_deref(), turn_id)
                    || tool.turn_index == terminal.turn_index
            })
            .map(|tool| tool.id.clone())
            .collect();
        record.produced_brief_ids = briefs
            .iter()
            .filter(|brief| {
                turn_optional_id_matches(brief.turn_id.as_deref(), turn_id)
                    || brief.turn_index == Some(terminal.turn_index)
            })
            .map(|brief| brief.id.clone())
            .collect();
        record.completed_work_item_ids = briefs
            .iter()
            .filter(|brief| {
                brief.kind == BriefKind::Result
                    && (turn_optional_id_matches(brief.turn_id.as_deref(), turn_id)
                        || brief.turn_index == Some(terminal.turn_index))
            })
            .filter_map(|brief| brief.work_item_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        record.waiting_condition_ids = wait_conditions
            .iter()
            .filter(|condition| turn_optional_id_matches(condition.turn_id.as_deref(), turn_id))
            .map(|condition| condition.id.clone())
            .collect();
        record.terminal = Some(TurnTerminalSummary::from_terminal(terminal));

        self.inner.storage.append_turn(&record)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_record",
            serde_json::json!({
                "turn_id": record.turn_id,
                "turn_index": record.turn_index,
                "agent_id": record.agent_id,
                "run_id": record.run_id,
                "current_work_item_id": record.current_work_item_id,
                "tool_execution_ids": record.tool_execution_ids,
                "produced_brief_ids": record.produced_brief_ids,
                "completed_work_item_ids": record.completed_work_item_ids,
                "waiting_condition_ids": record.waiting_condition_ids,
                "terminal": record.terminal,
                "created_at": record.created_at,
            }),
        ))?;
        Ok(())
    }

    async fn maybe_defer_provider_lineage_failure(
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

    pub(super) async fn persist_turn_aborted_record(
        &self,
        run_id: &str,
        reason: &str,
        last_assistant_message: Option<String>,
        duration_ms: u64,
    ) -> Result<TurnTerminalRecord> {
        let record = {
            let mut guard = self.inner.agent.lock().await;
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
                kind: TurnTerminalKind::Aborted,
                reason: Some(reason.to_string()),
                last_assistant_message,
                checkpoint: None,
                completed_at: chrono::Utc::now(),
                duration_ms,
            };
            guard.state.last_turn_terminal = Some(record.clone());
            guard.persist_state(&self.inner.storage)?;
            record
        };
        self.persist_turn_record(&record).await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal",
            serde_json::to_value(&record)?,
        ))?;
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal_aborted",
            serde_json::json!({
                "run_id": run_id,
                "reason": reason,
                "turn_id": record.turn_id.clone(),
                "turn_index": record.turn_index,
                "kind": record.kind,
                "completed_at": record.completed_at,
                "duration_ms": record.duration_ms,
            }),
        ))?;
        Ok(record)
    }

    async fn complete_turn_with_abort(
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

    async fn complete_turn_with_timing(
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

    async fn ensure_not_aborted(&self) -> Result<()> {
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

    async fn drain_operator_interjections(
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

    async fn append_operator_interjections_to_last_round(
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

    pub(super) async fn run_agent_loop(
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

fn provider_lineage_failure_text(text: &str) -> String {
    if text.trim().is_empty() {
        "provider failed".into()
    } else {
        truncate_preview(text, ROUND_TEXT_PREVIEW_LIMIT)
    }
}

fn provider_lineage_operator_message(
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
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_plan_artifact(
        work_item: &WorkItemRecord,
        preview: impl Into<String>,
    ) -> crate::types::WorkItemPlanArtifact {
        let preview = preview.into();
        crate::types::WorkItemPlanArtifact {
            owner_agent_id: work_item.agent_id.clone(),
            workspace_id: crate::types::agent_home_workspace_id(&work_item.agent_id),
            workspace_alias: Some(crate::types::AGENT_HOME_WORKSPACE_ID.into()),
            relative_path: crate::work_item_plan::plan_relative_path(&work_item.id),
            path: std::path::PathBuf::from(format!("/tmp/{}/plan.md", work_item.id)),
            hash: "sha256:test".into(),
            bytes: preview.len() as u64,
            updated_at: chrono::Utc::now(),
            preview,
            preview_complete: true,
        }
    }

    #[test]
    fn truncated_mutation_recovery_hint_is_tool_specific() {
        let apply_patch = truncated_mutation_recovery_hint("ApplyPatch");
        assert!(apply_patch.contains("previous ApplyPatch mutation"));
        assert!(apply_patch.contains("huge patch"));
        assert!(apply_patch.contains("bounded ExecCommand/scripted rewrite"));

        let work_item = truncated_mutation_recovery_hint("CreateWorkItem");
        assert!(work_item.contains("oversized tool call"));
        assert!(work_item.contains("complete smaller tool call"));
        assert!(!work_item.contains("huge patch"));
        assert!(!work_item.contains("ExecCommand/scripted rewrite"));
    }

    #[test]
    fn turn_local_round_recap_preserves_command_recovery_identity() {
        let command = "python - <<'PY'\nprint('turn_recap_hidden_1246')\nPY";
        let round = fixture_round_with_tool(
            3,
            "running a diagnostic command",
            "ExecCommand",
            serde_json::json!({"cmd": command}),
        );

        let recap = build_round_recap_line(&round);

        assert!(recap.contains("recoverable_command_inputs=[ExecCommand"));
        assert!(recap.contains("tool_call_id=call_3"));
        assert!(recap.contains("cmd_digest="));
        assert!(!recap.contains("cmd_preview="));
        assert!(!recap.contains("turn_recap_hidden_1246"));
    }

    #[test]
    fn turn_local_round_recap_preserves_batch_command_recovery_identity() {
        let round = fixture_round_with_tool(
            4,
            "running command batch",
            "ExecCommandBatch",
            serde_json::json!({
                "items": [
                    {"cmd": "rg -n \"foo\" src"},
                    {"cmd": "node - <<'NODE'\nconsole.log('turn_batch_hidden_1246')\nNODE"}
                ]
            }),
        );

        let recap = build_round_recap_line(&round);

        assert!(recap.contains("recoverable_command_inputs=[ExecCommandBatch"));
        assert!(recap.contains("tool_call_id=call_4"));
        assert!(recap.contains("item=1 cmd_digest="));
        assert!(recap.contains("item=2 cmd_digest="));
        assert!(!recap.contains("cmd_preview="));
        assert!(!recap.contains("turn_batch_hidden_1246"));
    }

    #[test]
    fn compacted_round_recap_preserves_view_image_observation_evidence() {
        let mut round = fixture_round_with_tool(
            5,
            "inspect screenshot",
            "ViewImage",
            serde_json::json!({
                "path": "screenshot.png",
                "prompt": "Read the warning banner."
            }),
        );
        round.tool_result_envelopes = vec![ToolResultEnvelope {
            tool_name: "ViewImage".to_string(),
            status: ToolResultStatus::Success,
            summary_text: Some("generated observation".to_string()),
            result: Some(serde_json::json!({
                "visual_reference": {
                    "id": "vis_warning",
                    "mime": "image/png"
                },
                "observation": {
                    "schema": "visual_observation.v1",
                    "prompt": "Read the warning banner.",
                    "summary": "A red warning banner is visible.",
                    "ocr": [
                        {"text": "DEPLOY BLOCKED"},
                        {"text": "Fix failing checks"}
                    ]
                }
            })),
            error: None,
        }];

        let recap = build_compacted_round_recap(&[round], 1_000);

        assert!(recap.contains("ViewImage visual_observation"));
        assert!(recap.contains("schema=visual_observation.v1"));
        assert!(recap.contains("ref=vis_warning"));
        assert!(recap.contains("summary=\"A red warning banner is visible.\""));
        assert!(recap.contains("ocr=[DEPLOY BLOCKED | Fix failing checks]"));
        assert!(!recap.contains("generated observation"));
    }

    fn fixture_round(round: usize, text: &str) -> TurnRoundRecord {
        let assistant_blocks = vec![ModelBlock::Text {
            text: text.to_string(),
        }];
        let text_blocks = vec![text.to_string()];
        let tool_results = Vec::new();
        TurnRoundRecord {
            round,
            estimated_tokens: build_round_estimated_tokens(&assistant_blocks, &tool_results, &[]),
            assistant_blocks,
            text_blocks,
            tool_calls: Vec::new(),
            tool_results,
            tool_result_envelopes: Vec::new(),
            follow_up_user_texts: Vec::new(),
        }
    }

    fn fixture_round_with_follow_up(round: usize, text: &str, follow_up: &str) -> TurnRoundRecord {
        let assistant_blocks = vec![ModelBlock::Text {
            text: text.to_string(),
        }];
        let text_blocks = vec![text.to_string()];
        let follow_up_user_texts = vec![follow_up.to_string()];
        let tool_results = Vec::new();
        TurnRoundRecord {
            round,
            estimated_tokens: build_round_estimated_tokens(
                &assistant_blocks,
                &tool_results,
                &follow_up_user_texts,
            ),
            assistant_blocks,
            text_blocks,
            tool_calls: Vec::new(),
            tool_results,
            tool_result_envelopes: Vec::new(),
            follow_up_user_texts,
        }
    }

    fn fixture_round_with_tool(
        round: usize,
        text: &str,
        tool_name: &str,
        input: Value,
    ) -> TurnRoundRecord {
        let call = ToolCall {
            id: format!("call_{round}"),
            name: tool_name.to_string(),
            input: input.clone(),
        };
        let assistant_blocks = vec![
            ModelBlock::Text {
                text: text.to_string(),
            },
            ModelBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input,
            },
        ];
        let text_blocks = vec![text.to_string()];
        let tool_results = Vec::new();
        TurnRoundRecord {
            round,
            estimated_tokens: build_round_estimated_tokens(&assistant_blocks, &tool_results, &[]),
            assistant_blocks,
            text_blocks,
            tool_calls: vec![call],
            tool_results,
            tool_result_envelopes: Vec::new(),
            follow_up_user_texts: Vec::new(),
        }
    }

    fn fixture_tool_only_round_with_result(round: usize, follow_up: &str) -> TurnRoundRecord {
        let call = ToolCall {
            id: format!("call_{round}"),
            name: "ExecCommand".to_string(),
            input: serde_json::json!({ "cmd": "printf ok" }),
        };
        let assistant_blocks = vec![ModelBlock::ToolUse {
            id: call.id.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
        }];
        let tool_results = vec![ToolResultBlock {
            tool_use_id: call.id.clone(),
            content: "ok".to_string(),
            is_error: false,
            error: None,
        }];
        let follow_up_user_texts = vec![follow_up.to_string()];
        TurnRoundRecord {
            round,
            estimated_tokens: build_round_estimated_tokens(
                &assistant_blocks,
                &tool_results,
                &follow_up_user_texts,
            ),
            assistant_blocks,
            text_blocks: Vec::new(),
            tool_calls: vec![call],
            tool_results,
            tool_result_envelopes: Vec::new(),
            follow_up_user_texts,
        }
    }

    #[test]
    fn exact_round_messages_preserves_tool_only_round_before_interjection_text() {
        let round = fixture_tool_only_round_with_result(
            7,
            "[Operator message received while this turn was in progress]\ncontinue",
        );

        let messages = exact_round_messages(&round);

        assert_eq!(messages.len(), 3);
        match &messages[0] {
            ConversationMessage::AssistantBlocks(blocks) => {
                assert!(matches!(blocks.as_slice(), [ModelBlock::ToolUse { .. }]));
            }
            other => panic!("expected assistant tool-use message, got {other:?}"),
        }
        match &messages[1] {
            ConversationMessage::UserToolResults(results) => {
                assert_eq!(results[0].tool_use_id, "call_7");
            }
            other => panic!("expected tool result message, got {other:?}"),
        }
        match &messages[2] {
            ConversationMessage::UserText(text) => {
                assert!(text.contains("Operator message received"));
            }
            other => panic!("expected follow-up user text, got {other:?}"),
        }
    }

    fn fixture_round_with_tool_result(
        round: usize,
        text: &str,
        tool_name: &str,
        input: Value,
        status: ToolResultStatus,
    ) -> TurnRoundRecord {
        let mut record = fixture_round_with_tool(round, text, tool_name, input);
        record.tool_result_envelopes = vec![ToolResultEnvelope {
            tool_name: tool_name.to_string(),
            status,
            summary_text: None,
            result: None,
            error: None,
        }];
        record
    }

    #[test]
    fn build_work_item_stale_reminder_includes_current_work_item_snapshot() {
        let mut work_item = WorkItemRecord::new(
            "default",
            "Ship work item reminder tests",
            crate::types::WorkItemState::Open,
        );
        work_item.id = "work_reminder".into();
        work_item.plan_status = WorkItemPlanStatus::Ready;
        work_item.plan_artifact = Some(fixture_plan_artifact(
            &work_item,
            "Patch runtime reminder.\nRun focused tests.",
        ));
        work_item.todo_list = vec![
            crate::types::TodoItem {
                text: "Patch runtime reminder".into(),
                state: TodoItemState::InProgress,
            },
            crate::types::TodoItem {
                text: "Run focused tests".into(),
                state: TodoItemState::Pending,
            },
        ];

        let reminder = build_work_item_stale_reminder(&work_item, 10);

        assert!(reminder.contains("[Runtime-generated work item progress reminder]"));
        assert!(reminder.contains("- Id: work_reminder"));
        assert!(reminder.contains("- Objective: Ship work item reminder tests"));
        assert!(reminder.contains("- Plan status: ready"));
        assert!(reminder.contains("Patch runtime reminder."));
        assert!(reminder.contains("  - [in_progress] Patch runtime reminder"));
        assert!(reminder.contains("  - [pending] Run focused tests"));
    }

    #[test]
    fn build_turn_local_projection_includes_runtime_reminder() {
        let prompt_frame = fixture_prompt_frame();
        let reminder = "[Runtime-generated work item progress reminder]\nCall UpdateWorkItem if material progress emerged.";

        let projection = build_turn_local_projection_with_runtime_reminder(
            &prompt_frame,
            &[],
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            4_000,
            120,
            Some(reminder),
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        assert!(projection.conversation.iter().any(|message| matches!(
            message,
            ConversationMessage::UserText(text) if text == reminder
        )));
        assert!(projection.compaction.is_none());
    }

    #[test]
    fn stale_reminder_cooldown_resets_only_when_reminder_is_injected() {
        let mut rounds_since_work_item_reminder = 12usize;
        maybe_reset_work_item_stale_reminder_cooldown(&mut rounds_since_work_item_reminder, false);
        assert_eq!(
            rounds_since_work_item_reminder, 12,
            "skipped reminder must not consume cooldown"
        );

        maybe_reset_work_item_stale_reminder_cooldown(&mut rounds_since_work_item_reminder, true);
        assert_eq!(
            rounds_since_work_item_reminder, 0,
            "only injected reminder should reset cooldown"
        );
    }

    #[test]
    fn large_work_item_stale_reminder_does_not_force_baseline_over_budget() {
        let mut work_item = WorkItemRecord::new(
            "default",
            "Keep reminder bounded with large plan and todo list",
            crate::types::WorkItemState::Open,
        );
        work_item.id = "work_large_reminder".into();
        work_item.plan_status = WorkItemPlanStatus::Ready;
        work_item.plan_artifact = Some(fixture_plan_artifact(
            &work_item,
            (0..80)
                .map(|idx| format!("step {idx}: {}", "inspect and verify ".repeat(40)))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        work_item.todo_list = (0..80)
            .map(|idx| crate::types::TodoItem {
                text: format!("todo {idx}: {}", "finish bounded work ".repeat(30)),
                state: if idx % 3 == 0 {
                    TodoItemState::Completed
                } else if idx % 3 == 1 {
                    TodoItemState::InProgress
                } else {
                    TodoItemState::Pending
                },
            })
            .collect();
        let reminder = build_work_item_stale_reminder(&work_item, 10);
        let prompt_frame = fixture_prompt_frame();

        assert!(reminder.contains("... plan truncated"));
        assert!(!reminder.contains("[completed]"));
        assert!(runtime_reminder_fits_baseline(
            &prompt_frame,
            &[],
            4_000,
            &reminder
        ));
        let projection = build_turn_local_projection_with_runtime_reminder(
            &prompt_frame,
            &[],
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-large".into()),
            4_000,
            120,
            Some(&reminder),
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected bounded reminder to fit baseline");
        };
        assert!(projection.conversation.iter().any(|message| matches!(
            message,
            ConversationMessage::UserText(text) if text == &reminder
        )));
    }

    fn checkpoint_state_with_latest(
        text: &str,
        response_round: usize,
        anchor_generation: u64,
    ) -> TurnLocalCheckpointState {
        TurnLocalCheckpointState {
            latest: Some(TurnLocalCheckpointRecord {
                request_id: format!("req-{response_round}"),
                requested_at_round: response_round,
                response_round: Some(response_round),
                source_turn_index: None,
                mode: TurnLocalCheckpointMode::Full,
                text: text.to_string(),
                anchor_generation,
            }),
            pending: None,
            anchor_generation,
        }
    }

    fn fixture_prompt_frame() -> ProviderPromptFrame {
        ProviderPromptFrame::structured(
            "system",
            vec![PromptContentBlock {
                text: "system".to_string(),
                stability: crate::prompt::PromptStability::Stable,
                cache_breakpoint: false,
            }],
            vec![PromptContentBlock {
                text: "context".to_string(),
                stability: crate::prompt::PromptStability::AgentScoped,
                cache_breakpoint: true,
            }],
            None,
        )
    }

    fn fixture_tool_spec(name: &str, payload_size: usize) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: format!("fixture tool {name}"),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "payload": "x".repeat(payload_size),
                },
            }),
            freeform_grammar: None,
        }
    }

    #[test]
    fn select_exact_tail_start_with_empty_rounds_returns_zero() {
        let rounds = Vec::new();
        assert_eq!(select_exact_tail_start(&rounds, 120), 0);
    }

    #[test]
    fn select_exact_tail_start_with_zero_keep_recent_budget_keeps_minimum_tail() {
        let rounds = vec![
            fixture_round(1, "recent alpha"),
            fixture_round(2, "recent beta"),
            fixture_round(3, "recent gamma"),
            fixture_round(4, "recent delta"),
        ];

        assert_eq!(
            select_exact_tail_start(&rounds, 0),
            rounds.len() - MIN_EXACT_TAIL_ROUNDS
        );
    }

    #[test]
    fn select_exact_tail_start_keeps_boundary_when_budget_equals_newest_round() {
        let rounds = vec![
            fixture_round(1, &"older ".repeat(10)),
            fixture_round(2, &"older ".repeat(11)),
            fixture_round(3, &"newest ".repeat(12)),
        ];
        let keep_recent_budget = estimate_round_tokens(rounds.last().unwrap());

        assert_eq!(
            select_exact_tail_start(&rounds, keep_recent_budget),
            rounds.len() - MIN_EXACT_TAIL_ROUNDS
        );
    }

    #[test]
    fn select_exact_tail_start_one_oversized_newest_round_stays_minimum_tail() {
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(20)),
            fixture_round(2, &"beta ".repeat(20)),
            fixture_round(3, &"oversized ".repeat(600)),
        ];
        let keep_recent_budget = estimate_round_tokens(rounds.last().unwrap()).saturating_sub(1);

        assert_eq!(
            select_exact_tail_start(&rounds, keep_recent_budget),
            rounds.len() - MIN_EXACT_TAIL_ROUNDS
        );
    }

    #[test]
    fn select_exact_tail_start_huge_old_round_excluded_before_recent_tail() {
        let rounds = vec![
            fixture_round(1, &"huge oldest ".repeat(800)),
            fixture_round(2, &"recent one ".repeat(6)),
            fixture_round(3, &"recent two ".repeat(6)),
            fixture_round(4, &"recent three ".repeat(6)),
        ];
        let keep_recent_budget = estimate_round_tokens(&rounds[1])
            + estimate_round_tokens(&rounds[2])
            + estimate_round_tokens(&rounds[3]);

        assert_eq!(select_exact_tail_start(&rounds, keep_recent_budget), 1);
    }

    #[test]
    fn select_exact_tail_start_respects_single_token_boundary_step() {
        let rounds = vec![
            fixture_round(1, &"older ".repeat(4)),
            fixture_round(2, &"older ".repeat(8)),
            fixture_round(3, &"boundary ".repeat(16)),
            fixture_round(4, &"boundary ".repeat(16)),
            fixture_round(5, &"boundary ".repeat(16)),
        ];
        let boundary_keep_recent_budget = estimate_round_tokens(&rounds[2])
            + estimate_round_tokens(&rounds[3])
            + estimate_round_tokens(&rounds[4]);

        assert_eq!(
            select_exact_tail_start(&rounds, boundary_keep_recent_budget.saturating_sub(1)),
            3
        );
        assert_eq!(
            select_exact_tail_start(&rounds, boundary_keep_recent_budget),
            2
        );
    }

    #[test]
    fn normalize_provider_attempt_timing_backfills_missing_attempt_timing() {
        fn attempt(attempt: usize) -> crate::provider::ProviderAttemptRecord {
            crate::provider::ProviderAttemptRecord {
                provider: "test".into(),
                model_ref: "test/model".into(),
                attempt,
                max_attempts: 2,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                failure_kind: None,
                disposition: None,
                outcome: crate::provider::ProviderAttemptOutcome::Succeeded,
                advanced_to_fallback: false,
                backoff_ms: None,
                token_usage: None,
                transport_diagnostics: None,
            }
        }

        let started_at = Utc::now();
        let completed_at = started_at + chrono::Duration::milliseconds(42);
        let single = ProviderAttemptTimeline {
            attempts: vec![attempt(1)],
            requested_model_ref: "test/model".into(),
            active_model_ref: None,
            winning_model_ref: None,
            pending_fallback_model_ref: None,
            aggregated_token_usage: None,
        };
        let single = normalize_provider_attempt_timing(single.into(), started_at, completed_at, 42)
            .expect("single-attempt timeline");

        assert_eq!(single.attempts[0].started_at, Some(started_at));
        assert_eq!(single.attempts[0].completed_at, Some(completed_at));
        assert_eq!(single.attempts[0].duration_ms, Some(42));

        let multiple = ProviderAttemptTimeline {
            attempts: vec![attempt(1), attempt(2)],
            requested_model_ref: "test/model".into(),
            active_model_ref: None,
            winning_model_ref: None,
            pending_fallback_model_ref: None,
            aggregated_token_usage: None,
        };
        let multiple =
            normalize_provider_attempt_timing(multiple.into(), started_at, completed_at, 42)
                .expect("multi-attempt timeline");

        for attempt in &multiple.attempts {
            assert_eq!(attempt.started_at, None);
            assert_eq!(attempt.completed_at, None);
            assert_eq!(attempt.duration_ms, None);
        }
    }

    #[test]
    fn build_turn_local_projection_exact_projection_meets_effective_budget() {
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(120)),
            fixture_round(2, &"beta ".repeat(160)),
        ];
        let prompt_frame = fixture_prompt_frame();
        let mut exact_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        for round in &rounds {
            exact_conversation.extend(exact_round_messages(round));
        }
        let exact_estimated_tokens = estimate_projection_tokens(&prompt_frame, &exact_conversation);

        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            exact_estimated_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS,
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        assert!(projection.compaction.is_none());
        assert!(
            !projection.conversation.iter().any(|message| match message {
                ConversationMessage::UserText(text) => {
                    text.contains("progress checkpoint request")
                }
                _ => false,
            })
        );
    }

    #[test]
    fn build_turn_local_projection_minimum_projection_can_hit_exact_budget() {
        let rounds = vec![
            fixture_round_with_follow_up(1, &"alpha ".repeat(240), "continue"),
            fixture_round_with_follow_up(2, &"gamma ".repeat(120), "continue"),
            fixture_round_with_follow_up(3, &"exact ".repeat(80), "continue"),
        ];
        let prompt_frame = fixture_prompt_frame();
        let mut minimum_viable_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        minimum_viable_conversation.extend(exact_round_messages(&rounds[2]));
        let minimum_projection_estimated_tokens =
            estimate_projection_tokens(&prompt_frame, &minimum_viable_conversation);
        let mut exact_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        for round in &rounds {
            exact_conversation.extend(exact_round_messages(round));
        }
        let exact_projection_estimated_tokens =
            estimate_projection_tokens(&prompt_frame, &exact_conversation);
        assert!(
            exact_projection_estimated_tokens > minimum_projection_estimated_tokens + 5,
            "exact projection should exceed minimum test budget"
        );

        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            minimum_projection_estimated_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS,
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        let compaction = projection
            .compaction
            .as_ref()
            .expect("expected compaction stats");
        assert_eq!(compaction.exact_tail_rounds, 1);
        assert_eq!(
            compaction.compacted_rounds + compaction.exact_tail_rounds,
            rounds.len()
        );
        assert_eq!(
            compaction.projected_estimated_tokens,
            minimum_projection_estimated_tokens
        );
        assert!(compaction.strict_fallback_applied);
    }

    #[test]
    fn build_turn_local_projection_zero_recap_budget_keeps_exact_tail() {
        let rounds = vec![
            fixture_round(1, &"huge ".repeat(400)),
            fixture_round_with_follow_up(2, &"compact ".repeat(4), "continue"),
            fixture_round_with_follow_up(3, &"exact ".repeat(4), "continue"),
        ];
        let prompt_frame = fixture_prompt_frame();
        let mut exact_tail_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        exact_tail_conversation.extend(exact_round_messages(&rounds[1]));
        exact_tail_conversation.extend(exact_round_messages(&rounds[2]));
        let exact_tail_projection_tokens =
            estimate_projection_tokens(&prompt_frame, &exact_tail_conversation);

        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            exact_tail_projection_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS,
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        let compaction = projection
            .compaction
            .as_ref()
            .expect("expected compaction stats");
        assert_eq!(compaction.exact_tail_rounds, 2);
        assert_eq!(
            compaction.compacted_rounds + compaction.exact_tail_rounds,
            rounds.len()
        );
        assert!(
            projection.conversation.iter().all(|message| match message {
                ConversationMessage::UserText(text) => !text.contains("Turn-local recap for older"),
                _ => true,
            }),
            "projection should not include an empty recap when recap budget is zero"
        );
    }

    #[test]
    fn build_turn_local_projection_minimum_projection_falls_off_by_one_on_tight_budget() {
        let rounds = vec![
            fixture_round_with_follow_up(1, &"alpha ".repeat(240), "continue"),
            fixture_round_with_follow_up(2, &"beta ".repeat(200), "continue"),
            fixture_round_with_follow_up(3, &"gamma ".repeat(120), "continue"),
        ];
        let prompt_frame = fixture_prompt_frame();
        let mut minimum_viable_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        minimum_viable_conversation.extend(exact_round_messages(&rounds[2]));
        let minimum_projection_estimated_tokens =
            estimate_projection_tokens(&prompt_frame, &minimum_viable_conversation);
        let optimal_prompt_budget =
            minimum_projection_estimated_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS;

        let tight_projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            optimal_prompt_budget - 1,
            120,
        );

        match tight_projection {
            TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                assert_eq!(
                    diagnostics.reason, "minimum_exact_round_unfit",
                    "tight budget should fail minimum projection"
                );
            }
            TurnLocalProjectionOutcome::Projection(_) => {
                panic!("expected baseline over budget at one-token tighter budget");
            }
        }
    }

    #[test]
    fn build_turn_local_projection_tool_overhead_can_flip_baseline_fit_boundary() {
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(120)),
            fixture_round(2, &"beta ".repeat(80)),
        ];
        let prompt_frame = fixture_prompt_frame();
        let mut exact_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        for round in &rounds {
            exact_conversation.extend(exact_round_messages(round));
        }
        let exact_projection_estimated_tokens =
            estimate_projection_tokens(&prompt_frame, &exact_conversation);
        let prompt_budget =
            exact_projection_estimated_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS;

        let projection_without_tools = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            prompt_budget,
            120,
        );

        assert!(matches!(
            projection_without_tools,
            TurnLocalProjectionOutcome::Projection(_)
        ));

        let heavy_tools = vec![
            fixture_tool_spec("tool-a", 2_000),
            fixture_tool_spec("tool-b", 2_000),
            fixture_tool_spec("tool-c", 2_000),
        ];
        let projection_with_tools = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &heavy_tools,
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            prompt_budget,
            120,
        );

        assert!(matches!(
            projection_with_tools,
            TurnLocalProjectionOutcome::BaselineOverBudget(_)
        ));
    }

    #[test]
    fn build_turn_local_projection_tool_overhead_preserves_exact_tail() {
        let rounds = vec![
            fixture_round(1, &"huge ".repeat(360)),
            fixture_round_with_follow_up(2, &"compact ".repeat(12), "continue"),
            fixture_round_with_follow_up(3, &"exact ".repeat(6), "continue"),
        ];
        let prompt_frame = fixture_prompt_frame();
        let heavy_tools = vec![
            fixture_tool_spec("tool-heavy", 1_200),
            fixture_tool_spec("tool-heavy-2", 1_200),
        ];
        let tool_overhead_estimated_tokens = estimate_tool_specs_tokens(&heavy_tools);
        let mut minimum_viable_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        minimum_viable_conversation.extend(exact_round_messages(&rounds[2]));
        let minimum_projection_estimated_tokens =
            estimate_projection_tokens(&prompt_frame, &minimum_viable_conversation);
        let prompt_budget = minimum_projection_estimated_tokens
            + tool_overhead_estimated_tokens
            + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS;

        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &heavy_tools,
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            prompt_budget,
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        let compaction = projection
            .compaction
            .as_ref()
            .expect("expected compaction stats");
        assert!(compaction.exact_tail_rounds > 0);
        assert_eq!(
            compaction.compacted_rounds + compaction.exact_tail_rounds,
            rounds.len()
        );
    }

    #[test]
    fn build_turn_local_projection_checkpoint_overhead_drives_fallback_boundary() {
        let rounds = vec![
            fixture_round(1, &"huge ".repeat(2_000)),
            fixture_round(2, &"compact ".repeat(20)),
            fixture_round(3, &"compact ".repeat(20)),
        ];
        let prompt_frame = fixture_prompt_frame();
        let checkpoint_prompt_tokens =
            estimate_text_tokens(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT);
        let mut exact_tail_single_round_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        exact_tail_single_round_conversation.extend(exact_round_messages(&rounds[2]));
        let mut exact_tail_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        exact_tail_conversation.extend(exact_round_messages(&rounds[1]));
        exact_tail_conversation.extend(exact_round_messages(&rounds[2]));
        let checkpointed_tail_projection_tokens =
            estimate_projection_tokens(&prompt_frame, &exact_tail_conversation)
                + checkpoint_prompt_tokens;
        let minimum_projection_tokens =
            estimate_projection_tokens(&prompt_frame, &exact_tail_single_round_conversation);
        let minimum_prompt_budget =
            minimum_projection_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS;
        let tight_prompt_budget =
            checkpointed_tail_projection_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS - 1;
        let relaxed_prompt_budget =
            checkpointed_tail_projection_tokens + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS;

        assert!(minimum_prompt_budget < tight_prompt_budget);
        assert!(relaxed_prompt_budget > tight_prompt_budget);

        let minimum_projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-min".into()),
            minimum_prompt_budget,
            120,
        );
        let TurnLocalProjectionOutcome::Projection(minimum_projection) = minimum_projection else {
            panic!("expected minimum baseline projection");
        };
        assert!(minimum_projection.compaction.is_some());
        assert!(
            minimum_projection
                .compaction
                .as_ref()
                .expect("minimum compaction")
                .strict_fallback_applied
        );

        let strict_projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            tight_prompt_budget,
            120,
        );
        let TurnLocalProjectionOutcome::Projection(strict_projection) = strict_projection else {
            panic!("expected projection outcome");
        };
        let strict_compaction = strict_projection
            .compaction
            .as_ref()
            .expect("strict projection compaction");
        assert_eq!(
            strict_projection
                .compaction
                .as_ref()
                .expect("compaction stats")
                .exact_tail_rounds,
            1
        );
        assert!(strict_compaction.strict_fallback_applied);

        let relaxed_projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-2".into()),
            relaxed_prompt_budget,
            120,
        );
        let TurnLocalProjectionOutcome::Projection(relaxed_projection) = relaxed_projection else {
            panic!("expected projection outcome");
        };
        let relaxed_compaction = relaxed_projection
            .compaction
            .as_ref()
            .expect("compaction stats");
        assert!(!relaxed_compaction.strict_fallback_applied);
        assert!(relaxed_compaction.exact_tail_rounds >= strict_compaction.exact_tail_rounds);
    }

    #[test]
    fn build_turn_local_projection_full_checkpoint_preserves_projection_boundaries() {
        let rounds = vec![
            fixture_round(1, &"huge ".repeat(300)),
            fixture_round(2, &"compact ".repeat(30)),
            fixture_round(3, &"compact ".repeat(30)),
        ];
        let prompt_frame = fixture_prompt_frame();
        let mut exact_tail_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        exact_tail_conversation.extend(exact_round_messages(&rounds[1]));
        exact_tail_conversation.extend(exact_round_messages(&rounds[2]));
        let prompt_budget = estimate_projection_tokens(&prompt_frame, &exact_tail_conversation)
            + estimate_text_tokens(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT)
            + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
            - 1;
        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-full".into()),
            prompt_budget,
            120,
        );
        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        let compaction = projection.compaction.expect("expected compaction stats");
        assert_eq!(
            compaction.checkpoint_mode,
            Some(TurnLocalCheckpointMode::Full)
        );
        assert_eq!(
            compaction.compacted_rounds + compaction.exact_tail_rounds,
            rounds.len()
        );
    }

    #[test]
    fn build_turn_local_projection_delta_checkpoint_preserves_projection_boundaries() {
        let rounds = vec![
            fixture_round(1, &"huge ".repeat(300)),
            fixture_round(2, &"compact ".repeat(30)),
            fixture_round(3, &"compact ".repeat(30)),
        ];
        let prompt_frame = fixture_prompt_frame();
        let checkpoint_state = checkpoint_state_with_latest("continuation notes", 1, 0);
        let mut exact_tail_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        exact_tail_conversation.extend(exact_round_messages(&rounds[1]));
        exact_tail_conversation.extend(exact_round_messages(&rounds[2]));
        let prompt_budget = estimate_projection_tokens(&prompt_frame, &exact_tail_conversation)
            + estimate_text_tokens(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT)
            + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
            - 1;
        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &checkpoint_state,
            Some("req-delta".into()),
            prompt_budget,
            120,
        );
        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        let compaction = projection.compaction.expect("expected compaction stats");
        assert_eq!(
            compaction.checkpoint_mode,
            Some(TurnLocalCheckpointMode::Delta)
        );
        assert_eq!(compaction.previous_checkpoint_round, Some(1));
        assert_eq!(
            compaction.compacted_rounds + compaction.exact_tail_rounds,
            rounds.len()
        );
    }

    #[test]
    fn build_turn_local_projection_checkpoint_mode_overhead_boundary() {
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(140)),
            fixture_round(2, &"beta ".repeat(140)),
            fixture_round(3, &"gamma ".repeat(140)),
            fixture_round(4, &"delta ".repeat(140)),
        ];
        let prompt_frame = fixture_prompt_frame();
        let full_state = TurnLocalCheckpointState::default();
        let delta_state = checkpoint_state_with_latest("checkpoint baseline", 2, 0);

        let baseline_budget = 1_200;
        let full_projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &full_state,
            Some("req-full".into()),
            baseline_budget,
            120,
        );
        let delta_projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &delta_state,
            Some("req-delta".into()),
            baseline_budget,
            120,
        );

        match full_projection {
            TurnLocalProjectionOutcome::Projection(full) => {
                if let Some(full_compaction) = full.compaction.as_ref() {
                    assert_eq!(
                        full_compaction.compacted_rounds + full_compaction.exact_tail_rounds,
                        rounds.len()
                    );
                    assert!(!full_compaction.strict_fallback_applied);
                    assert_eq!(
                        full_compaction.checkpoint_mode,
                        Some(TurnLocalCheckpointMode::Full)
                    );
                    assert_eq!(
                        full_compaction.checkpoint_request_id.as_deref(),
                        Some("req-full")
                    );
                }
            }
            TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                assert!(
                    diagnostics.minimum_projection_estimated_tokens
                        > diagnostics.effective_budget_estimated_tokens,
                    "expected full checkpoint mode to fail because minimum projection does not fit"
                );
            }
        }

        let TurnLocalProjectionOutcome::Projection(delta) = delta_projection else {
            panic!("expected projection under existing delta checkpoint state");
        };
        if let Some(delta_compaction) = delta.compaction.as_ref() {
            assert_eq!(
                delta_compaction.compacted_rounds + delta_compaction.exact_tail_rounds,
                rounds.len()
            );
            assert!(!delta_compaction.strict_fallback_applied);
            assert_eq!(
                delta_compaction.checkpoint_mode,
                Some(TurnLocalCheckpointMode::Delta)
            );
            assert_eq!(
                delta_compaction.checkpoint_request_id.as_deref(),
                Some("req-delta")
            );
        }
    }

    #[test]
    fn context_management_eligibility_keeps_recent_and_excludes_risky_results() {
        let tool_names = [
            "ExecCommand",
            "ApplyPatch",
            "ExecCommand",
            "SearchText",
            "ReadFile",
            "ExecCommand",
        ];
        let conversation = vec![
            ConversationMessage::AssistantBlocks(
                tool_names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| ModelBlock::ToolUse {
                        id: format!("call_{index}"),
                        name: (*name).to_string(),
                        input: serde_json::json!({}),
                    })
                    .collect(),
            ),
            ConversationMessage::UserToolResults(
                tool_names
                    .iter()
                    .enumerate()
                    .map(|(index, _)| ToolResultBlock {
                        tool_use_id: format!("call_{index}"),
                        content: format!("result-{index}"),
                        is_error: index == 2,
                        error: None,
                    })
                    .collect(),
            ),
        ];

        let stats = estimate_context_management_eligible_tool_results(&conversation, 3);

        assert_eq!(stats.eligible_tool_result_count, 1);
        assert_eq!(stats.eligible_tool_result_bytes, "result-0".len());
        assert_eq!(stats.excluded_tool_result_count, 2);
        assert_eq!(stats.retained_recent_tool_result_count, 3);
    }

    #[test]
    fn build_compacted_round_recap_stops_at_first_round_that_does_not_fit() {
        let rounds = vec![
            fixture_round(1, "short"),
            fixture_round(2, &"very long ".repeat(80)),
            fixture_round(3, "short again"),
        ];
        let round1_line = build_round_recap_line(&rounds[0]);
        let omission_line = "- Older compacted rounds omitted from this recap due to budget: 2";
        let budget = estimate_text_tokens(
            &format!(
                "Turn-local recap for older completed rounds (runtime-generated deterministic summary):\n{round1_line}\n{omission_line}"
            ),
        ) + 4;

        let recap = build_compacted_round_recap(&rounds, budget);

        assert!(recap.contains("Round 1"), "unexpected recap: {recap}");
        assert!(!recap.contains("Round 2"), "unexpected recap: {recap}");
        assert!(!recap.contains("Round 3"), "unexpected recap: {recap}");
        assert!(
            recap.contains("omitted from this recap due to budget: 2"),
            "unexpected recap: {recap}"
        );
    }

    #[test]
    fn build_turn_local_projection_reports_baseline_over_budget_when_min_tail_still_exceeds_budget()
    {
        let rounds = vec![fixture_round(1, &"alpha ".repeat(300))];
        let prompt_frame = fixture_prompt_frame();
        let exact_projection = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )]
        .into_iter()
        .chain(exact_round_messages(&rounds[0]))
        .collect::<Vec<_>>();
        let exact_estimated_tokens = estimate_projection_tokens(&prompt_frame, &exact_projection);

        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            320,
            120,
        );

        match projection {
            TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                assert!(
                    diagnostics.reason == "minimum_exact_round_unfit"
                        || diagnostics.reason == "baseline_unfit"
                );
                assert!(
                    diagnostics.minimum_projection_estimated_tokens > exact_estimated_tokens / 2
                );
                assert!(
                    diagnostics.minimum_projection_estimated_tokens
                        > diagnostics.effective_budget_estimated_tokens
                );
            }
            TurnLocalProjectionOutcome::Projection(_) => {
                panic!("expected baseline-over-budget outcome");
            }
        }
    }

    fn fixture_round_with_large_tool_result(
        round: usize,
        assistant_text: &str,
        tool_result_content: &str,
    ) -> TurnRoundRecord {
        let call = ToolCall {
            id: format!("call_{round}"),
            name: "ExecCommand".to_string(),
            input: serde_json::json!({ "cmd": "echo hello" }),
        };
        let assistant_blocks = vec![
            ModelBlock::Text {
                text: assistant_text.to_string(),
            },
            ModelBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            },
        ];
        let tool_results = vec![ToolResultBlock {
            tool_use_id: call.id.clone(),
            content: tool_result_content.to_string(),
            is_error: false,
            error: None,
        }];
        let follow_up_user_texts = vec!["continue".to_string()];
        TurnRoundRecord {
            round,
            estimated_tokens: build_round_estimated_tokens(
                &assistant_blocks,
                &tool_results,
                &follow_up_user_texts,
            ),
            assistant_blocks,
            text_blocks: vec![assistant_text.to_string()],
            tool_calls: vec![call],
            tool_results,
            tool_result_envelopes: Vec::new(),
            follow_up_user_texts,
        }
    }

    #[test]
    fn build_turn_local_projection_degraded_last_round_fits_when_exact_does_not() {
        // Baseline fits, last exact round is oversized, but a trimmed version should fit.
        let rounds = vec![
            fixture_round(1, "short"),
            fixture_round_with_large_tool_result(
                2,
                &"assistant text ".repeat(200),
                &"tool output content ".repeat(400),
            ),
        ];
        let prompt_frame = fixture_prompt_frame();

        // Compute the baseline tokens the same way the production code does,
        // so we can set prompt_budget to produce an exact degraded_available.
        let estimated_baseline = estimate_prompt_frame_tokens(&prompt_frame)
            + estimate_prompt_blocks_tokens(&prompt_frame.context_blocks);

        // Choose a degraded budget and set prompt_budget so the code computes
        // exactly the same degraded_available:
        //   effective_budget = prompt_budget - CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
        //   degraded_available = effective_budget - estimated_baseline
        //   => prompt_budget = degraded_budget + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS + estimated_baseline
        let degraded_budget = 2000;
        let prompt_budget =
            degraded_budget + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS + estimated_baseline;

        let (_degraded_messages, trimmed) = degraded_round_messages(&rounds[1], degraded_budget);
        assert!(trimmed, "expected content to be trimmed");

        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            prompt_budget,
            120,
        );

        match projection {
            TurnLocalProjectionOutcome::Projection(proj) => {
                let compaction = proj.compaction.expect("expected compaction stats");
                assert!(
                    compaction.last_round_degraded,
                    "expected last_round_degraded to be true"
                );
                assert_eq!(compaction.exact_tail_rounds, 1);

                // Verify provenance markers are present.
                let has_provenance = proj.conversation.iter().any(|msg| match msg {
                    ConversationMessage::UserText(text) => {
                        text.contains("last turn content trimmed")
                    }
                    _ => false,
                });
                assert!(
                    has_provenance,
                    "expected provenance marker in degraded projection"
                );

                // Verify trimmed content markers are present.
                let has_trimmed_marker = proj.conversation.iter().any(|msg| match msg {
                    ConversationMessage::AssistantBlocks(blocks) => blocks.iter().any(
                        |b| matches!(b, ModelBlock::Text { text } if text.contains("trimmed from")),
                    ),
                    ConversationMessage::UserToolResults(results) => {
                        results.iter().any(|r| r.content.contains("trimmed from"))
                    }
                    _ => false,
                });
                assert!(
                    has_trimmed_marker,
                    "expected trimmed content marker in degraded projection"
                );
            }
            TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                panic!(
                    "expected degraded projection, got BaselineOverBudget: reason={}",
                    diagnostics.reason
                );
            }
        }
    }

    #[test]
    fn build_turn_local_projection_degraded_last_round_still_fails_on_extreme_budget() {
        // When even the degraded version cannot fit, should still report minimum_exact_round_unfit.
        let rounds = vec![fixture_round(1, &"huge content ".repeat(500))];
        let prompt_frame = fixture_prompt_frame();

        // Very tight budget: just barely above baseline, but far below even a minimal degraded round.
        let baseline_conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        let baseline_tokens = estimate_projection_tokens(&prompt_frame, &baseline_conversation);
        let prompt_budget = baseline_tokens + 10 + CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS;

        let projection = build_turn_local_projection(
            &prompt_frame,
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            prompt_budget,
            120,
        );

        match projection {
            TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                assert!(
                    diagnostics.reason == "minimum_exact_round_unfit"
                        || diagnostics.reason == "baseline_unfit",
                    "expected minimum_exact_round_unfit or baseline_unfit, got: {}",
                    diagnostics.reason
                );
            }
            TurnLocalProjectionOutcome::Projection(_) => {
                panic!("expected BaselineOverBudget on extreme budget, got Projection");
            }
        }
    }

    #[test]
    fn build_turn_local_projection_strict_fallback_preserves_one_exact_round() {
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(170)),
            fixture_round(2, &"beta ".repeat(170)),
            fixture_round(3, &"gamma ".repeat(170)),
        ];

        let projection = build_turn_local_projection(
            &fixture_prompt_frame(),
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            700 + estimate_text_tokens(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT),
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        let compaction = projection.compaction.expect("expected compaction stats");

        assert!(compaction.strict_fallback_applied);
        assert_eq!(compaction.exact_tail_rounds, 1);
        assert_eq!(compaction.compacted_rounds, 2);
        assert!(
            projection
                .conversation
                .iter()
                .filter(|message| matches!(message, ConversationMessage::AssistantBlocks(_)))
                .count()
                >= 1
        );
    }

    #[test]
    fn build_turn_local_projection_adds_checkpoint_prompt_when_compaction_applies() {
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(170)),
            fixture_round(2, &"beta ".repeat(170)),
            fixture_round(3, &"gamma ".repeat(170)),
        ];

        let projection = build_turn_local_projection(
            &fixture_prompt_frame(),
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            700 + estimate_text_tokens(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT),
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        assert!(
            projection.compaction.is_some(),
            "expected turn-local compaction"
        );
        let compaction = projection.compaction.as_ref().expect("compaction stats");
        assert_eq!(
            compaction.checkpoint_mode,
            Some(TurnLocalCheckpointMode::Full)
        );
        assert_eq!(compaction.checkpoint_request_id.as_deref(), Some("req-1"));
        assert_eq!(compaction.checkpoint_anchor_generation, Some(0));
        assert_eq!(compaction.checkpoint_base_round, None);
        assert_eq!(compaction.previous_checkpoint_round, None);
        let checkpoint = projection
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserText(text)
                    if text.contains("progress checkpoint request") =>
                {
                    Some(text)
                }
                _ => None,
            })
            .expect("missing progress checkpoint prompt");
        assert!(checkpoint.contains("current user goal"));
        assert!(checkpoint.contains("what remains unknown"));
        assert!(checkpoint.contains("next goal-aligned action"));
        assert!(checkpoint.contains("Do not assume the task requires code changes"));
        assert!(!checkpoint.contains("start editing"));
        assert!(!checkpoint.contains("begin implementation"));
    }

    #[test]
    fn build_turn_local_projection_uses_structured_checkpoint_state_for_delta_prompt() {
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(170)),
            fixture_round(2, &"beta ".repeat(170)),
            fixture_round(
                3,
                &format!(
                    "{} 普通回复，不包含 checkpoint 关键词。",
                    "gamma ".repeat(170)
                ),
            ),
        ];
        let checkpoint_state = checkpoint_state_with_latest("结构化记录：继续处理 #495。", 2, 0);

        let projection = build_turn_local_projection(
            &fixture_prompt_frame(),
            &rounds,
            &[],
            &checkpoint_state,
            Some("req-delta".into()),
            700 + estimate_text_tokens(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT),
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        let compaction = projection.compaction.as_ref().expect("compaction stats");
        assert_eq!(
            compaction.checkpoint_mode,
            Some(TurnLocalCheckpointMode::Delta)
        );
        assert_eq!(
            compaction.checkpoint_request_id.as_deref(),
            Some("req-delta")
        );
        assert_eq!(compaction.checkpoint_base_round, Some(2));
        assert_eq!(compaction.previous_checkpoint_round, Some(2));
        assert!(!compaction.anchor_changed_since_checkpoint);
        let checkpoint = projection
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserText(text)
                    if text.contains("delta progress checkpoint request") =>
                {
                    Some(text)
                }
                _ => None,
            })
            .expect("missing delta checkpoint prompt");
        assert!(checkpoint.contains("结构化记录"));
    }

    #[test]
    fn build_turn_local_checkpoint_request_uses_delta_checkpoint_when_anchor_is_unchanged() {
        let state = checkpoint_state_with_latest(
            "进度记录\n当前目标：修复 issue #495。\n下一步：继续检查结构化状态。",
            2,
            3,
        );

        let checkpoint = build_turn_local_checkpoint_request(&state, Some("req-2".into()));

        assert_eq!(checkpoint.mode, TurnLocalCheckpointMode::Delta);
        assert_eq!(checkpoint.request_id.as_deref(), Some("req-2"));
        assert_eq!(checkpoint.previous_checkpoint_round, Some(2));
        assert_eq!(checkpoint.base_round, Some(2));
        assert_eq!(checkpoint.anchor_generation, 3);
        assert!(!checkpoint.anchor_changed_since_checkpoint);
        assert!(checkpoint
            .prompt
            .contains("delta progress checkpoint request"));
        assert!(checkpoint.prompt.contains("Base checkpoint round: 2"));
        assert!(checkpoint.prompt.contains("修复 issue #495"));
        assert!(checkpoint
            .prompt
            .contains("Do not restate the full checkpoint"));
        assert!(checkpoint.prompt.contains("If no material facts changed"));
    }

    #[test]
    fn build_turn_local_checkpoint_request_uses_full_checkpoint_without_latest() {
        let state = TurnLocalCheckpointState::default();

        let checkpoint = build_turn_local_checkpoint_request(&state, Some("req-1".into()));

        assert_eq!(checkpoint.mode, TurnLocalCheckpointMode::Full);
        assert_eq!(checkpoint.request_id.as_deref(), Some("req-1"));
        assert_eq!(checkpoint.previous_checkpoint_round, None);
        assert_eq!(checkpoint.base_round, None);
        assert_eq!(checkpoint.anchor_generation, 0);
        assert!(!checkpoint.anchor_changed_since_checkpoint);
        assert!(
            checkpoint
                .prompt
                .contains("full progress checkpoint request"),
            "expected a new full checkpoint prompt"
        );
    }

    #[test]
    fn build_turn_local_checkpoint_request_uses_full_checkpoint_after_anchor_generation_change() {
        let mut state = checkpoint_state_with_latest("任意旧记录，不需要英文关键词。", 2, 3);
        state.anchor_generation = 4;

        let checkpoint = build_turn_local_checkpoint_request(&state, Some("req-3".into()));

        assert_eq!(checkpoint.mode, TurnLocalCheckpointMode::Full);
        assert_eq!(checkpoint.previous_checkpoint_round, Some(2));
        assert_eq!(checkpoint.base_round, Some(2));
        assert_eq!(checkpoint.anchor_generation, 4);
        assert!(checkpoint.anchor_changed_since_checkpoint);
    }

    #[test]
    fn checkpoint_state_can_resume_from_structured_terminal_checkpoint() {
        let terminal = TurnTerminalRecord {
            turn_id: "test-turn".into(),
            turn_index: 7,
            kind: TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some("ordinary final text without checkpoint keywords".into()),
            checkpoint: Some(TurnTerminalCheckpointRecord {
                request_id: "checkpoint-7".into(),
                requested_at_round: 3,
                response_round: Some(4),
                source_turn_index: Some(7),
                text: "结构化 checkpoint：继续修复 issue。".into(),
                checkpoint_anchor_generation: 2,
                current_anchor_generation: 5,
            }),
            completed_at: chrono::Utc::now(),
            duration_ms: 10,
        };

        let state = checkpoint_state_from_last_terminal(Some(&terminal));

        let latest = state.latest.expect("checkpoint should seed latest state");
        assert_eq!(latest.request_id, "checkpoint-7");
        assert_eq!(latest.requested_at_round, 3);
        assert_eq!(latest.response_round, None);
        assert_eq!(latest.source_turn_index, Some(7));
        assert_eq!(latest.anchor_generation, 2);
        assert_eq!(state.anchor_generation, 5);
        assert!(latest.text.contains("结构化 checkpoint"));
    }

    #[test]
    fn checkpoint_state_ignores_terminal_text_without_structured_checkpoint() {
        let terminal = TurnTerminalRecord {
            turn_id: "test-turn".into(),
            turn_index: 7,
            kind: TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some(
                "Progress checkpoint:\n\n- current user goal: fix issue\n- next goal-aligned action: apply patch"
                    .into(),
            ),
            checkpoint: None,
            completed_at: chrono::Utc::now(),
            duration_ms: 10,
        };

        let state = checkpoint_state_from_last_terminal(Some(&terminal));

        assert!(state.latest.is_none());
    }

    #[test]
    fn terminal_checkpoint_from_state_preserves_anchor_generations() {
        let mut state = checkpoint_state_with_latest("结构化 checkpoint", 4, 2);
        state.anchor_generation = 5;

        let checkpoint = terminal_checkpoint_from_state(&state, 9).expect("terminal checkpoint");

        assert_eq!(checkpoint.request_id, "req-4");
        assert_eq!(checkpoint.response_round, Some(4));
        assert_eq!(checkpoint.source_turn_index, Some(9));
        assert_eq!(checkpoint.text, "结构化 checkpoint");
        assert_eq!(checkpoint.checkpoint_anchor_generation, 2);
        assert_eq!(checkpoint.current_anchor_generation, 5);
    }

    #[test]
    fn terminal_checkpoint_from_state_preserves_existing_source_turn() {
        let mut state = checkpoint_state_with_latest("结构化 checkpoint", 4, 2);
        let latest = state.latest.as_mut().expect("latest checkpoint");
        latest.source_turn_index = Some(7);

        let checkpoint = terminal_checkpoint_from_state(&state, 9).expect("terminal checkpoint");

        assert_eq!(checkpoint.source_turn_index, Some(7));
    }

    #[test]
    fn checkpoint_resume_round_carries_runtime_follow_up_prompt() {
        let round = build_checkpoint_resume_round(
            3,
            vec![ModelBlock::Text {
                text: "Progress checkpoint:\n- current user goal: fix issue\n- next goal-aligned action: apply patch".into(),
            }],
            vec![
                "Progress checkpoint:\n- current user goal: fix issue\n- next goal-aligned action: apply patch"
                    .into(),
            ],
        );

        assert_eq!(round.round, 3);
        assert_eq!(round.tool_calls.len(), 0);
        assert_eq!(round.follow_up_user_texts.len(), 1);
        assert!(round.follow_up_user_texts[0]
            .contains("Continue from the checkpoint's next goal-aligned action now"));
        assert!(round.estimated_tokens > 0);
    }

    #[test]
    fn round_invalidates_checkpoint_anchor_for_successful_state_mutation_tools_only() {
        assert!(round_invalidates_checkpoint_anchor(
            &fixture_round_with_tool_result(
                1,
                "patch",
                "ApplyPatch",
                serde_json::json!({ "patch": "--- a/app.txt\n+++ b/app.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n" }),
                ToolResultStatus::Success,
            )
        ));
        assert!(round_invalidates_checkpoint_anchor(
            &fixture_round_with_tool_result(
                2,
                "plan",
                "UpdateWorkItem",
                serde_json::json!({ "work_item_id": "item-1", "plan": [] }),
                ToolResultStatus::Success,
            )
        ));
        assert!(round_invalidates_checkpoint_anchor(
            &fixture_round_with_tool_result(
                3,
                "complete work item",
                "CompleteWorkItem",
                serde_json::json!({ "work_item_id": "item-1" }),
                ToolResultStatus::Success,
            )
        ));
        assert!(!round_invalidates_checkpoint_anchor(
            &fixture_round_with_tool_result(
                4,
                "verify",
                "ExecCommand",
                serde_json::json!({ "cmd": "cargo test --test runtime_compaction" }),
                ToolResultStatus::Success,
            )
        ));
        assert!(!round_invalidates_checkpoint_anchor(
            &fixture_round_with_tool_result(
                5,
                "failed plan",
                "UpdateWorkItem",
                serde_json::json!({ "work_item_id": "item-1", "plan": [] }),
                ToolResultStatus::Error,
            )
        ));
        assert!(!round_invalidates_checkpoint_anchor(
            &fixture_round_with_tool(
                6,
                "call without result",
                "ApplyPatch",
                serde_json::json!({})
            )
        ));
    }

    #[test]
    fn build_turn_local_projection_omits_checkpoint_prompt_without_compaction() {
        let rounds = vec![fixture_round(1, "short")];

        let projection = build_turn_local_projection(
            &fixture_prompt_frame(),
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            4_096,
            2_048,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        assert!(projection.compaction.is_none());
        assert!(
            projection.conversation.iter().all(|message| match message {
                ConversationMessage::UserText(text) =>
                    !text.contains("progress checkpoint request"),
                _ => true,
            }),
            "checkpoint prompt should only appear at a compaction boundary"
        );
    }

    #[test]
    fn build_turn_local_projection_keeps_follow_up_prompt_final_during_compaction() {
        let follow_up =
            "Output token limit hit. Continue exactly where you left off. Do not restart.";
        let rounds = vec![
            fixture_round(1, &"alpha ".repeat(170)),
            fixture_round(2, &"beta ".repeat(170)),
            fixture_round_with_follow_up(3, &"gamma ".repeat(170), follow_up),
        ];

        let projection = build_turn_local_projection(
            &fixture_prompt_frame(),
            &rounds,
            &[],
            &TurnLocalCheckpointState::default(),
            Some("req-1".into()),
            700 + estimate_text_tokens(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT),
            120,
        );

        let TurnLocalProjectionOutcome::Projection(projection) = projection else {
            panic!("expected projection outcome");
        };
        assert!(
            projection.compaction.is_some(),
            "expected turn-local compaction"
        );
        assert!(
            projection.conversation.iter().all(|message| match message {
                ConversationMessage::UserText(text) =>
                    !text.contains("progress checkpoint request"),
                _ => true,
            }),
            "checkpoint prompt must not override continuation recovery"
        );
        let last_user_text = projection
            .conversation
            .iter()
            .rev()
            .find_map(|message| match message {
                ConversationMessage::UserText(text) => Some(text.as_str()),
                _ => None,
            })
            .expect("missing final user text");
        assert_eq!(last_user_text, follow_up);
    }

    #[test]
    fn build_compacted_round_recap_uses_round_position_for_omitted_count() {
        let rounds = vec![
            fixture_round(10, &"very long ".repeat(200)),
            fixture_round(11, "short"),
            fixture_round(12, "short"),
        ];
        let omission_line = "- Older compacted rounds omitted from this recap due to budget: 3";
        let budget = estimate_text_tokens(
            &format!(
                "Turn-local recap for older completed rounds (runtime-generated deterministic summary):\n{omission_line}"
            ),
        ) + 10;

        let recap = build_compacted_round_recap(&rounds, budget);

        assert!(!recap.contains("Round 10"), "unexpected recap: {recap}");
        assert!(!recap.contains("Round 11"), "unexpected recap: {recap}");
        assert!(!recap.contains("Round 12"), "unexpected recap: {recap}");
        assert!(recap.contains(omission_line), "unexpected recap: {recap}");
    }

    // -----------------------------------------------------------------------
    // degraded_round_messages tests
    // -----------------------------------------------------------------------

    #[test]
    fn degraded_round_messages_no_trimmable_returns_exact() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "ExecCommand".to_string(),
            input: serde_json::json!({"cmd": "echo hi"}),
        };
        let round = TurnRoundRecord {
            round: 1,
            assistant_blocks: vec![ModelBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            }],
            text_blocks: Vec::new(),
            tool_calls: vec![call],
            tool_results: Vec::new(),
            tool_result_envelopes: Vec::new(),
            follow_up_user_texts: Vec::new(),
            estimated_tokens: 10,
        };

        let (messages, trimmed) = degraded_round_messages(&round, 100);
        assert!(!trimmed, "should not be trimmed when no trimmable content");
        assert_eq!(
            messages.len(),
            1,
            "should have exactly one message (assistant blocks)"
        );
    }

    #[test]
    fn degraded_round_messages_short_text_not_trimmed() {
        let short_text = "hello world";
        let round = fixture_round(1, short_text);

        let (messages, trimmed) = degraded_round_messages(&round, 10_000);
        assert!(!trimmed);
        let has_marker = messages
            .iter()
            .any(|m| matches!(m, ConversationMessage::UserText(t) if t.contains("trimmed")));
        assert!(
            !has_marker,
            "should not have provenance marker for short text"
        );
    }

    #[test]
    fn degraded_round_messages_large_text_is_trimmed_with_marker() {
        let large_text = "word ".repeat(500);
        let round = fixture_round(1, &large_text);

        let (messages, trimmed) = degraded_round_messages(&round, 100);
        assert!(trimmed, "large text should be trimmed");
        match &messages[0] {
            ConversationMessage::UserText(t) => {
                assert!(
                    t.contains("trimmed to fit prompt budget"),
                    "first message should be provenance marker, got: {t}"
                );
            }
            other => panic!("expected UserText provenance marker, got: {:?}", other),
        }
        match &messages[1] {
            ConversationMessage::AssistantBlocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    ModelBlock::Text { text } => {
                        assert!(text.contains("[runtime: assistant text trimmed from"));
                        assert!(text.chars().count() < large_text.chars().count());
                    }
                    other => panic!("expected Text block, got: {:?}", other),
                }
            }
            other => panic!("expected AssistantBlocks, got: {:?}", other),
        }
    }

    #[test]
    fn degraded_round_messages_tool_results_trimmed() {
        let content = "output line\n".repeat(300);
        let round = fixture_round_with_large_tool_result(1, "short", &content);

        let (messages, trimmed) = degraded_round_messages(&round, 200);
        assert!(trimmed);

        let results_msg = messages
            .iter()
            .find(|m| matches!(m, ConversationMessage::UserToolResults(_)));
        assert!(results_msg.is_some(), "should have tool results message");
        if let Some(ConversationMessage::UserToolResults(results)) = results_msg {
            assert_eq!(results.len(), 1);
            assert!(results[0]
                .content
                .contains("[runtime: tool output trimmed from"));
        }
    }

    #[test]
    fn degraded_round_messages_respects_minimum_content_chars_floor() {
        let text = "x".repeat(150);
        let round = fixture_round(1, &text);

        let (_messages, trimmed) = degraded_round_messages(&round, 1);
        assert!(
            !trimmed,
            "text shorter than DEGRADED_ROUND_MINIMUM_CONTENT_CHARS should not be trimmed"
        );
    }

    #[test]
    fn degraded_round_messages_preserves_follow_up_texts() {
        let large_text = "content ".repeat(500);
        let follow_up = "user follow-up instruction";
        let round = fixture_round_with_follow_up(1, &large_text, follow_up);

        let (messages, trimmed) = degraded_round_messages(&round, 100);
        assert!(trimmed);

        let last_msg = messages.last().unwrap();
        match last_msg {
            ConversationMessage::UserText(t) => assert_eq!(t, follow_up),
            other => panic!("expected follow-up UserText, got: {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // completion_report_texts_by_tool_id tests
    // -----------------------------------------------------------------------

    #[test]
    fn completion_report_texts_captures_text_before_complete_work_item() {
        let blocks = vec![
            ModelBlock::Text {
                text: "This is the completion report.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_cw1".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({"work_item_id": "work_123"}),
            },
        ];

        let reports = completion_report_texts_by_tool_id(&blocks);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, "call_cw1");
        assert_eq!(reports[0].1, "This is the completion report.");
    }

    #[test]
    fn completion_report_texts_skips_thinking_blocks() {
        let blocks = vec![
            ModelBlock::Text {
                text: "Report text.".to_string(),
            },
            ModelBlock::Thinking {
                text: "internal reasoning".to_string(),
                signature: "sig1".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_cw2".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({"work_item_id": "work_456"}),
            },
        ];

        let reports = completion_report_texts_by_tool_id(&blocks);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, "call_cw2");
        assert_eq!(reports[0].1, "Report text.");
    }

    #[test]
    fn completion_report_texts_clears_pending_on_non_complete_tool() {
        let blocks = vec![
            ModelBlock::Text {
                text: "Some analysis text.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_exec".to_string(),
                name: "ExecCommand".to_string(),
                input: serde_json::json!({"cmd": "echo hi"}),
            },
            ModelBlock::Text {
                text: "Completion report.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_cw3".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({"work_item_id": "work_789"}),
            },
        ];

        let reports = completion_report_texts_by_tool_id(&blocks);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, "call_cw3");
        assert_eq!(reports[0].1, "Completion report.");
    }

    #[test]
    fn completion_report_texts_multiple_complete_work_items() {
        let blocks = vec![
            ModelBlock::Text {
                text: "First report.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_cw_a".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({"work_item_id": "work_a"}),
            },
            ModelBlock::Text {
                text: "Second report.".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_cw_b".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({"work_item_id": "work_b"}),
            },
        ];

        let reports = completion_report_texts_by_tool_id(&blocks);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].0, "call_cw_a");
        assert_eq!(reports[0].1, "First report.");
        assert_eq!(reports[1].0, "call_cw_b");
        assert_eq!(reports[1].1, "Second report.");
    }

    #[test]
    fn completion_report_texts_empty_when_no_text_precedes() {
        let blocks = vec![ModelBlock::ToolUse {
            id: "call_cw_empty".to_string(),
            name: "CompleteWorkItem".to_string(),
            input: serde_json::json!({"work_item_id": "work_x"}),
        }];

        let reports = completion_report_texts_by_tool_id(&blocks);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, "call_cw_empty");
        assert_eq!(reports[0].1, "");
    }

    // -----------------------------------------------------------------------
    // round_has_post_completion_non_completion_tool_call tests
    // -----------------------------------------------------------------------

    #[test]
    fn post_completion_false_when_no_tools() {
        let blocks = vec![ModelBlock::Text {
            text: "hello".to_string(),
        }];
        assert!(!round_has_post_completion_non_completion_tool_call(&blocks));
    }

    #[test]
    fn post_completion_false_when_only_complete_work_item() {
        let blocks = vec![
            ModelBlock::Text {
                text: "report".to_string(),
            },
            ModelBlock::ToolUse {
                id: "call_1".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({}),
            },
        ];
        assert!(!round_has_post_completion_non_completion_tool_call(&blocks));
    }

    #[test]
    fn post_completion_true_when_tool_after_complete() {
        let blocks = vec![
            ModelBlock::ToolUse {
                id: "call_1".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({}),
            },
            ModelBlock::ToolUse {
                id: "call_2".to_string(),
                name: "ExecCommand".to_string(),
                input: serde_json::json!({"cmd": "echo hi"}),
            },
        ];
        assert!(round_has_post_completion_non_completion_tool_call(&blocks));
    }

    #[test]
    fn post_completion_false_when_multiple_completes_no_other() {
        let blocks = vec![
            ModelBlock::ToolUse {
                id: "call_1".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({}),
            },
            ModelBlock::ToolUse {
                id: "call_2".to_string(),
                name: "CompleteWorkItem".to_string(),
                input: serde_json::json!({}),
            },
        ];
        assert!(!round_has_post_completion_non_completion_tool_call(&blocks));
    }

    // -----------------------------------------------------------------------
    // tool_result_invalidates_checkpoint_anchor tests
    // -----------------------------------------------------------------------

    #[test]
    fn invalidates_checkpoint_for_successful_state_mutation_tools() {
        for tool_name in [
            "CreateWorkItem",
            "PickWorkItem",
            "UpdateWorkItem",
            "CompleteWorkItem",
            "ApplyPatch",
        ] {
            let envelope = ToolResultEnvelope {
                tool_name: tool_name.to_string(),
                status: ToolResultStatus::Success,
                summary_text: None,
                result: Some(serde_json::json!({"ok": true})),
                error: None,
            };
            assert!(
                tool_result_invalidates_checkpoint_anchor(&envelope),
                "{tool_name} success should invalidate checkpoint"
            );
        }
    }

    #[test]
    fn does_not_invalidate_checkpoint_for_failed_state_mutation() {
        let envelope = ToolResultEnvelope {
            tool_name: "CreateWorkItem".to_string(),
            status: ToolResultStatus::Error,
            summary_text: None,
            result: None,
            error: Some(ToolError {
                kind: "invalid_input".to_string(),
                message: "bad input".to_string(),
                details: None,
                recovery_hint: None,
                retryable: false,
            }),
        };
        assert!(!tool_result_invalidates_checkpoint_anchor(&envelope));
    }

    #[test]
    fn does_not_invalidate_checkpoint_for_non_state_tools() {
        for tool_name in [
            "ExecCommand",
            "WebFetch",
            "ListTasks",
            "WaitFor",
            "SpawnAgent",
        ] {
            let envelope = ToolResultEnvelope {
                tool_name: tool_name.to_string(),
                status: ToolResultStatus::Success,
                summary_text: None,
                result: Some(serde_json::json!({"ok": true})),
                error: None,
            };
            assert!(
                !tool_result_invalidates_checkpoint_anchor(&envelope),
                "{tool_name} success should NOT invalidate checkpoint"
            );
        }
    }

    // -----------------------------------------------------------------------
    // round_invalidates_checkpoint_anchor tests
    // -----------------------------------------------------------------------

    #[test]
    fn round_invalidates_when_any_envelope_invalidates() {
        let mut round = fixture_round(1, "text");
        round.tool_result_envelopes = vec![
            ToolResultEnvelope {
                tool_name: "ExecCommand".to_string(),
                status: ToolResultStatus::Success,
                summary_text: None,
                result: Some(serde_json::json!({})),
                error: None,
            },
            ToolResultEnvelope {
                tool_name: "ApplyPatch".to_string(),
                status: ToolResultStatus::Success,
                summary_text: None,
                result: Some(serde_json::json!({})),
                error: None,
            },
        ];
        assert!(round_invalidates_checkpoint_anchor(&round));
    }

    #[test]
    fn round_does_not_invalidate_when_no_state_mutations() {
        let mut round = fixture_round(1, "text");
        round.tool_result_envelopes = vec![
            ToolResultEnvelope {
                tool_name: "ExecCommand".to_string(),
                status: ToolResultStatus::Success,
                summary_text: None,
                result: Some(serde_json::json!({})),
                error: None,
            },
            ToolResultEnvelope {
                tool_name: "WebFetch".to_string(),
                status: ToolResultStatus::Success,
                summary_text: None,
                result: Some(serde_json::json!({})),
                error: None,
            },
        ];
        assert!(!round_invalidates_checkpoint_anchor(&round));
    }

    #[test]
    fn round_does_not_invalidate_when_empty_envelopes() {
        let round = fixture_round(1, "text");
        assert!(!round_invalidates_checkpoint_anchor(&round));
    }

    // -----------------------------------------------------------------------
    // round_updated_work_item tests
    // -----------------------------------------------------------------------

    #[test]
    fn round_updated_work_item_true_for_successful_work_item_tools() {
        for tool_name in [
            "CreateWorkItem",
            "PickWorkItem",
            "UpdateWorkItem",
            "CompleteWorkItem",
        ] {
            let mut round = fixture_round(1, "text");
            round.tool_result_envelopes = vec![ToolResultEnvelope {
                tool_name: tool_name.to_string(),
                status: ToolResultStatus::Success,
                summary_text: None,
                result: Some(serde_json::json!({})),
                error: None,
            }];
            assert!(
                round_updated_work_item(&round),
                "{tool_name} success should count as work item update"
            );
        }
    }

    #[test]
    fn round_updated_work_item_false_for_apply_patch() {
        let mut round = fixture_round(1, "text");
        round.tool_result_envelopes = vec![ToolResultEnvelope {
            tool_name: "ApplyPatch".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({})),
            error: None,
        }];
        assert!(!round_updated_work_item(&round));
    }

    #[test]
    fn round_updated_work_item_false_for_failed_work_item_tool() {
        let mut round = fixture_round(1, "text");
        round.tool_result_envelopes = vec![ToolResultEnvelope {
            tool_name: "CreateWorkItem".to_string(),
            status: ToolResultStatus::Error,
            summary_text: None,
            result: None,
            error: Some(ToolError {
                kind: "invalid".to_string(),
                message: "bad".to_string(),
                details: None,
                recovery_hint: None,
                retryable: false,
            }),
        }];
        assert!(!round_updated_work_item(&round));
    }

    // -----------------------------------------------------------------------
    // result_work_item_id tests
    // -----------------------------------------------------------------------

    #[test]
    fn result_work_item_id_extracts_from_nested_result() {
        let envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({
                "work_item": {
                    "id": "work_abc123"
                }
            })),
            error: None,
        };
        assert_eq!(
            result_work_item_id(&envelope),
            Some("work_abc123".to_string())
        );
    }

    #[test]
    fn result_work_item_id_none_when_missing_result() {
        let envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: None,
            error: None,
        };
        assert_eq!(result_work_item_id(&envelope), None);
    }

    #[test]
    fn result_work_item_id_none_when_missing_nested_path() {
        let envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({"other": "data"})),
            error: None,
        };
        assert_eq!(result_work_item_id(&envelope), None);
    }

    // -----------------------------------------------------------------------
    // envelope_completes_work_item tests
    // -----------------------------------------------------------------------

    #[test]
    fn envelope_completes_work_item_true_for_valid_completion() {
        let envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({
                "completed_transition": true,
                "work_item": {"id": "work_1"}
            })),
            error: None,
        };
        assert!(envelope_completes_work_item(&envelope));
    }

    #[test]
    fn envelope_completes_work_item_false_for_failed() {
        let envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Error,
            summary_text: None,
            result: Some(serde_json::json!({"completed_transition": true})),
            error: None,
        };
        assert!(!envelope_completes_work_item(&envelope));
    }

    #[test]
    fn envelope_completes_work_item_false_for_wrong_tool() {
        let envelope = ToolResultEnvelope {
            tool_name: "UpdateWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({"completed_transition": true})),
            error: None,
        };
        assert!(!envelope_completes_work_item(&envelope));
    }

    #[test]
    fn envelope_completes_work_item_false_for_missing_transition() {
        let envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({"work_item": {"id": "work_1"}})),
            error: None,
        };
        assert!(!envelope_completes_work_item(&envelope));
    }

    // -----------------------------------------------------------------------
    // provider_lineage_failure_text tests
    // -----------------------------------------------------------------------

    #[test]
    fn provider_lineage_failure_text_empty_becomes_default() {
        assert_eq!(provider_lineage_failure_text(""), "provider failed");
        assert_eq!(provider_lineage_failure_text("   "), "provider failed");
    }

    #[test]
    fn provider_lineage_failure_text_non_empty_is_truncated() {
        let short = "connection timeout";
        assert_eq!(provider_lineage_failure_text(short), short);

        let long = "a".repeat(5000);
        let result = provider_lineage_failure_text(&long);
        assert!(
            result.ends_with("..."),
            "long text should be truncated with ellipsis"
        );
        assert!(result.len() < long.len());
    }

    // -----------------------------------------------------------------------
    // provider_lineage_operator_message tests
    // -----------------------------------------------------------------------

    #[test]
    fn operator_message_side_effect_crossed() {
        let msg =
            provider_lineage_operator_message("anthropic/claude-sonnet", true, "rate limited");
        assert!(msg.contains("Turn stopped after the active provider lineage failed"));
        assert!(msg.contains("Queued recovery turn"));
        assert!(msg.contains("anthropic/claude-sonnet"));
        assert!(msg.contains("rate limited"));
    }

    #[test]
    fn operator_message_no_side_effect() {
        let msg = provider_lineage_operator_message("openai/gpt-4", false, "context too long");
        assert!(msg.contains("Turn stopped before provider output was accepted"));
        assert!(msg.contains("Queued fallback turn"));
        assert!(msg.contains("openai/gpt-4"));
        assert!(msg.contains("context too long"));
    }

    // -----------------------------------------------------------------------
    // estimate_text_tokens tests
    // -----------------------------------------------------------------------

    #[test]
    fn estimate_text_tokens_ascii() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("a"), 1);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2);
        assert_eq!(estimate_text_tokens(&"x".repeat(100)), 25);
    }

    #[test]
    fn estimate_text_tokens_multibyte() {
        let chinese = "你好世界";
        assert_eq!(estimate_text_tokens(chinese), 1);

        let japanese = "こんにちは世界テスト";
        assert_eq!(estimate_text_tokens(japanese), 3);
    }

    // -----------------------------------------------------------------------
    // completion_report_warning tests
    // -----------------------------------------------------------------------

    #[test]
    fn completion_report_warning_produces_correct_json() {
        let warning = completion_report_warning("missing_completion_report", "no report found");
        assert_eq!(
            warning.get("kind").and_then(Value::as_str),
            Some("missing_completion_report")
        );
        assert_eq!(
            warning.get("message").and_then(Value::as_str),
            Some("no report found")
        );
    }

    // -----------------------------------------------------------------------
    // append_completion_warning tests
    // -----------------------------------------------------------------------

    #[test]
    fn append_completion_warning_creates_array_when_missing() {
        let mut envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        let warning = completion_report_warning("test_kind", "test message");
        append_completion_warning(&mut envelope, warning);

        let warnings = envelope
            .result
            .as_ref()
            .unwrap()
            .get("warnings")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].get("kind").and_then(Value::as_str),
            Some("test_kind")
        );
    }

    #[test]
    fn append_completion_warning_appends_to_existing_array() {
        let mut envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({
                "ok": true,
                "warnings": [serde_json::json!({"kind": "existing", "message": "old"})]
            })),
            error: None,
        };
        let warning = completion_report_warning("new_kind", "new message");
        append_completion_warning(&mut envelope, warning);

        let warnings = envelope
            .result
            .as_ref()
            .unwrap()
            .get("warnings")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn append_completion_warning_noop_when_no_result() {
        let mut envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: None,
            error: None,
        };
        let warning = completion_report_warning("test", "test");
        append_completion_warning(&mut envelope, warning);
        assert!(envelope.result.is_none());
    }

    // -----------------------------------------------------------------------
    // update_tool_result_block_content tests
    // -----------------------------------------------------------------------

    #[test]
    fn update_tool_result_block_content_updates_correct_index() {
        let mut blocks = vec![
            ToolResultBlock {
                tool_use_id: "call_1".to_string(),
                content: "original_1".to_string(),
                is_error: false,
                error: None,
            },
            ToolResultBlock {
                tool_use_id: "call_2".to_string(),
                content: "original_2".to_string(),
                is_error: false,
                error: None,
            },
        ];
        let envelope = ToolResultEnvelope {
            tool_name: "CompleteWorkItem".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({"completion_report_promoted": true})),
            error: None,
        };

        let result = update_tool_result_block_content(1, &mut blocks, &envelope);
        assert!(result.is_ok());
        assert_eq!(
            blocks[0].content, "original_1",
            "index 0 should be unchanged"
        );
        assert!(
            blocks[1].content.contains("completion_report_promoted"),
            "index 1 should be updated"
        );
    }

    #[test]
    fn update_tool_result_block_content_out_of_bounds_is_noop() {
        let mut blocks = vec![ToolResultBlock {
            tool_use_id: "call_1".to_string(),
            content: "original".to_string(),
            is_error: false,
            error: None,
        }];
        let envelope = ToolResultEnvelope {
            tool_name: "Test".to_string(),
            status: ToolResultStatus::Success,
            summary_text: None,
            result: Some(serde_json::json!({})),
            error: None,
        };

        let result = update_tool_result_block_content(5, &mut blocks, &envelope);
        assert!(result.is_ok());
        assert_eq!(
            blocks[0].content, "original",
            "content should be unchanged for out-of-bounds index"
        );
    }

    // -----------------------------------------------------------------------
    // append_follow_up_user_texts tests
    // -----------------------------------------------------------------------

    #[test]
    fn append_follow_up_user_texts_empty_is_noop() {
        let mut round = fixture_round(1, "text");
        let original_tokens = round.estimated_tokens;
        append_follow_up_user_texts(&mut round, Vec::new());
        assert!(round.follow_up_user_texts.is_empty());
        assert_eq!(round.estimated_tokens, original_tokens);
    }

    #[test]
    fn append_follow_up_user_texts_extends_and_recalculates_tokens() {
        let mut round = fixture_round(1, "text");
        let original_tokens = round.estimated_tokens;
        append_follow_up_user_texts(
            &mut round,
            vec!["follow up 1".to_string(), "follow up 2".to_string()],
        );
        assert_eq!(round.follow_up_user_texts.len(), 2);
        assert!(
            round.estimated_tokens > original_tokens,
            "tokens should increase after adding follow-up texts"
        );
    }
}
