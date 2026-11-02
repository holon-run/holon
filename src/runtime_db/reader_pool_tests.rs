use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{RuntimeDb, RuntimeDbReadScope, RuntimeDbRetryableError};
// Only the protection-loss regression test below exercises these types, and
// it runs on Linux where the WAL sidecar can be removed underneath the pool.
#[cfg(target_os = "linux")]
use super::{RuntimeDbProtectionError, RuntimeDbProtectionState};

fn fixture(capacity: u32) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let directory = tempfile::tempdir()?;
    let mut db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    db.configure_reader_pool_for_tests(capacity, Duration::from_millis(100))?;
    Ok((directory, db))
}

fn read_one(db: &RuntimeDb) -> Result<i64> {
    db.with_read(|connection| Ok(connection.query_row("SELECT 1", [], |row| row.get(0))?))
}

#[tokio::test(start_paused = true)]
async fn advanced_runtime_clock_preserves_original_reader_acquisition_budget() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    tokio::time::advance(Duration::from_secs(60)).await;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_millis(100));
    let scoped = db.with_read_scope(scope);
    let value: i64 = scoped
        .with_read_async(|connection| Ok(connection.query_row("SELECT 1", [], |row| row.get(0))?))
        .await?;
    assert_eq!(value, 1);
    let expired = db.with_read_scope(RuntimeDbReadScope::new(Instant::now()));
    assert!(expired
        .with_read_async(|_| Ok(()))
        .await
        .unwrap_err()
        .is::<RuntimeDbRetryableError>());
    assert_eq!(read_one(&db)?, 1);
    Ok(())
}

#[test]
fn scoped_checkpoint_needs_no_reader_or_sql() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    db.check_read_scope()?;
    let expired = RuntimeDbReadScope::new(Instant::now());
    assert!(db
        .with_read_scope(expired)
        .check_read_scope()
        .unwrap_err()
        .is::<RuntimeDbRetryableError>());
    let cancelled = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    cancelled.cancel();
    assert!(db
        .with_read_scope(cancelled)
        .check_read_scope()
        .unwrap_err()
        .is::<RuntimeDbRetryableError>());
    assert_eq!(db.reader_pool_stats_for_tests().connect_attempts, 0);
    assert_eq!(read_one(&db)?, 1);
    Ok(())
}

// The current-thread runtime must keep driving cancellation/timers while an
// owned blocking worker uses Handle::block_on only to acquire a memory lock.
#[tokio::test(flavor = "current_thread")]
async fn scoped_memory_lock_deadline_and_cancel_allow_healthy_retry() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let lock = Arc::new(tokio::sync::Mutex::new(42));
    let held = lock.lock().await;
    for deadline in [true, false] {
        let scope = RuntimeDbReadScope::new(
            Instant::now()
                + if deadline {
                    Duration::from_millis(50)
                } else {
                    Duration::from_secs(30)
                },
        );
        let scoped = db.with_read_scope(scope.clone());
        let lock = lock.clone();
        let handle = tokio::runtime::Handle::current();
        let (started, running) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            handle.block_on(scoped.wait_for_memory_lock(lock.lock_owned()))
        });
        running.await?;
        if !deadline {
            scope.cancel();
        }
        let error = tokio::time::timeout(Duration::from_secs(5), worker)
            .await??
            .unwrap_err();
        assert!(error.is::<RuntimeDbRetryableError>(), "{error:#}");
    }
    assert_eq!(db.reader_pool_stats_for_tests().connect_attempts, 0);
    drop(held);
    let scoped = db.with_read_scope(RuntimeDbReadScope::new(
        Instant::now() + Duration::from_secs(30),
    ));
    assert_eq!(*scoped.wait_for_memory_lock(lock.lock()).await?, 42);
    assert_eq!(read_one(&db)?, 1);
    Ok(())
}

