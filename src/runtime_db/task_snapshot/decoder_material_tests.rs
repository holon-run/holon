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
fn seed(tx: &Transaction<'_>, count: usize) -> Result<()> {
    for index in 0..count {
        let record = task(
            &format!("actual-loader-{index:03}"),
            if index == 0 {
                TaskStatus::Running
            } else {
                TaskStatus::Completed
            },
        );
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
        let proof = writes::prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &record)?;
        writes::register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &policy(),
        )?;
    }
    Ok(())
}
#[test]
fn original_task_loader_retains_complete_128_rows_and_first_metadata_copies_with_the_actual_decoder_peak(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| seed(tx, 128))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope = scope();
        let request = policy();
        let mut material = TaskReadMaterial::new(&scope, &request, Some(&ledger));
        material.charge_writer_material(1024, 4, 1)?;
        let mut record = task("actual-loader-000", TaskStatus::Completed);
        record.summary = Some("actual legal terminal update".into());
        record.updated_at += chrono::Duration::milliseconds(1);
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
        let proof = writes::prepare_original_task_write_using_material_tx(
            tx,
            &scope,
            &keys,
            false,
            &request,
            &ledger,
            &mut material,
        )?;
        let before = proof.task_before(&record.id)?.clone();
        repositories::upsert_task_tx(tx, &record)?;
        let facts = writes::register_original_task_writes_using_material_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request,
            &ledger,
            &mut material,
        )?;
        assert_eq!(facts.delta.records.len(), 128);
        assert_eq!(facts.delta.coverage.terminal.keys.len(), 128);
        assert_eq!(facts.delta.coverage.task_metadata.len(), 128);
        assert_eq!(proof.task_before(&record.id)?, &before);
        let peak = material.phase_peak_for_test();
        let retained = material.material_estimate();
        assert!(peak.bytes <= request.byte_limit && peak.records <= request.record_limit);
        assert!(retained.bytes < peak.bytes && retained.records < peak.records);
        let last = facts.delta.clone();
        let guards = material.into_guards();
        let last_guards = guards.clone();
        drop((facts, guards, proof));
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last_guards);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
#[test]
fn original_task_current_metadata_retires_only_its_owned_dto_after_later_material_and_refuses_an_escaped_guard(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| seed(tx, 2))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope = scope();
        let request = policy();
        let mut material = TaskReadMaterial::new(&scope, &request, Some(&ledger));
        material.charge_writer_material(1024, 4, 1)?;
        let prefix = material.material_estimate();
        let previous = material.original_current_metadata_tx(tx)?;
        assert_eq!(
            previous
                .as_ref()
                .context("actual coverage missing")?
                .task_metadata
                .len(),
            2
        );
        let dto = material.material_estimate();
        material.charge_writer_material(4096, 7, 1)?;
        let before = material.material_estimate();
        previous.retire(&mut material)?;
        let after = material.material_estimate();
        assert_eq!(before.bytes - after.bytes, dto.bytes - prefix.bytes);
        assert_eq!(before.records - after.records, dto.records - prefix.records);
        assert_eq!(
            (after.bytes, after.records),
            (prefix.bytes + 4096 + 7 * 256, prefix.records + 7)
        );
        let previous = material.original_current_metadata_tx(tx)?;
        let escaped = material
            .guards
            .last()
            .context("actual current metadata guard missing")?
            .clone();
        let before = material.material_estimate();
        let error = previous.retire(&mut material).unwrap_err();
        assert!(format!("{error:#}").contains("another live holder"));
        assert_eq!(
            (material.bytes, material.records),
            (before.bytes, before.records)
        );
        drop(material);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(escaped);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
#[test]
fn original_task_row_and_metadata_each_require_their_actual_first_allocation_allowance_before_any_decoder_or_copy(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| seed(tx, 1))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope = scope();
        let request = policy();
        let key = TaskKey::new(TaskFamily::Task, "actual-loader-000");
        let item = stored_on(tx, &scope.agent_id, &key)?.context("actual Task missing")?;
        let mut phase = TaskReadMaterial::new(&scope, &request, Some(&ledger));
        phase.charge_writer_material(1024, 4, 1)?;
        phase.admit_loader_metadata(&item)?;
        let exact = phase.material_estimate();
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let mut tight = request.clone();
        tight.byte_limit = exact.bytes - 1;
        let mut phase = TaskReadMaterial::new(&scope, &tight, Some(&ledger));
        phase.charge_writer_material(1024, 4, 1)?;
        let error = phase.admit_loader_metadata(&item).unwrap_err();
        assert!(error
            .downcast_ref::<TaskSnapshotError>()
            .is_some_and(|e| matches!(e, TaskSnapshotError::ResourceExhausted(_))));
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let raw = "!".repeat(96 * 1024);
        tx.execute(
            "UPDATE tasks SET payload_json=?2 WHERE task_id=?1",
            params![key.id, raw],
        )?;
        let mut tight = request;
        tight.byte_limit = 64 * 1024;
        let mut phase = TaskReadMaterial::new(&scope, &tight, Some(&ledger));
        phase.charge_writer_material(1024, 4, 1)?;
        let error = phase
            .original_stored_row_tx(tx, &scope.agent_id, &key, |_| Ok(()))
            .unwrap_err();
        assert!(error
            .downcast_ref::<TaskSnapshotError>()
            .is_some_and(|e| matches!(e, TaskSnapshotError::ResourceExhausted(_))));
        let actual: String = tx.query_row(
            "SELECT payload_json FROM tasks WHERE task_id=?1",
            [&key.id],
            |row| row.get(0),
        )?;
        assert_eq!(actual, raw);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
