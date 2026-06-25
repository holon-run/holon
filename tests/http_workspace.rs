mod support;

macro_rules! http_async_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() -> anyhow::Result<()> {
                support::http_workspace::$name().await
            }
        )+
    };
}

http_async_tests!(
    workspace_enter_control_route_is_not_exposed,
    detach_workspace_route_removes_stale_non_active_binding,
    detach_workspace_route_rejects_active_binding,
    worktree_summary_route_returns_reviewable_candidate_summary,
    workspace_files_lists_directory,
    workspace_files_reads_text_file,
    workspace_files_path_traversal_rejected,
    workspace_files_returns_404_for_missing_file,
    workspace_files_metadata_only,
    workspace_files_unknown_workspace_404,
);