#[tokio::test]
async fn scoped_memory_lock_checks_cancel_after_acquisition() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let lock = tokio::sync::Mutex::new(42);
    let error = db
        .with_read_scope(scope.clone())
        .wait_for_memory_lock(async {
            scope.cancel();
            lock.lock().await
        })
        .await
        .unwrap_err();
    assert!(error.is::<RuntimeDbRetryableError>(), "{error:#}");
    assert!(
        lock.try_lock().is_ok(),
        "failed acquisition leaked its guard"
    );
    assert_eq!(db.reader_pool_stats_for_tests().connect_attempts, 0);
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn scoped_memory_lock_and_checkpoint_preserve_protection_priority_without_sql() -> Result<()>
{
    let (_directory, db) = fixture(1)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let lock = tokio::sync::Mutex::new(42);
    let mut wal = db.path().as_os_str().to_os_string();
    wal.push("-wal");
    let error = db
        .with_read_scope(scope.clone())
        .wait_for_memory_lock(async {
            std::fs::remove_file(std::path::PathBuf::from(wal)).unwrap();
            scope.cancel();
            lock.lock().await
        })
        .await
        .unwrap_err();
    assert!(error.is::<RuntimeDbProtectionError>(), "{error:#}");
    assert!(lock.try_lock().is_ok());
    let error = db.with_read_scope(scope).check_read_scope().unwrap_err();
    assert!(error.is::<RuntimeDbProtectionError>(), "{error:#}");
    assert_eq!(
        db.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    assert_eq!(db.reader_pool_stats_for_tests().connect_attempts, 0);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn scoped_assembly_checks_are_cheap_but_never_mask_lost_protection() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let scoped = db.with_read_scope(scope.clone());
    scoped.check_read_scope()?;
    let mut wal = db.path().as_os_str().to_os_string();
    wal.push("-wal");
    std::fs::remove_file(std::path::PathBuf::from(wal))?;
    // No filesystem protection probe on a healthy per-member checkpoint.
    db.check_read_cancellation()?;
    scoped.check_read_cancellation()?;
    assert_eq!(
        db.protection_status().state,
        RuntimeDbProtectionState::Protected
    );
    scope.cancel();
    let error = scoped.check_read_cancellation().unwrap_err();
    assert!(error.is::<RuntimeDbProtectionError>(), "{error:#}");
    assert_eq!(
        db.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    let error = db.check_read_scope().unwrap_err();
    assert!(error.is::<RuntimeDbProtectionError>(), "{error:#}");
    assert_eq!(db.reader_pool_stats_for_tests().connect_attempts, 0);
    Ok(())
}

// The first row proves the statement has started; the second step performs
// bounded but expensive recursive SQL, not a Rust sleep standing in for SQLite.
fn recursive_sql(
    connection: &mut rusqlite::Connection,
    started: impl FnOnce() -> Result<()>,
    interrupted: &AtomicBool,
) -> Result<()> {
    let mut statement = connection.prepare(
        "WITH RECURSIVE n(x) AS (
             VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < 100000000
         ) SELECT 0 UNION ALL SELECT sum(x) FROM n",
    )?;
    let mut rows = statement.query([])?;
    assert_eq!(rows.next()?.expect("first row").get::<_, i64>(0)?, 0);
    started()?;
    let result = rows.next();
    interrupted.store(
        matches!(
            &result,
            Err(rusqlite::Error::SqliteFailure(code, _))
                if code.code == rusqlite::ErrorCode::OperationInterrupted
        ),
        Ordering::SeqCst,
    );
    result?;
    Ok(())
}

#[test]
fn scoped_cancellation_interrupts_started_sql_and_reuses_capacity_one() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let scoped = db.with_read_scope(scope.clone());
    let interrupted = Arc::new(AtomicBool::new(false));
    let observed = interrupted.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        scoped.with_read(|connection| {
            recursive_sql(connection, || Ok(started_tx.send(())?), &observed)
        })
    });
    started_rx.recv_timeout(Duration::from_secs(5))?;
    scope.cancel();
    let error = worker.join().expect("read thread panicked").unwrap_err();
    assert!(error.is::<RuntimeDbRetryableError>());
    assert!(interrupted.load(Ordering::SeqCst), "{error:#}");
    assert_eq!(read_one(&db)?, 1);
    let stats = db.reader_pool_stats_for_tests();
    assert_eq!(stats.connections_opened, 1);
    assert_eq!(stats.connections_dropped, 0);
    Ok(())
}

