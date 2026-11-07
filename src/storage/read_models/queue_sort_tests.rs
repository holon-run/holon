use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{WakeSource, WorkItemPlanStatus},
    work_item_scheduling::derive_owned_work_item_scheduling,
};
use std::{cmp::Ordering, sync::Arc};

fn item(id: String, state: WorkItemState) -> WorkItemRecord {
    let mut item = WorkItemRecord::new("web", "sort 汉字\0🦀", state);
    item.id = id;
    item.plan_status = WorkItemPlanStatus::Ready;
    item.created_at = "2026-10-01T00:00:00Z".parse().unwrap();
    item.updated_at = item.created_at;
    item
}

#[test]
fn queue_sort_comparators_have_unique_id_ties_and_match_stable_sort() {
    let classes = [
        WorkItemCandidateClass::CurrentRunnable,
        WorkItemCandidateClass::TriggeredBlocked,
        WorkItemCandidateClass::QueuedRunnable,
        WorkItemCandidateClass::Yielded,
        WorkItemCandidateClass::WaitingForOperator,
        WorkItemCandidateClass::Blocked,
        WorkItemCandidateClass::Completing,
        WorkItemCandidateClass::CompletedRecent,
    ];
    let mut rows = Vec::new();
    for (class_index, class) in classes.into_iter().enumerate() {
        for index in (0..33).rev() {
            let mut record = item(
                format!("{class_index}-{index:03}-汉字\0🦀"),
                WorkItemState::Open,
            );
            record.created_at += chrono::Duration::seconds(index % 3);
            record.updated_at += chrono::Duration::seconds(index % 5);
            if index % 2 == 0 {
                record.blocked_by = Some("manual".into());
            }
            let mut row = derive_owned_work_item_scheduling(record, false, false, &[], &|_| None);
            row.candidate_class = class;
            row.last_triggered_at = (index % 2 == 0).then_some(row.work_item.updated_at);
            rows.push(row);
        }
    }
    for (index, left) in rows.iter().enumerate() {
        for right in &rows[index + 1..] {
            assert_ne!(left.id, right.id);
            for order in [
                compare_queue_display_order(left, right),
                compare_scheduling_projection_order(left, right),
            ] {
                assert_ne!(order, Ordering::Equal);
            }
        }
    }
    let mut stable = rows.clone();
    stable.sort_by(compare_scheduling_projection_order);
    rows.sort_unstable_by(compare_scheduling_projection_order);
    assert_eq!(rows, stable);
    let mut records = rows
        .into_iter()
        .map(|row| row.work_item)
        .collect::<Vec<_>>();
    let mut stable = records.clone();
    stable.sort_by(compare_queue_display_order);
    records.sort_unstable_by(compare_queue_display_order);
    assert_eq!(records, stable);
}

fn ids(rows: &[WorkItemSchedulingProjection]) -> Vec<&str> {
    rows.iter().map(|row| row.work_item.id.as_str()).collect()
}

#[test]
fn queue_sort_large_equal_timestamps_preserve_full_order_current_and_windows() -> Result<()> {
    let mut latest = QueueLatestWork::new();
    for index in (0..65).rev() {
        for (prefix, state) in [
            ("r", WorkItemState::Open),
            ("o", WorkItemState::Open),
            ("b", WorkItemState::Open),
            ("c", WorkItemState::Completing),
            ("d", WorkItemState::Completed),
        ] {
            let mut record = item(format!("{prefix}-{index:03}"), state);
            if prefix == "o" {
                record.plan_status = WorkItemPlanStatus::NeedsInput;
            } else if prefix == "b" {
                record.blocked_by = Some("manual".into());
            }
            latest.insert(record.id.clone(), record);
        }
    }
    let queue = derive_context_work_queue(Some("r-032"), latest, vec![], vec![], vec![])?;
    let expected = std::iter::once("r-032".to_owned())
        .chain(
            (0..65)
                .filter(|index| *index != 32)
                .map(|index| format!("r-{index:03}")),
        )
        .chain(
            ["o", "b", "c", "d"]
                .into_iter()
                .flat_map(|prefix| (0..65).map(move |index| format!("{prefix}-{index:03}"))),
        )
        .collect::<Vec<_>>();
    assert_eq!(ids(&queue.items), expected);
    let expected_display = ["o", "r", "b"]
        .into_iter()
        .flat_map(|prefix| (0..65).map(move |index| format!("{prefix}-{index:03}")))
        .filter(|id| id != "r-032")
        .collect::<Vec<_>>();
    assert_eq!(
        queue
            .queued_blocked
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        expected_display
    );
    assert_eq!(queue.current.as_ref().unwrap().id, "r-032");
    assert_eq!(queue.current_runnable.as_ref().unwrap().id, "r-032");
    for (rows, prefix, count) in [
        (&queue.queued_runnable, "r", 5),
        (&queue.waiting_for_operator, "o", 3),
        (&queue.blocked, "b", 3),
        (&queue.completing, "c", 3),
        (&queue.completed_recent, "d", 3),
    ] {
        assert_eq!(
            ids(rows),
            (0..count)
                .map(|index| format!("{prefix}-{index:03}"))
                .collect::<Vec<_>>()
        );
    }
    assert!(queue.triggered_blocked.is_empty());
    assert!(queue.yielded.is_empty());
    Ok(())
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-sort".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn total(count: usize) -> (u64, usize) {
    (
        header().0 + (count * std::mem::size_of::<WorkItemSchedulingProjection>()) as u64 * 8,
        header().1 + count * 8,
    )
}

