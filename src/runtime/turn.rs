use std::{collections::HashSet, env, time::Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

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
    tool::{
        helpers::{command_cost_diagnostics, command_preview, effective_tool_output_tokens},
        spec::{ToolResultEnvelope, ToolResultStatus},
        ToolCall, ToolError, ToolSpec,
    },
    types::{
        AuditEvent, MessageEnvelope, QueueEntryRecord, QueueEntryStatus, TodoItemState, TokenUsage,
        TranscriptEntry, TranscriptEntryKind, TrustLevel, TurnTerminalCheckpointRecord,
        TurnTerminalKind, TurnTerminalRecord, WorkItemPlanStatus, WorkItemRecord,
    },
};

use super::{
    combine_text_history, is_max_output_stop_reason, message_dispatch::message_text, scheduler,
    CurrentRunAborted, RuntimeHandle,
};

pub(super) struct AgentLoopOutcome {
    pub(super) final_text: String,
    pub(super) should_sleep: bool,
    pub(super) sleep_duration_ms: Option<u64>,
    pub(super) terminal_kind: TurnTerminalKind,
}

pub(super) struct LoopControlOptions {
    pub(super) max_tool_rounds: Option<usize>,
}

const MAX_OUTPUT_RECOVERY_ATTEMPTS: usize = 2;
const ROUND_TEXT_PREVIEW_LIMIT: usize = 600;
const RECAP_TEXT_PREVIEW_LIMIT: usize = 160;
const MIN_EXACT_TAIL_ROUNDS: usize = 2;
pub(super) const CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS: usize = 256;
const WORK_ITEM_STALE_REMINDER_ROUNDS: usize = 10;
const WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS: usize = 10;
const WORK_ITEM_STALE_REMINDER_MAX_TOKENS: usize = 512;
const WORK_ITEM_STALE_REMINDER_PLAN_LINE_LIMIT: usize = 8;
const WORK_ITEM_STALE_REMINDER_PLAN_CHAR_LIMIT: usize = 1_200;
const WORK_ITEM_STALE_REMINDER_TODO_LIMIT: usize = 8;
const OPERATOR_INTERJECTION_HEADER: &str =
    "[Operator message received while this turn was in progress]";
const COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT: &str = "\
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

const DELTA_CHECKPOINT_PREVIEW_LIMIT: usize = 1_200;
const CHECKPOINT_RESUME_PROMPT: &str = "\
[Runtime-generated checkpoint continuation]
Continue from the checkpoint's next goal-aligned action now. Do not restate the checkpoint. If the checkpoint says enough evidence exists to act, call the concrete mutation, verification, or delivery tool next; otherwise run only the named bounded command/query.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnLocalCheckpointMode {
    Full,
    Delta,
}

impl TurnLocalCheckpointMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Delta => "delta",
        }
    }
}

#[derive(Debug, Clone)]
struct TurnLocalCheckpointRequest {
    request_id: Option<String>,
    mode: TurnLocalCheckpointMode,
    prompt: String,
    previous_checkpoint_round: Option<usize>,
    anchor_changed_since_checkpoint: bool,
    anchor_generation: u64,
    base_round: Option<usize>,
}

fn truncate_preview(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let mut preview = trimmed.chars().take(limit).collect::<String>();
    if trimmed.chars().count() > limit {
        preview.push_str("...");
    }
    preview
}

#[derive(Debug, Default, Clone)]
struct TurnLocalCheckpointState {
    latest: Option<TurnLocalCheckpointRecord>,
    pending: Option<PendingCheckpointRequest>,
    anchor_generation: u64,
}

#[derive(Debug, Clone)]
struct PendingCheckpointRequest {
    request_id: String,
    mode: TurnLocalCheckpointMode,
    requested_at_round: usize,
    anchor_generation: u64,
    base_round: Option<usize>,
    text_fragments: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct TurnLocalCheckpointRecord {
    request_id: String,
    requested_at_round: usize,
    response_round: Option<usize>,
    source_turn_index: Option<u64>,
    mode: TurnLocalCheckpointMode,
    text: String,
    anchor_generation: u64,
}

fn checkpoint_state_from_last_terminal(
    terminal: Option<&TurnTerminalRecord>,
) -> TurnLocalCheckpointState {
    let Some(terminal) = terminal else {
        return TurnLocalCheckpointState::default();
    };
    if terminal.kind != TurnTerminalKind::Completed {
        return TurnLocalCheckpointState::default();
    }
    let Some(checkpoint) = terminal.checkpoint.as_ref() else {
        return TurnLocalCheckpointState::default();
    };
    TurnLocalCheckpointState {
        latest: Some(TurnLocalCheckpointRecord {
            request_id: checkpoint.request_id.clone(),
            requested_at_round: checkpoint.requested_at_round,
            response_round: None,
            source_turn_index: checkpoint.source_turn_index.or(Some(terminal.turn_index)),
            mode: TurnLocalCheckpointMode::Full,
            text: checkpoint.text.clone(),
            anchor_generation: checkpoint.checkpoint_anchor_generation,
        }),
        pending: None,
        anchor_generation: checkpoint.current_anchor_generation,
    }
}

fn terminal_checkpoint_from_state(
    checkpoint_state: &TurnLocalCheckpointState,
    terminal_turn_index: u64,
) -> Option<TurnTerminalCheckpointRecord> {
    let latest = checkpoint_state.latest.as_ref()?;
    Some(TurnTerminalCheckpointRecord {
        request_id: latest.request_id.clone(),
        requested_at_round: latest.requested_at_round,
        response_round: latest.response_round,
        source_turn_index: latest.source_turn_index.or(Some(terminal_turn_index)),
        text: latest.text.clone(),
        checkpoint_anchor_generation: latest.anchor_generation,
        current_anchor_generation: checkpoint_state.anchor_generation,
    })
}

fn build_checkpoint_resume_round(
    round: usize,
    assistant_blocks: Vec<ModelBlock>,
    text_blocks: Vec<String>,
) -> TurnRoundRecord {
    let continuation_text = CHECKPOINT_RESUME_PROMPT.to_string();
    TurnRoundRecord {
        round,
        estimated_tokens: build_round_estimated_tokens(
            &assistant_blocks,
            &[],
            std::slice::from_ref(&continuation_text),
        ),
        assistant_blocks,
        text_blocks,
        tool_calls: Vec::new(),
        tool_results: Vec::new(),
        tool_result_envelopes: Vec::new(),
        follow_up_user_texts: vec![continuation_text],
    }
}

fn append_follow_up_user_texts(round: &mut TurnRoundRecord, texts: Vec<String>) {
    if texts.is_empty() {
        return;
    }
    round.follow_up_user_texts.extend(texts);
    round.estimated_tokens = build_round_estimated_tokens(
        &round.assistant_blocks,
        &round.tool_results,
        &round.follow_up_user_texts,
    );
}

fn render_metadata_value<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(label)) => label,
        Ok(Value::Null) => "none".into(),
        Ok(value) => value.to_string(),
        Err(_) => "unavailable".into(),
    }
}

fn render_operator_interjection_text(message: &MessageEnvelope) -> String {
    format!(
        "{OPERATOR_INTERJECTION_HEADER}\nmessage_id={}\norigin={}\ntrust={}\nauthority_class={}\ndelivery_surface={}\nadmission_context={}\n\n{}",
        message.id,
        render_metadata_value(&message.origin),
        render_metadata_value(&message.trust),
        render_metadata_value(&message.authority_class),
        render_metadata_value(&message.delivery_surface),
        render_metadata_value(&message.admission_context),
        message_text(&message.body).trim(),
    )
}

#[derive(Debug, Clone)]
struct TurnRoundRecord {
    round: usize,
    assistant_blocks: Vec<ModelBlock>,
    text_blocks: Vec<String>,
    tool_calls: Vec<ToolCall>,
    tool_results: Vec<ToolResultBlock>,
    tool_result_envelopes: Vec<ToolResultEnvelope>,
    follow_up_user_texts: Vec<String>,
    estimated_tokens: usize,
}

