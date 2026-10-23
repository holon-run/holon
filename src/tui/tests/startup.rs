use super::*;
use crate::{
    client::LocalHttpError,
    http_dto::{AgentStateSnapshotDto, SlimAgentDto, StateSessionSnapshotDto},
};
use axum::{http::StatusCode, response::IntoResponse};
use std::sync::{Arc, Mutex};

fn roster(ids: &[&str]) -> serde_json::Value {
    json!({
        "contract_version": 1,
        "runtime_id": "test-runtime",
        "event_log_epoch": "test-epoch",
        "visibility_scope_id": "test-public",
        "agents": ids.iter().map(|id| json!({
            "agent": AgentListEntry::from_summary(&sample_agent_summary(id)),
            "event_window": {"event_head_seq": 0, "oldest_retained_seq": 0},
            "latest_brief": null,
        })).collect::<Vec<_>>(),
    })
}

fn compact_status(id: &str) -> serde_json::Value {
    serde_json::to_value(SlimAgentDto::from(&sample_agent_summary(id))).unwrap()
}

struct StartupServer {
    client: LocalClient,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for StartupServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn startup_server(
    roster_response: (StatusCode, serde_json::Value),
    status_response: (StatusCode, serde_json::Value),
) -> StartupServer {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = requests.clone();
    let router = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        server_requests.lock().unwrap().push(uri.to_string());
        let response = match uri.path() {
            "/api/agents/snapshot" => roster_response.clone(),
            "/api/agents/beta/status" => status_response.clone(),
            _ => (StatusCode::SERVICE_UNAVAILABLE, json!({"error": "offline"})),
        };
        async move {
            if response.1.is_null() {
                (response.0, "{invalid json").into_response()
            } else {
                (response.0, axum::Json(response.1)).into_response()
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    StartupServer {
        client: LocalClient::remote(test_config(), format!("http://{addr}"), "secret").unwrap(),
        requests,
        task,
    }
}

fn app(client: LocalClient) -> TuiApp {
    TuiApp::new(
        client,
        crate::tui::logging::TuiLogWriter::new_temp().unwrap(),
    )
}

fn explicit_app(client: LocalClient) -> TuiApp {
    let mut app = app(client);
    app.initial_agent_id = Some("beta".into());
    app.persist_selection = false;
    app
}

fn snapshot(id: &str) -> AgentStateSnapshotDto {
    AgentStateSnapshotDto {
        agent: SlimAgentDto::from(&sample_agent_summary(id)),
        session: StateSessionSnapshotDto {
            current_run_id: None,
            pending_count: 0,
            last_turn: None,
        },
        tasks: vec![],
        waits: vec![],
        timers: vec![],
        work_items: vec![],
        external_triggers: vec![],
        workspace: Default::default(),
    }
}

fn finish_bootstrap(app: &mut TuiApp, index: usize, id: &str) {
    app.apply_snapshot_result(
        app.snapshot_refresh_request_id,
        index,
        id.into(),
        None,
        Ok((snapshot(id), vec![], "epoch".into(), None, None, false)),
    );
}

#[tokio::test]
async fn explicit_preflight_reuses_public_roster_and_overrides_memory_and_default() {
    let server = startup_server(
        (StatusCode::OK, roster(&["default", "alpha", "beta"])),
        (StatusCode::OK, compact_status("beta")),
    )
    .await;
    let state_path = tui_state_path(&server.client);
    TuiClientState::new("alpha").save(&state_path).unwrap();
    let original = std::fs::read(&state_path).unwrap();
    let mut app = app(server.client.clone());

    app.initialize_with_agent(Some("beta".into()))
        .await
        .unwrap();

    assert_eq!(app.selected_agent_id(), Some("beta"));
    assert_eq!(app.initial_agent_id.as_deref(), Some("beta"));
    assert!(!app.persist_selection);
    assert!(app.snapshot_refresh_in_flight);
    assert!(!app.agent_list_refresh_in_flight);
    assert!(app.agent_list_refresh_deadline.is_some());
    assert_eq!(
        *server.requests.lock().unwrap(),
        [
            "/api/agents/snapshot",
            "/api/agents/beta/status?detail=compact"
        ]
    );
    assert_eq!(std::fs::read(state_path).unwrap(), original);
}

#[tokio::test]
async fn explicit_preflight_preserves_targeted_structured_errors_even_with_stale_roster() {
    for (status, code, expected) in [
        (404, "not_found", "agent beta not found"),
        (410, "agent_deleted", "agent beta has been deleted"),
        (409, "agent_deleting", "agent beta is being deleted"),
        (401, "unauthorized", "failed to read compact status"),
        (403, "forbidden", "failed to read compact status"),
        (500, "internal", "failed to read compact status"),
        (404, "route_not_found", "failed to read compact status"),
    ] {
        let server = startup_server(
            (StatusCode::OK, roster(&["default", "beta"])),
            (
                StatusCode::from_u16(status).unwrap(),
                json!({"error": "status failed", "code": code, "hint": "test hint"}),
            ),
        )
        .await;
        let mut app = app(server.client.clone());
        let err = app
            .initialize_with_agent(Some("beta".into()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains(expected), "{err:#}");
        let http = err.downcast_ref::<LocalHttpError>().unwrap();
        assert_eq!(http.status_code, status);
        assert_eq!(http.code.as_deref(), Some(code));
        assert_eq!(http.path, "/api/agents/beta/status?detail=compact");
        assert_eq!(http.hint.as_deref(), Some("test hint"));
        assert!(!format!("{err:#}").contains("secret"));
        assert!(app.agents.is_empty());
        assert!(!app.state_path.exists());
        assert!(!app.snapshot_refresh_in_flight);
        assert_eq!(server.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn explicit_preflight_does_not_confuse_roster_route_failure_with_missing_agent() {
    for status in [404, 401, 403, 503] {
        let server = startup_server(
            (
                StatusCode::from_u16(status).unwrap(),
                json!({"error": "roster route failed", "code": "not_found"}),
            ),
            (StatusCode::OK, compact_status("beta")),
        )
        .await;
        let mut app = app(server.client.clone());
        let err = app
            .initialize_with_agent(Some("beta".into()))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "failed to load public agent roster");
        let http = err.downcast_ref::<LocalHttpError>().unwrap();
        assert_eq!(http.status_code, status);
        assert_eq!(http.path, "/api/agents/snapshot");
        assert_eq!(*server.requests.lock().unwrap(), ["/api/agents/snapshot"]);
        assert!(!app.state_path.exists());
    }
}

#[tokio::test]
async fn explicit_preflight_rejects_private_or_unlisted_readable_status() {
    let server = startup_server(
        (StatusCode::OK, roster(&["default"])),
        (StatusCode::OK, compact_status("beta")),
    )
    .await;
    let mut app = app(server.client.clone());
    let err = app
        .initialize_with_agent(Some("beta".into()))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "agent beta is not in the currently selectable public roster"
    );
    assert!(!app.state_path.exists());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn explicit_preflight_keeps_protocol_and_transport_failures_distinct() {
    let mut future_roster = roster(&["beta"]);
    future_roster["contract_version"] = json!(999);
    for (roster_body, status_body, expected) in [
        (
            future_roster,
            compact_status("beta"),
            "unsupported public roster",
        ),
        (
            json!("invalid roster"),
            compact_status("beta"),
            "failed to load public",
        ),
        (
            roster(&["beta"]),
            json!("invalid status"),
            "failed to read compact",
        ),
        (roster(&["beta"]), json!(null), "failed to decode response"),
    ] {
        let server =
            startup_server((StatusCode::OK, roster_body), (StatusCode::OK, status_body)).await;
        let mut app = app(server.client.clone());
        let err = app
            .initialize_with_agent(Some("beta".into()))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains(expected), "{err:#}");
        assert!(err.downcast_ref::<LocalHttpError>().is_none());
        assert!(!format!("{err:#}").contains("agent beta not found"));
        assert!(!app.state_path.exists());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut app =
        app(LocalClient::remote(test_config(), format!("http://{addr}"), "secret").unwrap());
    let err = app
        .initialize_with_agent(Some("beta".into()))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "failed to load public agent roster");
    assert!(!format!("{err:#}").contains("agent beta not found"));
    assert!(!format!("{err:#}").contains("secret"));
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_preflight_timeout_is_not_missing_agent_and_never_falls_back() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let config = test_config();
    std::fs::create_dir_all(config.socket_path.parent().unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = requests.clone();
    let server = tokio::spawn(async move {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            server_requests
                .lock()
                .unwrap()
                .push(request.lines().next().unwrap().to_string());
            if index == 0 {
                let body = roster(&["default", "beta"]).to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            } else {
                // A quiet targeted response must retain transport-timeout semantics.
                tokio::time::sleep(TUI_LOCAL_NETWORK_POLICY.read_idle_timeout * 2).await;
            }
        }
    });
    let mut app = app(LocalClient::new(config).unwrap());
    let err = tokio::time::timeout(
        TUI_LOCAL_NETWORK_POLICY.read_idle_timeout + std::time::Duration::from_secs(3),
        app.initialize_with_agent(Some("beta".into())),
    )
    .await
    .expect("the client's timeout should settle preflight")
    .unwrap_err();
    assert!(format!("{err:#}").contains("timeout"), "{err:#}");
    assert!(!format!("{err:#}").contains("agent beta not found"));
    assert!(err.downcast_ref::<LocalHttpError>().is_none());
    assert_eq!(
        *requests.lock().unwrap(),
        [
            "GET /api/agents/snapshot HTTP/1.1",
            "GET /api/agents/beta/status?detail=compact HTTP/1.1"
        ]
    );
    assert!(app.agents.is_empty());
    assert!(!app.state_path.exists());
    server.abort();
}

#[tokio::test]
async fn startup_roster_race_and_old_snapshot_cannot_fall_back() {
    for replacement in [vec!["default"], vec![]] {
        let mut app = explicit_app(LocalClient::new(test_config()).unwrap());
        app.apply_agent_list(vec![
            sample_agent_summary("default"),
            sample_agent_summary("beta"),
        ]);
        app.snapshot_refresh_request_id = 7;
        app.apply_loaded_agents(Ok(replacement
            .iter()
            .map(|id| AgentListEntry::from_summary(&sample_agent_summary(id)))
            .collect()));
        assert!(app
            .tick()
            .await
            .unwrap_err()
            .to_string()
            .contains("startup agent beta"));
        assert_eq!(app.selected_agent_id(), None);
        assert_eq!(app.initial_agent_id.as_deref(), Some("beta"));
        app.apply_snapshot_result(
            7,
            1,
            "beta".into(),
            None,
            Ok((snapshot("beta"), vec![], "epoch".into(), None, None, false)),
        );
        assert!(app.projection.is_none());
        assert_eq!(app.initial_agent_id.as_deref(), Some("beta"));
        assert!(!app.state_path.exists());
    }
}

#[tokio::test]
async fn startup_bootstrap_failure_is_fatal_without_fallback_or_memory_write() {
    let mut app = explicit_app(LocalClient::new(test_config()).unwrap());
    app.apply_agent_list(vec![
        sample_agent_summary("default"),
        sample_agent_summary("beta"),
    ]);
    app.snapshot_refresh_request_id = 7;
    app.apply_snapshot_result(6, 1, "beta".into(), None, Err("old failure".into()));
    assert!(matches!(
        app.connection_state,
        TuiConnectionState::Bootstrapping
    ));
    app.begin_bootstrap_agent_index(0);
    assert_eq!(app.snapshot_refresh_request_id, 7);
    assert_eq!(app.selected_agent_id(), Some("beta"));
    // A completion for another agent cannot satisfy the explicit startup constraint.
    app.apply_snapshot_result(
        7,
        0,
        "default".into(),
        None,
        Ok((
            snapshot("default"),
            vec![],
            "epoch".into(),
            None,
            None,
            false,
        )),
    );
    assert!(app.projection.is_none());
    assert_eq!(app.initial_agent_id.as_deref(), Some("beta"));
    app.apply_snapshot_result(7, 1, "beta".into(), None, Err("agent_deleted".into()));
    let err = app.tick().await.unwrap_err();
    assert!(err
        .to_string()
        .contains("failed to bootstrap startup agent beta"));
    assert!(err.to_string().contains("agent_deleted"));
    assert_eq!(app.selected_agent_id(), Some("beta"));
    assert!(app.refresh_deadline.is_none());
    assert!(!app.state_path.exists());
}

#[tokio::test]
async fn successful_startup_consumes_constraint_but_entire_session_remains_unpersisted() {
    for remote in [false, true] {
        let config = test_config();
        let client = if remote {
            LocalClient::remote(config, "http://127.0.0.1:1", "secret").unwrap()
        } else {
            LocalClient::new(config).unwrap()
        };
        let path = tui_state_path(&client);
        TuiClientState::new("remembered").save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        for initial in ["beta", "default"] {
            let mut app = explicit_app(client.clone());
            app.initial_agent_id = Some(initial.into());
            let agents = vec![
                sample_agent_summary("default"),
                sample_agent_summary("beta"),
            ];
            let initial_index = usize::from(initial == "beta");
            let switched_index = 1 - initial_index;
            let switched_id = agents[switched_index].identity.agent_id.clone();
            app.apply_agent_list(agents.clone());
            finish_bootstrap(&mut app, initial_index, initial);
            assert!(app.initial_agent_id.is_none());
            assert!(!app.persist_selection);
            assert_eq!(app.preferred_agent_id.as_deref(), Some(initial));
            // Ordinary refresh and then a manual switch.
            finish_bootstrap(&mut app, initial_index, initial);
            app.begin_bootstrap_agent_index(switched_index);
            finish_bootstrap(&mut app, switched_index, &switched_id);
            assert_eq!(app.selected_agent_id(), Some(switched_id.as_str()));
            assert_eq!(app.apply_agent_list(agents), AgentListChange::Ready);
            assert_eq!(app.selected_agent_id(), Some(switched_id.as_str()));
            // After successful startup, losing the selected agent uses normal fallback.
            assert_eq!(
                app.apply_agent_list(vec![sample_agent_summary("gamma")]),
                AgentListChange::RequiresBootstrap
            );
            finish_bootstrap(&mut app, 0, "gamma");
            assert_eq!(app.selected_agent_id(), Some("gamma"));
            assert_eq!(app.preferred_agent_id.as_deref(), Some("gamma"));
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }
}
