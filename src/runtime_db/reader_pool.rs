//! Bounded read-role connections. The writer/sidecar owner outlives every reader.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use r2d2::{ManageConnection, Pool, PooledConnection};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{ffi::ErrorCode, Connection};
use tokio::sync::Semaphore;

use crate::diagnostics::attribution;

use super::write_queue::RuntimeDbWriterState;
use super::{read_block, RuntimeDbProtectionError, RuntimeDbReadScope, RuntimeDbRetryableError};

const DEFAULT_CAPACITY: u32 = 4;
const DEFAULT_BUDGET: Duration = Duration::from_millis(250);
const LOG_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct RuntimeDbReaderPool {
    shared: Arc<ReaderPoolShared>,
    scope: Option<RuntimeDbReadScope>,
}

struct ReaderPoolShared {
    pool: Mutex<Option<Pool<ReaderManager>>>,
    admission: Arc<Semaphore>,
    budget: Duration,
    control: Arc<ReaderPoolControl>,
}

struct ReaderPoolControl {
    writer: Arc<RuntimeDbWriterState>,
    owner: Weak<ReaderPoolShared>,
    closed: AtomicBool,
    stats: Counters,
    last_connect_warning: Mutex<Option<Instant>>,
    last_timeout_warning: Mutex<Option<Instant>>,
    #[cfg(test)]
    test_hook: Mutex<Option<Arc<ReaderTestHook>>>,
}

#[cfg(test)]
type ReaderTestHook = dyn Fn(ReaderTestEvent, &RuntimeDbWriterState) -> Result<()> + Send + Sync;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReaderTestEvent {
    BeforeOpen,
    Opened,
    Closed,
}

#[derive(Default)]
struct Counters {
    connect_attempts: AtomicU64,
    connect_failures: AtomicU64,
    connections_opened: AtomicU64,
    connections_dropped: AtomicU64,
    checkout_timeouts: AtomicU64,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct ReaderPoolStats {
    pub(crate) capacity: u32,
    pub(crate) connect_attempts: u64,
    pub(crate) connect_failures: u64,
    pub(crate) connections_opened: u64,
    pub(crate) connections_dropped: u64,
    pub(crate) checkout_timeouts: u64,
    pub(crate) current_connections: u32,
    pub(crate) idle_connections: u32,
    pub(crate) available_admission: usize,
}

struct ReaderManager {
    control: Arc<ReaderPoolControl>,
}

struct ReaderConnection {
    // Explicitly close SQLite before releasing the sidecar protection owner.
    connection: Option<Connection>,
    control: Arc<ReaderPoolControl>,
    broken: bool,
}

impl Drop for ReaderConnection {
    fn drop(&mut self) {
        drop(self.connection.take());
        #[cfg(test)]
        let _ = self.control.test_hook(ReaderTestEvent::Closed);
        self.control
            .stats
            .connections_dropped
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct ConnectError(anyhow::Error);

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for ConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[derive(Debug)]
struct QuietConnectErrors;

impl r2d2::HandleError<ConnectError> for QuietConnectErrors {
    fn handle_error(&self, _error: ConnectError) {
        // The manager records each failure and rate-limits its own structured log.
    }
}

impl RuntimeDbReaderPool {
    pub(crate) fn new(writer: Arc<RuntimeDbWriterState>) -> Self {
        Self::with_config(writer, DEFAULT_CAPACITY, DEFAULT_BUDGET)
    }

    pub(crate) fn with_config(
        writer: Arc<RuntimeDbWriterState>,
        capacity: u32,
        budget: Duration,
    ) -> Self {
        assert!(capacity > 0, "reader pool capacity must be positive");
        let shared = Arc::new_cyclic(|owner| {
            let control = Arc::new(ReaderPoolControl {
                writer,
                owner: owner.clone(),
                closed: AtomicBool::new(false),
                stats: Counters::default(),
                last_connect_warning: Mutex::new(None),
                last_timeout_warning: Mutex::new(None),
                #[cfg(test)]
                test_hook: Mutex::new(None),
            });
            let pool = Pool::builder()
                .max_size(capacity)
                .min_idle(Some(0))
                .connection_timeout(Duration::from_secs(30))
                .idle_timeout(None)
                .max_lifetime(None)
                .test_on_check_out(false)
                .error_handler(Box::new(QuietConnectErrors))
                .build_unchecked(ReaderManager {
                    control: Arc::clone(&control),
                });
            ReaderPoolShared {
                pool: Mutex::new(Some(pool)),
                admission: Arc::new(Semaphore::new(capacity as usize)),
                budget,
                control,
            }
        });
        Self {
            shared,
            scope: None,
        }
    }

    pub(crate) fn with_scope(&self, scope: RuntimeDbReadScope) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            scope: Some(scope),
        }
    }

