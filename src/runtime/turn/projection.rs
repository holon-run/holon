//! Token estimation, context projection, and compaction logic.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::config::ModelRouteRef;
use crate::provider::{
    ConversationMessage, ModelBlock, PromptContentBlock, ProviderAttemptTimeline,
    ProviderPromptFrame, ToolResultBlock,
};
use crate::tool::{
    spec::{ToolResultEnvelope, ToolResultStatus},
    ToolSpec,
};

use super::checkpoint::TurnLocalCheckpointMode;
use super::checkpoint::{TurnLocalCheckpointRequest, TurnLocalCheckpointState};
use super::reminders::{build_delta_checkpoint_prompt, push_runtime_reminder_message};
use super::tool_summary::build_compacted_round_recap;
use super::{
    TurnRoundRecord, COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT,
    CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS, DEGRADED_ROUND_MINIMUM_CONTENT_CHARS,
    DEGRADED_ROUND_PROVENANCE_MARKER, MIN_EXACT_TAIL_ROUNDS,
};

#[derive(Debug, Clone)]
pub(super) struct TurnLocalCompactionStats {
    pub(super) trigger_reason: &'static str,
    pub(super) compacted_rounds: usize,
    pub(super) exact_tail_rounds: usize,
    pub(super) degraded_rounds: usize,
    pub(super) pre_compaction_estimated_tokens: usize,
    pub(super) projected_estimated_tokens: usize,
    pub(super) prompt_budget_estimated_tokens: usize,
    pub(super) compaction_trigger_estimated_tokens: usize,
    pub(super) keep_recent_estimated_tokens: usize,
    pub(super) tool_output_budget_estimated_tokens: usize,
    pub(super) effective_budget_estimated_tokens: usize,
    pub(super) tool_overhead_estimated_tokens: usize,
    pub(super) compacted_tool_results: usize,
    pub(super) preserved_artifact_refs: usize,
    pub(super) trigger_budget_fallback_applied: bool,
    pub(super) strict_fallback_applied: bool,
    pub(super) checkpoint_request_id: Option<String>,
    pub(super) checkpoint_mode: Option<TurnLocalCheckpointMode>,
    pub(super) checkpoint_anchor_generation: Option<u64>,
    pub(super) checkpoint_base_round: Option<usize>,
    pub(super) previous_checkpoint_round: Option<usize>,
    pub(super) anchor_changed_since_checkpoint: bool,
    pub(super) last_round_degraded: bool,
}

#[derive(Debug, Clone)]
pub(super) struct TurnLocalProjection {
    pub(super) conversation: Vec<ConversationMessage>,
    pub(super) compaction: Option<TurnLocalCompactionStats>,
}

#[derive(Debug, Clone)]
pub(super) struct TurnLocalBaselineOverBudget {
    pub(super) reason: String,
    pub(super) estimated_baseline_tokens: usize,
    pub(super) minimum_exact_round_estimated_tokens: usize,
    pub(super) minimum_projection_estimated_tokens: usize,
    pub(super) effective_budget_estimated_tokens: usize,
    pub(super) tool_overhead_estimated_tokens: usize,
    pub(super) system_prompt_estimated_tokens: usize,
    pub(super) context_attachment_estimated_tokens: usize,
}

#[derive(Debug, Clone)]
pub(super) enum TurnLocalProjectionOutcome {
    Projection(TurnLocalProjection),
    BaselineOverBudget(TurnLocalBaselineOverBudget),
}

pub(super) fn estimate_text_tokens(text: &str) -> usize {
    text.chars().count().saturating_add(3) / 4
}

pub(super) fn estimate_json_tokens(value: &Value) -> usize {
    estimate_text_tokens(&serde_json::to_string(value).unwrap_or_default())
}

pub(super) fn estimate_model_block_tokens(block: &ModelBlock) -> usize {
    match block {
        ModelBlock::Text { text } => estimate_text_tokens(text),
        ModelBlock::ToolUse {
            id, name, input, ..
        } => estimate_text_tokens(id) + estimate_text_tokens(name) + estimate_json_tokens(input),
        ModelBlock::Thinking { text, .. } => estimate_text_tokens(text),
        ModelBlock::ReasoningText { text } => estimate_text_tokens(text),
        ModelBlock::RedactedThinking { data } => estimate_text_tokens(data),
        ModelBlock::Citations { citations } => citations
            .iter()
            .map(|citation| {
                estimate_text_tokens(&citation.url)
                    + citation
                        .title
                        .as_deref()
                        .map(estimate_text_tokens)
                        .unwrap_or(0)
            })
            .sum(),
    }
}

