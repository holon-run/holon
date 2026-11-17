//! Physical SQLite database maintenance and verified backups.

use std::{fs, path::Path, time::Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::connection::runtime_db_sidecar_file_path;

mod verified_backup;
pub use verified_backup::create_verified_backup;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use verified_backup::test_hooks;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDbCompactReport {
    pub started_at: DateTime<Utc>,
    pub elapsed_ms: u64,
    pub file_bytes_before: u64,
    pub file_bytes_after: u64,
    pub page_size: u64,
    pub page_count_before: u64,
    pub page_count_after: u64,
    pub freelist_pages_before: u64,
    pub freelist_pages_after: u64,
    pub estimated_reclaimable_bytes_before: u64,
    pub auto_vacuum_before: u32,
    pub auto_vacuum_after: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeDbWalCheckpointReport {
    pub started_at: DateTime<Utc>,
    pub elapsed_ms: u64,
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
    /// True when readers or writers held the WAL and the pass could not
    /// finish; the next maintenance round retries.
    pub busy: bool,
    /// Frames reported by the checkpoint pragma. A completed TRUNCATE
    /// resets the wal-index, so both counters read zero on success; the
    /// authoritative success signals are `busy == false` and
    /// `wal_bytes_after`.
    pub log_frames: u64,
    /// See `log_frames`: zero after a completed TRUNCATE pass.
    pub checkpointed_frames: u64,
}

pub struct DatabasePageStats {
    pub page_size: u64,
    pub page_count: u64,
    pub freelist_pages: u64,
}

/// Reclaim pages on databases configured for incremental auto-vacuum.
pub fn incremental_vacuum(connection: &Connection, pages: u32) -> Result<bool> {
    if pragma_u64(connection, "auto_vacuum")? != 2 {
        return Ok(false);
    }
    connection.execute_batch(&format!("PRAGMA incremental_vacuum({pages});"))?;
    Ok(true)
}

/// Compact a database after the caller has acquired the maintenance lock.
/// The timestamps include the caller's lock and migration time.
pub fn compact_database(
    db_path: &Path,
    connection: &Connection,
    started_at: DateTime<Utc>,
    started: Instant,
) -> Result<RuntimeDbCompactReport> {
    let before = database_page_stats(connection)?;
    let file_bytes_before = file_size(db_path)?;
    let auto_vacuum_before = pragma_u64(connection, "auto_vacuum")? as u32;
    connection.execute_batch(
        "PRAGMA wal_checkpoint(TRUNCATE);
         PRAGMA auto_vacuum = INCREMENTAL;
         VACUUM;",
    )?;
    let after = database_page_stats(connection)?;
    Ok(RuntimeDbCompactReport {
        started_at,
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        file_bytes_before,
        file_bytes_after: file_size(db_path)?,
        page_size: before.page_size,
        page_count_before: before.page_count,
        page_count_after: after.page_count,
        freelist_pages_before: before.freelist_pages,
        freelist_pages_after: after.freelist_pages,
        estimated_reclaimable_bytes_before: before.page_size.saturating_mul(before.freelist_pages),
        auto_vacuum_before,
        auto_vacuum_after: pragma_u64(connection, "auto_vacuum")? as u32,
    })
}

/// Best-effort online WAL checkpoint for long-running daemons.
///
/// `wal_autocheckpoint` only recycles WAL frames: the file keeps its
/// high-water mark, so every page read still pays a wal-index lookup that
/// grows with the checkpoint history. TRUNCATE also resets the file. The
/// short busy timeout keeps maintenance non-blocking: a busy runtime
/// reports `busy` and retries on the next maintenance round.
pub fn checkpoint_wal(
    db_path: &Path,
    open_source: impl FnOnce() -> Result<Connection>,
) -> Result<RuntimeDbWalCheckpointReport> {
    const WAL_CHECKPOINT_BUSY_TIMEOUT_MS: u64 = 500;

    let started_at = Utc::now();
    let started = Instant::now();
    let wal_path = runtime_db_sidecar_file_path(db_path, "-wal");
    let wal_bytes_before = file_size(&wal_path).unwrap_or(0);
    let connection = open_source()?;
    connection.busy_timeout(std::time::Duration::from_millis(
        WAL_CHECKPOINT_BUSY_TIMEOUT_MS,
    ))?;
    let (busy, log_frames, checkpointed_frames) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .or_else(|error| match error {
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ffi::ErrorCode::DatabaseBusy =>
            {
                Ok((1, 0, 0))
            }
            other => Err(other),
        })?;
    Ok(RuntimeDbWalCheckpointReport {
        started_at,
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        wal_bytes_before,
        wal_bytes_after: file_size(&wal_path).unwrap_or(0),
        busy: busy != 0,
        log_frames: log_frames.max(0) as u64,
        checkpointed_frames: checkpointed_frames.max(0) as u64,
    })
}

pub fn database_page_stats(connection: &Connection) -> Result<DatabasePageStats> {
    Ok(DatabasePageStats {
        page_size: pragma_u64(connection, "page_size")?,
        page_count: pragma_u64(connection, "page_count")?,
        freelist_pages: pragma_u64(connection, "freelist_count")?,
    })
}

fn pragma_u64(connection: &Connection, name: &str) -> Result<u64> {
    connection
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .with_context(|| format!("reading SQLite PRAGMA {name}"))
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("reading runtime database metadata {}", path.display()))?
        .len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_connection;
    use crate::diagnostics::NO_DIAGNOSTICS;

    #[test]
    fn wal_checkpoint_pass_truncates_the_wal_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime.sqlite");
        let writer = open_connection(&path, &NO_DIAGNOSTICS)?;
        writer.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE entries (id INTEGER PRIMARY KEY);
             INSERT INTO entries VALUES (1), (2), (3);",
        )?;
        assert!(file_size(&runtime_db_sidecar_file_path(&path, "-wal"))? > 0);

        let report = checkpoint_wal(&path, || open_connection(&path, &NO_DIAGNOSTICS))?;

        assert!(!report.busy);
        assert!(report.wal_bytes_before > 0);
        assert_eq!(report.wal_bytes_after, 0);
        Ok(())
    }

    #[test]
    fn wal_checkpoint_pass_reports_busy_while_reader_holds_snapshot() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime.sqlite");
        let writer = open_connection(&path, &NO_DIAGNOSTICS)?;
        writer.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE entries (id INTEGER PRIMARY KEY);
             INSERT INTO entries VALUES (1);",
        )?;
        let reader = open_connection(&path, &NO_DIAGNOSTICS)?;
        let read_tx = reader.unchecked_transaction()?;
        let count: i64 = read_tx.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))?;
        assert_eq!(count, 1);

        let busy_report = checkpoint_wal(&path, || open_connection(&path, &NO_DIAGNOSTICS))?;
        assert!(busy_report.busy);

        drop(read_tx);
        drop(reader);
        let retried = checkpoint_wal(&path, || open_connection(&path, &NO_DIAGNOSTICS))?;
        assert!(!retried.busy);
        assert_eq!(retried.wal_bytes_after, 0);
        Ok(())
    }

    #[test]
    fn compact_database_reclaims_pages_and_preserves_rows() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime.sqlite");
        let connection = open_connection(&path, &NO_DIAGNOSTICS)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE entries (id INTEGER PRIMARY KEY, payload BLOB);
             INSERT INTO entries VALUES (1, zeroblob(65536)), (2, X'ABCD');
             DELETE FROM entries WHERE id = 1;",
        )?;

        let report = compact_database(&path, &connection, Utc::now(), Instant::now())?;

        assert!(report.freelist_pages_before > 0);
        assert_eq!(report.freelist_pages_after, 0);
        assert!(report.page_count_after < report.page_count_before);
        assert_eq!(report.file_bytes_after, file_size(&path)?);
        assert_eq!(report.auto_vacuum_after, 2);
        let rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        let payload: Vec<u8> =
            connection.query_row("SELECT payload FROM entries WHERE id = 2", [], |row| {
                row.get(0)
            })?;
        assert_eq!(payload, [0xab, 0xcd]);
        Ok(())
    }

    #[test]
    fn incremental_vacuum_respects_database_auto_vacuum_mode() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        assert!(!incremental_vacuum(&connection, 1)?);
        connection.execute_batch(
            "PRAGMA auto_vacuum = INCREMENTAL;
             CREATE TABLE entries (payload BLOB);
             INSERT INTO entries VALUES (zeroblob(65536));
             DELETE FROM entries;",
        )?;

        let before = database_page_stats(&connection)?;
        assert!(before.freelist_pages > 0);
        assert!(incremental_vacuum(&connection, 1)?);
        let after = database_page_stats(&connection)?;
        assert_eq!(after.freelist_pages + 1, before.freelist_pages);
        Ok(())
    }
}
