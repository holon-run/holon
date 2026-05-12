use super::super::*;
use super::support::*;
use crate::types::WorkItemPlanStatus;

fn blocking_task_for_work_item(task_id: &str, work_item_id: Option<&str>) -> TaskRecord {
    TaskRecord {
        id: task_id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: work_item_id.map(ToString::to_string),
        summary: Some("blocking command".into()),
        detail: Some(serde_json::json!({
            "wait_policy": "blocking",
            "work_item_id": work_item_id,
        })),
        recovery: None,
    }
}

#[test]
fn work_item_record_revision_defaults_for_old_records() {
    let value = serde_json::json!({
        "id": "work-old",
        "agent_id": "default",
        "workspace_id": "agent_home",
        "objective": "old record",
        "state": "open",
        "plan_status": "draft",
        "created_at": Utc::now(),
        "updated_at": Utc::now(),
    });
    let record: WorkItemRecord = serde_json::from_value(value).unwrap();
    assert_eq!(record.revision, 1);
}

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

    let active = runtime
        .create_work_item("finish active delivery".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(active.id.clone()).await.unwrap();
    runtime
        .update_work_item_fields(
            active.id.clone(),
            None,
            None,
            Some(Some("Inspect query surface behavior.".into())),
            Some(vec![crate::types::TodoItem {
                text: "inspect query surface".into(),
                state: crate::types::TodoItemState::InProgress,
            }]),
            None,
        )
        .await
        .unwrap();
    let queued = runtime
        .create_work_item("queued delivery".into(), None, None, Vec::new())
        .await
        .unwrap();
    let completed = runtime
        .create_work_item("completed delivery".into(), None, None, Vec::new())
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
                input: serde_json::json!({"filter": "current", "include_plan": true, "include_todo_list": true}),
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
    assert_eq!(active_item["readiness"].as_str(), Some("runnable"));
    assert_eq!(active_item["is_current"].as_bool(), Some(true));
    assert_eq!(active_item["is_runnable"].as_bool(), Some(true));
    assert_eq!(
        active_item["plan"].as_str(),
        Some("Inspect query surface behavior.")
    );
    assert_eq!(active_item["todo_list"].as_array().unwrap().len(), 1);
    assert_eq!(
        active_item["todo_list"][0]["state"].as_str(),
        Some("in_progress")
    );

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

    let (completed_result, _) = registry
        .execute(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "completed".into(),
                name: "GetWorkItem".into(),
                input: serde_json::json!({"work_item_id": completed.id}),
            },
        )
        .await
        .unwrap();
    let completed_payload = completed_result.envelope.result.unwrap();
    assert_eq!(
        completed_payload["work_item"]["state"].as_str(),
        Some("completed")
    );
    assert_eq!(
        completed_payload["work_item"]["focus"].as_str(),
        Some("completed")
    );
    assert_eq!(
        completed_payload["work_item"]["readiness"].as_str(),
        Some("completed")
    );

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
async fn work_item_revision_increments_on_updates_and_completion() {
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

    let created = runtime
        .create_work_item("revision contract".into(), None, None, Vec::new())
        .await
        .unwrap();
    assert_eq!(created.revision, 1);

    let updated = runtime
        .update_work_item_fields(
            created.id.clone(),
            None,
            Some(WorkItemPlanStatus::Ready),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);

    let completed = runtime
        .complete_work_item(updated.id.clone(), Some("done".into()))
        .await
        .unwrap();
    assert_eq!(completed.revision, 3);
}

