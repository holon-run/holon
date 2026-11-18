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

use crate::diagnostics::{ReaderPoolEvent, ReaderPoolOutcome, ReaderPoolRole, StorageStage};

mod observation;
use observation::{CheckoutObservation, ConnectObservation, LeaseObservation};

use super::write_queue::RuntimeDbWriterState;
use super::{read_block, RuntimeDbProtectionError, RuntimeDbReadScope, RuntimeDbRetryableError};

const CHECK_INTERVAL: Duration = Duration::from_millis(100);
const LOG_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct RuntimeDbReaderPool {
    shared: Arc<ReaderPoolShared>,
    scope: Option<RuntimeDbReadScope>,
}

struct ReaderPoolShared {
    pool: Mutex<Option<Pool<ReaderManager>>>,
    admission: Arc<Semaphore>,
    budget: Option<Duration>,
    control: Arc<ReaderPoolControl>,
    capacity: u32,
}

struct ReaderPoolControl {
    writer: Arc<RuntimeDbWriterState>,
    owner: Weak<ReaderPoolShared>,
    closed: AtomicBool,
    stats: Counters,
    last_connect_warning: Mutex<Option<Instant>>,
    last_timeout_warning: Mutex<Option<Instant>>,
    role: &'static str,
    diagnostic_role: ReaderPoolRole,
    holders: Mutex<Vec<Option<(read_block::ReadOrigin, Instant)>>>,
    #[cfg(any(test, feature = "test-support"))]
    test_hook: Mutex<Option<Arc<ReaderTestHook>>>,
}

#[cfg(any(test, feature = "test-support"))]
type ReaderTestHook = dyn Fn(ReaderTestEvent, &RuntimeDbWriterState) -> Result<()> + Send + Sync;

#[cfg(any(test, feature = "test-support"))]
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
    #[cfg(any(test, feature = "test-support"))]
    checkout_attempts: AtomicU64,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct ReaderPoolStats {
    pub capacity: u32,
    pub connect_attempts: u64,
    pub connect_failures: u64,
    pub connections_opened: u64,
    pub connections_dropped: u64,
    pub checkout_timeouts: u64,
    pub checkout_attempts: u64,
    pub current_connections: u32,
    pub idle_connections: u32,
    pub available_admission: usize,
}

struct ReaderManager {
    control: Arc<ReaderPoolControl>,
}

struct ReaderConnection {
    // Explicitly close SQLite before releasing the sidecar protection owner.
    connection: Option<Connection>,
    control: Arc<ReaderPoolControl>,
    broken: bool,
    observed: bool,
}

