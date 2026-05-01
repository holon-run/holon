use super::support::*;
use super::super::*;

#[tokio::test]
async fn work_item_query_tools_return_current_open_done_views() {
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

    let (active, _) = runtime
        .create_work_item("finish active delivery".into(), None)
        .await
        .unwrap();
    runtime.pick_work_item(active.id.clone()).await.unwrap();
    runtime
        .update_work_plan(
            active.id.clone(),
            vec![crate::types::WorkPlanItem {
                step: "inspect query surface".into(),
                status: crate::types::WorkPlanStepStatus::InProgress,
            }],
        )
        .await
        .unwrap();
    let (queued, _) = runtime
        .create_work_item("queued delivery".into(), None)
        .await
        .unwrap();
    let (completed, _) = runtime
        .create_work_item("completed delivery".into(), None)
        .await
        .unwrap();
    let completed = runtime
        .complete_work_item(completed.id.clone(), None)
        .await
        .unwrap();
    bind_turn_to_work_item(&runtime, &active.id).await;

    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (active_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "active".into(),
                name: "ListWorkItems".into(),
                input: serde_json::json!({"filter": "current", "include_plan": true}),
            },
        )
        .await
        .unwrap();
    let active_payload = active_result.envelope.result.unwrap();
    let active_item = &active_payload["work_items"][0];
    assert_eq!(
        active_payload["context"]["current_work_item_id"].as_str(),
        Some(active.id.as_str())
    );
    assert_eq!(active_item["state"].as_str(), Some("open"));
    assert_eq!(active_item["focus"].as_str(), Some("current"));
    assert_eq!(active_item["is_current"].as_bool(), Some(true));
    assert_eq!(active_item["plan"]["items"].as_array().unwrap().len(), 1);

    let (list_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "list".into(),
                name: "ListWorkItems".into(),
                input: serde_json::json!({"filter": "open", "limit": 10}),
            },
        )
        .await
        .unwrap();
    let list_payload = list_result.envelope.result.unwrap();
    let items = list_payload["work_items"].as_array().unwrap();
    assert_eq!(list_payload["total_matching"].as_u64(), Some(2));
    assert!(items
        .iter()
        .any(|item| item["id"].as_str() == Some(active.id.as_str())));
    assert!(items
        .iter()
        .any(|item| item["id"].as_str() == Some(queued.id.as_str())));
    assert!(!items
        .iter()
        .any(|item| item["id"].as_str() == Some(completed.id.as_str())));

    let (done_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "done".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({"work_item_id": completed.id}),
            },
        )
        .await
        .unwrap();
    let done_payload = done_result.envelope.result.unwrap();
    assert_eq!(done_payload["work_item"]["state"].as_str(), Some("done"));
    assert_eq!(done_payload["work_item"]["focus"].as_str(), Some("done"));

    bind_turn_to_work_item(&runtime, completed.id.as_str()).await;
    let (fallback_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "fallback-active".into(),
                name: "ListWorkItems".into(),
                input: serde_json::json!({"filter": "current"}),
            },
        )
        .await
        .unwrap();
    let fallback_payload = fallback_result.envelope.result.unwrap();
    assert_eq!(
        fallback_payload["context"]["current_work_item_id"].as_str(),
        Some(active.id.as_str())
    );
    assert_eq!(
        fallback_payload["work_items"][0]["id"].as_str(),
        Some(active.id.as_str())
    );
}

#[tokio::test]
async fn persist_brief_binds_current_turn_work_item() {
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

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;

    runtime
        .persist_brief(&BriefRecord::new(
            "default",
            BriefKind::Result,
            "bound brief",
            None,
            None,
        ))
        .await
        .unwrap();

    let briefs = runtime.recent_briefs(10).await.unwrap();
    assert_eq!(briefs.len(), 1);
    assert_eq!(
        briefs[0].work_item_id.as_deref(),
        Some(work_item_id.as_str())
    );
}

#[tokio::test]
async fn create_callback_binds_current_turn_work_item() {
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

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;

    runtime
        .create_callback(
            "wait for review".into(),
            "github".into(),
            "review_submitted".into(),
            Some("pull_request:302".into()),
            CallbackDeliveryMode::WakeOnly,
        )
        .await
        .unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(
        waiting[0].work_item_id.as_deref(),
        Some(work_item_id.as_str())
    );
}

