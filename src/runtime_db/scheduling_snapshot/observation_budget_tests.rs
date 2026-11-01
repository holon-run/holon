use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime_db::scheduling_snapshot::tests::{ledger, legacy, request, scope, timer, wake};
use crate::types::TimerStatus;

fn cost(payload: usize, records: usize) -> (u64, usize) {
    (
        ((payload + "scheduling-agent".len() + 128) * 8) as u64,
        records * 8,
    )
}

fn header() -> (u64, usize) {
    ((("scheduling-agent".len() + 256) * 8) as u64, 64)
}

// These fixture IDs contain no controls or backslashes; only quotes expand.
fn wake_key_bytes(id: &str) -> usize {
    r#"{"family":"timer_wake","id":""}"#.len()
        + id.len()
        + id.bytes().filter(|byte| *byte == b'"').count()
}

#[test]
fn observation_charges_each_owned_key_and_wake_scratch_once() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let timer = SchedulingKey::new(SchedulingFamily::Timer, "observe-timer");
    let wake = SchedulingKey::wake("observe-timer", "observe-msg");
    let timer_cost = cost(r#"{"family":"timer","id":"observe-timer"}"#.len(), 1);
    let wake_cost = cost(wake_key_bytes(&wake.id), 1);
    let scratch = cost(r#"["observe-timer","observe-msg"]"#.len() * 2, 3);
    let expected = (
        timer_cost.0 + wake_cost.0 + scratch.0,
        timer_cost.1 + wake_cost.1 + scratch.1,
    );
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        header().0 + expected.0,
        header().1 + expected.1,
    )?;
    let weak = Arc::downgrade(&phase.guard()?);
    let keys = BTreeSet::from([timer, wake]);
    let material = db.transaction(|tx| {
        let _entry = phase.enter();
        let mut before = BTreeMap::new();
        let mut material = Material::new(&scope, &request, &ledger);
        observe_keys(tx, &scope, &keys, false, &[], &mut material, &mut before)?;
        assert_eq!((material.bytes as u64, material.records), expected);
        assert_eq!(before.len(), 2);
        assert!(before.values().all(Option::is_none));
        assert_eq!(
            phase.finish()?,
            (header().0 + expected.0, header().1 + expected.1)
        );
        // A repeated observation cannot read SQL or spend another key/scratch slot.
        tx.execute("DROP TABLE scheduling_domain_records", [])?;
        observe_keys(tx, &scope, &keys, false, &[], &mut material, &mut before)?;
        assert_eq!((material.bytes as u64, material.records), expected);
        Ok(material)
    })?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(material);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn observation_phase_gates_precede_wake_decode_and_parent_validation() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    for (id, expected_error) in [
        ("[", "EOF while parsing"),
        (r#"["timer", "msg"]"#, "noncanonical wake key"),
        (
            r#"["timer","msg"]"#,
            "Scheduling Wake proof omitted its owning Timer",
        ),
    ] {
        let key = SchedulingKey::new(SchedulingFamily::TimerWake, id);
        let key_cost = cost(wake_key_bytes(id), 1);
        let scratch = cost(id.len() * 2, 3);
        for bytes_bound in [false, true] {
            for stage in 0..3 {
                let allowance = match stage {
                    0 => (key_cost.0 - 8, key_cost.1 - 8),
                    1 => (key_cost.0 + scratch.0 - 8, key_cost.1 + scratch.1 - 8),
                    _ => (key_cost.0 + scratch.0, key_cost.1 + scratch.1),
                };
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope,
                    if bytes_bound {
                        header().0 + allowance.0
                    } else {
                        1024 * 1024
                    },
                    if bytes_bound {
                        8192
                    } else {
                        header().1 + allowance.1
                    },
                )?;
                db.transaction(|tx| {
                    let _entry = phase.enter();
                    let keys = BTreeSet::from([key.clone()]);
                    let mut before = BTreeMap::new();
                    let mut material = Material::new(&scope, &request, &ledger);
                    let error =
                        observe_keys(tx, &scope, &keys, false, &[], &mut material, &mut before)
                            .unwrap_err();
                    let text = format!("{error:#}");
                    assert!(
                        text.contains(if stage < 2 {
                            "ResourceExhausted: context resource read material"
                        } else {
                            expected_error
                        }),
                        "{id:?} bytes={bytes_bound} stage={stage}: {text}"
                    );
                    assert_eq!(phase.finish().is_err(), stage < 2);
                    assert!(before.is_empty());
                    Ok(())
                })?;
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn observation_local_key_and_scratch_bounds_precede_sql_and_error_copy() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let wake = SchedulingKey::wake("timer", "msg");
    let key_cost = cost(wake_key_bytes(&wake.id), 1);
    let scratch = cost(wake.id.len() * 2, 3);
    for bytes_bound in [false, true] {
        for stage in 0..2 {
            let mut request = request();
            let allowance = if stage == 0 {
                (key_cost.0 - 1, key_cost.1 - 1)
            } else {
                (key_cost.0 + scratch.0 - 1, key_cost.1 + scratch.1 - 1)
            };
            if bytes_bound {
                request.byte_limit = allowance.0 as usize;
            } else {
                request.record_limit = allowance.1;
            }
            db.transaction(|tx| {
                let mut material = Material::new(&scope, &request, &ledger);
                let mut before = BTreeMap::new();
                let error = observe_keys(
                    tx,
                    &scope,
                    &BTreeSet::from([wake.clone()]),
                    false,
                    &[],
                    &mut material,
                    &mut before,
                )
                .unwrap_err();
                assert!(
                    format!("{error:#}").contains("combined read material"),
                    "{error:#}"
                );
                assert!(before.is_empty());
                Ok(())
            })?;
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    db.transaction(|tx| {
        tx.execute(
            "ALTER TABLE scheduling_domain_records RENAME TO observation_alias_sentinel",
            [],
        )?;
        let request = request();
        let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "sql")]);
        for admitted in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                1024 * 1024,
                header().1 + if admitted { 8 } else { 0 },
            )?;
            let _entry = phase.enter();
            let mut material = Material::new(&scope, &request, &ledger);
            let mut before = BTreeMap::new();
            let error = observe_keys(tx, &scope, &keys, false, &[], &mut material, &mut before)
                .unwrap_err();
            assert!(
                format!("{error:#}").contains(if admitted {
                    "no such table: scheduling_domain_records"
                } else {
                    "ResourceExhausted: context resource read material"
                }),
                "{error:#}"
            );
            assert!(before.is_empty());
        }
        tx.execute(
            "ALTER TABLE observation_alias_sentinel RENAME TO scheduling_domain_records",
            [],
        )?;
        Ok(())
    })?;
    // An unqualified raw row fails with an owned error key, not a map entry.
    db.transaction(|tx| {
        let record = timer("raw", TimerStatus::Active);
        repositories::upsert_timer_tx(tx, &record)?;
        let request = request();
        let mut material = Material::new(&scope, &request, &ledger);
        let mut before = BTreeMap::new();
        let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "raw")]);
        let error = observe_keys(
            tx, &scope, &keys, false, &[], &mut material, &mut before,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<SchedulingSnapshotError>().is_some_and(
            |error| matches!(error, SchedulingSnapshotError::NeedsPreparation(key) if key == keys.first().unwrap())
        ));
        assert_eq!(
            (material.bytes as u64, material.records),
            cost(r#"{"family":"timer","id":"raw"}"#.len(), 1)
        );
        assert!(before.is_empty());
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn failed_reservation_retains_observed_prefix_and_its_original_guards() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let mut phase = Some(ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        1024 * 1024,
        8192,
    )?);
    let weak = Arc::downgrade(&phase.as_ref().unwrap().guard()?);
    db.transaction(|tx| {
        let mut proof =
            prepare_scheduling_write_tx(tx, &scope, &BTreeSet::new(), &request, &ledger)?;
        let prior = (proof.material_bytes, proof.material_records);
        let prefix = SchedulingKey::new(SchedulingFamily::Timer, "observed-prefix");
        let keys = BTreeSet::from([prefix.clone(), SchedulingKey::wake("omitted", "msg")]);
        {
            let _entry = phase.as_ref().unwrap().enter();
            let error = reserve_scheduling_write_keys_tx(tx, &mut proof, &keys, &request, None)
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("Scheduling Wake proof omitted its owning Timer"),
                "{error:#}"
            );
        }
        assert_eq!(proof.before.len(), 1);
        assert!(proof.before.get(&prefix).is_some_and(Option::is_none));
        assert!(proof.material_bytes > prior.0 && proof.material_records > prior.1);
        assert!(proof
            .guards
            .iter()
            .any(|guard| Arc::ptr_eq(guard, &weak.upgrade().unwrap())));
        drop(phase.take());
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(weak.upgrade().is_some());
        drop(proof);
        assert!(weak.upgrade().is_none());
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn observation_reservation_guards_survive_real_wake_writer_receipt() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let record = timer("observed-receipt", TimerStatus::Active);
    let (mut receipt, weak) = db.transaction(|tx| {
        let timer_key = SchedulingKey::new(SchedulingFamily::Timer, &record.id);
        let mut proof = prepare_scheduling_write_tx(
            tx,
            &scope,
            &BTreeSet::from([timer_key.clone()]),
            &request,
            &ledger,
        )?;
        let weak = Arc::downgrade(&proof.preparation_phase.as_ref().unwrap().guard()?);
        let keys = BTreeSet::from([timer_key, SchedulingKey::wake(&record.id, "observed-msg")]);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        reserve_scheduling_write_keys_tx(tx, &mut proof, &keys, &request, None)?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(proof.before.len(), 2);
        repositories::upsert_timer_tx(tx, &record)?;
        wake(tx, &record.id, "observed-msg", record.fire_count)?;
        let state = ContextDurableRead::new(tx, &scope.agent_id)
            .scope_facts()?
            .state
            .unwrap();
        let receipt = register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)?;
        assert!(receipt
            .guards
            .iter()
            .any(|guard| Arc::ptr_eq(guard, &weak.upgrade().unwrap())));
        Ok((receipt, weak))
    })?;
    assert!(receipt.delta.records.iter().any(|body| {
        matches!(&body.fact, CanonicalSchedulingFact::TimerWake(wake) if wake.timer_id == record.id && wake.message_id == "observed-msg")
    }));
    let guards = receipt.take_budget_views();
    drop(receipt);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(guards);
    assert!(weak.upgrade().is_none());
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
