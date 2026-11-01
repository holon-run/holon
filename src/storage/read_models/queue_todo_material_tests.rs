use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{TodoItem, TodoItemState, WakeSource, WorkItemPlanStatus},
    work_item_scheduling::{
        derive_queue_work_item_scheduling, derive_work_item_scheduling, WorkItemSchedulingFacts,
    },
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-todo".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn cost(todo: &TodoItem) -> Result<(u64, usize)> {
    // TodoItem serializes as two string-valued fields: braces, two key
    // cells, colon/colon/comma, and one string cell for each value.
    let value = serde_json::to_value(todo)?;
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert!(value["text"].is_string() && value["state"].is_string());
    let cells = 9;
    Ok((
        (serde_json::to_vec(todo)?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
        (cells + 16) * 8,
    ))
}

fn item(states: &[TodoItemState]) -> WorkItemRecord {
    let mut record = WorkItemRecord::new("web", "todo material", WorkItemState::Open);
    record.id = "work_todo".into();
    record.plan_status = WorkItemPlanStatus::Ready;
    record.todo_list = states
        .iter()
        .enumerate()
        .map(|(n, state)| TodoItem {
            text: format!("{n}: 汉字 🦀 \0 \"{{}}[],:\\"),
            state: state.clone(),
        })
        .collect();
    record
}

#[test]
fn todo_selection_matches_public_api_and_independent_first_candidate_matrix() -> Result<()> {
    use TodoItemState::{Completed, InProgress, Pending};
    for (states, selected) in [
        (vec![], None),
        (vec![Completed, Completed], None),
        (vec![Pending, Pending], Some(0)),
        (vec![Completed, Pending, Pending], Some(1)),
        (vec![Pending, InProgress, InProgress], Some(1)),
        (vec![Completed, InProgress, Pending], Some(1)),
    ] {
        for state in [
            WorkItemState::Open,
            WorkItemState::Completing,
            WorkItemState::Completed,
        ] {
            for current in [false, true] {
                for yielded in [false, true] {
                    let mut record = item(&states);
                    record.state = state.clone();
                    let expected = derive_work_item_scheduling(WorkItemSchedulingFacts {
                        work_item: &record,
                        is_current: current,
                        is_yielded: yielded,
                        active_wait_conditions: &[],
                        trigger_delivery_by_id: &BTreeMap::new(),
                    });
                    let selected = selected.map(|index| record.todo_list[index].clone());
                    let actual =
                        derive_queue_work_item_scheduling(record, current, yielded, &[], &|_| {
                            None
                        })?;
                    assert_eq!(actual.current_todo, selected);
                    assert_eq!(actual, expected);
                    assert_eq!(
                        serde_json::to_value(&actual)?,
                        serde_json::to_value(&expected)?
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn todo_exact_cost_preserves_original_record_and_independent_returned_copy() -> Result<()> {
    let record = item(&[TodoItemState::Pending, TodoItemState::InProgress]);
    let list_pointer = record.todo_list.as_ptr();
    let text_pointer = record.todo_list[1].text.as_ptr();
    let expected = record.todo_list[1].clone();
    let added = cost(&expected)?;
    let total = (header().0 + added.0, header().1 + added.1);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let original = phase.guard()?;
    let mut projection = {
        let _entry = phase.enter();
        let row = derive_queue_work_item_scheduling(record, false, false, &[], &|_| None)?;
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &original
        ));
        row
    };
    assert_eq!(phase.finish()?, total);
    assert_eq!(projection.todo_list.as_ptr(), list_pointer);
    assert_eq!(projection.todo_list[1].text.as_ptr(), text_pointer);
    assert_eq!(projection.current_todo.as_ref(), Some(&expected));
    assert_ne!(
        projection.current_todo.as_ref().unwrap().text.as_ptr(),
        text_pointer
    );
    projection
        .current_todo
        .as_mut()
        .unwrap()
        .text
        .push_str(" changed");
    assert_eq!(projection.todo_list[1], expected);
    drop((projection, phase));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(original);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn todo_one_short_bytes_or_records_rejects_before_clone_and_latches_original_phase() -> Result<()> {
    let record = item(&[TodoItemState::Pending, TodoItemState::InProgress]);
    let added = cost(&record.todo_list[1])?;
    let total = (header().0 + added.0, header().1 + added.1);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            if bytes { total.0 - 1 } else { total.0 },
            if bytes { total.1 } else { total.1 - 1 },
        )?;
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
            for _ in 0..2 {
                let error =
                    derive_queue_work_item_scheduling(record.clone(), false, false, &[], &|_| None)
                        .unwrap_err();
                assert_eq!(error.to_string(), "queue current todo clone admission");
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
    Ok(())
}

#[test]
fn no_todo_candidate_has_zero_increment_even_after_original_phase_failure() -> Result<()> {
    for states in [vec![], vec![TodoItemState::Completed]] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
        {
            let _entry = phase.enter();
            let row =
                derive_queue_work_item_scheduling(item(&states), false, false, &[], &|_| None)?;
            assert!(row.current_todo.is_none());
            assert_eq!(phase.finish()?, header());
            assert!(resource_io::charge_owned_metadata(1, 0).is_err());
            let row =
                derive_queue_work_item_scheduling(item(&states), false, false, &[], &|_| None)?;
            assert!(row.current_todo.is_none());
            assert!(phase.finish().is_err());
            assert_eq!(ledger.report()?.observed_bytes, header().0);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn shared_queue_todo_rejection_returns_no_partial_result_on_original_phase() -> Result<()> {
    let record = item(&[TodoItemState::Pending]);
    let todo = cost(&record.todo_list[0])?;
    let slots = (
        std::mem::size_of::<WorkItemSchedulingProjection>() as u64 * 8,
        8,
    );
    let prefix = (header().0 + slots.0, header().1 + slots.1);
    let now = record.created_at;
    let wait = WaitConditionRecord {
        id: "wait".into(),
        agent_id: "web".into(),
        work_item_id: Some(record.id.clone()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Task,
        source: None,
        subject_ref: None,
        waiting_for: "task".into(),
        wake_sources: vec![WakeSource::SystemTick],
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    };
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            prefix.0 + todo.0 - u64::from(bytes),
            prefix.1 + todo.1 - usize::from(!bytes),
        )?;
        {
            let _entry = phase.enter();
            let error = derive_context_work_queue(
                None,
                QueueLatestWork::from([(record.id.clone(), record.clone())]),
                vec![],
                vec![wait.clone()],
                vec![],
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "queue current todo clone admission");
            assert!(error.downcast_ref::<std::io::Error>().is_some());
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            assert!(phase.finish().is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