    fn acquisition_deadline(&self) -> Instant {
        let deadline = Instant::now() + self.shared.budget;
        self.effective_scope()
            .as_ref()
            .map_or(deadline, |scope| deadline.min(scope.deadline()))
    }

    /// Explicitly scoped handles win over the deadline of the enclosing
    /// blocking read block (`RuntimeDb::read_block`).
    fn effective_scope(&self) -> Option<RuntimeDbReadScope> {
        self.scope.clone().or_else(read_block::current_scope)
    }

    fn check_scope(&self, scope: &Option<RuntimeDbReadScope>) -> Result<()> {
        if let Some(scope) = scope {
            scope.check(&self.shared.control.writer.path)?;
        }
        Ok(())
    }

    pub(crate) fn check_read_scope(&self) -> Result<()> {
        self.shared.control.ensure_protected()?;
        self.check_scope(&self.effective_scope())
    }

    /// Cheap assembly checkpoint; callers must fully check entry and success.
    pub(crate) fn check_read_cancellation(&self) -> Result<()> {
        if let Err(error) = self.check_scope(&self.effective_scope()) {
            self.shared.control.ensure_protected()?;
            return Err(error);
        }
        Ok(())
    }

    /// Await only a memory-lock acquisition, never an async SQL wrapper.
    pub(crate) async fn wait_for_memory_lock<T>(
        &self,
        lock: impl std::future::Future<Output = T>,
    ) -> Result<T> {
        self.check_read_scope()?;
        let acquired = tokio::select! {
            guard = lock => Some(guard),
            () = RuntimeDbReadScope::cancelled(&self.scope) => None,
            () = async {
                match &self.scope {
                    Some(scope) => tokio::time::sleep_until(scope.deadline().into()).await,
                    None => std::future::pending().await,
                }
            } => None,
        };
        // Recheck even on success: an already-ready lock can race protection loss
        // or cancellation. No SQL or reader checkout is needed for this check.
        self.check_read_scope()?;
        acquired.ok_or_else(|| anyhow!("scoped memory lock interrupted"))
    }

    #[cfg(test)]
    pub(crate) fn with_read<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        self.with_read_at(read_block::ReadOrigin::UNKNOWN, f)
    }

    /// Only owned results may escape. Callers must not change connection hooks,
    /// authorizers or other connection-local configuration through the Rust API.
    pub(crate) fn with_read_at<T>(
        &self,
        origin: read_block::ReadOrigin,
        f: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let scope = self.effective_scope();
        self.read_until(
            self.acquisition_deadline(),
            scope.as_ref().map(RuntimeDbReadScope::child),
            origin,
            f,
        )
    }