#[test]
fn scoped_deadline_interrupts_sql_not_only_checkout() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    read_one(&db)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_millis(500));
    let interrupted = AtomicBool::new(false);
    let started = AtomicBool::new(false);
    let error = db
        .with_read_scope(scope)
        .with_read(|connection| {
            recursive_sql(
                connection,
                || {
                    started.store(true, Ordering::SeqCst);
                    Ok(())
                },
                &interrupted,
            )
        })
        .expect_err("execution must respect the explicit deadline");
    assert!(started.load(Ordering::SeqCst));
    assert!(interrupted.load(Ordering::SeqCst), "{error:#}");
    assert!(error.is::<RuntimeDbRetryableError>());
    assert_eq!(read_one(&db)?, 1);
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

#[test]
fn late_scope_cancellation_does_not_interrupt_the_next_lease() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let old = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    assert_eq!(read_one(&db.with_read_scope(old.clone()))?, 1);
    db.with_read(|connection| {
        let mut statement = connection.prepare(
            "WITH RECURSIVE n(x) AS (
                 VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < 10000
             ) SELECT 0 UNION ALL SELECT sum(x) FROM n",
        )?;
        let mut rows = statement.query([])?;
        assert!(rows.next()?.is_some());
        old.cancel();
        assert_eq!(rows.next()?.unwrap().get::<_, i64>(0)?, 50005000);
        Ok(())
    })?;
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

#[test]
fn scoped_checks_reject_expired_entry_and_cancelled_success_but_not_writers() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let expired = RuntimeDbReadScope::new(Instant::now());
    assert!(db
        .with_read_scope(expired.clone())
        .with_read::<()>(|_| panic!("expired scope cannot execute"))
        .unwrap_err()
        .is::<RuntimeDbRetryableError>());
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 0);
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let error = db
        .with_read_scope(scope.clone())
        .with_read(|_| {
            scope.cancel();
            Ok(42)
        })
        .expect_err("cancelled work cannot return successful values");
    assert!(error.is::<RuntimeDbRetryableError>());
    db.with_read_scope(expired)
        .connection()?
        .execute_batch("CREATE TABLE scope_writer_probe (id INTEGER)")?;
    assert_eq!(read_one(&db)?, 1);
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

// The off-worker read bridge owns a thread-local read deadline; nested pooled
// reads must inherit it and unscoped caller reads must not.
#[tokio::test]
async fn blocking_read_block_deadline_cancels_nested_reads_only() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let read_db = db.clone();
    let error = db
        .read_block("test.expired", Duration::from_millis(0), move || {
            read_db.with_read(|_| Ok(()))
        })
        .await
        .unwrap_err();
    assert!(error.is::<RuntimeDbRetryableError>(), "{error:#}");
    assert_eq!(read_one(&db)?, 1);
    Ok(())
}

