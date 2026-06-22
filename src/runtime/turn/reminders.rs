//! Work-item stale reminders and runtime reminder injection.

use std::env;

use crate::provider::{ConversationMessage, ProviderPromptFrame};
use crate::tool::{
    spec::{ToolResultEnvelope, ToolResultStatus},
    ToolSpec,
};
use crate::types::{TodoItemState, WorkItemPlanStatus, WorkItemRecord};

use super::projection::{
    estimate_prompt_blocks_tokens, estimate_prompt_frame_tokens, estimate_text_tokens,
    estimate_tool_specs_tokens,
};
use super::truncate_preview;
use super::{
    TurnRoundRecord, CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS, DELTA_CHECKPOINT_PREVIEW_LIMIT,
    WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS, WORK_ITEM_STALE_REMINDER_MAX_TOKENS,
    WORK_ITEM_STALE_REMINDER_PLAN_CHAR_LIMIT, WORK_ITEM_STALE_REMINDER_PLAN_LINE_LIMIT,
    WORK_ITEM_STALE_REMINDER_ROUNDS, WORK_ITEM_STALE_REMINDER_TODO_LIMIT,
};

pub(super) fn tool_result_invalidates_checkpoint_anchor(envelope: &ToolResultEnvelope) -> bool {
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

pub(super) fn round_invalidates_checkpoint_anchor(round: &TurnRoundRecord) -> bool {
    round
        .tool_result_envelopes
        .iter()
        .any(tool_result_invalidates_checkpoint_anchor)
}

pub(super) fn round_updated_work_item(round: &TurnRoundRecord) -> bool {
    round.tool_result_envelopes.iter().any(|envelope| {
        envelope.status == ToolResultStatus::Success
            && matches!(
                envelope.tool_name.as_str(),
                "CreateWorkItem" | "PickWorkItem" | "UpdateWorkItem" | "CompleteWorkItem"
            )
    })
}

pub(super) fn work_item_stale_reminder_rounds() -> usize {
    env_usize_or_default(
        "HOLON_WORK_ITEM_STALE_REMINDER_ROUNDS",
        WORK_ITEM_STALE_REMINDER_ROUNDS,
    )
}

pub(super) fn work_item_stale_reminder_cooldown_rounds() -> usize {
    env_usize_or_default(
        "HOLON_WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS",
        WORK_ITEM_STALE_REMINDER_COOLDOWN_ROUNDS,
    )
}

pub(super) fn work_item_stale_reminder_max_tokens() -> usize {
    env_usize_or_default(
        "HOLON_WORK_ITEM_STALE_REMINDER_MAX_TOKENS",
        WORK_ITEM_STALE_REMINDER_MAX_TOKENS,
    )
}

pub(super) fn env_usize_or_default(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub(super) fn build_work_item_stale_reminder(
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
    if let Some(plan) = work_item
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.preview.as_str())
        .filter(|preview| !preview.trim().is_empty())
    {
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

pub(super) fn maybe_reset_work_item_stale_reminder_cooldown(
    rounds_since_work_item_reminder: &mut usize,
    reminder_injected: bool,
) {
    if reminder_injected {
        *rounds_since_work_item_reminder = 0;
    }
}

pub(super) fn truncate_reminder_to_token_budget(reminder: &str, max_tokens: usize) -> String {
    if estimate_text_tokens(reminder) <= max_tokens {
        return reminder.to_string();
    }
    let max_chars = max_tokens.saturating_mul(4).max(256);
    format!(
        "{}\n... reminder truncated to fit token budget",
        truncate_preview(reminder, max_chars)
    )
}

pub(super) fn runtime_reminder_fits_baseline(
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

pub(super) fn work_item_plan_status_label(status: WorkItemPlanStatus) -> &'static str {
    match status {
        WorkItemPlanStatus::Draft => "draft",
        WorkItemPlanStatus::Ready => "ready",
        WorkItemPlanStatus::NeedsInput => "needs_input",
    }
}

pub(super) fn todo_item_state_label(state: TodoItemState) -> &'static str {
    match state {
        TodoItemState::Pending => "pending",
        TodoItemState::InProgress => "in_progress",
        TodoItemState::Completed => "completed",
    }
}

pub(super) fn build_delta_checkpoint_prompt(
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

pub(super) fn push_runtime_reminder_message(
    conversation: &mut Vec<ConversationMessage>,
    runtime_reminder: Option<&str>,
) {
    if let Some(reminder) = runtime_reminder.filter(|text| !text.trim().is_empty()) {
        conversation.push(ConversationMessage::UserText(reminder.to_string()));
    }
}
