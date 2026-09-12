//! SQLite connection setup, transaction retry, and file locking.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};
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
    use crate::diagnostics::attribution;
    let _attempt = attribution::CONNECTION.start();
    let started_at = Instant::now();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
    }
    ensure_runtime_db_sidecars_are_consistent(path)?;
    let sqlite_open = attribution::SQLITE_OPEN.start();
    let connection =
        Connection::open(path).with_context(|| format!("opening runtime db {}", path.display()))?;
    drop(sqlite_open);
    let configure = attribution::CONFIGURE.start();
    enable_persistent_wal(&connection, path)?;
    configure_connection(&connection)?;
    drop(configure);
    ensure_runtime_db_sidecars_are_consistent(path)?;
    crate::diagnostics::record_runtime_db_connection_open(started_at.elapsed());
    Ok(connection)
}

fn enable_persistent_wal(connection: &Connection, path: &Path) -> Result<()> {
    const MAIN_SCHEMA: &[u8] = b"main\0";

    let mut enabled = 1_i32;
    // SAFETY: `connection.handle()` remains valid for this call, `MAIN_SCHEMA` is
    // nul-terminated, and SQLite only reads/writes `enabled` before returning.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            MAIN_SCHEMA.as_ptr().cast(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut enabled as *mut i32).cast(),
        )
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(anyhow!(rusqlite::ffi::Error::new(result))).with_context(|| {
            format!(
                "enabling persistent WAL lifecycle for runtime db {}",
                path.display()
            )
        });
    }

    let mut current = -1_i32;
    // SAFETY: same pointer and connection lifetime guarantees as the setting
    // call above. A value of -1 queries the current file-control setting.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            MAIN_SCHEMA.as_ptr().cast(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut current as *mut i32).cast(),
        )
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(anyhow!(rusqlite::ffi::Error::new(result))).with_context(|| {
            format!(
                "verifying persistent WAL lifecycle for runtime db {}",
                path.display()
            )
        });
    }
    if current != 1 {
        bail_persistent_wal_not_enabled(path, current)?;
    }
    Ok(())
}

fn bail_persistent_wal_not_enabled(path: &Path, current: i32) -> Result<()> {
    anyhow::bail!(
        "persistent WAL lifecycle is not enabled for runtime db {}: file-control value {current}",
        path.display()
    )
}

#[cfg(target_os = "linux")]
fn ensure_runtime_db_sidecars_are_consistent(path: &Path) -> Result<()> {
    ensure_runtime_db_sidecars_are_consistent_with(path, inspect_runtime_db_sidecar_fd)
}

#[cfg(target_os = "linux")]
fn ensure_runtime_db_sidecars_are_consistent_with(
    path: &Path,
    inspect_fd: impl Fn(&Path, &Path, &str) -> Result<Option<RuntimeDbSidecarOpen>>,
) -> Result<()> {
    let _timer = crate::diagnostics::attribution::SIDECAR.start();
    let db_path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .canonicalize()
                .with_context(|| {
                    format!(
                        "resolving runtime db directory for sidecar inspection: {}",
                        path.display()
                    )
                })?;
            let file_name = path.file_name().ok_or_else(|| {
                anyhow!(
                    "runtime db path has no file name for sidecar inspection: {}",
                    path.display()
                )
            })?;
            parent.join(file_name)
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "resolving runtime db path for sidecar inspection: {}",
                    path.display()
                )
            });
        }
    };

    let identities = runtime_db_sidecar_identities(&db_path)?;
    if verified_runtime_db_sidecars(&db_path)
        .is_some_and(|verified| verified.identities == identities)
    {
        return Ok(());
    }

    let scan_start = Instant::now();
    for suffix in ["-wal", "-shm"] {
        ensure_runtime_db_sidecar_is_consistent_with(&db_path, suffix, &inspect_fd)?;
    }
    crate::diagnostics::record_runtime_db_sidecar_consistency_scan(scan_start.elapsed());
    remember_verified_runtime_db_sidecars(&db_path, identities);
    Ok(())
}

