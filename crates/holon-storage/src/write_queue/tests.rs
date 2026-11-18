use std::fs;

use tempfile::tempdir;

use super::*;
use crate::connection::configure_persistent_database;

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

#[test]
fn sidecar_divergence_quarantines_writer_without_reconcile() -> Result<()> {
    let directory = tempdir()?;
    let db_path = directory.path().join("runtime.sqlite");
    let connection = open_connection(&db_path, &crate::diagnostics::NO_DIAGNOSTICS)?;
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
