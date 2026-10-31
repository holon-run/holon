use super::*;
use crate::{
    config::AppConfig,
    provider::StubProvider,
    types::{
        AgentState, AuthorityClass, TaskKind, TaskRecord, TaskStatus, TimerRecord,
        ToolExecutionRecord, ToolExecutionStatus, WorkItemRecord, WorkItemState,
    },
};
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use tower::ServiceExt;

async fn request(
    state: &AppState,
    path: &str,
    expected_incarnation: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().uri(path);
    if let Some(expected) = expected_incarnation {
        builder = builder.header("x-holon-expected-incarnation", expected);
    }
    let response = crate::http::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn task_and_timer_list_http_incarnation_binding_preserves_legacy_and_recreation_isolation() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
    host.create_named_agent("list-agent", None).await.unwrap();
    host.unload_runtime("list-agent").await;
    let now = chrono::Utc::now();
    for index in 0..23 {
        let task: TaskRecord = serde_json::from_value(serde_json::json!({
            "id": format!("task-list-{index:02}"), "agent_id": "list-agent",
            "kind": "command_task", "status": if index == 0 { "running" } else { "completed" },
            "created_at": now, "updated_at": now, "summary": "original"
        }))
        .unwrap();
        host.runtime_db().tasks().upsert(&task).unwrap();
    }
    for index in 0..3 {
        let timer: TimerRecord = serde_json::from_value(serde_json::json!({
            "id": format!("timer-list-{index}"), "agent_id": "list-agent", "created_at": now,
            "duration_ms": 1000, "repeat": false, "summary": "original"
        }))
        .unwrap();
        host.runtime_db().timers().upsert(&timer).unwrap();
    }
    let storage = host.operator_agent_read_storage("list-agent").unwrap();
    let events_before = storage.read_recent_events(100).unwrap();
    let state = AppState::for_tcp(host.clone());
    let paths = [
        "/api/agents/list-agent/tasks",
        "/api/agents/list-agent/tasks?status=terminal&limit=100",
        "/api/agents/list-agent/timers",
        "/api/agents/list-agent/timers?limit=1",
    ];
    let task_dtos = |tasks: Vec<TaskRecord>| {
        serde_json::to_value(
            tasks
                .into_iter()
                .map(crate::http_dto::TaskRecordDto::from)
                .collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let legacy = [
        task_dtos(storage.latest_active_task_records(50).unwrap()),
        task_dtos(
            storage
                .latest_terminal_task_records_for_agent("list-agent", 20)
                .unwrap(),
        ),
        serde_json::to_value(storage.read_recent_timers(50).unwrap()).unwrap(),
        serde_json::to_value(storage.read_recent_timers(1).unwrap()).unwrap(),
    ];
    assert_eq!(legacy[1].as_array().unwrap().len(), 20);
    assert_eq!(legacy[3].as_array().unwrap().len(), 1);
    for (path, legacy) in paths.iter().zip(&legacy) {
        for expected in [None, Some("1")] {
            let (status, headers, body) = request(&state, path, expected).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(headers["x-holon-agent-id"], "list-agent");
            assert_eq!(headers["x-holon-agent-incarnation"], "1");
            assert_eq!(&body, legacy, "legacy payload/order must not change");
        }
        for expected in ["2", "invalid", "0", "", "-1", "18446744073709551616"] {
            let (status, headers, body) = request(&state, path, Some(expected)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}: {expected}");
            assert_eq!(body["code"], "conversation_identity_changed");
            assert_eq!(body["retryable"], false);
            assert!(!headers.contains_key("x-holon-agent-incarnation"));
        }
        let mut authenticated = state.clone();
        authenticated.require_control_token = true;
        assert_eq!(
            request(&authenticated, path, Some("invalid")).await.0,
            StatusCode::UNAUTHORIZED,
            "authentication must precede incarnation validation"
        );
    }
    assert_eq!(storage.read_recent_events(100).unwrap(), events_before);
    assert!(host.try_get_loaded_runtime("list-agent").await.is_none());
    host.runtime_db()
        .transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET payload_json = json_set(payload_json, '$.incarnation', 2)
                 WHERE agent_id = 'list-agent'",
                [],
            )?;
            for table in ["tasks", "timers"] {
                tx.execute(
                    &format!(
                        "UPDATE {table} SET payload_json =
                         json_set(payload_json, '$.summary', 'replacement-only')"
                    ),
                    [],
                )?;
            }
            Ok(())
        })
        .unwrap();
    for path in paths {
        let (status, _, body) = request(&state, path, Some("1")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "conversation_identity_changed");
        assert!(!body.to_string().contains("replacement-only"));
        for expected in [None, Some("2")] {
            let (status, headers, body) = request(&state, path, expected).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers["x-holon-agent-incarnation"], "2");
            assert!(body.to_string().contains("replacement-only"));
        }
    }
    assert!(host.try_get_loaded_runtime("list-agent").await.is_none());
}

