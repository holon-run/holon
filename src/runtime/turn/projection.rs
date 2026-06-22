//! Token estimation, context projection, and compaction logic.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::config::ModelRef;
use crate::provider::{
    ConversationMessage, ModelBlock, PromptContentBlock, ProviderAttemptTimeline,
    ProviderPromptFrame, ToolResultBlock,
};
use crate::tool::ToolSpec;

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
    pub(super) compacted_rounds: usize,
    pub(super) exact_tail_rounds: usize,
    pub(super) projected_estimated_tokens: usize,
    pub(super) effective_budget_estimated_tokens: usize,
    pub(super) tool_overhead_estimated_tokens: usize,
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
        ModelBlock::ToolUse { id, name, input } => {
            estimate_text_tokens(id) + estimate_text_tokens(name) + estimate_json_tokens(input)
        }
        ModelBlock::Thinking { text, .. } => estimate_text_tokens(text),
        ModelBlock::RedactedThinking { data } => estimate_text_tokens(data),
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
    pub(super) requested_model: Option<ModelRef>,
    pub(super) active_model: Option<ModelRef>,
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

    let available_chars = available_tokens.saturating_mul(4);
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

pub(super) fn build_turn_local_projection_with_runtime_reminder(
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
        // Try degrading the last round before giving up.
        if let Some(last_round) = rounds.last() {
            let degraded_available_tokens =
                effective_budget_estimated_tokens.saturating_sub(estimated_baseline_tokens);
            let mut degraded_conversation = vec![ConversationMessage::UserBlocks(
                prompt_frame.context_blocks.clone(),
            )];
            push_runtime_reminder_message(&mut degraded_conversation, runtime_reminder);
            let (degraded_messages, _trimmed) =
                degraded_round_messages(last_round, degraded_available_tokens);
            degraded_conversation.extend(degraded_messages);
            let degraded_projection_estimated_tokens =
                estimate_projection_tokens(prompt_frame, &degraded_conversation);
            if degraded_projection_estimated_tokens <= effective_budget_estimated_tokens {
                return TurnLocalProjectionOutcome::Projection(TurnLocalProjection {
                    conversation: degraded_conversation,
                    compaction: Some(TurnLocalCompactionStats {
                        compacted_rounds: rounds.len().saturating_sub(1),
                        exact_tail_rounds: 1,
                        projected_estimated_tokens: degraded_projection_estimated_tokens,
                        effective_budget_estimated_tokens,
                        tool_overhead_estimated_tokens,
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
                    last_round_degraded: false,
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
            last_round_degraded: false,
        }),
    })
}
