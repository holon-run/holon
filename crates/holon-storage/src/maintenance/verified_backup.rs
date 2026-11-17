//! Space preflight, disk-full attribution, and test fault injection for
//! verified runtime database backups (`VACUUM INTO` + integrity check).
//!
//! The preflight is a conservative lower-bound check, not a reservation:
//! concurrent disk use can still exhaust space mid-backup. When that
//! happens the failure must say "disk full" instead of surfacing an opaque
//! SQLite shared-memory I/O error, and the unverified artifact created by
//! this run must be removed instead of left behind.

use std::path::{Path, PathBuf};
use std::{fs::File, io};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::Connection;

use crate::{
    connection::{open_connection, runtime_db_sidecar_file_path},
    diagnostics::StorageDiagnostics,
};

/// Create and verify a fresh backup. Acquire the caller's protected connection
/// after checking the destination and available space.
pub fn create_verified_backup(
    db_path: &Path,
    label: &str,
    open_source: impl FnOnce() -> Result<Connection>,
    diagnostics: &StorageDiagnostics,
) -> Result<PathBuf> {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime.sqlite");
    let backup_path = db_path.with_file_name(format!("{file_name}.{label}.{timestamp}.bak"));
    if backup_path.exists() {
        bail!(
            "runtime database backup already exists: {}",
            backup_path.display()
        );
    }
    ensure_sufficient_backup_space(db_path, &backup_path)?;
    if let Err(error) = write_verified_backup(&backup_path, open_source, diagnostics) {
        // The path was verified absent above and carries a fresh
        // nanosecond timestamp, so any file there now is this run's
        // unverified artifact. Pre-existing backups are never touched.
        let _ = std::fs::remove_file(&backup_path);
        if error_chain_indicates_disk_full(&error) {
            return Err(error.context(format!(
                "disk full while writing runtime database backup {} (the space preflight \
                 passed, but concurrent disk use can still exhaust space)",
                backup_path.display()
            )));
        }
        return Err(error);
    }
    Ok(backup_path)
}

fn write_verified_backup(
    backup_path: &Path,
    open_source: impl FnOnce() -> Result<Connection>,
    diagnostics: &StorageDiagnostics,
) -> Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    {
        if let Some(failure) = test_hooks::vacuum_failure() {
            return Err(
                anyhow::Error::new(failure).context("runtime database backup vacuum failed")
            );
        }
    }
    let backup_path_text = backup_path.to_string_lossy().into_owned();
    open_source()?
        .execute("VACUUM INTO ?1", [&backup_path_text])
        .with_context(|| format!("creating runtime database backup {}", backup_path.display()))?;
    File::open(backup_path)?.sync_all()?;
    let integrity: String = verified_backup_integrity(backup_path, diagnostics)?;
    if integrity != "ok" {
        bail!(
            "runtime database backup {} failed integrity check: {integrity}",
            backup_path.display()
        );
    }
    Ok(())
}

fn verified_backup_integrity(
    backup_path: &Path,
    diagnostics: &StorageDiagnostics,
) -> Result<String> {
    #[cfg(any(test, feature = "test-support"))]
    {
        if let Some(injected) = test_hooks::integrity_result() {
            return Ok(injected);
        }
    }
    Ok(open_connection(backup_path, diagnostics)?.query_row(
        "PRAGMA integrity_check",
        [],
        |row| row.get(0),
    )?)
}

