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
    let rows: i64 = connection.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))?;
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
