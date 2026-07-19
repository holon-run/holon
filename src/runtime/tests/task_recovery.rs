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
                cmd: "true".into(),
                workdir: None,
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 0,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::OperatorInstruction,
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let latest = runtime.task_record(&task.id).await.unwrap().unwrap();
        if matches!(
            latest.status,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        ) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background task did not reach a terminal status before the deadline; latest status: {:?}",
            latest.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let active_tasks = runtime.active_tasks(10).await.unwrap();
        if !active_tasks.iter().any(|record| record.id == task.id) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "terminal background task remained in active task projection"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    runtime_task.abort();
}

#[tokio::test]
async fn runtime_records_scheduler_decision_before_dequeueing_message() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("scheduled")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "schedule me".into(),
            },
        ))
        .await
        .unwrap();

    let runtime_task = tokio::spawn(runtime.clone().run());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let events = runtime.storage().read_recent_events(200).unwrap();
        if events.iter().any(|event| {
            event.kind == "message_processing_started"
                && event.data["message_id"] == message.id.as_str()
        }) {
            let decision_index = events
                .iter()
                .position(|event| {
                    event.kind == "scheduler_decision"
                        && event.data["message_id"] == message.id.as_str()
                        && event.data["boundary"] == "run_loop"
                })
                .expect("run loop scheduler decision should be recorded");
            let processing_index = events
                .iter()
                .position(|event| {
                    event.kind == "message_processing_started"
                        && event.data["message_id"] == message.id.as_str()
                })
                .expect("message processing should start");
            assert!(
                decision_index < processing_index,
                "scheduler decision should be recorded before message processing starts"
            );
            let decision = &events[decision_index];
            assert_eq!(decision.data["decision"], "StartModelTurn");
            assert_eq!(decision.data["model_reentry"], true);
            assert!(!events.iter().any(|event| {
                event.kind == "scheduler_decision"
                    && event.data["message_id"] == message.id.as_str()
                    && event.data["boundary"] == "message_processing"
            }));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "message was not processed before deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    runtime_task.abort();
}

#[tokio::test]
async fn malformed_task_message_does_not_exit_runtime_loop() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("still running")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let mut malformed = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "bad-task".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "malformed task result".into(),
        },
    );
    malformed.metadata = Some(serde_json::json!({
        "task_kind": "child_agent_task",
        "task_status": "completed"
    }));
    let malformed = runtime.enqueue(malformed).await.unwrap();
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue after malformed task".into(),
            },
        ))
        .await
        .unwrap();

    let runtime_task = tokio::spawn(runtime.clone().run());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let briefs = runtime.storage().read_recent_briefs(10).unwrap();
        if briefs
            .iter()
            .any(|brief| brief.text.contains("still running"))
        {
            let events = runtime.storage().read_recent_events(200).unwrap();
            assert!(events.iter().any(|event| {
                event.kind == "runtime_error"
                    && event.data["message_id"] == malformed.id.as_str()
                    && event.data["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("metadata.task_id"))
            }));
            assert!(events.iter().any(|event| {
                event.kind == "scheduler_decision"
                    && event.data["message_id"] == malformed.id.as_str()
                    && event.data["boundary"] == "run_loop"
            }));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "runtime did not process the message after malformed task result"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    runtime_task.abort();
}

#[tokio::test]
async fn recovered_agent_with_none_workspace_initializes_active_entry() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new_for_test(dir.path()).unwrap();

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
        entry.workspace_id.starts_with("ws_"),
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
    let storage = AppStorage::new_for_test(dir.path()).unwrap();

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
