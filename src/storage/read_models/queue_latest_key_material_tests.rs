use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::context_inputs::ContextDurableRead,
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-latest-key".into(),
        identity_incarnation: 1,
    }
}

fn sql_cost(item: &WorkItemRecord) -> Result<(u64, usize)> {
    let cells = shape(&serde_json::to_value(item)?);
    Ok((
        (4 * std::mem::size_of::<WorkItemRecord>() as u64
            + serde_json::to_vec(item)?.len() as u64 * 4
            + cells as u64 * 128
            + 512)
            * 8,
        (4 + cells * 4 + 16) * 8,
    ))
}

fn slots_cost() -> (u64, usize) {
    (
        (4 * std::mem::size_of::<(String, WorkItemRecord, usize)>()) as u64 * 8,
        32,
    )
}

#[test]
fn latest_slots_sql_callers_reject_both_axes_after_key_before_index_allocation() -> Result<()> {
    for terminal in [false, true] {
        let (_home, db, item) = fixture(terminal)?;
        let prefix = add(add(header(), sql_cost(&item)?), key_cost(&item.id));
        let full = add(prefix, slots_cost());
        for route in 0..3 {
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    full.0 - u64::from(bytes),
                    full.1 - usize::from(!bytes),
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    assert!(Arc::ptr_eq(
                        &holder,
                        &ResourceReadMaterial::active_guard()?.unwrap()
                    ));
                    let error = read(&db, route).unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        "queue latest slots admission",
                        "route={route}"
                    );
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                    assert!(read(&db, route).is_err());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                }
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn latest_slots_sql_callers_keep_exact_cost_through_consumption_and_last_holder() -> Result<()> {
    for terminal in [false, true] {
        let (_home, db, item) = fixture(terminal)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        // Isolate the unchanged downstream projection from SQL/key/index material.
        let baseline = ResourceReadMaterial::new(&ledger, &scope())?;
        let expected = {
            let _entry = baseline.enter();
            derive_context_work_queue(
                None,
                [(item.id.clone(), item.clone())].into_iter().collect(),
                vec![],
                vec![],
                vec![],
            )?
        };
        let total = add(
            add(
                add(baseline.finish()?, sql_cost(&item)?),
                key_cost(&item.id),
            ),
            slots_cost(),
        );
        drop(baseline);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        for route in 0..3 {
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
            let holder = phase.guard()?;
            let queue = {
                let _entry = phase.enter();
                let queue = read(&db, route)?;
                assert!(Arc::ptr_eq(
                    &holder,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                queue
            };
            assert_eq!(queue.items, expected.items);
            assert_eq!(queue.completed_recent, expected.completed_recent);
            assert_eq!(phase.finish()?, total);
            drop(phase);
            let json = serde_json::to_vec(&queue.items)?;
            assert_eq!(
                serde_json::from_slice::<Vec<WorkItemSchedulingProjection>>(&json)?,
                queue.items
            );
            let last = holder.clone();
            drop((queue, holder));
            assert_eq!(ledger.report()?.observed_bytes, total.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn key_cost(id: &str) -> (u64, usize) {
    ((std::mem::size_of::<String>() + id.len()) as u64 * 8, 8)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

#[test]
fn latest_key_exact_cost_copies_only_utf8_length_and_keeps_original_phase() -> Result<()> {
    let ids = ["", "same", "same", "汉字\0🦀\"\\", &"long中".repeat(2048)];
    let total = ids.iter().fold(header(), |sum, id| add(sum, key_cost(id)));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let holder = phase.guard()?;
    let mut keys = Vec::new();
    {
        let _entry = phase.enter();
        for id in ids {
            let mut original = id.to_owned();
            original.reserve(137);
            let key = clone_queue_work_item_key(&original)?;
            assert_eq!(key, original);
            assert_eq!(key.capacity(), key.len());
            if !id.is_empty() {
                assert_ne!(key.as_ptr(), original.as_ptr());
            }
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            keys.push(key);
        }
    }
    assert_eq!(phase.finish()?, total);
    drop((keys, phase));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn latest_key_each_axis_rejects_before_clone_and_latches_after_prior_success() -> Result<()> {
    for id in ["", "汉字\0🦀", &"long-key".repeat(2048)] {
        let first = key_cost("already-admitted");
        let prefix = add(header(), first);
        let full = add(prefix, key_cost(id));
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                full.0 - u64::from(bytes),
                full.1 - usize::from(!bytes),
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert_eq!(
                    clone_queue_work_item_key("already-admitted")?,
                    "already-admitted"
                );
                for _ in 0..2 {
                    let error = clone_queue_work_item_key(id).unwrap_err();
                    assert_eq!(error.to_string(), "queue latest key admission");
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        // An ordinary read does not create material, even after a failed phase.
        assert_eq!(clone_queue_work_item_key(id)?, id);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}

fn fixture(terminal: bool) -> Result<(tempfile::TempDir, RuntimeDb, WorkItemRecord)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut item = WorkItemRecord::new(
        "web",
        "original key source",
        if terminal {
            WorkItemState::Completed
        } else {
            WorkItemState::Open
        },
    );
    item.id = "key-汉字\0🦀".repeat(17);
    assert!(db.work_items().insert_new(&item)?);
    Ok((home, db, item))
}

fn read(db: &RuntimeDb, route: usize) -> Result<WorkQueueReadModel> {
    let view = RuntimeReadModels::borrowed_for_agent(db, "web")?;
    match route {
        0 => view.work_queue(),
        1 => view.work_queue_with_observed_scheduling(&AgentState::new("web"), &[]),
        2 => {
            let mut connection = db.connection()?;
            let tx = connection.transaction()?;
            ContextDurableRead::new(&tx, "web").work_queue()
        }
        _ => unreachable!(),
    }
}

fn shape(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            2 + fields.len() * 2
                + fields.len().saturating_sub(1)
                + fields.values().map(shape).sum::<usize>()
        }
        serde_json::Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
        }
        serde_json::Value::String(_) => 1,
        _ => 0,
    }
}

#[test]
fn latest_key_sql_and_coherent_callers_reject_both_axes_on_open_and_terminal_rows() -> Result<()> {
    for terminal in [false, true] {
        let (_home, db, item) = fixture(terminal)?;
        let cells = shape(&serde_json::to_value(&item)?);
        let sql = (
            (4 * std::mem::size_of::<WorkItemRecord>() as u64
                + serde_json::to_vec(&item)?.len() as u64 * 4
                + cells as u64 * 128
                + 512)
                * 8,
            (4 + cells * 4 + 16) * 8,
        );
        let prefix = add(header(), sql);
        let full = add(prefix, key_cost(&item.id));
        for route in 0..3 {
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    full.0 - u64::from(bytes),
                    full.1 - usize::from(!bytes),
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    assert!(Arc::ptr_eq(
                        &holder,
                        &ResourceReadMaterial::active_guard()?.unwrap()
                    ));
                    let error = read(&db, route).unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        "queue latest key admission",
                        "route={route}"
                    );
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                    assert!(read(&db, route).is_err());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                }
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn latest_key_callers_preserve_payload_id_last_wins_and_original_snapshot_holder() -> Result<()> {
    let (_home, db, open) = fixture(false)?;
    let mut terminal = open.clone();
    terminal.id = "physical-terminal".into();
    terminal.state = WorkItemState::Completed;
    terminal.objective = "last payload wins".into();
    assert!(db.work_items().insert_new(&terminal)?);
    terminal.id = open.id.clone();
    db.connection()?.execute(
        "UPDATE work_items SET payload_json = ?1 WHERE work_item_id = 'physical-terminal'",
        [serde_json::to_string(&terminal)?],
    )?;
    let latest = [(terminal.id.clone(), terminal.clone())]
        .into_iter()
        .collect();
    let expected = derive_context_work_queue(None, latest, vec![], vec![], vec![])?;
    for route in 0..3 {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let queue = {
            let _entry = phase.enter();
            let queue = read(&db, route)?;
            assert!(Arc::ptr_eq(
                &original,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            queue
        };
        assert_eq!(queue.items, expected.items);
        assert_eq!(queue.completed_recent, expected.completed_recent);
        assert_eq!(queue.items.len(), 1);
        assert_eq!(queue.items[0].work_item, terminal);
        assert!(queue.current.is_none());
        let total = phase.finish()?;
        let encoded = serde_json::to_vec(&queue.items)?;
        assert_eq!(
            serde_json::from_slice::<Vec<WorkItemSchedulingProjection>>(&encoded)?,
            queue.items
        );
        drop((queue, phase));
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