#[cfg(target_os = "linux")]
fn runtime_db_sidecar_identity(path: &Path) -> Result<Option<(u64, u64)>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some((metadata.dev(), metadata.ino()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("reading runtime db sidecar metadata: {}", path.display())),
    }
}

/// Canonical `-wal`/`-shm` identities for one runtime db path.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeDbSidecarIdentities {
    wal: Option<(u64, u64)>,
    shm: Option<(u64, u64)>,
}

/// Identities verified by the last full fd-table scan for one db path, plus
/// how many scans that path has needed.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
struct VerifiedRuntimeDbSidecars {
    identities: RuntimeDbSidecarIdentities,
    fd_scans: u64,
}

// Sidecar identities verified by a full `/proc/self/fd` scan, kept for the
// process lifetime per canonical db path. Connection opens re-stat the
// canonical sidecars and skip the scan while identities are unchanged;
// replacing or deleting a sidecar changes identities and re-triggers the
// scan, so the #2850 fail-closed divergence detection is preserved without
// O(process FDs) work on every connection open (#2888).
#[cfg(target_os = "linux")]
static VERIFIED_RUNTIME_DB_SIDECARS: OnceLock<Mutex<HashMap<PathBuf, VerifiedRuntimeDbSidecars>>> =
    OnceLock::new();

#[cfg(target_os = "linux")]
fn runtime_db_sidecar_file_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(target_os = "linux")]
fn runtime_db_sidecar_identities(db_path: &Path) -> Result<RuntimeDbSidecarIdentities> {
    Ok(RuntimeDbSidecarIdentities {
        wal: runtime_db_sidecar_identity(&runtime_db_sidecar_file_path(db_path, "-wal"))?,
        shm: runtime_db_sidecar_identity(&runtime_db_sidecar_file_path(db_path, "-shm"))?,
    })
}

#[cfg(target_os = "linux")]
fn verified_runtime_db_sidecars(db_path: &Path) -> Option<VerifiedRuntimeDbSidecars> {
    let map = VERIFIED_RUNTIME_DB_SIDECARS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?;
    map.get(db_path).cloned()
}

#[cfg(target_os = "linux")]
fn remember_verified_runtime_db_sidecars(
    db_path: &Path,
    identities: RuntimeDbSidecarIdentities,
) -> u64 {
    let Ok(mut map) = VERIFIED_RUNTIME_DB_SIDECARS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        // A poisoned lock only costs a redundant rescan on the next open.
        return 0;
    };
    let entry = map
        .entry(db_path.to_path_buf())
        .and_modify(|entry| entry.fd_scans += 1)
        .or_insert(VerifiedRuntimeDbSidecars {
            identities: identities.clone(),
            fd_scans: 1,
        });
    entry.identities = identities;
    entry.fd_scans
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeDbSidecarOpen {
    deleted: bool,
    identity: (u64, u64),
}

#[cfg(target_os = "linux")]
fn inspect_runtime_db_sidecar_fd(
    fd_path: &Path,
    sidecar_path: &Path,
    deleted_path: &str,
) -> Result<Option<RuntimeDbSidecarOpen>> {
    inspect_runtime_db_sidecar_fd_with_hook(fd_path, sidecar_path, deleted_path, || Ok(()))
}

#[cfg(target_os = "linux")]
fn inspect_runtime_db_sidecar_fd_with_hook(
    fd_path: &Path,
    sidecar_path: &Path,
    deleted_path: &str,
    after_first_target: impl FnOnce() -> Result<()>,
) -> Result<Option<RuntimeDbSidecarOpen>> {
    inspect_runtime_db_sidecar_fd_with_hook_and_open(
        fd_path,
        sidecar_path,
        deleted_path,
        after_first_target,
        open_runtime_db_fd_metadata_handle,
    )
}

