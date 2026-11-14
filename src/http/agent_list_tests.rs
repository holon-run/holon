use super::*;
use axum::{body::to_bytes, http::Request};
use std::io::Write;
use tower::ServiceExt;
use tracing::{instrument::WithSubscriber, Instrument};

#[derive(Clone)]
struct LogBuffer(Arc<StdMutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn list_failure_trace_keeps_sanitized_causes_not_just_the_http_code() {
    let logs = Arc::new(StdMutex::new(Vec::new()));
    let output = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || LogBuffer(output.clone()))
        .finish();
    let (status, Json(body)) = tracing::subscriber::with_default(subscriber, || {
        list_build_error_response(
            "serialization",
            anyhow::Error::new(crate::runtime_db::RuntimeDbRetryableError::new(
                "copying list",
                std::path::Path::new("/private/list/runtime.sqlite"),
                "reader deadline exceeded",
            ))
            .context("list copy interrupted"),
        )
    });
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "runtime_db_busy");
    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    for expected in [
        "serialization",
        "list copy interrupted",
        "reader deadline exceeded",
    ] {
        assert!(logs.contains(expected), "{expected}: {logs}");
    }
    assert!(!logs.contains("/private/list"), "{logs}");
    assert!(!body.to_string().contains("/private/list"), "{body}");
}

#[tokio::test]
async fn list_503_traces_member_cause_in_request_span_and_recovers() -> Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = crate::config::AppConfig::load_with_home(Some(home.path().to_owned()))?;
    let host = RuntimeHost::new_with_provider(
        config,
        Arc::new(crate::provider::StubProvider::new("unused")),
    )?;
    let identity = host.ensure_default_agent_identity()?;
    let mut reservations = host
        .reserve_agent_mutations(&[identity.agent_id.clone()])
        .await?;
    reservations[0].begin("private-recovery-ticket".into())?;

    let mut state = AppState::for_unix(host.clone());
    state.projection_gate = Arc::new(ProjectionGate::new(4, StdDuration::ZERO));
    let app = crate::http::router(state);
    let request = || {
        Request::builder()
            .uri("/api/agents/list")
            .body(Body::empty())
            .unwrap()
    };
    let logs = Arc::new(StdMutex::new(Vec::new()));
    let output = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || LogBuffer(output.clone()))
        .finish();
    let response = async {
        app.clone()
            .oneshot(request())
            .instrument(tracing::info_span!(
                "list_test_request",
                request_id = "list-publication-request"
            ))
            .await
    }
    .with_subscriber(subscriber)
    .await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: Value = serde_json::from_slice(&body)?;
    assert_eq!(body["code"], "runtime_db_busy");
    assert_eq!(body["retryable"], true);
    assert!(!body.to_string().contains("private-recovery-ticket"));
    assert!(!body.to_string().contains(home.path().to_str().unwrap()));

    let logs = String::from_utf8(logs.lock().unwrap().clone())?;
    let member = logs
        .lines()
        .find(|line| line.contains("refusing an incomplete list"))
        .expect("member root-cause trace");
    for expected in [
        identity.agent_id.as_str(),
        "incarnation=",
        "identity_revision=",
        "reconciliation pending",
        "list-publication-request",
    ] {
        assert!(member.contains(expected), "{expected}: {logs}");
    }
    let build = logs
        .lines()
        .find(|line| line.contains("Agent list build failed"))
        .expect("build failure trace");
    for expected in ["members", "runtime_db_busy", "list-publication-request"] {
        assert!(build.contains(expected), "{expected}: {logs}");
    }
    assert!(!logs.contains("private-recovery-ticket"), "{logs}");
    assert!(!logs.contains(home.path().to_str().unwrap()), "{logs}");

    reservations[0].definitely_not_committed()?;
    drop(reservations);
    let response = app.clone().oneshot(request()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let entries: Value = serde_json::from_slice(&body)?;
    assert!(entries
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["identity"]["agent_id"] == identity.agent_id));

    // A previous complete response must not hide a subsequent failed build.
    let mut reservations = host
        .reserve_agent_mutations(&[identity.agent_id.clone()])
        .await?;
    reservations[0].begin("another-private-ticket".into())?;
    assert_eq!(
        app.oneshot(request()).await?.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    reservations[0].definitely_not_committed()?;
    drop(reservations);
    host.shutdown().await?;
    Ok(())
}
