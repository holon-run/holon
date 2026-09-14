//! Background writer task, write queue, and write context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::thread;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{Connection, Transaction};

#[cfg(target_os = "linux")]
use crate::runtime_db::connection::RuntimeDbSidecarGuard;
use crate::runtime_db::connection::{
    ensure_runtime_db_sidecars_are_consistent, is_retryable_db_error, next_runtime_db_retry_delay,
    open_connection, run_transaction_on_connection,
};
use crate::runtime_db::{
    RuntimeDbProtectionError, RuntimeDbProtectionState, RuntimeDbProtectionStatus,
    RUNTIME_DB_APPEND_RETRY_MAX_DELAY, RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY,
    RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY, RUNTIME_DB_TRANSACTION_RETRY_WARN_INTERVAL,
    RUNTIME_DB_WRITE_QUEUE_CAPACITY,
};

#[derive(Clone)]
pub(crate) struct RuntimeDbWriter {
    pub(crate) state: Arc<RuntimeDbWriterState>,
    pub(crate) append_tx: mpsc::SyncSender<RuntimeDbWriteRequest>,
}

pub(crate) struct RuntimeDbWriterState {
    pub(crate) path: PathBuf,
    pub(crate) connection: Mutex<Connection>,
    pub(crate) queue: Arc<RuntimeDbWriteQueue>,
    #[cfg(target_os = "linux")]
    pub(crate) sidecar_guard: Mutex<Option<RuntimeDbSidecarGuard>>,
    protection: Mutex<RuntimeDbProtectionStatus>,
}

pub(crate) struct RuntimeDbWriteQueue {
    pub(crate) state: Mutex<RuntimeDbWriteQueueState>,
    pub(crate) available: Condvar,
}

#[derive(Default)]
pub(crate) struct RuntimeDbWriteQueueState {
    next_ticket: u64,
    serving_ticket: u64,
}

pub(crate) struct RuntimeDbWriteTurn {
    pub(crate) queue: Arc<RuntimeDbWriteQueue>,
}

pub(crate) type RuntimeDbWriteJob =
    Box<dyn for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static>;

pub(crate) struct RuntimeDbWriteRequest {
    context: RuntimeDbWriteContext,
    job: RuntimeDbWriteJob,
    #[cfg(test)]
    completion: Option<mpsc::Sender<Result<(), String>>>,
}

#[derive(Clone, Copy)]
pub(crate) struct RuntimeDbWriteContext {
    pub(crate) operation: &'static str,
    pub(crate) table: &'static str,
    pub(crate) mode: RuntimeDbWriteMode,
}

impl RuntimeDbWriteContext {
    pub(crate) const fn new(
        operation: &'static str,
        table: &'static str,
        mode: RuntimeDbWriteMode,
    ) -> Self {
        Self {
            operation,
            table,
            mode,
        }
    }

    pub(crate) const fn sync(operation: &'static str, table: &'static str) -> Self {
        Self::new(operation, table, RuntimeDbWriteMode::Sync)
    }

