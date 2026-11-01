use super::*;
use crate::domain::execution_protocol::{
    ExecutionProtocolState, WaitReference, WorkItemExecutionRecord,
};
use crate::types::WorkItemState;
use std::collections::BTreeMap;

pub(super) fn execution() -> ExecutionProtocolState {
    let mut states = vec![
        WorkItemExecutionState::Runnable {
            generation: u64::MAX,
            recovery_ref: Some("discarded recovery 汉🦀".repeat(23)),
        },
        WorkItemExecutionState::Waiting {
            generation: 19,
            wait: WaitReference {
                wait_id: "retained wait 汉🦀".repeat(47),
            },
        },
        WorkItemExecutionState::InFlight {
            generation: 2,
            attempt_id: "discarded attempt".into(),
        },
        WorkItemExecutionState::Paused {
            generation: 3,
            reason: "discarded pause".into(),
        },
        WorkItemExecutionState::NeedsRepair {
            generation: 4,
            repair_id: "discarded repair".into(),
        },
        WorkItemExecutionState::Terminal {
            generation: 5,
            completion: "discarded completion".into(),
        },
        WorkItemExecutionState::Waiting {
            generation: 0,
            wait: WaitReference {
                wait_id: String::new(),
            },
        },
    ];
    let work_items = states
        .drain(..)
        .enumerate()
        .map(|(index, state)| {
            let mut key = if index == 0 {
                String::new()
            } else {
                format!("{index}-canonical key 汉🦀").repeat(31)
            };
            key.reserve(257);
            (
                key,
                WorkItemExecutionRecord {
                    source_revision: u64::MAX - index as u64,
                    state,
                },
            )
        })
        .collect();
    ExecutionProtocolState {
        agent_id: "snapshot identity is not the projection identity".into(),
        attempts: BTreeMap::new(),
        work_items,
        outcomes: BTreeMap::new(),
    }
}

pub(super) fn observations(
    execution_snapshot: Option<ExecutionProtocolState>,
) -> SchedulerObservations<'static> {
    SchedulerObservations {
        active_tasks: Vec::new(),
        has_interrupted_replay: true,
        active_wait_conditions: &[],
        activation_waits: Vec::new(),
        execution_snapshot,
        active_timers: 7,
        runtime_error: true,
    }
}

