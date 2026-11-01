use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime_db::scheduling_snapshot::tests::{ledger, legacy, request, scope, timer};
use crate::types::TimerStatus;

// Independent JSON tree/scalar accounting; never sample a successful installer.
fn clone_cost(value: &impl serde::Serialize) -> Result<(u64, usize)> {
    fn slots(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(_) => 1,
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(slots).sum::<usize>()
            }
            serde_json::Value::Object(values) => {
                2 + 2 * values.len()
                    + values.len().saturating_sub(1)
                    + values.values().map(slots).sum::<usize>()
            }
            _ => 0,
        }
    }
    let slots = slots(&serde_json::to_value(value)?);
    Ok((
        (serde_json::to_vec(value)?.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots + 16) * 8,
    ))
}

fn vector_cost<T>(count: usize) -> (u64, usize) {
    (
        (count * std::mem::size_of::<T>() + 128) as u64 * 8,
        (count + 1) * 8,
    )
}

fn parts(
    proof: &SchedulingWriteProof<'_, '_>,
    snapshot: &SchedulingCurrentSnapshot,
) -> Result<[(u64, usize); 3]> {
    Ok([
        vector_cost::<Arc<QualifiedSchedulingFact>>(
            snapshot
                .resident
                .coverage()
                .observations
                .iter()
                .filter(|observation| observation.availability == SchedulingAvailability::Present)
                .count(),
        ),
        vector_cost::<Arc<BudgetViewGuard>>(proof.guards.len() + snapshot._read_material.len() + 2),
        clone_cost(&proof.scope)?,
    ])
}

fn sum(parts: &[(u64, usize)]) -> (u64, usize) {
    parts.iter().fold((0, 0), |(bytes, records), part| {
        (bytes + part.0, records + part.1)
    })
}

#[test]
fn selected_install_each_projection_uses_original_remainder_before_copy() -> Result<()> {
    for populated in [false, true] {
        let (_home, db) = legacy(true, |tx| {
            if populated {
                repositories::upsert_timer_tx(tx, &timer("selected", TimerStatus::Active))?;
            }
            Ok(())
        })?;
        let ledger = ledger()?;
        let scope = scope();
        let request = request();
        let keys = BTreeSet::new();
        let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
        for gate in 0..3 {
            for bytes_bound in [false, true] {
                db.transaction(|tx| {
                    let snapshot =
                        prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger)?
                            .require_current()?;
                    let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
                    let costs = parts(&proof, &snapshot)?;
                    let upto = sum(&costs[..=gate]);
                    let phase = ResourceReadMaterial::with_test_limits(
                        &ledger,
                        &scope,
                        if bytes_bound {
                            header.0 + upto.0 - 1
                        } else {
                            64 * 1024 * 1024
                        },
                        if bytes_bound {
                            65_536
                        } else {
                            header.1 + upto.1 - 1
                        },
                    )?;
                    let weak = Arc::downgrade(&phase.guard()?);
                    let error = {
                        let _entered = phase.enter();
                        selected_install(&proof, snapshot, vec![]).unwrap_err()
                    };
                    assert!(
                        format!("{error:#}").contains("ResourceExhausted"),
                        "{gate}: {error:#}"
                    );
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard()?.is_none());
                    drop(phase);
                    assert!(
                        weak.upgrade().is_none(),
                        "partial carrier retained phase at gate {gate}"
                    );
                    Ok(())
                })?;
            }
        }
    }
    Ok(())
}

#[test]
fn selected_install_accumulates_and_receipt_last_holder_keeps_phase() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("selected", TimerStatus::Active))
    })?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    db.transaction(|tx| {
        // Real SQL sources are prepared before entering this projection-only phase.
        drop(
            prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger)?
                .require_current()?,
        );
        let mut snapshots = Vec::new();
        for _ in 0..4 {
            snapshots.push(
                prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger)?
                    .require_current()?,
            );
        }
        let keys = BTreeSet::new();
        let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
        let costs = parts(&proof, &snapshots[0])?;
        for snapshot in &snapshots {
            assert_eq!(parts(&proof, snapshot)?, costs);
        }
        let total = sum(&costs);
        let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            header.0 + 2 * total.0,
            header.1 + 2 * total.1,
        )?;
        let weak = Arc::downgrade(&phase.guard()?);
        let mut receipts = Vec::new();
        for snapshot in snapshots {
            let source: Vec<_> = snapshot.resident.records_for_test().cloned().collect();
            let selected = {
                let _entered = phase.enter();
                selected_install(&proof, snapshot, vec![])
            };
            if receipts.len() < 2 {
                let receipt = selected?;
                assert_eq!(receipt.delta.scope, scope);
                assert_eq!(receipt.delta.coverage.revision, receipt.delta.revision);
                assert_eq!(receipt.delta.records.len(), source.len());
                for (actual, original) in receipt.delta.records.iter().zip(&source) {
                    assert!(Arc::ptr_eq(actual, original));
                }
                receipts.push(receipt);
                assert_eq!(
                    phase.finish()?,
                    (
                        header.0 + receipts.len() as u64 * total.0,
                        header.1 + receipts.len() * total.1,
                    )
                );
            } else {
                let error = selected.unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(phase.finish().is_err());
            }
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let mut receipt = receipts.pop().unwrap();
        let held = Arc::new(receipt.take_budget_views());
        let last = held.clone();
        drop(receipts);
        drop(receipt);
        drop(phase);
        assert!(weak.upgrade().is_some());
        drop(held);
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
        Ok(())
    })
}

#[test]
fn selected_install_real_writer_preserves_revision_and_receipt_phase() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("selected", TimerStatus::Active))
    })?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 64 * 1024 * 1024, 65_536)?;
    let weak = Arc::downgrade(&phase.guard()?);
    let keys = BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "selected")]);
    let mut changed = timer("selected", TimerStatus::Active);
    changed.fire_count = 6;
    let mut receipt = db.transaction(|tx| {
        let _entered = phase.enter();
        let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &ledger)?;
        repositories::upsert_timer_tx(tx, &changed)?;
        let state = ContextDurableRead::new(tx, &scope.agent_id)
            .scope_facts()?
            .state
            .unwrap();
        register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)
    })?;
    assert_eq!(
        (receipt.delta.base_revision, receipt.delta.revision),
        (1, 2)
    );
    assert!(receipt.requires_prepared_domain);
    assert_eq!(receipt.delta.records.len(), 1);
    assert!(
        matches!(&receipt.delta.records[0].fact, CanonicalSchedulingFact::Timer(t) if t == &changed)
    );
    phase.finish()?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let guards = receipt.take_budget_views();
    drop(phase);
    assert!(weak.upgrade().is_some());
    drop(receipt);
    assert!(weak.upgrade().is_some());
    drop(guards);
    assert!(weak.upgrade().is_none());
    Ok(())
}
