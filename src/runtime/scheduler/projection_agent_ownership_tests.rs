use super::*;
use crate::types::{PendingWakeHint, TurnTerminalRecord, WorkItemState};

fn observations() -> SchedulerObservations<'static> {
    SchedulerObservations {
        active_tasks: Vec::new(),
        has_interrupted_replay: true,
        active_wait_conditions: &[],
        activation_waits: Vec::new(),
        execution_snapshot: None,
        active_timers: 7,
        runtime_error: true,
    }
}

// Independent old clone fields and unchanged facts for this explicit fixture.
fn clone_oracle(
    snapshot: &SchedulerAgentSnapshot,
    queue: &WorkQueueReadModel,
    now: DateTime<Utc>,
) -> SchedulerProjection {
    SchedulerProjection {
        now,
        agent_id: snapshot.id.clone(),
        status: snapshot.status.clone(),
        queue_len: 19,
        has_interrupted_replay: true,
        active_run_id: snapshot.active_run_id.clone(),
        active_tasks: Vec::new(),
        has_blocking_active_tasks: false,
        current_work_item: queue.current.clone(),
        current_work_item_scheduling_state: None,
        queued_runnable_work_items: Vec::new(),
        queued_work_items: 0,
        pending_wake_hint: snapshot.pending_wake_hint,
        active_waiting_intents: 0,
        active_work_item_waiting_intents: 0,
        active_agent_waiting_intents: 0,
        active_timers: 7,
        waiting_work_item: None,
        waiting_work_item_scheduling_state: None,
        last_turn_terminal: snapshot.last_turn_terminal.clone(),
        turn_in_progress: snapshot.active_run_id.is_some(),
        runtime_error: true,
        activation_waits: Vec::new(),
        canonical_work_states: Some(HashMap::new()),
    }
}

#[test]
fn agent_snapshot_move_matches_old_clone_fields_and_full_projection() {
    let now = Utc::now();
    for status in [
        AgentStatus::Booting,
        AgentStatus::AwakeIdle,
        AgentStatus::AwakeRunning,
        AgentStatus::AwaitingTask,
        AgentStatus::Asleep,
        AgentStatus::Stopped,
    ] {
        for terminal in [
            None,
            Some(TurnTerminalKind::Completed),
            Some(TurnTerminalKind::Aborted),
            Some(TurnTerminalKind::Interrupted),
            Some(TurnTerminalKind::BaselineOverBudget),
            Some(TurnTerminalKind::DeferredToFallback),
            Some(TurnTerminalKind::ProviderFailedNeedsRecovery),
        ] {
            for id in ["", "agent 汉🦀"] {
                for run in [None, Some(""), Some("run 汉🦀")] {
                    for pending in [false, true] {
                        for has_current in [false, true] {
                            let queue = WorkQueueReadModel {
                                current: has_current.then(|| {
                                    WorkItemRecord::new(
                                        "queue-owner",
                                        "retained objective 汉🦀",
                                        WorkItemState::Open,
                                    )
                                }),
                                ..Default::default()
                            };
                            let original = queue.clone();
                            for owned in [false, true] {
                                let snapshot = SchedulerAgentSnapshot {
                                    id: id.into(),
                                    status: status.clone(),
                                    active_run_id: run.map(str::to_owned),
                                    pending_wake_hint: pending,
                                    last_turn_terminal: terminal.clone(),
                                };
                                let expected = clone_oracle(&snapshot, &queue, now);
                                let actual = SchedulerProjection::from_queue_observations(
                                    snapshot,
                                    19,
                                    if owned {
                                        Cow::Owned(queue.clone())
                                    } else {
                                        Cow::Borrowed(&queue)
                                    },
                                    observations(),
                                    now,
                                );
                                assert_eq!(actual, expected);
                                assert_eq!(queue.current, original.current);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn agent_snapshot_moves_text_pointers_and_spare_capacity_for_both_queue_modes() {
    let now = Utc::now();
    let queue = WorkQueueReadModel::default();
    for owned in [false, true] {
        for text in ["", "retained identity 汉🦀"] {
            let mut id = String::with_capacity(4096);
            id.push_str(text);
            let mut run = String::with_capacity(8192);
            run.push_str(text);
            let id_allocation = (id.as_ptr(), id.capacity());
            let run_allocation = (run.as_ptr(), run.capacity());
            let projection = SchedulerProjection::from_queue_observations(
                SchedulerAgentSnapshot {
                    id,
                    status: AgentStatus::AwakeRunning,
                    active_run_id: Some(run),
                    pending_wake_hint: true,
                    last_turn_terminal: Some(TurnTerminalKind::Interrupted),
                },
                19,
                if owned {
                    Cow::Owned(queue.clone())
                } else {
                    Cow::Borrowed(&queue)
                },
                observations(),
                now,
            );
            assert_eq!(
                (projection.agent_id.as_ptr(), projection.agent_id.capacity()),
                id_allocation
            );
            let run = projection.active_run_id.as_ref().unwrap();
            assert_eq!((run.as_ptr(), run.capacity()), run_allocation);
            assert_eq!(projection.agent_id, text);
            assert_eq!(run, text);
            assert!(projection.turn_in_progress);
        }
    }
}

#[test]
fn agent_state_capture_stays_independent_and_moves_only_the_snapshot_copy() {
    let now = Utc::now();
    let mut state = AgentState::new("source identity 汉🦀".repeat(73));
    state.status = AgentStatus::Asleep;
    state.current_run_id = Some("source run 汉🦀".repeat(61));
    state.pending_wake_hint = Some(PendingWakeHint {
        reason: "wake 汉🦀".into(),
        description: None,
        source: None,
        scope: None,
        external_trigger_id: None,
        resource: None,
        body: None,
        content_type: None,
        correlation_id: None,
        causation_id: None,
        created_at: now,
    });
    state.last_turn_terminal = Some(TurnTerminalRecord {
        turn_id: "turn".into(),
        turn_index: 17,
        kind: TurnTerminalKind::ProviderFailedNeedsRecovery,
        reason: Some("not retained".repeat(31)),
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: now,
        duration_ms: 29,
    });
    let original = serde_json::to_value(&state).unwrap();
    let snapshot = SchedulerAgentSnapshot::from_state(&state);
    let snapshot_id = snapshot.id.as_ptr();
    let snapshot_run = snapshot.active_run_id.as_ref().unwrap().as_ptr();
    assert_ne!(snapshot_id, state.id.as_ptr());
    assert_ne!(
        snapshot_run,
        state.current_run_id.as_ref().unwrap().as_ptr()
    );
    let queue = WorkQueueReadModel::default();
    let expected = clone_oracle(&snapshot, &queue, now);
    let projection =
        SchedulerProjection::from_observations(snapshot, 19, queue, observations(), now);
    assert_eq!(projection, expected);
    assert_eq!(projection.agent_id.as_ptr(), snapshot_id);
    assert_eq!(
        projection.active_run_id.as_ref().unwrap().as_ptr(),
        snapshot_run
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), original);
}