#[tokio::test]
async fn dropping_scoped_read_interrupts_sql_without_cancelling_parent() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let scoped = db.with_read_scope(scope.clone());
    let worker_db = scoped.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(async move {
        worker_db
            .with_read_async(move |connection| {
                let interrupted = AtomicBool::new(false);
                let result = recursive_sql(
                    connection,
                    || {
                        let _ = started_tx.send(());
                        Ok(())
                    },
                    &interrupted,
                );
                let _ = finished_tx.send(interrupted.load(Ordering::SeqCst));
                result
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), started_rx).await??;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert!(tokio::time::timeout(Duration::from_secs(5), finished_rx).await??);
    assert!(!scope.is_cancelled());
    scoped.with_read_async(|_| Ok(())).await?;
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

#[tokio::test]
async fn dropped_scoped_future_retains_blocking_capacity_until_exit() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let scoped = db.with_read_scope(scope.clone());
    let worker_db = scoped.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = tokio::spawn(async move {
        worker_db
            .with_read_async(move |_| {
                let _ = started_tx.send(());
                release_rx.recv_timeout(Duration::from_secs(5))?;
                Ok(42)
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), started_rx).await??;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_eq!(db.reader_pool_stats_for_tests().available_admission, 0);
    let error = scoped.with_read_async(|_| Ok(())).await.unwrap_err();
    assert!(error.is::<RuntimeDbRetryableError>());
    assert_eq!(db.reader_pool_stats_for_tests().available_admission, 0);
    assert!(!scope.is_cancelled(), "single-future drop cancelled parent");
    release_tx.send(())?;
    tokio::time::timeout(
        Duration::from_secs(2),
        scoped.with_read_async(|connection| {
            Ok(connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
        }),
    )
    .await??;
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    assert_eq!(db.reader_pool_stats_for_tests().available_admission, 1);
    Ok(())
}

#[test]
fn scoped_panic_and_unfinished_transaction_discard_the_lease() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    read_one(&db)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let scoped = db.with_read_scope(scope.clone());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = scoped.with_read::<()>(|_| {
            scope.cancel();
            panic!("synthetic scoped reader panic")
        });
    }));
    assert!(panic.is_err());
    assert_eq!(read_one(&db)?, 1);
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 2);
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    assert!(db
        .with_read_scope(scope.clone())
        .with_read(|connection| {
            connection.execute_batch("BEGIN")?;
            scope.cancel();
            Ok(())
        })
        .is_err());
    assert_eq!(read_one(&db)?, 1);
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 3);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn scoped_cancellation_never_masks_protection_loss() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    read_one(&db)?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    let mut wal = db.path().as_os_str().to_os_string();
    wal.push("-wal");
    let error = db
        .with_read_scope(scope.clone())
        .with_read(|_| {
            std::fs::remove_file(std::path::PathBuf::from(&wal))?;
            scope.cancel();
            Ok(42)
        })
        .unwrap_err();
    assert!(error.is::<RuntimeDbProtectionError>(), "{error:#}");
    assert!(!error.is::<RuntimeDbRetryableError>());
    assert_eq!(
        db.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    Ok(())
}

#[test]
fn healthy_readers_reuse_one_connection_across_ten_thousand_calls_and_clones() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let clone = db.clone();
    for _ in 0..5_000 {
        assert_eq!(read_one(&db)?, 1);
        assert_eq!(read_one(&clone)?, 1);
    }
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

