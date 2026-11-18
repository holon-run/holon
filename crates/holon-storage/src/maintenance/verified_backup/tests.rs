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
