use super::*;

fn fixture(epoch: &str) -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    // No affinity on value: exercise the reader's actual SQL type boundary.
    connection.execute_batch(
        "CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value);
         CREATE TABLE observer_sync_capability_verifications (
           capability TEXT PRIMARY KEY, verified INTEGER, verification_version INTEGER,
           event_log_epoch TEXT, event_generation INTEGER, verified_event_generation INTEGER
         );",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES ('event_log_epoch', ?1)",
        [epoch],
    )?;
    connection.execute(
        "INSERT INTO observer_sync_capability_verifications VALUES (?1, 1, ?2, ?3, 0, 0)",
        rusqlite::params![
            EVENT_PROJECTION_EFFECT_COMPLETE,
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            epoch,
        ],
    )?;
    Ok(connection)
}

#[test]
fn current_epoch_borrow_preserves_empty_unicode_and_large_identity() -> Result<()> {
    for epoch in [String::new(), "epoch_路\\\"".into(), "x".repeat(128 * 1024)] {
        let connection = fixture(&epoch)?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            Some(true)
        );
        connection.execute(
            "UPDATE runtime_metadata SET value = value || 'different'",
            [],
        )?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            None
        );
        connection.execute("UPDATE runtime_metadata SET value = ?1", [&epoch])?;
        connection.execute(
            "UPDATE observer_sync_capability_verifications SET verified = 0",
            [],
        )?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            Some(false)
        );
        assert!(connection.is_autocommit());
    }
    Ok(())
}

#[test]
fn current_epoch_errors_release_rows_and_statements_without_hiding_sql_failures() -> Result<()> {
    let connection = fixture("epoch")?;
    for value in ["NULL", "17", "X'65706f6368'"] {
        connection.execute(&format!("UPDATE runtime_metadata SET value = {value}"), [])?;
        assert!(current_event_projection_effect_verification(&connection).is_err());
        // A failed borrowed read must not retain a statement or read transaction.
        connection.execute("UPDATE runtime_metadata SET value = 'epoch'", [])?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            Some(true)
        );
        assert!(connection.is_autocommit());
    }
    connection.execute("DELETE FROM runtime_metadata", [])?;
    assert_eq!(
        current_event_projection_effect_verification(&connection)
            .unwrap_err()
            .to_string(),
        "runtime metadata key event_log_epoch is missing"
    );
    connection.execute(
        "INSERT INTO runtime_metadata VALUES ('event_log_epoch', 'epoch')",
        [],
    )?;
    connection.execute("DELETE FROM observer_sync_capability_verifications", [])?;
    assert_eq!(
        current_event_projection_effect_verification(&connection)?,
        None
    );
    connection.execute_batch("DROP TABLE observer_sync_capability_verifications")?;
    assert!(current_event_projection_effect_verification(&connection).is_err());
    connection.execute_batch("DROP TABLE runtime_metadata")?;
    assert!(current_event_projection_effect_verification(&connection).is_err());
    Ok(())
}
