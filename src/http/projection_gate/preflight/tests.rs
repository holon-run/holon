use std::{
    future::poll_fn,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::Poll,
};

use tokio::sync::Notify;

use super::*;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn key() -> ProjectionKey {
    ProjectionKey::AgentState("preflight-agent".into())
}

async fn enroll<F: Future>(mut future: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn typed_preflight_runs_for_every_caller_without_caching() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let calls = Arc::new(AtomicUsize::new(0));
    for expected in 1..=2 {
        let calls = Arc::clone(&calls);
        assert_eq!(
            gate.preflight(deadline(), move |_| {
                Ok(calls.fetch_add(1, Ordering::SeqCst) + 1)
            })
            .await
            .unwrap(),
            expected
        );
        gate.drain_tasks().await;
    }
    let state = gate.state.lock().unwrap();
    assert!(state.entries.is_empty());
    assert!(state.stale.is_empty());
    assert!(state.preflights.is_empty());
    assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
}

#[tokio::test]
async fn dropped_caller_and_cancelled_drain_retain_actual_worker_capacity() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let mut caller = Box::pin(gate.preflight(deadline(), move |scope| {
        let _ = started_tx.send(());
        release_rx.blocking_recv().unwrap();
        assert!(scope.is_cancelled());
        Ok(42)
    }));
    enroll(caller.as_mut()).await;
    started.await.unwrap();
    drop(caller);
    assert_eq!(gate.preflight_workers.available_permits(), 0);
    assert_eq!(gate.state.lock().unwrap().preflights.len(), 1);
    let called = Arc::new(AtomicBool::new(false));
    let attempted = Arc::clone(&called);
    let failure = gate
        .preflight(deadline(), move |_| {
            attempted.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(failure.error_code(), Some("projection_preflight_saturated"));
    assert_eq!(failure.retryable(), Some(true));
    assert!(!called.load(Ordering::SeqCst));
    // The independent build budget is not held by the saturated preflight lane.
    assert_eq!(
        gate.run_owned(key(), deadline(), Duration::from_secs(10), |_| async {
            Ok(Bytes::from_static(b"independent build"))
        })
        .await
        .unwrap(),
        Bytes::from_static(b"independent build")
    );
    let mut drain = Box::pin(gate.close_and_drain());
    enroll(drain.as_mut()).await;
    drop(drain);
    assert_eq!(gate.preflight_workers.available_permits(), 0);
    assert!(!gate.state.lock().unwrap().tasks.is_empty());
    release.send(()).unwrap();
    gate.close_and_drain().await;
    assert!(gate.state.lock().unwrap().preflights.is_empty());
    assert!(gate.state.lock().unwrap().tasks.is_empty());
    assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
}

#[tokio::test(start_paused = true)]
async fn deadline_returns_without_releasing_a_running_worker() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut caller = Box::pin(gate.preflight(deadline, move |scope| {
        let _ = started_tx.send(());
        release_rx.blocking_recv().unwrap();
        assert!(scope.is_cancelled());
        Ok(true)
    }));
    enroll(caller.as_mut()).await;
    started.await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        caller.await.unwrap_err().error_code(),
        Some("projection_deadline_exhausted")
    );
    assert_eq!(gate.preflight_workers.available_permits(), 0);
    assert_eq!(gate.state.lock().unwrap().preflights.len(), 1);
    release.send(()).unwrap();
    gate.close_and_drain().await;
    assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
    assert!(gate.state.lock().unwrap().entries.is_empty());
}

