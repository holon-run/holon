#[path = "support/runtime_waiting.rs"]
mod runtime_waiting;

mod support;

use std::future::Future;

fn run_on_large_stack<F, Fut>(name: &str, test: F) -> anyhow::Result<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + 'static,
{
    let test_thread = std::thread::Builder::new()
        .name(name.into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(test())
        })?;

    match test_thread.join() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

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
    tool_only_wait_for_persists_turn_without_result_brief,
    wait_for_with_assistant_text_persists_result_brief,
    queued_task_result_wait_settles_tool_only_turn_and_reenters_model,
    turn_execution_boundary_persists_queue_transcript_and_briefs,
    message_processing_creates_briefs_and_sleeps,
    terminal_brief_uses_last_assistant_message_without_terminal_delivery_round,
    sleep_only_completion_keeps_last_assistant_message_from_previous_round,
    sleep_only_completion_preserves_brief_after_max_output_recovery,
    update_work_item_creates_and_updates_persisted_snapshot,
    update_work_item_replaces_latest_plan_snapshot_for_existing_work_item,
    multi_session_state_is_isolated,
);

#[test]
fn agent_summary_last_turn_token_usage_survives_transcript_windowing() -> anyhow::Result<()> {
    run_on_large_stack(
        "agent_summary_last_turn_token_usage_survives_transcript_windowing",
        || runtime_waiting::agent_summary_last_turn_token_usage_survives_transcript_windowing(),
    )
}
