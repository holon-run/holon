use super::super::*;
use super::support::*;

#[test]
fn openai_max_output_tokens_stop_reason_triggers_recovery() {
    assert!(is_max_output_stop_reason(Some("max_output_tokens")));
}

#[tokio::test]
async fn detached_host_runtime_starts_in_agent_home_workspace() {
    let (_home, host, runtime) = host_backed_test_runtime().await;
    let snapshot = runtime.execution_snapshot().await.unwrap();
    let default_agent_id = host.config().default_agent_id.as_str();
    let expected_workspace_id = crate::types::agent_home_workspace_id(default_agent_id);

    assert_eq!(
        snapshot.workspace_id.as_deref(),
        Some(expected_workspace_id.as_str())
    );
    assert_ne!(expected_workspace_id, AGENT_HOME_WORKSPACE_ID);
    assert_eq!(snapshot.workspace_anchor, runtime.agent_home());
    assert_eq!(snapshot.execution_root, runtime.agent_home());
    assert_eq!(
        snapshot.execution_root_id.as_deref(),
        Some(format!("canonical_root:{expected_workspace_id}").as_str())
    );
}

#[tokio::test]
async fn use_workspace_path_activates_project_workspace() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let workspace = tempdir().unwrap();

    crate::tool::tools::execute_builtin_tool(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-workspace".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({
                "path": workspace.path().display().to_string(),
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
    let (_home, host, runtime) = host_backed_test_runtime().await;
    let default_agent_id = host.config().default_agent_id.as_str();
    let workspace = tempdir().unwrap();
    let retained_file = workspace.path().join("retained.txt");
    std::fs::write(&retained_file, "keep").unwrap();

    crate::tool::tools::execute_builtin_tool(
        &runtime,
        default_agent_id,
        &AuthorityClass::OperatorInstruction,
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
        default_agent_id,
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-home".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({ "workspace_id": AGENT_HOME_WORKSPACE_ID }),
        },
    )
    .await
    .unwrap();
    let snapshot = runtime.execution_snapshot().await.unwrap();
    let expected_workspace_id = crate::types::agent_home_workspace_id(default_agent_id);

    assert_eq!(
        snapshot.workspace_id.as_deref(),
        Some(expected_workspace_id.as_str())
    );
    assert_eq!(snapshot.execution_root, runtime.agent_home());
    assert!(retained_file.is_file());
}

#[tokio::test]
async fn agent_home_workspace_ids_are_unique_per_agent_while_alias_remains_local() {
    let (_home, host, default_runtime) = host_backed_test_runtime().await;
    let default_agent_id = host.config().default_agent_id.as_str();
    host.create_named_agent("worker", None).await.unwrap();
    let worker_runtime = host.get_or_create_agent("worker").await.unwrap();

    let default_snapshot = default_runtime.execution_snapshot().await.unwrap();
    let worker_snapshot = worker_runtime.execution_snapshot().await.unwrap();

    assert_eq!(
        default_snapshot.workspace_id.as_deref(),
        Some(crate::types::agent_home_workspace_id(default_agent_id).as_str())
    );
    assert_eq!(
        worker_snapshot.workspace_id.as_deref(),
        Some(crate::types::agent_home_workspace_id("worker").as_str())
    );
    assert_ne!(default_snapshot.workspace_id, worker_snapshot.workspace_id);
    assert_ne!(
        default_snapshot.execution_root_id,
        worker_snapshot.execution_root_id
    );

    crate::tool::tools::execute_builtin_tool(
        &worker_runtime,
        "worker",
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-worker-home".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({ "workspace_id": AGENT_HOME_WORKSPACE_ID }),
        },
    )
    .await
    .unwrap();
    let worker_snapshot = worker_runtime.execution_snapshot().await.unwrap();
    assert_eq!(
        worker_snapshot.workspace_id.as_deref(),
        Some(crate::types::agent_home_workspace_id("worker").as_str())
    );
    assert_eq!(
        worker_snapshot.workspace_anchor,
        worker_runtime.agent_home()
    );
}

