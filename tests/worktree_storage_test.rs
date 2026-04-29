#[cfg(test)]
mod worktree_storage_tests {
    use holon::storage::AppStorage;
    use holon::types::{AgentState, AgentStatus, TokenUsage, WorktreeSession};
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn test_storage_round_trip_session_with_worktree() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut session = AgentState::new("test-worktree-session");
        session.status = AgentStatus::AwakeRunning;
        session.worktree_session = Some(WorktreeSession {
            original_cwd: PathBuf::from("/original/repo"),
            original_branch: "main".to_string(),
            worktree_path: PathBuf::from("/original/repo/.git/worktrees/feature-x"),
            worktree_branch: "feature-x".to_string(),
        });
        session.last_turn_token_usage = Some(TokenUsage::new(100, 50));

        storage.write_agent(&session).unwrap();

        let restored = storage.read_agent().unwrap().unwrap();

        assert_eq!(restored.id, "test-worktree-session");
        assert_eq!(restored.status, AgentStatus::AwakeRunning);
        assert!(restored.worktree_session.is_some());

        let wt = restored.worktree_session.as_ref().unwrap();
        assert_eq!(wt.original_cwd, PathBuf::from("/original/repo"));
        assert_eq!(wt.original_branch, "main");
        assert_eq!(
            wt.worktree_path,
            PathBuf::from("/original/repo/.git/worktrees/feature-x")
        );
        assert_eq!(wt.worktree_branch, "feature-x");
        assert_eq!(
            restored.last_turn_token_usage,
            Some(TokenUsage::new(100, 50))
        );
    }

    #[test]
    fn test_storage_round_trip_session_without_worktree() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let session = AgentState::new("normal-session");
        storage.write_agent(&session).unwrap();

        let restored = storage.read_agent().unwrap().unwrap();
        assert_eq!(restored.id, "normal-session");
        assert!(restored.worktree_session.is_none());
    }
}
