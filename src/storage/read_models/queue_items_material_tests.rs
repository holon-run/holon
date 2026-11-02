use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{WakeSource, WorkItemPlanStatus},
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-items".into(),
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

// Task waits suppress every independent display copy, isolating items slots.
fn inputs(count: usize) -> (QueueLatestWork, Vec<WaitConditionRecord>) {
    let mut records = QueueLatestWork::new();
    let mut waits = Vec::new();
    for index in (0..count).rev() {
        let mut item = WorkItemRecord::new("web", "items 汉字 \0 🦀", WorkItemState::Open);
        item.id = format!("item-{index:03}");
        item.plan_status = WorkItemPlanStatus::Ready;
        item.created_at = "2026-10-01T00:00:00Z".parse().unwrap();
        item.updated_at = item.created_at;
        item.id.reserve(101);
        item.objective.reserve(137);
        waits.push(WaitConditionRecord {
            id: format!("wait-{index:03}"),
            agent_id: "web".into(),
            work_item_id: Some(item.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: Some("task-fixture".into()),
            waiting_for: "task 🦀".into(),
            wake_sources: vec![WakeSource::SystemTick],
            continuation: None,
            created_at: item.created_at,
            updated_at: item.updated_at,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        });
        records.insert(item.id.clone(), item);
    }
    (records, waits)
}

#[test]
fn items_exact_slots_preserve_moves_order_values_and_original_holder() -> Result<()> {
    for count in [1, 8, 65] {
        let (records, waits) = inputs(count);
        let expected =
            derive_context_work_queue(None, records.clone(), vec![], waits.clone(), vec![])?;
        let pointers = records
            .iter()
            .map(|(id, item)| {
                (
                    id.clone(),
                    (
                        item.id.as_ptr() as usize,
                        item.id.capacity(),
                        item.objective.as_ptr() as usize,
                        item.objective.capacity(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        // Items slots are still exact; add only the new downstream summary
        // gates, independently counted once per one-wait owner.
        let summary = waits
            .iter()
            .try_fold((0u64, 0usize), |sum, wait| -> Result<_> {
                let next =
                    super::queue_wait_summary_material_tests::cost(std::slice::from_ref(wait))?;
                Ok((sum.0 + next.0, sum.1 + next.1))
            })?;
        let limit = (total(count).0 + summary.0, total(count).1 + summary.1);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limit.0, limit.1)?;
        let holder = phase.guard()?;
        let queue = {
            let _entry = phase.enter();
            let queue = derive_context_work_queue(None, records, vec![], waits, vec![])?;
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            queue
        };
        assert_eq!(queue.items, expected.items);
        assert_eq!(queue.items.len(), count);
        assert!(queue.items.capacity() >= count);
        assert_eq!(phase.finish()?, limit);
        assert_eq!(ledger.report()?.observed_bytes, limit.0);
        assert!(queue.current.is_none());
        assert!(queue.current_runnable.is_none());
        assert!(queue.queued_blocked.is_empty());
        assert!(queue.blocked.is_empty());
        assert!(queue.queued_runnable.is_empty());
        for (index, row) in queue.items.iter().enumerate() {
            assert_eq!(row.work_item.id, format!("item-{index:03}"));
            assert_eq!(row.scheduling_state, WorkItemSchedulingState::WaitingTask);
            let item = &row.work_item;
            assert_eq!(
                pointers[&item.id],
                (
                    item.id.as_ptr() as usize,
                    item.id.capacity(),
                    item.objective.as_ptr() as usize,
                    item.objective.capacity(),
                )
            );
        }
        drop(queue);
        assert_eq!(ledger.report()?.observed_bytes, limit.0);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, limit.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn items_one_short_bytes_or_records_reject_all_or_nothing_and_latch() -> Result<()> {
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
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                for _ in 0..2 {
                    let (records, waits) = inputs(count);
                    let error = derive_context_work_queue(None, records, vec![], waits, vec![])
                        .unwrap_err();
                    assert!(
                        error.to_string().contains("queue items slots admission"),
                        "{error:#}"
                    );
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, header().0);
                    assert_eq!(
                        phase
                            .finish()
                            .unwrap_err()
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
    }
    Ok(())
}

#[test]
fn empty_items_have_zero_capacity_and_no_charge_or_outside_phase_qualification() -> Result<()> {
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _entry = phase.enter();
        let queue = derive_context_work_queue(
            Some("absent".into()),
            QueueLatestWork::new(),
            vec![],
            vec![],
            vec![],
        )?;
        assert!(queue.items.is_empty());
        assert_eq!(queue.items.capacity(), 0);
        assert!(queue.current.is_none());
        assert_eq!(phase.finish()?, header());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (records, waits) = inputs(8);
    let queue = derive_context_work_queue(None, records, vec![], waits, vec![])?;
    assert_eq!(queue.items.len(), 8);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