#[derive(Debug, Clone)]
struct TurnLocalCompactionStats {
    compacted_rounds: usize,
    exact_tail_rounds: usize,
    projected_estimated_tokens: usize,
    effective_budget_estimated_tokens: usize,
    tool_overhead_estimated_tokens: usize,
    strict_fallback_applied: bool,
    checkpoint_request_id: Option<String>,
    checkpoint_mode: Option<TurnLocalCheckpointMode>,
    checkpoint_anchor_generation: Option<u64>,
    checkpoint_base_round: Option<usize>,
    previous_checkpoint_round: Option<usize>,
    anchor_changed_since_checkpoint: bool,
}

#[derive(Debug, Clone)]
struct TurnLocalProjection {
    conversation: Vec<ConversationMessage>,
    compaction: Option<TurnLocalCompactionStats>,
}

#[derive(Debug, Clone)]
struct TurnLocalBaselineOverBudget {
    reason: String,
    estimated_baseline_tokens: usize,
    minimum_exact_round_estimated_tokens: usize,
    minimum_projection_estimated_tokens: usize,
    effective_budget_estimated_tokens: usize,
    tool_overhead_estimated_tokens: usize,
    system_prompt_estimated_tokens: usize,
    context_attachment_estimated_tokens: usize,
}

#[derive(Debug, Clone)]
enum TurnLocalProjectionOutcome {
    Projection(TurnLocalProjection),
    BaselineOverBudget(TurnLocalBaselineOverBudget),
}

fn estimate_text_tokens(text: &str) -> usize {
    text.chars().count().saturating_add(3) / 4
}

fn estimate_json_tokens(value: &Value) -> usize {
    estimate_text_tokens(&serde_json::to_string(value).unwrap_or_default())
}

fn estimate_model_block_tokens(block: &ModelBlock) -> usize {
    match block {
        ModelBlock::Text { text } => estimate_text_tokens(text),
        ModelBlock::ToolUse { id, name, input } => {
            estimate_text_tokens(id) + estimate_text_tokens(name) + estimate_json_tokens(input)
        }
        ModelBlock::Thinking { text, .. } => estimate_text_tokens(text),
        ModelBlock::RedactedThinking { data } => estimate_text_tokens(data),
    }
}

fn estimate_tool_result_block_tokens(block: &ToolResultBlock) -> usize {
    estimate_text_tokens(&block.tool_use_id)
        .saturating_add(estimate_text_tokens(&block.content))
        .saturating_add(
            block
                .error
                .as_ref()
                .map(|error| estimate_text_tokens(&error.message))
                .unwrap_or_default(),
        )
}

fn build_round_estimated_tokens(
    assistant_blocks: &[ModelBlock],
    tool_results: &[ToolResultBlock],
    follow_up_user_texts: &[String],
) -> usize {
    assistant_blocks
        .iter()
        .map(estimate_model_block_tokens)
        .sum::<usize>()
        .saturating_add(
            tool_results
                .iter()
                .map(estimate_tool_result_block_tokens)
                .sum::<usize>(),
        )
        .saturating_add(
            follow_up_user_texts
                .iter()
                .map(|text| estimate_text_tokens(text))
                .sum::<usize>(),
        )
}

fn estimate_round_tokens(round: &TurnRoundRecord) -> usize {
    round.estimated_tokens
}

pub(super) fn estimate_tool_specs_tokens(available_tools: &[ToolSpec]) -> usize {
    available_tools
        .iter()
        .map(|tool| {
            estimate_text_tokens(&tool.name)
                .saturating_add(estimate_text_tokens(&tool.description))
                .saturating_add(estimate_json_tokens(&tool.input_schema))
        })
        .sum()
}

fn estimate_prompt_blocks_tokens(blocks: &[PromptContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| estimate_text_tokens(&block.text))
        .sum()
}

fn estimate_prompt_frame_tokens(prompt_frame: &ProviderPromptFrame) -> usize {
    let structured_tokens = estimate_prompt_blocks_tokens(&prompt_frame.system_blocks);
    if structured_tokens == 0 {
        estimate_text_tokens(&prompt_frame.system_prompt)
    } else {
        structured_tokens
    }
}

#[derive(Debug, Clone, Default)]
struct ProviderAttemptModelState {
    requested_model: Option<ModelRef>,
    active_model: Option<ModelRef>,
    fallback_active: bool,
}

fn normalize_provider_attempt_timing(
    timeline: Option<ProviderAttemptTimeline>,
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    duration_ms: u64,
) -> Option<ProviderAttemptTimeline> {
    let mut timeline = timeline?;
    if timeline.attempts.len() != 1 {
        return Some(timeline);
    }

    for attempt in &mut timeline.attempts {
        if attempt.started_at.is_none() {
            attempt.started_at = Some(started_at);
        }
        if attempt.completed_at.is_none() {
            attempt.completed_at = Some(completed_at);
        }
        if attempt.duration_ms.is_none() {
            attempt.duration_ms = Some(duration_ms);
        }
    }
    Some(timeline)
}

fn provider_attempt_model_state(
    timeline: Option<&ProviderAttemptTimeline>,
) -> ProviderAttemptModelState {
    let Some(timeline) = timeline else {
        return ProviderAttemptModelState::default();
    };
    let requested_model = (!timeline.requested_model_ref.is_empty())
        .then(|| ModelRef::parse(&timeline.requested_model_ref).ok())
        .flatten();
    let active_model = timeline
        .active_model_ref
        .as_deref()
        .or(timeline.winning_model_ref.as_deref())
        .and_then(|model| ModelRef::parse(model).ok());
    let fallback_active = requested_model
        .as_ref()
        .zip(active_model.as_ref())
        .is_some_and(|(requested, active)| requested != active);

    ProviderAttemptModelState {
        requested_model,
        active_model,
        fallback_active,
    }
}

fn estimate_projection_tokens(
    prompt_frame: &ProviderPromptFrame,
    conversation: &[ConversationMessage],
) -> usize {
    let mut total = estimate_prompt_frame_tokens(prompt_frame);
    for message in conversation {
        total = total.saturating_add(match message {
            ConversationMessage::UserText(text) => estimate_text_tokens(text),
            ConversationMessage::UserBlocks(blocks) => estimate_prompt_blocks_tokens(blocks),
            ConversationMessage::AssistantBlocks(blocks) => blocks
                .iter()
                .map(estimate_model_block_tokens)
                .sum::<usize>(),
            ConversationMessage::UserToolResults(results) => results
                .iter()
                .map(estimate_tool_result_block_tokens)
                .sum::<usize>(),
        });
    }
    total
}

fn exact_round_messages(round: &TurnRoundRecord) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();
    messages.push(ConversationMessage::AssistantBlocks(
        round.assistant_blocks.clone(),
    ));
    if !round.tool_results.is_empty() {
        messages.push(ConversationMessage::UserToolResults(
            round.tool_results.clone(),
        ));
    }
    messages.extend(
        round
            .follow_up_user_texts
            .iter()
            .cloned()
            .map(ConversationMessage::UserText),
    );
    messages
}

fn select_exact_tail_start(rounds: &[TurnRoundRecord], keep_recent_budget: usize) -> usize {
    if rounds.len() <= MIN_EXACT_TAIL_ROUNDS {
        return 0;
    }

    let mut exact_tail_tokens = 0usize;
    let mut tail_start = rounds.len();
    for index in (0..rounds.len()).rev() {
        let rounds_from_tail = rounds.len().saturating_sub(index);
        let round_tokens = estimate_round_tokens(&rounds[index]);
        if rounds_from_tail <= MIN_EXACT_TAIL_ROUNDS
            || exact_tail_tokens.saturating_add(round_tokens) <= keep_recent_budget
        {
            exact_tail_tokens = exact_tail_tokens.saturating_add(round_tokens);
            tail_start = index;
            continue;
        }
        break;
    }
    tail_start
}

