use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{TodoItem, TodoItemState, WorkItemState},
    work_item_scheduling::{derive_work_item_scheduling, WorkItemSchedulingFacts},
};
use std::collections::BTreeMap;

fn fixture() -> WorkQueueReadModel {
    let mut record =
        WorkItemRecord::new("agent", "汉🦀 \"{}[],:\\ ".repeat(300), WorkItemState::Open);
    record.todo_list = vec![TodoItem {
        text: "nested 汉🦀".repeat(200),
        state: TodoItemState::InProgress,
    }];
    let mut item = derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: &record,
        is_current: true,
        is_yielded: false,
        active_wait_conditions: &[],
        trigger_delivery_by_id: &BTreeMap::new(),
    });
    item.scheduling_state = WorkItemSchedulingState::WaitingOperator;
    item.has_active_waits = true;
    WorkQueueReadModel {
        current: Some(record),
        // Repeated records remain repeated copies, even when current == waiting.
        queued_runnable: vec![item.clone(), item.clone()],
        items: vec![item.clone(), item.clone()],
        completed_recent: vec![item],
        ..Default::default()
    }
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent".into(),
        identity_incarnation: 1,
    }
}

// Independent JSON-tree oracle: strings count one opening quote; object keys
// count an opening quote and colon. Punctuation in text is not structure.
fn shape(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    match value {
        Value::Object(fields) => {
            2 + fields.len().saturating_sub(1)
                + fields.values().map(|value| 2 + shape(value)).sum::<usize>()
        }
        Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
        }
        Value::String(_) => 1,
        _ => 0,
    }
}

fn costs(queue: &WorkQueueReadModel, borrowed: bool) -> Result<Vec<(u64, usize)>> {
    let mut costs = vec![
        ((("agent".len() + 256) * 8) as u64, 64),
        (
            (queue.queued_runnable.len() * std::mem::size_of::<WorkItemRecord>() * 8) as u64,
            queue.queued_runnable.len() * 8,
        ),
    ];
    if borrowed {
        // Fixture independently specifies current, two runnable, first waiting.
        for record in queue
            .current
            .iter()
            .chain(queue.queued_runnable.iter().map(|item| &item.work_item))
            .chain(queue.items.first().map(|item| &item.work_item))
        {
            let value = serde_json::to_value(record)?;
            let slots = shape(&value);
            costs.push((
                (serde_json::to_vec(&value)?.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
                (slots + 16) * 8,
            ));
        }
    }
    Ok(costs)
}

fn total(costs: &[(u64, usize)]) -> (u64, usize) {
    costs
        .iter()
        .fold((0, 0), |(b, r), (db, dr)| (b + db, r + dr))
}

fn observations() -> SchedulerObservations<'static> {
    SchedulerObservations {
        active_tasks: Vec::new(),
        has_interrupted_replay: false,
        active_wait_conditions: &[],
        activation_waits: Vec::new(),
        execution_snapshot: None,
        active_timers: 0,
        runtime_error: false,
    }
}

#[test]
fn cold_retained_work_exact_owned_borrowed_cost_and_fields() -> Result<()> {
    let now = Utc::now();
    for borrowed in [false, true] {
        let source = fixture();
        let expected = SchedulerProjection::from_borrowed_observations(
            SchedulerAgentSnapshot::from_state(&AgentState::new("agent")),
            0,
            &source,
            observations(),
            now,
        );
        let expected_cost = total(&costs(&source, borrowed)?);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            expected_cost.0,
            expected_cost.1,
        )?;
        let queue = if borrowed {
            Cow::Borrowed(&source)
        } else {
            Cow::Owned(source.clone())
        };
        let source_current_ptr = queue.current.as_ref().unwrap().objective.as_ptr();
        SchedulerProjection::admit_retained_work(&queue, &phase)?;
        assert_eq!(phase.finish()?, expected_cost);
        let actual = SchedulerProjection::from_queue_observations(
            SchedulerAgentSnapshot::from_state(&AgentState::new("agent")),
            0,
            queue,
            observations(),
            now,
        );
        assert_eq!(actual, expected);
        assert_eq!(actual.queued_runnable_work_items.len(), 2);
        if borrowed {
            assert_ne!(
                actual
                    .current_work_item
                    .as_ref()
                    .unwrap()
                    .objective
                    .as_ptr(),
                source_current_ptr
            );
        } else {
            assert_eq!(
                actual
                    .current_work_item
                    .as_ref()
                    .unwrap()
                    .objective
                    .as_ptr(),
                source_current_ptr
            );
        }
        let guard = phase.guard()?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, expected_cost.0);
        drop((actual, guard));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn cold_retained_work_each_allocation_prefix_refuses_and_latches() -> Result<()> {
    let queue = fixture();
    for borrowed in [false, true] {
        let costs = costs(&queue, borrowed)?;
        for end in 2..=costs.len() {
            let required = total(&costs[..end]);
            for bytes in [false, true] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    required.0 - u64::from(bytes),
                    required.1 - usize::from(!bytes),
                )?;
                let input = if borrowed {
                    Cow::Borrowed(&queue)
                } else {
                    Cow::Owned(queue.clone())
                };
                let error = SchedulerProjection::admit_retained_work(&input, &phase).unwrap_err();
                assert!(
                    error.downcast_ref::<std::io::Error>().is_some(),
                    "{error:#}"
                );
                let latched = phase.finish().unwrap_err().to_string();
                let guard = phase
                    .guard()
                    .err()
                    .context("failed phase cannot return guard")?;
                assert_eq!(guard.to_string(), latched);
                let prefix = ledger.report()?.observed_bytes;
                let retry = SchedulerProjection::admit_retained_work(&input, &phase);
                assert!(retry.is_err());
                assert_eq!(ledger.report()?.observed_bytes, prefix);
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn cold_retained_work_empty_and_unselected_members_have_no_body_cost() -> Result<()> {
    let mut queue = fixture();
    queue.current = None;
    queue.queued_runnable.clear();
    for item in &mut queue.items {
        item.scheduling_state = WorkItemSchedulingState::Runnable;
    }
    assert_eq!(SchedulerProjection::waiting_work_item_index(&queue), None);
    for input in [
        Cow::Borrowed(&queue),
        Cow::Owned(queue.clone()),
        Cow::Owned(Default::default()),
    ] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let header = ((("agent".len() + 256) * 8) as u64, 64);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header.0, header.1)?;
        SchedulerProjection::admit_retained_work(&input, &phase)?;
        assert_eq!(phase.finish()?, header);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
