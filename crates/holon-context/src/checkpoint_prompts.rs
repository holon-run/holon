//! Turn-local compaction and resume checkpoint prompt templates.
//!
//! Centralizing these prompts in `holon-context` keeps all model-visible runtime
//! instructions auditable and snapshot-governed alongside `L3a`/`L3b` prompt assets.

pub const COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT: &str = "\
[Runtime-generated full progress checkpoint request]
You are crossing a context compaction boundary. Produce a concise runtime-private progress checkpoint for continuation in your next assistant message.
This checkpoint is internal continuity state and has not been delivered to the operator. Do not treat it as a status update or later repeat its headings or metadata to the operator unless explicitly asked.
Best effort: write the checkpoint in the current target response language inferred from the trusted prompt and context. Language mismatch must not prevent checkpoint creation or continuation.

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

pub const DELTA_CHECKPOINT_PREVIEW_LIMIT: usize = 1_200;

pub const CHECKPOINT_RESUME_PROMPT: &str = "\
[Runtime-generated checkpoint continuation]
Continue from the checkpoint's next goal-aligned action now. The preceding checkpoint is runtime-private continuity state and has not been delivered to the operator. Do not restate its headings, metadata, or internal process unless the operator explicitly asks about it. If it contains conclusions that answer the operator's request and those conclusions have not appeared in an operator-visible response in this turn, synthesize them into the next formal operator-facing answer. Do not claim that checkpoint content was already delivered. If the checkpoint says enough evidence exists to act, call the concrete mutation, verification, or delivery tool next; otherwise run only the named bounded command/query.";

pub fn truncate_preview(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let mut preview = trimmed.chars().take(limit).collect::<String>();
    if trimmed.chars().count() > limit {
        preview.push_str("...");
    }
    preview
}

pub fn build_delta_checkpoint_prompt(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_prompts_preserve_runtime_private_boundary() {
        assert!(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT
            .contains("internal continuity state and has not been delivered to the operator"));
        assert!(CHECKPOINT_RESUME_PROMPT.contains(
            "runtime-private continuity state and has not been delivered to the operator"
        ));

        let delta = build_delta_checkpoint_prompt(Some(3), Some(1), "Confirmed A; next inspect B.");
        assert!(delta.contains("Base checkpoint round: 3"));
        assert!(delta.contains("Confirmed A; next inspect B."));
        assert!(delta.contains("runtime-private continuity state"));
    }

    #[test]
    fn test_truncate_preview_caps_oversized_text() {
        let oversized = "x".repeat(DELTA_CHECKPOINT_PREVIEW_LIMIT + 10);
        let preview = truncate_preview(&oversized, DELTA_CHECKPOINT_PREVIEW_LIMIT);
        assert_eq!(preview.chars().count(), DELTA_CHECKPOINT_PREVIEW_LIMIT + 3);
        assert!(preview.ends_with("..."));
    }
}
