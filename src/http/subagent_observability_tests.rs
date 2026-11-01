use super::*;
use crate::types::{
    AgentMessagePolicyRecord, AgentPolicyEffect, AgentRegistryStatus, TaskKind, TaskRecord,
    TaskStatus,
};
use axum::{body::to_bytes, http::Request};
use tower::ServiceExt;

use super::tests::{control_token_test_host, invoke_new_subagent};

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .unwrap()
}

fn prompt(agent_id: &str, body: Value, authorized: bool) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/api/control/agents/{agent_id}/prompt"))
        .header(CONTENT_TYPE, "application/json");
    if authorized {
        request = request.header(AUTHORIZATION, "Bearer secret");
    }
    request.body(Body::from(body.to_string())).unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn private_child_operator_prompt_preserves_delivery_and_parent_contracts() {
    let (_home, host) = control_token_test_host();
    let parent = host.default_runtime().await.unwrap();
    let spawned = invoke_new_subagent(
        &parent,
        "delegated work".into(),
        AuthorityClass::OperatorInstruction,
        None,
        None,
    )
    .await
    .unwrap();
    let child_id = &spawned.agent_id;
    let task_before = parent
        .storage()
        .latest_task_record(&spawned.task_handle.task_id)
        .unwrap()
        .unwrap();
    let relations_before = host
        .runtime_db()
        .agent_canonical_relations()
        .latest(child_id)
        .unwrap();
    let child = host.try_get_loaded_runtime(child_id).await.unwrap();
    let app = router(AppState::for_tcp(host.clone()));
    for target in [child_id.as_str(), "missing-agent"] {
        let response = app
            .clone()
            .oneshot(prompt(target, json!({"text": "hello"}), false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(matches!(
        host.get_public_agent_for_external_ingress(child_id).await,
        Err(PublicAgentError::Private { .. })
    ));
    let request = json!({
        "text": "operator correction",
        "expected_incarnation": host.agent_identity_record(child_id).unwrap().unwrap().incarnation,
        "client_request_id": "private-child-input",
        "attachments": [{
            "kind": "file",
            "name": "notes.txt",
            "media_type": "text/plain",
            "data_base64": "bm90ZXM="
        }]
    });
    let first = app
        .clone()
        .oneshot(prompt(child_id, request.clone(), true))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = json_body(first).await;
    let replay = app
        .clone()
        .oneshot(prompt(child_id, request, true))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay = json_body(replay).await;
    assert_eq!(first["message_id"], replay["message_id"]);
    assert_eq!(replay["disposition"], "duplicate");
    let conflict = app
        .clone()
        .oneshot(prompt(
            child_id,
            json!({
                "text": "different content", "client_request_id": "private-child-input"
            }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(conflict).await["code"], "idempotency_conflict");
    let messages = child.storage().read_recent_messages(100).unwrap();
    let message = messages
        .iter()
        .find(|message| message.id == first["message_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(message.kind, MessageKind::OperatorPrompt);
    assert_eq!(message.authority_class, AuthorityClass::OperatorInstruction);
    assert_eq!(message.priority, Priority::Interject);
    assert_eq!(
        message.delivery_surface,
        Some(MessageDeliverySurface::HttpControlPrompt)
    );
    assert!(matches!(message.origin, MessageOrigin::Operator { .. }));
    let delivery = host
        .runtime_db()
        .agent_message_deliveries()
        .latest_for_message(&message.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        delivery.caller.principal_kind,
        crate::types::AgentMessagePrincipalKind::Operator
    );
    assert_eq!(delivery.caller.caller_agent_id, None);
    assert_eq!(delivery.caller.route, "operator_control");
    assert_eq!(
        std::fs::read_dir(child.agent_home().join("media/inbox"))
            .unwrap()
            .count(),
        1
    );
    let task_after = parent
        .storage()
        .latest_task_record(&spawned.task_handle.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(task_after.agent_id, task_before.agent_id);
    assert_eq!(task_after.work_item_id, task_before.work_item_id);
    assert_eq!(task_after.target(), task_before.target());
    assert_eq!(task_after.recovery, task_before.recovery);
    assert_eq!(
        host.runtime_db()
            .agent_canonical_relations()
            .latest(child_id)
            .unwrap(),
        relations_before
    );

    let unkeyed = app
        .oneshot(prompt(
            child_id,
            json!({"text": "unkeyed operator input"}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(unkeyed.status(), StatusCode::OK);
    assert!(json_body(unkeyed).await["disposition"].is_null());
}

#[tokio::test]
async fn private_child_prompt_rejects_policy_stop_and_identity_fences() {
    let (_home, host) = control_token_test_host();
    let parent = host.default_runtime().await.unwrap();
    let spawned = invoke_new_subagent(
        &parent,
        "work".into(),
        AuthorityClass::OperatorInstruction,
        None,
        None,
    )
    .await
    .unwrap();
    let child_id = &spawned.agent_id;
    let child = host.try_get_loaded_runtime(child_id).await.unwrap();
    let app = router(AppState::for_tcp(host.clone()));
    let count = child.storage().read_recent_messages(100).unwrap().len();
    let incarnation = host
        .agent_identity_record(child_id)
        .unwrap()
        .unwrap()
        .incarnation;
    let response = app
        .clone()
        .oneshot(prompt(
            child_id,
            json!({"text": "stale page", "expected_incarnation": incarnation + 1}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(response).await["code"],
        "agent_incarnation_mismatch"
    );
    assert_eq!(
        child.storage().read_recent_messages(100).unwrap().len(),
        count
    );
    host.runtime_db()
        .agent_canonical_relations()
        .upsert_message_policy(&AgentMessagePolicyRecord {
            agent_id: child_id.clone(),
            revision: 100,
            default_effect: AgentPolicyEffect::Deny,
            rules: Vec::new(),
            created_at: Utc::now(),
        })
        .unwrap();
    for body in [
        json!({"text": "denied"}),
        json!({"text": "denied", "client_request_id": "denied-key"}),
    ] {
        let response = app
            .clone()
            .oneshot(prompt(child_id, body, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(json_body(response).await["code"], "message_not_authorized");
    }
    assert_eq!(
        child.storage().read_recent_messages(100).unwrap().len(),
        count
    );
    child
        .control(crate::types::ControlAction::Stop)
        .await
        .unwrap();
    host.unload_runtime(child_id).await;
    let response = app
        .clone()
        .oneshot(prompt(child_id, json!({"text": "must not start"}), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(response).await["code"], "agent_stopped");
    assert!(host.try_get_loaded_runtime(child_id).await.is_none());
    for (status, expected, code) in [
        (
            AgentRegistryStatus::Deleting,
            StatusCode::CONFLICT,
            "agent_deleting",
        ),
        (
            AgentRegistryStatus::Deleted,
            StatusCode::GONE,
            "agent_deleted",
        ),
    ] {
        let mut identity = host.agent_identity_record(child_id).unwrap().unwrap();
        identity.status = status;
        identity.revision += 1;
        host.append_agent_identity(&identity).unwrap();
        host.runtime_db()
            .agent_identities()
            .upsert(&identity)
            .unwrap();
        let unauthorized = app
            .clone()
            .oneshot(prompt(child_id, json!({"text": "blocked"}), false))
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(prompt(child_id, json!({"text": "blocked"}), true))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(json_body(response).await["code"], code);
        assert!(host.try_get_loaded_runtime(child_id).await.is_none());
    }
}

#[tokio::test]
async fn task_targets_are_pinned_and_shared_by_state_list_and_detail() {
    let (_home, host) = control_token_test_host();
    let parent = host.default_runtime().await.unwrap();
    let parent_id = host.config().default_agent_id.clone();
    let spawned = invoke_new_subagent(
        &parent,
        "work".into(),
        AuthorityClass::OperatorInstruction,
        None,
        None,
    )
    .await
    .unwrap();
    let mut identity = host
        .agent_identity_record(&spawned.agent_id)
        .unwrap()
        .unwrap();
    let expected = json!({"kind": "child_agent", "agent_id": spawned.agent_id, "incarnation": identity.incarnation});
    let mut record = parent
        .storage()
        .latest_task_record(&spawned.task_handle.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(record.target().unwrap()).unwrap(),
        expected
    );
    // A projection-only record keeps the assertion independent of child completion.
    record.id = "pinned-projection".into();
    record.status = TaskStatus::Running;
    record.detail.as_mut().unwrap()["rejoin_obligation_id"] = json!(record.id);
    let task_id = &record.id;
    crate::runtime::tests::support::persist_task_transition(
        &parent,
        &record,
        "http_pinned_task_fixture",
    )
    .await
    .unwrap();
    // Simulate a later identity with the same id; historical tasks must not re-resolve it.
    identity.incarnation += 1;
    identity.revision += 1;
    host.runtime_db()
        .agent_identities()
        .upsert(&identity)
        .unwrap();
    // Publish the replacement to the host registry as well as SQL.
    host.append_agent_identity(&identity).unwrap();
    let app = router(AppState::for_tcp(host.clone()));
    let list = json_body(
        app.clone()
            .oneshot(get(&format!("/api/agents/{parent_id}/tasks")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        list.as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == *task_id)
            .unwrap()["target"],
        expected
    );
    let response = app
        .clone()
        .oneshot(get(&format!("/api/agents/{parent_id}/tasks/{task_id}")))
        .await
        .unwrap();
    let status = response.status();
    let detail = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "TaskStatus response: {detail}");
    assert_eq!(detail["target"], expected);
    // Read the persisted projection without a live runtime cache or activation.
    host.unload_runtime(&parent_id).await;
    let state = json_body(
        app.clone()
            .oneshot(get(&format!("/api/agents/{parent_id}/state")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == *task_id)
            .unwrap()["target"],
        expected
    );
    host.unload_runtime(&spawned.agent_id).await;
    for uri in [
        format!("/api/agents/{}/state", spawned.agent_id),
        format!("/api/control/agents/{}/detail", spawned.agent_id),
        format!("/api/agents/{}/tasks?status=terminal", spawned.agent_id),
    ] {
        let response = app.clone().oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert!(host
            .try_get_loaded_runtime(&spawned.agent_id)
            .await
            .is_none());
    }
}

#[tokio::test]
async fn terminal_task_list_is_scoped_recent_and_bounded() {
    let (_home, host) = control_token_test_host();
    let parent = host.default_runtime().await.unwrap();
    let parent_id = host.config().default_agent_id.clone();
    let now = Utc::now();
    for index in 0..26 {
        let task = TaskRecord {
            id: format!("history-{index:02}"),
            agent_id: parent_id.clone(),
            kind: TaskKind::CommandTask,
            status: if index == 25 {
                TaskStatus::Running
            } else {
                [
                    TaskStatus::Completed,
                    TaskStatus::Failed,
                    TaskStatus::Cancelled,
                    TaskStatus::Interrupted,
                ][index % 4]
                    .clone()
            },
            created_at: now,
            updated_at: now + chrono::Duration::seconds(index as i64),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        };
        crate::runtime::tests::support::persist_task_transition(
            &parent,
            &task,
            "http_terminal_task_fixture",
        )
        .await
        .unwrap();
        host.runtime_db()
            .tasks()
            .upsert(&TaskRecord {
                id: format!("other-{index}"),
                agent_id: "other".into(),
                ..task
            })
            .unwrap();
    }
    let app = router(AppState::for_tcp(host.clone()));
    for suffix in ["?status=terminal", "?status=terminal&limit=1000"] {
        let tasks = json_body(
            app.clone()
                .oneshot(get(&format!("/api/agents/{parent_id}/tasks{suffix}")))
                .await
                .unwrap(),
        )
        .await;
        let tasks = tasks.as_array().unwrap();
        assert_eq!(tasks.len(), 20);
        assert_eq!(tasks.first().unwrap()["id"], "history-24");
        assert_eq!(tasks.last().unwrap()["id"], "history-05");
        assert!(tasks
            .iter()
            .all(|task| task["agent_id"] == parent_id && task.get("target").is_none()));
    }
    for (suffix, expected) in [
        ("", 1),
        ("?status=active", 1),
        ("?status=terminal&limit=2", 2),
        ("?status=terminal&limit=0", 0),
    ] {
        let tasks = json_body(
            app.clone()
                .oneshot(get(&format!("/api/agents/{parent_id}/tasks{suffix}")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(tasks.as_array().unwrap().len(), expected);
    }
    let invalid = app
        .oneshot(get(&format!(
            "/api/agents/{parent_id}/tasks?status=unknown"
        )))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}