impl Drop for ReaderConnection {
    fn drop(&mut self) {
        drop(self.connection.take());
        if self.observed {
            self.control.observe(ReaderPoolEvent::ConnectionClosed);
        }
        #[cfg(any(test, feature = "test-support"))]
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
    pub fn new(writer: Arc<RuntimeDbWriterState>) -> Self {
        Self::with_capacity(writer, super::DEFAULT_ORDINARY_MAX_CONNECTIONS, "ordinary")
    }

    pub fn for_preparation(writer: Arc<RuntimeDbWriterState>) -> Self {
        Self::with_capacity(
            writer,
            super::DEFAULT_PREPARATION_MAX_CONNECTIONS,
            "preparation",
        )
    }

    pub fn with_capacity(
        writer: Arc<RuntimeDbWriterState>,
        capacity: u32,
        role: &'static str,
    ) -> Self {
        Self::with_role(writer, capacity, None, role)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_config(writer: Arc<RuntimeDbWriterState>, capacity: u32, budget: Duration) -> Self {
        Self::with_role(writer, capacity, Some(budget), "test")
    }

    fn with_role(
        writer: Arc<RuntimeDbWriterState>,
        capacity: u32,
        budget: Option<Duration>,
        role: &'static str,
    ) -> Self {
        assert!(capacity > 0, "reader pool capacity must be positive");
        // Export only the two fixed production roles. Test and custom log
        // labels must never become metric labels: `with_capacity` keeps its
        // string signature, but the diagnostic dimension stays bounded.
        let diagnostic_role = match role {
            "preparation" => ReaderPoolRole::Preparation,
            _ => ReaderPoolRole::Ordinary,
        };
        let shared = Arc::new_cyclic(|owner| {
            let control = Arc::new(ReaderPoolControl {
                writer,
                owner: owner.clone(),
                closed: AtomicBool::new(false),
                stats: Counters::default(),
                last_connect_warning: Mutex::new(None),
                last_timeout_warning: Mutex::new(None),
                role,
                diagnostic_role,
                holders: Mutex::new(vec![None; capacity as usize]),
                #[cfg(any(test, feature = "test-support"))]
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
                capacity,
            }
        });
        shared
            .control
            .observe(ReaderPoolEvent::Created { capacity });
        Self {
            shared,
            scope: None,
        }
    }

    pub fn with_scope(&self, scope: RuntimeDbReadScope) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            scope: Some(scope),
        }
    }

    /// Fixture-only initialization: retain every checkout until all readers
    /// have actually read the database, including SQLite's lazy WAL open.
    pub fn warm_for_fixture(&self) -> Result<u32> {
        self.check_read_scope()?;
        let pool = self
            .shared
            .pool
            .lock()
            .map_err(|_| anyhow!("runtime db reader pool mutex poisoned"))?
            .clone()
            .ok_or_else(|| anyhow!("runtime db reader pool is closed"))?;
        let capacity = pool.max_size();
        let deadline = self.acquisition_deadline();
        let mut readers = Vec::with_capacity(capacity as usize);
        let checkout = CheckoutObservation::start(
            self.shared.control.writer.diagnostics,
            self.shared.control.role(),
        );
        let scope = self.effective_scope();
        for _ in 0..capacity {
            self.shared.control.ensure_protected()?;
            self.check_scope_observed(&scope, &checkout)?;
            let mut reader = self.checkout_connection(
                &pool,
                deadline,
                &scope,
                read_block::ReadOrigin::UNKNOWN,
                "warmup",
                &checkout,
            )?;
            self.shared.control.ensure_protected()?;
            self.check_scope_observed(&scope, &checkout)?;
            if let Err(error) = reader
                .connection
                .connection
                .as_ref()
                .expect("live reader")
                .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
            {
                reader.connection.broken = true;
                return Err(error.into());
            }
            self.shared.control.ensure_protected()?;
            self.check_scope_observed(&scope, &checkout)?;
            readers.push(reader);
        }
        checkout.finish(ReaderPoolOutcome::Success);
        Ok(capacity)
    }

    fn acquisition_deadline(&self) -> Option<Instant> {
        let budget = self.shared.budget.map(|budget| Instant::now() + budget);
        let scope = self.effective_scope().map(|scope| scope.deadline());
        match (budget, scope) {
            (Some(budget), Some(scope)) => Some(budget.min(scope)),
            (budget, scope) => budget.or(scope),
        }
    }

    fn checkout_connection(
        &self,
        pool: &Pool<ReaderManager>,
        deadline: Option<Instant>,
        scope: &Option<RuntimeDbReadScope>,
        origin: read_block::ReadOrigin,
        stage: &'static str,
        observation: &CheckoutObservation,
    ) -> Result<ReadLease> {
        #[cfg(any(test, feature = "test-support"))]
        self.shared
            .control
            .stats
            .checkout_attempts
            .fetch_add(1, Ordering::Relaxed);
        loop {
            self.shared.control.ensure_protected()?;
            self.check_scope_observed(scope, observation)?;
            let remaining =
                deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                self.shared.control.timeout(origin, stage);
                observation.finish(ReaderPoolOutcome::Timeout);
                return Err(RuntimeDbRetryableError::new(
                    "reader pool checkout",
                    &self.shared.control.writer.path,
                    "admission/checkout deadline exhausted before execution",
                )
                .into());
            }
            let interval =
                remaining.map_or(CHECK_INTERVAL, |remaining| remaining.min(CHECK_INTERVAL));
            match pool.get_timeout(interval) {
                Ok(connection) => {
                    return Ok(ReadLease::new(connection));
                }
                Err(error) => {
                    self.shared.control.ensure_protected()?;
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        self.shared.control.timeout(origin, stage);
                        observation.finish(ReaderPoolOutcome::Timeout);
                        self.check_scope_observed(scope, observation)?;
                        return Err(RuntimeDbRetryableError::new(
                            "reader pool checkout",
                            &self.shared.control.writer.path,
                            error,
                        )
                        .into());
                    }
                    self.check_scope_observed(scope, observation)?;
                }
            }
        }
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

