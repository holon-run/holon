use super::super::*;
use super::support::*;

#[tokio::test]
async fn runtime_emits_pending_wake_hint_as_system_tick_on_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    agent.pending_wake_hint = Some(PendingWakeHint {
        reason: "restart wake".into(),
        source: Some("test".into()),
        resource: None,
        body: None,
        content_type: None,
        correlation_id: Some("corr".into()),
        causation_id: None,
        created_at: Utc::now(),
    });
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("wake done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_wake_hint.is_none());
    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages
        .iter()
        .any(|message| message.kind == MessageKind::SystemTick
            && message.authority_class == AuthorityClass::RuntimeInstruction));
    runtime_task.abort();
}

#[tokio::test]
async fn runtime_emits_work_queue_system_tick_for_active_item_on_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    storage.append_work_item(&active).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let messages = runtime.storage().read_recent_messages(20).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("continue_active")
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

#[tokio::test]
async fn current_closure_reports_continuable_for_current_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    let active_id = active.id.clone();
    storage.append_work_item(&active).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active_id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Continuable);
    assert_eq!(closure.waiting_reason, None);
    let signal = closure.work_signal.expect("work signal should exist");
    assert_eq!(signal.work_item_id, active_id);
    assert_eq!(signal.state, WorkItemState::Open);
    assert_eq!(
        signal.reactivation_mode,
        WorkReactivationMode::ContinueActive
    );
}

#[tokio::test]
async fn current_closure_reports_continuable_for_queued_work_item_without_active_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let queued = WorkItemRecord::new(
        "default",
        "activate queued runtime cleanup",
        WorkItemState::Open,
    );
    let queued_id = queued.id.clone();
    storage.append_work_item(&queued).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Continuable);
    assert_eq!(closure.waiting_reason, None);
    let signal = closure.work_signal.expect("work signal should exist");
    assert_eq!(signal.work_item_id, queued_id);
    assert_eq!(signal.state, WorkItemState::Open);
    assert_eq!(
        signal.reactivation_mode,
        WorkReactivationMode::ActivateQueued
    );
}

#[tokio::test]
async fn idle_tick_prefers_current_work_item_over_queued_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    let queued = WorkItemRecord::new("default", "queued runtime cleanup", WorkItemState::Open);
    storage.append_work_item(&active).unwrap();
    storage.append_work_item(&queued).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    assert!(runtime.maybe_emit_pending_system_tick(None).await.unwrap());

    let queued_latest = runtime
        .latest_work_item(&queued.id)
        .await
        .unwrap()
        .expect("queued item should still exist");
    assert_eq!(queued_latest.state, WorkItemState::Open);

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("continue_active")
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("work_item_id"))
                .and_then(serde_json::Value::as_str)
                == Some(active.id.as_str())
    }));
}

#[tokio::test]
async fn idle_tick_suppresses_continue_active_after_model_visible_task_result() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    storage.append_work_item(&active).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let task_result_rejoin = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::TaskResult,
        class: ContinuationClass::ResumeExpectedWait,
        model_visible: true,
        prior_closure_outcome: ClosureOutcome::Waiting,
        prior_waiting_reason: Some(WaitingReason::AwaitingTaskResult),
        matched_waiting_reason: true,
        evidence: vec!["matches_waiting_reason".into()],
    };

    assert!(!runtime
        .maybe_emit_pending_system_tick(Some(&task_result_rejoin))
        .await
        .unwrap());

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(!messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("continue_active")
    }));
}

#[tokio::test]
async fn queued_activation_updates_working_memory_before_follow_up_turn() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let queued = WorkItemRecord::new("default", "queued runtime cleanup", WorkItemState::Open);
    storage.append_work_item(&queued).unwrap();

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

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "wrap up current work".into(),
            },
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state
            .working_memory
            .current_working_memory
            .current_work_item_id
            .as_deref(),
        Some(queued.id.as_str())
    );
    assert!(
        state.working_memory.working_memory_revision > 0,
        "working memory should refresh after queued activation"
    );
    let deltas = runtime
        .storage()
        .read_recent_working_memory_deltas(10)
        .unwrap();
    assert!(deltas.iter().any(|delta| {
        delta
            .changed_fields
            .iter()
            .any(|field| field == "current_work_item_id")
            && delta.to_revision == state.working_memory.working_memory_revision
    }));
    assert_eq!(
        state
            .working_memory
            .active_episode_builder
            .as_ref()
            .and_then(|builder| builder.current_work_item_id.as_deref()),
        Some(queued.id.as_str())
    );

    runtime_task.abort();
}

#[tokio::test]
async fn idle_tick_prefers_pending_wake_hint_over_work_queue_tick() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let active = WorkItemRecord::new(
        "default",
        "continue active runtime cleanup",
        WorkItemState::Open,
    );
    storage.append_work_item(&active).unwrap();

    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    agent.pending_wake_hint = Some(PendingWakeHint {
        reason: "resume from callback".into(),
        source: Some("test".into()),
        resource: None,
        body: None,
        content_type: None,
        correlation_id: Some("wake-priority".into()),
        causation_id: None,
        created_at: Utc::now(),
    });
    storage.write_agent(&agent).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    assert!(runtime.maybe_emit_pending_system_tick(None).await.unwrap());
    let state = runtime.agent_state().await.unwrap();
    assert!(state.pending_wake_hint.is_none());

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && matches!(
                message.origin,
                MessageOrigin::System { ref subsystem } if subsystem == "wake_hint"
            )
    }));
    assert!(!messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .is_some()
    }));
}

#[tokio::test]
async fn runtime_activates_queued_work_item_and_emits_work_queue_system_tick_on_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let queued = WorkItemRecord::new(
        "default",
        "activate queued runtime cleanup",
        WorkItemState::Open,
    );
    let queued_id = queued.id.clone();
    storage.append_work_item(&queued).unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let active = runtime
        .latest_work_item(&queued_id)
        .await
        .unwrap()
        .expect("queued item should still exist");
    assert_eq!(active.state, WorkItemState::Open);

    let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "system_tick_emitted"
            && event.data["work_queue"]["work_item_id"].as_str() == Some(queued_id.as_str())
    }));
    runtime_task.abort();
}

#[tokio::test]
async fn queued_work_item_update_wakes_sleeping_runtime_and_activates_it() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("tick done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (queued, _) = runtime
        .create_work_item("wake from direct queued work item update".into(), None)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let active = runtime
        .latest_work_item(&queued.id)
        .await
        .unwrap()
        .expect("queued item should still exist");
    assert_eq!(active.state, WorkItemState::Open);

    let messages = runtime.storage().read_recent_messages(20).unwrap();
    assert!(messages.iter().any(|message| {
        message.kind == MessageKind::SystemTick
            && message
                .metadata
                .as_ref()
                .and_then(|value| value.get("work_queue"))
                .and_then(|value| value.get("work_item_id"))
                .and_then(serde_json::Value::as_str)
                == Some(queued.id.as_str())
    }));
    runtime_task.abort();
}