#[tokio::test]
async fn work_item_completion_ignores_running_tasks_and_clears_explicit_waits() {
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

    let target = runtime
        .create_work_item("target".into(), None, None, Vec::new())
        .await
        .unwrap();
    let other = runtime
        .create_work_item("other".into(), None, None, Vec::new())
        .await
        .unwrap();
    let related_task = blocking_task_for_work_item("task-target", Some(&target.id));
    runtime.storage().append_task(&related_task).unwrap();
    let unrelated_task = blocking_task_for_work_item("task-other", Some(&other.id));
    runtime.storage().append_task(&unrelated_task).unwrap();
    let unscoped_task = blocking_task_for_work_item("task-unscoped", None);
    runtime.storage().append_task(&unscoped_task).unwrap();

    let completed = runtime
        .complete_work_item(target.id.clone(), Some("target done".into()))
        .await
        .unwrap();
    assert_eq!(completed.id, target.id);
    assert_eq!(completed.result_summary.as_deref(), Some("target done"));

    let explicit_wait = runtime
        .create_work_item("explicit wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            explicit_wait.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("review the external result".into())),
        )
        .await
        .unwrap();

    let completed_wait = runtime
        .complete_work_item(explicit_wait.id.clone(), Some("confirmed done".into()))
        .await
        .unwrap();
    assert_eq!(completed_wait.state, WorkItemState::Completed);
    assert!(completed_wait.blocked_by.is_none());
}

#[tokio::test]
async fn work_item_query_tools_return_readiness_views() {
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

    let runnable = runtime
        .create_work_item("runnable work".into(), None, None, Vec::new())
        .await
        .unwrap();
    let waiting = runtime
        .create_work_item(
            "waiting work".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let blocked = runtime
        .create_work_item("blocked work".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            blocked.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("waiting for CI".into())),
        )
        .await
        .unwrap();

    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let list = |filter: &'static str| {
        let registry = &registry;
        let runtime = &runtime;
        async move {
            let (result, _) = registry
                .execute(
                    runtime,
                    "default",
                    &TrustLevel::TrustedOperator,
                    &crate::tool::ToolCall {
                        id: format!("list-{filter}"),
                        name: "ListWorkItems".into(),
                        input: serde_json::json!({"filter": filter}),
                    },
                )
                .await
                .unwrap();
            result.envelope.result.unwrap()
        }
    };

    let runnable_payload = list("runnable").await;
    assert_eq!(runnable_payload["total_matching"].as_u64(), Some(1));
    assert_eq!(
        runnable_payload["work_items"][0]["id"].as_str(),
        Some(runnable.id.as_str())
    );
    assert_eq!(
        runnable_payload["work_items"][0]["readiness"].as_str(),
        Some("runnable")
    );

    let waiting_payload = list("waiting_for_operator").await;
    assert_eq!(waiting_payload["total_matching"].as_u64(), Some(1));
    assert_eq!(
        waiting_payload["work_items"][0]["id"].as_str(),
        Some(waiting.id.as_str())
    );
    assert_eq!(
        waiting_payload["work_items"][0]["readiness"].as_str(),
        Some("waiting_for_operator")
    );
    assert_eq!(
        waiting_payload["work_items"][0]["is_runnable"].as_bool(),
        Some(false)
    );

    let queued_payload = list("queued").await;
    assert_eq!(queued_payload["total_matching"].as_u64(), Some(2));
    let queued_ids = queued_payload["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert!(queued_ids.contains(&runnable.id.as_str()));
    assert!(queued_ids.contains(&waiting.id.as_str()));

    let blocked_payload = list("blocked").await;
    assert_eq!(blocked_payload["total_matching"].as_u64(), Some(1));
    assert_eq!(
        blocked_payload["work_items"][0]["id"].as_str(),
        Some(blocked.id.as_str())
    );
    assert_eq!(
        blocked_payload["work_items"][0]["readiness"].as_str(),
        Some("blocked")
    );
}