#[cfg(target_os = "linux")]
fn inspect_runtime_db_sidecar_fd_with_hook_and_open(
    fd_path: &Path,
    sidecar_path: &Path,
    deleted_path: &str,
    after_first_target: impl FnOnce() -> Result<()>,
    open_stable: impl FnOnce(&Path) -> std::io::Result<File>,
) -> Result<Option<RuntimeDbSidecarOpen>> {
    let observed_target = match fs::read_link(fd_path) {
        Ok(target) => target,
        Err(_) => return Ok(None),
    };
    let observed_deleted = observed_target.to_string_lossy() == deleted_path;
    if observed_target != sidecar_path && !observed_deleted {
        return Ok(None);
    }

    after_first_target()?;

    let stable_file = match open_stable(fd_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "opening runtime db fd metadata handle: {}",
                    fd_path.display()
                )
            });
        }
    };
    let stable_fd_path = Path::new("/proc/self/fd").join(stable_file.as_raw_fd().to_string());
    let stable_target = fs::read_link(&stable_fd_path).with_context(|| {
        format!(
            "reading stable runtime db fd metadata target: {}",
            stable_fd_path.display()
        )
    })?;
    if stable_target != observed_target {
        return Ok(None);
    }
    let stable_metadata = stable_file.metadata().with_context(|| {
        format!(
            "reading stable runtime db fd metadata: {}",
            stable_fd_path.display()
        )
    })?;
    if !stable_metadata.is_file() {
        return Ok(None);
    }

    Ok(Some(RuntimeDbSidecarOpen {
        deleted: stable_target.to_string_lossy() == deleted_path,
        identity: (stable_metadata.dev(), stable_metadata.ino()),
    }))
}

#[cfg(target_os = "linux")]
fn open_runtime_db_fd_metadata_handle(fd_path: &Path) -> std::io::Result<File> {
    // O_PATH pins the observed object without participating in POSIX record locks.
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(fd_path)
}