fn read_repository_bundle(db: &RuntimeDb) -> Result<()> {
    assert!(db.work_items().latest("missing-item")?.is_none());
    assert!(db.tasks().latest("missing-task")?.is_none());
    assert!(db.queue_entries().latest("missing-message")?.is_none());
    assert!(db.wait_conditions().for_turn("missing", "turn")?.is_empty());
    assert!(db.wait_conditions().active_for_agent("missing")?.is_empty());
    assert!(db
        .wait_conditions()
        .unresolved_for_agent("missing")?
        .is_empty());
    assert!(db.wait_conditions().active_all()?.is_empty());
    assert!(db
        .work_item_continuations()
        .active_for_agent("missing")?
        .is_empty());
    assert!(db
        .transitions()
        .execution_attempt_by_id("missing", "attempt")?
        .is_none());
    assert!(db
        .transitions()
        .load_execution_protocol_state_if_initialized("missing")?
        .is_none());
    let settlements = db.task_result_settlements();
    assert!(settlements
        .admitted_for_activation("missing", "activation")?
        .is_empty());
    assert!(settlements
        .due_deferred("missing", chrono::Utc::now(), 10)?
        .is_empty());
    assert!(settlements.unsettled_for_agent("missing", 10)?.is_empty());
    assert!(settlements
        .unsettled_for_owner("missing", None, 10)?
        .is_empty());
    assert!(settlements.next_recheck_at("missing")?.is_none());
    assert!(settlements.latest_for_message("missing")?.is_none());
    let outbox = db.runtime_index_outbox();
    assert_eq!(outbox.produced_watermark_for_agent("missing")?, 0);
    assert_eq!(outbox.pending_count_for_agent("missing", 0)?, 0);
    assert!(outbox.read_after("missing", 0, 10)?.is_empty());
    assert!(db
        .authentication()
        .find_user("issuer", "subject")?
        .is_none());
    assert!(db.agent_message_deliveries().latest("missing")?.is_none());
    assert!(db.agent_canonical_relations().latest("missing")?.is_none());
    assert!(db
        .conversation()
        .brief_snapshot("missing", "brief")?
        .is_none());
    assert!(db.subagent_cleanup_candidates("missing", 10)?.is_empty());
    assert!(!db.runtime_id()?.is_empty());
    assert!(!db.event_log_epoch()?.is_empty());
    db.visibility_policy_generation()?;
    db.provider_quota_incident("missing")?;
    db.agent_roster_snapshot_rows()?;
    db.agent_projection_snapshot_rows("missing")?;
    db.agent_event_recovery_window(Some("missing"))?;
    Ok(())
}

#[test]
fn migrated_repositories_share_capacity_one_and_release_snapshots() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    for _ in 0..20 {
        read_repository_bundle(&db)?;
    }
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    let item = crate::types::WorkItemRecord::new(
        "pool-agent",
        "committed after warm reads",
        crate::types::WorkItemState::Open,
    );
    db.work_items().insert_new(&item)?;
    assert_eq!(db.work_items().latest(&item.id)?, Some(item));
    read_repository_bundle(&db)?;
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

#[test]
fn migrated_repository_saturation_never_becomes_missing_or_success() -> Result<()> {
    let (_directory, mut db) = fixture(1)?;
    db.configure_reader_pool_for_tests(1, Duration::from_millis(25))?;
    let worker_db = db.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_db.with_read(|_| {
            started_tx.send(())?;
            release_rx.recv_timeout(Duration::from_secs(10))?;
            Ok(())
        })
    });
    started_rx.recv_timeout(Duration::from_secs(5))?;
    let reads = [
        ("work item", db.work_items().latest("missing").map(drop)),
        ("task", db.tasks().latest("missing").map(drop)),
        ("queue", db.queue_entries().latest("missing").map(drop)),
        (
            "wait",
            db.wait_conditions().active_for_agent("missing").map(drop),
        ),
        ("all waits", db.wait_conditions().active_all().map(drop)),
        (
            "protocol",
            db.transitions()
                .load_execution_protocol_state_if_initialized("missing")
                .map(drop),
        ),
        (
            "outbox",
            db.runtime_index_outbox()
                .produced_watermark_for_agent("missing")
                .map(drop),
        ),
        (
            "outbox ack precheck",
            db.runtime_index_outbox()
                .delete_acknowledged_through("missing", 0)
                .map(drop),
        ),
        (
            "authentication",
            db.authentication().find_user("issuer", "subject").map(drop),
        ),
        (
            "conversation",
            db.conversation()
                .brief_snapshot("missing", "brief")
                .map(drop),
        ),
        (
            "relations",
            db.agent_canonical_relations().latest("missing").map(drop),
        ),
        (
            "cleanup",
            db.subagent_cleanup_candidates("missing", 10).map(drop),
        ),
        ("roster", db.agent_roster_snapshot_rows().map(drop)),
        (
            "projection",
            db.agent_projection_snapshot_rows("missing").map(drop),
        ),
        ("metadata", db.runtime_id().map(drop)),
    ];
    // Named authority preparation has its own FIFO writer read, not a pooled
    // repository fallback. It must still reject missing canonical authority.
    let authority_error = db
        .transitions()
        .load_execution_authority_fences("missing")
        .unwrap_err();
    assert_eq!(
        authority_error.to_string(),
        "execution admission requires durable agent control state"
    );
    release_tx.send(())?;
    worker.join().expect("reader thread panicked")?;
    for (name, result) in reads {
        let error = result.expect_err(name);
        assert!(
            error.downcast_ref::<RuntimeDbRetryableError>().is_some(),
            "{name}: {error:#}"
        );
    }
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    read_repository_bundle(&db)?;
    Ok(())
}