#[tokio::test]
async fn work_item_tools_use_objective_plan_and_todo_list_shape() {
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
                    "objective": "ship work item objective contract",
                    "plan_status": "ready",
                    "plan": "1. Inspect current contract\n2. Update tool shape\n3. Verify regression",
                    "todo_list": [
                        { "text": "inspect current contract", "state": "completed" },
                        { "text": "update tool shape", "state": "in_progress" },
                        { "text": "verify regression", "state": "pending" }
                    ]
                }),
            },
        )
        .await
        .unwrap();
    let create_payload = create_result.envelope.result.unwrap();
    let work_item_id = create_payload["work_item"]["id"].as_str().unwrap();
    assert_eq!(
        create_payload["work_item"]["plan_status"].as_str(),
        Some("ready")
    );
    assert_eq!(
        create_payload["work_item"]["plan"].as_str(),
        Some("1. Inspect current contract\n2. Update tool shape\n3. Verify regression")
    );
    assert_eq!(
        create_payload["work_item"]["todo_list"][0]["state"].as_str(),
        Some("completed")
    );
    assert_eq!(
        create_payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
    assert_eq!(
        create_payload["work_item"]["todo_list"][2]["state"].as_str(),
        Some("pending")
    );
    assert!(create_payload["work_item"]["todo_list"][0]["status"].is_null());

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
                    "include_plan": true,
                    "include_todo_list": true
                }),
            },
        )
        .await
        .unwrap();
    let get_payload = get_result.envelope.result.unwrap();
    assert_eq!(
        get_payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
    assert!(get_payload["work_item"]["todo_list"][1]["status"].is_null());

    let returned_items = get_payload["work_item"]["todo_list"].clone();
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
                    "todo_list": returned_items
                }),
            },
        )
        .await
        .unwrap();
    let update_payload = update_result.envelope.result.unwrap();
    assert_eq!(
        update_payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
    assert!(update_payload["work_item"]["todo_list"][1]["status"].is_null());
}

#[tokio::test]
async fn update_work_item_can_refine_objective() {
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
    let work_item = runtime
        .create_work_item("Fix issue #869".into(), None, None, Vec::new())
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
                    "objective": "Fix issue #869 by allowing objective refinement"
                }),
            },
        )
        .await
        .unwrap();
    let payload = update_result.envelope.result.unwrap();
    assert_eq!(
        payload["work_item"]["objective"].as_str(),
        Some("Fix issue #869 by allowing objective refinement")
    );

    let latest = runtime
        .latest_work_item(&work_item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        latest.objective,
        "Fix issue #869 by allowing objective refinement"
    );
}

#[tokio::test]
async fn update_work_item_can_refine_objective_and_plan_together() {
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
    let work_item = runtime
        .create_work_item("Fix issue #869".into(), None, None, Vec::new())
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
                    "objective": "Fix issue #869 by allowing objective refinement",
                    "plan": "Extend UpdateWorkItem schema, then verify persistence.",
                    "todo_list": [
                        { "text": "extend UpdateWorkItem schema", "state": "completed" },
                        { "text": "verify target update persistence", "state": "in_progress" }
                    ]
                }),
            },
        )
        .await
        .unwrap();
    let payload = update_result.envelope.result.unwrap();
    assert_eq!(
        payload["work_item"]["objective"].as_str(),
        Some("Fix issue #869 by allowing objective refinement")
    );
    assert_eq!(
        payload["work_item"]["todo_list"][0]["state"].as_str(),
        Some("completed")
    );
    assert_eq!(
        payload["work_item"]["todo_list"][1]["state"].as_str(),
        Some("in_progress")
    );
}