#[tokio::test]
async fn runtime_startup_migrates_legacy_agent_home_attachment_alias() {
    let dir = tempdir().unwrap();
    let agent_id = "default";
    let canonical_agent_home_id = crate::types::agent_home_workspace_id(agent_id);
    let storage = crate::storage::AppStorage::new_for_agent(dir.path(), agent_id).unwrap();
    let preserved_cwd = dir.path().join("notes");
    std::fs::create_dir_all(&preserved_cwd).unwrap();
    let mut state = crate::types::AgentState::new(agent_id);
    state.attached_workspaces = vec![
        AGENT_HOME_WORKSPACE_ID.to_string(),
        canonical_agent_home_id.clone(),
    ];
    state.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
        workspace_id: AGENT_HOME_WORKSPACE_ID.to_string(),
        workspace_anchor: dir.path().to_path_buf(),
        execution_root_id: "canonical_root:agent_home".into(),
        execution_root: dir.path().to_path_buf(),
        projection_kind: WorkspaceProjectionKind::CanonicalRoot,
        access_mode: WorkspaceAccessMode::ExclusiveWrite,
        cwd: preserved_cwd.clone(),
        occupancy_id: None,
        projection_metadata: None,
    });
    storage.write_agent(&state).unwrap();

    let runtime = RuntimeHandle::new(
        agent_id,
        dir.path().to_path_buf(),
        InitialWorkspaceBinding::Detached,
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        agent_id.into(),
        context_config(),
    )
    .unwrap();
    let state = runtime.agent_state().await.unwrap();

    assert_eq!(
        state.attached_workspaces,
        vec![canonical_agent_home_id.clone()]
    );
    assert_eq!(
        state
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.workspace_id.as_str()),
        Some(canonical_agent_home_id.as_str())
    );
    assert_eq!(
        state
            .active_workspace_entry
            .as_ref()
            .map(|entry| &entry.cwd),
        Some(&preserved_cwd)
    );
    assert!(runtime
        .all_events()
        .unwrap()
        .iter()
        .any(|event| event.kind == "agent_home_workspace_bindings_migrated"));
}

#[tokio::test]
async fn detach_workspace_allows_only_redundant_legacy_agent_home_alias() {
    let (_home, host, runtime) = host_backed_test_runtime().await;
    let agent_id = host.config().default_agent_id.as_str();
    let canonical_agent_home_id = crate::types::agent_home_workspace_id(agent_id);
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.attached_workspaces = vec![
            AGENT_HOME_WORKSPACE_ID.to_string(),
            canonical_agent_home_id.clone(),
        ];
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    runtime
        .detach_workspace(AGENT_HOME_WORKSPACE_ID)
        .await
        .unwrap();
    let state = runtime.agent_state().await.unwrap();

    assert_eq!(state.attached_workspaces, vec![canonical_agent_home_id]);
    let err = runtime
        .detach_workspace(AGENT_HOME_WORKSPACE_ID)
        .await
        .expect_err("effective AgentHome should remain protected");
    assert!(err.to_string().contains("AgentHome cannot be detached"));
}

#[tokio::test]
async fn detach_workspace_rejects_canonical_agent_home_when_inactive() {
    let (_home, host, runtime) = host_backed_test_runtime().await;
    let agent_id = host.config().default_agent_id.as_str();
    let canonical_agent_home_id = crate::types::agent_home_workspace_id(agent_id);
    let project = tempdir().unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.attached_workspaces =
            vec![canonical_agent_home_id.clone(), "ws-project".to_string()];
        guard.state.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
            workspace_id: "ws-project".into(),
            workspace_anchor: project.path().to_path_buf(),
            execution_root_id: "canonical_root:ws-project".into(),
            execution_root: project.path().to_path_buf(),
            projection_kind: WorkspaceProjectionKind::CanonicalRoot,
            access_mode: WorkspaceAccessMode::ExclusiveWrite,
            cwd: project.path().to_path_buf(),
            occupancy_id: None,
            projection_metadata: None,
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let err = runtime
        .detach_workspace(&canonical_agent_home_id)
        .await
        .expect_err("canonical AgentHome should remain protected when inactive");
    assert!(err.to_string().contains("AgentHome cannot be detached"));
}

#[tokio::test]
async fn use_workspace_rejects_nonexistent_path() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let nonexistent = runtime.agent_home().join("__holon_test_nonexistent_dir__");
    // Ensure the path truly does not exist.
    if nonexistent.try_exists().unwrap_or(false) {
        std::fs::remove_dir_all(&nonexistent).unwrap();
    }

    let result = crate::tool::tools::execute_builtin_tool(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-workspace".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({
                "path": nonexistent.display().to_string(),
            }),
        },
    )
    .await;

    // Must fail with an appropriate error.
    assert_invalid_workspace_path_error(&result.unwrap_err(), "path does not exist", &nonexistent);
}