#[tokio::test]
async fn outbox_async_discovery_reuses_readers_and_propagates_saturation() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let item = crate::types::WorkItemRecord::new(
        "pool-agent",
        "outbox discovery",
        crate::types::WorkItemState::Open,
    );
    let change = super::index_outbox::RuntimeIndexChange {
        agent_id: item.agent_id.clone(),
        source_kind: "work_item".into(),
        source_id: item.id.clone(),
        source_ref: format!("work_item:{}", item.id),
        operation: super::index_outbox::RuntimeIndexOperation::Upsert,
        source_updated_at: Some(item.updated_at),
        reason: "reader_pool_test".into(),
    };
    assert!(db
        .work_items()
        .insert_new_with_index_changes(&item, &[change])?);
    let pending = db.runtime_index_outbox().agent_ids_with_pending()?;
    let watermarks = db.runtime_index_outbox().agent_ids_with_watermarks()?;
    assert_eq!(pending, vec!["pool-agent".to_string()]);
    assert_eq!(watermarks, pending);
    for _ in 0..10 {
        assert_eq!(
            db.runtime_index_outbox()
                .agent_ids_with_pending_async()
                .await?,
            pending
        );
        assert_eq!(
            db.runtime_index_outbox()
                .agent_ids_with_watermarks_async()
                .await?,
            watermarks
        );
    }
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    let worker_db = db.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_db.with_read(|_| {
            started_tx.send(())?;
            release_rx.recv_timeout(Duration::from_secs(10))?;
            Ok(())
        })
    });
    started_rx.recv_timeout(Duration::from_secs(5))?;
    let pending = db
        .runtime_index_outbox()
        .agent_ids_with_pending_async()
        .await;
    let watermarks = db
        .runtime_index_outbox()
        .agent_ids_with_watermarks_async()
        .await;
    release_tx.send(())?;
    worker.join().expect("reader thread panicked")?;
    for result in [pending, watermarks] {
        assert!(result
            .expect_err("saturation must not become an empty agent set")
            .downcast_ref::<RuntimeDbRetryableError>()
            .is_some());
    }
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

#[test]
fn last_facade_releases_idle_readers_and_writer_protection() -> Result<()> {
    let (directory, db) = fixture(1)?;
    let clone = db.clone();
    let writer = db.reader_pool_writer_weak_for_tests();
    assert_eq!(read_one(&db)?, 1);
    drop(db);
    assert!(writer.upgrade().is_some());
    assert_eq!(read_one(&clone)?, 1);
    assert_eq!(clone.reader_pool_stats_for_tests().connections_opened, 1);
    drop(clone);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while writer.upgrade().is_some() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(writer.upgrade().is_none(), "last facade retained writer");
    let reopened = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    assert_eq!(read_one(&reopened)?, 1);
    Ok(())
}

