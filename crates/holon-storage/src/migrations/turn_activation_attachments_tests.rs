use super::*;
use rusqlite::StatementStatus;
use serde_json::json;

fn legacy() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version < 112)
    {
        apply_migration(&mut connection, migration, |_, _| Ok(()))?;
    }
    Ok(connection)
}

fn put(connection: &Connection, agent: &str, turn: &str, payload: serde_json::Value) -> Result<()> {
    connection.execute(
        "INSERT INTO turn_records(turn_id,turn_index,agent_id,created_at,payload_json)
         VALUES(?1,1,?2,'2026-11-16',?3)
         ON CONFLICT(turn_id) DO UPDATE SET payload_json=excluded.payload_json",
        params![turn, agent, payload.to_string()],
    )?;
    Ok(())
}

#[test]
fn known_task_key_absence_does_not_scan_other_scopes() -> Result<()> {
    let mut connection = legacy()?;
    upgrade(&mut connection)?;
    let mut steps = Vec::new();
    for size in [100, 10_000] {
        let tx = connection.transaction()?;
        for index in 0..size {
            tx.execute(
                "INSERT OR IGNORE INTO task_domain_records
                 (agent_id,identity_incarnation,family,record_id,revision,availability)
                 VALUES('other-agent',1,'task',?1,1,'pruned')",
                [format!("history-{index}")],
            )?;
        }
        tx.commit()?;
        let mut statement = connection.prepare(
            "SELECT EXISTS(SELECT 1 FROM task_domain_records
             WHERE family=?1 AND record_id=?2 AND availability!='absent')",
        )?;
        assert!(!statement.query_row(params!["task", "unknown"], |row| row.get::<_, bool>(0))?);
        steps.push(statement.get_status(StatementStatus::VmStep));
    }
    assert_eq!(
        steps[0], steps[1],
        "Task absence check scanned other scopes"
    );
    assert!(steps[0] < 40);
    Ok(())
}

