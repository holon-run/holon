//! Background writer task, write queue, and write context.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock, RwLock, Weak};
use std::thread;
#[cfg(any(test, feature = "test-support"))]
use std::time::Duration;
use std::time::Instant;

use super::blocking::at_blocking_boundary;
use super::workspace_context::WorkspaceContextPublication;
use crate::diagnostics::StorageDiagnostics;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction};

#[cfg(target_os = "linux")]
use crate::connection::RuntimeDbSidecarGuard;
use crate::connection::{
    ensure_runtime_db_sidecars_are_consistent, is_retryable_transaction_error,
    next_runtime_db_retry_delay, open_connection, run_transaction_on_connection,
};
use crate::persistence::{mark_persistence_failure, PersistenceCertainty, PersistenceFailurePhase};
use crate::{
    RuntimeDbProtectionError, RuntimeDbProtectionState, RuntimeDbProtectionStatus,
    RUNTIME_DB_APPEND_RETRY_MAX_DELAY, RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY,
    RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY, RUNTIME_DB_TRANSACTION_RETRY_WARN_INTERVAL,
    RUNTIME_DB_WRITE_QUEUE_CAPACITY,
};

#[derive(Clone)]
pub struct RuntimeDbWriter {
    pub(crate) state: Arc<RuntimeDbWriterState>,
    pub(crate) append_tx: mpsc::SyncSender<RuntimeDbWriteRequest>,
}

pub struct RuntimeDbWriterState {
    pub(crate) path: PathBuf,
    connection: Mutex<Connection>,
    pub(crate) diagnostics: &'static StorageDiagnostics,
    workspace_context: Arc<RwLock<WorkspaceContextPublication>>,
    #[cfg(unix)]
    database_identity: RuntimeDbFileIdentity,
    pub(crate) queue: Arc<RuntimeDbWriteQueue>,
    #[cfg(target_os = "linux")]
    pub(crate) sidecar_guard: Mutex<Option<RuntimeDbSidecarGuard>>,
    protection: Mutex<RuntimeDbProtectionStatus>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RuntimeDbFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl RuntimeDbFileIdentity {
    fn read(path: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(path).map_err(|error| {
            RuntimeDbProtectionError::new(
                path,
                format!("cannot inspect runtime db file identity: {error}"),
            )
        })?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn bind(path: &Path, connection: &Connection) -> Result<Self> {
        let identity = Self::read(path)?;
        let mut moved = 0_i32;
        // SAFETY: the connection and nul-terminated schema outlive the call;
        // SQLite writes only to `moved` before returning.
        let result = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut i32).cast(),
            )
        };
        if result != rusqlite::ffi::SQLITE_OK || moved != 0 {
            return Err(RuntimeDbProtectionError::new(
                path,
                format!(
                    "cannot bind runtime db file identity to opened SQLite handle: result={result}, moved={moved}"
                ),
            )
            .into());
        }
        identity.ensure_matches(path)?;
        Ok(identity)
    }

    fn ensure_matches(self, path: &Path) -> Result<()> {
        let current = Self::read(path)?;
        if current != self {
            return Err(RuntimeDbProtectionError::new(
                path,
                format!("runtime db file identity changed: opened={self:?}, canonical={current:?}"),
            )
            .into());
        }
        Ok(())
    }
}

pub(crate) struct RuntimeDbWriteQueue {
    pub(crate) state: Mutex<RuntimeDbWriteQueueState>,
    pub(crate) available: Condvar,
    pub(super) delivery_summary_generation: AtomicU64,
    pub(super) capability_policy_generation: AtomicU64,
    audit_event_frontiers: Mutex<BTreeMap<Arc<str>, Weak<AtomicU64>>>,
    workspace_context: Mutex<Option<SharedWorkspaceContext>>,
}

struct SharedWorkspaceContext {
    #[cfg(unix)]
    database_identity: RuntimeDbFileIdentity,
    publication: Weak<RwLock<WorkspaceContextPublication>>,
}

#[derive(Default)]
pub(crate) struct RuntimeDbWriteQueueState {
    next_ticket: u64,
    serving_ticket: u64,
}

pub struct RuntimeDbWriteTurn {
    pub(crate) queue: Arc<RuntimeDbWriteQueue>,
}

