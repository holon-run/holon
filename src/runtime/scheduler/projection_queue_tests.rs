use super::*;
use crate::types::{TodoItem, TodoItemState, WorkItemPlanStatus, WorkItemState};
use crate::work_item_scheduling::{derive_work_item_scheduling, WorkItemSchedulingFacts};
use std::collections::BTreeMap;

fn presence_projection(
    current: Option<WorkItemSchedulingState>,
    queued: usize,
    authority: u8,
) -> SchedulerProjection {
    let state = AgentState::new("agent");
    let mut observed = observations();
    observed.has_interrupted_replay = false;
    observed.active_timers = 0;
    let mut projection = SchedulerProjection::from_observations(
        SchedulerAgentSnapshot::from_state(&state),
        0,
        WorkQueueReadModel::default(),
        observed,
        Utc::now(),
    );
    let mut record = WorkItemRecord::new("agent", "objective", WorkItemState::Open);
    record.id = "current 汉🦀".repeat(37);
    record.plan_status = WorkItemPlanStatus::Ready;
    projection.current_work_item = current.map(|_| record.clone());
    projection.current_work_item_scheduling_state = current;
    projection.queued_runnable_work_items = (0..queued)
        .map(|index| {
            let mut record = record.clone();
            record.id = format!("queued-{index} 汉🦀").repeat(31);
            record
        })
        .collect();
    projection.queued_work_items = queued;
    projection.canonical_work_states = match authority {
        0 => None,
        1 => Some(HashMap::new()),
        2 | 3 => Some(
            projection
                .current_work_item
                .iter()
                .chain(&projection.queued_runnable_work_items)
                .map(|record| {
                    (
                        record.id.clone(),
                        CanonicalWorkExecutionState::Runnable {
                            source_revision: record.revision + u64::from(authority == 3),
                            generation: 17,
                        },
                    )
                })
                .collect(),
        ),
        _ => unreachable!(),
    };
    projection
}

#[test]
fn borrowed_reactivation_presence_matches_signal_and_retains_original_records() {
    for current in [
        None,
        Some(WorkItemSchedulingState::Runnable),
        Some(WorkItemSchedulingState::WaitingOperator),
        Some(WorkItemSchedulingState::Blocked),
    ] {
        for queued in [0, 1, 2] {
            for authority in 0..4 {
                let projection = presence_projection(current, queued, authority);
                let original = projection.clone();
                let expected = (current == Some(WorkItemSchedulingState::Runnable) || queued > 0)
                    && matches!(authority, 0 | 2);
                let borrowed = projection.work_reactivation_work_item();
                let selected = select_autonomous_continuation(&projection);
                let expected_selection = selected.as_ref().and_then(|selected| {
                    resolve_autonomous_continuation_work_item(&projection, selected)
                });
                assert_eq!(
                    projection.work_reactivation_work_item_with_generation(),
                    expected_selection,
                );
                let signal = projection.work_reactivation_signal();
                assert_eq!(borrowed.is_some(), expected);
                assert_eq!(signal.is_some(), expected);
                if let (Some((record, mode)), Some(signal)) = (borrowed, signal) {
                    assert_eq!(record.id, signal.work_item_id);
                    assert_eq!(record.state, signal.state);
                    assert_eq!(mode, signal.reactivation_mode);
                    let source = match mode {
                        WorkReactivationMode::ContinueActive => {
                            projection.current_work_item.as_ref().unwrap()
                        }
                        WorkReactivationMode::ActivateQueued => projection
                            .queued_runnable_work_items
                            .iter()
                            .find(|source| source.id == record.id)
                            .unwrap(),
                    };
                    assert!(std::ptr::eq(record, source));
                    assert_eq!(record.id.as_ptr(), source.id.as_ptr());
                    assert_eq!(record.id.capacity(), source.id.capacity());
                }
                assert_eq!(projection, original);
            }
        }
    }
}

