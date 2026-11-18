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
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(unix))]
use anyhow::bail;
use anyhow::{anyhow, Context, Result};
use rusqlite::{ffi::ErrorCode, Connection, Transaction, TransactionBehavior};

use crate::diagnostics::{StorageDiagnostics, StorageStage};
use crate::persistence::{
    attach_write_context, mark_persistence_failure, persistence_certainty, sqlite_extended_code,
    PersistenceCertainty, PersistenceFailurePhase,
};
use crate::write_queue::RuntimeDbWriteContext;
#[cfg(target_os = "linux")]
use crate::RuntimeDbProtectionError;
use crate::{
    ReaderReprepareRequested, RuntimeDbRetryableError, RuntimeStateTransitionConflict,
    RUNTIME_DB_BEGIN_RETRY_WARN_INTERVAL, RUNTIME_DB_BUSY_TIMEOUT,
    RUNTIME_DB_TRANSACTION_RETRY_INITIAL_DELAY, RUNTIME_DB_TRANSACTION_RETRY_MAX_DELAY,
};

pub enum LockMode {
    Blocking,
    NonBlocking,
}

#[cfg(target_os = "linux")]
const SQLITE_PENDING_BYTE: libc::off_t = 0x4000_0000;
#[cfg(target_os = "linux")]
const SQLITE_SHARED_FIRST: libc::off_t = SQLITE_PENDING_BYTE + 2;
#[cfg(target_os = "linux")]
const SQLITE_SHARED_SIZE: libc::off_t = 510;

#[cfg(target_os = "linux")]
pub(crate) struct RuntimeDbSidecarGuard {
    _file: File,
}

#[cfg(target_os = "linux")]
impl RuntimeDbSidecarGuard {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        let canonical_path = path.canonicalize().with_context(|| {
            format!("resolving runtime db sidecar guard path {}", path.display())
        })?;
        let file = OpenOptions::new()
            .read(true)
            .open(&canonical_path)
            .with_context(|| {
                format!(
                    "opening runtime db sidecar guard {}",
                    canonical_path.display()
                )
            })?;
        let expected = fs::metadata(&canonical_path).with_context(|| {
            format!(
                "reading runtime db sidecar guard identity {}",
                canonical_path.display()
            )
        })?;
        let actual = file.metadata().with_context(|| {
            format!(
                "reading opened runtime db sidecar guard identity {}",
                canonical_path.display()
            )
        })?;
        if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
            anyhow::bail!(
                "runtime db changed while acquiring sidecar guard {}: expected={}:{} opened={}:{}",
                canonical_path.display(),
                expected.dev(),
                expected.ino(),
                actual.dev(),
                actual.ino()
            );
        }

        let lock = libc::flock {
            l_type: libc::F_RDLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: SQLITE_SHARED_FIRST,
            l_len: SQLITE_SHARED_SIZE,
            l_pid: 0,
        };
        // SAFETY: `file` owns a valid descriptor and `lock` remains valid for
        // the duration of this non-blocking fcntl call.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "acquiring Linux OFD sidecar guard for runtime db {}",
                    canonical_path.display()
                )
            });
        }

        Ok(Self { _file: file })
    }
}

pub fn open_connection(path: &Path, diagnostics: &StorageDiagnostics) -> Result<Connection> {
    (diagnostics.record_open_attempt)();
    let _attempt = diagnostics.start(StorageStage::Connection);
    let started_at = Instant::now();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating runtime db directory {}", parent.display()))?;
    }
    ensure_runtime_db_sidecars_are_consistent(path, diagnostics)?;
    let sqlite_open = diagnostics.start(StorageStage::SqliteOpen);
    let connection =
        Connection::open(path).with_context(|| format!("opening runtime db {}", path.display()))?;
    (diagnostics.observe_connection)(&connection)?;
    drop(sqlite_open);
    let configure = diagnostics.start(StorageStage::Configure);
    enable_persistent_wal(&connection, path)?;
    configure_connection(&connection)?;
    drop(configure);
    ensure_runtime_db_sidecars_are_consistent(path, diagnostics)?;
    (diagnostics.record_open_duration)(started_at.elapsed());
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
pub fn ensure_runtime_db_sidecars_are_consistent(
    path: &Path,
    diagnostics: &StorageDiagnostics,
) -> Result<()> {
    ensure_runtime_db_sidecars_are_consistent_with(path, diagnostics, inspect_runtime_db_sidecar_fd)
}

