use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::persistence::{persistence_certainty, PersistenceFailure};

fn writer_with_locked_commit() -> Result<(tempfile::TempDir, RuntimeDbWriter, Connection)> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("runtime.sqlite");
    let connection = Connection::open(&path)?;
    connection.busy_timeout(Duration::from_millis(1))?;
    // A rollback-journal reader allows BEGIN IMMEDIATE and blocks COMMIT.
    connection.execute_batch("CREATE TABLE test_rows (value INTEGER);")?;
    let writer = RuntimeDbWriter::open_starting(
        path.clone(),
        connection,
        &crate::diagnostics::NO_DIAGNOSTICS,
    )?;
    let reader = Connection::open(path)?;
    reader.execute_batch("BEGIN;")?;
    let _: u64 = reader.query_row("SELECT count(*) FROM test_rows", [], |row| row.get(0))?;
    Ok((temp, writer, reader))
}

#[test]
fn persistence_sync_commit_busy_is_not_retried() -> Result<()> {
    let (_temp, writer, reader) = writer_with_locked_commit()?;
    let attempts = Arc::new(AtomicUsize::new(0));
    let worker_attempts = Arc::clone(&attempts);
    let (completion_tx, completion_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = writer.append_wait(|tx| {
            worker_attempts.fetch_add(1, Ordering::SeqCst);
            tx.execute("INSERT INTO test_rows VALUES (1)", [])?;
            Ok(())
        });
        let _ = completion_tx.send(result);
    });
    let completed = completion_rx.recv_timeout(Duration::from_secs(2));
    // Release the lock even when a regression times out, so the worker can finish.
    reader.execute_batch("ROLLBACK;")?;
    worker
        .join()
        .map_err(|_| anyhow!("transaction worker panicked"))?;
    let error = completed
        .context("an indeterminate COMMIT must return without retrying")?
        .expect_err("the reader must make COMMIT busy");

    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        persistence_certainty(&error),
        Some(PersistenceCertainty::Indeterminate)
    );
    assert_eq!(
        error.downcast_ref::<PersistenceFailure>().unwrap().phase(),
        PersistenceFailurePhase::Commit
    );
    assert!(!is_retryable_transaction_error(&error));
    Ok(())
}

#[test]
fn persistence_background_commit_busy_is_not_retried() -> Result<()> {
    let (_temp, writer, reader) = writer_with_locked_commit()?;
    let attempts = Arc::new(AtomicUsize::new(0));
    let job_attempts = Arc::clone(&attempts);
    let (completion_tx, completion_rx) = mpsc::channel();
    writer
        .append_tx
        .send(RuntimeDbWriteRequest {
            context: RuntimeDbWriteContext::background("persistence_test", "test_rows"),
            job: Box::new(move |tx| {
                job_attempts.fetch_add(1, Ordering::SeqCst);
                tx.execute("INSERT INTO test_rows VALUES (1)", [])?;
                Ok(())
            }),
            completion: Some(completion_tx),
        })
        .map_err(|_| anyhow!("test writer queue disconnected"))?;
    let completed = completion_rx.recv_timeout(Duration::from_secs(2));
    reader.execute_batch("ROLLBACK;")?;
    writer.flush_background_writes_for_tests()?;

    assert!(completed
        .context("an indeterminate queued COMMIT must not replay its callback")?
        .is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn persistence_active_transaction_quarantines_writer_and_preserves_error() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("runtime.sqlite");
    let connection = Connection::open(&path)?;
    let writer =
        RuntimeDbWriter::open_starting(path, connection, &crate::diagnostics::NO_DIAGNOSTICS)?;
    let connection = writer.state.connection.lock().unwrap();
    connection.execute_batch("BEGIN IMMEDIATE;")?;
    let original = crate::RuntimeDbRetryableError::new(
        "test transaction",
        &writer.state.path,
        "database is busy",
    );
    let error = mark_persistence_failure(
        original.into(),
        PersistenceCertainty::Indeterminate,
        PersistenceFailurePhase::Rollback,
        Some("injected cleanup failure".into()),
    );
    let error = writer
        .state
        .finish_transaction_attempt(&connection, Err::<(), _>(error))
        .expect_err("cleanup failure must remain an error");

    assert_eq!(
        persistence_certainty(&error),
        Some(PersistenceCertainty::Indeterminate)
    );
    assert!(error
        .downcast_ref::<crate::RuntimeDbRetryableError>()
        .is_some());
    assert_eq!(
        writer.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    connection.execute_batch("ROLLBACK;")?;
    drop(connection);

    let called = Cell::new(false);
    let error = writer
        .append_wait_once(|_| {
            called.set(true);
            Ok(())
        })
        .expect_err("quarantined writer must reject the next transaction");
    assert!(!called.get());
    assert_eq!(
        persistence_certainty(&error),
        Some(PersistenceCertainty::DefinitelyNotCommitted)
    );
    assert_eq!(
        error.downcast_ref::<PersistenceFailure>().unwrap().phase(),
        PersistenceFailurePhase::BeforeBegin
    );
    assert!(error.downcast_ref::<RuntimeDbProtectionError>().is_some());
    Ok(())
}

#[test]
fn persistence_queue_rejection_happens_before_callback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("runtime.sqlite");
    let connection = Connection::open(&path)?;
    let writer =
        RuntimeDbWriter::open_starting(path, connection, &crate::diagnostics::NO_DIAGNOSTICS)?;
    writer.state.queue.state.lock().unwrap().next_ticket = u64::MAX;
    let called = Cell::new(false);
    let error = writer
        .append_wait(|_| {
            called.set(true);
            Ok(())
        })
        .expect_err("ticket overflow must reject this transaction");

    assert!(!called.get());
    assert_eq!(
        persistence_certainty(&error),
        Some(PersistenceCertainty::DefinitelyNotCommitted)
    );
    assert_eq!(
        error.downcast_ref::<PersistenceFailure>().unwrap().phase(),
        PersistenceFailurePhase::BeforeBegin
    );
    Ok(())
}