    fn read_until<T>(
        &self,
        deadline: Instant,
        scope: Option<RuntimeDbReadScope>,
        origin: read_block::ReadOrigin,
        f: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let started = Instant::now();
        let checkout = attribution::READER_CHECKOUT.start();
        let control = &self.shared.control;
        control.ensure_protected()?;
        self.check_scope(&scope)?;
        let pool = self
            .shared
            .pool
            .lock()
            .map_err(|_| anyhow!("runtime db reader pool mutex poisoned"))?
            .clone()
            .ok_or_else(|| anyhow!("runtime db reader pool is closed"))?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.check_scope(&scope)?;
            control.timeout();
            return Err(RuntimeDbRetryableError::new(
                "reader pool checkout",
                &control.writer.path,
                "admission/checkout budget exhausted before execution",
            )
            .into());
        }
        let connection = pool.get_timeout(remaining).map_err(|error| {
            control.timeout();
            // In particular, do not turn quarantine evidence into a timeout string.
            control
                .ensure_protected()
                .and_then(|_| self.check_scope(&scope))
                .err()
                .unwrap_or_else(|| {
                    RuntimeDbRetryableError::new(
                        "reader pool checkout",
                        &control.writer.path,
                        error,
                    )
                    .into()
                })
        })?;
        drop(checkout);
        let checkout_elapsed = started.elapsed();
        // Declare the timer first so the lease is returned before it stops.
        let _held = attribution::READER_LEASE.start();
        let mut lease = ReadLease {
            connection,
            progress_handler: false,
        };
        control.ensure_protected()?;
        self.check_scope(&scope)?;
        if let Some(scope) = scope.clone() {
            lease.progress_handler = true;
            lease
                .connection
                .connection
                .as_ref()
                .expect("live reader")
                .progress_handler(1000, Some(move || scope.is_cancelled()));
        }
        let operation_started = Instant::now();
        let result = {
            // The closure includes row decoding and transactions, not only SQL.
            let _operation = attribution::READER_OPERATION.start();
            let _origin = read_block::ReadContextGuard::enter_origin(origin);
            f(lease.connection.connection.as_mut().expect("live reader"))
        };
        read_block::record_read_block(origin, checkout_elapsed, operation_started.elapsed());
        // Cleanup SQL must not inherit the operation's expired/cancelled hook.
        lease.clear_progress_handler();
        if result.as_ref().err().is_some_and(is_broken_error) {
            lease.connection.broken = true;
        }
        let clean = reader_is_clean(&lease.connection);
        if let Err(error) = control.ensure_protected() {
            lease.connection.broken = true;
            return Err(error);
        }
        if !clean {
            lease.connection.broken = true;
            return result
                .and_then(|_| Err(anyhow!("runtime db reader returned with dirty state")));
        }
        // Protection/corruption evidence must not become a retryable cancellation.
        if !result.as_ref().err().is_some_and(is_broken_error) {
            self.check_scope(&scope)?;
        }
        result
    }

    pub(crate) async fn with_read_async_at<T: Send + 'static>(
        &self,
        origin: read_block::ReadOrigin,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let admission = attribution::READER_ADMISSION.start();
        let deadline = self.acquisition_deadline();
        let scope = self.scope.as_ref().map(RuntimeDbReadScope::child);
        self.shared.control.ensure_protected()?;
        self.check_scope(&scope)?;
        let admitted = tokio::select! {
            result = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                Arc::clone(&self.shared.admission).acquire_owned(),
            ) => result,
            () = RuntimeDbReadScope::cancelled(&scope) => {
                self.shared.control.ensure_protected()?;
                self.check_scope(&scope)?;
                unreachable!("cancelled scope must reject the read");
            }
        };
        let permit = match admitted {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                self.shared.control.ensure_protected()?;
                bail!("runtime db reader pool is closed");
            }
            Err(_) => {
                self.shared.control.timeout();
                self.shared.control.ensure_protected()?;
                self.check_scope(&scope)?;
                return Err(RuntimeDbRetryableError::new(
                    "reader pool admission",
                    &self.shared.control.writer.path,
                    "admission budget exhausted",
                )
                .into());
            }
        };
        drop(admission);
        self.shared.control.ensure_protected()?;
        self.check_scope(&scope)?;
        let reader = self.clone();
        let task_scope = scope.clone();
        let state = Arc::new(AtomicU8::new(0));
        let task_state = Arc::clone(&state);
        let blocking_admission = attribution::READER_BLOCKING_ADMISSION.start();
        let mut task = tokio::task::spawn_blocking(move || {
            drop(blocking_admission);
            let _permit = permit;
            // Cancellation and task start have a single linearization point.
            if task_state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                bail!("runtime db read cancelled before execution");
            }
            reader.read_until(deadline, task_scope, origin, f)
        });
        let _cancel = CancelQueuedRead {
            state: Arc::clone(&state),
            abort: task.abort_handle(),
            scope: scope.clone(),
        };
        let joined = tokio::select! {
            result = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline), &mut task,
            ) => result.ok(),
            () = RuntimeDbReadScope::cancelled(&scope) => None,
        };
        let joined = match joined {
            Some(result) => result,
            None => {
                if state
                    .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    task.abort();
                    self.shared.control.timeout();
                    self.shared.control.ensure_protected()?;
                    self.check_scope(&scope)?;
                    return Err(RuntimeDbRetryableError::new(
                        "reader pool blocking admission",
                        &self.shared.control.writer.path,
                        "blocking queue budget exhausted before execution",
                    )
                    .into());
                }
                // Started work retains its real permit and lease until it exits.
                // Only an explicit scope makes execution cancellable.
                task.await
            }
        };
        self.shared.control.ensure_protected()?;
        let result =
            joined.map_err(|error| anyhow!("runtime db blocking read task failed: {error}"))?;
        if !result.as_ref().err().is_some_and(is_broken_error) {
            self.check_scope(&scope)?;
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ReaderPoolStats {
        let stats = &self.shared.control.stats;
        let state = self
            .shared
            .pool
            .lock()
            .ok()
            .and_then(|pool| pool.as_ref().map(Pool::state));
        ReaderPoolStats {
            capacity: self
                .shared
                .pool
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0, Pool::max_size),
            connect_attempts: stats.connect_attempts.load(Ordering::Relaxed),
            connect_failures: stats.connect_failures.load(Ordering::Relaxed),
            connections_opened: stats.connections_opened.load(Ordering::Relaxed),
            connections_dropped: stats.connections_dropped.load(Ordering::Relaxed),
            checkout_timeouts: stats.checkout_timeouts.load(Ordering::Relaxed),
            current_connections: state.as_ref().map_or(0, |state| state.connections),
            idle_connections: state.map_or(0, |state| state.idle_connections),
            available_admission: self.shared.admission.available_permits(),
        }
    }

    #[cfg(test)]
    pub(crate) fn writer_weak(&self) -> Weak<RuntimeDbWriterState> {
        Arc::downgrade(&self.shared.control.writer)
    }
}