/// Conservative lower bound for a `VACUUM INTO` backup. The database
/// may compact smaller, but planning for the current file and WAL size
/// keeps the preflight independent of SQLite internals.
fn backup_required_lower_bound(db_path: &Path) -> u64 {
    file_len(db_path).saturating_add(file_len(&runtime_db_sidecar_file_path(db_path, "-wal")))
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Fails fast when the backup target's filesystem cannot hold the
/// conservative lower bound. Never touches the database and creates no
/// files, so a rejected backup leaves business state untouched.
fn ensure_sufficient_backup_space(db_path: &Path, backup_path: &Path) -> Result<()> {
    let required = backup_required_lower_bound(db_path);
    let available = backup_available_space(backup_path)?;
    if let Some(message) = backup_space_error(required, available, backup_path) {
        bail!("{message}");
    }
    Ok(())
}

/// Pure decision: `None` when there is room for the lower bound.
fn backup_space_error(required: u64, available: u64, target: &Path) -> Option<String> {
    (available < required).then(|| {
        format!(
            "insufficient disk space for runtime database backup {}: requires at least {} \
             bytes (database + WAL lower bound), {} bytes available",
            target.display(),
            required,
            available
        )
    })
}

fn backup_available_space(backup_path: &Path) -> Result<u64> {
    if let Some(available) = test_free_space_override() {
        return Ok(available);
    }
    // The backup path does not exist yet; statvfs-style checks resolve the
    // path, so query the containing directory instead.
    let target_dir = backup_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs2::available_space(target_dir).with_context(|| {
        format!(
            "checking free space for runtime database backup target {}",
            backup_path.display()
        )
    })
}

fn test_free_space_override() -> Option<u64> {
    #[cfg(any(test, feature = "test-support"))]
    {
        test_hooks::free_space_override()
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        None
    }
}

/// True when the error chain shows the disk actually ran out of space
/// (ENOSPC or `SQLITE_FULL`). Other SQLite I/O failures, notably the
/// `xShmMap` errors that appear *after* a disk-full backup attempt, are
/// deliberately not reclassified as disk full.
fn error_chain_indicates_disk_full(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(io_error) = cause.downcast_ref::<io::Error>() {
            if io_error_is_disk_full(io_error) {
                return true;
            }
        }
        if let Some(rusqlite::Error::SqliteFailure(failure, _)) =
            cause.downcast_ref::<rusqlite::Error>()
        {
            if failure.code == rusqlite::ErrorCode::DiskFull {
                return true;
            }
        }
        let text = cause.to_string();
        text.contains("os error 28")
            || text.contains("No space left on device")
            || text.contains("database or disk is full")
    })
}

fn io_error_is_disk_full(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::StorageFull) || error.raw_os_error() == Some(28)
}

