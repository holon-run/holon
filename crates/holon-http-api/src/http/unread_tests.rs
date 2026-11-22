use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility, BriefKind,
    BriefRecord,
};
use axum::{body::to_bytes, http::Request};
use std::sync::mpsc;
use tower::ServiceExt;

use super::super::tests::control_token_test_host;

const AGENT_ID: &str = "unread-http-fixture";
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn unread_fixture() -> (tempfile::TempDir, RuntimeHost, u64) {
    let (home, host) = control_token_test_host();
    host.runtime_db()
        .agent_identities()
        .upsert(&AgentIdentityRecord::new(
            AGENT_ID,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))
        .unwrap();
    let state = AppState::for_tcp(host.clone());
    let (principal, entitlement) = observer_sync::observer_scope_authority(&state);
    host.runtime_db()
        .observer_brief_read_state(principal, entitlement, AGENT_ID)
        .unwrap()
        .expect("initialize the observer cursor before appending unread Briefs");
    let mut head = 0;
    for text in ["first unread brief", "second unread brief"] {
        let brief = BriefRecord::new(AGENT_ID, BriefKind::Result, text, None, None);
        let event = crate::types::brief_created_event_for(&brief).unwrap();
        head = host
            .runtime_db()
            .evidence()
            .append_brief_with_created_event(Some(AGENT_ID), &brief, &event, &[])
            .unwrap()
            .event
            .event_seq;
    }
    assert!(host.agent_roster_snapshot().is_ok());
    (home, host, head)
}

fn unread_request(method: &str, uri: &str, token: Option<&str>, seq: u64) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, token);
    }
    let body = if method == "POST" {
        request = request.header(CONTENT_TYPE, "application/json");
        Body::from(json!({"read_through_event_seq": seq}).to_string())
    } else {
        Body::empty()
    };
    request.body(body).unwrap()
}