pub(super) fn estimate_tool_result_block_tokens(block: &ToolResultBlock) -> usize {
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

pub(crate) fn build_round_estimated_tokens(
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

pub(super) fn estimate_round_tokens(round: &TurnRoundRecord) -> usize {
    round.estimated_tokens
}

pub(crate) fn estimate_tool_specs_tokens(available_tools: &[ToolSpec]) -> usize {
    available_tools
        .iter()
        .map(|tool| {
            estimate_text_tokens(&tool.name)
                .saturating_add(estimate_text_tokens(&tool.description))
                .saturating_add(estimate_json_tokens(&tool.input_schema))
        })
        .sum()
}

pub(super) fn estimate_prompt_blocks_tokens(blocks: &[PromptContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| estimate_text_tokens(&block.text))
        .sum()
}

pub(super) fn estimate_prompt_frame_tokens(prompt_frame: &ProviderPromptFrame) -> usize {
    let structured_tokens = estimate_prompt_blocks_tokens(&prompt_frame.system_blocks);
    if structured_tokens == 0 {
        estimate_text_tokens(&prompt_frame.system_prompt)
    } else {
        structured_tokens
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ProviderAttemptModelState {
    pub(super) requested_model: Option<ModelRouteRef>,
    pub(super) active_model: Option<ModelRouteRef>,
    pub(super) fallback_active: bool,
}

pub(super) fn normalize_provider_attempt_timing(
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

pub(super) fn provider_attempt_model_state(
    timeline: Option<&ProviderAttemptTimeline>,
) -> ProviderAttemptModelState {
    let Some(timeline) = timeline else {
        return ProviderAttemptModelState::default();
    };
    let requested_model = (!timeline.requested_model_ref.is_empty())
        .then(|| ModelRouteRef::parse_compatible(&timeline.requested_model_ref).ok())
        .flatten();
    let active_model = timeline
        .active_model_ref
        .as_deref()
        .or(timeline.winning_model_ref.as_deref())
        .and_then(|model| ModelRouteRef::parse_compatible(model).ok());
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

pub(super) fn estimate_projection_tokens(
    prompt_frame: &ProviderPromptFrame,
    conversation: &[ConversationMessage],
) -> usize {
    let mut total = estimate_prompt_frame_tokens(prompt_frame);
    for message in conversation {
        total = total.saturating_add(match message {
            ConversationMessage::UserText(text) => estimate_text_tokens(text),
            ConversationMessage::UserBlocks(blocks) => estimate_prompt_blocks_tokens(blocks),
            ConversationMessage::UserImage {
                prompt,
                data_base64,
                ..
            } => estimate_text_tokens(prompt).saturating_add(data_base64.len() / 4),
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

pub(super) fn exact_round_messages(round: &TurnRoundRecord) -> Vec<ConversationMessage> {
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

#[derive(Debug, Clone, Default)]
pub(super) struct ToolResultProjectionStats {
    pub(super) compacted_tool_results: usize,
    pub(super) preserved_artifact_refs: usize,
}

const RECOVERABLE_MAX_DEPTH: usize = 4;
const RECOVERABLE_MAX_ARRAY_ITEMS: usize = 8;
const RECOVERABLE_MAX_NODES: usize = 64;
const RECOVERABLE_MAX_STRING_CHARS: usize = 256;
const COMPACT_SUMMARY_MAX_CHARS: usize = 256;
const COMPACT_ERROR_MAX_CHARS: usize = 256;

struct RecoverableValueBudget {
    remaining_nodes: usize,
}

fn truncate_receipt_text(value: &str, max_chars: usize) -> String {
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

fn bounded_recoverable_scalar(value: &Value) -> Value {
    match value {
        Value::String(value) => {
            Value::String(truncate_receipt_text(value, RECOVERABLE_MAX_STRING_CHARS))
        }
        _ => value.clone(),
    }
}

fn recoverable_result_value_at(
    value: &Value,
    depth: usize,
    budget: &mut RecoverableValueBudget,
) -> Option<Value> {
    if depth > RECOVERABLE_MAX_DEPTH || budget.remaining_nodes == 0 {
        return None;
    }
    match value {
        Value::Object(map) => {
            let mut recovered = serde_json::Map::new();
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort_by_key(|key| {
                if key.ends_with("_ref") || key.as_str() == "path" {
                    0
                } else if key.ends_with("_id") {
                    1
                } else if key.as_str() == "status" {
                    2
                } else {
                    3
                }
            });
            for key in keys {
                if budget.remaining_nodes == 0 {
                    break;
                }
                let nested = &map[key];
                let keep_scalar = key == "path"
                    || key == "status"
                    || key == "truncated"
                    || key == "content_truncated"
                    || key == "output_truncated"
                    || key == "initial_output_truncated"
                    || key.ends_with("_id")
                    || key.ends_with("_ref");
                let keep_nested = key == "artifacts"
                    || key == "task_handle"
                    || key == "work_item"
                    || key.ends_with("_refs");
                if keep_scalar {
                    budget.remaining_nodes = budget.remaining_nodes.saturating_sub(1);
                    recovered.insert(key.clone(), bounded_recoverable_scalar(nested));
                } else if keep_nested {
                    if let Some(value) = recoverable_result_value_at(nested, depth + 1, budget) {
                        recovered.insert(key.clone(), value);
                    }
                } else if let Some(value) = recoverable_result_value_at(nested, depth + 1, budget) {
                    if !matches!(&value, Value::Object(map) if map.is_empty())
                        && !matches!(&value, Value::Array(values) if values.is_empty())
                    {
                        recovered.insert(key.clone(), value);
                    }
                }
            }
            Some(Value::Object(recovered))
        }
        Value::Array(values) => {
            let items = values
                .iter()
                .take(RECOVERABLE_MAX_ARRAY_ITEMS)
                .filter_map(|value| recoverable_result_value_at(value, depth + 1, budget))
                .collect::<Vec<_>>();
            Some(serde_json::json!({
                "total": values.len(),
                "returned": items.len(),
                "truncated": items.len() < values.len(),
                "items": items,
            }))
        }
        _ => None,
    }
}

fn recoverable_result_value(value: &Value) -> Option<Value> {
    recoverable_result_value_at(
        value,
        0,
        &mut RecoverableValueBudget {
            remaining_nodes: RECOVERABLE_MAX_NODES,
        },
    )
}

fn count_artifact_refs_at(value: &Value, allow_path: bool) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                usize::from(
                    ((allow_path && key == "path") || key.ends_with("_ref"))
                        && value.as_str().is_some_and(|value| !value.is_empty()),
                ) + count_artifact_refs_at(
                    value,
                    key == "artifacts" || (allow_path && key == "items"),
                )
            })
            .sum(),
        Value::Array(values) => values
            .iter()
            .map(|value| count_artifact_refs_at(value, allow_path))
            .sum(),
        _ => 0,
    }
}

fn count_artifact_refs(value: &Value) -> usize {
    count_artifact_refs_at(value, true)
}

fn find_output_ref(value: &Value) -> Option<&str> {
    match value {
        Value::Object(map) => map
            .get("output_ref")
            .and_then(Value::as_str)
            .or_else(|| map.values().find_map(find_output_ref)),
        Value::Array(values) => values.iter().find_map(find_output_ref),
        _ => None,
    }
}

fn compact_tool_result_envelope(
    envelope: &ToolResultEnvelope,
    budget_estimated_tokens: usize,
) -> Option<(String, usize)> {
    let recovered_result = envelope.result.as_ref().and_then(recoverable_result_value);
    let preserved_artifact_refs = recovered_result
        .as_ref()
        .map(count_artifact_refs)
        .unwrap_or_default();
    let mut receipt = serde_json::Map::new();
    receipt.insert(
        "tool_name".into(),
        Value::String(envelope.tool_name.clone()),
    );
    receipt.insert(
        "status".into(),
        serde_json::to_value(&envelope.status).unwrap_or(Value::Null),
    );
    if let Some(output_ref) = envelope.result.as_ref().and_then(find_output_ref) {
        receipt.insert(
            "output_ref".into(),
            Value::String(truncate_receipt_text(
                output_ref,
                RECOVERABLE_MAX_STRING_CHARS,
            )),
        );
    }
    receipt.insert(
        "summary_text".into(),
        envelope
            .summary_text
            .as_deref()
            .map(|value| truncate_receipt_text(value, COMPACT_SUMMARY_MAX_CHARS))
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    if let Some(error) = envelope.error.as_ref() {
        receipt.insert(
            "error".into(),
            serde_json::json!({
                "kind": truncate_receipt_text(&error.kind, COMPACT_ERROR_MAX_CHARS),
                "message": truncate_receipt_text(&error.message, COMPACT_ERROR_MAX_CHARS),
                "recovery_hint": error.recovery_hint.as_deref().map(|value| truncate_receipt_text(value, COMPACT_ERROR_MAX_CHARS)),
                "retryable": error.retryable,
            }),
        );
    }
    if let Some(result) = recovered_result {
        receipt.insert("result_refs".into(), result);
    }
    receipt.insert("provider_projection_truncated".into(), Value::Bool(true));
    let mut rendered = serde_json::to_string(&Value::Object(receipt.clone())).ok()?;
    if estimate_text_tokens(&rendered) <= budget_estimated_tokens {
        return Some((rendered, preserved_artifact_refs));
    }

    receipt.remove("result_refs");
    receipt.remove("error");
    rendered = serde_json::to_string(&Value::Object(receipt.clone())).ok()?;
    if estimate_text_tokens(&rendered) <= budget_estimated_tokens {
        return Some((rendered, 0));
    }

    receipt.remove("summary_text");
    rendered = serde_json::to_string(&Value::Object(receipt)).ok()?;
    (estimate_text_tokens(&rendered) <= budget_estimated_tokens).then_some((rendered, 0))
}

pub(super) fn compacted_round_messages(
    round: &TurnRoundRecord,
    tool_output_budget_estimated_tokens: usize,
) -> Option<(Vec<ConversationMessage>, ToolResultProjectionStats)> {
    let mut messages = vec![ConversationMessage::AssistantBlocks(
        round.assistant_blocks.clone(),
    )];
    let mut stats = ToolResultProjectionStats::default();
    if !round.tool_results.is_empty() {
        let mut projected_results = Vec::with_capacity(round.tool_results.len());
        for (index, result) in round.tool_results.iter().enumerate() {
            if estimate_tool_result_block_tokens(result) <= tool_output_budget_estimated_tokens {
                projected_results.push(result.clone());
                continue;
            }
            let envelope = round.tool_result_envelopes.get(index)?;
            let (content, preserved_artifact_refs) =
                compact_tool_result_envelope(envelope, tool_output_budget_estimated_tokens)?;
            projected_results.push(ToolResultBlock {
                tool_use_id: result.tool_use_id.clone(),
                content,
                is_error: matches!(envelope.status, ToolResultStatus::Error),
                error: envelope.error.clone(),
            });
            stats.compacted_tool_results = stats.compacted_tool_results.saturating_add(1);
            stats.preserved_artifact_refs = stats
                .preserved_artifact_refs
                .saturating_add(preserved_artifact_refs);
        }
        messages.push(ConversationMessage::UserToolResults(projected_results));
    }
    messages.extend(
        round
            .follow_up_user_texts
            .iter()
            .cloned()
            .map(ConversationMessage::UserText),
    );
    Some((messages, stats))
}

pub(super) fn degraded_round_messages(
    round: &TurnRoundRecord,
    available_tokens: usize,
) -> (Vec<ConversationMessage>, bool) {
    let trimmable_count = round
        .assistant_blocks
        .iter()
        .filter(|block| matches!(block, ModelBlock::Text { .. }))
        .count()
        + round.tool_results.len();

    if trimmable_count == 0 {
        return (exact_round_messages(round), false);
    }

    // Estimate token cost of non-trimmable blocks (ToolUse, Thinking,
    // RedactedThinking) and subtract from the budget so they don't silently
    // consume the per-item char allocation reserved for trimmable content.
    // Without this deduction, I/O-heavy rounds with large tool calls would
    // exceed the intended degradation budget.
    let non_trimmable_estimated_tokens: usize = round
        .assistant_blocks
        .iter()
        .filter(|block| !matches!(block, ModelBlock::Text { .. }))
        .map(estimate_model_block_tokens)
        .sum();
    let trimmable_available_tokens =
        available_tokens.saturating_sub(non_trimmable_estimated_tokens);
    let available_chars = trimmable_available_tokens.saturating_mul(4);
    let per_item_char_limit =
        (available_chars / trimmable_count).max(DEGRADED_ROUND_MINIMUM_CONTENT_CHARS);

    let mut trimmed = false;
    let mut messages = Vec::new();

    let mut degraded_assistant = Vec::with_capacity(round.assistant_blocks.len());
    for block in &round.assistant_blocks {
        match block {
            ModelBlock::Text { text } => {
                let char_count = text.chars().count();
                if char_count > per_item_char_limit {
                    let original_tokens = estimate_text_tokens(text);
                    let truncated: String = text.chars().take(per_item_char_limit).collect();
                    degraded_assistant.push(ModelBlock::Text {
                        text: format!(
                            "[runtime: assistant text trimmed from ~{original_tokens} tokens]\n{truncated}"
                        ),
                    });
                    trimmed = true;
                } else {
                    degraded_assistant.push(block.clone());
                }
            }
            other => degraded_assistant.push(other.clone()),
        }
    }
    messages.push(ConversationMessage::AssistantBlocks(degraded_assistant));

    if !round.tool_results.is_empty() {
        let mut degraded_results = Vec::with_capacity(round.tool_results.len());
        for result in &round.tool_results {
            let char_count = result.content.chars().count();
            if char_count > per_item_char_limit {
                let original_tokens = estimate_text_tokens(&result.content);
                let truncated: String = result.content.chars().take(per_item_char_limit).collect();
                degraded_results.push(ToolResultBlock {
                    tool_use_id: result.tool_use_id.clone(),
                    content: format!(
                        "[runtime: tool output trimmed from ~{original_tokens} tokens]\n{truncated}"
                    ),
                    is_error: result.is_error,
                    error: result.error.clone(),
                });
                trimmed = true;
            } else {
                degraded_results.push(result.clone());
            }
        }
        messages.push(ConversationMessage::UserToolResults(degraded_results));
    }

    if trimmed {
        messages.insert(
            0,
            ConversationMessage::UserText(DEGRADED_ROUND_PROVENANCE_MARKER.to_string()),
        );
    }

    messages.extend(
        round
            .follow_up_user_texts
            .iter()
            .cloned()
            .map(ConversationMessage::UserText),
    );

    (messages, trimmed)
}

/// Check if two rounds contain identical tool calls (same name + input, ignoring id).
fn rounds_have_identical_tool_calls(a: &TurnRoundRecord, b: &TurnRoundRecord) -> bool {
    let extract = |round: &TurnRoundRecord| -> Vec<(String, serde_json::Value)> {
        round
            .assistant_blocks
            .iter()
            .filter_map(|block| match block {
                ModelBlock::ToolUse { name, input, .. } => Some((name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    };
    let a_tools = extract(a);
    let b_tools = extract(b);
    !a_tools.is_empty() && a_tools == b_tools
}

/// Fold consecutive identical tool-call rounds, keeping only the last one
/// in each group and replacing earlier rounds with a summary marker.
/// Returns a projection outcome if folding brings the conversation under budget.
fn fold_repeated_tool_call_rounds(
    prompt_frame: &ProviderPromptFrame,
    runtime_reminder: Option<&str>,
    rounds: &[TurnRoundRecord],
    pre_compaction_estimated_tokens: usize,
    prompt_budget_estimated_tokens: usize,
    compaction_trigger_estimated_tokens: usize,
    keep_recent_estimated_tokens: usize,
    tool_output_budget_estimated_tokens: usize,
    effective_budget_estimated_tokens: usize,
    tool_overhead_estimated_tokens: usize,
    trigger_budget_fallback_applied: bool,
) -> Option<TurnLocalProjectionOutcome> {
    // Detect if there are any consecutive identical rounds to fold.
    let has_repeats = rounds
        .windows(2)
        .any(|w| rounds_have_identical_tool_calls(&w[0], &w[1]));
    if !has_repeats {
        return None;
    }

    let mut conversation = vec![ConversationMessage::UserBlocks(
        prompt_frame.context_blocks.clone(),
    )];
    push_runtime_reminder_message(&mut conversation, runtime_reminder);

    let mut folded_count = 0usize;
    let mut skip_count = 0usize;
    let mut tool_stats = ToolResultProjectionStats::default();
    for (i, round) in rounds.iter().enumerate() {
        if i + 1 < rounds.len() && rounds_have_identical_tool_calls(round, &rounds[i + 1]) {
            skip_count += 1;
            folded_count += 1;
            continue;
        }
        if skip_count > 0 {
            conversation.push(ConversationMessage::UserText(format!(
                "[runtime: {skip_count} repeated identical tool call round(s) omitted]"
            )));
            skip_count = 0;
        }
        let (messages, round_stats) =
            compacted_round_messages(round, tool_output_budget_estimated_tokens)?;
        tool_stats.compacted_tool_results = tool_stats
            .compacted_tool_results
            .saturating_add(round_stats.compacted_tool_results);
        tool_stats.preserved_artifact_refs = tool_stats
            .preserved_artifact_refs
            .saturating_add(round_stats.preserved_artifact_refs);
        conversation.extend(messages);
    }
    if skip_count > 0 {
        conversation.push(ConversationMessage::UserText(format!(
            "[runtime: {skip_count} repeated identical tool call round(s) omitted]"
        )));
    }

    let projected_estimated_tokens = estimate_projection_tokens(prompt_frame, &conversation);
    if projected_estimated_tokens > effective_budget_estimated_tokens {
        return None; // Folding wasn't enough; fall through to more aggressive compaction.
    }

    Some(TurnLocalProjectionOutcome::Projection(
        TurnLocalProjection {
            conversation,
            compaction: Some(TurnLocalCompactionStats {
                trigger_reason: "estimated_tokens_exceeded_trigger",
                compacted_rounds: folded_count,
                exact_tail_rounds: rounds.len().saturating_sub(folded_count),
                degraded_rounds: 0,
                pre_compaction_estimated_tokens,
                projected_estimated_tokens,
                prompt_budget_estimated_tokens,
                compaction_trigger_estimated_tokens,
                keep_recent_estimated_tokens,
                tool_output_budget_estimated_tokens,
                effective_budget_estimated_tokens,
                tool_overhead_estimated_tokens,
                compacted_tool_results: tool_stats.compacted_tool_results,
                preserved_artifact_refs: tool_stats.preserved_artifact_refs,
                trigger_budget_fallback_applied,
                strict_fallback_applied: false,
                checkpoint_request_id: None,
                checkpoint_mode: None,
                checkpoint_anchor_generation: None,
                checkpoint_base_round: None,
                previous_checkpoint_round: None,
                anchor_changed_since_checkpoint: false,
                last_round_degraded: false,
            }),
        },
    ))
}

pub(super) fn select_exact_tail_start(
    rounds: &[TurnRoundRecord],
    keep_recent_budget: usize,
) -> usize {
    if rounds.len() <= MIN_EXACT_TAIL_ROUNDS {
        return 0;
    }

    // Check if the newest round alone exceeds the budget
    let newest_round_tokens = estimate_round_tokens(rounds.last().unwrap());
    if newest_round_tokens > keep_recent_budget {
        // When the newest round is oversized, ensure we keep at least MIN_EXACT_TAIL_ROUNDS
        return rounds.len().saturating_sub(MIN_EXACT_TAIL_ROUNDS);
    }

    // Otherwise, respect the budget exactly
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

pub(super) fn build_turn_local_checkpoint_request(
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
pub(super) fn build_turn_local_projection(
    prompt_frame: &ProviderPromptFrame,
    rounds: &[TurnRoundRecord],
    available_tools: &[ToolSpec],
    checkpoint_state: &TurnLocalCheckpointState,
    checkpoint_request_id: Option<String>,
    request_prompt_budget: usize,
    keep_recent_budget: usize,
) -> TurnLocalProjectionOutcome {
    build_turn_local_projection_with_runtime_reminder(
        prompt_frame,
        rounds,
        available_tools,
        checkpoint_state,
        checkpoint_request_id,
        request_prompt_budget,
        request_prompt_budget,
        keep_recent_budget,
        usize::MAX,
        None,
    )
}

pub(super) fn build_turn_local_projection_with_runtime_reminder(
    prompt_frame: &ProviderPromptFrame,
    rounds: &[TurnRoundRecord],
    available_tools: &[ToolSpec],
    checkpoint_state: &TurnLocalCheckpointState,
    checkpoint_request_id: Option<String>,
    request_prompt_budget: usize,
    compaction_trigger_budget: usize,
    keep_recent_budget: usize,
    tool_output_budget: usize,
    runtime_reminder: Option<&str>,
) -> TurnLocalProjectionOutcome {
    let tool_overhead_estimated_tokens = estimate_tool_specs_tokens(available_tools);
    let system_prompt_estimated_tokens = estimate_prompt_frame_tokens(prompt_frame);
    let context_attachment_estimated_tokens =
        estimate_prompt_blocks_tokens(&prompt_frame.context_blocks);
    let runtime_reminder_estimated_tokens = runtime_reminder
        .map(estimate_text_tokens)
        .unwrap_or_default();
    let hard_effective_budget_estimated_tokens = request_prompt_budget
        .saturating_sub(tool_overhead_estimated_tokens)
        .saturating_sub(CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS);
    let trigger_effective_budget_estimated_tokens = compaction_trigger_budget
        .saturating_sub(tool_overhead_estimated_tokens)
        .saturating_sub(CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS)
        .min(hard_effective_budget_estimated_tokens);
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
                effective_budget_estimated_tokens: hard_effective_budget_estimated_tokens,
                tool_overhead_estimated_tokens,
                system_prompt_estimated_tokens,
                context_attachment_estimated_tokens,
            })
        };

    let mut exact_conversation = vec![ConversationMessage::UserBlocks(
        prompt_frame.context_blocks.clone(),
    )];
    push_runtime_reminder_message(&mut exact_conversation, runtime_reminder);
    for round in rounds {
        exact_conversation.extend(exact_round_messages(round));
    }

    let exact_estimated_tokens = estimate_projection_tokens(prompt_frame, &exact_conversation);
    let mut tool_bounded_conversation = vec![ConversationMessage::UserBlocks(
        prompt_frame.context_blocks.clone(),
    )];
    push_runtime_reminder_message(&mut tool_bounded_conversation, runtime_reminder);
    let mut tool_bounded_stats = ToolResultProjectionStats::default();
    let mut tool_bounded_projection_available = true;
    for round in rounds {
        let Some((messages, round_stats)) = compacted_round_messages(round, tool_output_budget)
        else {
            tool_bounded_projection_available = false;
            break;
        };
        tool_bounded_stats.compacted_tool_results = tool_bounded_stats
            .compacted_tool_results
            .saturating_add(round_stats.compacted_tool_results);
        tool_bounded_stats.preserved_artifact_refs = tool_bounded_stats
            .preserved_artifact_refs
            .saturating_add(round_stats.preserved_artifact_refs);
        tool_bounded_conversation.extend(messages);
    }
    if tool_bounded_projection_available {
        let tool_bounded_estimated_tokens =
            estimate_projection_tokens(prompt_frame, &tool_bounded_conversation);
        if tool_bounded_estimated_tokens <= trigger_effective_budget_estimated_tokens {
            let compaction = (tool_bounded_stats.compacted_tool_results > 0).then_some(
                TurnLocalCompactionStats {
                    trigger_reason: "tool_output_budget_exceeded",
                    compacted_rounds: 0,
                    exact_tail_rounds: rounds.len(),
                    degraded_rounds: 0,
                    pre_compaction_estimated_tokens: exact_estimated_tokens,
                    projected_estimated_tokens: tool_bounded_estimated_tokens,
                    prompt_budget_estimated_tokens: request_prompt_budget,
                    compaction_trigger_estimated_tokens: compaction_trigger_budget,
                    keep_recent_estimated_tokens: keep_recent_budget,
                    tool_output_budget_estimated_tokens: tool_output_budget,
                    effective_budget_estimated_tokens: trigger_effective_budget_estimated_tokens,
                    tool_overhead_estimated_tokens,
                    compacted_tool_results: tool_bounded_stats.compacted_tool_results,
                    preserved_artifact_refs: tool_bounded_stats.preserved_artifact_refs,
                    trigger_budget_fallback_applied: false,
                    strict_fallback_applied: false,
                    checkpoint_request_id: None,
                    checkpoint_mode: None,
                    checkpoint_anchor_generation: None,
                    checkpoint_base_round: None,
                    previous_checkpoint_round: None,
                    anchor_changed_since_checkpoint: false,
                    last_round_degraded: false,
                },
            );
            return TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
                conversation: tool_bounded_conversation,
                compaction,
            });
        }
    }
    let trigger_budget_fallback_applied = trigger_effective_budget_estimated_tokens == 0;
    let effective_budget_estimated_tokens = if trigger_budget_fallback_applied {
        hard_effective_budget_estimated_tokens
    } else {
        trigger_effective_budget_estimated_tokens
    };

    // Try folding consecutive identical tool-call rounds before more aggressive compaction.
    let folded = fold_repeated_tool_call_rounds(
        prompt_frame,
        runtime_reminder,
        rounds,
        exact_estimated_tokens,
        request_prompt_budget,
        compaction_trigger_budget,
        keep_recent_budget,
        tool_output_budget,
        effective_budget_estimated_tokens,
        tool_overhead_estimated_tokens,
        trigger_budget_fallback_applied,
    );
    if let Some(outcome) = folded {
        return outcome;
    }

    // A round recap cannot reduce a single-round history. When the exact
    // projection still fits the hard request budget, preserve it rather than
    // issuing a checkpoint request with no compacted prefix.
    if rounds.len() < 2
        && tool_bounded_projection_available
        && exact_estimated_tokens <= hard_effective_budget_estimated_tokens
    {
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
    if minimum_projection_estimated_tokens > hard_effective_budget_estimated_tokens {
        // Prefer a recoverable projection from the canonical envelope before any
        // text degradation. If the compact receipt itself cannot fit, fail closed.
        if let Some(last_round) = rounds.last() {
            if let Some((compacted_messages, tool_stats)) =
                compacted_round_messages(last_round, tool_output_budget)
            {
                let mut compacted_conversation = vec![ConversationMessage::UserBlocks(
                    prompt_frame.context_blocks.clone(),
                )];
                push_runtime_reminder_message(&mut compacted_conversation, runtime_reminder);
                compacted_conversation.extend(compacted_messages);
                let compacted_projection_estimated_tokens =
                    estimate_projection_tokens(prompt_frame, &compacted_conversation);
                if tool_stats.compacted_tool_results > 0
                    && compacted_projection_estimated_tokens
                        <= hard_effective_budget_estimated_tokens
                {
                    return TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
                        conversation: compacted_conversation,
                        compaction: Some(TurnLocalCompactionStats {
                            trigger_reason: "estimated_tokens_exceeded_trigger",
                            compacted_rounds: rounds.len().saturating_sub(1),
                            exact_tail_rounds: 1,
                            degraded_rounds: 1,
                            pre_compaction_estimated_tokens: exact_estimated_tokens,
                            projected_estimated_tokens: compacted_projection_estimated_tokens,
                            prompt_budget_estimated_tokens: request_prompt_budget,
                            compaction_trigger_estimated_tokens: compaction_trigger_budget,
                            keep_recent_estimated_tokens: keep_recent_budget,
                            tool_output_budget_estimated_tokens: tool_output_budget,
                            effective_budget_estimated_tokens:
                                hard_effective_budget_estimated_tokens,
                            tool_overhead_estimated_tokens,
                            compacted_tool_results: tool_stats.compacted_tool_results,
                            preserved_artifact_refs: tool_stats.preserved_artifact_refs,
                            trigger_budget_fallback_applied,
                            strict_fallback_applied: true,
                            checkpoint_request_id: None,
                            checkpoint_mode: None,
                            checkpoint_anchor_generation: None,
                            checkpoint_base_round: None,
                            previous_checkpoint_round: None,
                            anchor_changed_since_checkpoint: false,
                            last_round_degraded: true,
                        }),
                    });
                }
                if tool_stats.compacted_tool_results > 0 {
                    return baseline_over_budget(
                        "minimum_compact_tool_receipt_unfit",
                        minimum_exact_round_estimated_tokens,
                        compacted_projection_estimated_tokens,
                    );
                }
            } else if last_round
                .tool_results
                .iter()
                .any(|result| estimate_tool_result_block_tokens(result) > tool_output_budget)
            {
                return baseline_over_budget(
                    "minimum_compact_tool_receipt_unfit",
                    minimum_exact_round_estimated_tokens,
                    minimum_projection_estimated_tokens,
                );
            }

            // Assistant text remains degradable when tool results are already bounded.
            let degraded_available_tokens =
                hard_effective_budget_estimated_tokens.saturating_sub(estimated_baseline_tokens);
            let mut degraded_conversation = vec![ConversationMessage::UserBlocks(
                prompt_frame.context_blocks.clone(),
            )];
            push_runtime_reminder_message(&mut degraded_conversation, runtime_reminder);
            let (degraded_messages, _trimmed) =
                degraded_round_messages(last_round, degraded_available_tokens);
            degraded_conversation.extend(degraded_messages);
            let degraded_projection_estimated_tokens =
                estimate_projection_tokens(prompt_frame, &degraded_conversation);
            if degraded_projection_estimated_tokens <= hard_effective_budget_estimated_tokens {
                return TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
                    conversation: degraded_conversation,
                    compaction: Some(TurnLocalCompactionStats {
                        trigger_reason: "estimated_tokens_exceeded_trigger",
                        compacted_rounds: rounds.len().saturating_sub(1),
                        exact_tail_rounds: 1,
                        degraded_rounds: 1,
                        pre_compaction_estimated_tokens: exact_estimated_tokens,
                        projected_estimated_tokens: degraded_projection_estimated_tokens,
                        prompt_budget_estimated_tokens: request_prompt_budget,
                        compaction_trigger_estimated_tokens: compaction_trigger_budget,
                        keep_recent_estimated_tokens: keep_recent_budget,
                        tool_output_budget_estimated_tokens: tool_output_budget,
                        effective_budget_estimated_tokens: hard_effective_budget_estimated_tokens,
                        tool_overhead_estimated_tokens,
                        compacted_tool_results: 0,
                        preserved_artifact_refs: 0,
                        trigger_budget_fallback_applied,
                        strict_fallback_applied: true,
                        checkpoint_request_id: None,
                        checkpoint_mode: None,
                        checkpoint_anchor_generation: None,
                        checkpoint_base_round: None,
                        previous_checkpoint_round: None,
                        anchor_changed_since_checkpoint: false,
                        last_round_degraded: true,
                    }),
                });
            }
        }
        return baseline_over_budget(
            "minimum_exact_round_unfit",
            minimum_exact_round_estimated_tokens,
            minimum_projection_estimated_tokens,
        );
    }

    let preferred_tail_start = select_exact_tail_start(rounds, keep_recent_budget).max(1);
    let minimum_tail_start = rounds.len().saturating_sub(1);

    'tail: for tail_start in preferred_tail_start..=minimum_tail_start {
        let mut conversation = vec![ConversationMessage::UserBlocks(
            prompt_frame.context_blocks.clone(),
        )];
        push_runtime_reminder_message(&mut conversation, runtime_reminder);
        let exact_tail = &rounds[tail_start..];
        let mut projected_exact_tail = Vec::new();
        let mut tool_stats = ToolResultProjectionStats::default();
        for round in exact_tail {
            let Some((messages, round_stats)) = compacted_round_messages(round, tool_output_budget)
            else {
                continue 'tail;
            };
            tool_stats.compacted_tool_results = tool_stats
                .compacted_tool_results
                .saturating_add(round_stats.compacted_tool_results);
            tool_stats.preserved_artifact_refs = tool_stats
                .preserved_artifact_refs
                .saturating_add(round_stats.preserved_artifact_refs);
            projected_exact_tail.extend(messages);
        }
        let exact_tail_tokens = projected_exact_tail
            .iter()
            .map(|message| match message {
                ConversationMessage::UserText(text) => estimate_text_tokens(text),
                ConversationMessage::UserBlocks(blocks) => estimate_prompt_blocks_tokens(blocks),
                ConversationMessage::AssistantBlocks(blocks) => {
                    blocks.iter().map(estimate_model_block_tokens).sum()
                }
                ConversationMessage::UserToolResults(results) => {
                    results.iter().map(estimate_tool_result_block_tokens).sum()
                }
                ConversationMessage::UserImage {
                    prompt,
                    data_base64,
                    ..
                } => estimate_text_tokens(prompt).saturating_add(data_base64.len() / 4),
            })
            .sum::<usize>();
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
        conversation.extend(projected_exact_tail);
        if let Some(checkpoint) = checkpoint_request.as_ref() {
            conversation.push(ConversationMessage::UserText(checkpoint.prompt.clone()));
        }

        let projected_estimated_tokens = estimate_projection_tokens(prompt_frame, &conversation);
        let strict_fallback_applied = tail_start > preferred_tail_start;
        if projected_estimated_tokens <= effective_budget_estimated_tokens {
            return TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
                conversation,
                compaction: Some(TurnLocalCompactionStats {
                    trigger_reason: "estimated_tokens_exceeded_trigger",
                    compacted_rounds: tail_start,
                    exact_tail_rounds: rounds.len().saturating_sub(tail_start),
                    degraded_rounds: 0,
                    pre_compaction_estimated_tokens: exact_estimated_tokens,
                    projected_estimated_tokens,
                    prompt_budget_estimated_tokens: request_prompt_budget,
                    compaction_trigger_estimated_tokens: compaction_trigger_budget,
                    keep_recent_estimated_tokens: keep_recent_budget,
                    tool_output_budget_estimated_tokens: tool_output_budget,
                    effective_budget_estimated_tokens,
                    tool_overhead_estimated_tokens,
                    compacted_tool_results: tool_stats.compacted_tool_results,
                    preserved_artifact_refs: tool_stats.preserved_artifact_refs,
                    trigger_budget_fallback_applied,
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
                    last_round_degraded: false,
                }),
            });
        }
    }

    if let Some(last_round) = rounds.last() {
        let oversized_tool_result = last_round
            .tool_results
            .iter()
            .any(|result| estimate_tool_result_block_tokens(result) > tool_output_budget);
        if oversized_tool_result
            && compacted_round_messages(last_round, tool_output_budget).is_none()
        {
            return baseline_over_budget(
                "minimum_compact_tool_receipt_unfit",
                minimum_exact_round_estimated_tokens,
                minimum_projection_estimated_tokens,
            );
        }
    }

    TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
        conversation: minimum_viable_conversation,
        compaction: Some(TurnLocalCompactionStats {
            trigger_reason: "estimated_tokens_exceeded_trigger",
            compacted_rounds: minimum_tail_start,
            exact_tail_rounds: rounds.len().saturating_sub(minimum_tail_start),
            degraded_rounds: 0,
            pre_compaction_estimated_tokens: exact_estimated_tokens,
            projected_estimated_tokens: minimum_projection_estimated_tokens,
            prompt_budget_estimated_tokens: request_prompt_budget,
            compaction_trigger_estimated_tokens: compaction_trigger_budget,
            keep_recent_estimated_tokens: keep_recent_budget,
            tool_output_budget_estimated_tokens: tool_output_budget,
            effective_budget_estimated_tokens,
            tool_overhead_estimated_tokens,
            compacted_tool_results: 0,
            preserved_artifact_refs: 0,
            trigger_budget_fallback_applied,
            strict_fallback_applied: minimum_tail_start > preferred_tail_start,
            checkpoint_request_id: None,
            checkpoint_mode: None,
            checkpoint_anchor_generation: None,
            checkpoint_base_round: None,
            previous_checkpoint_round: None,
            anchor_changed_since_checkpoint: false,
            last_round_degraded: false,
        }),
    })
}
