use super::*;

#[path = "operator_state_observation_http_tests.rs"]
mod observation_tests;

#[tokio::test]
async fn state_http_observes_the_published_cut_during_an_open_commit_window() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    let _run_loop = runtime.pause_run_loop_for_monitor_test().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
    let uri = format!("/api/agents/{}/state", runtime.instance_agent_id());
    let read = || {
        let app = app.clone();
        let uri = uri.clone();
        async move {
            let response = app
                .oneshot(Request::builder().uri(uri).body(Body::empty())?)
                .await?;
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await?;
            Ok::<_, anyhow::Error>((status, serde_json::from_slice::<serde_json::Value>(&body)?))
        }
    };
    let (status, before) = read().await?;
    assert_eq!(status, StatusCode::OK, "{before}");
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    reservation.begin("state-http-open-commit-window".into())?;
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(mutation_owner::MutationOwnerError::NeedsReconcile(_))
    ));
    let frontier = runtime
        .inner
        .scheduler_audit_frontier
        .fetch_add(1, Ordering::AcqRel);
    let during = tokio::time::timeout(std::time::Duration::from_secs(1), read()).await;
    runtime
        .inner
        .scheduler_audit_frontier
        .store(frontier, Ordering::Release);
    reservation.definitely_not_committed()?;
    drop(reservation);
    let (status, body) = during??;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["observation"]["status"], "committing");
    assert_eq!(
        body["observation"]["publication_generation"],
        root.token.generation
    );
    assert_eq!(body["observation"]["owner_epoch"], root.token.owner_epoch);
    assert_eq!(body["agent"], before["agent"]);
    assert_eq!(body["tasks"], before["tasks"]);
    assert_eq!(body["waits"], before["waits"]);
    assert_eq!(body["timers"], before["timers"]);
    let (status, after) = read().await?;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["observation"]["status"], "current");
    assert_eq!(
        after["observation"]["publication_generation"],
        root.token.generation
    );
    runtime
        .schedule_timer(3_600_000, None, Some("next State publication".into()))
        .await?;
    let next = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(next.token.generation > root.token.generation);
    let (status, committed) = read().await?;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(
        committed["observation"]["publication_generation"],
        next.token.generation
    );
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    reservation.begin("state-http-second-open-commit-window".into())?;
    let during = tokio::time::timeout(std::time::Duration::from_secs(1), read()).await;
    reservation.definitely_not_committed()?;
    drop(reservation);
    let (status, body) = during??;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["observation"]["status"], "committing");
    assert_eq!(
        body["observation"]["publication_generation"],
        next.token.generation
    );
    assert_eq!(body["agent"], committed["agent"]);
    assert_eq!(body["timers"], committed["timers"]);
    drop((_refresh, _run_loop));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn state_http_rejects_a_published_cut_after_identity_revision_or_incarnation_changes(
) -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    for incarnation_change in [false, true] {
        let (_home, host, runtime) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        runtime.await_initial_resources().await;
        let run_loop = runtime.pause_run_loop_for_monitor_test().await;
        let refresh = runtime.hold_resource_refresh_for_test().await?;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let mut identity = root.identity.as_deref().unwrap().clone();
        identity.revision += 1;
        if incarnation_change {
            identity.incarnation += 1;
        }
        host.append_agent_identity(&identity)?;
        let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/agents/{}/state", runtime.instance_agent_id()))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["code"], "runtime_db_busy", "{body}");
        assert_eq!(body["retryable"], true, "{body}");
        assert!(body.get("agent").is_none(), "{body}");
        assert!(body.get("observation").is_none(), "{body}");
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
        drop((refresh, run_loop));
        host.shutdown().await?;
    }
    Ok(())
}
