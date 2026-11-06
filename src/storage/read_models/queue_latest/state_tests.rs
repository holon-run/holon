use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::WorkItemState,
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "state-queue-latest".into(),
        identity_incarnation: 1,
    }
}

fn total(capacity: usize) -> (u64, usize) {
    (
        (scope().agent_id.len() as u64 + 256) * 8
            + (capacity * std::mem::size_of::<StateEntry>()) as u64 * 8,
        64 + capacity * 8,
    )
}

fn record(id: &str, ordinal: usize) -> WorkItemRecord {
    let mut row = WorkItemRecord::new("web", format!("row-{ordinal}"), WorkItemState::Open);
    row.id = id.into();
    row
}

#[test]
fn state_latest_slots_exact_cost_moves_complete_records_and_retains_last_holder() -> Result<()> {
    for (count, capacity) in [(0, 0), (1, 4), (4, 4), (5, 8), (9, 16), (200, 256)] {
        let rows = (0..count)
            .rev()
            .map(|n| record(&format!("key-{n:03}中🦀"), n))
            .collect::<Vec<_>>();
        let expected = rows
            .iter()
            .map(|row| {
                (
                    row.id.clone(),
                    (row.clone(), row.id.as_ptr(), row.objective.as_ptr()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let exact = total(capacity);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        let latest = {
            let _entry = phase.enter();
            let mut builder = StateQueueWorkBuilder::default();
            for row in rows {
                builder.push(row)?;
            }
            assert_eq!(builder.0.capacity(), capacity);
            let backing = builder.0.as_ptr();
            let latest = builder.finish();
            assert_eq!(latest.0.as_ptr(), backing);
            assert_eq!(latest.0.capacity(), capacity);
            assert_eq!(latest.len(), count);
            assert_eq!(latest.is_empty(), count == 0);
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            latest
        };
        let values = latest.into_values().collect::<Vec<_>>();
        for (row, (id, (original, id_pointer, objective_pointer))) in
            values.iter().zip(expected.iter())
        {
            assert_eq!(&row.id, id);
            assert_eq!(row, original);
            assert_eq!(row.id.as_ptr(), *id_pointer);
            assert_eq!(row.objective.as_ptr(), *objective_pointer);
        }
        assert_eq!(phase.finish()?, exact);
        drop((phase, values));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        let last = holder.clone();
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn state_latest_slots_each_growth_rejects_both_axes_before_reserve_and_latches() -> Result<()> {
    for (count, before, after) in [(0, 0, 4), (4, 4, 8), (8, 8, 16)] {
        for bytes in [true, false] {
            let exact = total(after);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(bytes),
                exact.1 - usize::from(!bytes),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                let mut builder = StateQueueWorkBuilder::default();
                for n in 0..count {
                    builder.push(record(&n.to_string(), n))?;
                }
                let backing = builder.0.as_ptr();
                for _ in 0..2 {
                    let error = builder.push(record("next", count)).unwrap_err();
                    assert_eq!(error.to_string(), "state queue latest slots admission");
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(builder.0.len(), count);
                    assert_eq!(builder.0.capacity(), before);
                    assert_eq!(builder.0.as_ptr(), backing);
                    assert_eq!(ledger.report()?.observed_bytes, total(before).0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, total(before).0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let recovered = ResourceReadMaterial::new(&ledger, &scope())?;
            {
                let _entry = recovered.enter();
                let mut builder = StateQueueWorkBuilder::default();
                builder.push(record("ok", 0))?;
                assert_eq!(builder.finish().len(), 1);
            }
            assert_eq!(recovered.finish()?, total(4));
            drop(recovered);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn state_latest_slots_last_source_wins_with_equal_times_and_no_body_copies() -> Result<()> {
    let mut rows = ["z", "", "中🦀", "a", "", "z", "中🦀", "a", ""]
        .iter()
        .enumerate()
        .map(|(n, id)| record(id, n))
        .collect::<Vec<_>>();
    let time = rows[0].created_at;
    for row in &mut rows {
        row.created_at = time;
        row.updated_at = time;
    }
    let expected = rows
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                (row.clone(), row.id.as_ptr(), row.objective.as_ptr()),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let exact = total(16);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let latest = {
        let _entry = phase.enter();
        let mut builder = StateQueueWorkBuilder::default();
        for row in rows {
            builder.push(row)?;
        }
        let backing = builder.0.as_ptr();
        let latest = builder.finish();
        assert_eq!(latest.0.as_ptr(), backing);
        assert_eq!(latest.0.capacity(), 16);
        latest
    };
    assert_eq!(latest.len(), expected.len());
    for ((row, _), (id, (original, id_pointer, objective_pointer))) in
        latest.0.iter().zip(expected.iter())
    {
        assert_eq!(&row.id, id);
        assert_eq!(row, original);
        assert_eq!(row.id.as_ptr(), *id_pointer);
        assert_eq!(row.objective.as_ptr(), *objective_pointer);
    }
    assert_eq!(phase.finish()?, exact);
    drop((latest, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