pub(crate) type RuntimeDbWriteJob =
    Box<dyn for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static>;

pub(crate) struct RuntimeDbWriteRequest {
    context: RuntimeDbWriteContext,
    job: RuntimeDbWriteJob,
    #[cfg(any(test, feature = "test-support"))]
    completion: Option<mpsc::Sender<Result<(), String>>>,
}

#[derive(Clone, Copy)]
pub struct RuntimeDbWriteContext {
    pub(crate) operation: &'static str,
    pub(crate) table: &'static str,
    pub(crate) mode: RuntimeDbWriteMode,
    origin: Option<&'static Location<'static>>,
}

impl RuntimeDbWriteContext {
    pub fn operation(self) -> &'static str {
        self.operation
    }

    pub(crate) const TRANSACTION: Self = Self::sync("runtime_db.transaction", "unknown");

    pub(crate) const fn new(
        operation: &'static str,
        table: &'static str,
        mode: RuntimeDbWriteMode,
    ) -> Self {
        Self {
            operation,
            table,
            mode,
            origin: None,
        }
    }

    pub const fn sync(operation: &'static str, table: &'static str) -> Self {
        Self::new(operation, table, RuntimeDbWriteMode::Sync)
    }

    pub(crate) const fn background(operation: &'static str, table: &'static str) -> Self {
        Self::new(operation, table, RuntimeDbWriteMode::Background)
    }

    #[track_caller]
    pub(crate) fn with_caller(mut self) -> Self {
        self.origin.get_or_insert(Location::caller());
        self
    }

    pub(crate) fn origin(self) -> (&'static str, u32) {
        self.origin
            .map_or(("unknown", 0), |origin| (origin.file(), origin.line()))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RuntimeDbWriteMode {
    Sync,
    Background,
}

impl RuntimeDbWriteMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Background => "background",
        }
    }
}

static RUNTIME_DB_WRITE_QUEUES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<RuntimeDbWriteQueue>>>> =
    OnceLock::new();

impl RuntimeDbWriter {
    #[cfg(test)]
    pub(crate) fn open(path: PathBuf, connection: Connection) -> Result<Self> {
        let writer = Self::open_starting(path, connection, &crate::diagnostics::NO_DIAGNOSTICS)?;
        writer.activate_sidecar_protection()?;
        Ok(writer)
    }