fn unread_routes() -> [(&'static str, String); 3] {
    [
        ("GET", "/api/agents/brief-read-states".into()),
        ("GET", format!("/api/agents/{AGENT_ID}/brief-read-state")),
        ("POST", format!("/api/agents/{AGENT_ID}/brief-read-cursor")),
    ]
}

async fn response_json(response: AxumResponse) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

#[test]
fn oidc_read_scope_is_isolated_per_principal() {
    let scope_a = observer_sync::observer_visibility_scope_for(
        "runtime_fixture",
        "user-a",
        observer_sync::CONTROL_SCOPE_ENTITLEMENT,
        0,
    );
    let scope_b = observer_sync::observer_visibility_scope_for(
        "runtime_fixture",
        "user-b",
        observer_sync::CONTROL_SCOPE_ENTITLEMENT,
        0,
    );
    assert_ne!(scope_a, scope_b);
}

#[tokio::test(flavor = "current_thread")]
async fn unread_handlers_do_not_assemble_roster_or_parse_brief_payloads() {
    for corrupt_state in [true, false] {
        let (_home, host, head) = unread_fixture();
        let connection = rusqlite::Connection::open(host.runtime_db().path()).unwrap();
        // Brief payload JSON is not part of an unread count. The roster reads
        // Brief metadata rather than this payload, so corrupt a roster anchor
        // independently to prove this fixture catches the former HTTP path.
        assert_eq!(
            connection
                .execute(
                    "UPDATE briefs SET payload_json = 'not-json' WHERE agent_id = ?1",
                    [AGENT_ID],
                )
                .unwrap(),
            2
        );
        if corrupt_state {
            connection
                .execute(
                    "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
                     VALUES (?1, 'idle', '2026-01-01T00:00:00Z', 'not-json')",
                    [AGENT_ID],
                )
                .unwrap();
        } else {
            assert_eq!(
                connection
                    .execute(
                        "UPDATE briefs SET created_at = 'not-a-timestamp' WHERE agent_id = ?1",
                        [AGENT_ID],
                    )
                    .unwrap(),
                2
            );
        }
        drop(connection);
        let roster_error = host
            .agent_roster_snapshot()
            .err()
            .expect("corrupt public roster anchors must make the old unread HTTP scope path fail");
        let expected_error = if corrupt_state {
            "agent state payload"
        } else {
            "latest brief timestamp"
        };
        assert!(
            roster_error.to_string().contains(expected_error),
            "{roster_error:#}"
        );

        let app = router(AppState::for_tcp(host));
        let mut visibility_scope = None;
        for (method, uri) in unread_routes() {
            let response = app
                .clone()
                .oneshot(unread_request(method, &uri, Some("Bearer secret"), head))
                .await
                .unwrap();
            let status = response.status();
            let body = response_json(response).await;
            assert_eq!(status, StatusCode::OK, "{method} {uri}: {body}");
            let state = if method == "POST" {
                assert_eq!(body["applied_read_through_event_seq"], head);
                assert_eq!(body["state"]["unread_count"], 0);
                &body["state"]
            } else if uri.ends_with("brief-read-states") {
                let states = body.as_array().expect("read states array");
                assert_eq!(states.len(), 1);
                &states[0]
            } else {
                &body
            };
            assert_eq!(state["agent_id"], AGENT_ID);
            assert_eq!(state["event_head_seq"], head);
            assert_eq!(
                state["read_through_event_seq"],
                if method == "POST" { head } else { 0 }
            );
            if method != "POST" {
                assert_eq!(state["unread_count"], 2);
            }
            let scope = state["visibility_scope_id"].as_str().unwrap();
            assert!(!scope.is_empty());
            if let Some(expected) = visibility_scope.as_deref() {
                assert_eq!(scope, expected);
            } else {
                visibility_scope = Some(scope.to_owned());
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unread_authentication_precedes_saturated_db_admission() {
    let (_home, host, head) = unread_fixture();
    let mut state = AppState::for_tcp(host);
    state.unread_db_jobs = Arc::new(tokio::sync::Semaphore::new(1));
    let _permit = state.unread_db_jobs.clone().acquire_owned().await.unwrap();
    let app = router(state);
    for token in [None, Some("Bearer wrong-token")] {
        for (method, uri) in unread_routes() {
            let response = app
                .clone()
                .oneshot(unread_request(method, &uri, token, head))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
            let body = response_json(response).await;
            assert_eq!(body["code"], "auth_required", "{method} {uri}: {body}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn static_token_unread_handlers_keep_lightweight_http_responsive_under_writer_lock() {
    // Isolate unread DB work: session authentication has its own existing
    // synchronous DB access before unread admission.
    let (_home, host, head) = unread_fixture();
    let path = host.runtime_db().path().to_path_buf();
    let state = AppState::for_tcp(host);
    let permits = state.unread_db_jobs.clone();
    let app = router(state);
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_released = released.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = mpsc::sync_channel(1);
    let writer = std::thread::spawn(move || {
        let mut connection = rusqlite::Connection::open(path).unwrap();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        locked.send(()).unwrap();
        // Release independently of Tokio even if an inline DB call blocks
        // the only executor thread, so a regression fails rather than hangs.
        let _ = blocked.recv_timeout(TEST_TIMEOUT * 2);
        writer_released.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(transaction);
    });
    tokio::time::timeout(TEST_TIMEOUT, ready)
        .await
        .unwrap()
        .unwrap();

    // The batch read and the single-agent read are pure reader-pool reads
    // once the baseline cursor is pinned, so both must answer while an
    // independent writer holds the database write lock. Only the genuine
    // cursor advance still waits for that writer.
    let batch = tokio::time::timeout(
        TEST_TIMEOUT,
        app.clone().oneshot(unread_request(
            "GET",
            "/api/agents/brief-read-states",
            Some("Bearer secret"),
            head,
        )),
    )
    .await
    .expect("the read-only batch must not wait for a writer")
    .unwrap();
    assert_eq!(batch.status(), StatusCode::OK);
    assert_eq!(response_json(batch).await[0]["unread_count"], 2);

    let single = tokio::time::timeout(
        TEST_TIMEOUT,
        app.clone().oneshot(unread_request(
            "GET",
            &format!("/api/agents/{AGENT_ID}/brief-read-state"),
            Some("Bearer secret"),
            head,
        )),
    )
    .await
    .expect("the pinned single-agent read must not wait for a writer")
    .unwrap();
    assert_eq!(single.status(), StatusCode::OK);
    let single_body = response_json(single).await;
    assert_eq!(single_body["unread_count"], 2);
    assert_eq!(single_body["read_through_event_seq"], 0);

    let mark = tokio::spawn(app.clone().oneshot(unread_request(
        "POST",
        &format!("/api/agents/{AGENT_ID}/brief-read-cursor"),
        Some("Bearer secret"),
        head,
    )));
    tokio::time::timeout(TEST_TIMEOUT, async {
        while permits.available_permits() != 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the genuine cursor advance must enter the bounded DB path");

    let response = tokio::time::timeout(
        TEST_TIMEOUT,
        app.oneshot(unread_request(
            "GET",
            "/api/handshake",
            Some("Bearer secret"),
            0,
        )),
    )
    .await
    .expect("lightweight HTTP must run alongside the advancing unread handler")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !released.load(std::sync::atomic::Ordering::SeqCst),
        "HTTP responses must arrive before the independent writer watchdog releases the lock"
    );
    assert!(
        !mark.is_finished(),
        "the genuine cursor advance must still be waiting on the writer"
    );
    release.send(()).unwrap();
    let response = tokio::time::timeout(TEST_TIMEOUT, mark)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["applied_read_through_event_seq"], head);
    assert_eq!(body["state"]["unread_count"], 0);
    writer.join().unwrap();
    assert_eq!(permits.available_permits(), 4);
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_unread_job_keeps_http_responsive_and_rejects_saturation() {
    let (_home, host, head) = unread_fixture();
    let mut state = AppState::for_tcp(host);
    assert_eq!(state.unread_db_jobs.available_permits(), 4);
    state.unread_db_jobs = Arc::new(tokio::sync::Semaphore::new(1));
    let app = router(state.clone());
    let state = Arc::new(state);
    let (release, blocked) = mpsc::sync_channel(1);
    let (entered, started) = tokio::sync::oneshot::channel();
    let job_state = state.clone();
    let job = tokio::spawn(async move {
        run_unread_db(&job_state, move || {
            entered.send(()).unwrap();
            blocked.recv_timeout(TEST_TIMEOUT).unwrap();
            Ok(42_u64)
        })
        .await
    });
    tokio::time::timeout(TEST_TIMEOUT, started)
        .await
        .expect("blocking job must start without blocking the current-thread executor")
        .unwrap();
    assert_eq!(state.unread_db_jobs.available_permits(), 0);

    let response = tokio::time::timeout(
        TEST_TIMEOUT,
        app.clone().oneshot(unread_request(
            "GET",
            "/api/handshake",
            Some("Bearer secret"),
            0,
        )),
    )
    .await
    .expect("lightweight HTTP must remain runnable while the DB closure is blocked")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["ok"], true);

    for (method, uri) in unread_routes() {
        let response = tokio::time::timeout(
            TEST_TIMEOUT,
            app.clone()
                .oneshot(unread_request(method, &uri, Some("Bearer secret"), head)),
        )
        .await
        .expect("saturated unread requests must reject rather than queue")
        .unwrap();
        let status = response.status();
        assert_eq!(
            response.headers().get("retry-after").unwrap(),
            "1",
            "{method} {uri}"
        );
        let body = response_json(response).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {uri}: {body}"
        );
        assert_eq!(body["retryable"], true, "{method} {uri}: {body}");
        assert_eq!(body["ok"], false, "{method} {uri}: {body}");
        assert_eq!(body["code"], "unread_busy", "{method} {uri}: {body}");
    }
    release.send(()).unwrap();
    assert_eq!(job.await.unwrap().unwrap(), 42);
    assert_eq!(state.unread_db_jobs.available_permits(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_waiter_does_not_release_running_blocking_job_permit() {
    let (_home, host) = control_token_test_host();
    let mut state = AppState::for_tcp(host);
    state.unread_db_jobs = Arc::new(tokio::sync::Semaphore::new(1));
    let state = Arc::new(state);
    let (release, blocked) = mpsc::sync_channel(1);
    let (entered, started) = tokio::sync::oneshot::channel();
    let job_state = state.clone();
    let waiter = tokio::spawn(async move {
        run_unread_db(&job_state, move || {
            entered.send(()).unwrap();
            blocked.recv_timeout(TEST_TIMEOUT).unwrap();
            Ok(())
        })
        .await
    });
    tokio::time::timeout(TEST_TIMEOUT, started)
        .await
        .unwrap()
        .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(
        state.unread_db_jobs.available_permits(),
        0,
        "cancelling the async waiter must not free capacity while its closure runs"
    );
    let rejected = tokio::time::timeout(
        TEST_TIMEOUT,
        run_unread_db(&state, || -> anyhow::Result<()> {
            panic!("saturated operation must not execute");
        }),
    )
    .await
    .expect("saturation must not queue behind the cancelled waiter");
    assert!(rejected.unwrap_err().is::<tokio::sync::TryAcquireError>());

    release.send(()).unwrap();
    let permit = tokio::time::timeout(TEST_TIMEOUT, state.unread_db_jobs.clone().acquire_owned())
        .await
        .expect("capacity must return when the detached closure finishes")
        .unwrap();
    drop(permit);
    assert_eq!(run_unread_db(&state, || Ok(7_u64)).await.unwrap(), 7);
    assert_eq!(state.unread_db_jobs.available_permits(), 1);
}