struct CancelQueuedRead {
    state: Arc<AtomicU8>,
    abort: tokio::task::AbortHandle,
    scope: Option<RuntimeDbReadScope>,
}

impl Drop for CancelQueuedRead {
    fn drop(&mut self) {
        if let Some(scope) = &self.scope {
            scope.cancel();
        }
        let _ = self
            .state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        self.abort.abort();
    }
}

struct ReadLease {
    connection: PooledConnection<ReaderManager>,
    progress_handler: bool,
}

impl ReadLease {
    fn clear_progress_handler(&mut self) {
        if self.progress_handler {
            self.connection
                .connection
                .as_ref()
                .expect("live reader")
                .progress_handler(0, None::<fn() -> bool>);
            self.progress_handler = false;
        }
    }
}

impl Drop for ReadLease {
    fn drop(&mut self) {
        self.clear_progress_handler();
        if std::thread::panicking() {
            self.connection.broken = true;
        }
    }
}

impl ReaderPoolControl {
    #[cfg(test)]
    fn test_hook(&self, event: ReaderTestEvent) -> Result<()> {
        let hook = self
            .test_hook
            .lock()
            .map_err(|_| anyhow!("reader pool test hook mutex poisoned"))?
            .clone();
        if let Some(hook) = hook {
            hook(event, &self.writer)?;
        }
        Ok(())
    }

    fn ensure_protected(&self) -> Result<()> {
        if let Err(error) = self.writer.ensure_protected() {
            self.close();
            return Err(error);
        }
        if self.closed.load(Ordering::Acquire) {
            bail!("runtime db reader pool is closed");
        }
        Ok(())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(owner) = self.owner.upgrade() {
            owner.admission.close();
            // Do not retain the facade while dropping connections or calling r2d2.
            let pool = owner.pool.lock().ok().and_then(|mut pool| pool.take());
            if let Some(pool) = pool {
                // Other leases can still own Pool clones. Explicitly drain idle FDs.
                while let Some(mut connection) = pool.try_get() {
                    connection.broken = true;
                    drop(connection);
                }
            }
        }
    }

    fn timeout(&self) {
        self.stats.checkout_timeouts.fetch_add(1, Ordering::Relaxed);
        if should_log(&self.last_timeout_warning) {
            tracing::warn!(
                path = %self.writer.path.display(),
                "runtime db reader pool admission/checkout timed out"
            );
        }
    }
}

impl Drop for ReaderPoolShared {
    fn drop(&mut self) {
        // Pending connect jobs retain their own protection owner, not this facade.
        self.control.closed.store(true, Ordering::Release);
        self.admission.close();
        if let Ok(pool) = self.pool.get_mut() {
            drop(pool.take());
        }
    }
}

impl ManageConnection for ReaderManager {
    type Connection = ReaderConnection;
    type Error = ConnectError;

