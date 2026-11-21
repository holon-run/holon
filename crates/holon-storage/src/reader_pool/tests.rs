use std::sync::mpsc;

use super::*;
use crate::diagnostics::{StorageDiagnostics, NO_DIAGNOSTICS};
use crate::write_queue::RuntimeDbWriter;
use crate::RuntimeDbProtectionState;

struct ReaderFixture {
    path: std::path::PathBuf,
    readers: RuntimeDbReaderPool,
    writer: RuntimeDbWriter,
}

impl ReaderFixture {
    fn with_read<T>(&self, read: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        self.readers.with_read(read)
    }
}

fn fixture() -> Result<(tempfile::TempDir, ReaderFixture)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let connection = crate::connection::open_connection(&path, &NO_DIAGNOSTICS)?;
    crate::connection::configure_persistent_database(&connection)?;
    // Commit once so SQLite creates the WAL and shared-memory sidecars.
    connection.execute_batch("CREATE TABLE reader_fixture (value INTEGER)")?;
    assert!(
        directory.path().join("runtime.sqlite-wal").is_file(),
        "reader fixture needs a live WAL file"
    );
    let writer = RuntimeDbWriter::open(path.clone(), connection)?;
    let readers =
        RuntimeDbReaderPool::with_config(Arc::clone(writer.state()), 1, Duration::from_secs(2));
    Ok((
        directory,
        ReaderFixture {
            path,
            readers,
            writer,
        },
    ))
}

fn install_hook(
    pool: &RuntimeDbReaderPool,
    hook: impl Fn(ReaderTestEvent, &RuntimeDbWriterState) -> Result<()> + Send + Sync + 'static,
) {
    *pool.shared.control.test_hook.lock().unwrap() = Some(Arc::new(hook));
}

