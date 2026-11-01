use super::tests::{fresh, request, scope, task};
use super::*;
use crate::types::{TaskRecord, TaskStatus};

fn write_task(
    tx: &Transaction<'_>,
    record: &TaskRecord,
    bindings: Option<[&str; 3]>,
) -> Result<CommittedTaskFacts> {
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    repositories::upsert_task_tx(tx, record)?;
    if let Some([turn, causation, correlation]) = bindings {
        tx.execute(
            "UPDATE tasks SET last_turn_id=?2,causation_id=?3,correlation_id=?4 WHERE task_id=?1",
            params![record.id, turn, causation, correlation],
        )?;
    }
    register_task_writes_tx(
        tx,
        &proof,
        &keys,
        &AgentState::new(&record.agent_id),
        &request(),
    )
}

#[test]
fn original_task_writes_preserve_sql_only_recovery_bindings_and_complete_fingerprint() -> Result<()>
{
    let (_dir, db) = fresh()?;
    let mut record = task("sql-bound-task", TaskStatus::Running);
    let key = TaskKey::new(TaskFamily::Task, &record.id);
    db.transaction(|tx| {
        write_task(
            tx,
            &record,
            Some(["bound-turn", "bound-cause", "bound-correlation"]),
        )?;
        let raw: String = tx.query_row(
            &format!(
                "SELECT {} FROM tasks WHERE task_id=?1",
                projection_sql(tx, TaskFamily::Task)?
            ),
            [&record.id],
            |row| row.get(0),
        )?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw)?
                .as_array()
                .unwrap()
                .len(),
            21
        );
        let stored = stored_on(tx, &record.agent_id, &key)?.unwrap();
        assert_eq!(stored.projection_digest, digest(raw.as_bytes()));
        assert_eq!(stored.fact, CanonicalTaskFact::Task(record.clone()));
        load_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        Ok(())
    })?;
    record.summary = Some("original update preserving SQL-only bindings".into());
    db.transaction(|tx| {
        let facts = write_task(tx, &record, None)?;
        assert!(facts.delta.revision > facts.delta.base_revision);
        let bindings: (String, String, String) = tx.query_row(
            "SELECT last_turn_id,causation_id,correlation_id FROM tasks WHERE task_id=?1",
            [&record.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            bindings,
            (
                "bound-turn".into(),
                "bound-cause".into(),
                "bound-correlation".into()
            )
        );
        load_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        Ok(())
    })
}

#[test]
fn sql_only_recovery_binding_external_changes_reject_prepared_task_reads_and_writes() -> Result<()>
{
    for bindings in [
        None,
        Some(["bound-turn", "bound-cause", "bound-correlation"]),
    ] {
        let (_dir, db) = fresh()?;
        let record = task("sql-drift-task", TaskStatus::Running);
        db.transaction(|tx| {
            write_task(tx, &record, bindings)?;
            let key = TaskKey::new(TaskFamily::Task, &record.id);
            let original = stored_on(tx, &record.agent_id, &key)?.unwrap();
            for (index, column) in ["last_turn_id", "causation_id", "correlation_id"]
                .iter()
                .enumerate()
            {
                tx.execute(
                    &format!("UPDATE tasks SET {column}='external-drift' WHERE task_id=?1"),
                    [&record.id],
                )?;
                let actual = stored_on(tx, &record.agent_id, &key)?.unwrap();
                assert_eq!(actual.fact, original.fact, "{column}");
                assert_ne!(
                    actual.projection_digest, original.projection_digest,
                    "{column}"
                );
                let read_error =
                    load_task_domain_snapshot_tx(tx, &scope(), &request()).unwrap_err();
                assert!(
                    matches!(
                        read_error.downcast_ref::<TaskSnapshotError>(),
                        Some(TaskSnapshotError::CanonicalChanged(changed)) if changed == &key
                    ),
                    "{column}: {read_error:#}"
                );
                let write_error =
                    prepare_task_write_tx(tx, &scope(), &BTreeSet::from([key.clone()]))
                        .err()
                        .unwrap();
                assert!(
                    matches!(
                        write_error.downcast_ref::<TaskSnapshotError>(),
                        Some(TaskSnapshotError::CanonicalChanged(changed)) if changed == &key
                    ),
                    "{column}: {write_error:#}"
                );
                tx.execute(
                    &format!("UPDATE tasks SET {column}=?2 WHERE task_id=?1"),
                    params![record.id, bindings.map(|values| values[index])],
                )?;
            }
            load_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn payload_projection_rejects_unexpected_task_sql_shape() -> Result<()> {
    for columns in [18, 20, 22] {
        let raw = serde_json::to_string(&vec![serde_json::Value::Null; columns])?;
        let error = normalized_payload_projection(&raw, TaskFamily::Task).unwrap_err();
        assert!(error.to_string().contains("SQL projection shape mismatch"));
    }
    Ok(())
}