fn artifact_paths(value: &Value) -> Vec<String> {
    value
        .get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|artifact| artifact.get("path").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn summarize_exec_command_result(result: &Value) -> String {
    let disposition = result
        .get("disposition")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match disposition {
        "completed" => {
            let exit_status = result
                .get("exit_status")
                .and_then(Value::as_i64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into());
            let truncated = result
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let artifacts = artifact_paths(result);
            let artifact_note = if artifacts.is_empty() {
                String::new()
            } else {
                format!(" artifacts={}", artifacts.join(", "))
            };
            format!("completed exit_status={exit_status} truncated={truncated}{artifact_note}")
        }
        "promoted_to_task" => {
            let task_id = result
                .get("task_handle")
                .and_then(|handle| handle.get("task_id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let truncated = result
                .get("initial_output_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            format!("promoted_to_task task_id={task_id} initial_output_truncated={truncated}")
        }
        _ => format!("disposition={disposition}"),
    }
}

fn summarize_task_output_result(result: &Value) -> String {
    let retrieval_status = result
        .get("retrieval_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let task = result.get("task").unwrap_or(result);
    let task_id = task.get("id").and_then(Value::as_str).unwrap_or("unknown");
    let truncated = task
        .get("output_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_status = task
        .get("exit_status")
        .and_then(Value::as_i64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into());
    let artifacts = artifact_paths(task);
    let artifact_note = if artifacts.is_empty() {
        String::new()
    } else {
        format!(" artifacts={}", artifacts.join(", "))
    };
    format!(
        "retrieval_status={retrieval_status} task_id={task_id} output_truncated={truncated} exit_status={exit_status}{artifact_note}"
    )
}

fn summarize_spawn_agent_result(result: &Value) -> Option<String> {
    let agent_id = result.get("agent_id").and_then(Value::as_str)?;
    let task_id = result
        .get("task_handle")
        .and_then(|handle| handle.get("task_id"))
        .and_then(Value::as_str);
    Some(match task_id {
        Some(task_id) => format!("agent_id={agent_id} task_id={task_id}"),
        None => format!("agent_id={agent_id}"),
    })
}

fn summarize_tool_result_envelope(envelope: &ToolResultEnvelope) -> String {
    match envelope.status {
        ToolResultStatus::Error => {
            let error = envelope.error.as_ref();
            let kind = error
                .map(|error| error.kind.as_str())
                .unwrap_or("tool_execution_failed");
            let message = envelope
                .summary_text
                .as_deref()
                .or_else(|| error.map(|error| error.message.as_str()))
                .unwrap_or("tool failed");
            format!("{} error {}: {}", envelope.tool_name, kind, message)
        }
        ToolResultStatus::Success => {
            let detail = envelope
                .result
                .as_ref()
                .and_then(|result| match envelope.tool_name.as_str() {
                    "ExecCommand" => Some(summarize_exec_command_result(result)),
                    "TaskOutput" => Some(summarize_task_output_result(result)),
                    "SpawnAgent" => summarize_spawn_agent_result(result),
                    _ => None,
                })
                .or_else(|| envelope.summary_text.clone())
                .unwrap_or_else(|| "completed".into());
            format!("{} {}", envelope.tool_name, detail)
        }
    }
}

fn build_round_recap_line(round: &TurnRoundRecord) -> String {
    let assistant_text = round
        .text_blocks
        .iter()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let assistant_preview = if assistant_text.is_empty() {
        None
    } else {
        Some(truncate_preview(&assistant_text, RECAP_TEXT_PREVIEW_LIMIT))
    };
    let tool_calls = round
        .tool_calls
        .iter()
        .map(|call| call.name.as_str())
        .collect::<Vec<_>>();
    let tool_results = round
        .tool_result_envelopes
        .iter()
        .map(summarize_tool_result_envelope)
        .collect::<Vec<_>>();

    let mut parts = Vec::new();
    if let Some(preview) = assistant_preview {
        parts.push(format!("assistant=\"{preview}\""));
    }
    if !tool_calls.is_empty() {
        parts.push(format!("tool_calls=[{}]", tool_calls.join(", ")));
    }
    if !tool_results.is_empty() {
        parts.push(format!("results=[{}]", tool_results.join(" | ")));
    }
    if !round.follow_up_user_texts.is_empty() {
        parts.push(format!(
            "follow_up_user_texts={}",
            round.follow_up_user_texts.len()
        ));
    }

    let detail = if parts.is_empty() {
        "no compactable detail".into()
    } else {
        parts.join("; ")
    };
    format!("- Round {}: {}", round.round, detail)
}

fn build_compacted_round_recap(rounds: &[TurnRoundRecord], recap_budget: usize) -> String {
    if rounds.is_empty() {
        return String::new();
    }

    let header =
        "Turn-local recap for older completed rounds (runtime-generated deterministic summary):";
    let fallback = format!(
        "{header}\n- {} older rounds compacted; consult transcript or referenced artifacts if exact details are needed.",
        rounds.len()
    );
    if estimate_text_tokens(&fallback) > recap_budget {
        if estimate_text_tokens(header) <= recap_budget {
            return header.to_string();
        }
        return String::new();
    }

    let mut recap = String::from(header);
    let mut omitted = 0usize;
    for (idx, round) in rounds.iter().enumerate() {
        let line = build_round_recap_line(round);
        let candidate = format!("{recap}\n{line}");
        if estimate_text_tokens(&candidate) > recap_budget {
            omitted = rounds.len().saturating_sub(idx);
            break;
        }
        recap = candidate;
    }

    if omitted > 0 {
        let omission_line =
            format!("- Older compacted rounds omitted from this recap due to budget: {omitted}");
        let candidate = format!("{recap}\n{omission_line}");
        if estimate_text_tokens(&candidate) <= recap_budget {
            recap = candidate;
        }
    }

    recap
}

fn tool_result_invalidates_checkpoint_anchor(envelope: &ToolResultEnvelope) -> bool {
    envelope.status == ToolResultStatus::Success
        && matches!(
            envelope.tool_name.as_str(),
            "CreateWorkItem"
                | "PickWorkItem"
                | "UpdateWorkItem"
                | "CompleteWorkItem"
                | "ApplyPatch"
        )
}

fn round_invalidates_checkpoint_anchor(round: &TurnRoundRecord) -> bool {
    round
        .tool_result_envelopes
        .iter()
        .any(tool_result_invalidates_checkpoint_anchor)
}

fn round_updated_work_item(round: &TurnRoundRecord) -> bool {
    round.tool_result_envelopes.iter().any(|envelope| {
        envelope.status == ToolResultStatus::Success
            && matches!(
                envelope.tool_name.as_str(),
                "CreateWorkItem" | "PickWorkItem" | "UpdateWorkItem" | "CompleteWorkItem"
            )
    })
}

fn work_item_stale_reminder_rounds() -> usize {
    env_usize_or_default(
        "HOLON_WORK_ITEM_STALE_REMINDER_ROUNDS",
        WORK_ITEM_STALE_REMINDER_ROUNDS,
    )
}

fn work_item_stale_reminder_cooldown_rounds() -> usize {
    env_usize_or_default(
        "HOLON_WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS",
        WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS,
    )
}

fn work_item_stale_reminder_max_tokens() -> usize {
    env_usize_or_default(
        "HOLON_WORK_ITEM_STALE_REMINDER_MAX_TOKENS",
        WORK_ITEM_STALE_REMINDER_MAX_TOKENS,
    )
}

fn env_usize_or_default(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn build_work_item_stale_reminder(
    work_item: &WorkItemRecord,
    rounds_since_update: usize,
) -> String {
    let mut lines = vec![
        "[Runtime-generated work item progress reminder]".to_string(),
        format!(
            "The current work item has gone {rounds_since_update} provider rounds without a successful CreateWorkItem, PickWorkItem, UpdateWorkItem, or CompleteWorkItem call."
        ),
        "Before continuing broad exploration, realign with the current work item. If material progress, scope changes, blockers, or completed checklist items have emerged, call UpdateWorkItem. If the current in-progress step is still not ready to update, state the specific missing fact and run only the next bounded command/query.".to_string(),
        String::new(),
        "Current work item snapshot:".to_string(),
        format!("- Id: {}", work_item.id),
        format!("- Objective: {}", work_item.objective),
        format!(
            "- Plan status: {}",
            work_item_plan_status_label(work_item.plan_status)
        ),
    ];
    if let Some(plan) = work_item.plan.as_deref() {
        lines.push("- Plan:".to_string());
        let mut plan_chars = 0usize;
        for line in plan.lines().take(WORK_ITEM_STALE_REMINDER_PLAN_LINE_LIMIT) {
            let remaining = WORK_ITEM_STALE_REMINDER_PLAN_CHAR_LIMIT.saturating_sub(plan_chars);
            if remaining == 0 {
                break;
            }
            let rendered = truncate_preview(line, remaining);
            plan_chars = plan_chars.saturating_add(rendered.len());
            lines.push(format!("  {rendered}"));
        }
        let omitted_lines = plan
            .lines()
            .skip(WORK_ITEM_STALE_REMINDER_PLAN_LINE_LIMIT)
            .count();
        if omitted_lines > 0 || plan_chars >= WORK_ITEM_STALE_REMINDER_PLAN_CHAR_LIMIT {
            lines.push("  ... plan truncated".to_string());
        }
    }
    if !work_item.todo_list.is_empty() {
        lines.push("- Todo list:".to_string());
        lines.extend(
            work_item
                .todo_list
                .iter()
                .filter(|todo| todo.state != TodoItemState::Completed)
                .take(WORK_ITEM_STALE_REMINDER_TODO_LIMIT)
                .map(|todo| format!("  - [{}] {}", todo_item_state_label(todo.state), todo.text)),
        );
        let omitted = work_item
            .todo_list
            .iter()
            .filter(|todo| todo.state != TodoItemState::Completed)
            .skip(WORK_ITEM_STALE_REMINDER_TODO_LIMIT)
            .count();
        if omitted > 0 {
            lines.push(format!(
                "  - ... {omitted} more active todo item(s) omitted"
            ));
        }
    }
    if let Some(blocked_by) = work_item.blocked_by.as_deref() {
        lines.push(format!("- Blocked by: {blocked_by}"));
    }
    let reminder = lines.join("\n");
    truncate_reminder_to_token_budget(&reminder, work_item_stale_reminder_max_tokens())
}

fn maybe_reset_work_item_stale_reminder_cooldown(
    rounds_since_work_item_reminder: &mut usize,
    reminder_injected: bool,
) {
    if reminder_injected {
        *rounds_since_work_item_reminder = 0;
    }
}

fn truncate_reminder_to_token_budget(reminder: &str, max_tokens: usize) -> String {
    if estimate_text_tokens(reminder) <= max_tokens {
        return reminder.to_string();
    }
    let max_chars = max_tokens.saturating_mul(4).max(256);
    format!(
        "{}\n... reminder truncated to fit token budget",
        truncate_preview(reminder, max_chars)
    )
}

fn runtime_reminder_fits_baseline(
    prompt_frame: &ProviderPromptFrame,
    available_tools: &[ToolSpec],
    prompt_budget: usize,
    reminder: &str,
) -> bool {
    let effective_budget_estimated_tokens = prompt_budget
        .saturating_sub(estimate_tool_specs_tokens(available_tools))
        .saturating_sub(CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS);
    let baseline_with_reminder = estimate_prompt_frame_tokens(prompt_frame)
        .saturating_add(estimate_prompt_blocks_tokens(&prompt_frame.context_blocks))
        .saturating_add(estimate_text_tokens(reminder));
    baseline_with_reminder <= effective_budget_estimated_tokens
}

fn work_item_plan_status_label(status: WorkItemPlanStatus) -> &'static str {
    match status {
        WorkItemPlanStatus::Draft => "draft",
        WorkItemPlanStatus::Ready => "ready",
        WorkItemPlanStatus::NeedsInput => "needs_input",
    }
}

fn todo_item_state_label(state: TodoItemState) -> &'static str {
    match state {
        TodoItemState::Pending => "pending",
        TodoItemState::InProgress => "in_progress",
        TodoItemState::Completed => "completed",
    }
}

fn build_delta_checkpoint_prompt(
    previous_round: Option<usize>,
    source_turn_index: Option<u64>,
    previous_checkpoint: &str,
) -> String {
    let previous = truncate_preview(previous_checkpoint, DELTA_CHECKPOINT_PREVIEW_LIMIT);
    let base_source = match (previous_round, source_turn_index) {
        (Some(round), _) => format!("Base checkpoint round: {round}"),
        (None, Some(turn_index)) => format!("Base checkpoint source: previous turn {turn_index}"),
        (None, None) => "Base checkpoint source: previous turn".to_string(),
    };
    format!(
        "\
[Runtime-generated delta progress checkpoint request]
You are crossing another context compaction boundary. A previous checkpoint is still the active base.

{base_source}
Base checkpoint preview:
{previous}

Do not restate the full checkpoint. Provide only a concise delta since that base checkpoint.

Include:
- new confirmed facts since the base checkpoint, if any
- new blockers or missing facts since the base checkpoint, if any
- whether the next bounded action changed

If no material facts changed, say exactly that and continue from the base checkpoint's next action.
Keep this delta brief; it exists to preserve continuity after tool output compression, not to re-summarize the full task."
    )
}

fn push_runtime_reminder_message(
    conversation: &mut Vec<ConversationMessage>,
    runtime_reminder: Option<&str>,
) {
    if let Some(reminder) = runtime_reminder.filter(|text| !text.trim().is_empty()) {
        conversation.push(ConversationMessage::UserText(reminder.to_string()));
    }
}

fn build_turn_local_checkpoint_request(
    checkpoint_state: &TurnLocalCheckpointState,
    request_id: Option<String>,
) -> TurnLocalCheckpointRequest {
    let Some(latest) = checkpoint_state.latest.as_ref() else {
        return TurnLocalCheckpointRequest {
            request_id,
            mode: TurnLocalCheckpointMode::Full,
            prompt: COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT.to_string(),
            previous_checkpoint_round: None,
            anchor_changed_since_checkpoint: false,
            anchor_generation: checkpoint_state.anchor_generation,
            base_round: None,
        };
    };

    let base_round = latest.response_round;
    let anchor_changed_since_checkpoint =
        latest.anchor_generation != checkpoint_state.anchor_generation;
    if anchor_changed_since_checkpoint {
        TurnLocalCheckpointRequest {
            request_id,
            mode: TurnLocalCheckpointMode::Full,
            prompt: COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT.to_string(),
            previous_checkpoint_round: base_round,
            anchor_changed_since_checkpoint,
            anchor_generation: checkpoint_state.anchor_generation,
            base_round,
        }
    } else {
        TurnLocalCheckpointRequest {
            request_id,
            mode: TurnLocalCheckpointMode::Delta,
            prompt: build_delta_checkpoint_prompt(
                latest.response_round,
                latest.source_turn_index,
                &latest.text,
            ),
            previous_checkpoint_round: base_round,
            anchor_changed_since_checkpoint,
            anchor_generation: checkpoint_state.anchor_generation,
            base_round,
        }
    }
}

#[cfg(test)]
fn build_turn_local_projection(
    prompt_frame: &ProviderPromptFrame,
    rounds: &[TurnRoundRecord],
    available_tools: &[ToolSpec],
    checkpoint_state: &TurnLocalCheckpointState,
    checkpoint_request_id: Option<String>,
    prompt_budget: usize,
    keep_recent_budget: usize,
) -> TurnLocalProjectionOutcome {
    build_turn_local_projection_with_runtime_reminder(
        prompt_frame,
        rounds,
        available_tools,
        checkpoint_state,
        checkpoint_request_id,
        prompt_budget,
        keep_recent_budget,
        None,
    )
}

fn build_turn_local_projection_with_runtime_reminder(
    prompt_frame: &ProviderPromptFrame,
    rounds: &[TurnRoundRecord],
    available_tools: &[ToolSpec],
    checkpoint_state: &TurnLocalCheckpointState,
    checkpoint_request_id: Option<String>,
    prompt_budget: usize,
    keep_recent_budget: usize,
    runtime_reminder: Option<&str>,
) -> TurnLocalProjectionOutcome {
    let tool_overhead_estimated_tokens = estimate_tool_specs_tokens(available_tools);
    let system_prompt_estimated_tokens = estimate_prompt_frame_tokens(prompt_frame);
    let context_attachment_estimated_tokens =
        estimate_prompt_blocks_tokens(&prompt_frame.context_blocks);
    let runtime_reminder_estimated_tokens = runtime_reminder
        .map(estimate_text_tokens)
        .unwrap_or_default();
    let effective_budget_estimated_tokens = prompt_budget
        .saturating_sub(tool_overhead_estimated_tokens)
        .saturating_sub(CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS);
    let estimated_baseline_tokens = system_prompt_estimated_tokens
        .saturating_add(context_attachment_estimated_tokens)
        .saturating_add(runtime_reminder_estimated_tokens);

    let baseline_over_budget =
        |reason: &str,
         minimum_exact_round_estimated_tokens: usize,
         minimum_projection_estimated_tokens: usize| {
            TurnLocalProjectionOutcome::BaselineOverBudget(TurnLocalBaselineOverBudget {
                reason: reason.to_string(),
                estimated_baseline_tokens,
                minimum_exact_round_estimated_tokens,
                minimum_projection_estimated_tokens,
                effective_budget_estimated_tokens,
                tool_overhead_estimated_tokens,
                system_prompt_estimated_tokens,
                context_attachment_estimated_tokens,
            })
        };

    if estimated_baseline_tokens > effective_budget_estimated_tokens {
        return baseline_over_budget("baseline_unfit", 0, estimated_baseline_tokens);
    }

    let mut exact_conversation = vec![ConversationMessage::UserBlocks(
        prompt_frame.context_blocks.clone(),
    )];
    push_runtime_reminder_message(&mut exact_conversation, runtime_reminder);
    for round in rounds {
        exact_conversation.extend(exact_round_messages(round));
    }

    let exact_estimated_tokens = estimate_projection_tokens(prompt_frame, &exact_conversation);
    if exact_estimated_tokens <= effective_budget_estimated_tokens {
        return TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
            conversation: exact_conversation,
            compaction: None,
        });
    }

    let minimum_exact_round_estimated_tokens =
        rounds.last().map(estimate_round_tokens).unwrap_or_default();
    let mut minimum_viable_conversation = vec![ConversationMessage::UserBlocks(
        prompt_frame.context_blocks.clone(),
    )];
    push_runtime_reminder_message(&mut minimum_viable_conversation, runtime_reminder);
    if let Some(last_round) = rounds.last() {
        minimum_viable_conversation.extend(exact_round_messages(last_round));
    }
    let minimum_projection_estimated_tokens =
        estimate_projection_tokens(prompt_frame, &minimum_viable_conversation);
    if minimum_projection_estimated_tokens > effective_budget_estimated_tokens {
        return baseline_over_budget(
            "minimum_exact_round_unfit",
            minimum_exact_round_estimated_tokens,
            minimum_projection_estimated_tokens,
        );
    }

    let preferred_tail_start = select_exact_tail_start(rounds, keep_recent_budget);
    let minimum_tail_start = rounds.len().saturating_sub(1);

    for tail_start in preferred_tail_start..=minimum_tail_start {
        let mut conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        push_runtime_reminder_message(&mut conversation, runtime_reminder);
        let exact_tail = &rounds[tail_start..];
        let exact_tail_tokens = exact_tail.iter().map(estimate_round_tokens).sum::<usize>();
        let include_checkpoint = exact_tail
            .iter()
            .all(|round| round.follow_up_user_texts.is_empty());
        let checkpoint_request = if include_checkpoint {
            Some(build_turn_local_checkpoint_request(
                checkpoint_state,
                checkpoint_request_id.clone(),
            ))
        } else {
            None
        };
        let checkpoint_estimated_tokens = if let Some(checkpoint) = checkpoint_request.as_ref() {
            estimate_text_tokens(&checkpoint.prompt)
        } else {
            0
        };
        let recap_budget = effective_budget_estimated_tokens.saturating_sub(
            system_prompt_estimated_tokens
                .saturating_add(context_attachment_estimated_tokens)
                .saturating_add(runtime_reminder_estimated_tokens)
                .saturating_add(exact_tail_tokens)
                .saturating_add(checkpoint_estimated_tokens),
        );
        let recap = build_compacted_round_recap(&rounds[..tail_start], recap_budget);
        if !recap.trim().is_empty() {
            conversation.push(ConversationMessage::UserText(recap));
        }
        for round in exact_tail {
            conversation.extend(exact_round_messages(round));
        }
        if let Some(checkpoint) = checkpoint_request.as_ref() {
            conversation.push(ConversationMessage::UserText(checkpoint.prompt.clone()));
        }

        let projected_estimated_tokens = estimate_projection_tokens(prompt_frame, &conversation);
        let strict_fallback_applied = tail_start > preferred_tail_start;
        if projected_estimated_tokens <= effective_budget_estimated_tokens {
            return TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
                conversation,
                compaction: Some(TurnLocalCompactionStats {
                    compacted_rounds: tail_start,
                    exact_tail_rounds: rounds.len().saturating_sub(tail_start),
                    projected_estimated_tokens,
                    effective_budget_estimated_tokens,
                    tool_overhead_estimated_tokens,
                    strict_fallback_applied,
                    checkpoint_request_id: checkpoint_request
                        .as_ref()
                        .and_then(|checkpoint| checkpoint.request_id.clone()),
                    checkpoint_mode: checkpoint_request
                        .as_ref()
                        .map(|checkpoint| checkpoint.mode),
                    checkpoint_anchor_generation: checkpoint_request
                        .as_ref()
                        .map(|checkpoint| checkpoint.anchor_generation),
                    checkpoint_base_round: checkpoint_request
                        .as_ref()
                        .and_then(|checkpoint| checkpoint.base_round),
                    previous_checkpoint_round: checkpoint_request
                        .as_ref()
                        .and_then(|checkpoint| checkpoint.previous_checkpoint_round),
                    anchor_changed_since_checkpoint: checkpoint_request
                        .as_ref()
                        .is_some_and(|checkpoint| checkpoint.anchor_changed_since_checkpoint),
                }),
            });
        }
    }

    TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
        conversation: minimum_viable_conversation,
        compaction: Some(TurnLocalCompactionStats {
            compacted_rounds: minimum_tail_start,
            exact_tail_rounds: rounds.len().saturating_sub(minimum_tail_start),
            projected_estimated_tokens: minimum_projection_estimated_tokens,
            effective_budget_estimated_tokens,
            tool_overhead_estimated_tokens,
            strict_fallback_applied: minimum_tail_start > preferred_tail_start,
            checkpoint_request_id: None,
            checkpoint_mode: None,
            checkpoint_anchor_generation: None,
            checkpoint_base_round: None,
            previous_checkpoint_round: None,
            anchor_changed_since_checkpoint: false,
        }),
    })
}

