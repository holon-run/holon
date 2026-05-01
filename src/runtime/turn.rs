use std::{collections::HashSet, time::Instant};

use anyhow::Result;
use serde_json::Value;

use crate::{
    config::ModelRef,
    prompt::EffectivePrompt,
    provider::{
        provider_attempt_timeline, provider_error_is_context_length_exceeded, AgentProvider,
        ConversationMessage, ModelBlock, PromptContentBlock, ProviderAttemptTimeline,
        ProviderPromptFrame, ProviderTurnRequest, ToolResultBlock,
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
        AuditEvent, TokenUsage, TranscriptEntry, TranscriptEntryKind, TrustLevel, TurnTerminalKind,
        TurnTerminalRecord,
    },
};

use super::{combine_text_history, is_max_output_stop_reason, RuntimeHandle};

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
const CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS: usize = 256;
const COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT: &str = "\
[Runtime-generated full progress checkpoint request]
You are crossing a context compaction boundary. Before continuing, include a concise progress checkpoint for continuation in your next assistant message.

Include:
- current user goal
- current work plan state
- files, commands, or sources already inspected
- key findings and ruled-out paths
- what remains unknown
- the next goal-aligned action

If continuing exploration, name the specific missing information and the next bounded command/query.
If the current plan step is complete, update the work plan before proceeding.
This is not a request to finish the task; after the checkpoint, continue with the next goal-aligned action when useful.
Do not assume the task requires code changes unless the user goal does.";

const DELTA_CHECKPOINT_PREVIEW_LIMIT: usize = 1_200;

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
    response_round: usize,
    mode: TurnLocalCheckpointMode,
    text: String,
    anchor_generation: u64,
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

