use super::*;

#[test]
fn runtime_backfill_failure_rolls_back_schema_and_migration_record() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    let migration = Migration {
        version: 94,
        name: "explicit_episode_continuity",
        sql: "CREATE TABLE capture_probe (value TEXT NOT NULL);",
    };

    let err = apply_migration(&mut connection, &migration, |tx, step| {
        assert!(matches!(step, RuntimeBackfill::EpisodeContinuity));
        tx.execute("INSERT INTO capture_probe VALUES ('partial')", [])?;
        bail!("injected runtime backfill failure");
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "injected runtime backfill failure");
    assert!(!table_exists(&connection, "capture_probe")?);
    assert_eq!(current_schema_version(&connection)?, 0);

    apply_migration(&mut connection, &migration, |tx, step| {
        assert!(matches!(step, RuntimeBackfill::EpisodeContinuity));
        tx.execute("INSERT INTO capture_probe VALUES ('complete')", [])?;
        Ok(())
    })?;
    assert_eq!(current_schema_version(&connection)?, migration.version);
    assert_eq!(
        connection.query_row("SELECT value FROM capture_probe", [], |r| r
            .get::<_, String>(0))?,
        "complete"
    );
    Ok(())
}
