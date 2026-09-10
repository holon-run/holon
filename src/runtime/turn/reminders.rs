//! Runtime reminder and checkpoint helpers.

use crate::tool::names as tn;

use crate::provider::ConversationMessage;
use crate::tool::spec::{ToolResultEnvelope, ToolResultStatus};

use super::truncate_preview;
use super::{TurnRoundRecord, DELTA_CHECKPOINT_PREVIEW_LIMIT};

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
This delta is runtime-private continuity state, not operator-facing prose. Do not later repeat its headings or metadata to the operator unless explicitly asked.
Best effort: write the delta in the current target response language inferred from the trusted prompt and context. Language mismatch must not prevent checkpoint creation or continuation.

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