fn estimate_tool_specs_tokens(available_tools: &[ToolSpec]) -> usize {
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
        parts.push("continuation_prompt=max_output_tokens".into());
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

fn exec_command_changes_checkpoint_anchor(command: &str) -> bool {
    let command = command.trim_start();
    let lower = command.to_ascii_lowercase();
    let mut segments = lower
        .split(['&', ';', '\n'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty());
    segments.any(|segment| {
        segment.starts_with("cargo test")
            || segment.starts_with("cargo nextest")
            || segment.starts_with("cargo fmt")
            || segment.starts_with("cargo clippy")
            || segment.starts_with("make test")
            || segment.starts_with("make check")
            || segment.starts_with("npm test")
            || segment.starts_with("npm run test")
            || segment.starts_with("npm run lint")
            || segment.starts_with("npm run format")
            || segment.starts_with("pnpm test")
            || segment.starts_with("yarn test")
            || segment.starts_with("pytest")
            || segment.starts_with("uv run pytest")
            || segment.starts_with("go test")
            || segment.starts_with("git commit")
    })
}

fn tool_call_changes_checkpoint_anchor(call: &ToolCall) -> bool {
    match call.name.as_str() {
        "CreateWorkItem" | "PickWorkItem" | "UpdateWorkItem" | "CompleteWorkItem"
        | "ApplyPatch" => true,
        "ExecCommand" => call
            .input
            .get("cmd")
            .and_then(Value::as_str)
            .is_some_and(exec_command_changes_checkpoint_anchor),
        _ => false,
    }
}

fn round_changes_checkpoint_anchor(round: &TurnRoundRecord) -> bool {
    round
        .tool_calls
        .iter()
        .any(tool_call_changes_checkpoint_anchor)
}

fn build_delta_checkpoint_prompt(previous_round: usize, previous_checkpoint: &str) -> String {
    let previous = truncate_preview(previous_checkpoint, DELTA_CHECKPOINT_PREVIEW_LIMIT);
    format!(
        "\
[Runtime-generated delta progress checkpoint request]
You are crossing another context compaction boundary. A previous checkpoint is still the active base.

Base checkpoint round: {previous_round}
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

    let base_round = Some(latest.response_round);
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
            prompt: build_delta_checkpoint_prompt(latest.response_round, &latest.text),
            previous_checkpoint_round: base_round,
            anchor_changed_since_checkpoint,
            anchor_generation: checkpoint_state.anchor_generation,
            base_round,
        }
    }
}

fn build_turn_local_projection(
    prompt_frame: &ProviderPromptFrame,
    rounds: &[TurnRoundRecord],
    available_tools: &[ToolSpec],
    checkpoint_state: &TurnLocalCheckpointState,
    checkpoint_request_id: Option<String>,
    prompt_budget: usize,
    keep_recent_budget: usize,
) -> TurnLocalProjectionOutcome {
    let tool_overhead_estimated_tokens = estimate_tool_specs_tokens(available_tools);
    let system_prompt_estimated_tokens = estimate_prompt_frame_tokens(prompt_frame);
    let context_attachment_estimated_tokens =
        estimate_prompt_blocks_tokens(&prompt_frame.context_blocks);
    let effective_budget_estimated_tokens = prompt_budget
        .saturating_sub(tool_overhead_estimated_tokens)
        .saturating_sub(CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS);
    let estimated_baseline_tokens =
        system_prompt_estimated_tokens.saturating_add(context_attachment_estimated_tokens);

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
    ) -> Result<TurnTerminalRecord> {
        let record = {
            let mut guard = self.inner.agent.lock().await;
            let record = TurnTerminalRecord {
                turn_index: guard.state.turn_index,
                kind,
                last_assistant_message,
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

    pub(super) async fn run_agent_loop(
        &self,
        agent_id: &str,
        trust: TrustLevel,
        effective_prompt: EffectivePrompt,
        loop_control: LoopControlOptions,
    ) -> Result<AgentLoopOutcome> {
        let mut completed_rounds = Vec::<TurnRoundRecord>::new();
        let turn_started_at = Instant::now();
        let mut should_sleep = false;
        let mut sleep_duration_ms = None;
        let mut round = 0usize;
        let mut truncated_text_history = Vec::new();
        let mut last_assistant_message: Option<String> = None;
        let mut max_output_recovery_count = 0usize;
        let mut checkpoint_state = TurnLocalCheckpointState::default();

        loop {
            round += 1;
            if let Some(max_tool_rounds) = loop_control.max_tool_rounds {
                if round > max_tool_rounds {
                    let final_text = format!(
                        "Stopped after reaching the maximum tool loop depth ({max_tool_rounds})."
                    );
                    self.persist_turn_terminal_record(
                        TurnTerminalKind::Aborted,
                        Some(final_text.clone()),
                        turn_started_at.elapsed().as_millis() as u64,
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

            let identity = self.agent_identity_view().await?;
            let available_tools = self.filtered_tool_specs(&identity)?;
            let allowed_tool_names = available_tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<HashSet<_>>();

            let (response, attempt_timeline, context_management) = if round == 1 {
                let request = build_provider_turn_request(&effective_prompt, available_tools);
                let provider = self.current_provider().await;
                let context_management = context_management_diagnostic(provider.as_ref(), &request);
                match provider.complete_turn_with_diagnostics(request).await {
                    Ok((response, attempt_timeline)) => {
                        (response, attempt_timeline, context_management)
                    }
                    Err(err) => {
                        if let Some(outcome) = self
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
                        self.persist_turn_terminal_record(
                            TurnTerminalKind::Aborted,
                            last_assistant_message.clone(),
                            turn_started_at.elapsed().as_millis() as u64,
                        )
                        .await?;
                        return Err(err);
                    }
                }
            } else {
                let context_config = self.current_context_config().await;
                let turn_index = {
                    let guard = self.inner.agent.lock().await;
                    guard.state.turn_index
                };
                let checkpoint_request_id =
                    Some(format!("turn-{turn_index}-round-{round}-checkpoint"));
                let prompt_frame = build_provider_prompt_frame(&effective_prompt);
                let projection = match build_turn_local_projection(
                    &prompt_frame,
                    &completed_rounds,
                    &available_tools,
                    &checkpoint_state,
                    checkpoint_request_id,
                    context_config.prompt_budget_estimated_tokens,
                    context_config.compaction_keep_recent_estimated_tokens,
                ) {
                    TurnLocalProjectionOutcome::Projection(projection) => projection,
                    TurnLocalProjectionOutcome::BaselineOverBudget(diagnostics) => {
                        self.inner.storage.append_event(&AuditEvent::new(
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
                        self.persist_turn_terminal_record(
                            TurnTerminalKind::BaselineOverBudget,
                            Some(final_text.clone()),
                            turn_started_at.elapsed().as_millis() as u64,
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
                    self.inner.storage.append_event(&AuditEvent::new(
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
                        self.inner.storage.append_event(&AuditEvent::new(
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
                let provider = self.current_provider().await;
                let context_management = context_management_diagnostic(provider.as_ref(), &request);
                match provider.complete_turn_with_diagnostics(request).await {
                    Ok((response, attempt_timeline)) => {
                        (response, attempt_timeline, context_management)
                    }
                    Err(err) => {
                        if let Some(outcome) = self
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
                        self.persist_turn_terminal_record(
                            TurnTerminalKind::Aborted,
                            last_assistant_message.clone(),
                            turn_started_at.elapsed().as_millis() as u64,
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

            {
                let mut guard = self.inner.agent.lock().await;
                guard.state.total_input_tokens += response.input_tokens;
                guard.state.total_output_tokens += response.output_tokens;
                guard.state.total_model_rounds += 1;
                guard.state.last_turn_token_usage = Some(TokenUsage::new(
                    response.input_tokens,
                    response.output_tokens,
                ));
                guard.state.last_requested_model = model_attempt_state.requested_model.clone();
                guard.state.last_active_model = model_attempt_state.active_model.clone();
                self.inner.storage.write_agent(&guard.state)?;
            }

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

            self.inner.storage.append_event(&AuditEvent::new(
                "provider_round_completed",
                serde_json::json!({
                    "agent_id": agent_id,
                    "round": round,
                    "stop_reason": stop_reason,
                    "input_tokens": response.input_tokens,
                    "output_tokens": response.output_tokens,
                    "token_usage": token_usage,
                    "tool_call_count": tool_calls.len(),
                    "tool_names": tool_calls.iter().map(|call| call.name.clone()).collect::<Vec<_>>(),
                    "text_block_count": text_blocks.len(),
                    "text_char_count": combined_text.chars().count(),
                    "text_preview": if combined_text.is_empty() {
                        None::<String>
                    } else {
                        Some(truncate_preview(&combined_text, ROUND_TEXT_PREVIEW_LIMIT))
                    },
                    "only_sleep_tools": only_sleep_tools,
                    "provider_cache_usage": cache_usage,
                    "prompt_cache_key": effective_prompt.cache_identity.prompt_cache_key.clone(),
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
                if checkpoint_recorded {
                    checkpoint_state.latest = Some(TurnLocalCheckpointRecord {
                        request_id: pending_checkpoint.request_id.clone(),
                        requested_at_round: pending_checkpoint.requested_at_round,
                        response_round: round,
                        mode: pending_checkpoint.mode,
                        text: checkpoint_text.clone(),
                        anchor_generation: pending_checkpoint.anchor_generation,
                    });
                }
                self.inner.storage.append_event(&AuditEvent::new(
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
            self.inner
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
                            "blocks": text_blocks.iter().map(|text| serde_json::json!({
                                "type": "text",
                                "text": text,
                            })).chain(tool_calls.iter().map(|call| serde_json::json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.name,
                                "input": call.input,
                            }))).collect::<Vec<_>>(),
                            "token_usage": token_usage,
                            "provider_cache_usage": cache_usage,
                            "prompt_cache_key": effective_prompt.cache_identity.prompt_cache_key.clone(),
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

            if tool_calls.is_empty() {
                self.inner.storage.append_event(&AuditEvent::new(
                    "text_only_round_observed",
                    serde_json::json!({
                        "agent_id": agent_id,
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
                    self.inner
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
                    self.inner.storage.append_event(&AuditEvent::new(
                        "max_output_tokens_recovery",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "attempt": max_output_recovery_count,
                        }),
                    ))?;
                    continue;
                }
            }

            if tool_calls.is_empty() {
                let final_text = last_assistant_message.clone().unwrap_or_default();
                self.persist_turn_terminal_record(
                    TurnTerminalKind::Completed,
                    last_assistant_message.clone(),
                    turn_started_at.elapsed().as_millis() as u64,
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
                    self.inner.storage.append_event(&AuditEvent::new(
                        "tool_execution_failed",
                        serde_json::json!({
                            "tool_call_id": tool_call_id,
                            "tool_name": tool_name,
                            "exec_command_cmd": command_preview_field(&call),
                            "exec_command_cost": command_cost_field(
                                &call,
                                self.inner.default_tool_output_tokens,
                                self.inner.max_tool_output_tokens
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
                    self.inner.storage.append_event(&AuditEvent::new(
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
                match self
                    .inner
                    .tools
                    .execute(self, agent_id, &trust, &call)
                    .await
                {
                    Ok((result, mut record)) => {
                        let result_content =
                            crate::tool::tools::render_tool_result_for_model(&result)?;
                        let duration_ms = record.duration_ms;
                        let (turn_index, work_item_id) = {
                            let guard = self.inner.agent.lock().await;
                            (
                                guard.state.turn_index,
                                guard.state.current_turn_work_item_id.clone(),
                            )
                        };
                        record.turn_index = turn_index;
                        if record.work_item_id.is_none() {
                            record.work_item_id = work_item_id;
                        }

                        if result.should_sleep {
                            should_sleep = true;
                            sleep_duration_ms = result.sleep_duration_ms;
                        }
                        self.inner.storage.append_tool_execution(&record)?;
                        if matches!(record.status, crate::types::ToolExecutionStatus::Success) {
                            self.record_skill_tool_activation(&record.tool_name, &record.input)
                                .await?;
                        }

                        self.inner.storage.append_event(&AuditEvent::new(
                            "tool_executed",
                            serde_json::json!({
                                "tool_call_id": tool_call_id,
                                "tool_name": tool_name,
                                "exec_command_cmd": command_preview_field(&call),
                                "exec_command_cost": command_cost_field(
                                    &call,
                                    self.inner.default_tool_output_tokens,
                                    self.inner.max_tool_output_tokens
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
                        let error = ToolError::from_anyhow(&err);
                        let message = error.render();
                        self.inner.storage.append_event(&AuditEvent::new(
                            "tool_execution_failed",
                            serde_json::json!({
                                "tool_call_id": tool_call_id,
                                "tool_name": tool_name,
                                "exec_command_cmd": command_preview_field(&call),
                                "exec_command_cost": command_cost_field(
                                    &call,
                                    self.inner.default_tool_output_tokens,
                                    self.inner.max_tool_output_tokens
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
            self.inner
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
            let round_record = TurnRoundRecord {
                round,
                estimated_tokens: build_round_estimated_tokens(
                    &completed_round_assistant_blocks,
                    &tool_results,
                    &[],
                ),
                assistant_blocks: completed_round_assistant_blocks,
                text_blocks,
                tool_calls: round_tool_calls,
                tool_results,
                tool_result_envelopes,
                follow_up_user_texts: Vec::new(),
            };
            if round_changes_checkpoint_anchor(&round_record) {
                checkpoint_state.anchor_generation =
                    checkpoint_state.anchor_generation.saturating_add(1);
            }
            completed_rounds.push(round_record);

            if only_sleep_tools {
                let final_text = last_assistant_message.clone().unwrap_or_default();
                self.persist_turn_terminal_record(
                    TurnTerminalKind::Completed,
                    last_assistant_message.clone(),
                    turn_started_at.elapsed().as_millis() as u64,
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

fn command_preview_field(call: &ToolCall) -> Option<String> {
    (call.name == "ExecCommand")
        .then(|| call.input.get("cmd").and_then(Value::as_str))
        .flatten()
        .map(command_preview)
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

    fn checkpoint_state_with_latest(
        text: &str,
        response_round: usize,
        anchor_generation: u64,
    ) -> TurnLocalCheckpointState {
        TurnLocalCheckpointState {
            latest: Some(TurnLocalCheckpointRecord {
                request_id: format!("req-{response_round}"),
                requested_at_round: response_round,
                response_round,
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
    fn round_changes_checkpoint_anchor_for_mutation_and_verifier_tools_only() {
        assert!(round_changes_checkpoint_anchor(&fixture_round_with_tool(
            1,
            "patch",
            "ApplyPatch",
            serde_json::json!({ "patch": "--- a/app.txt\n+++ b/app.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n" }),
        )));
        assert!(round_changes_checkpoint_anchor(&fixture_round_with_tool(
            2,
            "plan",
            "UpdateWorkItem",
            serde_json::json!({ "work_item_id": "item-1", "plan": [] }),
        )));
        assert!(round_changes_checkpoint_anchor(&fixture_round_with_tool(
            3,
            "complete work item",
            "CompleteWorkItem",
            serde_json::json!({ "work_item_id": "item-1" }),
        )));
        assert!(round_changes_checkpoint_anchor(&fixture_round_with_tool(
            4,
            "verify",
            "ExecCommand",
            serde_json::json!({ "cmd": "cargo test --test runtime_compaction" }),
        )));
        assert!(!round_changes_checkpoint_anchor(&fixture_round_with_tool(
            5,
            "read",
            "ExecCommand",
            serde_json::json!({ "cmd": "sed -n '1,40p' src/runtime/turn.rs" }),
        )));
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
