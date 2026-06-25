//! SQLite connection setup, transaction retry, and file locking.

use std::fs::{self, File};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(unix))]
use anyhow::bail;
use anyhow::{anyhow, Context, Result};
use rusqlite::{ffi::ErrorCode, Connection, Transaction, TransactionBehavior};

use crate::runtime_db::write_queue::RuntimeDbWriteContext;
use crate::runtime_db::{
    RuntimeDbRetryableError, RUNTIME_DB_BEGIN_RETRY_WARN_INTERVAL, RUNTIME_DB_BUSY_TIMEOUT,
    RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY, RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY,
};

pub(crate) enum LockMode {
    Blocking,
    NonBlocking,
}

pub(crate) fn open_connection(path: &Path) -> Result<Connection> {
    let started_at = Instant::now();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
    }
    let connection =
        Connection::open(path).with_context(|| format!("opening runtime db {}", path.display()))?;
    configure_connection(&connection)?;
    crate::diagnostics::record_runtime_db_connection_open(started_at.elapsed());
    Ok(connection)
}

pub(crate) fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(RUNTIME_DB_BUSY_TIMEOUT)?;
    connection.execute_batch(
        r#"
PRAGMA foreign_keys = ON;
"#,
    )?;
    Ok(())
}

pub(crate) fn configure_persistent_database(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
PRAGMA journal_mode = WAL;
"#,
    )?;
    Ok(())
}

pub(crate) fn run_transaction_on_connection<T>(
    connection: &Connection,
    path: &Path,
    context: RuntimeDbWriteContext,
    queue_wait: Duration,
    mutex_wait: Duration,
    f: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let started_at = Instant::now();
    tracing::trace!(
        path = %path.display(),
        operation = context.operation,
        table = context.table,
        mode = context.mode.as_str(),
        queue_wait_ms = queue_wait.as_millis(),
        mutex_wait_ms = mutex_wait.as_millis(),
        "runtime db write starting"
    );
    let (transaction, begin_retry_count, begin_wait) =
        begin_immediate_transaction_with_retry(connection, path)?;
    match f(&transaction) {
        Ok(value) => {
            transaction.commit().map_err(|error| {
                map_runtime_db_sqlite_error("committing transaction", path, error)
            })?;
            let elapsed = started_at.elapsed();
            tracing::trace!(
                path = %path.display(),
                operation = context.operation,
                table = context.table,
                mode = context.mode.as_str(),
                queue_wait_ms = queue_wait.as_millis(),
                mutex_wait_ms = mutex_wait.as_millis(),
                begin_wait_ms = begin_wait.as_millis(),
                begin_retry_count,
                elapsed_ms = elapsed.as_millis(),
                "runtime db write committed"
            );
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback();
            let elapsed = started_at.elapsed();
            tracing::warn!(
                error = %error,
                retryable = is_retryable_db_error(&error),
                path = %path.display(),
                operation = context.operation,
                table = context.table,
                mode = context.mode.as_str(),
                queue_wait_ms = queue_wait.as_millis(),
                mutex_wait_ms = mutex_wait.as_millis(),
                begin_wait_ms = begin_wait.as_millis(),
                begin_retry_count,
                elapsed_ms = elapsed.as_millis(),
                "runtime db write rolled back"
            );
            Err(error)
        }
    }
}

pub(crate) fn begin_immediate_transaction_with_retry<'connection>(
    connection: &'connection Connection,
    path: &Path,
) -> Result<(Transaction<'connection>, u32, Duration)> {
    let started_at = Instant::now();
    let mut retry_delay = RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY;
    let mut retry_count = 0;
    let mut next_warn_at = RUNTIME_DB_BEGIN_RETRY_WARN_INTERVAL;
    loop {
        // The writer mutex prevents concurrent transactions on this connection;
        // the retry loop absorbs transient locks from external processes or connections.
        match Transaction::new_unchecked(connection, TransactionBehavior::Immediate) {
            Ok(transaction) => return Ok((transaction, retry_count, started_at.elapsed())),
            Err(error) if is_sqlite_locked(&error) => {
                retry_count += 1;
                let elapsed = started_at.elapsed();
                tracing::trace!(
                    error = %error,
                    path = %path.display(),
                    retry_count,
                    retry_delay_ms = retry_delay.as_millis(),
                    "runtime db begin immediate transaction retrying"
                );
                if elapsed >= next_warn_at {
                    tracing::warn!(
                        error = %error,
                        path = %path.display(),
                        retry_count,
                        elapsed_ms = elapsed.as_millis(),
                        retry_delay_ms = retry_delay.as_millis(),
                        "runtime db begin immediate transaction still locked"
                    );
                    next_warn_at += RUNTIME_DB_BEGIN_RETRY_WARN_INTERVAL;
                }
                thread::sleep(retry_delay);
                retry_delay = next_runtime_db_retry_delay(
                    retry_delay,
                    RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY,
                );
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "starting immediate runtime db transaction for {}",
                        path.display()
                    )
                });
            }
        }
    }
}

pub(crate) fn next_runtime_db_retry_delay(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

pub(crate) fn map_runtime_db_sqlite_error(
    operation: &'static str,
    path: &Path,
    error: rusqlite::Error,
) -> anyhow::Error {
    if is_sqlite_locked(&error) {
        RuntimeDbRetryableError::new(operation, path, error).into()
    } else {
        anyhow!(error).context(format!("{} for {}", operation, path.display()))
    }
}

pub fn is_sqlite_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
}

/// Check if an error is retryable (retryable DB error or SQLite locked).
pub fn is_retryable_db_error(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
            .is_some()
            || source
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(is_sqlite_locked)
    })
}

#[cfg(unix)]
pub(crate) fn flock(file: &File, mode: LockMode) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut operation = libc::LOCK_EX;
    if matches!(mode, LockMode::NonBlocking) {
        operation |= libc::LOCK_NB;
    }
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if matches!(mode, LockMode::NonBlocking) {
        if let Some(raw_error) = error.raw_os_error() {
            if raw_error == libc::EWOULDBLOCK || raw_error == libc::EAGAIN {
                return Err(anyhow!("runtime db lock is already held"));
            }
        }
    }
    Err(error.into())
}

#[cfg(unix)]
pub(crate) fn unlock(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(not(unix))]
pub(crate) fn flock(_file: &File, _mode: LockMode) -> Result<()> {
    bail!("runtime db file lock is only implemented on Unix platforms")
}

#[cfg(not(unix))]
pub(crate) fn unlock(_file: &File) -> Result<()> {
    Ok(())
}
