use super::*;
use axum::{body::to_bytes, http::Request};
use std::sync::mpsc;
use tower::ServiceExt;

/// Hold the actual host's entire reader pool until this guard is dropped.
/// The worker threads do not depend on the request's Tokio executor.
struct HeldReaders {
    releases: Vec<mpsc::Sender<()>>,
    workers: Vec<std::thread::JoinHandle<anyhow::Result<()>>>,
}

#[tokio::test]
async fn local_actor_logout_and_pairing_preserve_storage_errors() {
    let (_home, host) = tests::control_token_test_host();
    let secret = issue_session(&host);
    let state = AppState::for_tcp(host.clone());
    let app = router(state.clone());
    let held = HeldReaders::new(host.runtime_db());
    for (method, uri) in [
        ("GET", "/api/auth/session/me"),
        ("POST", "/api/auth/session/logout"),
        ("POST", "/api/auth/pairing/issue"),
    ] {
        assert_db_busy(
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(COOKIE, format!("holon_session={secret}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
    }
    drop(held);
    let mut headers = HeaderMap::new();
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&format!("holon_session={secret}")).unwrap(),
    );
    assert!(authenticate_session(&headers, &state).is_ok());
}

#[tokio::test]
async fn oidc_api_and_html_preserve_storage_errors_without_login_redirect() {
    let (_home, host, secret) = tests::oidc_test_host_with_session();
    let app = router(AppState::for_tcp(host.clone()));
    let held = HeldReaders::new(host.runtime_db());
    for uri in ["/api/status", "/"] {
        assert_db_busy(
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(ACCEPT, "text/html")
                        .header(COOKIE, format!("holon_session={secret}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
    }
    drop(held);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/session/me")
                .header(COOKIE, format!("holon_session={secret}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn origin_bearer_session_checkout_failure_is_not_csrf_rejection() {
    let (_home, host) = tests::control_token_test_host_with_origin_guard();
    let secret = issue_session(&host);
    let app = router(AppState::for_tcp(host.clone()));
    let held = HeldReaders::new(host.runtime_db());
    assert_db_busy(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/pairing/issue")
                .header(AUTHORIZATION, format!("Bearer {secret}"))
                .header(COOKIE, "holon_session=invalid-cookie")
                .header(ORIGIN, "https://attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    drop(held);
}

impl HeldReaders {
    fn new(db: &crate::runtime_db::RuntimeDb) -> Self {
        let capacity = db.reader_pool_stats_for_tests().capacity;
        assert!(capacity > 0);
        let mut held = Self {
            releases: Vec::new(),
            workers: Vec::new(),
        };
        for _ in 0..capacity {
            let db = db.clone();
            let (ready_tx, ready_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            held.releases.push(release_tx);
            held.workers.push(std::thread::spawn(move || {
                db.with_read(|_| {
                    ready_tx.send(())?;
                    release_rx.recv_timeout(Duration::from_secs(30))?;
                    Ok(())
                })
            }));
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        held
    }
}

impl Drop for HeldReaders {
    fn drop(&mut self) {
        for release in self.releases.drain(..) {
            let _ = release.send(());
        }
        for worker in self.workers.drain(..) {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap().unwrap();
            }
        }
    }
}

fn issue_session(host: &RuntimeHost) -> String {
    let now = Utc::now();
    let authentication = host.runtime_db().authentication();
    authentication
        .upsert_user(&crate::authentication::AuthUserRecord {
            user_id: "storage-error-user".into(),
            issuer: "https://issuer.example".into(),
            subject: "storage-error-subject".into(),
            display_name: None,
            email: None,
            created_at: now,
            updated_at: now,
            disabled_at: None,
        })
        .unwrap();
    let secret = "storage-error-session".to_string();
    authentication
        .create_session(&crate::authentication::AuthSessionRecord {
            session_digest: crate::authentication::digest_secret(&secret),
            user_id: "storage-error-user".into(),
            auth_method: "oidc".into(),
            created_at: now,
            expires_at: None,
            last_seen_at: now,
            revoked_at: None,
        })
        .unwrap();
    secret
}

async fn assert_db_busy(response: AxumResponse) {
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!response.headers().contains_key(LOCATION));
    assert!(!response.headers().contains_key(SET_COOKIE));
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["code"], "runtime_db_busy");
    assert_eq!(body["domain"], "storage");
    assert_eq!(body["retryable"], true);
    assert_eq!(body["context"]["operation"], "reader pool checkout");
    assert!(!body["error"]
        .as_str()
        .unwrap()
        .contains("missing Authorization header"));
}

#[tokio::test]
async fn cookie_only_reader_checkout_failure_remains_storage_error() {
    let (_home, host) = tests::control_token_test_host();
    let secret = issue_session(&host);
    let _runtime = host.default_runtime().await.unwrap();
    let uri = format!("/api/agents/{}/events", host.config().default_agent_id);
    let app = router(AppState::for_tcp(host.clone()));
    let request = || {
        Request::builder()
            .uri(&uri)
            .header(COOKIE, format!("holon_session={secret}"))
            .body(Body::empty())
            .unwrap()
    };
    let held = HeldReaders::new(host.runtime_db());
    assert_db_busy(app.clone().oneshot(request()).await.unwrap()).await;
    drop(held);
    assert_eq!(
        app.oneshot(request()).await.unwrap().status(),
        StatusCode::OK
    );
}

fn session_headers(secret: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&format!("holon_session={secret}")).unwrap(),
    );
    headers
}

#[test]
fn session_fallback_only_continues_after_credential_rejection() {
    let (_home, host) = tests::control_token_test_host();
    let secret = issue_session(&host);
    let state = AppState::for_tcp(host);
    let session = authenticate_session(&session_headers(&secret), &state).unwrap();
    let credentials = || ["bearer".to_string(), "cookie".to_string()];
    let mut attempted = Vec::new();
    let result = authenticate_session_credentials(credentials(), |credential| {
        attempted.push(credential.to_string());
        if credential == "bearer" {
            Err(AuthenticationError::Rejected("stale bearer"))
        } else {
            Ok(session.clone())
        }
    });
    assert_eq!(result.unwrap(), session);
    assert_eq!(attempted, ["bearer", "cookie"]);

    attempted.clear();
    let error = authenticate_session_credentials(credentials(), |credential| {
        attempted.push(credential.to_string());
        Err(AuthenticationError::Infrastructure(
            crate::runtime_db::RuntimeDbRetryableError::new(
                "reader pool checkout",
                std::path::Path::new("runtime.sqlite"),
                "pool occupied",
            )
            .into(),
        ))
    })
    .unwrap_err();
    assert_eq!(attempted, ["bearer"]);
    let (status, Json(body)) = authentication_error_response(error);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "runtime_db_busy");
    assert_eq!(body["context"]["operation"], "reader pool checkout");
}

#[test]
fn local_static_token_and_independent_admission_keep_their_boundaries() {
    let (_home, host) = tests::control_token_test_host();
    let secret = issue_session(&host);
    let state = AppState::for_tcp(host.clone());
    let held = HeldReaders::new(host.runtime_db());
    let mut headers = session_headers(&secret);
    assert!(matches!(
        authorize_control(&headers, &state),
        Err(AuthenticationError::Infrastructure(_))
    ));
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
    assert!(matches!(
        authorize_control(&headers, &state),
        Err(AuthenticationError::Infrastructure(_))
    ));
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
    assert!(authorize_control(&headers, &state).is_ok());
    // Admission by the static token does not bypass the existing cookie actor check.
    assert!(matches!(
        control_actor(&headers, &state),
        Err(AuthenticationError::Infrastructure(_))
    ));
    headers.remove(COOKIE);
    assert!(authorize_control(&headers, &state).is_ok());
    assert_eq!(
        control_actor(&headers, &state).unwrap(),
        ControlActor::LocalControl
    );
    headers.remove(AUTHORIZATION);
    headers.extend(session_headers(&secret));
    let mut open_state = state.clone();
    open_state.require_control_token = false;
    assert!(authorize_control(&headers, &open_state).is_ok());
    assert!(authorize_control(&headers, &AppState::for_unix(host)).is_ok());
    drop(held);

    for authorization in [None, Some("Bearer wrong"), Some("bearer secret")] {
        let mut headers = HeaderMap::new();
        if let Some(value) = authorization {
            headers.insert(AUTHORIZATION, HeaderValue::from_static(value));
        }
        let error = authorize_control(&headers, &state).unwrap_err();
        assert!(matches!(error, AuthenticationError::Rejected(_)));
        assert_eq!(
            authentication_error_response(error).0,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[test]
fn unhealthy_session_user_lookup_and_touch_keep_sqlite_sources() {
    for (sql, expected_context) in [
        (
            "ALTER TABLE auth_users RENAME TO unavailable_auth_users",
            "looking up authenticated user by id",
        ),
        (
            "CREATE TRIGGER fail_session_touch BEFORE UPDATE OF last_seen_at
             ON auth_sessions BEGIN SELECT RAISE(ABORT, 'session touch unavailable'); END",
            "session touch unavailable",
        ),
    ] {
        let (_home, host) = tests::control_token_test_host();
        let secret = issue_session(&host);
        host.runtime_db()
            .transaction(|tx| Ok(tx.execute_batch(sql)?))
            .unwrap();
        let state = AppState::for_tcp(host);
        let AuthenticationError::Infrastructure(source) =
            authenticate_session(&session_headers(&secret), &state).unwrap_err()
        else {
            panic!("database failure must not become a credential rejection");
        };
        assert!(source.downcast_ref::<rusqlite::Error>().is_some());
        assert!(format!("{source:#}").contains(expected_context));
        assert!(!describe_runtime_error(&source).source_chain.is_empty());
        let (status, Json(body)) =
            authentication_error_response(AuthenticationError::Infrastructure(source));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["retryable"], false);
        assert_ne!(body["code"], "auth_required");
        assert_ne!(body["code"], "runtime_db_busy");
    }
}

#[test]
fn healthy_database_still_rejects_inactive_or_missing_credentials() {
    for (credential, sql) in [
        (None, ""),
        (Some("invalid"), ""),
        (
            Some("storage-error-session"),
            "UPDATE auth_sessions SET expires_at = '2000-01-01T00:00:00Z'",
        ),
        (
            Some("storage-error-session"),
            "UPDATE auth_sessions SET last_seen_at = '2000-01-01T00:00:00Z'",
        ),
        (
            Some("storage-error-session"),
            "UPDATE auth_sessions SET revoked_at = '2000-01-01T00:00:00Z'",
        ),
        (
            Some("storage-error-session"),
            "UPDATE auth_users SET disabled_at = '2000-01-01T00:00:00Z'",
        ),
    ] {
        let (_home, host) = tests::control_token_test_host();
        issue_session(&host);
        host.runtime_db()
            .transaction(|tx| Ok(tx.execute_batch(sql)?))
            .unwrap();
        let state = AppState::for_tcp(host);
        let headers = credential.map(session_headers).unwrap_or_default();
        let error = authenticate_session(&headers, &state).unwrap_err();
        assert!(matches!(error, AuthenticationError::Rejected(_)));
        let (status, Json(body)) = authentication_error_response(error);
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "auth_required");
    }
}

#[test]
fn infrastructure_mapper_keeps_quarantine_distinct_from_retryable_busy() {
    let source = crate::runtime_db::RuntimeDbProtectionError::new(
        std::path::Path::new("runtime.sqlite"),
        "deleted-open sidecar",
    );
    let (status, Json(body)) =
        authentication_error_response(AuthenticationError::Infrastructure(source.into()));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "runtime_db_quarantined");
    assert_eq!(body["retryable"], false);
    assert_eq!(body["context"]["protection_state"], "quarantined");
}

#[tokio::test]
async fn oidc_static_token_is_not_session_admission_and_rejection_still_redirects() {
    let (_home, host, _) = tests::oidc_test_host_with_session();
    let mut config = (*host.config()).clone();
    config.control_token = Some("oidc-static-token".into());
    let host = RuntimeHost::new_with_provider(
        config,
        Arc::new(crate::provider::StubProvider::new("done")),
    )
    .unwrap();
    let state = AppState::for_tcp(host);
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer oidc-static-token"),
    );
    assert!(matches!(
        authorize_control(&headers, &state),
        Err(AuthenticationError::Rejected(_))
    ));
    let app = router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header(AUTHORIZATION, "Bearer oidc-static-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(ACCEPT, "text/html")
                .header(COOKIE, "holon_session=invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers()[LOCATION], "/login");
}