    pub fn open_starting(
        path: PathBuf,
        connection: Connection,
        diagnostics: &'static StorageDiagnostics,
    ) -> Result<Self> {
        #[cfg(unix)]
        let database_identity = RuntimeDbFileIdentity::bind(&path, &connection)?;
        let queue = runtime_db_write_queue(&path)?;
        let workspace_context = queue.workspace_context(
            #[cfg(unix)]
            database_identity,
        )?;
        let state = Arc::new(RuntimeDbWriterState {
            path,
            connection: Mutex::new(connection),
            diagnostics,
            workspace_context,
            #[cfg(unix)]
            database_identity,
            queue,
            #[cfg(target_os = "linux")]
            sidecar_guard: Mutex::new(None),
            protection: Mutex::new(RuntimeDbProtectionStatus {
                state: RuntimeDbProtectionState::Starting,
                evidence: None,
            }),
        });
        let (append_tx, append_rx) =
            mpsc::sync_channel::<RuntimeDbWriteRequest>(RUNTIME_DB_WRITE_QUEUE_CAPACITY);
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("holon-runtime-db-writer".to_string())
            .spawn(move || {
                while let Ok(request) = append_rx.recv() {
                    let RuntimeDbWriteRequest {
                        context,
                        job,
                        #[cfg(any(test, feature = "test-support"))]
                        mut completion,
                    } = request;
                    let mut retry_delay = RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY;
                    loop {
                        match thread_state.append_wait_with_context(context, None, |tx| job(tx)) {
                            Ok(()) => {
                                #[cfg(any(test, feature = "test-support"))]
                                if let Some(completion) = completion.take() {
                                    let _ = completion.send(Ok(()));
                                }
                                break;
                            }
                            Err(error) if is_retryable_transaction_error(&error) => {
                                tracing::warn!(
                                    error = %error,
                                    path = %thread_state.path.display(),
                                    operation = context.operation,
                                    table = context.table,
                                    mode = context.mode.as_str(),
                                    retry_delay_ms = retry_delay.as_millis(),
                                    "runtime db queued write retrying"
                                );
                                thread::sleep(retry_delay);
                                retry_delay = next_runtime_db_retry_delay(
                                    retry_delay,
                                    RUNTIME_DB_APPEND_RETRY_MAX_DELAY,
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    error = %error,
                                    path = %thread_state.path.display(),
                                    operation = context.operation,
                                    table = context.table,
                                    mode = context.mode.as_str(),
                                    "runtime db queued write failed"
                                );
                                #[cfg(any(test, feature = "test-support"))]
                                if let Some(completion) = completion.take() {
                                    let _ = completion.send(Err(error.to_string()));
                                }
                                break;
                            }
                        }
                    }
                }
            })
            .context("spawning runtime db writer thread")?;
        Ok(Self { state, append_tx })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn outstanding_writes_for_tests(&self) -> u64 {
        let queue = self.state.queue.state.lock().unwrap();
        queue.next_ticket - queue.serving_ticket
    }

    pub fn activate_sidecar_protection(&self) -> Result<()> {
        self.state.activate_sidecar_protection()
    }

    pub fn open_connection(&self) -> Result<Connection> {
        self.state.open_connection()
    }

    pub fn protection_status(&self) -> RuntimeDbProtectionStatus {
        self.state.protection_status()
    }

    pub fn state(&self) -> &Arc<RuntimeDbWriterState> {
        &self.state
    }

    pub fn workspace_context(&self) -> &RwLock<WorkspaceContextPublication> {
        &self.state.workspace_context
    }

    pub fn register_audit_frontier(&self, agent_id: &str) -> Result<Arc<AtomicU64>> {
        self.state.queue.register_audit_frontier(agent_id)
    }

    pub fn capability_policy_generation(&self) -> Option<u64> {
        let generation = self
            .state
            .queue
            .capability_policy_generation
            .load(Ordering::Acquire);
        (generation % 2 == 0).then_some(generation)
    }

    pub fn bump_delivery_summary_generation(&self) {
        self.state
            .queue
            .delivery_summary_generation
            .fetch_add(1, Ordering::Release);
    }

    pub fn delivery_summary_generation(&self) -> u64 {
        self.state
            .queue
            .delivery_summary_generation
            .load(Ordering::Acquire)
    }

    pub fn wait_turn(&self) -> Result<RuntimeDbWriteTurn> {
        self.state.queue.wait_turn()
    }

    pub fn connection_mutex(&self) -> &Mutex<Connection> {
        &self.state.connection
    }

    #[track_caller]
    pub fn append(
        &self,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.append_with_context(
            RuntimeDbWriteContext::background("runtime_db.append", "unknown"),
            f,
        )
    }

    #[track_caller]
    pub(crate) fn append_with_context(
        &self,
        context: RuntimeDbWriteContext,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let request = RuntimeDbWriteRequest {
            context: context.with_caller(),
            job: Box::new(f),
            #[cfg(any(test, feature = "test-support"))]
            completion: None,
        };
        match self.append_tx.try_send(request) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => bail!("runtime db write queue is full"),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                bail!("runtime db write queue is disconnected")
            }
        }
    }

    #[track_caller]
    pub fn append_wait<T>(&self, f: impl FnMut(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.append_wait_with_context(RuntimeDbWriteContext::TRANSACTION, f)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn flush_background_writes_for_tests(&self) -> Result<()> {
        let (completion_tx, completion_rx) = mpsc::channel();
        let request = RuntimeDbWriteRequest {
            context: RuntimeDbWriteContext::background("runtime_db.flush_for_tests", "unknown"),
            job: Box::new(|_| Ok(())),
            completion: Some(completion_tx),
        };
        self.append_tx
            .send(request)
            .map_err(|_| anyhow!("runtime db write queue is disconnected"))?;
        completion_rx
            .recv_timeout(Duration::from_secs(5))
            .context("timed out waiting for runtime db background writes to flush")?
            .map_err(|error| anyhow!(error))
    }

    #[track_caller]
    pub fn append_wait_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.append_wait_with_callback_phases(context, None, f)
    }

    #[track_caller]
    pub fn append_wait_with_callback_phases<T>(
        &self,
        context: RuntimeDbWriteContext,
        phases: Option<&crate::write_callback_phases::WriteCallbackPhases>,
        f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let context = context.with_caller();
        at_blocking_boundary(|| self.state.append_wait_with_context(context, phases, f))
    }

    #[track_caller]
    pub fn append_wait_once<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.append_wait_once_with_context(
            RuntimeDbWriteContext::sync("runtime_db.transaction_once", "unknown"),
            f,
        )
    }

    #[track_caller]
    pub fn append_wait_once_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.append_wait_once_with_callback_phases(context, None, f)
    }

    #[track_caller]
    pub fn append_wait_once_with_callback_phases<T>(
        &self,
        context: RuntimeDbWriteContext,
        phases: Option<&crate::write_callback_phases::WriteCallbackPhases>,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let context = context.with_caller();
        at_blocking_boundary(|| self.state.append_wait_once(context, phases, f))
    }
}