#[tokio::test]
async fn update_work_item_rejects_empty_objective() {
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
    let work_item = runtime
        .create_work_item("Fix issue #869".into(), None, None, Vec::new())
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
                    "objective": "   "
                }),
            },
        )
        .await
        .unwrap_err();
    let tool_error = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
    assert!(tool_error.message.contains("objective"));
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
                    "todo_list": [
                        { "text": "inspect current handler", "status": "completed" }
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
    assert!(recovery_hint.contains("\"state\":\"completed\""));
    assert!(recovery_hint.contains("pending, in_progress, or completed"));
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
                    "todo_list": [
                        { "text": "inspect current handler", "state": "completed" }
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
    assert!(recovery_hint.contains("\"state\":\"completed\""));
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

    let work_item = runtime
        .create_work_item("verify binding".into(), None, None, Vec::new())
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
async fn turn_end_work_item_commit_ignores_unfinished_turn_binding() {
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

    let work = runtime
        .create_work_item("no terminal yet".into(), None, None, Vec::new())
        .await
        .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.turn_index = 7;
        guard.state.current_turn_work_item_id = Some(work.id.clone());
        guard.state.last_turn_terminal = None;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await
        .unwrap();
    assert!(committed.is_none());
    assert_eq!(
        runtime
            .agent_state()
            .await
            .unwrap()
            .current_turn_work_item_id
            .as_deref(),
        Some(work.id.as_str())
    );
    assert_eq!(
        runtime
            .latest_work_item(&work.id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
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
            reason: None,
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
async fn turn_end_work_item_commit_preserves_existing_blocker_on_failed_turn() {
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
        None,
        Some("Waiting for reviewer response."),
    )
    .await;
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.last_turn_terminal = Some(TurnTerminalRecord {
            turn_index: guard.state.turn_index,
            kind: TurnTerminalKind::Aborted,
            reason: None,
            last_assistant_message: Some("provider timeout".into()),
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
    assert_eq!(
        committed.blocked_by.as_deref(),
        Some("Waiting for reviewer response.")
    );
}

#[tokio::test]
async fn turn_end_work_item_commit_does_not_block_bound_item_for_active_task_presence() {
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
    assert_eq!(committed.blocked_by.as_deref(), None);
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
        WorkItemState::Completed,
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
    assert_eq!(committed.state, WorkItemState::Completed);
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
        WorkItemState::Completed,
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
    assert_eq!(committed.state, WorkItemState::Completed);
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
    assert_eq!(
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
async fn triggered_work_item_waiting_intent_preserves_explicit_work_item_state_until_completion() {
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

    let work = runtime
        .create_work_item("wait for CI".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .update_work_item_fields(
            work.id.clone(),
            None,
            None,
            None,
            None,
            Some(Some("awaiting CI success".into())),
        )
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let capability = runtime
        .create_external_trigger(
            "wait for CI".into(),
            "github".into(),
            ExternalTriggerScope::WorkItem,
            CallbackDeliveryMode::WakeHint,
            Some("CI run completed".into()),
            Some("holon-run/holon#1079".into()),
        )
        .await
        .unwrap();

    let result = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Json {
                    value: serde_json::json!({"check": "Rust", "conclusion": "success"}),
                }),
                content_type: Some("application/json".into()),
                correlation_id: Some("corr-ci".into()),
                causation_id: Some("run-123".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.disposition, CallbackIngressDisposition::Triggered);

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    let waiting = waiting
        .iter()
        .find(|record| record.id == capability.waiting_intent_id)
        .expect("waiting intent should remain visible");
    assert_eq!(waiting.status, WaitingIntentStatus::Active);
    assert_eq!(waiting.trigger_count, 1);
    assert_eq!(waiting.work_item_id.as_deref(), Some(work.id.as_str()));
    let latest = runtime
        .storage()
        .latest_work_item(&work.id)
        .unwrap()
        .expect("work item should exist");
    assert_eq!(latest.blocked_by.as_deref(), Some("awaiting CI success"));

    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "callback_delivered"
            && event.data["waiting_intent_id"].as_str()
                == Some(capability.waiting_intent_id.as_str())
            && event.data["work_item_id"].as_str() == Some(work.id.as_str())
            && event.data["resource"].as_str() == Some("holon-run/holon#1079")
            && event.data["trigger_count"].as_u64() == Some(1)
    }));

    let completed = runtime
        .complete_work_item(work.id.clone(), Some("CI confirmed success".into()))
        .await
        .unwrap();
    assert_eq!(completed.state, WorkItemState::Completed);
    assert!(completed.blocked_by.is_none());
    let waiting = runtime.latest_waiting_intents().await.unwrap();
    let waiting = waiting
        .iter()
        .find(|record| record.id == capability.waiting_intent_id)
        .expect("waiting intent should remain auditable after completion");
    assert_eq!(waiting.status, WaitingIntentStatus::Cancelled);
}

#[tokio::test]
async fn completing_work_item_cancels_work_item_scoped_external_trigger() {
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

    let work = runtime
        .create_work_item("wait for external review".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let capability = runtime
        .create_external_trigger(
            "wait for review".into(),
            "github".into(),
            ExternalTriggerScope::WorkItem,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();

    runtime
        .complete_work_item(work.id.clone(), Some("review no longer needed".into()))
        .await
        .unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    let descriptor = runtime
        .latest_external_triggers()
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.external_trigger_id == capability.external_trigger_id)
        .expect("external trigger descriptor should exist");
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, capability.waiting_intent_id);
    assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
    assert_eq!(descriptor.status, ExternalTriggerStatus::Revoked);

    let result = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Text {
                    text: "late callback".into(),
                }),
                content_type: Some("text/plain".into()),
                correlation_id: None,
                causation_id: None,
            },
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("not active"));
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "work_item_waiting_intents_cancelled"
            && event.data["reason"] == "work_item_completed"
            && event.data["work_item_id"].as_str() == Some(work.id.as_str())
    }));
}

