#[path = "support/runtime_workspace_worktree.rs"]
mod runtime_workspace_worktree;

mod support;

macro_rules! runtime_async_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                runtime_workspace_worktree::$name().await
            }
        )*
    };
}

runtime_async_tests!(
    task_output_returns_worktree_subagent_result_text,
    enter_worktree_tool_switches_workspace_and_restores_on_reload,
    enter_workspace_conflict_preserves_existing_occupancy,
    detach_workspace_persists_empty_binding_across_restart,
    enter_worktree_projection_honors_requested_cwd,
    exit_worktree_keep_restores_workspace_and_persists_state,
    exit_worktree_does_not_remove_clean_worktree,
    exit_worktree_does_not_remove_dirty_worktree,
    worktree_subagent_task_creates_dedicated_per_task_worktree,
    worktree_child_agent_task_records_workspace_mode,
    worktree_subagent_task_returns_metadata_to_parent_session,
    worktree_subagent_task_auto_removes_worktree_when_no_changes_wt104,
    worktree_subagent_task_retains_worktree_when_changes_detected_wt105,
);
