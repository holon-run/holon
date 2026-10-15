use super::*;
use crate::runtime_db::task_snapshot::tests::{fresh, scope, task};
use crate::types::TaskStatus;
fn policy() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    }
}
#[test]
fn original_task_scalar_read_drops_only_its_temporary_coverage_and_projection_before_business_envelope(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope = scope();
        let request = policy();
        let record = task("actual-scalar-lifetime", TaskStatus::Running);
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
        let proof = writes::prepare_task_write_tx(tx, &scope, &keys)?;
        repositories::upsert_task_tx(tx, &record)?;
        writes::register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request,
        )?;
        let mut phase = TaskReadMaterial::new(&scope, &request, Some(&ledger));
        phase.charge_writer_material(1024, 4, 1)?;
        phase.admit_original_aliases_tx(tx, &scope, &keys)?;
        let before = phase.material_estimate();
        let observed = ledger.report()?.observed_bytes;
        let revision = phase.original_revision_tx(tx, &scope)?;
        assert!(revision > 1);
        assert_eq!((phase.bytes, phase.records), (before.bytes, before.records));
        assert_eq!(ledger.report()?.observed_bytes, observed);
        let actual = phase.original_maintenance_size_tx(tx, &scope, &keys)?;
        let original = task_maintenance_material_tx(tx, &scope, &keys)?;
        assert_eq!(
            (actual.bytes, actual.records),
            (original.bytes, original.records)
        );
        assert_eq!((phase.bytes, phase.records), (before.bytes, before.records));
        assert_eq!(ledger.report()?.observed_bytes, observed);
        phase.charge_writer_material(actual.bytes, actual.records, 1)?;
        assert_eq!(
            phase.bytes,
            before.bytes + actual.bytes + actual.records * 256
        );
        let last = phase.into_guards();
        let copy = last.clone();
        drop(last);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(copy);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
#[test]
fn original_task_scalar_read_still_bounds_oversized_normalized_projection_before_sql_json_and_rolls_back(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let record = task("actual-scalar-too-large", TaskStatus::Running);
    let scope = scope();
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
    db.transaction(|tx| {
        let proof = writes::prepare_task_write_tx(tx, &scope, &keys)?;
        repositories::upsert_task_tx(tx, &record)?;
        writes::register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &policy(),
        )?;
        Ok(())
    })?;
    let before = db.connection()?.query_row(
        "SELECT summary FROM tasks WHERE task_id=?1",
        [&record.id],
        |r| r.get::<_, String>(0),
    )?;
    let result: Result<()> = db.transaction(|tx| {
        tx.execute(
            "UPDATE tasks SET summary=?1 WHERE task_id=?2",
            params!["actual-scalar-raw".repeat(32768), record.id],
        )?;
        let mut request = policy();
        request.byte_limit = 1024 * 1024;
        let mut phase = TaskReadMaterial::new(&scope, &request, Some(&ledger));
        phase.original_maintenance_size_tx(tx, &scope, &keys)?;
        anyhow::bail!("oversized normalized projection escaped first raw gate")
    });
    let error = result.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT summary FROM tasks WHERE task_id=?1",
            [&record.id],
            |r| r.get::<_, String>(0)
        )?,
        before
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
