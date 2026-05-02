#[path = "support/runtime_compaction.rs"]
mod runtime_compaction;

mod support;

macro_rules! runtime_async_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                runtime_compaction::$name().await
            }
        )*
    };
}

runtime_async_tests!(
    preview_prompt_after_compaction_keeps_work_item_plan_and_pending_work_visible,
    task_result_rejoin_after_compaction_preserves_current_work_truth,
    contentful_wake_hint_after_compaction_keeps_active_work_truth,
    queued_notification_after_compaction_keeps_queued_work_visible,
    runtime_compaction_multi_pass_recovery_preserves_progress_and_artifacts,
);

#[tokio::test]
#[ignore = "slow stress-style compaction regression; run manually when tuning checkpoint timing"]
async fn repeated_turn_local_compaction_evolves_checkpoint_mode_and_keeps_latest_exact_tail(
) -> anyhow::Result<()> {
    runtime_compaction::repeated_turn_local_compaction_evolves_checkpoint_mode_and_keeps_latest_exact_tail()
        .await
}

#[tokio::test]
#[ignore = "slow stress-style compaction regression; run manually when tuning checkpoint timing"]
async fn max_output_recovery_followed_by_turn_local_compaction_preserves_progress_signal(
) -> anyhow::Result<()> {
    runtime_compaction::max_output_recovery_followed_by_turn_local_compaction_preserves_progress_signal()
        .await
}