#[tokio::test]
async fn interactive_tool_execution_binds_current_turn_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(OneToolThenTextProvider {
            calls: Mutex::new(0),
        }),
        "default".into(),
        ContextConfig {
            prompt_budget_estimated_tokens: 16384,
            compaction_keep_recent_estimated_tokens: 2048,
            ..context_config()
        },
    )
    .unwrap();

    let (work_item, _) = runtime
        .create_work_item("verify binding".into(), None)
        .await
        .unwrap();
    runtime.pick_work_item(work_item.id.clone()).await.unwrap();
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "run one verification command".into(),
        },
    );

    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_name, "ExecCommand");
    assert_eq!(
        tools[0].work_item_id.as_deref(),
        Some(work_item.id.as_str())
    );
}

#[tokio::test]
async fn runtime_sleeps_after_processing() {
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

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "hello".into(),
        },
    );
    runtime.enqueue(message).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let state = runtime.agent_state().await.unwrap();
    assert_eq!(state.status, AgentStatus::Asleep);
    runtime_task.abort();
}

#[tokio::test]
async fn turn_end_work_item_commit_defaults_completed_turn_to_active() {
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

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;
    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert!(committed.blocked_by.is_none());
    assert!(runtime
        .agent_state()
        .await
        .unwrap()
        .current_turn_work_item_id
        .is_none());
}

#[tokio::test]
async fn turn_end_work_item_commit_moves_failed_turn_to_waiting() {
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

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.last_turn_terminal = Some(TurnTerminalRecord {
            turn_index: guard.state.turn_index,
            kind: TurnTerminalKind::Aborted,
            last_assistant_message: Some("provider context_length_exceeded".into()),
            completed_at: Utc::now(),
            duration_ms: 42,
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert_eq!(
        committed.blocked_by.as_deref(),
        Some("Turn failed and requires operator intervention before continuing.")
    );
}

#[tokio::test]
async fn turn_end_work_item_commit_moves_bound_item_to_waiting_when_runtime_is_waiting() {
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

    let work_item_id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;
    mark_blocking_task(&runtime, "blocking-wait").await;

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert_eq!(
        committed.blocked_by.as_deref(),
        Some("Waiting on a task result.")
    );
}

#[tokio::test]
async fn turn_end_work_item_commit_preserves_explicit_completed_claim_without_waiting_facts() {
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

    let work_item_id = seed_bound_work_item(
        &runtime,
        WorkItemState::Done,
        Some("finished"),
        Some("all requested changes are done"),
    )
    .await;
    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Done);
    assert!(committed.blocked_by.is_none());
}

#[tokio::test]
async fn turn_end_work_item_commit_rejects_completed_claim_when_runtime_is_still_waiting() {
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

    let work_item_id = seed_bound_work_item(
        &runtime,
        WorkItemState::Done,
        Some("finished"),
        Some("marked complete too early"),
    )
    .await;
    mark_blocking_task(&runtime, "blocking-after-complete").await;

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Done);
    assert!(committed.blocked_by.is_none());
}

#[tokio::test]
async fn turn_end_work_item_commit_preserves_explicit_queued_claim() {
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

    let work_item_id = seed_bound_work_item(
        &runtime,
        WorkItemState::Open,
        Some("yield the active slot"),
        Some("requeue after this turn"),
    )
    .await;
    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(committed.id, work_item_id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert!(committed.blocked_by.is_none());
}

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

#[tokio::test]
async fn runtime_fires_overdue_timer_after_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    storage
        .append_timer(&TimerRecord {
            id: "timer-recover".into(),
            agent_id: "default".into(),
            created_at: Utc::now(),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer recovered".into()),
            next_fire_at: Some(Utc::now() - chrono::Duration::milliseconds(5)),
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let timer = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|timer| timer.id == "timer-recover" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test]
async fn runtime_recovers_active_timer_without_next_fire_at() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    storage
        .append_timer(&TimerRecord {
            id: "timer-missing-next-fire".into(),
            agent_id: "default".into(),
            created_at: Utc::now() - chrono::Duration::milliseconds(20),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer fallback".into()),
            next_fire_at: None,
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer fallback done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let timer = runtime
        .recent_timers(10)
        .await
        .unwrap()
        .into_iter()
        .find(|timer| timer.id == "timer-missing-next-fire" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test]
async fn schedule_timer_rejects_unrepresentable_duration() {
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

    let result = runtime.schedule_timer(u64::MAX, None, None).await;
    assert!(result.is_err());
}

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
