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
        description: None,
        scope: None,
        waiting_intent_id: None,
        external_trigger_id: None,
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
        description: None,
        scope: None,
        waiting_intent_id: None,
        external_trigger_id: None,
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
async fn runtime_surfaces_queued_work_item_with_work_queue_system_tick_on_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let queued = WorkItemRecord::new(
        "default",
        "surface queued runtime cleanup",
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
async fn queued_work_item_update_wakes_sleeping_runtime_and_surfaces_it() {
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
