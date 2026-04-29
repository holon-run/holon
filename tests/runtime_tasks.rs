#[path = "support/runtime_tasks.rs"]
mod runtime_tasks;

mod support;

macro_rules! runtime_async_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                runtime_tasks::$name().await
            }
        )*
    };
}

runtime_async_tests!(
    background_task_rejoins_main_session,
    stop_task_cancels_running_background_task,
    tool_use_round_trip_executes_and_returns_result,
    file_tools_can_modify_workspace_and_reenter_context,
    shell_tools_capture_command_output,
    shell_tools_truncate_large_output_before_provider_reinjection,
    exec_command_reports_nonzero_exit_and_truncates_output,
    exec_command_batch_returns_grouped_item_results,
    exec_command_batch_stop_on_error_skips_later_items,
    exec_command_workdir_violation_returns_structured_error,
    exec_command_spawn_failure_returns_shell_recovery_hint,
    tool_schema_and_dispatch_errors_are_recorded_without_corrupting_runtime_state,
    runtime_provider_failure_surfaces_failure_brief_and_transcript_entry,
    runtime_failure_brief_sanitizes_long_provider_error_but_transcript_keeps_full_error,
    command_task_runs_to_completion_and_persists_detail,
    task_output_returns_completed_command_task_output,
    task_output_non_blocking_reports_running_command_task,
    task_output_waits_for_command_task_completion,
    task_input_delivers_stdin_to_managed_command_task,
    task_output_times_out_for_long_running_task,
    task_output_prefers_terminal_task_record_over_stale_task_message,
    task_output_accepts_terminal_command_snapshot_without_explicit_readiness_flag,
    task_output_accepts_terminal_command_without_snapshot_fields,
    task_output_rejects_message_only_terminal_status_for_running_command,
    command_task_stop_cancels_running_command,
    background_command_task_persists_terminal_state_while_runtime_paused,
    command_task_result_is_canonical_follow_up_on_completion,
    blocking_command_task_sets_awaiting_task_closure,
    command_task_runner_failure_marks_task_failed_and_cleans_up,
    command_task_nonzero_exit_produces_failed_output_and_runtime_state,
    exec_command_auto_promotes_long_running_command_task,
);