fn context_management_diagnostic(
    provider: &dyn AgentProvider,
    request: &ProviderTurnRequest,
) -> Value {
    let Some(policy) = provider.context_management_policy() else {
        return serde_json::json!({
            "enabled": false,
            "disabled_reason": "provider_context_management_not_enabled",
        });
    };

    let stats = estimate_context_management_eligible_tool_results(
        &request.conversation,
        policy.keep_recent_tool_uses,
    );
    serde_json::json!({
        "enabled": true,
        "policy": {
            "provider": policy.provider,
            "strategy": policy.strategy,
            "trigger_input_tokens": policy.trigger_input_tokens,
            "keep_recent_tool_uses": policy.keep_recent_tool_uses,
            "clear_at_least_input_tokens": policy.clear_at_least_input_tokens,
            "clears_tool_results_only": true,
            "excludes_errors": true,
            "excluded_tool_names": ["ApplyPatch", "NotifyOperator"],
        },
        "eligible_tool_result_count": stats.eligible_tool_result_count,
        "eligible_tool_result_bytes": stats.eligible_tool_result_bytes,
        "retained_recent_tool_result_count": stats.retained_recent_tool_result_count,
        "excluded_tool_result_count": stats.excluded_tool_result_count,
    })
}

#[derive(Default)]
struct ContextManagementEligibilityStats {
    eligible_tool_result_count: usize,
    eligible_tool_result_bytes: usize,
    retained_recent_tool_result_count: usize,
    excluded_tool_result_count: usize,
}