    pub(crate) const fn background(operation: &'static str, table: &'static str) -> Self {
        Self::new(operation, table, RuntimeDbWriteMode::Background)
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
    #[allow(dead_code)]
    pub(crate) fn open(path: PathBuf, connection: Connection) -> Result<Self> {
        let writer = Self::open_starting(path, connection)?;
        writer.activate_sidecar_protection()?;
        Ok(writer)
    }

    pub(crate) fn open_starting(path: PathBuf, connection: Connection) -> Result<Self> {
        let queue = runtime_db_write_queue(&path)?;
        let state = Arc::new(RuntimeDbWriterState {
            path,
            connection: Mutex::new(connection),
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
                        #[cfg(test)]
                        mut completion,
                    } = request;
                    let mut retry_delay = RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY;
                    loop {
                        match thread_state.append_wait_with_context(context, |tx| job(tx)) {
                            Ok(()) => {
                                #[cfg(test)]
                                if let Some(completion) = completion.take() {
                                    let _ = completion.send(Ok(()));
                                }
                                break;
                            }
                            Err(error) if is_retryable_db_error(&error) => {
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
                                #[cfg(test)]
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

    pub(crate) fn activate_sidecar_protection(&self) -> Result<()> {
        self.state.activate_sidecar_protection()
    }

    pub(crate) fn open_connection(&self) -> Result<Connection> {
        self.state.open_connection()
    }

    pub(crate) fn protection_status(&self) -> RuntimeDbProtectionStatus {
        self.state.protection_status()
    }

    pub(crate) fn append(
        &self,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.append_with_context(
            RuntimeDbWriteContext::background("runtime_db.append", "unknown"),
            f,
        )
    }

    pub(crate) fn append_with_context(
        &self,
        context: RuntimeDbWriteContext,
        f: impl for<'transaction> Fn(&Transaction<'transaction>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let request = RuntimeDbWriteRequest {
            context,
            job: Box::new(f),
            #[cfg(test)]
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

    pub(crate) fn append_wait<T>(&self, f: impl FnMut(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.state.append_wait(f)
    }

    #[cfg(test)]
    pub(crate) fn flush_background_writes_for_tests(&self) -> Result<()> {
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

    pub(crate) fn append_wait_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.state.append_wait_with_context(context, f)
    }

    pub(crate) fn append_wait_once<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.state.append_wait_once(f)
    }
}

impl RuntimeDbWriterState {
    fn activate_sidecar_protection(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let guard = RuntimeDbSidecarGuard::acquire(&self.path)?;
            ensure_runtime_db_sidecars_are_consistent(&self.path)?;
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

    fn ensure_protected(&self) -> Result<()> {
        let status = self.protection_status();
        match status.state {
            RuntimeDbProtectionState::Starting | RuntimeDbProtectionState::Unsupported => {
                return Ok(());
            }
            RuntimeDbProtectionState::Quarantined => {
                return Err(RuntimeDbProtectionError::new(
                    &self.path,
                    status
                        .evidence
                        .unwrap_or_else(|| "sidecar protection was lost".into()),
                )
                .into());
            }
            RuntimeDbProtectionState::Protected => {}
        }
        if let Err(error) = ensure_runtime_db_sidecars_are_consistent(&self.path) {
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

    fn open_connection(&self) -> Result<Connection> {
        self.ensure_protected()?;
        open_connection(&self.path).map_err(|error| {
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

    fn append_wait<T>(&self, f: impl FnMut(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.append_wait_with_context(
            RuntimeDbWriteContext::sync("runtime_db.transaction", "unknown"),
            f,
        )
    }

    fn append_wait_with_context<T>(
        &self,
        context: RuntimeDbWriteContext,
        mut f: impl FnMut(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let queue_wait_started_at = Instant::now();
        let _turn = self.queue.wait_turn()?;
        let queue_wait = queue_wait_started_at.elapsed();
        let mutex_wait_started_at = Instant::now();
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("runtime db writer mutex poisoned"))?;
        let mutex_wait = mutex_wait_started_at.elapsed();
        let started_at = Instant::now();
        let mut retry_delay = RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY;
        let mut retry_count = 0u32;
        let mut next_warn_at = RUNTIME_DB_TRANSACTION_RETRY_WARN_INTERVAL;
        loop {
            self.ensure_protected()?;
            match run_transaction_on_connection(
                &connection,
                &self.path,
                context,
                queue_wait,
                mutex_wait,
                |tx| f(tx),
            ) {
                Ok(value) => return Ok(value),
                Err(error) if is_retryable_db_error(&error) => {
                    retry_count += 1;
                    let elapsed = started_at.elapsed();
                    tracing::trace!(
                        error = %error,
                        path = %self.path.display(),
                        operation = context.operation,
                        table = context.table,
                        mode = context.mode.as_str(),
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

    fn append_wait_once<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let queue_wait_started_at = Instant::now();
        let _turn = self.queue.wait_turn()?;
        let queue_wait = queue_wait_started_at.elapsed();
        let mutex_wait_started_at = Instant::now();
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("runtime db writer mutex poisoned"))?;
        let mutex_wait = mutex_wait_started_at.elapsed();
        self.ensure_protected()?;
        run_transaction_on_connection(
            &connection,
            &self.path,
            RuntimeDbWriteContext::sync("runtime_db.transaction_once", "unknown"),
            queue_wait,
            mutex_wait,
            f,
        )
    }
}

impl RuntimeDbWriteQueue {
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::runtime_db::connection::configure_persistent_database;

    fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
        let mut path = db_path.as_os_str().to_owned();
        path.push(suffix);
        PathBuf::from(path)
    }

    #[test]
    fn sidecar_divergence_quarantines_writer_without_reconcile() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let connection = open_connection(&db_path)?;
        configure_persistent_database(&connection)?;
        connection.execute_batch(
            "CREATE TABLE values_seen(value INTEGER NOT NULL);
             INSERT INTO values_seen(value) VALUES (1);",
        )?;
        let writer = RuntimeDbWriter::open(db_path.clone(), connection)?;
        let wal_path = sidecar_path(&db_path, "-wal");

        fs::remove_file(&wal_path)?;
        let first_error = writer
            .append_wait_once::<()>(|_| panic!("quarantined writer must not begin a transaction"))
            .expect_err("deleted-open WAL must quarantine the writer");
        assert!(first_error
            .chain()
            .any(|source| source.is::<RuntimeDbProtectionError>()));
        let first_status = writer.protection_status();
        assert_eq!(first_status.state, RuntimeDbProtectionState::Quarantined);
        assert!(first_status
            .evidence
            .as_deref()
            .is_some_and(|evidence| evidence.contains("deleted-open sidecar")));
        assert!(!wal_path.exists(), "quarantine must not recreate the WAL");

        let second_error = writer
            .open_connection()
            .expect_err("quarantined writer must reject new connections");
        assert!(second_error
            .chain()
            .any(|source| source.is::<RuntimeDbProtectionError>()));
        assert_eq!(writer.protection_status(), first_status);
        assert!(!wal_path.exists(), "quarantine must remain fail-stop");
        Ok(())
    }
}