#[test]
fn sql_configuration_is_denied_and_detected_attachments_are_discarded() -> Result<()> {
    let (_directory, db) = fixture()?;
    for sql in [
        "PRAGMA query_only = OFF",
        "PRAGMA foreign_keys = OFF",
        "PRAGMA cache_size = 123",
        "ATTACH DATABASE ':memory:' AS extra",
    ] {
        db.with_read(|connection| {
            assert!(connection.execute_batch(sql).is_err(), "{sql}");
            Ok(())
        })?;
    }
    assert_eq!(db.readers.stats().connections_opened, 1);

    let error = db
        .with_read(|connection| {
            // Deliberately violate the Rust API contract to exercise the
            // independent attachment check rather than the SQL authorizer.
            connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            connection.execute_batch("ATTACH DATABASE ':memory:' AS extra")?;
            connection.authorizer(Some(read_authorizer));
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("dirty state"));
    assert_eq!(db.readers.stats().connections_dropped, 1);
    db.with_read(|_| Ok(()))?;
    assert_eq!(db.readers.stats().connections_opened, 2);
    Ok(())
}

#[cfg(unix)]
#[test]
fn removed_or_replaced_database_quarantines_idle_readers_without_running_closure() -> Result<()> {
    for replace in [false, true] {
        let (directory, db) = fixture()?;
        let pool = &db.readers;
        pool.with_read(|_| Ok(()))?;
        let attempts = pool.stats().connect_attempts;
        if replace {
            let replacement = directory.path().join("replacement.sqlite");
            std::fs::write(&replacement, b"replacement database evidence")?;
            std::fs::rename(replacement, &db.path)?;
        } else {
            std::fs::remove_file(&db.path)?;
        }

        let error = pool
            .with_read(|_| -> Result<()> { panic!("must not run after DB identity loss") })
            .unwrap_err();
        assert!(error.downcast_ref::<RuntimeDbProtectionError>().is_some());
        let evidence = error.to_string();
        assert_eq!(
            db.writer.protection_status().state,
            RuntimeDbProtectionState::Quarantined
        );
        assert_eq!(pool.stats().connections_dropped, 1);
        assert_eq!(pool.stats().idle_connections, 0);
        assert_eq!(pool.stats().current_connections, 0);
        assert_eq!(
            db.writer.open_connection().err().unwrap().to_string(),
            evidence
        );
        assert_eq!(
            pool.with_read(|_| Ok(())).unwrap_err().to_string(),
            evidence
        );
        assert_eq!(pool.stats().connect_attempts, attempts);
        if replace {
            assert_eq!(std::fs::read(&db.path)?, b"replacement database evidence");
        } else {
            assert!(!db.path.exists(), "must not recreate a removed DB");
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn replaced_database_rejects_in_flight_result_and_future_closures() -> Result<()> {
    let (directory, db) = fixture()?;
    let replacement = directory.path().join("replacement.sqlite");
    std::fs::write(&replacement, b"replacement database evidence")?;
    let error = db
        .with_read(|connection| {
            let value = connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
            std::fs::rename(&replacement, &db.path)?;
            Ok(value)
        })
        .unwrap_err();
    assert!(error.downcast_ref::<RuntimeDbProtectionError>().is_some());
    assert_eq!(db.readers.stats().connections_dropped, 1);
    assert_eq!(
        db.readers
            .with_read(|_| -> Result<()> { panic!("must not reuse an old DB descriptor") })
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
    assert_eq!(std::fs::read(&db.path)?, b"replacement database evidence");
    Ok(())
}

#[cfg(unix)]
#[test]
fn writer_identity_binding_rejects_an_already_replaced_open_handle() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let connection = crate::connection::open_connection(&path, &NO_DIAGNOSTICS)?;
    let replacement = directory.path().join("replacement.sqlite");
    std::fs::write(&replacement, b"replacement database evidence")?;
    std::fs::rename(&replacement, &path)?;
    let error =
        crate::write_queue::RuntimeDbWriter::open_starting(path, connection, &NO_DIAGNOSTICS)
            .err()
            .expect("must bind to the existing descriptor, not just stat the new path");
    assert!(error.downcast_ref::<RuntimeDbProtectionError>().is_some());
    assert!(error.to_string().contains("moved=1"));
    Ok(())
}

#[test]
fn manager_failures_back_off_without_running_or_replaying_business_sql() -> Result<()> {
    let (_directory, db) = fixture()?;
    let pool = &db.readers;
    let reject = Arc::new(AtomicBool::new(true));
    let failures = Arc::clone(&reject);
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    install_hook(pool, move |event, _writer| {
        if event == ReaderTestEvent::BeforeOpen {
            let reject = failures.load(Ordering::Acquire);
            attempt_tx.send(Instant::now())?;
            if reject {
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))?;
                bail!("synthetic reader creation failure");
            }
        }
        Ok(())
    });
    let calls = AtomicU64::new(0);
    for _ in 0..3 {
        let error = pool
            .read_until(
                Some(Instant::now() + Duration::from_millis(30)),
                None,
                read_block::ReadOrigin::UNKNOWN,
                |_| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.downcast_ref::<RuntimeDbRetryableError>().is_some());
    }
    let first = attempt_rx.recv_timeout(Duration::from_secs(2))?;
    release_tx.send(())?;
    let second = attempt_rx.recv_timeout(Duration::from_secs(2))?;
    release_tx.send(())?;
    let third = attempt_rx.recv_timeout(Duration::from_secs(2))?;
    assert!(second.duration_since(first) >= Duration::from_millis(300));
    assert!(third.duration_since(second) >= Duration::from_millis(600));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let failed = pool.stats();
    assert_eq!(failed.connect_attempts, 3);
    assert_eq!(failed.connect_failures, 2);
    assert_eq!(failed.connections_opened, 0);
    assert_eq!(failed.connections_dropped, 0);
    assert_eq!(failed.current_connections, 0);
    assert_eq!(failed.idle_connections, 0);
    assert_eq!(failed.checkout_timeouts, 3);

    // The hook snapshots rejection before notifying us, so all three attempts fail.
    reject.store(false, Ordering::Release);
    release_tx.send(())?;
    assert_eq!(
        pool.read_until(
            Some(Instant::now() + Duration::from_secs(4)),
            None,
            read_block::ReadOrigin::UNKNOWN,
            |connection| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
            }
        )?,
        1
    );
    let stats = pool.stats();
    assert_eq!(stats.connect_attempts, 4);
    assert_eq!(stats.connect_failures, 3);
    assert_eq!(stats.connections_opened, 1);
    assert_eq!(stats.connections_dropped, 0);
    assert_eq!(stats.current_connections, 1);
    assert_eq!(stats.idle_connections, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_ne!(
        db.writer.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    Ok(())
}

#[test]
fn closing_during_connect_discards_reader_without_running_business_sql() -> Result<()> {
    let (_directory, db) = fixture()?;
    let pool = &db.readers;
    let (opened_tx, opened_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let (closed_tx, closed_rx) = mpsc::channel();
    install_hook(pool, move |event, writer| {
        match event {
            ReaderTestEvent::Opened => {
                opened_tx.send(())?;
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))?;
            }
            ReaderTestEvent::Closed => {
                closed_tx.send(writer.ensure_protected().is_ok())?;
            }
            ReaderTestEvent::BeforeOpen => {}
        }
        Ok(())
    });
    let manager = ReaderManager {
        control: Arc::clone(&pool.shared.control),
    };
    let connect = std::thread::spawn(move || manager.connect());
    opened_rx.recv_timeout(Duration::from_secs(2))?;
    pool.shared.control.close();
    assert!(pool.shared.admission.is_closed());
    release_tx.send(())?;
    let error = connect
        .join()
        .map_err(|_| anyhow!("reader connect thread panicked"))?
        .err()
        .expect("a reader completed after pool closure must be rejected");
    assert_eq!(error.to_string(), "runtime db reader pool is closed");
    assert!(closed_rx.recv_timeout(Duration::from_secs(2))?);

    let calls = AtomicU64::new(0);
    assert!(pool
        .with_read(|_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let stats = pool.stats();
    assert_eq!(stats.connect_attempts, 1);
    assert_eq!(stats.connect_failures, 1);
    assert_eq!(stats.connections_opened, 1);
    assert_eq!(stats.connections_dropped, 1);
    assert_eq!(stats.current_connections, 0);
    assert_eq!(stats.idle_connections, 0);
    assert_ne!(
        db.writer.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    Ok(())
}

#[test]
fn dropping_last_facade_during_connect_closes_reader_before_protection_owner() -> Result<()> {
    let (_directory, db) = fixture()?;
    #[cfg(target_os = "linux")]
    let guard_probe = std::fs::File::open(&db.path)?;
    #[cfg(target_os = "linux")]
    let guard_is_held = || -> Result<bool> {
        use std::os::fd::AsRawFd;

        let mut lock = libc::flock {
            l_type: libc::F_WRLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: 0x4000_0000 + 2,
            l_len: 510,
            l_pid: 0,
        };
        // SAFETY: this query borrows a live descriptor and a valid writable flock.
        let result = unsafe { libc::fcntl(guard_probe.as_raw_fd(), libc::F_OFD_GETLK, &mut lock) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(lock.l_type != libc::F_UNLCK as libc::c_short)
    };
    let pool = db.readers.clone();
    let writer = pool.writer_weak();
    let (opened_tx, opened_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let (closed_tx, closed_rx) = mpsc::channel();
    install_hook(&pool, move |event, writer| {
        match event {
            ReaderTestEvent::Opened => {
                opened_tx.send(())?;
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))?;
            }
            ReaderTestEvent::Closed => {
                let protected = writer.ensure_protected().is_ok();
                #[cfg(target_os = "linux")]
                let protected = protected && writer.sidecar_guard.lock().unwrap().is_some();
                closed_tx.send(protected)?;
            }
            ReaderTestEvent::BeforeOpen => {}
        }
        Ok(())
    });

    // Checkout times out, but an already-started pool connect still owns protection.
    assert!(pool
        .read_until(
            Some(Instant::now() + Duration::from_millis(100)),
            None,
            read_block::ReadOrigin::UNKNOWN,
            |_| Ok(())
        )
        .is_err());
    opened_rx.recv_timeout(Duration::from_secs(2))?;
    drop(db);
    drop(pool);
    assert!(writer.strong_count() > 0);
    #[cfg(target_os = "linux")]
    assert!(guard_is_held()?, "in-flight connect lost sidecar guard");
    release_tx.send(())?;
    assert!(closed_rx.recv_timeout(Duration::from_secs(2))?);

    let deadline = Instant::now() + Duration::from_secs(2);
    while writer.strong_count() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        writer.strong_count(),
        0,
        "background connect retained writer"
    );
    #[cfg(target_os = "linux")]
    {
        while guard_is_held()? && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!guard_is_held()?, "last owner retained sidecar guard");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn protection_loss_during_connect_discards_reader_and_preserves_evidence() -> Result<()> {
    let (_directory, db) = fixture()?;
    let pool = &db.readers;
    let mut wal = db.path.as_os_str().to_os_string();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    let removed = wal.clone();
    install_hook(pool, move |event, _writer| {
        if event == ReaderTestEvent::Opened {
            std::fs::remove_file(&removed)?;
        }
        Ok(())
    });
    let manager = ReaderManager {
        control: Arc::clone(&pool.shared.control),
    };
    let error = manager
        .connect()
        .err()
        .expect("manager must reject a reader that lost protection");
    let evidence = error
        .0
        .downcast_ref::<RuntimeDbProtectionError>()
        .expect("manager must preserve the original protection error")
        .to_string();
    let stats = pool.stats();
    assert_eq!(stats.connect_attempts, 1);
    assert_eq!(stats.connect_failures, 1);
    assert_eq!(stats.connections_opened, 1);
    assert_eq!(stats.connections_dropped, 1);
    assert_eq!(stats.current_connections, 0);
    assert_eq!(stats.idle_connections, 0);
    assert_eq!(
        db.writer.protection_status().state,
        RuntimeDbProtectionState::Quarantined
    );
    assert_eq!(
        pool.with_read(|_| Ok(())).unwrap_err().to_string(),
        evidence
    );
    assert_eq!(pool.stats().connect_attempts, stats.connect_attempts);
    assert!(!wal.exists(), "pool must not recreate quarantined sidecars");
    Ok(())
}

static RECORDED: Mutex<Vec<(ReaderPoolRole, ReaderPoolEvent)>> = Mutex::new(Vec::new());
static RECORDING_LOCK: Mutex<()> = Mutex::new(());

fn record_reader_pool(role: ReaderPoolRole, event: ReaderPoolEvent) {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((role, event));
}

fn recording_diagnostics() -> &'static StorageDiagnostics {
    static RECORDING: StorageDiagnostics = StorageDiagnostics {
        record_open_attempt: || {},
        observe_connection: |_| Ok(()),
        record_open_duration: |_| {},
        record_sidecar_scan: |_| {},
        record_stage: |_, _| {},
        reader_pool: record_reader_pool,
    };
    &RECORDING
}

fn diagnostics_guard() -> std::sync::MutexGuard<'static, ()> {
    RECORDING_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn clear_recorded() {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

fn recorded_events() -> Vec<(ReaderPoolRole, ReaderPoolEvent)> {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn recorded_checkout_outcomes() -> Vec<ReaderPoolOutcome> {
    recorded_events()
        .iter()
        .filter_map(|(_, event)| match event {
            ReaderPoolEvent::CheckoutFinished { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .collect()
}

fn recording_fixture(
    capacity: u32,
    budget: Duration,
) -> Result<(tempfile::TempDir, ReaderFixture)> {
    let diagnostics = recording_diagnostics();
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let connection = crate::connection::open_connection(&path, diagnostics)?;
    crate::connection::configure_persistent_database(&connection)?;
    connection.execute_batch("CREATE TABLE reader_fixture (value INTEGER)")?;
    let writer = RuntimeDbWriter::open_starting(path.clone(), connection, diagnostics)?;
    let readers = RuntimeDbReaderPool::with_config(Arc::clone(writer.state()), capacity, budget);
    Ok((
        directory,
        ReaderFixture {
            path,
            readers,
            writer,
        },
    ))
}

#[test]
fn reader_pool_diagnostics_record_lifecycle_and_success() -> Result<()> {
    let _guard = diagnostics_guard();
    clear_recorded();
    let (_directory, db) = recording_fixture(1, Duration::from_secs(2))?;
    assert_eq!(
        recorded_events(),
        vec![(
            ReaderPoolRole::Ordinary,
            ReaderPoolEvent::Created { capacity: 1 }
        )]
    );
    db.with_read(|connection| {
        connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
        Ok(())
    })?;
    let events = recorded_events();
    assert!(events.contains(&(ReaderPoolRole::Ordinary, ReaderPoolEvent::CheckoutStarted)));
    assert!(events
        .iter()
        .any(|(role, event)| *role == ReaderPoolRole::Ordinary
            && matches!(
                event,
                ReaderPoolEvent::CheckoutFinished {
                    outcome: ReaderPoolOutcome::Success,
                    ..
                }
            )));
    assert!(events.contains(&(ReaderPoolRole::Ordinary, ReaderPoolEvent::ConnectStarted)));
    assert!(events
        .iter()
        .any(|(role, event)| *role == ReaderPoolRole::Ordinary
            && matches!(
                event,
                ReaderPoolEvent::ConnectFinished { success: true, .. }
            )));
    assert!(events.contains(&(ReaderPoolRole::Ordinary, ReaderPoolEvent::ConnectionOpened)));
    assert!(events.contains(&(ReaderPoolRole::Ordinary, ReaderPoolEvent::LeaseStarted)));
    assert!(events
        .iter()
        .any(|(role, event)| *role == ReaderPoolRole::Ordinary
            && matches!(event, ReaderPoolEvent::LeaseFinished { .. })));

    drop(db);
    let mut events = recorded_events();
    assert!(events.contains(&(
        ReaderPoolRole::Ordinary,
        ReaderPoolEvent::Dropped { capacity: 1 }
    )));

    // A background connect can outlive the pool and close its connection later.
    let closed = (ReaderPoolRole::Ordinary, ReaderPoolEvent::ConnectionClosed);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !events.contains(&closed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        events = recorded_events();
    }
    assert!(
        events.contains(&closed),
        "missing ConnectionClosed after dropping the reader pool: {events:?}"
    );
    Ok(())
}

#[test]
fn reader_pool_diagnostics_finish_lease_after_connection_close() -> Result<()> {
    let _guard = diagnostics_guard();
    clear_recorded();
    let (_directory, db) = recording_fixture(1, Duration::from_secs(2))?;
    // Deliberately violate the Rust API contract to force discard on return.
    let error = db
        .with_read(|connection| {
            connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            connection.execute_batch("PRAGMA query_only = OFF")?;
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("dirty state"));
    let events = recorded_events();
    let closed = events
        .iter()
        .position(|(_, event)| matches!(event, ReaderPoolEvent::ConnectionClosed))
        .expect("discarded connection closes");
    let finished = events
        .iter()
        .position(|(_, event)| matches!(event, ReaderPoolEvent::LeaseFinished { .. }))
        .expect("lease finishes");
    assert!(
        closed < finished,
        "lease must include connection return/close"
    );
    Ok(())
}

#[test]
fn reader_pool_diagnostics_separate_connect_failure_from_checkout_timeout() -> Result<()> {
    let _guard = diagnostics_guard();
    clear_recorded();
    let (_directory, db) = recording_fixture(1, Duration::from_millis(30))?;
    install_hook(&db.readers, |event, _writer| {
        if event == ReaderTestEvent::BeforeOpen {
            bail!("synthetic reader creation failure");
        }
        Ok(())
    });
    let error = db.readers.with_read(|_| Ok(())).unwrap_err();
    assert!(error.downcast_ref::<RuntimeDbRetryableError>().is_some());
    let events = recorded_events();
    assert!(events
        .iter()
        .any(|(role, event)| *role == ReaderPoolRole::Ordinary
            && matches!(
                event,
                ReaderPoolEvent::CheckoutFinished {
                    outcome: ReaderPoolOutcome::Timeout,
                    ..
                }
            )));
    assert!(events.contains(&(ReaderPoolRole::Ordinary, ReaderPoolEvent::ConnectStarted)));
    assert!(events
        .iter()
        .any(|(role, event)| *role == ReaderPoolRole::Ordinary
            && matches!(
                event,
                ReaderPoolEvent::ConnectFinished { success: false, .. }
            )));
    assert!(!events.contains(&(ReaderPoolRole::Ordinary, ReaderPoolEvent::ConnectionOpened)));
    Ok(())
}

#[test]
fn reader_pool_diagnostics_checkout_lives_until_last_owner() {
    let _guard = diagnostics_guard();
    for outcome in [ReaderPoolOutcome::Success, ReaderPoolOutcome::Failed] {
        clear_recorded();
        let caller = CheckoutObservation::start(recording_diagnostics(), ReaderPoolRole::Ordinary);
        let worker = caller.clone();
        drop(caller);
        assert!(recorded_checkout_outcomes().is_empty());
        if outcome == ReaderPoolOutcome::Success {
            worker.finish(outcome);
        }
        drop(worker);
        assert_eq!(recorded_checkout_outcomes(), vec![outcome]);
    }
}

#[test]
fn reader_pool_diagnostics_warmup_observes_all_readers() -> Result<()> {
    let _guard = diagnostics_guard();
    for outcome in [
        ReaderPoolOutcome::Success,
        ReaderPoolOutcome::Timeout,
        ReaderPoolOutcome::Cancelled,
    ] {
        clear_recorded();
        let budget = if outcome == ReaderPoolOutcome::Timeout {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(2)
        };
        let (_directory, db) = recording_fixture(2, budget)?;
        let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
        let cancel = scope.clone();
        let attempts = Arc::new(AtomicU64::new(0));
        let hook_attempts = Arc::clone(&attempts);
        install_hook(&db.readers, move |event, _writer| {
            if event == ReaderTestEvent::BeforeOpen {
                let attempt = hook_attempts.fetch_add(1, Ordering::Relaxed);
                if attempt == 1 {
                    // The first reader is held while the second is acquired.
                    assert!(recorded_events()
                        .iter()
                        .any(|(_, event)| matches!(event, ReaderPoolEvent::LeaseStarted)));
                    assert!(recorded_checkout_outcomes().is_empty());
                }
                if attempt >= 1 {
                    match outcome {
                        ReaderPoolOutcome::Timeout => bail!("second reader cannot connect"),
                        ReaderPoolOutcome::Cancelled => cancel.cancel(),
                        _ => {}
                    }
                }
            }
            Ok(())
        });
        let result = db.readers.with_scope(scope).warm_for_fixture();
        assert!(attempts.load(Ordering::Relaxed) >= 2);
        if outcome == ReaderPoolOutcome::Success {
            assert_eq!(result?, 2);
        } else {
            assert!(result.is_err());
        }
        assert_eq!(recorded_checkout_outcomes(), vec![outcome]);
    }
    Ok(())
}

#[test]
fn reader_pool_diagnostics_detached_worker_keeps_checkout_open() -> Result<()> {
    let _guard = diagnostics_guard();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let (_directory, db) = recording_fixture(1, Duration::from_secs(5))?;
    let raw_pool = db.readers.shared.pool.lock().unwrap().clone().unwrap();
    let held = raw_pool.get_timeout(Duration::from_secs(5))?;
    clear_recorded();
    runtime.block_on(async {
        let reader = db.readers.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            reader
                .with_read_async_at(read_block::ReadOrigin::UNKNOWN, move |_| {
                    let _ = done_tx.send(());
                    Ok(())
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while db.readers.stats().checkout_attempts == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(recorded_checkout_outcomes().is_empty());
        drop(held);
        tokio::time::timeout(Duration::from_secs(5), done_rx).await??;
        assert_eq!(
            recorded_checkout_outcomes(),
            vec![ReaderPoolOutcome::Success]
        );
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}

#[test]
fn reader_pool_diagnostics_classify_scoped_blocking_queue_exit() -> Result<()> {
    use std::future::Future;
    use std::task::Poll;

    let _guard = diagnostics_guard();
    for explicit_cancel in [false, true] {
        clear_recorded();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()?;
        let (_directory, db) = recording_fixture(1, Duration::from_secs(30))?;
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        runtime.block_on(async {
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            });
            started_rx.recv_timeout(Duration::from_secs(5))?;
            let allowance = if explicit_cancel {
                Duration::from_secs(30)
            } else {
                Duration::from_millis(250)
            };
            let scope = RuntimeDbReadScope::new(Instant::now() + allowance);
            let pool = db.readers.with_scope(scope.clone());
            let ran = Arc::new(AtomicBool::new(false));
            let task_ran = Arc::clone(&ran);
            let read = pool.with_read_async_at(read_block::ReadOrigin::UNKNOWN, move |_| {
                task_ran.store(true, Ordering::Relaxed);
                Ok(())
            });
            tokio::pin!(read);
            // Poll into spawn_blocking while the sole worker is held.
            std::future::poll_fn(|cx| {
                assert!(read.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert_eq!(pool.stats().available_admission, 0);
            assert_eq!(pool.stats().checkout_attempts, 0);
            if explicit_cancel {
                scope.cancel();
            }
            let result = tokio::time::timeout(Duration::from_secs(5), read).await?;
            drop(release_tx);
            blocker.await?;
            assert!(result.is_err());
            assert!(!ran.load(Ordering::Relaxed));
            assert_eq!(
                recorded_checkout_outcomes(),
                vec![if explicit_cancel {
                    ReaderPoolOutcome::Cancelled
                } else {
                    ReaderPoolOutcome::Timeout
                }]
            );
            Ok::<_, anyhow::Error>(())
        })?;
    }
    Ok(())
}

#[test]
fn reader_pool_diagnostics_report_scope_cancellation_as_cancelled() -> Result<()> {
    let _guard = diagnostics_guard();
    clear_recorded();
    let (_directory, db) = recording_fixture(1, Duration::from_secs(2))?;
    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(30));
    scope.cancel();
    let pool = db.readers.with_scope(scope);
    let error = pool.with_read(|_| Ok(())).unwrap_err();
    assert!(error.downcast_ref::<RuntimeDbRetryableError>().is_some());
    let events = recorded_events();
    assert!(events
        .iter()
        .any(|(role, event)| *role == ReaderPoolRole::Ordinary
            && matches!(
                event,
                ReaderPoolEvent::CheckoutFinished {
                    outcome: ReaderPoolOutcome::Cancelled,
                    ..
                }
            )));
    assert!(!events.contains(&(ReaderPoolRole::Ordinary, ReaderPoolEvent::ConnectionOpened)));
    Ok(())
}

#[test]
fn reader_pool_diagnostics_report_expired_scope_as_timeout() -> Result<()> {
    let _guard = diagnostics_guard();
    clear_recorded();
    let (_directory, db) = recording_fixture(1, Duration::from_secs(2))?;
    let scope = RuntimeDbReadScope::new(Instant::now());
    // An exhausted deadline takes precedence when both causes are present.
    scope.cancel();
    let pool = db.readers.with_scope(scope);
    let error = pool.with_read(|_| Ok(())).unwrap_err();
    assert!(error.downcast_ref::<RuntimeDbRetryableError>().is_some());
    let outcomes: Vec<_> = recorded_events()
        .iter()
        .filter_map(|(_, event)| match event {
            ReaderPoolEvent::CheckoutFinished { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes, vec![ReaderPoolOutcome::Timeout]);
    assert!(!recorded_events()
        .iter()
        .any(|(_, event)| matches!(event, ReaderPoolEvent::ConnectStarted)));
    Ok(())
}

#[test]
fn reader_pool_diagnostics_keep_roles_bounded() -> Result<()> {
    let _guard = diagnostics_guard();
    clear_recorded();
    let (_directory, db) = recording_fixture(1, Duration::from_secs(2))?;
    let preparation =
        RuntimeDbReaderPool::with_capacity(Arc::clone(db.writer.state()), 1, "preparation");
    let events = recorded_events();
    assert!(events.contains(&(
        ReaderPoolRole::Ordinary,
        ReaderPoolEvent::Created { capacity: 1 }
    )));
    assert!(events.contains(&(
        ReaderPoolRole::Preparation,
        ReaderPoolEvent::Created { capacity: 1 }
    )));
    drop(preparation);
    assert!(recorded_events().contains(&(
        ReaderPoolRole::Preparation,
        ReaderPoolEvent::Dropped { capacity: 1 }
    )));
    Ok(())
}