impl RuntimeDbWriterState {
    fn activate_sidecar_protection(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let guard = RuntimeDbSidecarGuard::acquire(&self.path)?;
            ensure_runtime_db_sidecars_are_consistent(&self.path, self.diagnostics)?;
            let mut slot = self
                .sidecar_guard
                .lock()
                .map_err(|_| anyhow!("runtime db sidecar guard mutex poisoned"))?;
            *slot = Some(guard);
        }
        let mut protection = self
            .protection
            .lock()
            .map_err(|_| anyhow!("runtime db protection mutex poisoned"))?;
        protection.state = if cfg!(target_os = "linux") {
            RuntimeDbProtectionState::Protected
        } else {
            RuntimeDbProtectionState::Unsupported
        };
        protection.evidence = None;
        Ok(())
    }

    fn protection_status(&self) -> RuntimeDbProtectionStatus {
        self.protection
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| RuntimeDbProtectionStatus {
                state: RuntimeDbProtectionState::Quarantined,
                evidence: Some("runtime db protection mutex poisoned".into()),
            })
    }

    pub(crate) fn ensure_protected(&self) -> Result<()> {
        let status = self.protection_status();
        match status.state {
            RuntimeDbProtectionState::Quarantined => {
                return Err(RuntimeDbProtectionError::new(
                    &self.path,
                    status
                        .evidence
                        .unwrap_or_else(|| "sidecar protection was lost".into()),
                )
                .into());
            }
            _ => {}
        }
        #[cfg(unix)]
        if let Err(error) = self.database_identity.ensure_matches(&self.path) {
            return Err(self.quarantine(error));
        }
        if status.state != RuntimeDbProtectionState::Protected {
            return Ok(());
        }
        if let Err(error) = ensure_runtime_db_sidecars_are_consistent(&self.path, self.diagnostics)
        {
            return Err(self.quarantine(error));
        }
        Ok(())
    }

    fn quarantine(&self, error: anyhow::Error) -> anyhow::Error {
        let incident = error
            .chain()
            .find_map(|source| source.downcast_ref::<RuntimeDbProtectionError>())
            .cloned()
            .unwrap_or_else(|| RuntimeDbProtectionError::new(&self.path, format!("{error:#}")));
        let mut protection = match self.protection.lock() {
            Ok(protection) => protection,
            Err(_) => return incident.into(),
        };
        if protection.state != RuntimeDbProtectionState::Quarantined {
            tracing::error!(
                error = %incident,
                path = %self.path.display(),
                "runtime db sidecar protection quarantined"
            );
            protection.state = RuntimeDbProtectionState::Quarantined;
            protection.evidence = Some(incident.evidence().to_string());
        }
        RuntimeDbProtectionError::new(
            &self.path,
            protection
                .evidence
                .clone()
                .unwrap_or_else(|| incident.evidence().to_string()),
        )
        .into()
    }

    pub(crate) fn open_connection(&self) -> Result<Connection> {
        self.ensure_protected()?;
        open_connection(&self.path, self.diagnostics).map_err(|error| {
            if error
                .chain()
                .any(|source| source.is::<RuntimeDbProtectionError>())
            {
                self.quarantine(error)
            } else {
                error
            }
        })
    }

    fn append_wait_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        phases: Option<&crate::write_callback_phases::WriteCallbackPhases>,
        mut f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let queue_wait_started_at = Instant::now();
        let _turn = self.queue.wait_turn().map_err(before_begin_failure)?;
        let queue_wait = queue_wait_started_at.elapsed();
        let mutex_wait_started_at = Instant::now();
        let connection = self
            .connection
            .lock()
            .map_err(|_| before_begin_failure(anyhow!("runtime db writer mutex poisoned")))?;
        let mutex_wait = mutex_wait_started_at.elapsed();
        let started_at = Instant::now();
        let mut retry_delay = RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY;
        let mut retry_count = 0u32;
        let mut next_warn_at = RUNTIME_DB_TRANSACTION_RETRY_WARN_INTERVAL;
        let (origin_file, origin_line) = context.origin();
        loop {
            self.ensure_protected().map_err(before_begin_failure)?;
            match self.run_transaction_attempt(
                &connection,
                context,
                queue_wait,
                mutex_wait,
                phases,
                |tx| f(tx),
            ) {
                Ok(value) => return Ok(value),
                Err(error) if is_retryable_transaction_error(&error) => {
                    retry_count += 1;
                    let elapsed = started_at.elapsed();
                    tracing::trace!(
                        error = %error,
                        path = %self.path.display(),
                        operation = context.operation,
                        table = context.table,
                        mode = context.mode.as_str(),
                        origin_file,
                        origin_line,
                        retry_count,
                        retry_delay_ms = retry_delay.as_millis(),
                        "runtime db transaction retrying"
                    );
                    if elapsed >= next_warn_at {
                        tracing::warn!(
                            error = %error,
                            path = %self.path.display(),
                            operation = context.operation,
                            table = context.table,
                            mode = context.mode.as_str(),
                            origin_file,
                            origin_line,
                            retry_count,
                            elapsed_ms = elapsed.as_millis(),
                            retry_delay_ms = retry_delay.as_millis(),
                            "runtime db transaction still locked"
                        );
                        next_warn_at += RUNTIME_DB_TRANSACTION_RETRY_WARN_INTERVAL;
                    }
                    thread::sleep(retry_delay);
                    retry_delay = next_runtime_db_retry_delay(
                        retry_delay,
                        RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY,
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn append_wait_once<T>(
        &self,
        context: RuntimeDbWriteContext,
        phases: Option<&crate::write_callback_phases::WriteCallbackPhases>,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let queue_wait_started_at = Instant::now();
        let _turn = self.queue.wait_turn().map_err(before_begin_failure)?;
        let queue_wait = queue_wait_started_at.elapsed();
        let mutex_wait_started_at = Instant::now();
        let connection = self
            .connection
            .lock()
            .map_err(|_| before_begin_failure(anyhow!("runtime db writer mutex poisoned")))?;
        let mutex_wait = mutex_wait_started_at.elapsed();
        self.ensure_protected().map_err(before_begin_failure)?;
        self.run_transaction_attempt(&connection, context, queue_wait, mutex_wait, phases, f)
    }

    fn run_transaction_attempt<T>(
        &self,
        connection: &Connection,
        context: RuntimeDbWriteContext,
        queue_wait: std::time::Duration,
        mutex_wait: std::time::Duration,
        phases: Option<&crate::write_callback_phases::WriteCallbackPhases>,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let observations = TransactionObservation::begin(&self.queue);
        let result = self.finish_transaction_attempt(
            connection,
            run_transaction_on_connection(
                connection, &self.path, context, queue_wait, mutex_wait, phases, f,
            ),
        );
        let mut writes = observations.finish();
        if let Err(error) = &result {
            if super::persistence::persistence_certainty(error)
                == Some(PersistenceCertainty::Indeterminate)
                && connection.is_autocommit()
            {
                // Recheck under the original write turn when COMMIT returned
                // an uncertain result. On a failed read, fence the older cut.
                for write in &mut writes {
                    if let Ok(latest) = connection.query_row(
                        "SELECT event_seq FROM audit_events WHERE agent_id=?1 ORDER BY event_seq DESC LIMIT 1",
                        [write.agent_id.as_ref()],
                        |row| row.get(0),
                    ).optional() {
                        write.sequence = latest.unwrap_or(0);
                    }
                }
            } else {
                writes.clear();
            }
        }
        for write in writes {
            write.frontier.fetch_max(write.sequence, Ordering::Release);
        }
        result
    }

    fn finish_transaction_attempt<T>(
        &self,
        connection: &Connection,
        result: Result<T>,
    ) -> Result<T> {
        if connection.is_autocommit() {
            return result;
        }
        let incident = self.quarantine(anyhow!(
            "runtime db writer still has an active transaction after transaction cleanup"
        ));
        // Preserve the attempt's original error and certainty while fencing this connection.
        match result {
            Err(error) => Err(error),
            Ok(_) => Err(mark_persistence_failure(
                incident,
                PersistenceCertainty::Indeterminate,
                PersistenceFailurePhase::Commit,
                None,
            )),
        }
    }
}

fn before_begin_failure(error: anyhow::Error) -> anyhow::Error {
    mark_persistence_failure(
        error,
        PersistenceCertainty::DefinitelyNotCommitted,
        PersistenceFailurePhase::BeforeBegin,
        None,
    )
}

thread_local! {
    // Original transactions run synchronously under the shared write turn.
    static TRANSACTION_WRITES: RefCell<Option<TransactionWrites>> = const { RefCell::new(None) };
}

struct TransactionWrites {
    queue: Arc<RuntimeDbWriteQueue>,
    audit: Vec<ObservedAuditWrite>,
    capability_policy: Option<CapabilityPolicyWrite>,
}

struct CapabilityPolicyWrite(Arc<RuntimeDbWriteQueue>);

impl Drop for CapabilityPolicyWrite {
    fn drop(&mut self) {
        self.0
            .capability_policy_generation
            .fetch_add(1, Ordering::Release);
    }
}

struct ObservedAuditWrite {
    agent_id: Arc<str>,
    frontier: Arc<AtomicU64>,
    sequence: u64,
}

struct TransactionObservation {
    previous: Option<TransactionWrites>,
    active: bool,
}

impl TransactionObservation {
    fn begin(queue: &Arc<RuntimeDbWriteQueue>) -> Self {
        Self {
            previous: TRANSACTION_WRITES.with(|writes| {
                writes.replace(Some(TransactionWrites {
                    queue: queue.clone(),
                    audit: Vec::new(),
                    capability_policy: None,
                }))
            }),
            active: true,
        }
    }

    fn finish(mut self) -> Vec<ObservedAuditWrite> {
        self.active = false;
        TRANSACTION_WRITES.with(|writes| {
            writes
                .replace(self.previous.take())
                .map_or_else(Vec::new, |writes| writes.audit)
        })
    }
}

impl Drop for TransactionObservation {
    fn drop(&mut self) {
        if self.active {
            TRANSACTION_WRITES.with(|writes| writes.replace(self.previous.take()));
        }
    }
}

pub fn record_audit_insert(agent_id: &str, sequence: u64) {
    TRANSACTION_WRITES.with(|writes| {
        let mut slot = writes.borrow_mut();
        let Some(writes) = slot.as_mut() else { return };
        if let Some(write) = writes
            .audit
            .iter_mut()
            .find(|write| write.agent_id.as_ref() == agent_id)
        {
            write.sequence = write.sequence.max(sequence);
            return;
        }
        // The writer retains registered handles and the sequence. Event body
        // admission and last-holder material stay with the original writer.
        let frontiers = writes
            .queue
            .audit_event_frontiers
            .lock()
            .expect("Audit frontiers poisoned");
        if let Some((agent_id, frontier)) = frontiers.get_key_value(agent_id) {
            if let Some(frontier) = frontier.upgrade() {
                writes.audit.push(ObservedAuditWrite {
                    agent_id: agent_id.clone(),
                    frontier,
                    sequence,
                });
            }
        }
    });
}

pub fn begin_capability_policy_write() {
    TRANSACTION_WRITES.with(|writes| {
        let mut slot = writes.borrow_mut();
        let Some(writes) = slot.as_mut() else { return };
        writes.capability_policy.get_or_insert_with(|| {
            // Readers must decline a cached policy before the first change can commit.
            writes
                .queue
                .capability_policy_generation
                .fetch_add(1, Ordering::AcqRel);
            CapabilityPolicyWrite(writes.queue.clone())
        });
    });
}

impl RuntimeDbWriteQueue {
    pub(super) fn register_audit_frontier(
        self: &Arc<Self>,
        agent_id: &str,
    ) -> Result<Arc<AtomicU64>> {
        // Registration precedes the bootstrap read. Any write already underway
        // completes before a new observer begins tracking its scope.
        let _turn = self.wait_turn()?;
        let mut frontiers = self
            .audit_event_frontiers
            .lock()
            .map_err(|_| anyhow!("Audit frontiers poisoned"))?;
        if let Some(frontier) = frontiers.get(agent_id).and_then(Weak::upgrade) {
            return Ok(frontier);
        }
        frontiers.retain(|_, frontier| frontier.strong_count() > 0);
        let frontier = Arc::new(AtomicU64::new(0));
        frontiers.insert(Arc::from(agent_id), Arc::downgrade(&frontier));
        Ok(frontier)
    }

    fn workspace_context(
        &self,
        #[cfg(unix)] database_identity: RuntimeDbFileIdentity,
    ) -> Result<Arc<RwLock<WorkspaceContextPublication>>> {
        let mut slot = self
            .workspace_context
            .lock()
            .map_err(|_| anyhow!("workspace context slot poisoned"))?;
        if let Some(shared) = slot.as_ref() {
            #[cfg(unix)]
            let same_database = shared.database_identity == database_identity;
            #[cfg(not(unix))]
            let same_database = true;
            if let Some(publication) = same_database
                .then(|| shared.publication.upgrade())
                .flatten()
            {
                return Ok(publication);
            }
        }
        let publication = Arc::new(RwLock::default());
        *slot = Some(SharedWorkspaceContext {
            #[cfg(unix)]
            database_identity,
            publication: Arc::downgrade(&publication),
        });
        Ok(publication)
    }

    pub(crate) fn wait_turn(self: &Arc<Self>) -> Result<RuntimeDbWriteTurn> {
        let ticket = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("runtime db write queue mutex poisoned"))?;
            let ticket = state.next_ticket;
            state.next_ticket = state
                .next_ticket
                .checked_add(1)
                .context("runtime db write queue ticket overflow")?;
            ticket
        };

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("runtime db write queue mutex poisoned"))?;
        while state.serving_ticket != ticket {
            state = self
                .available
                .wait(state)
                .map_err(|_| anyhow!("runtime db write queue mutex poisoned"))?;
        }
        Ok(RuntimeDbWriteTurn {
            queue: Arc::clone(self),
        })
    }
}

