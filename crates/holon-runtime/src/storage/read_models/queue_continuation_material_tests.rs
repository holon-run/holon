use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{WorkItemContinuationFrame, WorkItemPlanStatus},
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-continuation".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn total(count: usize) -> (u64, usize) {
    (
        header().0 + (count * std::mem::size_of::<String>()) as u64 * 8,
        header().1 + count * 8,
    )
}

fn frames(count: usize) -> Vec<WorkItemContinuationFrame> {
    (0..count)
        .rev()
        .map(|index| {
            let mut id = format!("suspended-{index:03} 汉字\0🦀");
            id.reserve(137);
            WorkItemContinuationFrame::new_on_completed("web", id, "active", None)
        })
        .collect()
}

#[test]
fn continuation_index_exact_slots_move_ids_and_keep_original_phase() -> Result<()> {
    for count in [1, 8, 65] {
        let frames = frames(count);
        let pointers = frames
            .iter()
            .map(|frame| {
                let id = &frame.suspended_work_item_id;
                (id.clone(), (id.as_ptr() as usize, id.capacity()))
            })
            .collect::<BTreeMap<_, _>>();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let cost = total(count);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
        let original = phase.guard()?;
        let ids = {
            let _entry = phase.enter();
            let ids = queue_continuation_suspended_ids(frames)?;
            assert!(Arc::ptr_eq(
                &original,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            ids
        };
        assert_eq!(ids.len(), count);
        assert!(ids.capacity() >= count);
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        for id in &ids {
            assert_eq!(pointers[id], (id.as_ptr() as usize, id.capacity()));
        }
        assert_eq!(phase.finish()?, cost);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(ids);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn continuation_index_duplicates_charge_input_slots_and_empty_input_is_free() -> Result<()> {
    let input = ["z", "", "汉字\0🦀", "z", ""]
        .into_iter()
        .map(|id| WorkItemContinuationFrame::new_on_completed("web", id, "active", None))
        .collect::<Vec<_>>();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let cost = total(input.len());
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
    {
        let _entry = phase.enter();
        let ids = queue_continuation_suspended_ids(input)?;
        assert_eq!(ids, ["", "z", "汉字\0🦀"]);
        assert!(ids.capacity() >= 5);
        assert_eq!(queue_continuation_suspended_ids(vec![])?.capacity(), 0);
    }
    assert_eq!(phase.finish()?, cost);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    let cost = header();
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
    {
        let _entry = phase.enter();
        assert_eq!(queue_continuation_suspended_ids(vec![])?.capacity(), 0);
    }
    assert_eq!(phase.finish()?, cost);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(queue_continuation_suspended_ids(frames(8))?.len(), 8);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn continuation_index_one_short_rejects_partial_queue_and_latches() -> Result<()> {
    for count in [1, 8, 65] {
        let full = total(count);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { full.0 - 1 } else { full.0 },
                if bytes { full.1 } else { full.1 - 1 },
            )?;
            let original = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &original,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                for _ in 0..2 {
                    let error = derive_context_work_queue(
                        None,
                        QueueLatestWork::new(),
                        vec![],
                        vec![],
                        frames(count),
                    )
                    .unwrap_err();
                    assert!(error
                        .to_string()
                        .contains("queue continuation slots admission"));
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, header().0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, header().0);
            drop(original);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn continuation_index_membership_preserves_focus_yielded_windows_and_terminal_state() -> Result<()>
{
    let mut latest = QueueLatestWork::new();
    let suspended = ["", "a", "b", "c", "d", "e", "汉字\0🦀"];
    for id in suspended.into_iter().chain(["ready", "completed"]) {
        let state = if id == "completed" {
            WorkItemState::Completed
        } else {
            WorkItemState::Open
        };
        let mut item = WorkItemRecord::new("web", id, state);
        item.id = id.into();
        item.plan_status = WorkItemPlanStatus::Ready;
        item.created_at = "2026-10-01T00:00:00Z".parse()?;
        item.updated_at = item.created_at;
        latest.insert(item.id.clone(), item);
    }
    let frames = suspended
        .into_iter()
        .rev()
        .chain(["a", "", "missing", "completed"])
        .map(|id| WorkItemContinuationFrame::new_on_completed("web", id, "ready", None))
        .collect::<Vec<_>>();
    for current in [None, Some("a"), Some("completed")] {
        let queue =
            derive_context_work_queue(current, latest.clone(), vec![], vec![], frames.clone())?;
        assert_eq!(queue.items.len(), 9);
        assert_eq!(
            queue
                .items
                .iter()
                .filter(|row| row.scheduling_state == WorkItemSchedulingState::YieldedToWorkItem)
                .count(),
            7
        );
        assert_eq!(queue.yielded.len(), 5);
        assert_eq!(
            queue
                .yielded
                .iter()
                .map(|row| row.work_item.id.as_str())
                .collect::<Vec<_>>(),
            ["", "a", "b", "c", "d"]
        );
        assert_eq!(queue.queued_runnable.len(), 1);
        assert_eq!(queue.queued_runnable[0].work_item.id, "ready");
        assert_eq!(queue.queued_blocked.len(), 1);
        assert_eq!(queue.queued_blocked[0].id, "ready");
        assert_eq!(queue.completed_recent.len(), 1);
        assert_eq!(
            queue.completed_recent[0].scheduling_state,
            WorkItemSchedulingState::Completed
        );
        assert_eq!(
            queue.current.as_ref().map(|row| row.id.as_str()),
            current.filter(|id| *id != "completed")
        );
        assert!(queue.current_runnable.is_none());
        assert!(queue.blocked.is_empty());
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
