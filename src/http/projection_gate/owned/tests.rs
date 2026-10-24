use std::{
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};

use tokio::sync::Notify;

use super::*;

fn key() -> ProjectionKey {
    ProjectionKey::AgentState("agent-a".into())
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

async fn enroll<F: Future>(mut future: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

fn assert_deadline(result: ProjectionDelivery) {
    match result {
        Err(ProjectionGateError::Build(failure)) => {
            assert_eq!(failure.error_code(), Some("projection_deadline_exhausted"));
        }
        other => panic!("expected deadline failure without stale, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_commits_the_deadline_crossed_since_the_precheck() {
    let gate = ProjectionGate::new(1, Duration::from_millis(250));
    let shared_deadline = Instant::now() + Duration::from_millis(100);
    let scope = RuntimeDbReadScope::new(shared_deadline.into_std());
    let (sender, _) = watch::channel(FlightState::Pending);
    let flight = Arc::new(Flight {
        state: sender,
        scope: Some(scope),
        deadline: Some(shared_deadline),
    });
    gate.state.lock().unwrap().entries.insert(
        key(),
        Entry::InFlight {
            flight: Arc::clone(&flight),
            waiters: HashMap::from([(1, deadline())]),
        },
    );
    assert!(shared_deadline > Instant::now());
    assert!(!flight.scope.as_ref().unwrap().is_cancelled());
    // The timer has not committed a terminal delivery when this path resumes.
    tokio::time::advance(Duration::from_millis(101)).await;
    gate.close_cancelled_build(&key(), &flight);
    assert_deadline(wait_for_flight(Arc::clone(&flight)).await);
    assert!(gate.state.lock().unwrap().entries.is_empty());
    assert!(gate.state.lock().unwrap().stale.is_empty());
    assert!(flight.scope.as_ref().unwrap().is_cancelled());
    gate.close_cancelled_build(&key(), &flight);
    assert_deadline(wait_for_flight(flight).await);
}

#[tokio::test]
async fn close_before_the_registered_supervisor_polls_skips_the_build() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let called = Arc::new(AtomicBool::new(false));
    let mut caller = Box::pin(gate.run_owned(key(), deadline(), Duration::from_secs(10), {
        let called = Arc::clone(&called);
        move |_| {
            called.store(true, Ordering::SeqCst);
            async { Ok(Bytes::from_static(b"unexpected")) }
        }
    }));
    enroll(caller.as_mut()).await;
    gate.close();
    assert_eq!(gate.leaders.available_permits(), 0);
    assert!(!gate.state.lock().unwrap().tasks.is_empty());
    assert!(matches!(caller.await, Err(ProjectionGateError::Build(_))));
    gate.close_and_drain().await;
    assert!(gate.state.lock().unwrap().tasks.is_empty());
    assert_eq!(gate.leaders.available_permits(), 1);
    assert!(!called.load(Ordering::SeqCst));
}

#[test]
fn settled_protection_failure_precedes_an_expired_shared_deadline() {
    let gate = ProjectionGate::new(1, Duration::from_millis(250));
    let expired = Instant::now() - Duration::from_millis(1);
    let scope = RuntimeDbReadScope::new(expired.into_std());
    let (sender, _) = watch::channel(FlightState::Pending);
    let flight = Arc::new(Flight {
        state: sender,
        scope: Some(scope),
        deadline: Some(expired),
    });
    gate.state.lock().unwrap().entries.insert(
        key(),
        Entry::InFlight {
            flight: Arc::clone(&flight),
            waiters: HashMap::from([(1, deadline())]),
        },
    );
    let lease = Arc::new(BuildLease::new(
        Arc::clone(&gate.leaders).try_acquire_owned().unwrap(),
    ));
    let mut guard = LeaderGuard::with_lease(&gate, key(), flight, lease);
    match guard.finish(Err(ProjectionFailure {
        status: StatusCode::SERVICE_UNAVAILABLE,
        body: json!({"code": "runtime_db_quarantined", "retryable": false}),
    })) {
        Err(ProjectionGateError::Build(failure)) => {
            assert_eq!(failure.error_code(), Some("runtime_db_quarantined"));
            assert_eq!(failure.retryable(), Some(false));
        }
        other => panic!("settled protection failure was reclassified: {other:?}"),
    }
    assert!(gate.state.lock().unwrap().entries.is_empty());
    assert!(gate.state.lock().unwrap().stale.is_empty());
}

#[tokio::test]
async fn dropping_first_caller_does_not_cancel_a_live_follower() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut first = Box::pin(gate.run_owned(key(), deadline(), Duration::from_secs(10), {
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        move |context| async move {
            started.notify_one();
            release.notified().await;
            assert!(!context.read_scope().is_cancelled());
            Ok(Bytes::from_static(b"shared"))
        }
    }));
    enroll(first.as_mut()).await;
    started.notified().await;
    let mut follower = Box::pin(gate.run_owned(
        key(),
        deadline(),
        Duration::from_secs(10),
        |_| async { panic!("follower must not build") },
    ));
    enroll(follower.as_mut()).await;
    drop(first);
    release.notify_one();
    assert_eq!(follower.await.unwrap(), Bytes::from_static(b"shared"));
    gate.close_and_drain().await;
    assert_eq!(gate.leaders.available_permits(), 1);
}

#[tokio::test]
async fn last_waiter_cancellation_keeps_capacity_until_worker_returns() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut caller = Box::pin(gate.run_owned(key(), deadline(), Duration::from_secs(10), {
        let started = Arc::clone(&started);
        let cancelled = Arc::clone(&cancelled);
        let release = Arc::clone(&release);
        move |context| async move {
            let scope = Some(context.read_scope());
            started.notify_one();
            RuntimeDbReadScope::cancelled(&scope).await;
            cancelled.notify_one();
            release.notified().await;
            Ok(Bytes::from_static(b"late"))
        }
    }));
    enroll(caller.as_mut()).await;
    started.notified().await;
    drop(caller);
    cancelled.notified().await;
    assert_eq!(gate.leaders.available_permits(), 0);
    assert!(matches!(
        gate.run_owned(key(), deadline(), Duration::from_secs(10), |_| async {
            Ok(Bytes::from_static(b"unexpected"))
        })
        .await,
        Err(ProjectionGateError::Rejected)
    ));
    release.notify_one();
    gate.drain_tasks().await;
    assert!(gate.state.lock().unwrap().entries.is_empty());
    assert!(gate.state.lock().unwrap().stale.is_empty());
    assert_eq!(
        gate.run_owned(key(), deadline(), Duration::from_secs(10), |_| async {
            Ok(Bytes::from_static(b"healthy"))
        })
        .await
        .unwrap(),
        Bytes::from_static(b"healthy")
    );
    gate.close_and_drain().await;
}

#[tokio::test(start_paused = true)]
async fn caller_deadlines_are_independent_of_each_other() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut first = Box::pin(gate.run_owned(
        key(),
        Instant::now() + Duration::from_millis(100),
        Duration::from_secs(10),
        {
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            move |context| async move {
                started.notify_one();
                release.notified().await;
                assert!(!context.read_scope().is_cancelled());
                Ok(Bytes::from_static(b"shared"))
            }
        },
    ));
    enroll(first.as_mut()).await;
    started.notified().await;
    let mut follower = Box::pin(gate.run_owned(
        key(),
        deadline(),
        Duration::from_secs(10),
        |_| async { panic!("follower must not build") },
    ));
    enroll(follower.as_mut()).await;
    tokio::time::advance(Duration::from_millis(101)).await;
    assert_deadline(first.await);
    release.notify_one();
    assert_eq!(follower.await.unwrap(), Bytes::from_static(b"shared"));
    gate.close_and_drain().await;
}