#[cfg(target_os = "linux")]
fn ensure_runtime_db_sidecars_are_consistent_with(
    path: &Path,
    diagnostics: &StorageDiagnostics,
    inspect_fd: impl Fn(&Path, &Path, &str) -> Result<Option<RuntimeDbSidecarOpen>>,
) -> Result<()> {
    let _timer = diagnostics.start(StorageStage::Sidecar);
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
    (diagnostics.record_sidecar_scan)(scan_start.elapsed());
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

pub(crate) fn runtime_db_sidecar_file_path(db_path: &Path, suffix: &str) -> PathBuf {
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
    Err(RuntimeDbProtectionError::new(
        db_path,
        format!(
            "runtime db sidecar divergence detected ({reason}); refusing a new connection: db={}, sidecar={suffix}, fd={fd}, open_dev={}, open_inode={}, canonical_identity={canonical_identity:?}; preserve the files and FD/inode evidence, then perform offline recovery or restart",
            db_path.display(),
            open_identity.0,
            open_identity.1,
        ),
    )
    .into())
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_runtime_db_sidecars_are_consistent(
    _path: &Path,
    _diagnostics: &StorageDiagnostics,
) -> Result<()> {
    Ok(())
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(RUNTIME_DB_BUSY_TIMEOUT)?;
    connection.execute_batch(
        r#"
PRAGMA foreign_keys = ON;
"#,
    )?;
    Ok(())
}

pub fn configure_persistent_database(connection: &Connection) -> Result<()> {
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

pub fn configure_new_database_auto_vacuum(connection: &Connection) -> Result<()> {
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

#[track_caller]
pub(crate) fn run_transaction_on_connection<T>(
    connection: &Connection,
    path: &Path,
    context: RuntimeDbWriteContext,
    queue_wait: Duration,
    mutex_wait: Duration,
    f: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    run_transaction_attempt(
        connection,
        path,
        context.with_caller(),
        queue_wait,
        mutex_wait,
        f,
    )
    .map_err(|error| attach_write_context(error, context.operation, context.table))
}

fn run_transaction_attempt<T>(
    connection: &Connection,
    path: &Path,
    context: RuntimeDbWriteContext,
    queue_wait: Duration,
    mutex_wait: Duration,
    f: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let (origin_file, origin_line) = context.origin();
    let started_at = Instant::now();
    tracing::trace!(
        path = %path.display(),
        operation = context.operation,
        table = context.table,
        mode = context.mode.as_str(),
        origin_file,
        origin_line,
        queue_wait_ms = queue_wait.as_millis(),
        mutex_wait_ms = mutex_wait.as_millis(),
        "runtime db write starting"
    );
    let (transaction, begin_retry_count, begin_wait) =
        begin_immediate_transaction_with_retry(connection, path, context).map_err(|error| {
            mark_persistence_failure(
                error,
                PersistenceCertainty::DefinitelyNotCommitted,
                PersistenceFailurePhase::Begin,
                None,
            )
        })?;
    let callback_started_at = Instant::now();
    let result = f(&transaction);
    let callback_finished_at = Instant::now();
    let callback_elapsed = callback_finished_at.duration_since(callback_started_at);
    match result {
        Ok(value) => {
            let commit = transaction.commit();
            let committed_at = Instant::now();
            let commit_elapsed = committed_at.duration_since(callback_finished_at);
            let elapsed = committed_at.duration_since(started_at);
            let held_write_lock = elapsed.saturating_sub(begin_wait);
            commit.map_err(|error| {
                tracing::warn!(
                    operation = context.operation,
                    table = context.table,
                    origin_file,
                    origin_line,
                    phase = "commit",
                    sqlite_code = error
                        .sqlite_error()
                        .map(|failure| failure.extended_code & 0xff),
                    sqlite_extended_code =
                        error.sqlite_error().map(|failure| failure.extended_code),
                    held_write_lock_ms = held_write_lock.as_millis(),
                    callback_ms = callback_elapsed.as_millis(),
                    commit_ms = commit_elapsed.as_millis(),
                    "runtime db commit outcome unknown"
                );
                mark_persistence_failure(
                    map_runtime_db_sqlite_error("committing transaction", path, error),
                    PersistenceCertainty::Indeterminate,
                    PersistenceFailurePhase::Commit,
                    None,
                )
            })?;
            if queue_wait
                .saturating_add(mutex_wait)
                .saturating_add(elapsed)
                >= Duration::from_secs(1)
            {
                tracing::warn!(
                    operation = context.operation,
                    table = context.table,
                    mode = context.mode.as_str(),
                    origin_file,
                    origin_line,
                    phase = "committed",
                    queue_wait_ms = queue_wait.as_millis(),
                    mutex_wait_ms = mutex_wait.as_millis(),
                    begin_wait_ms = begin_wait.as_millis(),
                    begin_retry_count,
                    held_write_lock_ms = held_write_lock.as_millis(),
                    callback_ms = callback_elapsed.as_millis(),
                    commit_ms = commit_elapsed.as_millis(),
                    elapsed_ms = elapsed.as_millis(),
                    "slow runtime db write"
                );
            }
            tracing::trace!(
                path = %path.display(),
                operation = context.operation,
                table = context.table,
                mode = context.mode.as_str(),
                origin_file,
                origin_line,
                queue_wait_ms = queue_wait.as_millis(),
                mutex_wait_ms = mutex_wait.as_millis(),
                begin_wait_ms = begin_wait.as_millis(),
                begin_retry_count,
                elapsed_ms = elapsed.as_millis(),
                held_write_lock_us = held_write_lock.as_micros(),
                callback_us = callback_elapsed.as_micros(),
                commit_us = commit_elapsed.as_micros(),
                "runtime db write committed"
            );
            Ok(value)
        }
        Err(error) => {
            let rollback = transaction.rollback();
            let rolled_back_at = Instant::now();
            let rollback_elapsed = rolled_back_at.duration_since(callback_finished_at);
            let (error, rolled_back) = match rollback {
                Ok(()) => (
                    mark_persistence_failure(
                        error,
                        PersistenceCertainty::DefinitelyNotCommitted,
                        PersistenceFailurePhase::Callback,
                        None,
                    ),
                    true,
                ),
                Err(rollback_error) => (
                    mark_persistence_failure(
                        error,
                        PersistenceCertainty::Indeterminate,
                        PersistenceFailurePhase::Rollback,
                        Some(rollback_error.to_string()),
                    ),
                    false,
                ),
            };
            let elapsed = rolled_back_at.duration_since(started_at);
            let retryable = is_retryable_db_error(&error);
            let sqlite_extended_code = sqlite_extended_code(&error);
            let sqlite_code = sqlite_extended_code.map(|code| code & 0xff);
            let held_write_lock = elapsed.saturating_sub(begin_wait);
            let definitely_not_committed =
                persistence_certainty(&error) == Some(PersistenceCertainty::DefinitelyNotCommitted);
            if rolled_back && definitely_not_committed && is_reader_reprepare_requested(&error) {
                // Intermediate reprepare control flow: business SQL never
                // ran, the writer was released, and the layer owning the
                // bounded attempts will re-enter. Not a write failure.
                tracing::debug!(
                    error = %error,
                    reader_reprepare = true,
                    retryable,
                    phase = "callback",
                    sqlite_code,
                    sqlite_extended_code,
                    held_write_lock_ms = held_write_lock.as_millis(),
                    callback_ms = callback_elapsed.as_millis(),
                    rollback_ms = rollback_elapsed.as_millis(),
                    path = %path.display(),
                    operation = context.operation,
                    table = context.table,
                    mode = context.mode.as_str(),
                    origin_file,
                    origin_line,
                    queue_wait_ms = queue_wait.as_millis(),
                    mutex_wait_ms = mutex_wait.as_millis(),
                    begin_wait_ms = begin_wait.as_millis(),
                    begin_retry_count,
                    elapsed_ms = elapsed.as_millis(),
                    "runtime db write deferred to reader reprepare"
                );
            } else if rolled_back && is_retryable_state_transition_conflict(&error) {
                tracing::debug!(
                    error = %error,
                    retryable,
                    phase = if rolled_back { "callback" } else { "rollback" },
                    sqlite_code,
                    sqlite_extended_code,
                    held_write_lock_ms = held_write_lock.as_millis(),
                    callback_ms = callback_elapsed.as_millis(),
                    rollback_ms = rollback_elapsed.as_millis(),
                    path = %path.display(),
                    operation = context.operation,
                    table = context.table,
                    mode = context.mode.as_str(),
                    origin_file,
                    origin_line,
                    queue_wait_ms = queue_wait.as_millis(),
                    mutex_wait_ms = mutex_wait.as_millis(),
                    begin_wait_ms = begin_wait.as_millis(),
                    begin_retry_count,
                    elapsed_ms = elapsed.as_millis(),
                    "runtime db write rolled back"
                );
            } else {
                tracing::warn!(
                    error = %error,
                    retryable,
                    phase = if rolled_back { "callback" } else { "rollback" },
                    sqlite_code,
                    sqlite_extended_code,
                    held_write_lock_ms = held_write_lock.as_millis(),
                    callback_ms = callback_elapsed.as_millis(),
                    rollback_ms = rollback_elapsed.as_millis(),
                    rolled_back,
                    path = %path.display(),
                    operation = context.operation,
                    table = context.table,
                    mode = context.mode.as_str(),
                    origin_file,
                    origin_line,
                    queue_wait_ms = queue_wait.as_millis(),
                    mutex_wait_ms = mutex_wait.as_millis(),
                    begin_wait_ms = begin_wait.as_millis(),
                    begin_retry_count,
                    elapsed_ms = elapsed.as_millis(),
                    "runtime db write callback failed"
                );
            }
            Err(error)
        }
    }
}

fn begin_immediate_transaction_with_retry<'connection>(
    connection: &'connection Connection,
    path: &Path,
    context: RuntimeDbWriteContext,
) -> Result<(Transaction<'connection>, u32, Duration)> {
    let (origin_file, origin_line) = context.origin();
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
                    operation = context.operation,
                    table = context.table,
                    origin_file,
                    origin_line,
                    phase = "begin",
                    sqlite_extended_code = error.sqlite_error().map(|failure| failure.extended_code),
                    retry_count,
                    retry_delay_ms = retry_delay.as_millis(),
                    "runtime db begin immediate transaction retrying"
                );
                if elapsed >= next_warn_at {
                    tracing::warn!(
                        error = %error,
                        path = %path.display(),
                        operation = context.operation,
                        table = context.table,
                        origin_file,
                        origin_line,
                        phase = "begin",
                        sqlite_extended_code = error.sqlite_error().map(|failure| failure.extended_code),
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

fn map_runtime_db_sqlite_error(
    operation: &'static str,
    path: &Path,
    error: rusqlite::Error,
) -> anyhow::Error {
    if is_sqlite_locked(&error) {
        let context = RuntimeDbRetryableError::new(operation, path, &error);
        anyhow!(error).context(context)
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

fn is_retryable_state_transition_conflict(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some_and(RuntimeStateTransitionConflict::retryable)
    })
}

fn is_reader_reprepare_requested(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|source| source.downcast_ref::<ReaderReprepareRequested>().is_some())
}

pub(crate) fn is_retryable_transaction_error(error: &anyhow::Error) -> bool {
    if persistence_certainty(error) == Some(PersistenceCertainty::Indeterminate) {
        return false;
    }
    error.downcast_ref::<RuntimeDbRetryableError>().is_some()
        || error.chain().any(|source| {
            source.downcast_ref::<RuntimeDbRetryableError>().is_some()
                || source
                    .downcast_ref::<rusqlite::Error>()
                    .is_some_and(is_sqlite_locked)
        })
}

/// Check if an error is retryable for runtime error classification and logging.
pub fn is_retryable_db_error(error: &anyhow::Error) -> bool {
    if persistence_certainty(error) == Some(PersistenceCertainty::Indeterminate) {
        return false;
    }
    is_retryable_state_transition_conflict(error) || is_retryable_transaction_error(error)
}

#[cfg(unix)]
pub fn flock(file: &File, mode: LockMode) -> Result<()> {
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
pub fn unlock(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(not(unix))]
pub fn flock(_file: &File, _mode: LockMode) -> Result<()> {
    bail!("runtime db file lock is only implemented on Unix platforms")
}

#[cfg(not(unix))]
pub fn unlock(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
