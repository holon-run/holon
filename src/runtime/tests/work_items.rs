use super::super::*;
use super::support::*;

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