#[tokio::test(start_paused = true)]
async fn shared_deadline_does_not_extend_or_serve_stale_before_late_protection() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(1)));
    gate.run(key(), || async { Ok(Bytes::from_static(b"old")) })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_millis(2)).await;
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut first = Box::pin(gate.run_owned(key(), deadline(), Duration::from_secs(1), {
        let started = Arc::clone(&started);
        let cancelled = Arc::clone(&cancelled);
        let release = Arc::clone(&release);
        move |context| async move {
            let scope = Some(context.read_scope());
            started.notify_one();
            RuntimeDbReadScope::cancelled(&scope).await;
            cancelled.notify_one();
            release.notified().await;
            Err(ProjectionFailure {
                status: StatusCode::SERVICE_UNAVAILABLE,
                body: json!({"code": "runtime_db_quarantined", "retryable": false}),
            })
        }
    }));
    enroll(first.as_mut()).await;
    started.notified().await;
    tokio::time::advance(Duration::from_millis(900)).await;
    let mut follower = Box::pin(gate.run_owned(
        key(),
        deadline(),
        Duration::from_secs(100),
        |_| async { panic!("follower must not extend the shared deadline") },
    ));
    enroll(follower.as_mut()).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_deadline(first.await);
    assert_deadline(follower.await);
    cancelled.notified().await;
    assert_eq!(gate.leaders.available_permits(), 0);
    release.notify_one();
    gate.drain_tasks().await;
    let state = gate.state.lock().unwrap();
    assert!(state.entries.is_empty());
    assert_eq!(
        state.stale.get(&key()).unwrap().bytes,
        Bytes::from_static(b"old")
    );
}

