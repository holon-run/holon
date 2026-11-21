use super::test_support::TempRuntimeDb;
use anyhow::Result;
use rusqlite::TransactionBehavior;
use std::time::Duration;

#[tokio::test]
async fn observer_sync_self_heal_keeps_preflight_capacity_until_the_writer_finishes() -> Result<()>
{
    use std::future::{poll_fn, Future};
    use std::sync::{atomic::Ordering, Arc};
    use std::task::Poll;

    use crate::http::{error_response, ProjectionFailure, ProjectionGate};

    let fixture = TempRuntimeDb::new()?;
    let db = &fixture.db;
    let mut connection = db.connection()?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verified = 0
         WHERE capability = 'agent_identity_reserved'",
        [],
    )?;
    assert!(!db.observer_sync_foundations()?.agent_identity_reserved);
    db.observer_sync_self_heal_last_attempt_ms
        .store(0, Ordering::Relaxed);
    // WAL readers can still see the failing verdict, but self-heal cannot write.
    let writer = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let gate = Arc::new(ProjectionGate::default());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let worker_db = db.clone();
    let mut caller = Box::pin(gate.preflight(deadline, move |scope| {
        worker_db
            .with_read_scope(scope)
            .observer_sync_foundations_with_self_heal()
            .map_err(error_response)
            .map_err(ProjectionFailure::from)
    }));
    poll_fn(|cx| {
        assert!(caller.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while db
            .observer_sync_self_heal_last_attempt_ms
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    // The heal has passed the read and entered its uncancellable writer phase.
    drop(caller);
    let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&attempted);
    let mut queued = Box::pin(gate.preflight::<(), _>(deadline, move |_| {
        observed.store(true, Ordering::SeqCst);
        Ok(())
    }));
    poll_fn(|cx| {
        assert!(queued.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    // The uncancellable writer keeps the only worker slot, so the second
    // caller queues in bounded admission instead of running its SQL.
    // Staying pending (rather than rejected) is the queued admission.
    assert!(!attempted.load(Ordering::SeqCst));
    let mut drain = Box::pin(gate.close_and_drain());
    poll_fn(|cx| {
        assert!(drain.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    writer.rollback()?;
    tokio::time::timeout(Duration::from_secs(5), drain).await?;
    // Cancellation may reject the final scoped read, but cannot undo the heal.
    // Close also failed the queued caller before it ever reached its SQL.
    let failure = tokio::time::timeout(Duration::from_secs(5), queued)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.error_code(), Some("projection_leader_cancelled"));
    assert!(!attempted.load(Ordering::SeqCst));
    assert!(db.observer_sync_foundations()?.agent_identity_reserved);
    Ok(())
}