#[test]
fn borrowed_baseline_selection_needs_no_owned_material_allowance() -> Result<()> {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: "borrowed-closure-baseline".into(),
        identity_incarnation: 1,
    };
    let header = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    let (bytes, records) = header.finish()?;
    drop(header);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
    for current in [None, Some(WorkItemSchedulingState::Runnable)] {
        for authority in 0..4 {
            let projection = presence_projection(current, 3, authority);
            let selected = select_autonomous_continuation(&projection);
            let expected = selected.as_ref().and_then(|selected| {
                resolve_autonomous_continuation_work_item(&projection, selected)
            });
            let actual =
                phase.read(|| Ok(projection.work_reactivation_work_item_with_generation()))?;
            assert_eq!(actual, expected);
            assert_eq!(phase.finish()?, (bytes, records));
            if let (Some((actual, _, _)), Some((expected, _, _))) = (actual, expected) {
                assert!(std::ptr::eq(actual, expected));
            }
        }
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn borrowed_reactivation_presence_preserves_wait_guard_and_replay_priority() {
    for current in [
        None,
        Some(WorkItemSchedulingState::Runnable),
        Some(WorkItemSchedulingState::WaitingOperator),
    ] {
        for queued in [0, 2] {
            for authority in 0..4 {
                for replay in [false, true] {
                    let mut projection = presence_projection(current, queued, authority);
                    projection.has_interrupted_replay = replay;
                    projection.active_waiting_intents = 1;
                    projection.active_agent_waiting_intents = 1;
                    projection.active_timers = 1;
                    let expected_runnable = (current == Some(WorkItemSchedulingState::Runnable)
                        || queued > 0)
                        && matches!(authority, 0 | 2);
                    let decision = super::super::wait_decision_for_projection(&projection);
                    assert_eq!(decision.is_none(), replay || expected_runnable);
                    if let Some(decision) = decision {
                        assert_eq!(decision.kind, SchedulerDecisionKind::WaitForExternalChange);
                        assert_eq!(decision.reason, "active_agent_waiting_intents");
                    }
                }
            }
        }
    }
}

fn observations() -> SchedulerObservations<'static> {
    SchedulerObservations {
        active_tasks: Vec::new(),
        has_interrupted_replay: true,
        active_wait_conditions: &[],
        activation_waits: Vec::new(),
        execution_snapshot: None,
        active_timers: 3,
        runtime_error: true,
    }
}

