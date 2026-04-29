#[path = "support/runtime_subagents.rs"]
mod runtime_subagents;

mod support;

macro_rules! runtime_async_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                runtime_subagents::$name().await
            }
        )*
    };
}

runtime_async_tests!(
    task_output_returns_subagent_result_text,
    subagent_task_updates_parent_state_and_child_summary_during_lifecycle,
    subagent_task_status_exposes_live_and_terminal_child_observability,
    blocking_subagent_result_does_not_regress_to_running_task_status,
    subagent_task_failure_propagates_failed_output_to_parent,
    multiple_subagent_tasks_do_not_cross_contaminate_outputs,
    subagent_task_returns_result_to_parent_session,
);