// Task waits suppress display copies; scheduling still retains their summaries.
fn task_inputs(count: usize) -> (QueueLatestWork, Vec<WaitConditionRecord>) {
    let mut latest = QueueLatestWork::new();
    let mut waits = Vec::new();
    for index in (0..count).rev() {
        let mut record = item(format!("item-{index:03}"), WorkItemState::Open);
        record.id.reserve(101);
        record.objective.reserve(137);
        waits.push(WaitConditionRecord {
            id: format!("wait-{index:03}"),
            agent_id: "web".into(),
            work_item_id: Some(record.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: Some("task-fixture".into()),
            waiting_for: "task".into(),
            wake_sources: vec![WakeSource::SystemTick],
            continuation: None,
            created_at: record.created_at,
            updated_at: record.updated_at,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        });
        latest.insert(record.id.clone(), record);
    }
    (latest, waits)
}

#[test]
fn queue_sort_exact_phase_keeps_canonical_allocations_and_last_holder() -> Result<()> {
    for count in [0, 1, 65, 257] {
        let (latest, waits) = task_inputs(count);
        let pointers = latest
            .iter()
            .map(|(id, item)| {
                (
                    id.clone(),
                    (
                        item.id.as_ptr(),
                        item.id.capacity(),
                        item.objective.as_ptr(),
                        item.objective.capacity(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let limit = total(count);
        let summary = super::queue_wait_summary_material_tests::cost(&waits)?;
        let limit = (limit.0 + summary.0, limit.1 + summary.1);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limit.0, limit.1)?;
        let holder = phase.guard()?;
        let queue = {
            let _entry = phase.enter();
            let queue = derive_context_work_queue(None, latest, vec![], waits, vec![])?;
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            queue
        };
        assert_eq!(phase.finish()?, limit);
        assert_eq!(queue.items.len(), count);
        assert!(queue.queued_blocked.is_empty());
        assert!(queue.blocked.is_empty());
        for (index, row) in queue.items.iter().enumerate() {
            assert_eq!(row.id, format!("item-{index:03}"));
            assert_eq!(row.scheduling_state, WorkItemSchedulingState::WaitingTask);
            let item = &row.work_item;
            assert_eq!(
                pointers[&item.id],
                (
                    item.id.as_ptr(),
                    item.id.capacity(),
                    item.objective.as_ptr(),
                    item.objective.capacity()
                )
            );
        }
        drop(queue);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, limit.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn queue_sort_keeps_items_gate_rejection_and_same_phase_latch() -> Result<()> {
    let count = 65;
    let limit = total(count);
    for byte_short in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            limit.0 - u64::from(byte_short),
            limit.1 - usize::from(!byte_short),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for _ in 0..2 {
                let (latest, waits) = task_inputs(count);
                let error =
                    derive_context_work_queue(None, latest, vec![], waits, vec![]).unwrap_err();
                assert!(
                    error.to_string().contains("queue items slots admission"),
                    "{error:#}"
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, header().0);
                let error = phase.finish().unwrap_err();
                assert_eq!(
                    error
                        .downcast_ref::<crate::runtime_error::RuntimeError>()
                        .unwrap()
                        .descriptor()
                        .code,
                    "context_resource_exhausted"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, header().0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