fn estimate_context_management_eligible_tool_results(
    conversation: &[ConversationMessage],
    keep_recent_tool_uses: usize,
) -> ContextManagementEligibilityStats {
    let mut tool_names_by_id = std::collections::HashMap::<&str, &str>::new();
    let mut tool_results = Vec::<(&ToolResultBlock, Option<&str>)>::new();
    for message in conversation {
        match message {
            ConversationMessage::AssistantBlocks(blocks) => {
                for block in blocks {
                    if let ModelBlock::ToolUse { id, name, .. } = block {
                        tool_names_by_id.insert(id.as_str(), name.as_str());
                    }
                }
            }
            ConversationMessage::UserToolResults(results) => {
                for result in results {
                    tool_results.push((
                        result,
                        tool_names_by_id.get(result.tool_use_id.as_str()).copied(),
                    ));
                }
            }
            ConversationMessage::UserText(_) | ConversationMessage::UserBlocks(_) => {}
        }
    }

    let recent_start = tool_results.len().saturating_sub(keep_recent_tool_uses);
    let mut stats = ContextManagementEligibilityStats::default();
    for (index, (result, tool_name)) in tool_results.into_iter().enumerate() {
        if index >= recent_start {
            stats.retained_recent_tool_result_count += 1;
            continue;
        }
        if result.is_error || is_context_management_excluded_tool(tool_name) {
            stats.excluded_tool_result_count += 1;
            continue;
        }
        stats.eligible_tool_result_count += 1;
        stats.eligible_tool_result_bytes = stats
            .eligible_tool_result_bytes
            .saturating_add(result.content.len());
    }
    stats
}

