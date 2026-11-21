#[cfg(test)]
mod worktree_session_tests {
    use crate::types::{AgentState, AgentStatus, WorktreeSession};
    use std::path::PathBuf;

    #[test]
    fn test_worktree_session_serialization() {
        let worktree = WorktreeSession {
            original_cwd: PathBuf::from("/original/path"),
            original_branch: "main".to_string(),
            worktree_path: PathBuf::from("/worktree/path"),
            worktree_branch: "feature-branch".to_string(),
        };

        let json = serde_json::to_string(&worktree).unwrap();
        let restored: WorktreeSession = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.original_cwd, PathBuf::from("/original/path"));
        assert_eq!(restored.original_branch, "main");
        assert_eq!(restored.worktree_path, PathBuf::from("/worktree/path"));
        assert_eq!(restored.worktree_branch, "feature-branch");
    }

    #[test]
    fn test_session_state_with_worktree_serialization() {
        let mut session = AgentState::new("test-session");
        session.status = AgentStatus::AwakeRunning;
        session.worktree_session = Some(WorktreeSession {
            original_cwd: PathBuf::from("/repo"),
            original_branch: "main".to_string(),
            worktree_path: PathBuf::from("/repo/.git/worktrees/feature-1"),
            worktree_branch: "feature-1".to_string(),
        });

        let json = serde_json::to_string_pretty(&session).unwrap();
        let restored: AgentState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.id, "test-session");
        assert_eq!(restored.status, AgentStatus::AwakeRunning);
        assert!(restored.worktree_session.is_some());
        let wt = restored.worktree_session.unwrap();
        assert_eq!(wt.original_cwd, PathBuf::from("/repo"));
        assert_eq!(wt.original_branch, "main");
        assert_eq!(
            wt.worktree_path,
            PathBuf::from("/repo/.git/worktrees/feature-1")
        );
        assert_eq!(wt.worktree_branch, "feature-1");
    }

    #[test]
    fn test_session_state_without_worktree_defaults_to_none() {
        let session = AgentState::new("default-session");
        let json = serde_json::to_string(&session).unwrap();
        let restored: AgentState = serde_json::from_str(&json).unwrap();

        assert!(restored.worktree_session.is_none());
    }
}
