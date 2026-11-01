use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{TodoItem, TodoItemState, WorkItemPlanStatus},
    work_item_scheduling::WorkItemSchedulingProjection,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-display".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn slots<T>(count: usize) -> (u64, usize) {
    ((count * std::mem::size_of::<T>()) as u64 * 8, count * 8)
}

// Count the JSON tree, not the production byte scanner or admission helper.
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

fn clone_cost(value: &impl serde::Serialize) -> Result<(u64, usize)> {
    let bytes = serde_json::to_vec(value)?.len();
    let cells = shape(&serde_json::to_value(value)?);
    Ok((
        (bytes as u64 * 4 + cells as u64 * 128 + 512) * 8,
        (cells + 16) * 8,
    ))
}

fn item(id: &str, state: WorkItemState) -> WorkItemRecord {
    let mut record = WorkItemRecord::new("web", "display 汉字 \0 \"{}[]:,🦀\"", state);
    record.id = id.into();
    record.plan_status = WorkItemPlanStatus::Ready;
    record.created_at = "2026-10-01T00:00:00Z".parse().unwrap();
    record.updated_at = record.created_at;
    record.todo_list = vec![TodoItem {
        text: "nested 🦀 \"{}[]\"".into(),
        state: TodoItemState::InProgress,
    }];
    record
}

#[derive(serde::Serialize)]
struct CloneProbe {
    text: String,
    #[serde(skip)]
    clones: Arc<AtomicUsize>,
}

impl Clone for CloneProbe {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Ordering::SeqCst);
        Self {
            text: self.text.clone(),
            clones: self.clones.clone(),
        }
    }
}