fn queue() -> WorkQueueReadModel {
    let mut record = WorkItemRecord::new("agent", "objective 汉🦀".repeat(71), WorkItemState::Open);
    record.id = "current".into();
    record.plan_status = WorkItemPlanStatus::Ready;
    record.todo_list = vec![TodoItem {
        text: "nested Todo 汉🦀".repeat(43),
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
    // Fill every display group, including those the scheduler never retains.
    WorkQueueReadModel {
        current: Some(record.clone()),
        queued_blocked: vec![record],
        items: vec![item.clone()],
        current_runnable: Some(item.clone()),
        triggered_blocked: vec![item.clone()],
        queued_runnable: vec![item.clone()],
        yielded: vec![item.clone()],
        waiting_for_operator: vec![item.clone()],
        blocked: vec![item.clone()],
        completing: vec![item.clone()],
        completed_recent: vec![item],
    }
}

#[test]
fn borrowed_queue_matches_owned_scheduler_and_preserves_all_groups() {
    let state = AgentState::new("agent");
    let now = Utc::now();
    for mut queue in [WorkQueueReadModel::default(), queue()] {
        for current in [true, false] {
            if !current {
                queue.current = None;
            }
            let original = queue.clone();
            let items_ptr = queue.items.as_ptr();
            let borrowed = SchedulerProjection::from_borrowed_observations(
                SchedulerAgentSnapshot::from_state(&state),
                17,
                &queue,
                observations(),
                now,
            );
            let owned = SchedulerProjection::from_observations(
                SchedulerAgentSnapshot::from_state(&state),
                17,
                queue.clone(),
                observations(),
                now,
            );
            assert_eq!(borrowed, owned);
            assert_eq!(borrowed.current_work_item, original.current);
            assert_eq!(
                borrowed.queued_runnable_work_items,
                original
                    .queued_runnable
                    .iter()
                    .map(|item| item.work_item.clone())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                borrowed.waiting_work_item,
                original.items.first().map(|item| item.work_item.clone())
            );
            assert_eq!(borrowed.active_timers, 3);
            assert_eq!(queue.items.as_ptr(), items_ptr);
            assert_eq!(queue.current, original.current);
            assert_eq!(queue.queued_blocked, original.queued_blocked);
            assert_eq!(queue.items, original.items);
            assert_eq!(queue.current_runnable, original.current_runnable);
            assert_eq!(queue.triggered_blocked, original.triggered_blocked);
            assert_eq!(queue.queued_runnable, original.queued_runnable);
            assert_eq!(queue.yielded, original.yielded);
            assert_eq!(queue.waiting_for_operator, original.waiting_for_operator);
            assert_eq!(queue.blocked, original.blocked);
            assert_eq!(queue.completing, original.completing);
            assert_eq!(queue.completed_recent, original.completed_recent);
        }
    }
}

#[test]
fn queue_ownership_moves_owned_current_and_only_copies_borrowed_retained_current() {
    let queue = queue();
    let original = queue.current.as_ref().unwrap();
    let objective = original.objective.as_ptr();
    let todo = original.todo_list.as_ptr();
    let state = AgentState::new("agent");
    let now = Utc::now();
    let borrowed = SchedulerProjection::from_queue_observations(
        SchedulerAgentSnapshot::from_state(&state),
        0,
        Cow::Borrowed(&queue),
        observations(),
        now,
    );
    let copied = borrowed.current_work_item.as_ref().unwrap();
    assert_eq!(copied, original);
    assert_ne!(copied.objective.as_ptr(), objective);
    assert_ne!(copied.todo_list.as_ptr(), todo);
    let owned = SchedulerProjection::from_observations(
        SchedulerAgentSnapshot::from_state(&state),
        0,
        queue,
        observations(),
        now,
    );
    let moved = owned.current_work_item.as_ref().unwrap();
    assert_eq!(moved.objective.as_ptr(), objective);
    assert_eq!(moved.todo_list.as_ptr(), todo);
    assert_eq!(owned, borrowed);
}

fn assert_retained_clone_oracle(actual: &SchedulerProjection, queue: &WorkQueueReadModel) {
    let waiting = queue.items.iter().find(|item| {
        (item.is_current || item.has_active_waits || item.has_active_task_waits)
            && matches!(
                item.scheduling_state,
                WorkItemSchedulingState::WaitingOperator
                    | WorkItemSchedulingState::WaitingTask
                    | WorkItemSchedulingState::WaitingExternal
                    | WorkItemSchedulingState::WaitingTimer
                    | WorkItemSchedulingState::WaitingSystem
            )
    });
    assert_eq!(actual.current_work_item, queue.current);
    assert_eq!(
        actual.current_work_item_scheduling_state,
        queue
            .items
            .iter()
            .find(|item| item.is_current)
            .map(|item| item.scheduling_state)
    );
    assert_eq!(
        actual.queued_runnable_work_items,
        queue
            .queued_runnable
            .iter()
            .map(|item| item.work_item.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(actual.queued_work_items, queue.queued_runnable.len());
    assert_eq!(
        actual.queued_runnable_work_items.capacity(),
        queue.queued_runnable.len()
    );
    assert_eq!(
        actual.waiting_work_item,
        waiting.map(|item| item.work_item.clone())
    );
    assert_eq!(
        actual.waiting_work_item_scheduling_state,
        waiting.map(|item| item.scheduling_state)
    );
}

fn payload_addresses(record: &WorkItemRecord) -> [usize; 7] {
    [
        record.id.as_ptr() as usize,
        record.agent_id.as_ptr() as usize,
        record.workspace_id.as_ptr() as usize,
        record.objective.as_ptr() as usize,
        record.todo_list.as_ptr() as usize,
        record.todo_list[0].text.as_ptr() as usize,
        record.blocked_by.as_ref().unwrap().as_ptr() as usize,
    ]
}

fn retained_queue() -> WorkQueueReadModel {
    let mut queue = queue();
    let template = queue.items[0].clone();
    queue.items = (0..3)
        .map(|index| {
            let mut item = template.clone();
            item.work_item.id = "duplicate payload ID 汉🦀".into();
            item.work_item.objective.push_str(&index.to_string());
            item.work_item.blocked_by = Some("retained blocker 汉🦀".repeat(31));
            item.is_current = index == 1;
            item
        })
        .collect();
    queue.queued_runnable = queue.items.iter().rev().cloned().collect();
    queue
}

#[test]
fn owned_retained_records_match_clone_oracle_for_wait_selection_matrix() {
    let agent_state = AgentState::new("agent");
    let now = Utc::now();
    for state in [
        WorkItemSchedulingState::Runnable,
        WorkItemSchedulingState::YieldedToWorkItem,
        WorkItemSchedulingState::WaitingOperator,
        WorkItemSchedulingState::WaitingTask,
        WorkItemSchedulingState::WaitingExternal,
        WorkItemSchedulingState::WaitingTimer,
        WorkItemSchedulingState::WaitingSystem,
        WorkItemSchedulingState::Blocked,
        WorkItemSchedulingState::Completing,
        WorkItemSchedulingState::Completed,
    ] {
        for flags in 0..8 {
            // Zero, one and several eligible candidates; duplicate IDs must not
            // collapse distinct records or replace the first eligible member.
            for first in 0..=3 {
                let mut queue = retained_queue();
                for (index, item) in queue.items.iter_mut().enumerate() {
                    item.scheduling_state = state;
                    item.is_current = index >= first && flags & 1 != 0;
                    item.has_active_waits = index >= first && flags & 2 != 0;
                    item.has_active_task_waits = index >= first && flags & 4 != 0;
                }
                let borrowed = SchedulerProjection::from_borrowed_observations(
                    SchedulerAgentSnapshot::from_state(&agent_state),
                    19,
                    &queue,
                    observations(),
                    now,
                );
                assert_retained_clone_oracle(&borrowed, &queue);
                let owned = SchedulerProjection::from_observations(
                    SchedulerAgentSnapshot::from_state(&agent_state),
                    19,
                    queue.clone(),
                    observations(),
                    now,
                );
                assert_retained_clone_oracle(&owned, &queue);
                assert_eq!(owned, borrowed);
            }
        }
    }
}

#[test]
fn owned_runnable_and_first_waiting_payloads_keep_original_heap_addresses() {
    let state = AgentState::new("agent");
    let now = Utc::now();
    for first in 0..3 {
        let mut queue = retained_queue();
        queue.queued_runnable.reserve(23);
        for (index, item) in queue.items.iter_mut().enumerate() {
            item.is_current = index == 1;
            item.has_active_waits = index >= first;
            item.has_active_task_waits = false;
            item.scheduling_state = if index < first {
                WorkItemSchedulingState::Runnable
            } else {
                WorkItemSchedulingState::WaitingTask
            };
        }
        let original = queue.clone();
        let runnable_addresses = queue
            .queued_runnable
            .iter()
            .map(|item| payload_addresses(&item.work_item))
            .collect::<Vec<_>>();
        let waiting_addresses = payload_addresses(&queue.items[first].work_item);
        let owned = SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&state),
            0,
            queue,
            observations(),
            now,
        );
        assert_retained_clone_oracle(&owned, &original);
        assert_eq!(
            owned
                .queued_runnable_work_items
                .iter()
                .map(payload_addresses)
                .collect::<Vec<_>>(),
            runnable_addresses
        );
        assert_eq!(
            payload_addresses(owned.waiting_work_item.as_ref().unwrap()),
            waiting_addresses
        );
    }
}

#[test]
fn borrowed_retained_payloads_are_independent_and_preserve_source_queue() {
    let queue = retained_queue();
    let original = queue.clone();
    let items_ptr = queue.items.as_ptr();
    let runnable_ptr = queue.queued_runnable.as_ptr();
    let state = AgentState::new("agent");
    let mut borrowed = SchedulerProjection::from_queue_observations(
        SchedulerAgentSnapshot::from_state(&state),
        0,
        Cow::Borrowed(&queue),
        observations(),
        Utc::now(),
    );
    assert_retained_clone_oracle(&borrowed, &queue);
    for (copy, source) in borrowed
        .queued_runnable_work_items
        .iter()
        .zip(&queue.queued_runnable)
        .chain(borrowed.waiting_work_item.iter().zip(queue.items.first()))
    {
        assert!(payload_addresses(copy)
            .iter()
            .zip(payload_addresses(&source.work_item))
            .all(|(copy, source)| *copy != source));
    }
    borrowed.queued_runnable_work_items[0].objective.push('!');
    borrowed.waiting_work_item.as_mut().unwrap().todo_list[0]
        .text
        .push('!');
    assert_eq!(queue.items.as_ptr(), items_ptr);
    assert_eq!(queue.queued_runnable.as_ptr(), runnable_ptr);
    let preserved = SchedulerProjection::from_observations(
        SchedulerAgentSnapshot::from_state(&state),
        0,
        queue.clone(),
        observations(),
        borrowed.now,
    );
    assert_retained_clone_oracle(&preserved, &original);
    assert_eq!(queue.items, original.items);
    assert_eq!(queue.queued_runnable, original.queued_runnable);
}

#[test]
fn borrowed_observations_move_buffers_and_preserve_retained_queue_after_projection_drop() {
    use crate::types::TaskKind;
    let state = AgentState::new("agent");
    let now = Utc::now();
    let queue_value = |queue: &WorkQueueReadModel| {
        serde_json::to_value((
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
        ))
        .unwrap()
    };
    for queue in [WorkQueueReadModel::default(), retained_queue()] {
        let original = queue_value(&queue);
        let items_ptr = queue.items.as_ptr();
        let items_capacity = queue.items.capacity();
        let runnable_ptr = queue.queued_runnable.as_ptr();
        let runnable_capacity = queue.queued_runnable.capacity();
        let mut observed = observations();
        observed.active_tasks = Vec::with_capacity(5);
        observed.active_tasks.push(TaskRecord {
            id: "observed-task".into(),
            agent_id: state.id.clone(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("original task payload 汉🦀".repeat(79)),
            detail: None,
            recovery: None,
        });
        observed.activation_waits = Vec::with_capacity(7);
        let tasks_ptr = observed.active_tasks.as_ptr();
        let tasks_capacity = observed.active_tasks.capacity();
        let summary_ptr = observed.active_tasks[0].summary.as_ref().unwrap().as_ptr();
        let waits_ptr = observed.activation_waits.as_ptr();
        let waits_capacity = observed.activation_waits.capacity();
        let mut expected_observations = observations();
        expected_observations.active_tasks = observed.active_tasks.clone();
        let expected = SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&state),
            23,
            queue.clone(),
            expected_observations,
            now,
        );
        let actual = SchedulerProjection::from_borrowed_observations(
            SchedulerAgentSnapshot::from_state(&state),
            23,
            &queue,
            observed,
            now,
        );
        assert_eq!(actual, expected);
        assert_retained_clone_oracle(&actual, &queue);
        assert_eq!(actual.active_tasks.as_ptr(), tasks_ptr);
        assert_eq!(actual.active_tasks.capacity(), tasks_capacity);
        assert_eq!(
            actual.active_tasks[0].summary.as_ref().unwrap().as_ptr(),
            summary_ptr
        );
        assert_eq!(actual.activation_waits.as_ptr(), waits_ptr);
        assert_eq!(actual.activation_waits.capacity(), waits_capacity);
        assert!(!actual.has_blocking_active_tasks);
        drop(actual);
        assert_eq!(queue_value(&queue), original);
        assert_eq!(queue.items.as_ptr(), items_ptr);
        assert_eq!(queue.items.capacity(), items_capacity);
        assert_eq!(queue.queued_runnable.as_ptr(), runnable_ptr);
        assert_eq!(queue.queued_runnable.capacity(), runnable_capacity);
    }
}