#[cfg(target_os = "linux")]
fn ensure_runtime_db_sidecar_is_consistent_with(
    db_path: &Path,
    suffix: &str,
    inspect_fd: &impl Fn(&Path, &Path, &str) -> Result<Option<RuntimeDbSidecarOpen>>,
) -> Result<()> {
    let sidecar_path = runtime_db_sidecar_file_path(db_path, suffix);
    let deleted_path = format!("{} (deleted)", sidecar_path.display());

    let fd_entries = fs::read_dir("/proc/self/fd")
        .context("reading /proc/self/fd")?
        .collect::<std::io::Result<Vec<_>>>()
        .context("reading entries from /proc/self/fd")?;
    for entry in fd_entries {
        let canonical_before = runtime_db_sidecar_identity(&sidecar_path)?;
        let Some(open) = inspect_fd(&entry.path(), &sidecar_path, &deleted_path)? else {
            continue;
        };
        let canonical_after = runtime_db_sidecar_identity(&sidecar_path)?;
        let fd = entry.file_name().to_string_lossy().into_owned();

        if canonical_before != canonical_after {
            return runtime_db_sidecar_divergence(
                db_path,
                suffix,
                &fd,
                open.identity,
                canonical_after,
                "canonical sidecar changed during inspection",
            );
        }
        if open.deleted {
            return runtime_db_sidecar_divergence(
                db_path,
                suffix,
                &fd,
                open.identity,
                canonical_after,
                "deleted-open sidecar",
            );
        }
        if canonical_after.is_some_and(|identity| identity != open.identity) {
            return runtime_db_sidecar_divergence(
                db_path,
                suffix,
                &fd,
                open.identity,
                canonical_after,
                "open/canonical inode mismatch",
            );
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn runtime_db_sidecar_divergence(
    db_path: &Path,
    suffix: &str,
    fd: &str,
    open_identity: (u64, u64),
    canonical_identity: Option<(u64, u64)>,
    reason: &str,
) -> Result<()> {
    let error = anyhow!(
        "runtime db sidecar divergence detected ({reason}); refusing a new connection: db={}, sidecar={suffix}, fd={fd}, open_dev={}, open_inode={}, canonical_identity={canonical_identity:?}; preserve the files and FD/inode evidence, then perform offline recovery or restart",
        db_path.display(),
        open_identity.0,
        open_identity.1,
    );
    tracing::error!(error = %error, "runtime db sidecar divergence");
    Err(error)
}

#[cfg(not(target_os = "linux"))]
fn ensure_runtime_db_sidecars_are_consistent(_path: &Path) -> Result<()> {
    Ok(())
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
PRAGMA synchronous = NORMAL;
PRAGMA wal_autocheckpoint = 10000;
PRAGMA mmap_size = 268435456;
"#,
    )?;
    Ok(())
}

pub(crate) fn configure_new_database_auto_vacuum(connection: &Connection) -> Result<()> {
    let application_tables: u64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if application_tables == 0 {
        connection.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use rusqlite::OpenFlags;
    use std::ffi::OsString;
    #[cfg(target_os = "linux")]
    use std::os::fd::IntoRawFd;
    #[cfg(target_os = "linux")]
    use std::process::{Command, Stdio};
    use tempfile::tempdir;

    #[cfg(target_os = "linux")]
    const OBSERVER_CHILD_DB_ENV: &str = "HOLON_RUNTIME_DB_OBSERVER_CHILD_DB";
    #[cfg(target_os = "linux")]
    const OBSERVER_CHILD_READY_ENV: &str = "HOLON_RUNTIME_DB_OBSERVER_CHILD_READY";
    #[cfg(target_os = "linux")]
    const OBSERVER_CHILD_RELEASE_ENV: &str = "HOLON_RUNTIME_DB_OBSERVER_CHILD_RELEASE";
    #[cfg(target_os = "linux")]
    const OBSERVER_TEST_NAME: &str =
        "runtime_db::connection::tests::external_observer_does_not_replace_runtime_db_sidecars";
    #[cfg(target_os = "linux")]
    const LOCK_PROBE_PATH_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_PATH";
    #[cfg(target_os = "linux")]
    const LOCK_PROBE_READY_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_READY";
    #[cfg(target_os = "linux")]
    const LOCK_PROBE_START_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_START";
    #[cfg(target_os = "linux")]
    const LOCK_PROBE_LEN_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_LEN";
    #[cfg(target_os = "linux")]
    const LOCK_PROBE_EXPECT_ENV: &str = "HOLON_RUNTIME_DB_LOCK_PROBE_EXPECT";
    #[cfg(target_os = "linux")]
    const LOCK_PROBE_TEST_NAME: &str =
        "runtime_db::connection::tests::runtime_db_posix_lock_probe_child";

    fn persistent_wal_setting(connection: &Connection) -> Result<i32> {
        const MAIN_SCHEMA: &[u8] = b"main\0";
        let mut current = -1_i32;
        // SAFETY: the connection remains alive for the call, the schema name is
        // nul-terminated, and SQLite writes the result before returning.
        let result = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(),
                MAIN_SCHEMA.as_ptr().cast(),
                rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
                (&mut current as *mut i32).cast(),
            )
        };
        if result != rusqlite::ffi::SQLITE_OK {
            return Err(anyhow!(rusqlite::ffi::Error::new(result)));
        }
        Ok(current)
    }

    fn sidecar_path(db_path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut path: OsString = db_path.as_os_str().to_owned();
        path.push(suffix);
        path.into()
    }

    #[cfg(target_os = "linux")]
    fn try_posix_write_lock(file: &File, start: i64, len: i64) -> std::io::Result<()> {
        // SAFETY: flock is initialized before the fcntl call and the file remains open.
        let mut lock = unsafe { std::mem::zeroed::<libc::flock>() };
        lock.l_type = libc::F_WRLCK as libc::c_short;
        lock.l_whence = libc::SEEK_SET as libc::c_short;
        lock.l_start = start as libc::off_t;
        lock.l_len = len as libc::off_t;
        // SAFETY: the descriptor and flock pointer remain valid for the call.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) } == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_for_child_ready(
        mut child: std::process::Child,
        ready_path: &Path,
        label: &str,
    ) -> Result<std::process::Child> {
        let started_at = Instant::now();
        while !ready_path.exists() {
            if child.try_wait()?.is_some() {
                let output = child.wait_with_output()?;
                anyhow::bail!(
                    "{label} exited before ready: status={}; stdout={}; stderr={}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            if started_at.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let output = child.wait_with_output()?;
                anyhow::bail!(
                    "timed out waiting for {label}: stdout={}; stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    fn wait_for_child_exit(
        mut child: std::process::Child,
        label: &str,
    ) -> Result<std::process::Output> {
        let started_at = Instant::now();
        loop {
            if child.try_wait()?.is_some() {
                return child
                    .wait_with_output()
                    .with_context(|| format!("collecting {label} output"));
            }
            if started_at.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let output = child.wait_with_output()?;
                anyhow::bail!(
                    "timed out waiting for {label} to exit: stdout={}; stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    fn assert_lock_conflict_from_child(path: &Path, start: i64, len: i64) -> Result<()> {
        let ready_directory = tempdir()?;
        let ready_path = ready_directory.path().join("ready");
        let child = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg(LOCK_PROBE_TEST_NAME)
            .arg("--nocapture")
            .env(LOCK_PROBE_PATH_ENV, path)
            .env(LOCK_PROBE_READY_ENV, &ready_path)
            .env(LOCK_PROBE_START_ENV, start.to_string())
            .env(LOCK_PROBE_LEN_ENV, len.to_string())
            .env(LOCK_PROBE_EXPECT_ENV, "conflict")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let child = wait_for_child_ready(child, &ready_path, "POSIX lock probe")?;
        let output = wait_for_child_exit(child, "POSIX lock probe")?;
        if !output.status.success() {
            anyhow::bail!(
                "POSIX lock probe failed: status={}; stdout={}; stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_db_posix_lock_probe_child() -> Result<()> {
        let Some(path) = std::env::var_os(LOCK_PROBE_PATH_ENV) else {
            return Ok(());
        };
        let ready_path = std::env::var_os(LOCK_PROBE_READY_ENV)
            .ok_or_else(|| anyhow!("POSIX lock probe ready path is missing"))?;
        let start = std::env::var(LOCK_PROBE_START_ENV)
            .context("POSIX lock probe start is missing")?
            .parse::<i64>()
            .context("parsing POSIX lock probe start")?;
        let len = std::env::var(LOCK_PROBE_LEN_ENV)
            .context("POSIX lock probe length is missing")?
            .parse::<i64>()
            .context("parsing POSIX lock probe length")?;
        let expected = std::env::var(LOCK_PROBE_EXPECT_ENV)
            .context("POSIX lock probe expectation is missing")?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| {
                format!("opening POSIX lock probe file: {}", path.to_string_lossy())
            })?;
        File::create(ready_path)?;

        let result = try_posix_write_lock(&file, start, len);
        match expected.as_str() {
            "conflict" => match result {
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::EACCES) | Some(libc::EAGAIN)
                    ) =>
                {
                    Ok(())
                }
                Err(error) => Err(error).context("POSIX lock probe failed unexpectedly"),
                Ok(()) => anyhow::bail!("POSIX lock probe unexpectedly acquired the lock"),
            },
            "acquire" => result.context("POSIX lock probe could not acquire the lock"),
            _ => anyhow::bail!("unknown POSIX lock probe expectation: {expected}"),
        }
    }

    #[test]
    fn every_runtime_db_connection_enables_persistent_wal() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let first = open_connection(&db_path)?;
        let second = open_connection(&db_path)?;

        assert_eq!(persistent_wal_setting(&first)?, 1);
        assert_eq!(persistent_wal_setting(&second)?, 1);
        Ok(())
    }

    #[test]
    fn closing_last_runtime_db_connection_preserves_wal_sidecars() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let connection = open_connection(&db_path)?;
        configure_persistent_database(&connection)?;
        connection.execute_batch(
            "CREATE TABLE values_seen(value INTEGER NOT NULL);
             INSERT INTO values_seen(value) VALUES (1);",
        )?;

        let wal_path = sidecar_path(&db_path, "-wal");
        let shm_path = sidecar_path(&db_path, "-shm");
        assert!(wal_path.is_file());
        assert!(shm_path.is_file());

        drop(connection);

        assert!(wal_path.is_file());
        assert!(shm_path.is_file());
        let reader = open_connection(&db_path)?;
        assert_eq!(
            reader.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
                .get::<_, i64>(0))?,
            1
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deleted_open_sidecar_prevents_new_runtime_db_connection() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let wal_path = sidecar_path(&db_path, "-wal");
        let _deleted_open_file = File::create(&wal_path)?;
        fs::remove_file(&wal_path)?;
        File::create(&wal_path)?;

        let error = open_connection(&db_path).expect_err("deleted-open WAL must be rejected");
        let message = format!("{error:#}");
        assert!(message.contains("sidecar divergence detected"));
        assert!(message.contains("deleted-open sidecar"));
        assert!(message.contains("sidecar=-wal"));
        assert!(message.contains("open_inode="));
        assert!(message.contains("canonical_identity="));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn runtime_db_sidecar_fd_scans(db_path: &Path) -> Result<u64> {
        let canonical = db_path.canonicalize()?;
        Ok(verified_runtime_db_sidecars(&canonical)
            .map(|verified| verified.fd_scans)
            .unwrap_or(0))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sidecar_fd_scans_are_skipped_while_identities_are_unchanged() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let writer = open_connection(&db_path)?;
        configure_persistent_database(&writer)?;
        writer.execute_batch(
            "CREATE TABLE values_seen(value INTEGER NOT NULL);
             INSERT INTO values_seen(value) VALUES (1);",
        )?;
        drop(writer);

        // Absorb the one scan owed to the (absent -> present) sidecar
        // transition so the cache reflects the live sidecar identities.
        let settled = open_connection(&db_path)?;
        drop(settled);
        let scans_before = runtime_db_sidecar_fd_scans(&db_path)?;

        let reader = open_connection(&db_path)?;
        let reader_two = open_connection(&db_path)?;
        assert_eq!(
            runtime_db_sidecar_fd_scans(&db_path)?,
            scans_before,
            "opens with unchanged sidecar identities must not rescan /proc/self/fd"
        );
        assert_eq!(
            reader.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
                .get::<_, i64>(0))?,
            1
        );
        assert_eq!(
            reader_two.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
                .get::<_, i64>(0))?,
            1
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn replaced_sidecar_identity_retriggers_the_fd_scan() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let writer = open_connection(&db_path)?;
        configure_persistent_database(&writer)?;
        writer.execute_batch("CREATE TABLE values_seen(value INTEGER NOT NULL);")?;
        drop(writer);
        // Warm the verified-identity cache for the current sidecars.
        let settled = open_connection(&db_path)?;
        drop(settled);

        let wal_path = sidecar_path(&db_path, "-wal");
        let _held_open_sidecar = File::open(&wal_path)?;
        fs::remove_file(&wal_path)?;
        File::create(&wal_path)?;

        // The refusal itself proves the identity change re-ran the fd scan;
        // a wrongly trusted cache would have opened without detecting the
        // deleted-open sidecar.
        let error = open_connection(&db_path).expect_err("deleted-open WAL must be rejected");
        assert!(format!("{error:#}").contains("deleted-open sidecar"));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sidecar_inspection_preserves_posix_write_lock() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let wal_path = sidecar_path(&db_path, "-wal");
        let locked_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&wal_path)?;
        try_posix_write_lock(&locked_file, 0, 1)?;
        assert_lock_conflict_from_child(&wal_path, 0, 1)?;

        let fd_path = Path::new("/proc/self/fd").join(locked_file.as_raw_fd().to_string());
        let deleted_path = format!("{} (deleted)", wal_path.display());
        let metadata = locked_file.metadata()?;
        assert_eq!(
            inspect_runtime_db_sidecar_fd(&fd_path, &wal_path, &deleted_path)?,
            Some(RuntimeDbSidecarOpen {
                deleted: false,
                identity: (metadata.dev(), metadata.ino()),
            })
        );

        assert_lock_conflict_from_child(&wal_path, 0, 1)?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sidecar_inspection_ignores_concurrent_fd_reuse() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let wal_path = sidecar_path(&db_path, "-wal");
        let main_path = db_path;
        let inspected_fd = File::create(&wal_path)?.into_raw_fd();
        let inspected_fd_path = Path::new("/proc/self/fd").join(inspected_fd.to_string());
        let deleted_path = format!("{} (deleted)", wal_path.display());
        let locked_main = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&main_path)?;
        assert_ne!(locked_main.as_raw_fd(), inspected_fd);
        try_posix_write_lock(&locked_main, 0, 1)?;
        assert_lock_conflict_from_child(&main_path, 0, 1)?;
        let locked_main_fd = locked_main.as_raw_fd();

        let observation = inspect_runtime_db_sidecar_fd_with_hook(
            &inspected_fd_path,
            &wal_path,
            &deleted_path,
            move || {
                thread::spawn(move || -> Result<()> {
                    // SAFETY: both descriptors remain valid, and dup2 atomically replaces
                    // the inspected sidecar descriptor with the locked main-db descriptor.
                    if unsafe { libc::dup2(locked_main_fd, inspected_fd) } < 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                    Ok(())
                })
                .join()
                .map_err(|_| anyhow!("fd replacement thread panicked"))?
            },
        )?;

        assert_eq!(observation, None);
        assert_lock_conflict_from_child(&main_path, 0, 1)?;

        // SAFETY: dup2 transferred a duplicate of locked_main into raw ownership.
        unsafe {
            libc::close(inspected_fd);
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sidecar_inspection_classifies_metadata_handle_errors() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let wal_path = sidecar_path(&db_path, "-wal");
        let wal_file = File::create(&wal_path)?;
        let fd_path = Path::new("/proc/self/fd").join(wal_file.as_raw_fd().to_string());
        let deleted_path = format!("{} (deleted)", wal_path.display());

        let disappeared = inspect_runtime_db_sidecar_fd_with_hook_and_open(
            &fd_path,
            &wal_path,
            &deleted_path,
            || Ok(()),
            |_| Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        )?;
        assert_eq!(disappeared, None);

        let error = inspect_runtime_db_sidecar_fd_with_hook_and_open(
            &fd_path,
            &wal_path,
            &deleted_path,
            || Ok(()),
            |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
        )
        .expect_err("systemic O_PATH failure must fail closed");
        assert!(format!("{error:#}").contains("opening runtime db fd metadata handle"));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sidecar_scan_error_does_not_update_verified_cache() -> Result<()> {
        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        File::create(&db_path)?;
        let wal_path = sidecar_path(&db_path, "-wal");
        let _wal_file = File::create(&wal_path)?;

        let error = ensure_runtime_db_sidecars_are_consistent_with(
            &db_path,
            |fd_path, sidecar_path, deleted_path| {
                inspect_runtime_db_sidecar_fd_with_hook_and_open(
                    fd_path,
                    sidecar_path,
                    deleted_path,
                    || Ok(()),
                    |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
                )
            },
        )
        .expect_err("sidecar scan must propagate systemic O_PATH failure");
        assert!(format!("{error:#}").contains("opening runtime db fd metadata handle"));
        assert!(
            verified_runtime_db_sidecars(&db_path.canonicalize()?).is_none(),
            "failed scans must not populate the verified identities cache"
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_db_fd_metadata_handle_closes_on_drop() -> Result<()> {
        let directory = tempdir()?;
        let wal_path = directory.path().join("runtime.sqlite-wal");
        let wal_file = File::create(&wal_path)?;
        let fd_path = Path::new("/proc/self/fd").join(wal_file.as_raw_fd().to_string());
        let metadata_handle = open_runtime_db_fd_metadata_handle(&fd_path)?;
        let metadata_fd_path =
            Path::new("/proc/self/fd").join(metadata_handle.as_raw_fd().to_string());

        assert_eq!(fs::read_link(&metadata_fd_path)?, wal_path);
        assert!(metadata_handle.metadata()?.is_file());
        drop(metadata_handle);

        let error = fs::read_link(&metadata_fd_path)
            .expect_err("dropping the metadata handle must close its exact descriptor");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sidecar_divergence_detection_resolves_runtime_db_symlink() -> Result<()> {
        use std::os::unix::fs::symlink;

        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let alias_path = directory.path().join("runtime-alias.sqlite");
        let connection = open_connection(&db_path)?;
        configure_persistent_database(&connection)?;
        connection.execute_batch("CREATE TABLE values_seen(value INTEGER NOT NULL);")?;
        drop(connection);
        symlink(&db_path, &alias_path)?;

        let wal_path = sidecar_path(&db_path, "-wal");
        let _deleted_open_file = File::open(&wal_path)?;
        fs::remove_file(&wal_path)?;
        File::create(&wal_path)?;

        let error =
            open_connection(&alias_path).expect_err("deleted-open WAL through alias must fail");
        assert!(format!("{error:#}").contains("deleted-open sidecar"));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn external_observer_does_not_replace_runtime_db_sidecars() -> Result<()> {
        if let Some(db_path) = std::env::var_os(OBSERVER_CHILD_DB_ENV) {
            let connection =
                Connection::open_with_flags(Path::new(&db_path), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let max_value =
                connection.query_row("SELECT MAX(value) FROM values_seen", [], |row| {
                    row.get::<_, i64>(0)
                })?;
            assert_eq!(max_value, 1);
            let ready_path = std::env::var_os(OBSERVER_CHILD_READY_ENV)
                .ok_or_else(|| anyhow!("observer child ready path is missing"))?;
            let release_path = std::env::var_os(OBSERVER_CHILD_RELEASE_ENV)
                .ok_or_else(|| anyhow!("observer child release path is missing"))?;
            File::create(ready_path)?;
            let started_at = Instant::now();
            while !Path::new(&release_path).exists() {
                if started_at.elapsed() > Duration::from_secs(10) {
                    anyhow::bail!("timed out waiting to release observer child");
                }
                thread::sleep(Duration::from_millis(10));
            }
            drop(connection);
            return Ok(());
        }

        let directory = tempdir()?;
        let db_path = directory.path().join("runtime.sqlite");
        let ready_path = directory.path().join("observer-ready");
        let release_path = directory.path().join("observer-release");
        let writer = open_connection(&db_path)?;
        configure_persistent_database(&writer)?;
        writer.execute_batch(
            "CREATE TABLE values_seen(value INTEGER NOT NULL);
             INSERT INTO values_seen(value) VALUES (1);",
        )?;

        let wal_path = sidecar_path(&db_path, "-wal");
        let shm_path = sidecar_path(&db_path, "-shm");
        let wal_identity = fs::metadata(&wal_path)?.ino();
        let shm_identity = fs::metadata(&shm_path)?.ino();
        let child = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg(OBSERVER_TEST_NAME)
            .arg("--nocapture")
            .env(OBSERVER_CHILD_DB_ENV, &db_path)
            .env(OBSERVER_CHILD_READY_ENV, &ready_path)
            .env(OBSERVER_CHILD_RELEASE_ENV, &release_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let child = wait_for_child_ready(child, &ready_path, "runtime db observer")?;

        let canonical_db_path = db_path.canonicalize()?;
        for suffix in ["-wal", "-shm"] {
            ensure_runtime_db_sidecar_is_consistent_with(
                &canonical_db_path,
                suffix,
                &inspect_runtime_db_sidecar_fd,
            )?;
        }
        writer.execute("INSERT INTO values_seen(value) VALUES (2)", [])?;

        File::create(&release_path)?;
        let output = wait_for_child_exit(child, "runtime db observer")?;
        assert!(
            output.status.success(),
            "observer child failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        writer.execute("INSERT INTO values_seen(value) VALUES (3)", [])?;

        assert_eq!(fs::metadata(&wal_path)?.ino(), wal_identity);
        assert_eq!(fs::metadata(&shm_path)?.ino(), shm_identity);
        ensure_runtime_db_sidecars_are_consistent(&db_path)?;
        drop(writer);

        let reader = open_connection(&db_path)?;
        assert_eq!(
            reader.query_row("SELECT MAX(value) FROM values_seen", [], |row| row
                .get::<_, i64>(0))?,
            3
        );
        Ok(())
    }
}
