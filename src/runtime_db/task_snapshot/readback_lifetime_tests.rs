use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
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
fn seed(tx: &Transaction<'_>) -> Result<()> {
    for index in 0..24 {
        let mut record = task(
            &format!("original-readback-{index:02}"),
            if index == 0 { TaskStatus::Running } else { TaskStatus::Completed },
        );
        record.detail = Some(serde_json::json!({"actual":[0,1,2,3,4,5,6,7]}));
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &record)?;
        register_task_writes_tx(tx, &proof, &keys, &AgentState::new("task-agent"), &policy())?;
    }
    Ok(())
}
fn changed() -> crate::types::TaskRecord {
    let mut record = task("original-readback-00", TaskStatus::Completed);
    record.detail = Some(serde_json::json!({"actual":[0,1,2,3,4,5,6,7]}));
    record.summary = Some("actual original changed readback".into());
    record.updated_at += chrono::Duration::milliseconds(1);
    record
}

#[test]
fn original_task_final_readback_drops_only_dead_decoder_suffix_and_keeps_actual_facts_until_last_ack_clone(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(seed)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope = scope();
        let policy = policy();
        let record = changed();
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &policy, Some(&ledger));
        material.charge_writer_material(1024, 4, 1)?;
        let proof = prepare_original_task_write_using_material_tx(
            tx,
            &scope,
            &keys,
            false,
            &policy,
            &ledger,
            &mut material,
        )?;
        let before = material.material_estimate();
        let original = proof.task_before(&record.id)?.clone();
        repositories::upsert_task_tx(tx, &record)?;
        let mut peak = None;
        let facts = material.with_original_final_readback(|material| {
            let facts = register_task_writes_inner(
                tx,
                &proof,
                &keys,
                &AgentState::new("task-agent"),
                &policy,
                Some(&ledger),
                None,
                None,
                None,
                Some(material),
            )?;
            peak = Some(material.phase_peak_for_test());
            Ok(facts)
        })?;
        let peak = peak.context("actual original decoder peak absent")?;
        let held = material.material_estimate();
        assert!(held.bytes < peak.bytes && held.records < peak.records);
        assert!(held.bytes > before.bytes && held.records > before.records);
        assert_eq!(
            proof.task_before(&record.id)?,
            &original,
            "original before remains held and cannot be retired"
        );
        assert_eq!(facts.delta.records.len(), 24);
        assert_eq!(facts.delta.coverage.terminal.keys.len(), 24);
        assert!(facts
            .delta
            .records
            .iter()
            .any(|fact| matches!(&fact.fact,CanonicalTaskFact::Task(actual) if actual==&record)));
        let guards = material.into_guards();
        let last_guards = guards.clone();
        let delta = facts.delta;
        let last_delta = delta.clone();
        drop((guards, delta, proof));
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last_delta);
        assert!(
            ledger.report()?.observed_bytes > 0,
            "actual native/coverage guards must survive a dropped body holder"
        );
        drop(last_guards);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn original_task_final_readback_still_admits_its_full_peak_and_rolls_back_before_retiring_any_material(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(seed)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let record = changed();
    let scope = scope();
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
    let before = db.transaction(|tx| current_coverage(tx, &scope))?;
    let peak = {
        let mut connection = db.connection()?;
        let tx = connection.transaction()?;
        let policy = policy();
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &policy, Some(&ledger));
        material.charge_writer_material(1024, 4, 1)?;
        let proof = prepare_original_task_write_using_material_tx(
            &tx,
            &scope,
            &keys,
            false,
            &policy,
            &ledger,
            &mut material,
        )?;
        repositories::upsert_task_tx(&tx, &record)?;
        let mut peak = None;
        let facts = material.with_original_final_readback(|material| {
            let facts = register_task_writes_inner(
                &tx,
                &proof,
                &keys,
                &AgentState::new("task-agent"),
                &policy,
                Some(&ledger),
                None,
                None,
                None,
                Some(material),
            )?;
            peak = Some(material.phase_peak_for_test().bytes);
            Ok(facts)
        })?;
        drop((facts, proof, material));
        tx.rollback()?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
        peak.context("readback peak absent")?
    };
    let result: Result<()> = db.transaction(|tx| {
        let mut policy = policy();
        policy.byte_limit = peak - 1;
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &policy, Some(&ledger));
        material.charge_writer_material(1024, 4, 1)?;
        let proof = prepare_original_task_write_using_material_tx(
            tx,
            &scope,
            &keys,
            false,
            &policy,
            &ledger,
            &mut material,
        )?;
        repositories::upsert_task_tx(tx, &record)?;
        register_original_task_writes_using_material_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &policy,
            &ledger,
            &mut material,
        )?;
        anyhow::bail!("readback skipped its actual simultaneous decoder peak")
    });
    let error = result
        .err()
        .context("one byte below real decoder peak must reject")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(db.transaction(|tx| current_coverage(tx, &scope))?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_ne!(
        db.tasks()
            .latest(&record.id)?
            .context("old actual Task vanished")?
            .summary,
        record.summary
    );
    Ok(())
}

#[test]
fn original_task_final_readback_refuses_an_escaped_actual_decoder_guard_and_does_not_retire_its_prefix(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(seed)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let scope = scope();
    let policy = policy();
    let record = changed();
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
    let before = db.transaction(|tx| current_coverage(tx, &scope))?;
    let mut connection = db.connection()?;
    let tx = connection.transaction()?;
    let mut material = read_preparation::TaskReadMaterial::new(&scope, &policy, Some(&ledger));
    material.charge_writer_material(1024, 4, 1)?;
    let proof = prepare_original_task_write_using_material_tx(
        &tx,
        &scope,
        &keys,
        false,
        &policy,
        &ledger,
        &mut material,
    )?;
    repositories::upsert_task_tx(&tx, &record)?;
    let mut escaped = None;
    let mut before_reject = None;
    let error = material
        .with_original_final_readback(|material| {
            let facts = register_task_writes_inner(
                &tx,
                &proof,
                &keys,
                &AgentState::new("task-agent"),
                &policy,
                Some(&ledger),
                None,
                None,
                None,
                Some(material),
            )?;
            escaped = Some(material.charge_stops(1024, 1)?);
            before_reject = Some(material.material_estimate());
            Ok(facts)
        })
        .err()
        .context("held decoder guard must forbid retirement")?;
    assert!(format!("{error:#}").contains("another live holder"));
    let before_reject = before_reject.context("actual held decoder counter absent")?;
    let peak = material.phase_peak_for_test();
    assert!(peak.bytes >= before_reject.bytes && peak.records >= before_reject.records);
    assert!(peak.bytes <= policy.byte_limit && peak.records <= policy.record_limit);
    let held = material.material_estimate();
    assert_eq!((held.bytes, held.records), (before_reject.bytes, before_reject.records));
    assert_eq!(proof.task_before(&record.id)?.id, record.id);
    drop((proof, material));
    assert!(ledger.report()?.observed_bytes > 0);
    drop(escaped);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    tx.rollback()?;
    assert_eq!(db.transaction(|tx| current_coverage(tx, &scope))?, before);
    Ok(())
}
