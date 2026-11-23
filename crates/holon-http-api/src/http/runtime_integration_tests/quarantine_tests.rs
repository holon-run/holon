use super::*;
use crate::runtime::http_test_support::{
    reject_quarantined_constructor as reject_constructor, restart_fixture_host,
};
use crate::types::AgentMessageSendRequest;

#[tokio::test]
async fn quarantine_rejects_operator_control_and_peer_delivery_without_target_changes() {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host) = test_host();
    let target = "isolated-peer";
    host.create_named_agent(target, None).await.unwrap();
    let host = restart_fixture_host(host).await;
    let sender = host.default_runtime().await.unwrap();
    let storage = host.agent_storage(target).unwrap();
    let before_state = storage.read_agent().unwrap();
    let before_queue = storage.latest_queue_entries().unwrap();
    reject_constructor(&host, target);

    let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
    let response = app
        .oneshot(
            Request::post(format!("/api/control/agents/{target}/control"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"action":"start"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert!(body.contains("agent_quarantined"));
    assert!(!body.contains(target));

    let error = sender
        .agent_messaging_service()
        .send(
            AgentMessageSendRequest {
                target_agent_id: target.to_owned(),
                content: MessageBody::Text {
                    text: "synthetic peer delivery".into(),
                },
                client_idempotency_key: "quarantine-peer-test".into(),
                correlation_id: None,
                causation_id: None,
                requested_priority: Some(Priority::Normal),
                forward: false,
                reply_context_id: None,
            },
            AuthorityClass::RuntimeInstruction,
        )
        .await
        .err()
        .expect("peer delivery must not reach an isolated target");
    assert!(error.is::<crate::host::AgentQuarantined>(), "{error:#}");
    assert!(host.try_get_loaded_runtime(target).await.is_none());
    assert_eq!(storage.read_agent().unwrap(), before_state);
    assert_eq!(storage.latest_queue_entries().unwrap(), before_queue);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn quarantine_callback_routes_preserve_auth_and_do_not_enqueue() {
    use crate::types::{
        CallbackDeliveryMode, ExternalTriggerRecord, ExternalTriggerScope, ExternalTriggerStatus,
    };
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    // New default ingress is wake-only; seed a valid legacy record for enqueue.
    for (mode, correct_path, wrong_path) in [
        (CallbackDeliveryMode::WakeHint, "wake", "enqueue"),
        (CallbackDeliveryMode::EnqueueMessage, "enqueue", "wake"),
    ] {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();
        let token = match mode {
            CallbackDeliveryMode::WakeHint => {
                let runtime = host.default_runtime().await.unwrap();
                let capability = runtime.default_external_trigger(mode).await.unwrap();
                capability
                    .trigger_url
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned()
            }
            CallbackDeliveryMode::EnqueueMessage => {
                let record = ExternalTriggerRecord {
                    external_trigger_id: crate::ids::external_trigger_id(),
                    target_agent_id: agent_id.clone(),
                    scope: ExternalTriggerScope::Agent,
                    delivery_mode: mode,
                    token_hash: String::new(),
                    token: None,
                    status: ExternalTriggerStatus::Active,
                    created_at: Utc::now(),
                    revoked_at: None,
                    last_delivered_at: None,
                    delivery_count: 0,
                };
                fixture::seed_callback_fixture(host.runtime_db(), record).unwrap()
            }
        };
        let host = restart_fixture_host(host).await;
        assert!(!host.bootstrap_mode_active());
        fixture::inject_constructor_error(
            &host,
            &agent_id,
            anyhow!("synthetic callback internal detail"),
        );
        let pending = || -> usize {
            host.runtime_db()
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM queue_entries", [], |row| row.get(0))
                .unwrap()
        };
        let baseline = pending();
        let app = crate::http::router(crate::http::AppState::for_tcp(host.clone()));
        for (case, (path, credential, status)) in [
            (correct_path, "invalid", StatusCode::FORBIDDEN),
            (wrong_path, token.as_str(), StatusCode::FORBIDDEN),
            (
                correct_path,
                token.as_str(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                correct_path,
                token.as_str(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (correct_path, "invalid", StatusCode::FORBIDDEN),
            (
                correct_path,
                token.as_str(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if case == 3 {
                reject_constructor(&host, &agent_id);
            }
            let request = Request::post(format!("/api/callbacks/{path}/{credential}"))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            let actual = response.status();
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                actual, status,
                "mode {correct_path}, case {case}, error {:?}",
                envelope["error"]
            );
            if status == StatusCode::SERVICE_UNAVAILABLE {
                let body = std::str::from_utf8(&body).unwrap();
                assert!(body.contains("agent_quarantined"));
                assert!(!body.contains(&token));
                assert!(!body.contains(&agent_id));
            } else if status == StatusCode::INTERNAL_SERVER_ERROR {
                let body = std::str::from_utf8(&body).unwrap();
                assert!(body.contains("callback_internal_error"));
                assert!(!body.contains("agent_quarantined"));
                assert!(!body.contains("synthetic callback internal detail"));
                assert!(!body.contains(&token));
                assert!(!body.contains(&agent_id));
            }
            assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());
            assert_eq!(pending(), baseline);
        }
        host.shutdown().await.unwrap();
    }
}
