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
        agent_id: "queue-latest-slots".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn total(capacity: usize) -> (u64, usize) {
    (
        header().0 + (capacity * std::mem::size_of::<Entry>()) as u64 * 8,
        header().1 + capacity * 8,
    )
}

fn record(id: &str, ordinal: usize) -> WorkItemRecord {
    let mut row = WorkItemRecord::new("web", format!("row-{ordinal}"), WorkItemState::Open);
    row.id = id.into();
    row
}

#[test]
fn latest_slots_exact_geometric_cost_and_moved_records_keep_original_holder() -> Result<()> {
    for (count, capacity) in [(0, 0), (1, 4), (4, 4), (5, 8), (8, 8), (9, 16), (33, 64)] {
        let rows = (0..count)
            .rev()
            .map(|n| {
                let id = format!("key-{n:03}中🦀");
                let row = record(&id, n);
                (id, row)
            })
            .collect::<Vec<_>>();
        let pointers = rows
            .iter()
            .map(|(key, row)| {
                (
                    key.clone(),
                    (key.as_ptr(), row.id.as_ptr(), row.objective.as_ptr()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let exact = total(capacity);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        let latest = {
            let _entry = phase.enter();
            let mut builder = QueueWorkBuilder::default();
            for (key, row) in rows {
                builder.push(key, row)?;
            }
            assert_eq!(builder.0.capacity(), capacity);
            let backing = builder.0.as_ptr();
            let latest = builder.finish();
            assert_eq!(latest.0.as_ptr(), backing);
            assert_eq!(latest.0.capacity(), capacity);
            assert_eq!(latest.len(), count);
            for (key, row, _) in &latest.0 {
                assert_eq!(
                    (key.as_ptr(), row.id.as_ptr(), row.objective.as_ptr()),
                    pointers[key]
                );
                assert!(std::ptr::eq(latest.get(key).unwrap(), row));
            }
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            latest
        };
        assert_eq!(phase.finish()?, exact);
        drop(phase);
        let last = holder.clone();
        drop((latest, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn latest_slots_each_growth_rejects_both_axes_before_reserve_and_latches() -> Result<()> {
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
                let mut builder = QueueWorkBuilder::default();
                for n in 0..count {
                    builder.push(n.to_string(), record(&n.to_string(), n))?;
                }
                let backing = builder.0.as_ptr();
                for _ in 0..2 {
                    let error = builder
                        .push("next".into(), record("next", count))
                        .unwrap_err();
                    assert_eq!(error.to_string(), "queue latest slots admission");
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
                let mut builder = QueueWorkBuilder::default();
                builder.push("ok".into(), record("ok", 0))?;
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
fn latest_slots_duplicates_use_last_source_record_without_sort_scratch_or_copies() -> Result<()> {
    let keys = ["z", "", "中🦀", "a", "", "z", "中🦀", "a", ""];
    let rows = keys
        .iter()
        .enumerate()
        .map(|(n, id)| (id.to_string(), record(id, n)))
        .collect::<Vec<_>>();
    let expected = rows
        .iter()
        .cloned()
        .collect::<std::collections::HashMap<_, _>>();
    let pointers = rows
        .iter()
        .map(|(id, row)| (id.clone(), row.objective.as_ptr()))
        .collect::<std::collections::HashMap<_, _>>();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let exact = total(16);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let latest = {
        let _entry = phase.enter();
        let mut builder = QueueWorkBuilder::default();
        for (id, row) in rows {
            builder.push(id, row)?;
        }
        let backing = builder.0.as_ptr();
        let latest = builder.finish();
        assert_eq!(latest.0.as_ptr(), backing);
        assert_eq!(latest.0.capacity(), 16);
        latest
    };
    assert_eq!(latest.len(), expected.len());
    assert_eq!(
        latest
            .0
            .iter()
            .map(|entry| entry.0.as_str())
            .collect::<Vec<_>>(),
        ["", "a", "z", "中🦀"]
    );
    for (id, row, _) in &latest.0 {
        assert_eq!(row, &expected[id]);
        assert_eq!(row.objective.as_ptr(), pointers[id]);
    }
    assert!(latest.get("missing").is_none());
    assert_eq!(phase.finish()?, exact);
    drop((latest, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn latest_slots_ordinary_read_has_no_phase_and_final_empty_index_allocates_nothing() -> Result<()> {
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let empty = QueueWorkBuilder::default().finish();
    assert!(empty.is_empty());
    assert_eq!(empty.0.capacity(), 0);
    let mut builder = QueueWorkBuilder::default();
    builder.push("same".into(), record("same", 0))?;
    builder.push("same".into(), record("same", 1))?;
    let latest = builder.finish();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest.get("same").unwrap().objective, "row-1");
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
