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
use std::sync::Arc;

fn wait(index: usize, work_id: Option<&str>, kind: WaitConditionKind) -> WaitConditionRecord {
    let time = "2026-10-01T00:00:00Z".parse().unwrap();
    WaitConditionRecord {
        id: format!("wait-{index:04}"),
        agent_id: "web".into(),
        work_item_id: work_id.map(str::to_owned),
        status: WaitConditionStatus::Active,
        kind,
        source: None,
        subject_ref: Some("task-fixture".into()),
        waiting_for: "wait 汉字\0🦀".into(),
        wake_sources: vec![WakeSource::SystemTick],
        continuation: None,
        created_at: time,
        updated_at: time,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}

fn mixed_waits(count: usize) -> Vec<WaitConditionRecord> {
    let ids = [Some(""), Some("a"), None, Some("汉字\0🦀"), Some("z")];
    (0..count)
        .rev()
        .map(|index| {
            let mut row = wait(index, ids[index % ids.len()], WaitConditionKind::Task);
            row.created_at += chrono::Duration::seconds((index % 3) as i64);
            row.id.reserve(83);
            row.waiting_for.reserve(137);
            if let Some(id) = &mut row.work_item_id {
                id.reserve(101);
            }
            row
        })
        .collect()
}

fn legacy_groups(waits: Vec<WaitConditionRecord>) -> BTreeMap<String, Vec<WaitConditionRecord>> {
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for row in waits {
        if let Some(id) = row.work_item_id.clone() {
            groups.entry(id).or_default().push(row);
        }
    }
    groups
}

#[test]
fn wait_groups_reuse_original_vec_and_owned_text_allocations() {
    for count in [0, 1, 65, 257] {
        let mut waits = mixed_waits(count);
        waits.reserve(19);
        let backing = (waits.as_ptr(), waits.capacity());
        let texts = waits
            .iter()
            .filter(|row| row.work_item_id.is_some())
            .map(|row| {
                let id = row.work_item_id.as_ref().unwrap();
                (
                    row.id.clone(),
                    (
                        row.id.as_ptr(),
                        row.id.capacity(),
                        id.as_ptr(),
                        id.capacity(),
                        row.waiting_for.as_ptr(),
                        row.waiting_for.capacity(),
                        row.wake_sources.as_ptr(),
                        row.wake_sources.capacity(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let grouped = group_queue_waits(waits);
        assert_eq!((grouped.as_ptr(), grouped.capacity()), backing);
        assert_eq!(grouped.len(), texts.len());
        for row in &grouped {
            let id = row.work_item_id.as_ref().unwrap();
            assert_eq!(
                texts[&row.id],
                (
                    row.id.as_ptr(),
                    row.id.capacity(),
                    id.as_ptr(),
                    id.capacity(),
                    row.waiting_for.as_ptr(),
                    row.waiting_for.capacity(),
                    row.wake_sources.as_ptr(),
                    row.wake_sources.capacity(),
                )
            );
        }
    }
}

#[test]
fn wait_group_slices_match_legacy_groups_and_scheduling_order() {
    for count in [0, 1, 65, 257] {
        let waits = mixed_waits(count);
        let mut old = legacy_groups(waits.clone());
        let grouped = group_queue_waits(waits);
        for rows in old.values_mut() {
            rows.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        for id in ["", "a", "a-missing", "z", "汉字\0🦀", "🦀-missing"] {
            let slice = queue_waits_for_item(&grouped, id);
            assert_eq!(slice, old.get(id).map(Vec::as_slice).unwrap_or(&[]));
            if !slice.is_empty() {
                let index = grouped
                    .iter()
                    .position(|row| row.id == slice[0].id)
                    .unwrap();
                assert_eq!(slice.as_ptr(), grouped[index..].as_ptr());
            }
        }
    }
    let unscoped = (0..65)
        .map(|index| wait(index, None, WaitConditionKind::Task))
        .collect();
    assert!(group_queue_waits(unscoped).is_empty());
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-wait-index".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

#[test]
fn wait_grouping_needs_no_gate_on_exact_or_failed_original_phase() -> Result<()> {
    for count in [0, 1, 257] {
        let waits = mixed_waits(count);
        let more_waits = waits.clone();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let limit = header();
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limit.0, limit.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let grouped = group_queue_waits(waits);
            assert_eq!(
                queue_waits_for_item(&grouped, "a").len(),
                more_waits
                    .iter()
                    .filter(|row| row.work_item_id.as_deref() == Some("a"))
                    .count()
            );
            assert_eq!(phase.finish()?, limit);
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            assert!(crate::diagnostics::resource_io::charge_owned_metadata(1, 1).is_err());
            let grouped = group_queue_waits(more_waits);
            assert!(queue_waits_for_item(&grouped, "absent").is_empty());
            assert!(phase.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
            assert_eq!(ledger.report()?.observed_bytes, limit.0);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, limit.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn wait_index_preserves_legacy_scheduling_focus_and_class_windows() -> Result<()> {
    let mut latest = QueueLatestWork::new();
    let mut waits = mixed_waits(65);
    let kinds = [
        WaitConditionKind::Task,
        WaitConditionKind::Operator,
        WaitConditionKind::Timer,
        WaitConditionKind::External,
        WaitConditionKind::System,
    ];
    for index in (0..68).rev() {
        let id = match index {
            65 => "".to_owned(),
            66 => "a".to_owned(),
            67 => "汉字\0🦀".to_owned(),
            _ => format!("work-{index:03}"),
        };
        let state = match index % 11 {
            0 => WorkItemState::Completed,
            1 => WorkItemState::Completing,
            _ => WorkItemState::Open,
        };
        let mut item = WorkItemRecord::new("web", "wait matrix", state);
        item.id = id;
        item.plan_status = WorkItemPlanStatus::Ready;
        if index % 5 == 0 {
            item.blocked_by = Some("manual".into());
        }
        if index % 7 != 0 {
            for offset in 0..3 {
                waits.push(wait(
                    1000 + index * 3 + offset,
                    Some(&item.id),
                    kinds[(index + offset) % kinds.len()].clone(),
                ));
            }
        }
        latest.insert(item.id.clone(), item);
    }
    let old = legacy_groups(waits.clone());
    for current in [
        None,
        Some("work-003"),
        Some("work-000"),
        Some(""),
        Some("missing"),
    ] {
        let queue =
            derive_context_work_queue(current, latest.clone(), vec![], waits.clone(), vec![])?;
        let mut expected = latest
            .values()
            .cloned()
            .map(|item| {
                let is_current =
                    Some(item.id.as_str()) == current && item.state == WorkItemState::Open;
                let conditions = old.get(&item.id).map(Vec::as_slice).unwrap_or(&[]);
                derive_owned_work_item_scheduling(item, is_current, false, conditions, &|_| None)
            })
            .collect::<Vec<_>>();
        expected.sort_by(compare_scheduling_projection_order);
        assert_eq!(queue.items, expected);
        let mut blocked = latest
            .values()
            .filter(|item| {
                item.state == WorkItemState::Open
                    && Some(item.id.as_str()) != current
                    && !old.get(&item.id).is_some_and(|rows| {
                        rows.iter().any(|row| row.kind == WaitConditionKind::Task)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        blocked.sort_by(compare_queue_display_order);
        assert_eq!(queue.queued_blocked, blocked);
        assert_eq!(
            queue.current,
            latest
                .values()
                .find(|item| Some(item.id.as_str()) == current && item.state == WorkItemState::Open)
                .cloned()
        );
        for (actual, class, limit) in [
            (
                &queue.queued_runnable,
                WorkItemCandidateClass::QueuedRunnable,
                5,
            ),
            (
                &queue.waiting_for_operator,
                WorkItemCandidateClass::WaitingForOperator,
                3,
            ),
            (&queue.completing, WorkItemCandidateClass::Completing, 3),
            (
                &queue.completed_recent,
                WorkItemCandidateClass::CompletedRecent,
                3,
            ),
        ] {
            assert_eq!(
                *actual,
                expected
                    .iter()
                    .filter(|row| row.candidate_class == class)
                    .take(limit)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

fn task_inputs() -> (QueueLatestWork, Vec<WaitConditionRecord>) {
    let mut latest = QueueLatestWork::new();
    let mut waits = Vec::new();
    for index in (0..65).rev() {
        let mut item = WorkItemRecord::new("web", "task wait", WorkItemState::Open);
        item.id = format!("task-{index:03}");
        item.plan_status = WorkItemPlanStatus::Ready;
        waits.push(wait(index, Some(&item.id), WaitConditionKind::Task));
        latest.insert(item.id.clone(), item);
    }
    (latest, waits)
}

fn total() -> (u64, usize) {
    (
        header().0 + (65 * std::mem::size_of::<WorkItemSchedulingProjection>()) as u64 * 8,
        header().1 + 65 * 8,
    )
}

#[test]
fn wait_index_queue_retains_exact_items_cost_and_last_original_holder() -> Result<()> {
    let (latest, waits) = task_inputs();
    let expected = derive_context_work_queue(None, latest.clone(), vec![], waits.clone(), vec![])?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    // Each owner has one wait; grouping stays allocation-free, while the
    // downstream scheduling summary has its own independently counted gates.
    let summary = waits
        .iter()
        .try_fold((0u64, 0usize), |sum, wait| -> Result<_> {
            let next = super::queue_wait_summary_material_tests::cost(std::slice::from_ref(wait))?;
            Ok((sum.0 + next.0, sum.1 + next.1))
        })?;
    let limit = (total().0 + summary.0, total().1 + summary.1);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limit.0, limit.1)?;
    let holder = phase.guard()?;
    let queue = {
        let _entry = phase.enter();
        let queue = derive_context_work_queue(None, latest, vec![], waits, vec![])?;
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        assert_eq!(
            (
                &queue.current,
                &queue.queued_blocked,
                &queue.items,
                &queue.current_runnable,
                &queue.triggered_blocked,
                &queue.queued_runnable,
                &queue.yielded,
                &queue.waiting_for_operator,
                &queue.blocked,
                &queue.completing,
                &queue.completed_recent,
            ),
            (
                &expected.current,
                &expected.queued_blocked,
                &expected.items,
                &expected.current_runnable,
                &expected.triggered_blocked,
                &expected.queued_runnable,
                &expected.yielded,
                &expected.waiting_for_operator,
                &expected.blocked,
                &expected.completing,
                &expected.completed_recent,
            )
        );
        assert_eq!(
            serde_json::to_value(&queue.items)?,
            serde_json::to_value(&expected.items)?
        );
        queue
    };
    assert_eq!(phase.finish()?, limit);
    assert!(queue.queued_blocked.is_empty());
    assert!(queue.blocked.is_empty());
    drop(queue);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, limit.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn wait_index_preserves_items_rejection_error_and_same_phase_latch() -> Result<()> {
    for byte_short in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let limit = total();
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
                let (latest, waits) = task_inputs();
                let error =
                    derive_context_work_queue(None, latest, vec![], waits, vec![]).unwrap_err();
                assert!(
                    error.to_string().contains("queue items slots admission"),
                    "{error:#}"
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, header().0);
                assert!(phase.finish().is_err());
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
