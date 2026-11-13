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
async fn active_state_inventory_and_explicit_history_use_separate_identity_fenced_reads() {
    use crate::runtime::resident_scheduling::{SchedulingFamily, SchedulingKey};
    use crate::runtime_db::scheduling_snapshot::{
        prepare_scheduling_write_tx, register_scheduling_writes_tx,
    };
    use crate::types::TimerStatus;
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused"))).unwrap();
    host.create_named_agent("inventory", None).await.unwrap();
    host.unload_runtime("inventory").await;
    let mut current = None;
    let mut terminal_id = None;
    for (index, status) in [
        WorkItemState::Completed,
        WorkItemState::Completing,
        WorkItemState::Open,
        WorkItemState::Open,
    ]
    .into_iter()
    .enumerate()
    {
        let item = WorkItemRecord::new("inventory", format!("item-{index}"), status.clone());
        if status == WorkItemState::Completed {
            terminal_id = Some(item.id.clone());
        }
        if index == 3 {
            current = Some(item.id.clone());
        }
        host.runtime_db().work_items().insert_new(&item).unwrap();
    }
    let mut agent = AgentState::new("inventory");
    agent.current_work_item_id = current.clone();
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&agent)
        .unwrap();
    let mut timers = Vec::new();
    for (index, status) in [
        TimerStatus::Active,
        TimerStatus::Completed,
        TimerStatus::Cancelled,
    ]
    .into_iter()
    .enumerate()
    {
        let timer: TimerRecord = serde_json::from_value(serde_json::json!({
            "id": format!("timer-{index}"), "agent_id": "inventory",
            "created_at": chrono::Utc::now(), "status": status,
            "duration_ms": 1000, "repeat": false
        }))
        .unwrap();
        timers.push(timer);
    }
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: "inventory".into(),
        identity_incarnation: 1,
    };
    let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        crate::config::StateMemoryLimits::default(),
    )
    .unwrap();
    let scheduling_request = crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
        capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    let keys = timers
        .iter()
        .map(|timer| SchedulingKey::new(SchedulingFamily::Timer, &timer.id))
        .collect();
    host.runtime_db()
        .transaction(|tx| {
            let proof =
                prepare_scheduling_write_tx(tx, &scope, &keys, &scheduling_request, &ledger)?;
            for timer in &timers {
                crate::runtime_db::repositories::upsert_timer_tx(tx, timer)?;
            }
            register_scheduling_writes_tx(tx, &proof, &keys, &agent, &scheduling_request)
        })
        .unwrap();
    let state = AppState::for_unix(host.clone());
    let (status, _, body) = request(&state, "/api/agents/inventory/state", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["active_work_item_count"], 3);
    assert_eq!(body["active_timer_count"], 1);
    assert_eq!(body["work_items"].as_array().unwrap().len(), 3);
    assert_eq!(body["work_items"][0]["id"].as_str(), current.as_deref());
    assert!(body["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["state"] != "completed"));
    assert_eq!(body["timers"].as_array().unwrap().len(), 1);
    assert_eq!(body["timers"][0]["id"], "timer-0");
    for (path, count) in [
        ("work-items?status=active", 3),
        ("work-items?status=terminal&limit=1", 1),
        ("timers?status=active&limit=1", 1),
        ("timers?status=terminal", 2),
    ] {
        let path = format!("/api/agents/inventory/{path}");
        let (status, headers, body) = request(&state, &path, Some("1")).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(headers["x-holon-agent-incarnation"], "1");
        assert_eq!(body.as_array().unwrap().len(), count);
        if path.contains("work-items?status=terminal") {
            assert_eq!(body[0]["id"].as_str(), terminal_id.as_deref());
        }
        let (status, _, _) = request(&state, &path, Some("2")).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    let response = crate::http::router(state)
        .oneshot(
            Request::builder()
                .uri("/api/agents/inventory/work-items?status=bogus")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(host.try_get_loaded_runtime("inventory").await.is_none());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_work_item_history_detail_is_not_limited_by_the_queue_window() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused"))).unwrap();
    host.create_named_agent("history", None).await.unwrap();
    host.unload_runtime("history").await;
    let mut completed = WorkItemRecord::new("history", "old history", WorkItemState::Completed);
    completed.created_at -= chrono::Duration::days(1);
    completed.updated_at = completed.created_at;
    host.runtime_db()
        .work_items()
        .insert_new(&completed)
        .unwrap();
    for index in 0..128 {
        let item = WorkItemRecord::new(
            "history",
            format!("newer completing {index}"),
            WorkItemState::Completing,
        );
        host.runtime_db().work_items().insert_new(&item).unwrap();
    }
    let foreign = WorkItemRecord::new("other", "foreign history", WorkItemState::Completed);
    host.runtime_db().work_items().insert_new(&foreign).unwrap();
    let state = AppState::for_unix(host.clone());
    let (status, _, body) = request(
        &state,
        "/api/agents/history/work-items?status=terminal&limit=1",
        Some("1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["id"], completed.id);
    let path = format!("/api/agents/history/work-items/{}", completed.id);
    let (status, headers, body) = request(&state, &path, Some("1")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers["x-holon-agent-incarnation"], "1");
    assert_eq!(body["id"], completed.id);
    assert_eq!(body["state"], "completed");
    assert_eq!(body["scheduling_state"], "completed");
    assert_eq!(body["focus"], "completed");
    let (status, _, _) = request(&state, &path, Some("2")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    for id in [foreign.id.as_str(), "missing"] {
        let (status, _, _) = request(
            &state,
            &format!("/api/agents/history/work-items/{id}"),
            Some("1"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    assert!(host.try_get_loaded_runtime("history").await.is_none());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn work_item_details_preserve_scheduling_and_mixed_list_ranking() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let host =
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused"))).unwrap();
    host.create_named_agent("scheduling", None).await.unwrap();
    host.unload_runtime("scheduling").await;
    let mut completing =
        WorkItemRecord::new("scheduling", "older completing", WorkItemState::Completing);
    completing.created_at -= chrono::Duration::days(1);
    completing.updated_at = completing.created_at;
    let completed = WorkItemRecord::new("scheduling", "newer completed", WorkItemState::Completed);
    for item in [&completing, &completed] {
        host.runtime_db().work_items().insert_new(item).unwrap();
    }
    let state = AppState::for_unix(host.clone());
    for query in ["limit=1", "status=all&limit=1"] {
        let (status, _, body) = request(
            &state,
            &format!("/api/agents/scheduling/work-items?{query}"),
            Some("1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["id"], completing.id);
    }

    let current = WorkItemRecord::new("scheduling", "current", WorkItemState::Open);
    let mut blocked = WorkItemRecord::new("scheduling", "blocked", WorkItemState::Open);
    blocked.blocked_by = Some("manual dependency".into());
    let mut waiting = WorkItemRecord::new("scheduling", "waiting", WorkItemState::Open);
    waiting.blocked_by = Some("operator decision".into());
    waiting.blocked_by_wait_id = Some("operator-wait".into());
    for item in [&current, &blocked, &waiting] {
        host.runtime_db().work_items().insert_new(item).unwrap();
    }
    let mut agent = AgentState::new("scheduling");
    agent.current_work_item_id = Some(current.id.clone());
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&agent)
        .unwrap();
    let now = chrono::Utc::now();
    host.runtime_db()
        .wait_conditions()
        .upsert(&crate::types::WaitConditionRecord {
            id: "operator-wait".into(),
            agent_id: "scheduling".into(),
            work_item_id: Some(waiting.id.clone()),
            status: crate::types::WaitConditionStatus::Active,
            kind: crate::types::WaitConditionKind::Operator,
            source: None,
            subject_ref: None,
            waiting_for: "operator decision".into(),
            wake_sources: vec![crate::types::WakeSource::OperatorInput],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        })
        .unwrap();
    let (status, _, list) = request(
        &state,
        "/api/agents/scheduling/work-items?status=active",
        Some("1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    for (item, scheduling_state, focus) in [
        (&current, "runnable", "current"),
        (&blocked, "blocked", "blocked"),
        (&waiting, "waiting_operator", "blocked"),
        (&completing, "completing", "completing"),
        (&completed, "completed", "completed"),
    ] {
        let (status, headers, body) = request(
            &state,
            &format!("/api/agents/scheduling/work-items/{}", item.id),
            Some("1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(headers["x-holon-agent-incarnation"], "1");
        assert_eq!(body["scheduling_state"], scheduling_state, "{body}");
        assert_eq!(body["focus"], focus, "{body}");
        assert!(!body["readiness"].is_null(), "{body}");
        if item.id == waiting.id {
            assert_eq!(body["readiness"], "waiting_for_operator");
            assert_eq!(body["reason_code"], "active_operator_wait");
            assert_eq!(body["active_wait_conditions"][0]["id"], "operator-wait");
        }
        if item.state != WorkItemState::Completed {
            let summary = list
                .as_array()
                .unwrap()
                .iter()
                .find(|summary| summary["id"] == item.id)
                .unwrap();
            assert_eq!(&body, summary);
        }
    }
    assert!(host.try_get_loaded_runtime("scheduling").await.is_none());
    host.shutdown().await.unwrap();
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
    host.runtime_db()
        .agent_states()
        .upsert_for_fixture(&agent)
        .unwrap();
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
            call_binding: None,
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
