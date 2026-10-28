use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial};
use crate::runtime_db::scheduling_snapshot::tests::{ledger, legacy, request, scope, timer};
use crate::runtime_db::transitions::PostCommitEffects;
use crate::types::TimerStatus;

#[test]
fn standalone_install_admits_header_and_first_material_before_decode_and_rolls_back() -> Result<()>
{
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let scope = scope();
    let request = request();
    let record = timer("standalone-rejected", TimerStatus::Active);
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &record.id)]);
    // 64 records admit exactly the header; 63 reject before creating it.
    for records in [63, 64, 65_536] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits {
            record_limit: records,
            ..StateMemoryLimits::default()
        })?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let error = db
            .transaction(|tx| {
                let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
                let state = ContextDurableRead::new(tx, &scope.agent_id)
                    .scope_facts()?
                    .state
                    .unwrap();
                repositories::upsert_timer_tx(tx, &record)?;
                tx.execute(
                    "UPDATE timers SET payload_json='{' WHERE timer_id=?1",
                    [&record.id],
                )?;
                register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)
            })
            .unwrap_err();
        assert_eq!(
            error
                .to_string()
                .to_ascii_lowercase()
                .contains("resourceexhausted: context resource read material"),
            records < 65_536,
            "{error:#}"
        );
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let count: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM timers WHERE timer_id=?1",
            [&record.id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
    }
    Ok(())
}

#[test]
fn standalone_install_reuses_active_remainder_and_never_replaces_failed_phase() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let record = timer("active-rejected", TimerStatus::Active);
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &record.id)]);
    let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
    for bytes_bound in [false, true] {
        let rejected: Result<()> = db.transaction(|tx| {
            let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
            repositories::upsert_timer_tx(tx, &record)?;
            let state = ContextDurableRead::new(tx, &scope.agent_id)
                .scope_facts()?
                .state
                .unwrap();
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                if bytes_bound {
                    header.0 + 8
                } else {
                    64 * 1024 * 1024
                },
                if bytes_bound { 65_536 } else { header.1 + 8 },
            )?;
            let source = phase.guard()?;
            {
                let _entry = phase.enter();
                charge_owned_metadata(1, 1)?;
                assert_eq!(phase.finish()?, (header.0 + 8, header.1 + 8));
                let before = ledger.report()?.observed_bytes;
                for _ in 0..2 {
                    let error = register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)
                        .unwrap_err();
                    assert!(
                        error
                            .to_string()
                            .to_ascii_lowercase()
                            .contains("resourceexhausted: context resource read material"),
                        "{error:#}"
                    );
                    assert!(phase.finish().is_err());
                    assert_eq!(ledger.report()?.observed_bytes, before);
                }
                // The failed entry restores the caller's TLS phase, not None.
                assert!(charge_owned_metadata(0, 0).is_err());
            }
            drop((source, phase));
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            // Catching the failures must not commit the unqualified Timer.
            anyhow::bail!("rollback rejected active install")
        });
        assert_eq!(
            rejected.unwrap_err().to_string(),
            "rollback rejected active install"
        );
        let count: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM timers WHERE timer_id=?1",
            [&record.id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
    }
    drop(keys);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn standalone_install_source_survives_receipt_owner_capture_and_last_body() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let SchedulingCurrentSnapshot { resident: base, .. } = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger)?.require_current()
    })?;
    let record = timer("standalone-body", TimerStatus::Active);
    let effects = db.transaction(|tx| {
        let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &record.id)]);
        let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
        repositories::upsert_timer_tx(tx, &record)?;
        let state = ContextDurableRead::new(tx, &scope.agent_id)
            .scope_facts()?
            .state
            .unwrap();
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let mut receipt = register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let mut effects = PostCommitEffects::default();
        effects.scheduling_material_budget_views = receipt.take_budget_views();
        effects.scheduling_delta = Some(Box::new(receipt.delta));
        Ok(effects)
    })?;
    let delta = effects.scheduling_delta.as_ref().unwrap();
    let source = delta.records[0]._source_material.as_ref().unwrap();
    let weak = Arc::downgrade(source);
    assert!(effects
        .scheduling_material_budget_views
        .iter()
        .any(|guard| Arc::ptr_eq(guard, source)));
    let next = base.with_committed(delta)?;
    let capture = next.capture(&SchedulingCaptureRequest::current())?;
    assert!(Arc::ptr_eq(&delta.records[0], &capture.records[0]));
    assert_eq!(capture.timer(&record.id)?, Some(&record));
    let last = capture.records[0].clone();
    drop((effects, base, next, capture));
    assert!(weak.upgrade().is_some());
    assert!(std::ptr::eq(
        Arc::as_ptr(last._source_material.as_ref().unwrap()),
        weak.as_ptr()
    ));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
