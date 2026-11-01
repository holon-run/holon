use super::*;
use crate::http::{agents, tests::control_token_test_host, AppState};
use axum::{
    extract::{Query, State},
    http::{header::AUTHORIZATION, HeaderMap, HeaderValue},
    response::Response,
};

async fn roster_request(state: Arc<AppState>) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
    agents::list_agent_entries(
        State(state),
        headers,
        Query(agents::AgentListQuery { parent: None }),
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn unloaded_roster_acquisition_does_not_read_on_the_async_worker() {
    let (_home, host) = control_token_test_host();
    host.ensure_default_agent_identity().unwrap();
    let state = Arc::new(AppState::for_tcp(host.clone()));
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let response = tokio::time::timeout(Duration::from_secs(5), roster_request(state.clone()))
        .await
        .unwrap();
    assert_eq!(denied.attempts(), 0);
    drop(denied);
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let entries: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert!(
        !host
            .has_runtime_entry_for_fixture(&host.config().default_agent_id)
            .await
    );
    state.projection_gate.close_and_drain().await;
    host.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn loaded_roster_cancellation_and_deadline_release_capacity_with_busy_owner() {
    let (_home, host) = control_token_test_host();
    let runtime = host.default_runtime().await.unwrap();
    runtime.await_initial_resources_for_test().await;
    let refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
    drop(host.list_agent_entries().await.unwrap());
    let agent_id = host.config().default_agent_id.clone();
    let incarnation = host
        .agent_identity_record(&agent_id)
        .unwrap()
        .unwrap()
        .incarnation;
    for cancel in [true, false] {
        let owner = host
            .reserve_mutation_scopes(&[(agent_id.clone(), incarnation)])
            .await
            .unwrap();
        let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
        let mut state = AppState::for_tcp(host.clone());
        state.projection_gate = gate.clone();
        let state = Arc::new(state);
        let request = tokio::spawn(roster_request(state.clone()));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // Register acquisition before waiting on the busy owner.
                if gate.state.lock().unwrap().tasks.len() >= 2 {
                    break;
                }
                assert!(!request.is_finished());
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(gate.leaders.available_permits(), 0);
        if cancel {
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
        } else {
            let response =
                tokio::time::timeout(DEFAULT_BUILD_BUDGET + Duration::from_secs(5), request)
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        tokio::time::timeout(Duration::from_secs(5), gate.drain_tasks())
            .await
            .unwrap();
        assert_eq!(gate.leaders.available_permits(), 1);
        // Neither interruption needs the unrelated writer to finish first.
        drop(owner);
        let response = tokio::time::timeout(Duration::from_secs(5), roster_request(state))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        gate.close_and_drain().await;
    }
    drop(refresh);
    host.shutdown().await.unwrap();
}