    /// Classify a pre-lease scope rejection without changing scope semantics.
    fn check_scope_observed(
        &self,
        scope: &Option<RuntimeDbReadScope>,
        observation: &CheckoutObservation,
    ) -> Result<()> {
        match self.check_scope(scope) {
            Ok(()) => Ok(()),
            Err(error) => {
                let outcome = if scope
                    .as_ref()
                    .is_some_and(|scope| Instant::now() >= scope.deadline())
                {
                    ReaderPoolOutcome::Timeout
                } else {
                    ReaderPoolOutcome::Cancelled
                };
                observation.finish(outcome);
                Err(error)
            }
        }
    }

    pub fn check_read_scope(&self) -> Result<()> {
        self.shared.control.ensure_protected()?;
        self.check_scope(&self.effective_scope())
    }

    /// Cheap assembly checkpoint; callers must fully check entry and success.
    pub fn check_read_cancellation(&self) -> Result<()> {
        if let Err(error) = self.check_scope(&self.effective_scope()) {
            self.shared.control.ensure_protected()?;
            return Err(error);
        }
        Ok(())
    }

    /// Await only a memory-lock acquisition, never an async SQL wrapper.
    pub async fn wait_for_memory_lock<T>(
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
    fn with_read<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        self.with_read_at(read_block::ReadOrigin::UNKNOWN, f)
    }

    /// Only owned results may escape. Callers must not change connection hooks,
    /// authorizers or other connection-local configuration through the Rust API.
    pub fn with_read_at<T>(
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
        deadline: Option<Instant>,
        scope: Option<RuntimeDbReadScope>,
        origin: read_block::ReadOrigin,
        f: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let checkout = CheckoutObservation::start(
            self.shared.control.writer.diagnostics,
            self.shared.control.role(),
        );
        self.read_until_observed(deadline, scope, origin, f, &checkout)
    }

    fn read_until_observed<T>(
        &self,
        deadline: Option<Instant>,
        scope: Option<RuntimeDbReadScope>,
        origin: read_block::ReadOrigin,
        f: impl FnOnce(&mut Connection) -> Result<T>,
        checkout: &CheckoutObservation,
    ) -> Result<T> {
        let checkout_stage = self
            .shared
            .control
            .writer
            .diagnostics
            .start(StorageStage::ReaderCheckout);
        let control = &self.shared.control;
        control.ensure_protected()?;
        self.check_scope_observed(&scope, checkout)?;
        let pool = self
            .shared
            .pool
            .lock()
            .map_err(|_| anyhow!("runtime db reader pool mutex poisoned"))?
            .clone()
            .ok_or_else(|| anyhow!("runtime db reader pool is closed"))?;
        let lease =
            self.checkout_connection(&pool, deadline, &scope, origin, "checkout", checkout)?;
        checkout.finish(ReaderPoolOutcome::Success);
        drop(checkout_stage);
        let checkout_elapsed = checkout.elapsed();
        // Declare the timer first so the lease is returned before it stops.
        let _held = self
            .shared
            .control
            .writer
            .diagnostics
            .start(StorageStage::ReaderLease);
        let mut lease = lease;
        let _holder = control.track_holder(origin);
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
            let _operation = self
                .shared
                .control
                .writer
                .diagnostics
                .start(StorageStage::ReaderOperation);
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
        result.map_err(|error| {
            if error.chain().any(|source| {
                source
                    .downcast_ref::<rusqlite::Error>()
                    .is_some_and(super::connection::is_sqlite_locked)
            }) {
                let context = RuntimeDbRetryableError::new(
                    "reading runtime db",
                    &control.writer.path,
                    &error,
                );
                error.context(context)
            } else {
                error
            }
        })
    }