// Independent old borrowed/clone mapping, including its fail-closed default.
pub(super) fn clone_oracle(
    execution: Option<&ExecutionProtocolState>,
) -> Option<HashMap<String, CanonicalWorkExecutionState>> {
    Some(
        execution
            .map(|snapshot| {
                snapshot
                    .work_items
                    .iter()
                    .map(|(id, record)| {
                        (
                            id.clone(),
                            match &record.state {
                                WorkItemExecutionState::Runnable { generation, .. } => {
                                    CanonicalWorkExecutionState::Runnable {
                                        source_revision: record.source_revision,
                                        generation: *generation,
                                    }
                                }
                                WorkItemExecutionState::Waiting { wait, .. } => {
                                    CanonicalWorkExecutionState::Waiting {
                                        wait_id: wait.wait_id.clone(),
                                    }
                                }
                                _ => CanonicalWorkExecutionState::Other,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

#[test]
fn canonical_transfer_matches_old_clone_map_and_all_projection_fields() {
    let state = AgentState::new("agent");
    let now = Utc::now();
    let mut empty = execution();
    empty.work_items.clear();
    for execution in [None, Some(empty), Some(execution())] {
        let mut queue = WorkQueueReadModel::default();
        queue.current = Some(WorkItemRecord::new(
            "agent",
            "retained objective 汉🦀".repeat(31),
            WorkItemState::Open,
        ));
        let original = queue.clone();
        let mut expected = SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&state),
            13,
            queue.clone(),
            observations(None),
            now,
        );
        expected.canonical_work_states = clone_oracle(execution.as_ref());
        let borrowed = SchedulerProjection::from_queue_observations(
            SchedulerAgentSnapshot::from_state(&state),
            13,
            Cow::Borrowed(&queue),
            observations(execution.clone()),
            now,
        );
        assert_eq!(borrowed, expected);
        assert_eq!(queue.current, original.current);
        let owned = SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&state),
            13,
            queue,
            observations(execution),
            now,
        );
        assert_eq!(owned, expected);
        assert!(owned.canonical_work_states.is_some());
    }
}

#[test]
fn canonical_transfer_moves_original_key_and_wait_buffers_for_both_queue_modes() {
    let state = AgentState::new("agent");
    for borrowed in [false, true] {
        let execution = execution();
        let expected = clone_oracle(Some(&execution));
        let addresses = execution
            .work_items
            .iter()
            .map(|(key, record)| {
                let wait = match &record.state {
                    WorkItemExecutionState::Waiting { wait, .. } => {
                        Some((wait.wait_id.as_ptr() as usize, wait.wait_id.capacity()))
                    }
                    _ => None,
                };
                (key.clone(), key.as_ptr() as usize, key.capacity(), wait)
            })
            .collect::<Vec<_>>();
        let queue = WorkQueueReadModel::default();
        let queue = if borrowed {
            Cow::Borrowed(&queue)
        } else {
            Cow::Owned(queue)
        };
        let projection = SchedulerProjection::from_queue_observations(
            SchedulerAgentSnapshot::from_state(&state),
            0,
            queue,
            observations(Some(execution)),
            Utc::now(),
        );
        assert_eq!(projection.canonical_work_states, expected);
        let states = projection.canonical_work_states.as_ref().unwrap();
        for (id, pointer, capacity, wait_buffer) in addresses {
            let (key, state) = states.get_key_value(&id).unwrap();
            assert_eq!(key.as_ptr() as usize, pointer);
            assert_eq!(key.capacity(), capacity);
            if let Some((pointer, capacity)) = wait_buffer {
                let CanonicalWorkExecutionState::Waiting { wait_id } = state else {
                    panic!("Waiting mapping changed");
                };
                assert_eq!(wait_id.as_ptr() as usize, pointer);
                assert_eq!(wait_id.capacity(), capacity);
            }
        }
    }
}

#[test]
fn canonical_transfer_preserves_revision_generation_and_fail_closed_authority() {
    let mut item = WorkItemRecord::new("agent", "authority", WorkItemState::Open);
    item.id = String::new();
    item.revision = u64::MAX;
    let state = AgentState::new("agent");
    let mut empty = execution();
    empty.work_items.clear();
    for execution in [None, Some(empty), Some(execution())] {
        let has_runnable = execution
            .as_ref()
            .is_some_and(|snapshot| !snapshot.work_items.is_empty());
        let projection = SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&state),
            0,
            WorkQueueReadModel::default(),
            observations(execution),
            Utc::now(),
        );
        assert_eq!(
            projection.execution_authorizes_autonomous(&item),
            has_runnable
        );
        assert_eq!(
            projection.autonomous_execution_generation(&item),
            has_runnable.then_some(u64::MAX)
        );
        let mut stale = item.clone();
        stale.revision -= 1;
        assert!(!projection.execution_authorizes_autonomous(&stale));
        assert_eq!(projection.autonomous_execution_generation(&stale), None);
        let mut absent = item.clone();
        absent.id = "absent canonical key".into();
        assert!(!projection.execution_authorizes_autonomous(&absent));
        assert_eq!(projection.autonomous_execution_generation(&absent), None);
        for (id, state) in projection.canonical_work_states.as_ref().unwrap() {
            if !matches!(state, CanonicalWorkExecutionState::Runnable { .. }) {
                let mut not_runnable = item.clone();
                not_runnable.id = id.clone();
                assert!(!projection.execution_authorizes_autonomous(&not_runnable));
                assert_eq!(
                    projection.autonomous_execution_generation(&not_runnable),
                    None
                );
            }
        }
    }
}
