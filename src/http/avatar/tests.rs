use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
    AgentVisibility,
};
use axum::{body::to_bytes, http::Request};
use tower::ServiceExt;

const ID: &str = "avatar-http-fixture";

fn fixture() -> (tempfile::TempDir, RuntimeHost, AgentIdentityRecord) {
    let (home, host) = super::super::tests::control_token_test_host();
    let identity = AgentIdentityRecord::new(
        ID,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    host.append_agent_identity(&identity).unwrap();
    (home, host, identity)
}

fn request(id: &str, query: &str, token: bool, etag: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(format!("/api/agents/{id}/avatar?{query}"));
    if token {
        builder = builder.header(AUTHORIZATION, "Bearer secret");
    }
    if let Some(etag) = etag {
        builder = builder.header(IF_NONE_MATCH, etag);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn image_reads_are_conditional_deterministic_and_do_not_load_agents() {
    let (home, host, identity) = fixture();
    let before_events = host
        .runtime_db()
        .audit_events()
        .max_event_seq(Some(ID))
        .unwrap();
    let agent_home = home.path().join("agents").join(ID);
    assert!(!agent_home.exists());
    assert!(host.try_get_loaded_runtime(ID).await.is_none());
    let app = router(AppState::for_tcp(host.clone()));
    let response = app
        .clone()
        .oneshot(request(ID, "format=svg", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "image/svg+xml");
    assert_eq!(response.headers()[CACHE_CONTROL], "private, no-cache");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let etag = response.headers()[ETAG].to_str().unwrap().to_owned();
    let svg = to_bytes(response.into_body(), MAX_IMAGE_BYTES)
        .await
        .unwrap();
    assert_eq!(
        svg,
        artwork::render_svg(artwork::recipe_index(ID), Expression::default())
    );
    let response = app
        .clone()
        .oneshot(request(
            ID,
            "format=svg",
            true,
            Some(&format!("\"other\", W/{etag}")),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()[ETAG], etag);
    assert!(to_bytes(response.into_body(), 100)
        .await
        .unwrap()
        .is_empty());
    for size in [32, 64, 128, 256, 512] {
        let response = app
            .clone()
            .oneshot(request(ID, &format!("size={size}"), true, None))
            .await
            .unwrap();
        assert_eq!(response.headers()[CONTENT_TYPE], "image/png");
        let png = to_bytes(response.into_body(), MAX_IMAGE_BYTES)
            .await
            .unwrap();
        let image = image::load_from_memory(&png).unwrap();
        assert_eq!((image.width(), image.height()), (size, size));
    }
    let mut renamed = identity.clone();
    renamed.name = Some("different display name".into());
    host.append_agent_identity(&renamed).unwrap();
    // A fresh service models a restart/cache eviction.
    let fresh = router(AppState::for_tcp(host.clone()));
    let response = fresh
        .oneshot(request(ID, "format=svg", true, None))
        .await
        .unwrap();
    assert_eq!(
        to_bytes(response.into_body(), MAX_IMAGE_BYTES)
            .await
            .unwrap(),
        svg
    );
    assert_eq!(host.agent_identity_record(ID).unwrap().unwrap(), renamed);
    assert_eq!(
        host.runtime_db()
            .audit_events()
            .max_event_seq(Some(ID))
            .unwrap(),
        before_events
    );
    assert!(!agent_home.exists());
    assert!(host.try_get_loaded_runtime(ID).await.is_none());
}

#[tokio::test]
async fn authentication_and_current_target_checks_precede_cache_and_304() {
    let (_home, host, mut identity) = fixture();
    let app = router(AppState::for_tcp(host.clone()));
    let response = app
        .clone()
        .oneshot(request(ID, "format=svg", true, None))
        .await
        .unwrap();
    let etag = response.headers()[ETAG].to_str().unwrap().to_owned();
    let response = app
        .clone()
        .oneshot(request(ID, "format=svg", false, Some(&etag)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    for (status, expected) in [
        (AgentRegistryStatus::Deleting, StatusCode::CONFLICT),
        (AgentRegistryStatus::Deleted, StatusCode::GONE),
    ] {
        identity.status = status;
        host.append_agent_identity(&identity).unwrap();
        let response = app
            .clone()
            .oneshot(request(ID, "format=svg", true, Some(&etag)))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert!(response.headers()[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/json"));
    }
    let response = app
        .clone()
        .oneshot(request("missing", "format=svg", true, Some("*")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(host.agent_identity_record("missing").unwrap().is_none());
    identity.status = AgentRegistryStatus::Active;
    identity.incarnation += 1;
    // The existing registry forbids reusing a retired ID in the same epoch.
    assert!(host.append_agent_identity(&identity).is_err());
    assert_eq!(
        app.oneshot(request(ID, "format=svg", true, Some(&etag)))
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
    let (_fresh_home, fresh_host, _) = fixture();
    assert_eq!(
        router(AppState::for_tcp(fresh_host))
            .oneshot(request(ID, "format=svg", true, Some(&etag)))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_MODIFIED
    );
}

#[tokio::test]
async fn parameters_are_strict_and_target_access_matches_operator_reads() {
    let (_home, host, mut identity) = fixture();
    let app = router(AppState::for_tcp(host.clone()));
    for query in [
        "version=latest",
        "version=light-mech-1-vivid",
        "format=jpeg",
        "format=svg&size=128",
        "size=1024",
        "size=0",
        "size=0128",
        "expression=surprised",
        "format=svg&format=svg",
        "size=32&%73ize=32",
        "token=secret",
        "unknown=1",
    ] {
        let response = app
            .clone()
            .oneshot(request(ID, query, true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        assert!(response.headers()[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/json"));
    }
    for expression in ["neutral", "focused", "questioning", "waiting", "sleeping"] {
        let response = app
            .clone()
            .oneshot(request(
                ID,
                &format!("format=svg&expression={expression}"),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    // Like GET /agents/{id}, an authenticated operator can read private children.
    identity.visibility = AgentVisibility::Private;
    host.append_agent_identity(&identity).unwrap();
    assert_eq!(
        app.oneshot(request(ID, "format=svg", true, None))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn overload_has_retry_after_and_does_not_bypass_authentication() {
    let (_home, host, _) = fixture();
    let state = AppState::for_tcp(host);
    let builders = state
        .avatar_resources
        .builders
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let waiters = state
        .avatar_resources
        .waiters
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap();
    let app = router(state);
    let response = app
        .clone()
        .oneshot(request(ID, "", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    assert_eq!(
        app.oneshot(request(ID, "", false, None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    drop((builders, waiters));
}

#[tokio::test]
async fn revoked_cookie_cannot_reuse_cached_bytes_or_etag() {
    let (_home, host, secret) = super::super::tests::oidc_test_host_with_session();
    host.append_agent_identity(&AgentIdentityRecord::new(
        ID,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    ))
    .unwrap();
    let app = router(AppState::for_tcp(host.clone()));
    let cookie_request = |etag: Option<&str>| {
        let mut request = request(ID, "format=svg", false, etag);
        request.headers_mut().insert(
            COOKIE,
            HeaderValue::from_str(&format!("holon_session={secret}")).unwrap(),
        );
        request
    };
    let response = app.clone().oneshot(cookie_request(None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[ETAG].to_str().unwrap().to_owned();
    host.runtime_db()
        .authentication()
        .revoke_session(&crate::authentication::digest_secret(&secret), Utc::now())
        .unwrap();
    assert_eq!(
        app.oneshot(cookie_request(Some(&etag)))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn stopped_runtime_is_not_started_or_enqueued_by_an_image_read() {
    let (_home, host) = super::super::tests::control_token_test_host();
    let runtime = host.default_runtime().await.unwrap();
    runtime.wait_for_bootstrap().await.unwrap();
    runtime
        .control(crate::types::ControlAction::Stop)
        .await
        .unwrap();
    let id = runtime.instance_agent_id();
    let before = host
        .runtime_db()
        .audit_events()
        .max_event_seq(Some(&id))
        .unwrap();
    let app = router(AppState::for_tcp(host.clone()));
    assert_eq!(
        app.oneshot(request(&id, "", true, None))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        runtime.agent_state().await.unwrap().status,
        crate::types::AgentStatus::Stopped
    );
    assert_eq!(
        host.runtime_db()
            .audit_events()
            .max_event_seq(Some(&id))
            .unwrap(),
        before
    );
    host.shutdown().await.unwrap();
}

#[cfg(unix)]
fn benchmark_resources() -> (u64, u64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the value on success and retains no pointer.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return (0, 0);
        }
        usage.assume_init()
    };
    let micros = |time: libc::timeval| time.tv_sec as u64 * 1_000_000 + time.tv_usec as u64;
    #[cfg(target_os = "macos")]
    let rss = usage.ru_maxrss as u64;
    #[cfg(not(target_os = "macos"))]
    let rss = usage.ru_maxrss as u64 * 1024;
    (micros(usage.ru_utime) + micros(usage.ru_stime), rss)
}

#[cfg(not(unix))]
fn benchmark_resources() -> (u64, u64) {
    (0, 0)
}

/// Reproducible offline router benchmark; never connects to a running daemon.
/// Run with `HOLON_LIB_TEST_SHARD=c cargo test --release --lib avatar_http_benchmark -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "offline release measurement, not a timing assertion"]
async fn avatar_http_benchmark() {
    use std::time::Instant;
    use tokio::task::JoinSet;

    let (_home, host, _) = fixture();
    for index in 0..128 {
        host.append_agent_identity(&AgentIdentityRecord::new(
            format!("avatar-bench-{index}"),
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))
        .unwrap();
    }
    for group in 0..3 {
        for count in [32, 128] {
            for (query, same_key) in [
                ("format=svg", false),
                ("format=svg", true),
                ("format=png&size=32", false),
                ("format=png&size=32", true),
                ("format=png&size=128", false),
                ("format=png&size=128", true),
                ("format=png&size=512", false),
                ("format=png&size=512", true),
            ] {
                let app = router(AppState::for_tcp(host.clone()));
                for phase in ["cold", "hot"] {
                    if phase == "hot" {
                        for index in 0..count {
                            let response = app
                                .clone()
                                .oneshot(request(
                                    &format!("avatar-bench-{}", if same_key { 0 } else { index }),
                                    query,
                                    true,
                                    None,
                                ))
                                .await
                                .unwrap();
                            assert_eq!(response.status(), StatusCode::OK);
                            to_bytes(response.into_body(), MAX_IMAGE_BYTES)
                                .await
                                .unwrap();
                        }
                    }
                    let cpu_before = benchmark_resources().0;
                    let start = Instant::now();
                    let mut tasks = JoinSet::new();
                    for index in 0..count {
                        let app = app.clone();
                        tasks.spawn(async move {
                            let start = Instant::now();
                            let response = app
                                .oneshot(request(
                                    &format!("avatar-bench-{}", if same_key { 0 } else { index }),
                                    query,
                                    true,
                                    None,
                                ))
                                .await
                                .unwrap();
                            let status = response.status();
                            let bytes = to_bytes(response.into_body(), MAX_IMAGE_BYTES)
                                .await
                                .unwrap();
                            (start.elapsed().as_micros(), status, bytes.len())
                        });
                    }
                    let mut samples = Vec::new();
                    let mut rejected_samples = Vec::new();
                    let mut total_bytes = 0;
                    let mut rejected_bytes = 0;
                    while let Some(result) = tasks.join_next().await {
                        let (elapsed, status, bytes) = result.unwrap();
                        assert!(matches!(
                            status,
                            StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE
                        ));
                        if status == StatusCode::OK {
                            total_bytes += bytes;
                            samples.push(elapsed);
                        } else {
                            rejected_bytes += bytes;
                            rejected_samples.push(elapsed);
                        }
                    }
                    let list_us = start.elapsed().as_micros();
                    let (cpu_us, rss_bytes) = benchmark_resources();
                    samples.sort_unstable();
                    rejected_samples.sort_unstable();
                    println!(
                        "{}",
                        json!({
                            "benchmark":"avatar_http_list","group":group,"count":count,
                            "query":query,"same_key":same_key,"phase":phase,
                            "successful":samples.len(),"p50_us":samples.get(samples.len()/2),
                            "p95_us":samples.get(samples.len()*95/100),
                            "list_us":list_us,"overloaded":rejected_samples.len(),"bytes":total_bytes,
                            "rejected_p50_us":rejected_samples.get(rejected_samples.len()/2),
                            "rejected_p95_us":rejected_samples.get(rejected_samples.len()*95/100),
                            "rejected_bytes":rejected_bytes,
                            "process_cpu_us":cpu_us.saturating_sub(cpu_before),
                            "process_peak_rss_bytes":rss_bytes
                        })
                    );
                    if phase == "hot" {
                        assert!(rejected_samples.is_empty());
                    }
                }
            }
        }
    }
}

#[tokio::test]
#[ignore = "offline release measurement, not a timing assertion"]
async fn avatar_cache_churn_benchmark() {
    use std::time::Instant;

    for group in 0..3 {
        let resources = AvatarResources::default();
        for phase in ["cold", "evicted", "hot"] {
            // Cycling all recipes exceeds the 512-entry cap. The last 512
            // remain resident, so the final phase isolates cache-hit overhead.
            let indices = if phase == "hot" { 640..1152 } else { 0..1152 };
            let mut samples = Vec::new();
            let mut bytes = 0;
            let cpu_before = benchmark_resources().0;
            for recipe in indices {
                let key = Key {
                    recipe,
                    ..Key::parse(ID, "format=svg").unwrap()
                };
                let start = Instant::now();
                let image = resources.get(key).await.unwrap();
                samples.push(start.elapsed().as_nanos() as u64);
                bytes += image.bytes.len();
            }
            let (cpu_us, rss_bytes) = benchmark_resources();
            samples.sort_unstable();
            println!(
                "{}",
                json!({
                    "benchmark":"avatar_cache_churn","group":group,"phase":phase,
                    "count":samples.len(),"p50_us":samples[samples.len()/2] as f64/1000.0,
                    "p95_us":samples[samples.len()*95/100] as f64/1000.0,
                    "process_cpu_us":cpu_us.saturating_sub(cpu_before),
                    "process_peak_rss_bytes":rss_bytes,"bytes":bytes
                })
            );
        }
    }
}