fn is_context_management_excluded_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("ApplyPatch" | "NotifyOperator"))
}

fn tool_result_error_envelope(tool_name: &str, error: ToolError) -> ToolResultEnvelope {
    ToolResultEnvelope {
        tool_name: tool_name.to_string(),
        status: ToolResultStatus::Error,
        summary_text: Some(error.message.clone()),
        result: None,
        error: Some(error),
    }
}

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
        self.persist_turn_terminal_record(
            TurnTerminalKind::Aborted,
            Some(final_text.clone()),
            duration_ms,
            None,
        )
        .await?;
        Ok(Some(AgentLoopOutcome {
            final_text,
            should_sleep: false,
            sleep_duration_ms: None,
            terminal_kind: TurnTerminalKind::Aborted,
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
            let record = TurnTerminalRecord {
                turn_index: guard.state.turn_index,
                kind,
                reason: None,
                last_assistant_message,
                checkpoint,
                completed_at: chrono::Utc::now(),
                duration_ms,
            };
            guard.state.last_turn_terminal = Some(record.clone());
            self.inner.storage.write_agent(&guard.state)?;
            record
        };
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal",
            serde_json::to_value(&record)?,
        ))?;
        Ok(record)
    }

    async fn persist_turn_aborted_record(
        &self,
        run_id: &str,
        reason: &str,
        last_assistant_message: Option<String>,
        duration_ms: u64,
    ) -> Result<TurnTerminalRecord> {
        let record = {
            let mut guard = self.inner.agent.lock().await;
            let record = TurnTerminalRecord {
                turn_index: guard.state.turn_index,
                kind: TurnTerminalKind::Aborted,
                reason: Some(reason.to_string()),
                last_assistant_message,
                checkpoint: None,
                completed_at: chrono::Utc::now(),
                duration_ms,
            };
            guard.state.last_turn_terminal = Some(record.clone());
            self.inner.storage.write_agent(&guard.state)?;
            record
        };
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal",
            serde_json::to_value(&record)?,
        ))?;
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_terminal_aborted",
            serde_json::json!({
                "run_id": run_id,
                "reason": reason,
                "record": record,
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
                if let Err(err) = self.inner.storage.write_agent(&guard.state) {
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
                        "trust": message.trust,
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
                    let _ = self.inner.storage.write_agent(&guard.state);
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
        trust: TrustLevel,
        effective_prompt: EffectivePrompt,
        loop_control: LoopControlOptions,
    ) -> Result<AgentLoopOutcome> {
        TurnExecution {
            runtime: self,
            agent_id,
            trust,
            effective_prompt,
            loop_control,
        }
        .run()
        .await
    }
}

struct TurnExecution<'a> {
    runtime: &'a RuntimeHandle,
    agent_id: &'a str,
    trust: TrustLevel,
    effective_prompt: EffectivePrompt,
    loop_control: LoopControlOptions,
}