fn edges(connection: &Connection, activation: &str) -> Result<Vec<(String, String, bool, bool)>> {
    Ok(connection
        .prepare(
            "SELECT agent_id,turn_id,has_live,has_idle FROM turn_activation_attachments
             WHERE activation_id=?1 ORDER BY agent_id,turn_id",
        )?
        .query_map([activation], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn upgrade(connection: &mut Connection) -> Result<()> {
    let migration = MIGRATIONS
        .iter()
        .find(|migration| migration.version == 112)
        .unwrap();
    apply_migration(connection, migration, |_, _| Ok(()))
}

#[test]
fn sparse_upgrade_does_not_invent_missing_canonical_sources() -> Result<()> {
    for omitted in [
        "turn_records",
        "wait_conditions",
        "task_domain_records",
        "task_stop_effects",
    ] {
        let mut connection = legacy()?;
        connection.execute_batch(&format!("DROP TABLE {omitted}"))?;
        upgrade(&mut connection)?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [omitted],
            |row| row.get(0),
        )?;
        assert!(!exists);
        assert_eq!(current_schema_version(&connection)?, 112);
        if omitted != "turn_records" {
            assert!(edges(&connection, "unknown")?.is_empty());
        }
    }
    Ok(())
}

#[test]
fn upgrade_materializes_old_batches_without_hiding_ambiguity() -> Result<()> {
    let mut connection = legacy()?;
    put(
        &connection,
        "a",
        "first",
        json!({
            "live_input_batches": [{"activation_id":"activation"}, {"activation_id":"activation"}],
            "idle_input_batch": {"activation_id":"activation"}
        }),
    )?;
    put(
        &connection,
        "a",
        "second",
        json!({
            "terminal": {"kind":"completed"},
            "live_input_batches": [{"activation_id":"activation"}]
        }),
    )?;
    put(
        &connection,
        "b",
        "foreign",
        json!({
            "idle_input_batch": {"activation_id":"activation"}
        }),
    )?;
    put(
        &connection,
        "a",
        "batchless",
        json!({
            "idle_input_batch": {"activation_id":""}
        }),
    )?;
    upgrade(&mut connection)?;
    assert_eq!(
        edges(&connection, "activation")?,
        vec![
            ("a".into(), "first".into(), true, true),
            ("a".into(), "second".into(), true, false),
            ("b".into(), "foreign".into(), false, true),
        ]
    );
    assert!(edges(&connection, "")?.is_empty());
    assert_eq!(current_schema_version(&connection)?, 112);
    // Applying an already recorded migration does not rediscover history.
    upgrade(&mut connection)?;
    assert_eq!(edges(&connection, "activation")?.len(), 3);
    Ok(())
}

#[test]
fn actual_turn_update_rebinds_edges_atomically_and_delete_cleans_them() -> Result<()> {
    let mut connection = legacy()?;
    upgrade(&mut connection)?;
    put(
        &connection,
        "a",
        "turn",
        json!({
            "live_input_batches": [{"activation_id":"before"}]
        }),
    )?;
    {
        let tx = connection.transaction()?;
        put(
            &tx,
            "a",
            "turn",
            json!({
                "live_input_batches": [{"activation_id":"after"}],
                "idle_input_batch": {"activation_id":"after"}
            }),
        )?;
        assert!(edges(&tx, "before")?.is_empty());
        assert_eq!(edges(&tx, "after")?.len(), 1);
        tx.rollback()?;
    }
    assert_eq!(edges(&connection, "before")?.len(), 1);
    assert!(edges(&connection, "after")?.is_empty());
    // An ignored older upsert must not publish its proposed attachment.
    connection.execute(
        "INSERT INTO turn_records(turn_id,turn_index,agent_id,created_at,payload_json)
         VALUES('turn',1,'a','2026-11-15',?1)
         ON CONFLICT(turn_id) DO UPDATE SET payload_json=excluded.payload_json
         WHERE excluded.created_at >= turn_records.created_at",
        [json!({"idle_input_batch":{"activation_id":"ignored"}}).to_string()],
    )?;
    assert!(edges(&connection, "ignored")?.is_empty());
    put(
        &connection,
        "a",
        "turn",
        json!({
            "idle_input_batch": {"activation_id":"after"}
        }),
    )?;
    assert!(edges(&connection, "before")?.is_empty());
    assert_eq!(
        edges(&connection, "after")?,
        vec![("a".into(), "turn".into(), false, true)]
    );
    connection.execute("DELETE FROM turn_records WHERE turn_id='turn'", [])?;
    assert!(edges(&connection, "after")?.is_empty());
    Ok(())
}

#[test]
fn malformed_historical_material_does_not_abort_attachment_upgrade() -> Result<()> {
    let mut connection = legacy()?;
    put(&connection, "a", "broken", json!({}))?;
    connection.execute(
        "UPDATE turn_records SET payload_json='malformed canonical body' WHERE turn_id='broken'",
        [],
    )?;
    put(
        &connection,
        "a",
        "mixed",
        json!({"live_input_batches": ["not an object", null, 7, {"activation_id":"valid"}]}),
    )?;
    upgrade(&mut connection)?;
    assert_eq!(
        edges(&connection, "valid")?,
        vec![("a".into(), "mixed".into(), true, false)]
    );
    assert_eq!(
        connection.query_row::<String, _, _>(
            "SELECT payload_json FROM turn_records WHERE turn_id='broken'",
            [],
            |row| row.get(0),
        )?,
        "malformed canonical body"
    );
    Ok(())
}

#[test]
fn malformed_updates_remove_only_derived_edges_and_roll_back_atomically() -> Result<()> {
    let mut connection = legacy()?;
    upgrade(&mut connection)?;
    connection.execute(
        "INSERT INTO turn_records(turn_id,turn_index,agent_id,created_at,payload_json)
         VALUES('broken',1,'a','2026-11-16','malformed canonical body')",
        [],
    )?;
    put(
        &connection,
        "a",
        "turn",
        json!({"live_input_batches":[{"activation_id":"valid"}]}),
    )?;
    {
        let tx = connection.transaction()?;
        tx.execute(
            "UPDATE turn_records SET payload_json='malformed canonical body' WHERE turn_id='turn'",
            [],
        )?;
        assert!(edges(&tx, "valid")?.is_empty());
        tx.rollback()?;
    }
    assert_eq!(edges(&connection, "valid")?.len(), 1);
    connection.execute(
        "UPDATE turn_records SET payload_json='malformed canonical body' WHERE turn_id='turn'",
        [],
    )?;
    assert!(edges(&connection, "valid")?.is_empty());
    put(
        &connection,
        "a",
        "turn",
        json!({"live_input_batches":["not an object"],"idle_input_batch":{"activation_id":"repaired"}}),
    )?;
    assert_eq!(
        edges(&connection, "repaired")?,
        vec![("a".into(), "turn".into(), false, true)]
    );
    Ok(())
}

#[test]
fn exact_activation_lookup_does_not_visit_unrelated_turn_history() -> Result<()> {
    let mut connection = legacy()?;
    upgrade(&mut connection)?;
    put(
        &connection,
        "a",
        "selected",
        json!({
            "live_input_batches": [{"activation_id":"activation"}]
        }),
    )?;
    put(
        &connection,
        "a",
        "idle-only",
        json!({
            "idle_input_batch": {"activation_id":"idle-activation"}
        }),
    )?;
    let mut steps = Vec::new();
    for size in [100, 10_000] {
        let tx = connection.transaction()?;
        for index in 0..size {
            put(
                &tx,
                "a",
                &format!("history-{index}"),
                json!({
                    "live_input_batches": [{"activation_id":format!("unrelated-{index}")}]
                }),
            )?;
        }
        tx.commit()?;
        let mut current_steps = Vec::new();
        for (activation, include_idle, expected) in [
            ("activation", true, vec!["selected"]),
            ("absent", true, vec![]),
            ("idle-activation", false, vec![]),
            ("idle-activation", true, vec!["idle-only"]),
        ] {
            let mut statement = connection.prepare(
                "SELECT turn_id FROM turn_activation_attachments
                 WHERE agent_id=?1 AND activation_id=?2 AND (has_live=1 OR (?3 AND has_idle=1))
                 LIMIT 2",
            )?;
            let rows = statement
                .query_map(params!["a", activation, include_idle], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(rows, expected);
            current_steps.push(statement.get_status(StatementStatus::VmStep));
        }
        steps.push(current_steps);
    }
    assert_eq!(
        steps[0], steps[1],
        "unrelated history increased lookup work"
    );
    assert!(
        steps[0].iter().all(|steps| *steps < 100),
        "activation lookup is not an indexed point read: {steps:?}"
    );
    Ok(())
}
