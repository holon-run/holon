use super::*;
use crate::connection::configure_persistent_database;
use std::sync::mpsc;
use std::time::Duration;

fn file_connection(path: &std::path::Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    configure_persistent_database(&connection)?;
    connection.busy_timeout(Duration::from_millis(5_000))?;
    Ok(connection)
}

fn extended_code(error: &rusqlite::Error) -> Option<i32> {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => Some(failure.extended_code),
        _ => None,
    }
}

#[test]
fn deferred_read_then_concurrent_write_observes_busy_snapshot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let a = file_connection(&path)?;
    a.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY, value TEXT NOT NULL)")?;
    let b = file_connection(&path)?;

    a.execute_batch("BEGIN DEFERRED")?;
    let _: i64 = a.query_row("SELECT COUNT(*) FROM probe", [], |row| row.get(0))?;
    b.execute("INSERT INTO probe(value) VALUES ('writer')", [])?;
    let error = a
        .execute("INSERT INTO probe(value) VALUES ('reader')", [])
        .expect_err("a deferred read snapshot must not be promoted after another writer commits");
    assert_eq!(
        extended_code(&error),
        Some(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT)
    );
    a.execute_batch("ROLLBACK")?;
    Ok(())
}

#[test]
fn immediate_migration_transaction_excludes_concurrent_writers() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let mut a = file_connection(&path)?;
    a.execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY, value TEXT NOT NULL)")?;

    let transaction = begin_migration_transaction(&mut a)?;
    let _: i64 = transaction.query_row("SELECT COUNT(*) FROM probe", [], |row| row.get(0))?;

    let b = file_connection(&path)?;
    b.busy_timeout(Duration::from_millis(200))?;
    let error = b
        .execute("INSERT INTO probe(value) VALUES ('writer')", [])
        .expect_err("BEGIN IMMEDIATE must already hold the write lock");
    assert_eq!(extended_code(&error), Some(rusqlite::ffi::SQLITE_BUSY));

    transaction.execute("INSERT INTO probe(value) VALUES ('migration')", [])?;
    transaction.commit()?;
    Ok(())
}

#[test]
fn apply_migration_waits_for_an_external_writer_without_busy_snapshot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let mut applier_connection = file_connection(&path)?;
    ensure_migration_table(&applier_connection)?;
    let mut writer_connection = file_connection(&path)?;
    writer_connection.busy_timeout(Duration::from_millis(5_000))?;

    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || -> Result<()> {
        let transaction = writer_connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute("CREATE TABLE external_writer (value TEXT NOT NULL)", [])?;
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        transaction.commit()?;
        Ok(())
    });
    ready_rx.recv().unwrap();

    let migration = Migration {
        version: 9_001,
        name: "immediate_migration_transaction_regression",
        sql: "CREATE TABLE applied_by_migration (value TEXT NOT NULL);",
    };
    let expected_version = migration.version;
    let applier = std::thread::spawn(move || -> Result<()> {
        apply_migration(
            &mut applier_connection,
            &migration,
            unexpected_runtime_backfill,
        )
    });
    // The migration must be waiting on the external writer instead of failing
    // with SQLITE_BUSY_SNAPSHOT after reading the pre-write snapshot.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!applier.is_finished());

    release_tx.send(()).unwrap();
    writer.join().unwrap()?;
    applier.join().unwrap()?;

    let verification = file_connection(&path)?;
    assert_eq!(current_schema_version(&verification)?, expected_version);
    assert!(table_exists(&verification, "applied_by_migration")?);
    Ok(())
}
