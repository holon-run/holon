#[path = "support/runtime_waiting.rs"]
mod runtime_waiting;

mod support;

macro_rules! runtime_async_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                runtime_waiting::$name().await
            }
        )*
    };
}

#[test]
fn policy_blocks_mismatched_origin() {
    runtime_waiting::policy_blocks_mismatched_origin();
}

runtime_async_tests!(
    message_processing_creates_briefs_and_sleeps,
    terminal_brief_uses_last_assistant_message_without_terminal_delivery_round,
    sleep_only_completion_keeps_last_assistant_message_from_previous_round,
    sleep_only_completion_preserves_brief_after_max_output_recovery,
    update_work_item_creates_and_updates_persisted_snapshot,
    update_work_plan_replaces_latest_snapshot_for_existing_work_item,
    multi_session_state_is_isolated,
    agent_summary_last_turn_token_usage_survives_transcript_windowing,
);