#[tokio::test]
async fn invalidated_worker_cannot_publish_over_a_replacement() {
    let gate = Arc::new(ProjectionGate::new(2, Duration::from_millis(250)));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut old = Box::pin(gate.run_owned(key(), deadline(), Duration::from_secs(10), {
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        move |_| async move {
            started.notify_one();
            release.notified().await;
            Ok(Bytes::from_static(b"old"))
        }
    }));
    enroll(old.as_mut()).await;
    started.notified().await;
    gate.invalidate_agent("agent-a");
    assert!(matches!(old.await, Err(ProjectionGateError::Build(_))));
    assert_eq!(
        gate.run_owned(key(), deadline(), Duration::from_secs(10), |_| async {
            Ok(Bytes::from_static(b"replacement"))
        })
        .await
        .unwrap(),
        Bytes::from_static(b"replacement")
    );
    release.notify_one();
    gate.drain_tasks().await;
    assert_eq!(
        gate.run_owned(key(), deadline(), Duration::from_secs(10), |_| async {
            panic!("replacement must remain cached")
        })
        .await
        .unwrap(),
        Bytes::from_static(b"replacement")
    );
    gate.close_and_drain().await;
}

#[tokio::test]
async fn cancelled_drain_retains_registered_blocking_work_and_capacity() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let mut caller = Box::pin(gate.run_owned(
        key(),
        deadline(),
        Duration::from_secs(10),
        move |context| async move {
            context
                .blocking(move |scope| {
                    let _ = started_tx.send(());
                    release_rx.blocking_recv().unwrap();
                    assert!(scope.is_cancelled());
                    Bytes::from_static(b"late")
                })
                .await
        },
    ));
    enroll(caller.as_mut()).await;
    started.await.unwrap();
    gate.close();
    assert!(matches!(caller.await, Err(ProjectionGateError::Build(_))));
    let mut drain = Box::pin(gate.close_and_drain());
    enroll(drain.as_mut()).await;
    drop(drain);
    assert_eq!(gate.leaders.available_permits(), 0);
    assert!(!gate.state.lock().unwrap().tasks.is_empty());
    release.send(()).unwrap();
    gate.close_and_drain().await;
    assert!(gate.state.lock().unwrap().tasks.is_empty());
    assert_eq!(gate.leaders.available_permits(), 1);
    assert!(matches!(
        gate.run_owned(key(), deadline(), Duration::from_secs(10), |_| async {
            panic!("closed gate must not build")
        })
        .await,
        Err(ProjectionGateError::Rejected)
    ));
}

#[tokio::test]
async fn panicked_supervisor_does_not_release_its_blocking_workers_capacity() {
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let mut caller = Box::pin(gate.run_owned(
        key(),
        deadline(),
        Duration::from_secs(10),
        move |context| async move {
            let mut work = Box::pin(context.blocking(move |_| {
                let _ = started_tx.send(());
                release_rx.blocking_recv().unwrap();
            }));
            enroll(work.as_mut()).await;
            started.await.unwrap();
            panic!("injected projection supervisor panic");
        },
    ));
    enroll(caller.as_mut()).await;
    assert!(matches!(caller.await, Err(ProjectionGateError::Build(_))));
    assert_eq!(gate.leaders.available_permits(), 0);
    release.send(()).unwrap();
    gate.drain_tasks().await;
    assert_eq!(gate.leaders.available_permits(), 1);
    assert_eq!(
        gate.run_owned(key(), deadline(), Duration::from_secs(10), |_| async {
            Ok(Bytes::from_static(b"healthy"))
        })
        .await
        .unwrap(),
        Bytes::from_static(b"healthy")
    );
    gate.close_and_drain().await;
}