#[tokio::test]
async fn work_queue_http_incarnation_binding_preserves_legacy_reads_without_runtime_or_writes() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
    host.create_named_agent("queue-agent", None).await.unwrap();
    host.unload_runtime("queue-agent").await;
    let item = WorkItemRecord::new("queue-agent", "original", WorkItemState::Open);
    let second = WorkItemRecord::new("queue-agent", "second", WorkItemState::Open);
    host.runtime_db().work_items().insert_new(&item).unwrap();
    host.runtime_db().work_items().insert_new(&second).unwrap();
    let mut agent = AgentState::new("queue-agent");
    agent.current_work_item_id = Some(item.id.clone());
    host.runtime_db().agent_states().upsert(&agent).unwrap();
    let storage = host.operator_agent_read_storage("queue-agent").unwrap();
    let legacy = storage.work_queue_read_model().unwrap();
    let events_before = storage.read_recent_events(100).unwrap();
    let state = AppState::for_tcp(host.clone());
    let list = "/api/agents/queue-agent/work-items";
    let detail = format!("{list}/{}", item.id);
    assert!(host.try_get_loaded_runtime("queue-agent").await.is_none());

    for path in [list, detail.as_str()] {
        for expected in [None, Some("1")] {
            let (status, headers, body) = request(&state, path, expected).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(headers["x-holon-agent-id"], "queue-agent");
            assert_eq!(headers["x-holon-agent-incarnation"], "1");
            let expected_body = if path == list {
                serde_json::to_value(&legacy.items).unwrap()
            } else {
                serde_json::to_value(
                    legacy
                        .items
                        .iter()
                        .find(|entry| entry.id == item.id)
                        .unwrap(),
                )
                .unwrap()
            };
            assert_eq!(body, expected_body, "legacy projection must not change");
        }
        for expected in ["2", "invalid", "0", "", "-1", "18446744073709551616"] {
            let (status, headers, body) = request(&state, path, Some(expected)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}: {expected}");
            assert_eq!(body["code"], "conversation_identity_changed");
            assert_eq!(body["retryable"], false);
            assert!(!headers.contains_key("x-holon-agent-incarnation"));
        }
    }
    let (_, _, limited) = request(&state, &format!("{list}?limit=1"), Some("1")).await;
    assert_eq!(limited.as_array().unwrap().len(), 1);
    let missing = format!("{list}/missing");
    assert_eq!(
        request(&state, &missing, Some("1")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&state, &missing, Some("2")).await.0,
        StatusCode::CONFLICT
    );
    let mut authenticated = state.clone();
    authenticated.require_control_token = true;
    assert_eq!(
        request(&authenticated, list, Some("invalid")).await.0,
        StatusCode::UNAUTHORIZED,
        "authentication must precede incarnation validation"
    );
    assert_eq!(
        storage.read_recent_events(100).unwrap(),
        events_before,
        "GET must not append business events"
    );
    assert_eq!(storage.read_agent().unwrap(), Some(agent));
    assert_eq!(
        host.runtime_db().work_items().latest(&item.id).unwrap(),
        Some(item.clone())
    );
    assert!(host.try_get_loaded_runtime("queue-agent").await.is_none());

    host.runtime_db()
        .transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET payload_json = json_set(payload_json, '$.incarnation', 2)
                 WHERE agent_id = 'queue-agent'",
                [],
            )?;
            tx.execute(
                "UPDATE work_items SET objective = 'replacement-only',
                 payload_json = json_set(payload_json, '$.objective', 'replacement-only')
                 WHERE agent_id = 'queue-agent'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    for path in [list, detail.as_str()] {
        let (status, _, body) = request(&state, path, Some("1")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "conversation_identity_changed");
        assert!(!body.to_string().contains("replacement-only"));
        for expected in [None, Some("2")] {
            let (status, headers, body) = request(&state, path, expected).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers["x-holon-agent-incarnation"], "2");
            assert!(body.to_string().contains("replacement-only"));
        }
    }
    assert!(host.try_get_loaded_runtime("queue-agent").await.is_none());
}

