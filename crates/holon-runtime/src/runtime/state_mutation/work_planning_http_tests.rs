use super::test_support::publish_unprepared_scheduler_frontier;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_work_items_progress_while_system_tick_waits_for_preparation() -> Result<()> {
    exercise_http_tick_progress(1, None).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "isolated #4169 performance run; do not run in routine CI"]
async fn measure_dual_reader_pools_http_and_tick() -> Result<()> {
    for epoch in 0..8 {
        exercise_http_tick_progress(8, Some(epoch)).await?;
    }
    Ok(())
}

async fn exercise_http_tick_progress(requests: usize, epoch: Option<usize>) -> Result<()> {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    let started = std::time::Instant::now();
    let stage = |phase, responses| {
        if let Some(epoch) = epoch {
            println!(
                "reader_pool_sample {}",
                serde_json::json!({
                    "scenario": "http_and_tick",
                    "epoch": epoch,
                    "phase": phase,
                    "elapsed_us": started.elapsed().as_micros() as u64,
                    "responses": responses,
                    "reader_pools": crate::diagnostics::performance_snapshot().reader_pools,
                })
            );
        }
    };
    stage("before_open", 0);
    let guard = Duration::from_secs(5);
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    // Measure tick emission separately from the run loop consuming that tick.
    let run_loop = tokio::time::timeout(guard, runtime.inner.run_loop_test_gate.write()).await?;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let work = runtime
        .create_work_item("isolated dual-pool work".into(), None, None, Vec::new())
        .await?;
    publish_unprepared_scheduler_frontier(&runtime).await?;
    let db = host.runtime_db();
    let capacity = db.preparation_pool_stats_for_tests().capacity;
    let mut leases = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..capacity {
        let db = db.clone();
        let (entered, held) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        releases.push(release);
        leases.push(tokio::task::spawn_blocking(move || {
            db.read_for_preparation(|tx| {
                tx.query_row("SELECT 1", [], |_| Ok(()))?;
                let _ = entered.send(());
                match released.recv_timeout(guard * 3) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(()),
                    Err(error) => Err(error.into()),
                }
            })
        }));
        tokio::time::timeout(guard, held).await??;
    }
    let before = db.preparation_pool_stats_for_tests();
    assert_eq!(before.current_connections, capacity);
    assert_eq!(before.idle_connections, 0);
    let tick_runtime = runtime.clone();
    let tick = tokio::spawn(async move {
        let pending = PendingWakeHint {
            reason: "dual-pool progress".into(),
            description: None,
            source: None,
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: tick_runtime.now(),
        };
        tick_runtime
            .emit_system_tick_from_wake_hint_with_decision(&pending)
            .await
    });
    // Observe checkout, not elapsed time: a cache hit cannot satisfy this oracle.
    tokio::time::timeout(guard, async {
        let mut observe = tokio::time::interval(Duration::from_millis(1));
        while db.preparation_pool_stats_for_tests().checkout_attempts == before.checkout_attempts
            && !tick.is_finished()
        {
            observe.tick().await;
        }
    })
    .await?;
    assert!(
        !tick.is_finished(),
        "tick did not wait for a preparation lease"
    );
    stage("tick_queued", 0);

    let ordinary_before = db.reader_pool_stats_for_tests().checkout_attempts;
    let app = crate::http::router(crate::http::AppState::for_unix(host.clone()));
    let uri = format!("/api/agents/{}/work-items", host.config().default_agent_id);
    let mut responses = tokio::task::JoinSet::new();
    for _ in 0..requests {
        responses.spawn(
            app.clone()
                .oneshot(Request::builder().uri(uri.as_str()).body(Body::empty())?),
        );
    }
    tokio::time::timeout(guard, async {
        while let Some(response) = responses.join_next().await {
            let response = response??;
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await?;
            assert_eq!(
                status,
                axum::http::StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&body)
            );
            let items: Vec<crate::types::WorkItemRecord> = serde_json::from_slice(&body)?;
            assert!(items.iter().any(|item| item.id == work.id));
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert!(
        db.reader_pool_stats_for_tests().checkout_attempts >= ordinary_before + requests as u64
    );
    assert!(!tick.is_finished());
    assert_eq!(db.preparation_pool_stats_for_tests().idle_connections, 0);
    stage("http_finished_before_release", requests);

    for release in releases {
        release.send(())?;
    }
    for lease in leases {
        tokio::time::timeout(guard, lease).await???;
    }
    assert!(tokio::time::timeout(guard, tick).await???);
    let after = db.preparation_pool_stats_for_tests();
    assert_eq!(after.idle_connections, capacity);
    assert_eq!(after.checkout_timeouts, before.checkout_timeouts);
    assert_eq!(
        runtime
            .storage()
            .read_recent_messages(20)?
            .iter()
            .filter(|message| message.kind == MessageKind::SystemTick)
            .count(),
        1
    );
    stage("tick_finished_after_release", requests);
    drop(app);
    drop(refresh);
    drop(run_loop);
    drop(runtime);
    tokio::time::timeout(guard, host.shutdown()).await??;
    drop(host);
    stage("after_close", requests);
    Ok(())
}
