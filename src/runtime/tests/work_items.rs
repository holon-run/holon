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
    assert_eq!(
        active_item["plan"]["items"][0]["state"].as_str(),
        Some("doing")
    );
    assert!(active_item["plan"]["items"][0]["status"].is_null());

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
async fn work_item_plan_tools_use_state_vocabulary_for_model_visible_shape() {
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
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());

    let (create_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "create".into(),
                name: "CreateWorkItem".into(),
                input: serde_json::json!({
                    "delivery_target": "ship work item plan contract",
                    "plan": [
                        { "step": "inspect current contract", "state": "done" },
                        { "step": "update tool shape", "state": "doing" },
                        { "step": "verify regression", "state": "pending" }
                    ]
                }),
            },
        )
        .await
        .unwrap();
    let create_payload = create_result.envelope.result.unwrap();
    let work_item_id = create_payload["work_item"]["id"].as_str().unwrap();
    assert_eq!(
        create_payload["plan"]["items"][0]["state"].as_str(),
        Some("done")
    );
    assert_eq!(
        create_payload["plan"]["items"][1]["state"].as_str(),
        Some("doing")
    );
    assert_eq!(
        create_payload["plan"]["items"][2]["state"].as_str(),
        Some("pending")
    );
    assert!(create_payload["plan"]["items"][0]["status"].is_null());

    let (get_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "get".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item_id,
                    "include_plan": true
                }),
            },
        )
        .await
        .unwrap();
    let get_payload = get_result.envelope.result.unwrap();
    assert_eq!(
        get_payload["work_item"]["plan"]["items"][1]["state"].as_str(),
        Some("doing")
    );
    assert!(get_payload["work_item"]["plan"]["items"][1]["status"].is_null());

    let returned_items = get_payload["work_item"]["plan"]["items"].clone();
    let (update_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "update".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item_id,
                    "plan": returned_items
                }),
            },
        )
        .await
        .unwrap();
    let update_payload = update_result.envelope.result.unwrap();
    assert_eq!(
        update_payload["plan"]["items"][1]["state"].as_str(),
        Some("doing")
    );
    assert!(update_payload["plan"]["items"][1]["status"].is_null());
}

#[tokio::test]
async fn update_work_item_can_refine_delivery_target() {
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
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (work_item, _) = runtime
        .create_work_item("Fix issue #869".into(), None)
        .await
        .unwrap();

    let (update_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "refine-target".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item.id.clone(),
                    "delivery_target": "Fix issue #869 by allowing delivery_target refinement"
                }),
            },
        )
        .await
        .unwrap();
    let payload = update_result.envelope.result.unwrap();
    assert_eq!(
        payload["work_item"]["delivery_target"].as_str(),
        Some("Fix issue #869 by allowing delivery_target refinement")
    );

    let latest = runtime
        .latest_work_item(&work_item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        latest.delivery_target,
        "Fix issue #869 by allowing delivery_target refinement"
    );
}

#[tokio::test]
async fn update_work_item_can_refine_delivery_target_and_plan_together() {
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
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (work_item, _) = runtime
        .create_work_item("Fix issue #869".into(), None)
        .await
        .unwrap();

    let (update_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "refine-target-and-plan".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item.id.clone(),
                    "delivery_target": "Fix issue #869 by allowing delivery_target refinement",
                    "plan": [
                        { "step": "extend UpdateWorkItem schema", "state": "done" },
                        { "step": "verify target update persistence", "state": "doing" }
                    ]
                }),
            },
        )
        .await
        .unwrap();
    let payload = update_result.envelope.result.unwrap();
    assert_eq!(
        payload["work_item"]["delivery_target"].as_str(),
        Some("Fix issue #869 by allowing delivery_target refinement")
    );
    assert_eq!(payload["plan"]["items"][0]["state"].as_str(), Some("done"));
    assert_eq!(payload["plan"]["items"][1]["state"].as_str(), Some("doing"));
}

#[tokio::test]
async fn update_work_item_rejects_empty_delivery_target() {
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
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let (work_item, _) = runtime
        .create_work_item("Fix issue #869".into(), None)
        .await
        .unwrap();

    let error = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "empty-target".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": work_item.id.clone(),
                    "delivery_target": "   "
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert!(tool_error.message.contains("delivery_target"));
}