#[tokio::test]
async fn task_detail_http_incarnation_binding_fences_status_output_and_tool_execution() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
    host.create_named_agent("detail-agent", None).await.unwrap();
    host.unload_runtime("detail-agent").await;
    let now = chrono::Utc::now();
    let task = TaskRecord {
        id: "task-detail-1".into(),
        agent_id: "detail-agent".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("terminal detail".into()),
        detail: None,
        recovery: None,
    };
    host.runtime_db().tasks().upsert(&task).unwrap();
    host.runtime_db()
        .evidence()
        .append_tool_execution(&ToolExecutionRecord {
            id: "tool-detail-1".into(),
            agent_id: "detail-agent".into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: None,
            tool_name: "ExecCommand".into(),
            created_at: now,
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({"cmd": "echo ok"}),
            output: serde_json::json!({"exit_status": 0}),
            summary: "detail read".into(),
            invocation_surface: None,
        })
        .unwrap();
    assert!(host.try_get_loaded_runtime("detail-agent").await.is_none());
    let state = AppState::for_tcp(host.clone());
    let status_path = "/api/agents/detail-agent/tasks/task-detail-1";
    let output_path = "/api/agents/detail-agent/tasks/task-detail-1/output";
    let tool_path = "/api/agents/detail-agent/tool-executions/tool-detail-1";

    for expected in [None, Some("1")] {
        let (status, headers, body) = request(&state, status_path, expected).await;
        assert_eq!(status, StatusCode::OK, "status: {expected:?}");
        assert_eq!(headers["x-holon-agent-id"], "detail-agent");
        assert_eq!(headers["x-holon-agent-incarnation"], "1");
        assert_eq!(body["task_id"], "task-detail-1");
        assert_eq!(body["summary"], "terminal detail");

        let (status, headers, body) = request(&state, tool_path, expected).await;
        assert_eq!(status, StatusCode::OK, "tool: {expected:?}");
        assert_eq!(headers["x-holon-agent-id"], "detail-agent");
        assert_eq!(headers["x-holon-agent-incarnation"], "1");
        assert_eq!(body["id"], "tool-detail-1");
        assert_eq!(body["agent_id"], "detail-agent");

        // The runtime stays unloaded, so an identity-admitted output read
        // must still refuse the payload with task_not_live.
        let (status, _, body) = request(&state, output_path, expected).await;
        assert_eq!(status, StatusCode::CONFLICT, "output: {expected:?}");
        assert_eq!(body["code"], "task_not_live");
    }
    for expected in ["2", "invalid", "0"] {
        for path in [status_path, output_path, tool_path] {
            let (status, headers, body) = request(&state, path, Some(expected)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}: {expected}");
            assert_eq!(body["code"], "conversation_identity_changed");
            assert!(!headers.contains_key("x-holon-agent-incarnation"));
        }
    }
    let missing_status = "/api/agents/detail-agent/tasks/missing";
    let missing_tool = "/api/agents/detail-agent/tool-executions/missing";
    assert_eq!(
        request(&state, missing_status, Some("1")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&state, missing_status, Some("2")).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&state, missing_tool, Some("1")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&state, missing_tool, Some("2")).await.0,
        StatusCode::CONFLICT
    );

    host.runtime_db()
        .transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET payload_json = json_set(payload_json, '$.incarnation', 2)
                 WHERE agent_id = 'detail-agent'",
                [],
            )?;
            tx.execute(
                "UPDATE tasks SET payload_json = json_set(payload_json, '$.summary', 'replacement-only')
                 WHERE task_id = 'task-detail-1'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    for path in [status_path, output_path, tool_path] {
        let (status, _, body) = request(&state, path, Some("1")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}");
        assert_eq!(body["code"], "conversation_identity_changed");
        assert!(!body.to_string().contains("replacement-only"));
    }
    let (status, headers, body) = request(&state, status_path, Some("2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-holon-agent-incarnation"], "2");
    assert_eq!(body["summary"], "replacement-only");
    let (status, headers, _) = request(&state, tool_path, Some("2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-holon-agent-incarnation"], "2");
    assert!(host.try_get_loaded_runtime("detail-agent").await.is_none());
}