#[tokio::test(start_paused = true)]
async fn capability_read_and_build_share_the_callers_absolute_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )
    .unwrap();
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let began = Instant::now();
    let deadline = began + Duration::from_secs(5);
    let (started_tx, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let mut verification = Box::pin(gate.preflight(deadline, move |scope| {
        started_tx.send(()).unwrap();
        released.blocking_recv().unwrap();
        db.with_read_scope(scope)
            .observer_sync_foundations_with_self_heal()
            .map_err(crate::http::error_response)
            .map_err(ProjectionFailure::from)
    }));
    enroll(verification.as_mut()).await;
    started.await.unwrap();
    tokio::time::advance(Duration::from_secs(4)).await;
    release.send(()).unwrap();
    assert!(verification.await.unwrap().roster_snapshot_verified);
    assert_eq!(Instant::now() - began, Duration::from_secs(4));

    let (started_tx, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let mut caller = Box::pin(gate.run_owned(
        key(),
        deadline,
        Duration::from_secs(30),
        move |context| async move {
            context
                .blocking(move |_| {
                    started_tx.send(()).unwrap();
                    released.blocking_recv().unwrap();
                    Ok(Bytes::from_static(b"too late"))
                })
                .await?
        },
    ));
    enroll(caller.as_mut()).await;
    started.await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    let Err(ProjectionGateError::Build(failure)) = caller.await else {
        panic!("the original request deadline must still expire");
    };
    assert_eq!(failure.error_code(), Some("projection_deadline_exhausted"));
    assert_eq!(Instant::now() - began, Duration::from_secs(5));
    assert_eq!(gate.leaders.available_permits(), 0);
    release.send(()).unwrap();
    gate.close_and_drain().await;
    assert_eq!(gate.leaders.available_permits(), 1);
    assert!(gate.state.lock().unwrap().stale.is_empty());
}

#[tokio::test]
async fn busy_builds_leave_preflight_capacity_and_followers_available() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut leader = Box::pin(gate.run_owned(key(), deadline(), Duration::from_secs(10), {
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        move |_| async move {
            started.notify_one();
            release.notified().await;
            Ok(Bytes::from_static(b"verified follower"))
        }
    }));
    enroll(leader.as_mut()).await;
    started.notified().await;
    assert_eq!(gate.leaders.available_permits(), 0);
    assert!(gate.preflight(deadline(), |_| Ok(true)).await.unwrap());
    let called = Arc::new(AtomicBool::new(false));
    let attempted = Arc::clone(&called);
    let mut follower =
        Box::pin(
            gate.run_owned(key(), deadline(), Duration::from_secs(10), move |_| {
                attempted.store(true, Ordering::SeqCst);
                async { Ok(Bytes::from_static(b"unexpected new build")) }
            }),
        );
    enroll(follower.as_mut()).await;
    release.notify_one();
    assert_eq!(
        leader.await.unwrap(),
        Bytes::from_static(b"verified follower")
    );
    assert_eq!(
        follower.await.unwrap(),
        Bytes::from_static(b"verified follower")
    );
    assert!(!called.load(Ordering::SeqCst));
    gate.close_and_drain().await;
}

#[test]
fn close_cancels_queued_work_before_sql_and_rejects_new_registration() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (occupied_tx, occupied) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = occupied_tx.send(());
            release_rx.blocking_recv().unwrap();
        });
        occupied.await.unwrap();
        let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
        let called = Arc::new(AtomicBool::new(false));
        let attempted = Arc::clone(&called);
        let mut caller = Box::pin(gate.preflight(deadline(), move |_| {
            attempted.store(true, Ordering::SeqCst);
            Ok(())
        }));
        enroll(caller.as_mut()).await;
        gate.close();
        assert_eq!(gate.preflight_workers.available_permits(), 0);
        assert_eq!(gate.state.lock().unwrap().preflights.len(), 1);
        assert_eq!(
            caller.await.unwrap_err().error_code(),
            Some("projection_leader_cancelled")
        );
        release.send(()).unwrap();
        blocker.await.unwrap();
        gate.close_and_drain().await;
        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
        let attempted = Arc::clone(&called);
        assert_eq!(
            gate.preflight(deadline(), move |_| {
                attempted.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err()
            .error_code(),
            Some("projection_leader_cancelled")
        );
        assert!(!called.load(Ordering::SeqCst));
        assert!(gate.state.lock().unwrap().tasks.is_empty());
    });
}

#[tokio::test(start_paused = true)]
async fn settled_protection_failure_precedes_an_expired_caller_deadline() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let deadline = Instant::now() + Duration::from_secs(1);
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let mut caller = Box::pin(gate.preflight::<(), _>(deadline, move |_| {
        let _ = started_tx.send(());
        release_rx.blocking_recv().unwrap();
        Err(ProjectionFailure {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: json!({"code": "runtime_db_quarantined", "retryable": false}),
        })
    }));
    enroll(caller.as_mut()).await;
    started.await.unwrap();
    release.send(()).unwrap();
    gate.drain_tasks().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    let failure = caller.await.unwrap_err();
    assert_eq!(failure.error_code(), Some("runtime_db_quarantined"));
    assert_eq!(failure.retryable(), Some(false));
    assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
}