fn assert_invalid_workspace_path_error(
    error: &anyhow::Error,
    expected_validation_error: &str,
    expected_path: &std::path::Path,
) {
    let tool_error = crate::tool::ToolError::from_anyhow(error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let details = tool_error.details.as_ref().unwrap().as_object().unwrap();
    assert_eq!(details["field"].as_str(), Some("path"));
    assert_eq!(
        details["validation_error"].as_str(),
        Some(expected_validation_error)
    );
    let expected_path = expected_path.display().to_string();
    assert_eq!(details["path"].as_str(), Some(expected_path.as_str()));
    assert!(tool_error
        .recovery_hint
        .as_deref()
        .unwrap_or_default()
        .contains("existing directory"));
}

#[tokio::test]
async fn use_workspace_nonexistent_path_preserves_existing_workspace() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;

    // Establish an initial valid workspace.
    let workspace = tempdir().unwrap();
    crate::tool::tools::execute_builtin_tool(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-valid".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({
                "path": workspace.path().display().to_string(),
            }),
        },
    )
    .await
    .unwrap();

    // Now attempt to switch to a nonexistent path.
    let nonexistent = runtime.agent_home().join("__holon_test_nonexistent_dir2__");
    if nonexistent.try_exists().unwrap_or(false) {
        std::fs::remove_dir_all(&nonexistent).unwrap();
    }

    let result = crate::tool::tools::execute_builtin_tool(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-bad".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({
                "path": nonexistent.display().to_string(),
            }),
        },
    )
    .await;
    assert_invalid_workspace_path_error(&result.unwrap_err(), "path does not exist", &nonexistent);

    // The existing valid workspace must still be active.
    let snapshot = runtime.execution_snapshot().await.unwrap();
    assert_eq!(snapshot.workspace_anchor, workspace.path());
    assert_eq!(snapshot.execution_root, workspace.path());

    // Verify the nonexistent path was never registered as an attached workspace.
    let nonexistent_display = nonexistent.display().to_string();
    assert!(
        !snapshot
            .attached_workspaces
            .iter()
            .any(|(_, p)| p.display().to_string() == nonexistent_display),
        "nonexistent path must not appear in attached_workspaces"
    );
}

#[tokio::test]
async fn use_workspace_regular_file_preserves_existing_workspace() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;

    // Establish an initial valid workspace.
    let workspace = tempdir().unwrap();
    crate::tool::tools::execute_builtin_tool(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-valid".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({
                "path": workspace.path().display().to_string(),
            }),
        },
    )
    .await
    .unwrap();

    let file_dir = tempdir().unwrap();
    let file_path = file_dir.path().join("regular.txt");
    std::fs::write(&file_path, "not a directory").unwrap();

    let result = crate::tool::tools::execute_builtin_tool(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &crate::tool::ToolCall {
            id: "use-file".into(),
            name: "UseWorkspace".into(),
            input: serde_json::json!({
                "path": file_path.display().to_string(),
            }),
        },
    )
    .await;
    assert_invalid_workspace_path_error(
        &result.unwrap_err(),
        "path is not a directory",
        &file_path,
    );

    // The existing valid workspace must still be active.
    let snapshot = runtime.execution_snapshot().await.unwrap();
    assert_eq!(snapshot.workspace_anchor, workspace.path());
    assert_eq!(snapshot.execution_root, workspace.path());

    // Verify the regular file path was never registered as an attached workspace.
    let file_display = file_path.display().to_string();
    assert!(
        !snapshot
            .attached_workspaces
            .iter()
            .any(|(_, p)| p.display().to_string() == file_display),
        "regular file path must not appear in attached_workspaces"
    );
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

#[tokio::test]
async fn current_closure_returns_none_while_foreground_work_remains() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.pending = 1;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    assert!(runtime.current_closure().await.unwrap().is_none());

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.pending = 0;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let closure = runtime.current_closure().await.unwrap().unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Completed);
}

#[tokio::test]
async fn current_closure_returns_none_while_pending_wake_hint_remains() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.pending_wake_hint = Some(PendingWakeHint {
            reason: "wake".into(),
            description: None,
            scope: None,
            waiting_intent_id: None,
            external_trigger_id: None,
            source: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    assert!(runtime.current_closure().await.unwrap().is_none());

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.pending_wake_hint = None;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let closure = runtime.current_closure().await.unwrap().unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Completed);
}

#[tokio::test]
async fn wait_for_closure_blocks_until_foreground_work_clears() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.pending = 1;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let wait_runtime = runtime.clone();
    let waiter = tokio::spawn(async move { wait_runtime.wait_for_closure().await });

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(!waiter.is_finished());

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.pending = 0;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let closure = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Completed);
}