#[test]
fn query_errors_reuse_reader_and_next_transaction_observes_new_commit() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    db.transaction(|transaction| {
        transaction.execute_batch(
            "CREATE TABLE reader_values(value INTEGER);
             INSERT INTO reader_values VALUES (1);",
        )?;
        Ok(())
    })?;
    let read_value = || {
        db.with_read(|connection| {
            assert!(connection.is_autocommit());
            let transaction = connection.transaction()?;
            let value: i64 =
                transaction.query_row("SELECT value FROM reader_values", [], |row| row.get(0))?;
            transaction.commit()?;
            Ok(value)
        })
    };
    assert_eq!(read_value()?, 1);
    assert!(db
        .with_read(|connection| {
            connection.execute("UPDATE reader_values SET value = 99", [])?;
            Ok(())
        })
        .is_err());
    assert!(db
        .with_read(|connection| {
            connection.query_row("SELECT missing_column FROM reader_values", [], |_| Ok(()))?;
            Ok(())
        })
        .is_err());
    db.transaction(|transaction| {
        transaction.execute("UPDATE reader_values SET value = 2", [])?;
        Ok(())
    })?;
    assert_eq!(read_value()?, 2);
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

#[test]
fn panic_and_unfinished_transaction_discard_connection() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    read_one(&db)?;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = db.with_read::<()>(|_| panic!("synthetic reader panic"));
    }));
    assert!(panic.is_err());
    read_one(&db)?;
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 2);

    // Deliberately violate the internal closure contract. The next request
    // must not inherit this snapshot even if the closure reports success.
    let _ = db.with_read(|connection| {
        connection.execute_batch("BEGIN")?;
        Ok(())
    });
    db.with_read(|connection| {
        assert!(connection.is_autocommit());
        Ok(())
    })?;
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 3);
    Ok(())
}

#[test]
fn saturated_pool_times_out_without_overflow_connection() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let worker_db = db.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_db.with_read(|_| {
            started_tx.send(())?;
            release_rx.recv_timeout(Duration::from_secs(5))?;
            Ok(())
        })
    });
    started_rx.recv_timeout(Duration::from_secs(5))?;
    let error = read_one(&db).expect_err("a checked-out reader must not cause overflow");
    release_tx.send(())?;
    worker.join().expect("reader thread panicked")?;
    assert!(
        format!("{error:#}").contains("tim"),
        "expected observable acquisition timeout, got {error:#}"
    );
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    assert_eq!(read_one(&db)?, 1);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_retains_started_work_and_removes_unstarted_waiter() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_db = db.clone();
    let running = tokio::spawn(async move {
        worker_db
            .with_read_async(move |_| {
                let _ = started_tx.send(());
                release_rx.recv_timeout(Duration::from_secs(5))?;
                Ok(())
            })
            .await
    });
    started_rx.await?;

    let was_started = Arc::new(AtomicBool::new(false));
    let started = was_started.clone();
    let waiting_db = db.clone();
    let waiting = tokio::spawn(async move {
        waiting_db
            .with_read_async(move |_| {
                started.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
    });
    tokio::task::yield_now().await;
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());

    // On a single Tokio worker this also proves pool admission does not
    // synchronously block the worker that must poll the timeout.
    let timeout =
        tokio::time::timeout(Duration::from_secs(2), db.with_read_async(|_| Ok(()))).await?;
    assert!(
        timeout.is_err(),
        "aborted caller must not free an active lease"
    );
    release_tx.send(())?;
    tokio::time::timeout(Duration::from_secs(2), db.with_read_async(|_| Ok(()))).await??;
    assert!(!was_started.load(Ordering::SeqCst));
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}

#[test]
fn blocking_queue_wait_obeys_acquisition_budget_without_running_read_closure() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            release_rx.recv_timeout(Duration::from_secs(5))
        });
        started_rx.await?;
        let was_started = Arc::new(AtomicBool::new(false));
        let started = Arc::clone(&was_started);
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            db.with_read_async(move |_| {
                started.store(true, Ordering::SeqCst);
                Ok(())
            }),
        )
        .await?
        .expect_err("queued read must exhaust its acquisition budget");
        assert!(error.downcast_ref::<RuntimeDbRetryableError>().is_some());
        assert_eq!(db.reader_pool_stats_for_tests().checkout_timeouts, 1);
        assert!(!was_started.load(Ordering::SeqCst));
        release_tx.send(())?;
        blocker.await??;
        db.with_read_async(|_| Ok(())).await?;
        assert!(!was_started.load(Ordering::SeqCst));
        assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
        Ok::<_, anyhow::Error>(())
    })
}