#[tokio::test]
async fn panicked_worker_is_joined_and_its_lane_recovers() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let failure = gate
        .preflight::<(), _>(deadline(), |_| panic!("injected preflight worker panic"))
        .await
        .unwrap_err();
    assert_eq!(
        failure.error_code(),
        Some("projection_preflight_worker_failed")
    );
    assert_eq!(failure.retryable(), Some(false));
    gate.drain_tasks().await;
    assert!(gate.state.lock().unwrap().preflights.is_empty());
    assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
    assert_eq!(gate.preflight(deadline(), |_| Ok(42)).await.unwrap(), 42);
    gate.close_and_drain().await;
}

#[tokio::test]
async fn protection_failure_is_observed_before_sent_result_is_consumed_or_dropped() {
    use std::io::Write;
    use std::sync::Mutex;
    use tracing::instrument::WithSubscriber;

    struct LogWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let log = Arc::new(Mutex::new(Vec::new()));
    let captured = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || LogWriter(captured.clone()))
        .finish();
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let (release, released) = oneshot::channel();
    let mut caller = Box::pin(
        gate.preflight::<(), _>(deadline(), move |_| {
            released.blocking_recv().unwrap();
            Err(ProjectionFailure {
                status: StatusCode::SERVICE_UNAVAILABLE,
                body: json!({
                    "code": "runtime_db_quarantined",
                    "retryable": false,
                    "error": "sensitive preflight detail",
                }),
            })
        })
        .with_subscriber(subscriber),
    );
    enroll(caller.as_mut()).await;
    release.send(()).unwrap();
    // Receiver is alive so send succeeds; never poll the caller again.
    gate.drain_tasks().await;
    drop(caller);
    let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    assert!(log.contains("runtime_db_quarantined"), "{log}");
    assert!(log.contains("503"), "{log}");
    assert!(log.contains("non-retryable failure"), "{log}");
    assert!(!log.contains("sensitive preflight detail"), "{log}");
    assert!(gate.state.lock().unwrap().preflights.is_empty());
    assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
}

#[test]
fn warning_respects_each_dispatchers_enabled_and_event_filters() {
    use tracing::{span, subscriber::Interest, Metadata, Subscriber};

    struct Capture {
        enabled: bool,
        event_enabled: bool,
        events: Arc<AtomicUsize>,
    }
    impl Subscriber for Capture {
        fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
            // Reproduce an earlier dispatcher caching this callsite as disabled.
            Interest::never()
        }
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            self.enabled
        }
        fn event_enabled(&self, _: &tracing::Event<'_>) -> bool {
            self.event_enabled
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {
            self.events.fetch_add(1, Ordering::SeqCst);
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    let emit = || emit_warning(Some("test_code"), StatusCode::SERVICE_UNAVAILABLE, "test");
    let events = Arc::new(AtomicUsize::new(0));
    let dispatch = |enabled, event_enabled| {
        tracing::Dispatch::new(Capture {
            enabled,
            event_enabled,
            events: events.clone(),
        })
    };
    let disabled = dispatch(false, true);
    let event_disabled = dispatch(true, false);
    let enabled = dispatch(true, true);
    tracing::dispatcher::with_default(&disabled, emit);
    tracing::dispatcher::with_default(&event_disabled, emit);
    assert_eq!(events.load(Ordering::SeqCst), 0);
    tracing::dispatcher::with_default(&enabled, || {
        emit();
        tracing::dispatcher::with_default(&disabled, emit);
        tracing::dispatcher::with_default(&event_disabled, emit);
        assert_eq!(events.load(Ordering::SeqCst), 1);
        emit();
    });
    assert_eq!(events.load(Ordering::SeqCst), 2);
}

#[test]
fn expired_queued_preflight_never_runs_its_sql_closure() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (occupied_tx, occupied) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = occupied_tx.send(());
            release_rx.blocking_recv().unwrap();
        });
        occupied.await.unwrap();
        let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
        let called = Arc::new(AtomicBool::new(false));
        let attempted = called.clone();
        let mut caller = Box::pin(gate.preflight(
            Instant::now() + Duration::from_millis(50),
            move |_| {
                attempted.store(true, Ordering::SeqCst);
                Ok(())
            },
        ));
        enroll(caller.as_mut()).await;
        assert_eq!(
            caller.await.unwrap_err().error_code(),
            ProjectionFailure::deadline_exhausted().error_code()
        );
        assert_eq!(gate.preflight_workers.available_permits(), 0);
        release.send(()).unwrap();
        blocker.await.unwrap();
        gate.close_and_drain().await;
        assert!(!called.load(Ordering::SeqCst));
        assert!(gate.state.lock().unwrap().preflights.is_empty());
        assert_eq!(gate.preflight_workers.available_permits(), MAX_WORKERS);
    });
}
