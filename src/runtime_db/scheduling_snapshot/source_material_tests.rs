use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime_db::scheduling_snapshot::stored::read_stored;
use crate::runtime_db::transitions::PostCommitEffects;

fn phase(ledger: &InputEvidenceBudgetLedger) -> Result<ResourceReadMaterial> {
    ResourceReadMaterial::with_test_limits(ledger, &scope(), 64 * 1024 * 1024, 65_536)
}

fn write_timer(
    db: &RuntimeDb,
    ledger: &InputEvidenceBudgetLedger,
    phase: &ResourceReadMaterial,
    record: &TimerRecord,
) -> Result<PostCommitEffects> {
    db.transaction(|tx| {
        let _entry = phase.enter();
        let scope = scope();
        let request = request();
        let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &record.id)]);
        let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, ledger)?;
        repositories::upsert_timer_tx(tx, record)?;
        let state = ContextDurableRead::new(tx, &scope.agent_id)
            .scope_facts()?
            .state
            .unwrap();
        let mut receipt = register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)?;
        let mut effects = PostCommitEffects::default();
        effects.scheduling_material_budget_views = receipt.take_budget_views();
        effects.scheduling_delta_requires_prepared_domain = receipt.requires_prepared_domain;
        effects.scheduling_delta = Some(Box::new(receipt.delta));
        Ok(effects)
    })
}

#[test]
fn sql_source_survives_delayed_qualification_without_changing_value_or_wire() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("source", TimerStatus::Active))
    })?;
    let ledger = ledger()?;
    let original = phase(&ledger)?;
    let weak = Arc::downgrade(&original.guard()?);
    let key = SchedulingKey::new(SchedulingFamily::Timer, "source");
    let stored = db.transaction(|tx| {
        let scope = scope();
        let request = request();
        let mut material = Material::new(&scope, &request, &ledger);
        let _entry = original.enter();
        read_stored(tx, &key, &scope.agent_id, Some(&mut material)).map(|record| record.unwrap())
    })?;
    drop(original);
    assert!(weak.upgrade().is_some());
    let unrelated = phase(&ledger)?;
    let other = Arc::downgrade(&unrelated.guard()?);
    let body = {
        let _entry = unrelated.enter();
        let body = stored.qualified(&scope(), 1)?;
        assert!(std::ptr::eq(
            Arc::as_ptr(body._source_material.as_ref().unwrap()),
            weak.as_ptr()
        ));
        // The fixed, non-Material reader cannot acquire this unrelated phase.
        let fixed = db.transaction(|tx| {
            read_stored(tx, &key, &scope().agent_id, None).map(|record| record.unwrap())
        })?;
        assert!(fixed.qualified(&scope(), 1)?._source_material.is_none());
        body
    };
    let wire = serde_json::to_value(body.as_ref())?;
    assert_eq!(wire.as_object().unwrap().len(), 3);
    let plain: QualifiedSchedulingFact = serde_json::from_value(wire)?;
    assert!(plain._source_material.is_none());
    assert_eq!(body.as_ref(), &plain);
    drop((unrelated, stored));
    assert!(other.upgrade().is_none());
    assert!(weak.upgrade().is_some());
    drop(body);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn sql_source_body_outlives_effects_resident_capture_and_last_body_holder() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let SchedulingCurrentSnapshot { resident: base, .. } = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &ledger)?.require_current()
    })?;
    let source = phase(&ledger)?;
    let weak = Arc::downgrade(&source.guard()?);
    let record = timer("new-body", TimerStatus::Active);
    let effects = write_timer(&db, &ledger, &source, &record)?;
    let delta = effects.scheduling_delta.as_ref().unwrap();
    assert!(effects
        .scheduling_material_budget_views
        .iter()
        .any(|guard| { std::ptr::eq(Arc::as_ptr(guard), weak.as_ptr()) }));
    let next = base.with_committed(delta)?;
    let capture = next.capture(&SchedulingCaptureRequest::current())?;
    assert!(Arc::ptr_eq(&delta.records[0], &capture.records[0]));
    assert_eq!(capture.timer(&record.id)?, Some(&record));
    let last = capture.records[0].clone();
    drop((source, effects, base, next, capture));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(weak.upgrade().is_some());
    assert!(std::ptr::eq(
        Arc::as_ptr(last._source_material.as_ref().unwrap()),
        weak.as_ptr()
    ));
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn sql_source_replacements_keep_each_reused_body_source_without_old_receipt_lists() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let SchedulingCurrentSnapshot { resident: base, .. } = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &ledger)?.require_current()
    })?;
    let first_phase = phase(&ledger)?;
    let first_source = Arc::downgrade(&first_phase.guard()?);
    let mut first_timer = timer("first", TimerStatus::Active);
    let first_effects = write_timer(&db, &ledger, &first_phase, &first_timer)?;
    let first = base.with_committed(first_effects.scheduling_delta.as_ref().unwrap())?;
    let first_capture = first.capture(&SchedulingCaptureRequest::current())?;
    let second_phase = phase(&ledger)?;
    let second_source = Arc::downgrade(&second_phase.guard()?);
    let second_timer = timer("second", TimerStatus::Active);
    let second_effects = write_timer(&db, &ledger, &second_phase, &second_timer)?;
    let second = first.with_committed(second_effects.scheduling_delta.as_ref().unwrap())?;
    let second_capture = second.capture(&SchedulingCaptureRequest::current())?;
    let key = SchedulingKey::new(SchedulingFamily::Timer, &first_timer.id);
    let reused = second_capture
        .records
        .iter()
        .find(|body| body.locator.key == key)
        .unwrap();
    assert!(Arc::ptr_eq(&first_capture.records[0], reused));
    assert!(std::ptr::eq(
        Arc::as_ptr(reused._source_material.as_ref().unwrap()),
        first_source.as_ptr()
    ));
    let third_phase = phase(&ledger)?;
    let third_source = Arc::downgrade(&third_phase.guard()?);
    first_timer.fire_count += 1;
    let third_effects = write_timer(&db, &ledger, &third_phase, &first_timer)?;
    let third = second.with_committed(third_effects.scheduling_delta.as_ref().unwrap())?;
    let capture = third.capture(&SchedulingCaptureRequest::current())?;
    assert_eq!(capture.timer(&first_timer.id)?, Some(&first_timer));
    assert_eq!(capture.timer(&second_timer.id)?, Some(&second_timer));
    drop((
        first_phase,
        second_phase,
        third_phase,
        first_effects,
        second_effects,
        third_effects,
    ));
    drop((base, first, second, third, first_capture, second_capture));
    // No body in the current selection refers to the first allocation phase.
    assert!(first_source.upgrade().is_none());
    assert!(second_source.upgrade().is_some());
    assert!(third_source.upgrade().is_some());
    let last_bodies = capture.records.clone();
    drop(capture);
    assert!(second_source.upgrade().is_some());
    assert!(third_source.upgrade().is_some());
    drop(last_bodies);
    assert!(second_source.upgrade().is_none());
    assert!(third_source.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