    pub async fn with_read_async_at<T: Send + 'static>(
        &self,
        origin: read_block::ReadOrigin,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let admission = self
            .shared
            .control
            .writer
            .diagnostics
            .start(StorageStage::ReaderAdmission);
        let deadline = self.acquisition_deadline();
        // Read scopes use the real clock; Tokio's clock can have a different
        // origin (notably after a paused runtime advances). Keep one allowance.
        let async_deadline = deadline.map(|deadline| {
            tokio::time::Instant::now() + deadline.saturating_duration_since(Instant::now())
        });
        let scope = self.scope.as_ref().map(RuntimeDbReadScope::child);
        let checkout = CheckoutObservation::start(
            self.shared.control.writer.diagnostics,
            self.shared.control.role(),
        );
        self.shared.control.ensure_protected()?;
        self.check_scope_observed(&scope, &checkout)?;
        let pending = Arc::clone(&self.shared.admission).acquire_owned();
        tokio::pin!(pending);
        let admitted = loop {
            tokio::select! {
                result = &mut pending => break Some(result),
                () = wait_for_deadline(async_deadline) => break None,
                () = RuntimeDbReadScope::cancelled(&scope) => {
                    self.shared.control.ensure_protected()?;
                    self.check_scope_observed(&scope, &checkout)?;
                    unreachable!("cancelled scope must reject the read");
                }
                () = tokio::time::sleep(CHECK_INTERVAL) => {
                    self.shared.control.ensure_protected()?;
                    self.check_scope_observed(&scope, &checkout)?;
                }
            }
        };
        let permit = match admitted {
            Some(Ok(permit)) => permit,
            Some(Err(_)) => {
                self.shared.control.ensure_protected()?;
                bail!("runtime db reader pool is closed");
            }
            None => {
                self.shared.control.timeout(origin, "admission");
                checkout.finish(ReaderPoolOutcome::Timeout);
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
        self.check_scope_observed(&scope, &checkout)?;
        let reader = self.clone();
        let task_checkout = checkout.clone();
        let task_scope = scope.clone();
        let state = Arc::new(AtomicU8::new(0));
        let task_state = Arc::clone(&state);
        let blocking_admission = self
            .shared
            .control
            .writer
            .diagnostics
            .start(StorageStage::ReaderBlockingAdmission);
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
            reader.read_until_observed(deadline, task_scope, origin, f, &task_checkout)
        });
        let _cancel = CancelQueuedRead {
            state: Arc::clone(&state),
            abort: task.abort_handle(),
            scope: scope.clone(),
        };
        let joined = tokio::select! {
            result = &mut task => Some(result),
            () = wait_for_deadline(async_deadline) => None,
            () = RuntimeDbReadScope::cancelled(&scope) => None,
        };
        let joined = match joined {
            Some(result) => result,
            None => {
                if state
                    .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let outcome = if scope.as_ref().is_some_and(|scope| {
                        scope.is_cancelled() && Instant::now() < scope.deadline()
                    }) {
                        ReaderPoolOutcome::Cancelled
                    } else {
                        ReaderPoolOutcome::Timeout
                    };
                    // Finish before aborting so the queued task's drop cannot
                    // report this planned deadline/cancellation as a failure.
                    checkout.finish(outcome);
                    task.abort();
                    self.shared.control.timeout(origin, "blocking_admission");
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn stats(&self) -> ReaderPoolStats {
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
            checkout_attempts: stats.checkout_attempts.load(Ordering::Relaxed),
            current_connections: state.as_ref().map_or(0, |state| state.connections),
            idle_connections: state.map_or(0, |state| state.idle_connections),
            available_admission: self.shared.admission.available_permits(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn writer_weak(&self) -> Weak<RuntimeDbWriterState> {
        Arc::downgrade(&self.shared.control.writer)
    }
}

async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
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
    // Fields drop in declaration order: return/close the connection first.
    _observation: LeaseObservation,
}

impl ReadLease {
    fn new(connection: PooledConnection<ReaderManager>) -> Self {
        let observation = LeaseObservation::start(
            connection.control.writer.diagnostics,
            connection.control.role(),
        );
        Self {
            connection,
            progress_handler: false,
            _observation: observation,
        }
    }

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
    #[cfg(any(test, feature = "test-support"))]
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

    fn role(&self) -> ReaderPoolRole {
        self.diagnostic_role
    }

    fn observe(&self, event: ReaderPoolEvent) {
        (self.writer.diagnostics.reader_pool)(self.diagnostic_role, event);
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

    fn track_holder(&self, origin: read_block::ReadOrigin) -> Option<ReaderHolder<'_>> {
        let mut holders = self.holders.lock().ok()?;
        let slot = holders.iter().position(Option::is_none)?;
        holders[slot] = Some((origin, Instant::now()));
        Some(ReaderHolder {
            control: self,
            slot,
        })
    }

    fn timeout(&self, origin: read_block::ReadOrigin, stage: &'static str) {
        self.stats.checkout_timeouts.fetch_add(1, Ordering::Relaxed);
        if should_log(&self.last_timeout_warning) {
            let holders = self.holders.lock().ok().map(|holders| {
                holders
                    .iter()
                    .flatten()
                    .map(|(origin, acquired)| (*origin, acquired.elapsed().as_millis()))
                    .collect::<Vec<_>>()
            });
            tracing::warn!(
                path = %self.writer.path.display(),
                pool_role = self.role,
                stage,
                waiting_origin = ?origin,
                active_holders_ms = ?holders,
                "runtime db reader pool admission/checkout timed out"
            );
        }
    }
}

struct ReaderHolder<'a> {
    control: &'a ReaderPoolControl,
    slot: usize,
}

impl Drop for ReaderHolder<'_> {
    fn drop(&mut self) {
        if let Ok(mut holders) = self.control.holders.lock() {
            holders[self.slot] = None;
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
        self.control.observe(ReaderPoolEvent::Dropped {
            capacity: self.capacity,
        });
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
        let mut observation = ConnectObservation::start(control.writer.diagnostics, control.role());
        let result: Result<ReaderConnection> = (|| {
            control.ensure_protected()?;
            #[cfg(any(test, feature = "test-support"))]
            control.test_hook(ReaderTestEvent::BeforeOpen)?;
            let mut connection = control.writer.open_connection()?;
            // Slow statement reporting is a reader concern: every runtime DB
            // read runs on a pooled reader connection.
            read_block::install_statement_profile(&mut connection)?;
            control
                .stats
                .connections_opened
                .fetch_add(1, Ordering::Relaxed);
            let mut reader = ReaderConnection {
                connection: Some(connection),
                control: Arc::clone(control),
                broken: false,
                observed: false,
            };
            let connection = reader.connection.as_ref().expect("live reader");
            connection.pragma_update(None, "query_only", true)?;
            connection.authorizer(Some(read_authorizer));
            reader.observed = true;
            control.observe(ReaderPoolEvent::ConnectionOpened);
            #[cfg(any(test, feature = "test-support"))]
            control.test_hook(ReaderTestEvent::Opened)?;
            control.ensure_protected()?;
            Ok(reader)
        })();
        observation.finish(result.is_ok());
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
mod tests;
