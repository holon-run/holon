use super::super::*;
use super::support::*;

#[tokio::test]
async fn runtime_tracks_background_task() {
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
    let runtime_task = tokio::spawn(runtime.clone().run());
    let task = runtime
        .schedule_command_task(
            "demo task".into(),
            crate::types::CommandTaskSpec {
                cmd: "sleep 1".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let state = runtime.agent_state().await.unwrap();
        if !state.active_task_ids.contains(&task.id) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background task remained active past test deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    runtime_task.abort();
}

#[tokio::test]
async fn runtime_replays_unprocessed_queue_messages_after_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("replayed")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "recover me".into(),
            },
        ))
        .await
        .unwrap();
    drop(runtime);

    let recovered = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("replayed")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(recovered.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let briefs = recovered.storage().read_recent_briefs(10).unwrap();
    assert!(briefs.iter().any(|brief| brief.text.contains("replayed")));
    runtime_task.abort();
}

#[tokio::test]
async fn runtime_interrupts_inflight_task_after_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    storage
        .append_task(&TaskRecord {
            id: "sleep-recover".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some("recoverable command".into()),
            detail: None,
            recovery: Some(TaskRecoverySpec::CommandTask {
                summary: "recoverable command".into(),
                spec: crate::types::CommandTaskSpec {
                    cmd: "sleep 5".into(),
                    workdir: None,
                    shell: None,
                    login: true,
                    tty: false,
                    yield_time_ms: 10,
                    max_output_tokens: None,
                    accepts_input: false,
                    continue_on_result: false,
                },
                trust: TrustLevel::TrustedOperator,
                promoted_from_exec_command: false,
            }),
        })
        .unwrap();

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
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let task = runtime
        .latest_task_records()
        .await
        .unwrap()
        .into_iter()
        .find(|task| task.id == "sleep-recover")
        .unwrap();
    assert_eq!(task.status, TaskStatus::Interrupted);
    assert_eq!(
        task.detail
            .as_ref()
            .and_then(|detail| detail.get("status_before_restart"))
            .and_then(serde_json::Value::as_str),
        Some("running")
    );
    let output = runtime
        .task_output("sleep-recover", false, 0)
        .await
        .unwrap();
    assert_eq!(output.retrieval_status, TaskOutputRetrievalStatus::NotReady);
    assert_eq!(output.task.status, TaskStatus::Interrupted);
    let events = runtime.storage().read_recent_events(100).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "task_interrupted_on_restart"));
    let messages = runtime.storage().read_recent_messages(20).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && matches!(
                message.origin,
                MessageOrigin::System { ref subsystem } if subsystem == "task_restart"
            )
    }));
    assert!(messages.iter().any(|message| {
        message
            .metadata
            .as_ref()
            .and_then(|value| value.get("interrupted_tasks"))
            .and_then(|value| value.get("count"))
            .and_then(serde_json::Value::as_u64)
            == Some(1)
    }));
    assert!(messages.iter().any(|message| {
        message
            .metadata
            .as_ref()
            .and_then(|value| value.get("interrupted_tasks"))
            .and_then(|value| value.get("items"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("status_before_restart")
                        .and_then(serde_json::Value::as_str)
                        == Some("running")
                })
            })
    }));
    runtime_task.abort();
}

#[tokio::test]
async fn recovered_agent_with_none_workspace_initializes_active_entry() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();

    // Create a recovered agent state without active_workspace_entry
    let mut agent = AgentState::new("default");
    agent.active_workspace_entry = None;
    agent.attached_workspaces = vec![];
    storage.write_agent(&agent).unwrap();

    // Recover the runtime - should initialize active_workspace_entry
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

    // Verify that active_workspace_entry was initialized
    let state = runtime.inner.agent.lock().await.state.clone();
    assert!(
        state.active_workspace_entry.is_some(),
        "active_workspace_entry should be initialized for new agents"
    );
    let entry = state.active_workspace_entry.as_ref().unwrap();
    assert!(
        entry.workspace_id.starts_with("ws-"),
        "workspace_id should be generated for initial workspace"
    );
    assert_eq!(
        entry.execution_root,
        workspace.path(),
        "execution_root should match initial workspace path"
    );
}

#[tokio::test]
async fn recovered_agent_with_missing_worktree_clears_workspace_fields() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();

    // Create a recovered agent with missing worktree session
    let mut agent = AgentState::new("default");
    let worktree_path = workspace.path().join("nonexistent");
    agent.worktree_session = Some(crate::types::WorktreeSession {
        original_cwd: worktree_path.clone(),
        original_branch: "main".into(),
        worktree_path: worktree_path.clone(),
        worktree_branch: "test-branch".into(),
    });
    agent.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
        workspace_id: "test-workspace".into(),
        workspace_anchor: worktree_path.clone(),
        execution_root_id: "test-root".into(),
        execution_root: worktree_path.clone(),
        projection_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
        access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
        cwd: worktree_path.clone(),
        occupancy_id: None,
        projection_metadata: None,
    });
    storage.write_agent(&agent).unwrap();

    // Recover the runtime - should clear missing worktree
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

    // Verify that worktree_session was cleared and agent_home is activated
    let state = runtime.inner.agent.lock().await.state.clone();
    assert!(
        state.worktree_session.is_none(),
        "worktree_session should be cleared when worktree is missing"
    );
    // Verify agent_home is activated as fallback
    let entry = state.active_workspace_entry.as_ref();
    assert!(
        entry.is_some(),
        "active_workspace_entry should be set to agent_home when worktree is missing"
    );
    assert_eq!(
        entry.unwrap().workspace_id.starts_with("agent_home"),
        true,
        "workspace_id should be agent_home when worktree is missing"
    );
    assert_eq!(
        entry.unwrap().projection_kind,
        WorkspaceProjectionKind::CanonicalRoot,
        "projection_kind should be CanonicalRoot when worktree is missing"
    );
}
