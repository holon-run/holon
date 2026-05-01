use super::*;
use super::support::*;
    #[test]
    fn openai_max_output_tokens_stop_reason_triggers_recovery() {
        assert!(is_max_output_stop_reason(Some("max_output_tokens")));
    }

    #[tokio::test]
    async fn detached_host_runtime_starts_in_agent_home_workspace() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let snapshot = runtime.execution_snapshot().await.unwrap();

        assert_eq!(
            snapshot.workspace_id.as_deref(),
            Some(AGENT_HOME_WORKSPACE_ID)
        );
        assert_eq!(snapshot.workspace_anchor, runtime.agent_home());
        assert_eq!(snapshot.execution_root, runtime.agent_home());
    }

    #[tokio::test]
    async fn use_workspace_path_activates_project_workspace() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let workspace = tempdir().unwrap();

        crate::tool::tools::execute_builtin_tool(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "use-workspace".into(),
                name: "UseWorkspace".into(),
                input: serde_json::json!({
                    "path": workspace.path().display().to_string(),
                    "access_mode": "exclusive_write",
                }),
            },
        )
        .await
        .unwrap();
        let snapshot = runtime.execution_snapshot().await.unwrap();

        assert_ne!(
            snapshot.workspace_id.as_deref(),
            Some(AGENT_HOME_WORKSPACE_ID)
        );
        assert_eq!(snapshot.workspace_anchor, workspace.path());
        assert_eq!(snapshot.execution_root, workspace.path());
        assert_eq!(snapshot.cwd, workspace.path());
    }

    #[tokio::test]
    async fn use_workspace_agent_home_returns_to_fallback_without_deleting_project() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let workspace = tempdir().unwrap();
        let retained_file = workspace.path().join("retained.txt");
        std::fs::write(&retained_file, "keep").unwrap();

        crate::tool::tools::execute_builtin_tool(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "use-project".into(),
                name: "UseWorkspace".into(),
                input: serde_json::json!({ "path": workspace.path().display().to_string() }),
            },
        )
        .await
        .unwrap();
        crate::tool::tools::execute_builtin_tool(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "use-home".into(),
                name: "UseWorkspace".into(),
                input: serde_json::json!({ "workspace_id": AGENT_HOME_WORKSPACE_ID }),
            },
        )
        .await
        .unwrap();
        let snapshot = runtime.execution_snapshot().await.unwrap();

        assert_eq!(
            snapshot.workspace_id.as_deref(),
            Some(AGENT_HOME_WORKSPACE_ID)
        );
        assert_eq!(snapshot.execution_root, runtime.agent_home());
        assert!(retained_file.is_file());
    }

    #[test]
    fn execution_snapshot_includes_attached_workspaces() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();

        // Add workspace entries to storage
        let entry1 = crate::types::WorkspaceEntry::new(
            String::from("ws-boot"),
            workspace.path().to_path_buf(),
            None,
        );
        runtime
            .inner
            .storage
            .append_workspace_entry(&entry1)
            .unwrap();

        let workspace2 = tempdir().unwrap();
        let entry2 = crate::types::WorkspaceEntry::new(
            String::from("ws-second"),
            workspace2.path().to_path_buf(),
            None,
        );
        runtime
            .inner
            .storage
            .append_workspace_entry(&entry2)
            .unwrap();

        // Create a state with multiple attached workspaces
        let mut state = crate::types::AgentState::new("default");
        state.attached_workspaces = vec!["ws-boot".into(), "ws-second".into()];
        state.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
            workspace_id: "ws-second".into(),
            workspace_anchor: workspace2.path().to_path_buf(),
            execution_root_id: "canonical_root:ws-second".into(),
            execution_root: workspace2.path().to_path_buf(),
            projection_kind: WorkspaceProjectionKind::CanonicalRoot,
            access_mode: WorkspaceAccessMode::ExclusiveWrite,
            cwd: workspace2.path().to_path_buf(),
            occupancy_id: None,
            projection_metadata: None,
        });
        state.execution_profile = ExecutionProfile::default();

        // Build the execution snapshot
        let workspace_view = runtime.workspace_view_from_state(&state).unwrap();
        let snapshot = runtime.execution_snapshot_for_view(
            state.execution_profile.clone(),
            &workspace_view,
            &state.attached_workspaces,
        );

        // Verify that attached_workspaces includes both workspaces
        assert_eq!(snapshot.attached_workspaces.len(), 2);
        assert_eq!(snapshot.attached_workspaces[0].0, "ws-second");
        assert_eq!(snapshot.attached_workspaces[0].1, workspace2.path());
        assert_eq!(snapshot.attached_workspaces[1].0, "ws-boot");
        assert_eq!(snapshot.attached_workspaces[1].1, workspace.path());
    }
