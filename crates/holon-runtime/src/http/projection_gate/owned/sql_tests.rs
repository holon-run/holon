use std::sync::atomic::{AtomicBool, Ordering};

use crate::runtime_db::{RuntimeDb, RuntimeDbRetryableError};

use super::*;

#[tokio::test]
async fn last_waiter_interrupts_real_sql_and_retains_capacity_until_worker_exits() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )
    .unwrap();
    db.configure_reader_pool_for_tests(1, Duration::from_millis(100))
        .unwrap();
    let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
    let key = ProjectionKey::AgentState("sql-agent".into());
    let deadline = Instant::now() + Duration::from_secs(30);
    let (started_tx, started) = oneshot::channel();
    let (cancelled_tx, cancelled) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let interrupted = Arc::new(AtomicBool::new(false));
    let observed = interrupted.clone();
    let worker_db = db.clone();
    let mut caller = Box::pin(gate.run_owned(
        key.clone(),
        deadline,
        Duration::from_secs(30),
        move |context| async move {
            context
                .blocking(move |scope| {
                    let scoped = worker_db.with_read_scope(scope);
                    let result = scoped.with_read(|connection| {
                        let mut statement = connection.prepare(
                            "WITH RECURSIVE n(x) AS (
                                VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < 100000000
                             ) SELECT 0 UNION ALL SELECT sum(x) FROM n",
                        )?;
                        let mut rows = statement.query([])?;
                        assert_eq!(rows.next()?.unwrap().get::<_, i64>(0)?, 0);
                        started_tx.send(()).unwrap();
                        let result = rows.next();
                        observed.store(
                            matches!(
                                &result, Err(rusqlite::Error::SqliteFailure(code, _))
                                    if code.code == rusqlite::ErrorCode::OperationInterrupted
                            ),
                            Ordering::SeqCst,
                        );
                        result?;
                        Ok(())
                    });
                    cancelled_tx.send(result).unwrap();
                    // SQL has ended, but the actual worker still owns the build lease.
                    released.blocking_recv().unwrap();
                    Ok(Bytes::from_static(b"must not be cached"))
                })
                .await?
        },
    ));
    poll_fn(|cx| {
        assert!(caller.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    drop(caller);
    let error = tokio::time::timeout(Duration::from_secs(5), cancelled)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.is::<RuntimeDbRetryableError>(), "{error:#}");
    assert!(interrupted.load(Ordering::SeqCst));
    assert_eq!(gate.leaders.available_permits(), 0);
    assert!(matches!(
        gate.run_owned(key.clone(), deadline, Duration::from_secs(30), |_| async {
            Ok(Bytes::new())
        })
        .await,
        Err(ProjectionGateError::Rejected)
    ));
    release.send(()).unwrap();
    gate.drain_tasks().await;
    assert_eq!(gate.leaders.available_permits(), 1);
    assert!(gate.state.lock().unwrap().entries.is_empty());
    assert!(gate.state.lock().unwrap().stale.is_empty());
    assert_eq!(
        db.with_read(|connection| {
            Ok(connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
        })
        .unwrap(),
        1
    );
    let stats = db.reader_pool_stats_for_tests();
    assert_eq!(stats.connections_opened, 1);
    assert_eq!(stats.connections_dropped, 0);
    assert_eq!(
        gate.run_owned(key, deadline, Duration::from_secs(30), |_| async {
            Ok(Bytes::from_static(b"healthy"))
        })
        .await
        .unwrap(),
        Bytes::from_static(b"healthy")
    );
    gate.close_and_drain().await;
}