#[test]
fn display_rows_exact_cost_ownership_and_outside_phase_contract() -> Result<()> {
    let records = [
        item("a", WorkItemState::Open),
        item("b", WorkItemState::Completed),
    ];
    let expected = records
        .iter()
        .try_fold(slots::<WorkItemRecord>(2), |sum, row| {
            Ok::<_, anyhow::Error>(add(sum, clone_cost(row)?))
        })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let total = add(header(), expected);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let holder = phase.guard()?;
    let rows = {
        let _entry = phase.enter();
        let rows = clone_queue_display_rows(records.iter())?;
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        rows
    };
    assert_eq!(phase.finish()?, total);
    assert_eq!(rows, records);
    for (row, original) in rows.iter().zip(&records) {
        assert_ne!(row.id.as_ptr(), original.id.as_ptr());
        assert_ne!(row.todo_list.as_ptr(), original.todo_list.as_ptr());
        assert_ne!(
            row.todo_list[0].text.as_ptr(),
            original.todo_list[0].text.as_ptr()
        );
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(rows);
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(clone_queue_display_rows(records.iter())?, records);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn display_slots_and_each_clone_reject_before_allocating_or_cloning_and_latch() -> Result<()> {
    let clones = Arc::new(AtomicUsize::new(0));
    let rows = [
        CloneProbe {
            text: "one 汉字 \"{}[]\"".into(),
            clones: clones.clone(),
        },
        CloneProbe {
            text: "two 🦀 \0".into(),
            clones: clones.clone(),
        },
    ];
    let gates = [
        slots::<CloneProbe>(2),
        clone_cost(&rows[0])?,
        clone_cost(&rows[1])?,
    ];
    let total = gates.iter().copied().fold(header(), add);
    for failed_gate in 0..gates.len() {
        let prefix = gates[..failed_gate].iter().copied().fold(header(), add);
        let boundary = add(prefix, gates[failed_gate]);
        for bytes in [true, false] {
            clones.store(0, Ordering::SeqCst);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { boundary.0 - 1 } else { total.0 },
                if bytes { total.1 } else { boundary.1 - 1 },
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(clone_queue_display_rows(rows.iter()).is_err());
                assert_eq!(clones.load(Ordering::SeqCst), failed_gate.saturating_sub(1));
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(clone_queue_display_rows(rows.iter()).is_err());
                assert_eq!(clones.load(Ordering::SeqCst), failed_gate.saturating_sub(1));
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(resource_io::charge_owned_metadata(0, 0).is_err());
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

fn display_cost(queue: &WorkQueueReadModel) -> Result<(u64, usize)> {
    let mut total = (0, 0);
    if let Some(current) = &queue.current {
        total = add(total, clone_cost(current)?);
    }
    if !queue.queued_blocked.is_empty() {
        total = add(total, slots::<WorkItemRecord>(queue.queued_blocked.len()));
        for row in &queue.queued_blocked {
            total = add(total, clone_cost(row)?);
        }
    }
    if let Some(current) = &queue.current_runnable {
        total = add(total, clone_cost(current)?);
    }
    for rows in [
        &queue.triggered_blocked,
        &queue.queued_runnable,
        &queue.yielded,
        &queue.waiting_for_operator,
        &queue.blocked,
        &queue.completing,
        &queue.completed_recent,
    ] {
        if !rows.is_empty() {
            total = add(total, slots::<WorkItemSchedulingProjection>(rows.len()));
            for row in rows {
                total = add(total, clone_cost(row)?);
            }
        }
    }
    Ok(total)
}

#[test]
fn queue_display_total_keeps_windows_sorting_and_independent_records() -> Result<()> {
    let mut records = QueueLatestWork::new();
    for (prefix, state, count) in [
        ("open", WorkItemState::Open, 9),
        ("completed", WorkItemState::Completed, 6),
        ("completing", WorkItemState::Completing, 6),
    ] {
        for n in 0..count {
            let row = item(&format!("{prefix}{n}"), state.clone());
            records.insert(row.id.clone(), row);
        }
    }
    let plain = derive_context_work_queue(
        Some("open0".into()),
        records.clone(),
        vec![],
        vec![],
        vec![],
    )?;
    let total = add(
        add(header(), display_cost(&plain)?),
        add(
            slots::<WorkItemSchedulingProjection>(plain.items.len()),
            plain
                .items
                .iter()
                .filter_map(|row| row.current_todo.as_ref())
                .try_fold((0, 0), |sum, todo| -> Result<_> {
                    Ok(add(sum, clone_cost(todo)?))
                })?,
        ),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let queue = {
        let _entry = phase.enter();
        derive_context_work_queue(Some("open0".into()), records, vec![], vec![], vec![])?
    };
    assert_eq!(phase.finish()?, total);
    assert_eq!(queue.items, plain.items);
    assert_eq!(queue.queued_blocked, plain.queued_blocked);
    assert_eq!(queue.current_runnable, plain.current_runnable);
    for (rows, ids) in [
        (
            &queue.queued_runnable,
            vec!["open1", "open2", "open3", "open4", "open5"],
        ),
        (
            &queue.completed_recent,
            vec!["completed0", "completed1", "completed2"],
        ),
        (
            &queue.completing,
            vec!["completing0", "completing1", "completing2"],
        ),
    ] {
        assert_eq!(
            rows.iter()
                .map(|row| row.work_item.id.as_str())
                .collect::<Vec<_>>(),
            ids
        );
        for row in rows {
            let canonical = queue
                .items
                .iter()
                .find(|x| x.work_item.id == row.work_item.id)
                .unwrap();
            assert_eq!(row, canonical);
            assert_ne!(
                row.work_item.objective.as_ptr(),
                canonical.work_item.objective.as_ptr()
            );
            assert_ne!(
                row.current_todo.as_ref().unwrap().text.as_ptr(),
                canonical.current_todo.as_ref().unwrap().text.as_ptr()
            );
        }
    }
    assert_eq!(queue.queued_blocked.len(), 8);
    assert!(queue.triggered_blocked.is_empty());
    assert!(queue.yielded.is_empty());
    assert!(queue.waiting_for_operator.is_empty());
    assert!(queue.blocked.is_empty());
    drop((queue, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_display_and_current_runnable_clones_fail_all_or_nothing_on_original_phase() -> Result<()>
{
    let current = item("current", WorkItemState::Open);
    let plain = derive_context_work_queue(
        Some(current.id.as_str()),
        QueueLatestWork::from([(current.id.clone(), current.clone())]),
        vec![],
        vec![],
        vec![],
    )?;
    let gates = [
        clone_cost(&current)?,
        slots::<WorkItemSchedulingProjection>(1),
        clone_cost(&current.todo_list[0])?,
        clone_cost(plain.current_runnable.as_ref().unwrap())?,
    ];
    let total = gates.iter().copied().fold(header(), add);
    for index in 0..gates.len() {
        let prefix = gates[..index].iter().copied().fold(header(), add);
        let boundary = add(prefix, gates[index]);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { boundary.0 - 1 } else { total.0 },
                if bytes { total.1 } else { boundary.1 - 1 },
            )?;
            {
                let _entry = phase.enter();
                for attempt in 0..2 {
                    let error = derive_context_work_queue(
                        Some(current.id.as_str()),
                        QueueLatestWork::from([(current.id.clone(), current.clone())]),
                        vec![],
                        vec![],
                        vec![],
                    )
                    .unwrap_err();
                    assert!(error.to_string().contains(match (index, attempt) {
                        (1, 0) => "queue items slots admission",
                        (2, 0) => "queue current todo clone admission",
                        _ => "queue display clone admission",
                    }));
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn empty_display_lists_do_not_allocate_charge_or_qualify() -> Result<()> {
    let clones = Arc::new(AtomicUsize::new(0));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _entry = phase.enter();
        assert!(clone_queue_display_rows(std::iter::empty::<&CloneProbe>())?.is_empty());
        let empty = derive_context_work_queue(
            Some("absent".into()),
            QueueLatestWork::new(),
            vec![],
            vec![],
            vec![],
        )?;
        assert!(empty.current.is_none());
        assert!(empty.items.is_empty());
        assert_eq!(phase.finish()?, header());
    }
    assert_eq!(clones.load(Ordering::SeqCst), 0);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
