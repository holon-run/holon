use super::super::*;
use super::support::*;

#[tokio::test]
async fn reconcile_waiting_contract_cancels_wait_without_anchor_and_emits_audit() {
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

    let capability = runtime
        .create_callback(
            "wait for external review".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:305".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await
        .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "test".into(),
        },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "tick".into(),
        },
    );
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, capability.waiting_intent_id);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
    let events = runtime.storage().read_recent_events(16).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "missing_current_work_item_before_wait"));
}

#[tokio::test]
async fn reconcile_waiting_contract_cancels_old_waits_after_active_work_switch() {
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

    let (old_work, _) = runtime
        .create_work_item("old delivery target".into(), None)
        .await
        .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard
            .state
            .working_memory
            .current_working_memory
            .current_work_item_id = Some(old_work.id.clone());
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let capability = runtime
        .create_callback(
            "wait for old review".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:123".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await
        .unwrap();
    let old_waiting_created_at = runtime
        .latest_waiting_intents()
        .await
        .unwrap()
        .first()
        .expect("waiting intent should exist")
        .created_at;

    runtime
        .complete_work_item(old_work.id.clone(), Some("old work done".into()))
        .await
        .unwrap();
    let (new_work, _) = runtime
        .create_work_item("new delivery target".into(), None)
        .await
        .unwrap();
    runtime.pick_work_item(new_work.id.clone()).await.unwrap();

    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "switch to the new target".into(),
        },
    );
    message.created_at = old_waiting_created_at + chrono::Duration::seconds(1);
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, capability.waiting_intent_id);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
    assert_eq!(
        runtime
            .storage()
            .work_queue_prompt_projection()
            .unwrap()
            .current
            .as_ref()
            .map(|item| item.id.as_str()),
        Some(new_work.id.as_str())
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "stale_waiting_intents_cancelled"));
}

#[tokio::test]
async fn reconcile_waiting_contract_cancels_waits_when_only_waiting_anchor_exists() {
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

    let (waiting_work, _) = runtime
        .create_work_item("waiting-only delivery target".into(), None)
        .await
        .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard
            .state
            .working_memory
            .current_working_memory
            .current_work_item_id = Some(waiting_work.id.clone());
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let capability = runtime
        .create_callback(
            "wait for external response".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:456".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await
        .unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "test".into(),
        },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "tick".into(),
        },
    );
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, capability.waiting_intent_id);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
    assert!(runtime
        .storage()
        .work_queue_prompt_projection()
        .unwrap()
        .current
        .is_none());
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "missing_current_work_item_before_wait"));
}

#[tokio::test]
async fn reconcile_waiting_contract_keeps_waits_when_anchor_is_newly_established() {
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

    let (work, _) = runtime
        .create_work_item("newly anchored delivery target".into(), None)
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let capability = runtime
        .create_callback(
            "wait for fresh review".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:789".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await
        .unwrap();
    let waiting_created_at = runtime
        .latest_waiting_intents()
        .await
        .unwrap()
        .first()
        .expect("waiting intent should exist")
        .created_at;

    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "test".into(),
        },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "tick".into(),
        },
    );
    message.created_at = waiting_created_at + chrono::Duration::seconds(1);
    let closure = runtime.current_closure_decision().await.unwrap();

    runtime
        .reconcile_waiting_contract(&message, &closure)
        .await
        .unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, capability.waiting_intent_id);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Active);
    assert_eq!(
        runtime
            .storage()
            .waiting_contract_anchor()
            .unwrap()
            .as_ref()
            .map(|item| item.id.as_str()),
        Some(work.id.as_str())
    );
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "stale_waiting_intents_cancelled"));
}

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


