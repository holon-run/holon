//! Runtime reminder and checkpoint helpers.

use crate::tool::names as tn;

use crate::provider::ConversationMessage;
use crate::tool::spec::{ToolResultEnvelope, ToolResultStatus};

use super::TurnRoundRecord;
pub(super) use holon_context::checkpoint_prompts::build_delta_checkpoint_prompt;

pub(super) fn tool_result_invalidates_checkpoint_anchor(envelope: &ToolResultEnvelope) -> bool {
    envelope.status == ToolResultStatus::Success
        && matches!(
            envelope.tool_name.as_str(),
            tn::CREATE_WORK_ITEM
                | tn::PICK_WORK_ITEM
                | tn::UPDATE_WORK_ITEM
                | tn::COMPLETE_WORK_ITEM
                | tn::APPLY_PATCH
        )
}

pub(super) fn round_invalidates_checkpoint_anchor(round: &TurnRoundRecord) -> bool {
    round
        .tool_result_envelopes
        .iter()
        .any(tool_result_invalidates_checkpoint_anchor)
}

/// Build a turn budget warning injected when the agent is on the last
/// allowed turn of a run. This is a cooperative hint, not enforcement;
/// the scheduling layer enforces max_turns independently.
pub(super) fn build_turn_budget_warning(max_turns: u64, turns_elapsed: u64) -> String {
    [
        "[Runtime-generated turn budget warning]".to_string(),
        format!(
            "Turn budget: this is turn {turns_elapsed} of {max_turns} maximum turns for this run."
        ),
        "Please wrap up your work now: deliver results, write summaries, and complete any open work items. After the last allowed turn, the runtime will stop scheduling new turns.".to_string(),
    ]
    .join("
")
}

/// Time snapshots are immutable request input, not regeneratable reminders.
pub(super) fn push_inference_time_message(
    conversation: &mut Vec<ConversationMessage>,
    inference_time: Option<&str>,
) {
    if let Some(time) = inference_time {
        crate::runtime::provider_turn::append_inference_time(conversation, time);
    }
}

pub(super) fn push_runtime_reminder_message(
    conversation: &mut Vec<ConversationMessage>,
    runtime_reminder: Option<&str>,
) {
    if let Some(reminder) = runtime_reminder.filter(|text| !text.trim().is_empty()) {
        conversation.push(ConversationMessage::UserText(reminder.to_string()));
    }
}