/// Test-only fault injection for the verified-backup pipeline. Overrides
/// live in thread-local state so parallel tests cannot overwrite each
/// other's installed faults, and they reset when the guard drops so a
/// failed assertion cannot leak state into later tests on the same thread.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_hooks {
    use std::cell::RefCell;
    use std::io;

    #[derive(Default)]
    pub struct Faults {
        pub free_space_override: Option<u64>,
        pub vacuum_failure: Option<io::Error>,
        pub integrity_result: Option<String>,
    }

    impl Faults {
        const fn empty() -> Self {
            Self {
                free_space_override: None,
                vacuum_failure: None,
                integrity_result: None,
            }
        }
    }

    thread_local! {
        static FAULTS: RefCell<Faults> = RefCell::new(Faults::empty());
    }

    pub struct FaultGuard {
        _private: (),
    }

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            FAULTS.with(|faults| *faults.borrow_mut() = Faults::empty());
        }
    }

    /// Installs overrides for the current test; they reset on drop.
    pub fn install(faults: Faults) -> FaultGuard {
        FAULTS.with(|cell| *cell.borrow_mut() = faults);
        FaultGuard { _private: () }
    }

    fn read<T>(extract: fn(&Faults) -> T) -> T {
        FAULTS.with(|cell| extract(&cell.borrow()))
    }

    pub(crate) fn free_space_override() -> Option<u64> {
        read(|faults| faults.free_space_override)
    }

    pub(crate) fn vacuum_failure() -> Option<io::Error> {
        // io::Error is not Clone; the fault is consumed once. Backup
        // creation aborts on the first failure, so one-shot delivery is
        // sufficient for every injection site.
        FAULTS.with(|cell| cell.borrow_mut().vacuum_failure.take())
    }

    pub(crate) fn integrity_result() -> Option<String> {
        read(|faults| faults.integrity_result.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_error_decision_reports_required_and_available() {
        assert!(backup_space_error(100, 99, Path::new("/tmp/x.bak"))
            .expect("insufficient space must error")
            .contains("requires at least 100 bytes"));
        assert!(backup_space_error(100, 99, Path::new("/tmp/x.bak"))
            .expect("message names the target")
            .contains("/tmp/x.bak"));
        assert_eq!(backup_space_error(100, 100, Path::new("/tmp/x.bak")), None);
        assert_eq!(backup_space_error(0, 0, Path::new("/tmp/x.bak")), None);
    }

    #[test]
    fn disk_full_classification_keeps_other_io_errors_out() {
        let enospc = anyhow::Error::new(io::Error::from_raw_os_error(28));
        assert!(error_chain_indicates_disk_full(&enospc));
        let sqlite_full = anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DiskFull,
                extended_code: 13,
            },
            None,
        ));
        assert!(error_chain_indicates_disk_full(&sqlite_full));
        let shm_map = anyhow::Error::new(io::Error::new(
            io::ErrorKind::Other,
            "Error code 4874: I/O error within the xShmMap method",
        ));
        assert!(!error_chain_indicates_disk_full(&shm_map));
        let textual = anyhow::anyhow!("database or disk is full");
        assert!(error_chain_indicates_disk_full(&textual));
    }

    fn backup_fixture() -> Result<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state/runtime.sqlite");
        let db = open_connection(&path, &crate::diagnostics::NO_DIAGNOSTICS)?;
        db.execute_batch(
            "CREATE TABLE example (id INTEGER PRIMARY KEY); INSERT INTO example VALUES (1);",
        )?;
        Ok((dir, path))
    }

    fn backup_artifacts(dir: &tempfile::TempDir) -> usize {
        std::fs::read_dir(dir.path().join("state"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "bak"))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn create_verified_backup_fails_fast_when_space_insufficient() -> anyhow::Result<()> {
        let (dir, path) = backup_fixture()?;
        let _guard = test_hooks::install(test_hooks::Faults {
            free_space_override: Some(1),
            ..Default::default()
        });
        let error = create_verified_backup(
            &path,
            "space-preflight",
            || panic!("insufficient space must prevent opening the source database"),
            &crate::diagnostics::NO_DIAGNOSTICS,
        )
        .expect_err("preflight must reject the backup");
        let message = format!("{error:#}");
        assert!(message.contains("insufficient disk space"));
        assert!(message.contains("requires at least"));
        assert!(message.contains("bytes available"));
        assert_eq!(backup_artifacts(&dir), 0);
        Ok(())
    }

    #[test]
    fn create_verified_backup_attributes_disk_full_and_cleans_up() -> anyhow::Result<()> {
        let (dir, path) = backup_fixture()?;
        let _guard = test_hooks::install(test_hooks::Faults {
            vacuum_failure: Some(io::Error::from_raw_os_error(28)),
            ..Default::default()
        });
        let error = create_verified_backup(
            &path,
            "disk-full",
            || open_connection(&path, &crate::diagnostics::NO_DIAGNOSTICS),
            &crate::diagnostics::NO_DIAGNOSTICS,
        )
        .expect_err("vacuum failure must propagate");
        let message = format!("{error:#}");
        assert!(message.contains("disk full while writing runtime database backup"));
        // The underlying ENOSPC reason stays visible instead of an opaque
        // shared-memory I/O error.
        assert!(message.contains("os error 28"));
        assert_eq!(backup_artifacts(&dir), 0);
        Ok(())
    }

    #[test]
    fn create_verified_backup_removes_artifact_when_integrity_fails() -> anyhow::Result<()> {
        let (dir, path) = backup_fixture()?;
        let _guard = test_hooks::install(test_hooks::Faults {
            integrity_result: Some("main database is corrupt".to_string()),
            ..Default::default()
        });
        let error = create_verified_backup(
            &path,
            "bad-integrity",
            || open_connection(&path, &crate::diagnostics::NO_DIAGNOSTICS),
            &crate::diagnostics::NO_DIAGNOSTICS,
        )
        .expect_err("integrity failure must propagate");
        assert!(format!("{error:#}").contains("failed integrity check"));
        assert_eq!(backup_artifacts(&dir), 0);
        Ok(())
    }

    #[test]
    fn create_verified_backup_succeeds_when_space_sufficient() -> anyhow::Result<()> {
        let (dir, path) = backup_fixture()?;
        let _guard = test_hooks::install(test_hooks::Faults {
            free_space_override: Some(u64::MAX),
            ..Default::default()
        });
        let backup = create_verified_backup(
            &path,
            "happy-path",
            || open_connection(&path, &crate::diagnostics::NO_DIAGNOSTICS),
            &crate::diagnostics::NO_DIAGNOSTICS,
        )?;
        assert!(backup.is_file());
        assert!(backup.metadata()?.len() > 0);
        assert_eq!(backup_artifacts(&dir), 1);
        Ok(())
    }
}