impl TurnExecution<'_> {
    async fn run(self) -> Result<AgentLoopOutcome> {
        let TurnExecution {
            runtime,
            agent_id,
            trust,
            effective_prompt,
            loop_control,
        } = self;
        let mut completed_rounds = Vec::<TurnRoundRecord>::new();
        let turn_started_at = Instant::now();
        let mut should_sleep = false;
        let mut sleep_duration_ms = None;
        let mut round = 0usize;
        let mut truncated_text_history = Vec::new();
        let mut last_assistant_message: Option<String> = None;
        let mut max_output_recovery_count = 0usize;
        let mut rounds_since_work_item_update = 0usize;
        let mut rounds_since_work_item_reminder = work_item_stale_reminder_cooldown_rounds();
        let mut checkpoint_state = {
            let guard = runtime.inner.agent.lock().await;
            checkpoint_state_from_last_terminal(guard.state.last_turn_terminal.as_ref())
        };

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
                    runtime
                        .persist_turn_terminal_record(
                            TurnTerminalKind::Aborted,
                            Some(final_text.clone()),
                            turn_started_at.elapsed().as_millis() as u64,
                            None,
                        )
                        .await?;
                    return Ok(AgentLoopOutcome {
                        final_text,
                        should_sleep: false,
                        sleep_duration_ms: None,
                        terminal_kind: TurnTerminalKind::Aborted,
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
            let identity = runtime.agent_identity_view().await?;
            let available_tools = runtime.filtered_tool_specs(&identity)?;
            let allowed_tool_names = available_tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<HashSet<_>>();

            let (
                response,
                attempt_timeline,
                context_management,
                context_build_ms,
                provider_started_at,
                provider_completed_at,
                provider_round_ms,
            ) = if round == 1 {
                let request = build_provider_turn_request(&effective_prompt, available_tools);
                let provider = runtime.current_provider().await;
                let context_management = context_management_diagnostic(provider.as_ref(), &request);
                let context_build_ms = context_build_started.elapsed().as_millis() as u64;
                let (result, provider_started_at, provider_completed_at, provider_round_ms) =
                    runtime.complete_turn_with_timing(provider, request).await;
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
                    if runtime_reminder_fits_baseline(
                        &prompt_frame,
                        &available_tools,
                        context_config.prompt_budget_estimated_tokens,
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
                    context_config.prompt_budget_estimated_tokens,
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
                        runtime
                            .persist_turn_terminal_record(
                                TurnTerminalKind::BaselineOverBudget,
                                Some(final_text.clone()),
                                turn_started_at.elapsed().as_millis() as u64,
                                None,
                            )
                            .await?;
                        return Ok(AgentLoopOutcome {
                            final_text,
                            should_sleep: false,
                            sleep_duration_ms: None,
                            terminal_kind: TurnTerminalKind::BaselineOverBudget,
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
                    available_tools,
                );
                let provider = runtime.current_provider().await;
                let context_management = context_management_diagnostic(provider.as_ref(), &request);
                let context_build_ms = context_build_started.elapsed().as_millis() as u64;
                let (result, provider_started_at, provider_completed_at, provider_round_ms) =
                    runtime.complete_turn_with_timing(provider, request).await;
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
                runtime.inner.storage.write_agent(&guard.state)?;
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

            let completed_round_assistant_blocks = assistant_blocks.clone();
            let only_sleep_tools =
                !tool_calls.is_empty() && tool_calls.iter().all(|call| call.name == "Sleep");
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
                    "working_memory_revision": effective_prompt.cache_identity.working_memory_revision,
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
            runtime.inner
                .storage
                .append_transcript_entry(&TranscriptEntry {
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
                            "working_memory_revision": effective_prompt.cache_identity.working_memory_revision,
                            "compression_epoch": effective_prompt.cache_identity.compression_epoch,
                            "requested_model": model_attempt_state.requested_model,
                            "active_model": model_attempt_state.active_model,
                            "fallback_active": model_attempt_state.fallback_active,
                            "context_management": context_management,
                            "provider_request_diagnostics": request_diagnostics,
                            "provider_attempt_timeline": attempt_timeline,
                        }),
                    )
                })?;
            runtime.inner.storage.append_event(&AuditEvent::new(
                "assistant_round_recorded",
                serde_json::json!({
                    "agent_id": agent_id,
                    "turn_index": turn_index,
                    "run_id": run_id,
                    "round": round,
                    "work_item_id": round_work_item_id.clone(),
                    "stop_reason": stop_reason,
                    "text_preview": if combined_text.is_empty() {
                        None::<String>
                    } else {
                        Some(truncate_preview(&combined_text, ROUND_TEXT_PREVIEW_LIMIT))
                    },
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

            if !tool_calls.is_empty() {
                let interjections = runtime
                    .drain_operator_interjections(agent_id, round, "before_tool_execution")
                    .await?;
                if !interjections.is_empty() {
                    let text_only_assistant_blocks = text_blocks
                        .iter()
                        .cloned()
                        .map(|text| ModelBlock::Text { text })
                        .collect::<Vec<_>>();
                    let mut round_record = TurnRoundRecord {
                        round,
                        estimated_tokens: build_round_estimated_tokens(
                            &text_only_assistant_blocks,
                            &[],
                            &[],
                        ),
                        assistant_blocks: text_only_assistant_blocks,
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

            if tool_calls.is_empty() && is_max_output_stop_reason(stop_reason.as_deref()) {
                if max_output_recovery_count < MAX_OUTPUT_RECOVERY_ATTEMPTS {
                    if !combined_text.is_empty() {
                        truncated_text_history.push(combined_text.clone());
                    }
                    max_output_recovery_count += 1;
                    let continuation_text =
                        "Output token limit hit. Continue exactly where you left off. Do not restart from the top. Finish the remaining report directly.".to_string();
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
                    runtime
                        .inner
                        .storage
                        .append_transcript_entry(&TranscriptEntry::new(
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
                runtime
                    .inner
                    .storage
                    .append_transcript_entry(&TranscriptEntry::new(
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
                runtime
                    .persist_turn_terminal_record(
                        TurnTerminalKind::Completed,
                        last_assistant_message.clone(),
                        turn_started_at.elapsed().as_millis() as u64,
                        Some(&checkpoint_state),
                    )
                    .await?;
                return Ok(AgentLoopOutcome {
                    final_text,
                    should_sleep,
                    sleep_duration_ms,
                    terminal_kind: TurnTerminalKind::Completed,
                });
            }

            let round_tool_calls = tool_calls.clone();
            let mut tool_results = Vec::new();
            let mut tool_result_envelopes = Vec::new();
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
                if !allowed_tool_names.contains(&call.name) {
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
                    let message = error.render();
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
                            "exec_command_batch_items": command_batch_preview_field(&call),
                            "exec_command_cost": command_cost_field(
                                &call,
                                runtime.inner.default_tool_output_tokens,
                                runtime.inner.max_tool_output_tokens
                            ),
                            "error": message,
                            "error_kind": error.kind.clone(),
                            "tool_error": error.clone(),
                            "reason": "tool_not_exposed_for_round",
                        }),
                    ))?;
                    tool_results.push(ToolResultBlock {
                        tool_use_id: tool_call_id.clone(),
                        content: message,
                        is_error: true,
                        error: Some(error.clone()),
                    });
                    tool_result_envelopes.push(tool_result_error_envelope(&tool_name, error));
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
                    .with_recovery_hint(
                        "retry the mutation as a complete, smaller tool call after inspecting any needed context",
                    )
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
                let tool_execution = if let Some(snapshot) = runtime.current_run_abort_token().await
                {
                    tokio::select! {
                        result = runtime.inner.tools.execute(runtime, agent_id, &trust, &call) => result,
                        _ = snapshot.token.cancelled() => Err(CurrentRunAborted {
                            run_id: snapshot.run_id.clone(),
                            reason: snapshot.reason(),
                        }.into()),
                    }
                } else {
                    runtime
                        .inner
                        .tools
                        .execute(runtime, agent_id, &trust, &call)
                        .await
                };
                match tool_execution {
                    Ok((result, mut record)) => {
                        let result_content =
                            crate::tool::tools::render_tool_result_for_model(&result)?;
                        let duration_ms = record.duration_ms;
                        let (turn_index, run_id, current_work_item_id) = {
                            let guard = runtime.inner.agent.lock().await;
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
                        record.turn_index = turn_index;
                        if record.work_item_id.is_none() {
                            record.work_item_id = pre_tool_work_item_id
                                .clone()
                                .or(current_work_item_id)
                                .or_else(|| result_work_item_id(&result.envelope));
                        }

                        if result.should_sleep {
                            should_sleep = true;
                            sleep_duration_ms = result.sleep_duration_ms;
                        }
                        runtime.inner.storage.append_tool_execution(&record)?;
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
                            serde_json::json!({
                                "tool_call_id": tool_call_id,
                                "tool_name": tool_name,
                                "turn_index": turn_index,
                                "run_id": run_id,
                                "work_item_id": record.work_item_id.clone(),
                                "exec_command_cmd": command_preview_field(&call),
                                "exec_command_batch_items": command_batch_preview_field(&call),
                                "exec_command_cost": command_cost_field(
                                    &call,
                                    runtime.inner.default_tool_output_tokens,
                                    runtime.inner.max_tool_output_tokens
                                ),
                                "status": record.status,
                                "duration_ms": duration_ms,
                                "summary": record.summary,
                                "error": result.tool_error().map(|error| error.render()),
                                "error_kind": result.tool_error().map(|error| error.kind.clone()),
                                "tool_error": result.tool_error().cloned(),
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
                        let message = error.render();
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
                                "exec_command_batch_items": command_batch_preview_field(&call),
                                "exec_command_cost": command_cost_field(
                                    &call,
                                    runtime.inner.default_tool_output_tokens,
                                    runtime.inner.max_tool_output_tokens
                                ),
                                "error": message,
                                "error_kind": error.kind.clone(),
                                "tool_error": error.clone(),
                            }),
                        ))?;
                        tool_results.push(ToolResultBlock {
                            tool_use_id: tool_call_id,
                            content: message,
                            is_error: true,
                            error: Some(error.clone()),
                        });
                        tool_result_envelopes.push(tool_result_error_envelope(&tool_name, error));
                    }
                }
            }
            runtime
                .promote_round_completion_report_if_present(
                    agent_id,
                    round,
                    turn_index,
                    &combined_text,
                    &mut tool_results,
                    &mut tool_result_envelopes,
                )
                .await?;
            runtime
                .inner
                .storage
                .append_transcript_entry(&TranscriptEntry::new(
                    agent_id.to_string(),
                    TranscriptEntryKind::ToolResults,
                    Some(round),
                    None,
                    serde_json::json!({
                        "results": tool_results.clone(),
                    }),
                ))?;
            let interjections = runtime
                .drain_operator_interjections(agent_id, round, "after_tool_results")
                .await?;
            let has_operator_interjections = !interjections.is_empty();
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

            if only_sleep_tools && !has_operator_interjections {
                let final_text = last_assistant_message.clone().unwrap_or_default();
                runtime
                    .persist_turn_terminal_record(
                        TurnTerminalKind::Completed,
                        last_assistant_message.clone(),
                        turn_started_at.elapsed().as_millis() as u64,
                        Some(&checkpoint_state),
                    )
                    .await?;
                return Ok(AgentLoopOutcome {
                    final_text,
                    should_sleep: true,
                    sleep_duration_ms,
                    terminal_kind: TurnTerminalKind::Completed,
                });
            }
        }
    }
}

impl RuntimeHandle {
    async fn promote_round_completion_report_if_present(
        &self,
        agent_id: &str,
        round: usize,
        turn_index: u64,
        combined_text: &str,
        tool_results: &mut [ToolResultBlock],
        tool_result_envelopes: &mut [ToolResultEnvelope],
    ) -> Result<()> {
        let completion_indexes = tool_result_envelopes
            .iter()
            .enumerate()
            .filter(|(_, envelope)| {
                envelope.tool_name == "CompleteWorkItem"
                    && envelope.status == ToolResultStatus::Success
                    && envelope
                        .result
                        .as_ref()
                        .and_then(|result| result.get("completed_transition"))
                        .and_then(Value::as_bool)
                        == Some(true)
            })
            .filter_map(|(index, envelope)| result_work_item_id(envelope).map(|id| (index, id)))
            .collect::<Vec<_>>();
        if completion_indexes.is_empty() {
            return Ok(());
        }

        if completion_indexes.len() != 1 {
            for (index, work_item_id) in completion_indexes {
                let warning = completion_report_warning(
                    "completion_report_not_promoted_multiple_completions",
                    "Completion report was not promoted because this round completed multiple work items.",
                );
                append_completion_warning(&mut tool_result_envelopes[index], warning.clone());
                update_tool_result_block_content(
                    index,
                    tool_results,
                    &tool_result_envelopes[index],
                )?;
                self.record_work_item_completion_warning(
                    work_item_id,
                    "completion_report_not_promoted_multiple_completions",
                    "Completion report was not promoted because this round completed multiple work items.",
                    Some(turn_index),
                    Some(round),
                )
                .await?;
            }
            return Ok(());
        }

        let (index, work_item_id) = completion_indexes
            .into_iter()
            .next()
            .expect("checked non-empty");
        let report_text = combined_text.trim();
        if report_text.is_empty() {
            let warning = completion_report_warning(
                "missing_completion_report",
                "CompleteWorkItem succeeded without same-round operator-facing report text; no canonical completion report was promoted.",
            );
            append_completion_warning(&mut tool_result_envelopes[index], warning.clone());
            update_tool_result_block_content(index, tool_results, &tool_result_envelopes[index])?;
            self.record_work_item_completion_warning(
                work_item_id,
                "missing_completion_report",
                "CompleteWorkItem succeeded without same-round operator-facing report text; no canonical completion report was promoted.",
                Some(turn_index),
                Some(round),
            )
            .await?;
            return Ok(());
        }

        let warnings = envelope_warnings(&tool_result_envelopes[index]);
        self.promote_work_item_completion_report(
            work_item_id.clone(),
            report_text.to_string(),
            Some(turn_index),
            Some(round),
            warnings,
        )
        .await?;
        if let Some(result) = tool_result_envelopes[index].result.as_mut() {
            if let Some(object) = result.as_object_mut() {
                object.insert("completion_report_promoted".into(), serde_json::json!(true));
                object.insert(
                    "completion_report_source".into(),
                    serde_json::json!("same_assistant_round"),
                );
            }
        }
        update_tool_result_block_content(index, tool_results, &tool_result_envelopes[index])?;
        self.inner.storage.append_event(&AuditEvent::new(
            "work_item_completion_report_candidate_promoted",
            serde_json::json!({
                "agent_id": agent_id,
                "work_item_id": work_item_id,
                "turn_index": turn_index,
                "round": round,
                "text_preview": truncate_preview(report_text, ROUND_TEXT_PREVIEW_LIMIT),
            }),
        ))?;
        Ok(())
    }
}

fn result_work_item_id(envelope: &ToolResultEnvelope) -> Option<String> {
    envelope
        .result
        .as_ref()?
        .get("work_item")?
        .get("id")?
        .as_str()
        .map(ToString::to_string)
}

fn envelope_warnings(envelope: &ToolResultEnvelope) -> Vec<Value> {
    envelope
        .result
        .as_ref()
        .and_then(|result| result.get("warnings"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn completion_report_warning(kind: &str, message: &str) -> Value {
    serde_json::json!({
        "kind": kind,
        "message": message,
    })
}

fn append_completion_warning(envelope: &mut ToolResultEnvelope, warning: Value) {
    let Some(result) = envelope.result.as_mut() else {
        return;
    };
    let Some(object) = result.as_object_mut() else {
        return;
    };
    let warnings = object
        .entry("warnings")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(array) = warnings.as_array_mut() {
        array.push(warning);
    }
}

fn update_tool_result_block_content(
    index: usize,
    tool_results: &mut [ToolResultBlock],
    envelope: &ToolResultEnvelope,
) -> Result<()> {
    if let Some(block) = tool_results.get_mut(index) {
        block.content = serde_json::to_string(envelope)?;
    }
    Ok(())
}

fn command_preview_field(call: &ToolCall) -> Option<String> {
    (call.name == "ExecCommand")
        .then(|| call.input.get("cmd").and_then(Value::as_str))
        .flatten()
        .map(command_preview)
}

fn command_batch_preview_field(call: &ToolCall) -> Option<Value> {
    if call.name != "ExecCommandBatch" {
        return None;
    }
    let items = call.input.get("items").and_then(Value::as_array)?;
    let previews = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            item.get("cmd")
                .and_then(Value::as_str)
                .map(command_preview)
                .map(|cmd| {
                    serde_json::json!({
                        "index": index,
                        "cmd": cmd,
                    })
                })
        })
        .collect::<Vec<_>>();
    (!previews.is_empty()).then(|| Value::Array(previews))
}

fn command_cost_field(
    call: &ToolCall,
    default_tool_output_tokens: u64,
    max_tool_output_tokens: u64,
) -> Option<serde_json::Value> {
    if call.name != "ExecCommand" {
        return None;
    }
    let cmd = call.input.get("cmd").and_then(Value::as_str)?;
    let requested = call.input.get("max_output_tokens").and_then(Value::as_u64);
    let effective = effective_tool_output_tokens(
        requested,
        default_tool_output_tokens,
        max_tool_output_tokens,
    );
    match serde_json::to_value(command_cost_diagnostics(cmd, effective)) {
        Ok(value) => Some(value),
        Err(error) => {
            eprintln!(
                "failed to serialize command cost diagnostics for ExecCommand audit event: {error}"
            );
            None
        }
    }
}

fn rejects_truncated_mutation_tool_call(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "ApplyPatch" | "CreateWorkItem" | "PickWorkItem" | "UpdateWorkItem" | "CompleteWorkItem"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        work_item.plan = Some("Patch runtime reminder.\nRun focused tests.".into());
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
        work_item.plan = Some(
            (0..80)
                .map(|idx| format!("step {idx}: {}", "inspect and verify ".repeat(40)))
                .collect::<Vec<_>>()
                .join("\n"),
        );
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
}