#[tokio::test]
async fn update_work_item_old_plan_shape_returns_state_example_hint() {
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
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());

    let error = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "bad-update".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "work_item_id": "item-1",
                    "plan": [
                        { "step": "inspect current handler", "status": "completed" }
                    ]
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let parse_error = tool_error
        .details
        .as_ref()
        .and_then(|details| details.get("parse_error"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(parse_error.contains("unknown field `status`"));
    assert!(parse_error.contains("state"));
    let recovery_hint = tool_error.recovery_hint.as_deref().unwrap_or_default();
    assert!(recovery_hint.contains("work_item_id"));
    assert!(recovery_hint.contains("\"state\":\"done\""));
    assert!(recovery_hint.contains("pending, doing, or done"));
}

#[tokio::test]
async fn update_work_item_missing_id_returns_top_level_field_hint() {
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
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());

    let error = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "missing-id".into(),
                name: "UpdateWorkItem".into(),
                input: serde_json::json!({
                    "plan": [
                        { "step": "inspect current handler", "state": "done" }
                    ]
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    let recovery_hint = tool_error.recovery_hint.as_deref().unwrap_or_default();
    assert!(recovery_hint.contains("UpdateWorkItem schema"));
    assert!(recovery_hint.contains("work_item_id"));
    assert!(recovery_hint.contains("\"state\":\"done\""));
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
            CallbackDeliveryMode::WakeHint,
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
async fn turn_end_work_item_commit_keeps_failed_turn_open_without_blocker() {
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
            checkpoint: None,
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
    assert!(committed.blocked_by.is_none());
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
async fn work_item_scoped_external_trigger_requires_current_work_item_anchor() {
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

    let result = runtime
        .create_external_trigger(
            "wait for external review".into(),
            "github".into(),
            ExternalTriggerScope::WorkItem,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await;
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("requires a current work item"));
    assert!(runtime.latest_waiting_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn agent_scoped_external_trigger_survives_missing_work_item_cleanup() {
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
        .create_external_trigger(
            "Check durable inbox for unread entries".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
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
    assert_eq!(waiting[0].scope, ExternalTriggerScope::Agent);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Active);
    let closure = runtime.current_closure_decision().await.unwrap();
    assert_ne!(
        closure.waiting_reason,
        Some(WaitingReason::AwaitingExternalChange)
    );
    let events = runtime.storage().read_recent_events(16).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == "missing_current_work_item_before_wait"));
}

#[tokio::test]
async fn agent_scoped_wake_hint_preserves_external_trigger_provenance() {
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
        guard.state.status = AgentStatus::Asleep;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let capability = runtime
        .create_external_trigger(
            "Check AgentInbox for unread items".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();

    let result = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Json {
                    value: serde_json::json!({
                        "latest_entry_id": "ent_123",
                        "preview": "new inbox item"
                    }),
                }),
                content_type: Some("application/json".into()),
                correlation_id: Some("corr-inbox".into()),
                causation_id: Some("cause-webhook".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.disposition, CallbackIngressDisposition::Triggered);
    assert_eq!(result.scope, ExternalTriggerScope::Agent);

    let messages = runtime.storage().read_recent_messages(10).unwrap();
    let tick = messages
        .iter()
        .find(|message| message.kind == MessageKind::SystemTick)
        .expect("wake hint should emit a system tick");
    let wake_hint = tick
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("wake_hint"))
        .expect("wake hint metadata should exist");
    assert_eq!(wake_hint["source"].as_str(), Some("agentinbox"));
    assert_eq!(wake_hint["scope"].as_str(), Some("agent"));
    assert_eq!(
        wake_hint["waiting_intent_id"].as_str(),
        Some(capability.waiting_intent_id.as_str())
    );
    assert_eq!(
        wake_hint["external_trigger_id"].as_str(),
        Some(capability.external_trigger_id.as_str())
    );
    assert_eq!(
        wake_hint["description"].as_str(),
        Some("Check AgentInbox for unread items")
    );
    assert_eq!(wake_hint["correlation_id"].as_str(), Some("corr-inbox"));
    assert_eq!(wake_hint["causation_id"].as_str(), Some("cause-webhook"));
    assert_eq!(
        wake_hint["body"]["value"]["latest_entry_id"].as_str(),
        Some("ent_123")
    );
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
            CallbackDeliveryMode::WakeHint,
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
async fn reconcile_waiting_contract_keeps_agent_scoped_waits_after_active_work_switch() {
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
    runtime.pick_work_item(old_work.id.clone()).await.unwrap();
    let capability = runtime
        .create_external_trigger(
            "Check durable inbox for unread entries".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
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
    assert_eq!(waiting[0].scope, ExternalTriggerScope::Agent);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Active);
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(!events
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
            CallbackDeliveryMode::WakeHint,
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
            CallbackDeliveryMode::WakeHint,
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
async fn queued_notification_keeps_working_memory_unfocused_before_pick() {
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
    assert!(state.current_work_item_id.is_none());
    assert!(state
        .working_memory
        .current_working_memory
        .current_work_item_id
        .is_none());
    assert!(
        state.working_memory.working_memory_revision > 0,
        "working memory should refresh after queued notification"
    );
    let deltas = runtime
        .storage()
        .read_recent_working_memory_deltas(10)
        .unwrap();
    assert!(!deltas.iter().any(|delta| delta
        .changed_fields
        .iter()
        .any(|field| field == "current_work_item_id")));
    assert!(state
        .working_memory
        .active_episode_builder
        .as_ref()
        .and_then(|builder| builder.current_work_item_id.as_deref())
        .is_none());

    runtime_task.abort();
}