#[tokio::test]
async fn replacing_work_item_trigger_revokes_previous_waiting_condition() {
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

    let work = runtime
        .create_work_item("replace external condition".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let old_capability = runtime
        .create_external_trigger(
            "wait for CI".into(),
            "github".into(),
            ExternalTriggerScope::WorkItem,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let new_capability = runtime
        .create_external_trigger(
            "wait for review approval".into(),
            "github".into(),
            ExternalTriggerScope::WorkItem,
            CallbackDeliveryMode::EnqueueMessage,
            None,
            None,
        )
        .await
        .unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    let descriptors = runtime.latest_external_triggers().await.unwrap();
    assert_eq!(
        waiting
            .iter()
            .filter(|record| record.status == WaitingIntentStatus::Active)
            .count(),
        1
    );
    assert!(waiting.iter().any(|record| {
        record.id == old_capability.waiting_intent_id
            && record.status == WaitingIntentStatus::Cancelled
    }));
    assert!(waiting.iter().any(|record| {
        record.id == new_capability.waiting_intent_id
            && record.status == WaitingIntentStatus::Active
    }));
    assert!(descriptors.iter().any(|record| {
        record.external_trigger_id == old_capability.external_trigger_id
            && record.status == ExternalTriggerStatus::Revoked
    }));
    assert!(descriptors.iter().any(|record| {
        record.external_trigger_id == new_capability.external_trigger_id
            && record.status == ExternalTriggerStatus::Active
    }));
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "work_item_waiting_intents_cancelled"
            && event.data["reason"] == "waiting_condition_replaced"
            && event.data["work_item_id"].as_str() == Some(work.id.as_str())
    }));
}

