use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial};
use crate::runtime_db::scheduling_snapshot::tests::{ledger, legacy, request, scope, timer};
use crate::types::TimerStatus;

fn exhausted(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("resourceexhausted: context resource read material")
}

#[test]
fn proof_header_and_selector_admission_precede_canonical_decode() -> Result<()> {
    let record = timer("proof-corrupt", TimerStatus::Active);
    let (_home, db) = legacy(true, |tx| repositories::upsert_timer_tx(tx, &record))?;
    db.connection()?.execute(
        "UPDATE timers SET payload_json='{' WHERE timer_id=?1",
        [&record.id],
    )?;
    let scope = scope();
    let request = request();
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &record.id)]);
    // 63 rejects the header; 64 admits only that header, not selector material.
    for records in [63, 64, 65_536] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits {
            record_limit: records,
            ..StateMemoryLimits::default()
        })?;
        let error = db
            .transaction(|tx| {
                prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger).map(|_| ())
            })
            .unwrap_err();
        assert_eq!(exhausted(&error), records < 65_536, "{error:#}");
        if records == 65_536 {
            assert!(
                format!("{error:#}").contains("decoding timer payload from runtime db"),
                "{error:#}"
            );
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let raw: String = db.connection()?.query_row(
            "SELECT payload_json FROM timers WHERE timer_id=?1",
            [&record.id],
            |row| row.get(0),
        )?;
        assert_eq!(raw, "{");
    }
    Ok(())
}

#[test]
fn proof_and_reservation_keep_active_remainder_failure_and_tls() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "active-proof")]);
    let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
    db.transaction(|tx| {
        let mut proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
        for reserve in [false, true] {
            for bytes_bound in [false, true] {
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope,
                    if bytes_bound {
                        header.0 + 8
                    } else {
                        1024 * 1024
                    },
                    if bytes_bound { 8192 } else { header.1 + 8 },
                )?;
                {
                    let _entry = phase.enter();
                    charge_owned_metadata(1, 1)?;
                    assert_eq!(phase.finish()?, (header.0 + 8, header.1 + 8));
                    let observed = ledger.report()?.observed_bytes;
                    for _ in 0..2 {
                        let result = if reserve {
                            reserve_scheduling_write_keys_tx(tx, &mut proof, &keys, &request, None)
                        } else {
                            prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)
                                .map(|_| ())
                        };
                        let error = result.unwrap_err();
                        assert!(exhausted(&error), "{error:#}");
                        assert!(phase.finish().is_err());
                        assert_eq!(ledger.report()?.observed_bytes, observed);
                    }
                    assert!(charge_owned_metadata(0, 0).is_err());
                }
                assert!(ResourceReadMaterial::active_guard()?.is_none());
            }
        }
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn reservation_without_tls_resumes_proof_used_allowance_and_failure() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let keys = BTreeSet::from([SchedulingKey::new(
        SchedulingFamily::Timer,
        "reserved-proof",
    )]);
    let limits = (1024 * 1024, 8192);
    for bytes_bound in [false, true] {
        db.transaction(|tx| {
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope, limits.0, limits.1)?;
            let header = phase.finish()?;
            let mut proof = {
                let _entry = phase.enter();
                prepare_scheduling_write_tx(tx, &scope, &BTreeSet::new(), &request, &ledger)?
            };
            // This equality checks the original Material contract, not a sampled limit.
            for _ in 0..2 {
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                reserve_scheduling_write_keys_tx(tx, &mut proof, &keys, &request, None)?;
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                assert_eq!(
                    phase.finish()?,
                    (
                        header.0 + proof.material_bytes as u64,
                        header.1 + proof.material_records,
                    )
                );
                assert!(proof.before.contains_key(keys.first().unwrap()));
            }
            // Fill the known remaining allowance, independently of the next SQL path.
            let used = phase.finish()?;
            {
                let _entry = phase.enter();
                if bytes_bound {
                    charge_owned_metadata((limits.0 - used.0) / 8, 0)?;
                } else {
                    charge_owned_metadata(0, (limits.1 - used.1) / 8)?;
                }
            }
            let totals = (proof.material_bytes, proof.material_records);
            let observed = ledger.report()?.observed_bytes;
            for _ in 0..2 {
                let error = reserve_scheduling_write_keys_tx(tx, &mut proof, &keys, &request, None)
                    .unwrap_err();
                assert!(exhausted(&error), "{error:#}");
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                assert_eq!((proof.material_bytes, proof.material_records), totals);
                assert_eq!(ledger.report()?.observed_bytes, observed);
            }
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn proof_phase_survives_finalizer_receipt_until_last_guard_holder() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let record = timer("proof-receipt", TimerStatus::Active);
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &record.id)]);
    let (mut receipt, source) = db.transaction(|tx| {
        let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
        let source = Arc::downgrade(&proof.preparation_phase.as_ref().unwrap().guard()?);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        repositories::upsert_timer_tx(tx, &record)?;
        let state = ContextDurableRead::new(tx, &scope.agent_id)
            .scope_facts()?
            .state
            .unwrap();
        let receipt = register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)?;
        assert!(receipt
            .guards
            .iter()
            .any(|guard| Arc::ptr_eq(guard, &source.upgrade().unwrap())));
        Ok((receipt, source))
    })?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(source.upgrade().is_some());
    assert!(receipt.delta.records.iter().any(
        |body| matches!(&body.fact, CanonicalSchedulingFact::Timer(actual) if actual == &record)
    ));
    let guards = receipt.take_budget_views();
    drop(receipt);
    assert!(source.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(guards);
    assert!(source.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