impl Drop for RuntimeDbWriteTurn {
    fn drop(&mut self) {
        if let Ok(mut state) = self.queue.state.lock() {
            state.serving_ticket = state.serving_ticket.saturating_add(1);
            self.queue.available.notify_all();
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl RuntimeDbWriterState {
    /// Write-channel admissions (queue tickets issued) so far. Tests assert
    /// that a request path never entered the single writer channel by
    /// comparing this counter before and after the call.
    pub fn write_channel_admissions_for_tests(&self) -> Result<u64> {
        let state = self
            .queue
            .state
            .lock()
            .map_err(|_| anyhow!("runtime db write queue mutex poisoned"))?;
        Ok(state.next_ticket)
    }
}

fn runtime_db_write_queue(path: &Path) -> Result<Arc<RuntimeDbWriteQueue>> {
    let key = runtime_db_write_queue_key(path);
    let queues = RUNTIME_DB_WRITE_QUEUES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut queues = queues
        .lock()
        .map_err(|_| anyhow!("runtime db write queues mutex poisoned"))?;
    Ok(Arc::clone(queues.entry(key).or_insert_with(|| {
        Arc::new(RuntimeDbWriteQueue {
            state: Mutex::new(RuntimeDbWriteQueueState::default()),
            available: Condvar::new(),
            delivery_summary_generation: AtomicU64::new(0),
            capability_policy_generation: AtomicU64::new(0),
            audit_event_frontiers: Mutex::default(),
            workspace_context: Mutex::default(),
        })
    })))
}

fn runtime_db_write_queue_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => parent
            .canonicalize()
            .map(|parent| parent.join(file_name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod persistence_tests;

#[cfg(all(test, target_os = "linux"))]
mod tests;