    fn connect(&self) -> std::result::Result<ReaderConnection, ConnectError> {
        let control = &self.control;
        control
            .stats
            .connect_attempts
            .fetch_add(1, Ordering::Relaxed);
        let result: Result<ReaderConnection> = (|| {
            control.ensure_protected()?;
            #[cfg(test)]
            control.test_hook(ReaderTestEvent::BeforeOpen)?;
            let mut connection = control.writer.open_connection()?;
            // Slow statement reporting is a reader concern: every runtime DB
            // read runs on a pooled reader connection.
            read_block::install_statement_profile(&mut connection)?;
            control
                .stats
                .connections_opened
                .fetch_add(1, Ordering::Relaxed);
            let reader = ReaderConnection {
                connection: Some(connection),
                control: Arc::clone(control),
                broken: false,
            };
            let connection = reader.connection.as_ref().expect("live reader");
            connection.pragma_update(None, "query_only", true)?;
            connection.authorizer(Some(read_authorizer));
            #[cfg(test)]
            control.test_hook(ReaderTestEvent::Opened)?;
            control.ensure_protected()?;
            Ok(reader)
        })();
        result.map_err(|error| {
            if error
                .chain()
                .any(|source| source.is::<RuntimeDbProtectionError>())
            {
                control.close();
            }
            control
                .stats
                .connect_failures
                .fetch_add(1, Ordering::Relaxed);
            if should_log(&control.last_connect_warning) {
                tracing::warn!(
                    path = %control.writer.path.display(),
                    error = %format!("{error:#}"),
                    "runtime db reader connection failed; pool retries with bounded backoff"
                );
            }
            ConnectError(error)
        })
    }

    fn is_valid(
        &self,
        _connection: &mut ReaderConnection,
    ) -> std::result::Result<(), ConnectError> {
        self.control.ensure_protected().map_err(ConnectError)
    }

    fn has_broken(&self, connection: &mut ReaderConnection) -> bool {
        connection.broken
            || self.control.closed.load(Ordering::Acquire)
            || !connection
                .connection
                .as_ref()
                .expect("live reader")
                .is_autocommit()
    }
}

fn reader_is_clean(reader: &ReaderConnection) -> bool {
    let connection = reader.connection.as_ref().expect("live reader");
    // SQL configuration changes are denied by the authorizer. Also reject
    // attachments introduced through a caller violating the Rust API contract.
    // SAFETY: the connection remains live and SQLite only inspects its schemas.
    let has_attachment =
        unsafe { !rusqlite::ffi::sqlite3_db_name(connection.handle(), 2).is_null() };
    connection.is_autocommit()
        && !has_attachment
        && connection
            .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
            .unwrap_or(false)
}

fn read_authorizer(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Read { .. }
        | AuthAction::Select
        | AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. }
        | AuthAction::Recursive => Authorization::Allow,
        AuthAction::Function { function_name }
            if !function_name.eq_ignore_ascii_case("load_extension") =>
        {
            Authorization::Allow
        }
        // query_only is inspected at return; other PRAGMAs can mutate state even
        // without a value (e.g. optimize/shrink_memory). No business PRAGMA API.
        AuthAction::Pragma {
            pragma_name,
            pragma_value: None,
        } if pragma_name.eq_ignore_ascii_case("query_only") => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

fn is_broken_error(error: &anyhow::Error) -> bool {
    error.chain().any(|error| {
        if error.is::<RuntimeDbProtectionError>() {
            return true;
        }
        matches!(
            error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _))
                if matches!(
                    code.code,
                    ErrorCode::DatabaseCorrupt
                        | ErrorCode::NotADatabase
                        | ErrorCode::SystemIoFailure
                        | ErrorCode::InternalMalfunction
                        | ErrorCode::ApiMisuse
                        | ErrorCode::CannotOpen
                        | ErrorCode::FileLockingProtocolFailed
                        | ErrorCode::OutOfMemory
                )
        )
    })
}

fn should_log(last: &Mutex<Option<Instant>>) -> bool {
    let Ok(mut last) = last.lock() else {
        return false;
    };
    let now = Instant::now();
    if last.is_some_and(|previous| now.duration_since(previous) < LOG_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::runtime_db::{RuntimeDb, RuntimeDbProtectionState};

    fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
        let directory = tempfile::tempdir()?;
        let mut db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        db.configure_reader_pool_for_tests(1, Duration::from_secs(2))?;
        Ok((directory, db))
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
    fn removed_or_replaced_database_quarantines_idle_readers_without_running_closure() -> Result<()>
    {
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
            assert_eq!(db.connection().err().unwrap().to_string(), evidence);
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
        let connection = super::super::connection::open_connection(&path)?;
        let replacement = directory.path().join("replacement.sqlite");
        std::fs::write(&replacement, b"replacement database evidence")?;
        std::fs::rename(&replacement, &path)?;
        let error = super::super::write_queue::RuntimeDbWriter::open_starting(path, connection)
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
                    Instant::now() + Duration::from_millis(30),
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
                Instant::now() + Duration::from_secs(4),
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
            let result =
                unsafe { libc::fcntl(guard_probe.as_raw_fd(), libc::F_OFD_GETLK, &mut lock) };
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
                Instant::now() + Duration::from_millis(100),
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
}