#[tokio::test]
async fn picking_new_work_item_cancels_previous_work_item_scoped_trigger_only() {
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

    let old_work = runtime
        .create_work_item("old waiting condition".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(old_work.id.clone()).await.unwrap();
    let old_work_trigger = runtime
        .create_external_trigger(
            "wait for old work event".into(),
            "github".into(),
            ExternalTriggerScope::WorkItem,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let agent_trigger = runtime
        .create_external_trigger(
            "watch durable inbox".into(),
            "agentinbox".into(),
            ExternalTriggerScope::Agent,
            CallbackDeliveryMode::WakeHint,
            None,
            None,
        )
        .await
        .unwrap();
    let new_work = runtime
        .create_work_item("new active condition".into(), None, None, Vec::new())
        .await
        .unwrap();

    runtime.pick_work_item(new_work.id.clone()).await.unwrap();

    let waiting = runtime.latest_waiting_intents().await.unwrap();
    assert!(waiting.iter().any(|record| {
        record.id == old_work_trigger.waiting_intent_id
            && record.status == WaitingIntentStatus::Cancelled
    }));
    assert!(waiting.iter().any(|record| {
        record.id == agent_trigger.waiting_intent_id
            && record.scope == ExternalTriggerScope::Agent
            && record.status == WaitingIntentStatus::Active
    }));
    let events = runtime.storage().read_recent_events(20).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "work_item_waiting_intents_cancelled"
            && event.data["reason"] == "active_work_item_switched"
            && event.data["work_item_id"].as_str() == Some(old_work.id.as_str())
    }));
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

    let old_work = runtime
        .create_work_item("old objective".into(), None, None, Vec::new())
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
    let new_work = runtime
        .create_work_item("new objective".into(), None, None, Vec::new())
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
    assert!(events.iter().any(|event| {
        event.kind == "work_item_waiting_intents_cancelled"
            && event.data["reason"] == "work_item_completed"
            && event.data["work_item_id"].as_str() == Some(old_work.id.as_str())
    }));
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

    let old_work = runtime
        .create_work_item("old objective".into(), None, None, Vec::new())
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
    let new_work = runtime
        .create_work_item("new objective".into(), None, None, Vec::new())
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
async fn agent_scoped_waiting_intent_contributes_to_closure() {
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

    runtime
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

    let closure = runtime.current_closure_decision().await.unwrap();
    assert_eq!(closure.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        closure.waiting_reason,
        Some(WaitingReason::AwaitingExternalChange)
    );
    assert!(closure
        .evidence
        .iter()
        .any(|item| item == "active_waiting_intents=1"));
}

#[tokio::test]
async fn current_closure_ignores_other_agent_timers() {
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
    runtime
        .inner
        .storage
        .append_timer(&TimerRecord {
            id: "other-timer".into(),
            agent_id: "other".into(),
            created_at: Utc::now(),
            duration_ms: 1000,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: None,
            next_fire_at: Some(Utc::now()),
            last_fired_at: None,
            fire_count: 0,
        })
        .unwrap();

    let closure = runtime.current_closure_decision().await.unwrap();

    assert_ne!(closure.waiting_reason, Some(WaitingReason::AwaitingTimer));
    assert!(!closure
        .evidence
        .iter()
        .any(|evidence| evidence == "active_timers=1"));
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

    let waiting_work = runtime
        .create_work_item("waiting-only objective".into(), None, None, Vec::new())
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

    let work = runtime
        .create_work_item("newly anchored objective".into(), None, None, Vec::new())
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
async fn current_needs_input_work_item_waits_for_operator_without_work_signal() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut active = WorkItemRecord::new(
        "default",
        "choose implementation direction",
        WorkItemState::Open,
    );
    active.plan_status = WorkItemPlanStatus::NeedsInput;
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
    assert_eq!(closure.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        closure.waiting_reason,
        Some(WaitingReason::AwaitingOperatorInput)
    );
    assert!(closure.work_signal.is_none());
    assert!(closure
        .evidence
        .iter()
        .any(|item| item == "awaiting_operator_input_signal"));

    let emitted = runtime.maybe_emit_pending_system_tick(None).await.unwrap();
    assert!(
        !emitted,
        "needs_input current work item must not auto-resume through work_queue"
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
async fn queued_needs_input_work_item_is_not_runnable() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut queued =
        WorkItemRecord::new("default", "waiting planning candidate", WorkItemState::Open);
    queued.plan_status = WorkItemPlanStatus::NeedsInput;
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
    assert_eq!(closure.outcome, ClosureOutcome::Completed);
    assert_eq!(closure.waiting_reason, None);
    assert!(closure.work_signal.is_none());

    let emitted = runtime.maybe_emit_pending_system_tick(None).await.unwrap();
    assert!(
        !emitted,
        "queued needs_input work item should not emit queued_available"
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