#[tokio::test(flavor = "current_thread")]
async fn started_read_can_outlive_acquisition_budget() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_db = db.clone();
    let running = tokio::spawn(async move {
        worker_db
            .with_read_async(move |_| {
                let _ = started_tx.send(());
                release_rx.recv_timeout(Duration::from_secs(5))?;
                Ok(())
            })
            .await
    });
    started_rx.await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!running.is_finished());
    release_tx.send(())?;
    running.await??;
    assert_eq!(db.reader_pool_stats_for_tests().checkout_timeouts, 0);
    Ok(())
}

#[test]
fn cancellation_before_blocking_task_start_never_runs_read_closure() -> Result<()> {
    use std::future::Future;
    use std::task::Poll;

    let (_directory, db) = fixture(1)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            release_rx.recv_timeout(Duration::from_secs(5))
        });
        started_rx.await?;
        let was_started = Arc::new(AtomicBool::new(false));
        let started = Arc::clone(&was_started);
        {
            let mut queued = std::pin::pin!(db.with_read_async(move |_| {
                started.store(true, Ordering::SeqCst);
                Ok(())
            }));
            // Poll through admission into the occupied blocking queue, then
            // drop the future before the sole blocking worker is released.
            std::future::poll_fn(|context| {
                assert!(queued.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        release_tx.send(())?;
        blocker.await??;
        db.with_read_async(|_| Ok(())).await?;
        assert!(!was_started.load(Ordering::SeqCst));
        assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
        Ok::<_, anyhow::Error>(())
    })
}

#[cfg(target_os = "linux")]
#[test]
fn protection_loss_rejects_inflight_result_and_all_future_read_closures() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    read_one(&db)?;
    let mut wal = db.path().as_os_str().to_os_string();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    let error = db
        .with_read(|_| {
            std::fs::remove_file(&wal)?;
            Ok(42)
        })
        .expect_err("a result produced after protection loss must not escape");
    assert!(error
        .chain()
        .any(|source| source.is::<RuntimeDbProtectionError>()));
    assert_eq!(
        db.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    let first_status = db.protection_status();
    assert!(db
        .with_read::<()>(|_| panic!("quarantined pool must not run business closures"))
        .is_err());
    assert_eq!(db.protection_status(), first_status);
    assert!(
        !wal.exists(),
        "reader reuse must never recreate lost sidecars"
    );
    Ok(())
}

#[tokio::test]
async fn list_sync_and_async_share_committed_fact_contract() -> Result<()> {
    let (_directory, db) = fixture(1)?;
    db.agent_states()
        .upsert_for_fixture(&crate::types::AgentState::new("pool-agent"))?;
    let ids = vec!["pool-agent".to_owned(), "absent-agent".to_owned()];
    let synchronous = db.agent_list_posture_rows(&ids)?;
    let asynchronous = db.agent_list_posture_rows_async(ids).await?;
    assert_eq!(synchronous.rows.len(), asynchronous.rows.len());
    for (left, right) in synchronous.rows.iter().zip(&asynchronous.rows) {
        assert_eq!(left.agent_id, right.agent_id);
        assert_eq!(left.agent_state_json, right.agent_state_json);
        assert_eq!(
            serde_json::to_value(left.facts.as_ref().unwrap().open_work_items.clone())?,
            serde_json::to_value(right.facts.as_ref().unwrap().open_work_items.clone())?
        );
    }
    assert!(db.agent_list_posture_rows(&[])?.rows.is_empty());
    assert!(db
        .agent_list_posture_rows_async(Vec::new())
        .await?
        .rows
        .is_empty());
    assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
    Ok(())
}
