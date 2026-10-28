use super::*;

fn wait(
    kind: WaitConditionKind,
    status: WaitConditionStatus,
    work_item_id: Option<&str>,
    now: DateTime<Utc>,
) -> WaitConditionRecord {
    serde_json::from_value(serde_json::json!({
        "id": "duplicate-wait",
        "agent_id": "agent",
        "work_item_id": work_item_id,
        "kind": kind,
        "status": status,
        "source": "source 汉🦀".repeat(13),
        "subject_ref": "subject 汉🦀".repeat(17),
        "waiting_for": "nested wait body 汉🦀".repeat(71),
        "wake_sources": [
            {"kind": "task_result", "task_id": "task 汉🦀".repeat(19)},
            {"kind": "operator_input"}
        ],
        "continuation": {"nested": [{"text": "continuation 汉🦀".repeat(47)}]},
        "created_at": now,
        "updated_at": now,
        "turn_id": "turn 汉🦀".repeat(23),
        "trigger_message_id": "message 汉🦀".repeat(29)
    }))
    .unwrap()
}

fn observations(waits: &[WaitConditionRecord]) -> SchedulerObservations<'_> {
    SchedulerObservations {
        active_tasks: Vec::new(),
        has_interrupted_replay: true,
        active_wait_conditions: waits,
        activation_waits: Vec::new(),
        execution_snapshot: None,
        active_timers: 7,
        runtime_error: true,
    }
}

fn counts(projection: &SchedulerProjection) -> (usize, usize, usize) {
    (
        projection.active_waiting_intents,
        projection.active_work_item_waiting_intents,
        projection.active_agent_waiting_intents,
    )
}

// Independent count oracle: membership/status selection belongs to the caller.
fn count_oracle(waits: &[WaitConditionRecord]) -> (usize, usize, usize) {
    let (mut work, mut agent) = (0, 0);
    for wait in waits {
        match (&wait.work_item_id, &wait.kind) {
            (Some(_), _) => work += 1,
            (None, WaitConditionKind::Task) => {}
            (None, _) => agent += 1,
        }
    }
    (waits.len(), work, agent)
}

#[test]
fn borrowed_wait_observations_preserve_kind_status_owner_matrix_and_clone_results() {
    let state = AgentState::new("agent");
    let queue = WorkQueueReadModel::default();
    let now = Utc::now();
    let mut waits = Vec::new();
    for kind in [
        WaitConditionKind::Task,
        WaitConditionKind::External,
        WaitConditionKind::Operator,
        WaitConditionKind::Timer,
        WaitConditionKind::System,
    ] {
        for status in [
            WaitConditionStatus::Active,
            WaitConditionStatus::Triggered,
            WaitConditionStatus::Resolved,
            WaitConditionStatus::Cancelled,
            WaitConditionStatus::Expired,
        ] {
            for owner in [None, Some("work"), Some("")] {
                waits.push(wait(kind.clone(), status.clone(), owner, now));
                // Cover every matrix prefix, not only the aggregate counts.
                let observed = observations(&waits);
                assert!(std::ptr::eq(
                    observed.active_wait_conditions,
                    waits.as_slice()
                ));
                let borrowed = SchedulerProjection::from_borrowed_observations(
                    SchedulerAgentSnapshot::from_state(&state),
                    19,
                    &queue,
                    observed,
                    now,
                );
                let legacy_wait_copy = waits.clone();
                let legacy = SchedulerProjection::from_observations(
                    SchedulerAgentSnapshot::from_state(&state),
                    19,
                    queue.clone(),
                    observations(&legacy_wait_copy),
                    now,
                );
                assert_eq!(borrowed, legacy);
                assert_eq!(counts(&borrowed), count_oracle(&waits));
            }
        }
    }
    // Duplicate IDs and identical records are still counted, not deduplicated.
    waits.push(waits.last().unwrap().clone());
    let original = waits.clone();
    let allocation = (waits.as_ptr(), waits.capacity());
    let text_pointers = waits
        .iter()
        .map(|wait| wait.waiting_for.as_ptr())
        .collect::<Vec<_>>();
    let projection = SchedulerProjection::from_borrowed_observations(
        SchedulerAgentSnapshot::from_state(&state),
        19,
        &queue,
        observations(&waits),
        now,
    );
    assert_eq!(counts(&projection), (76, 51, 20));
    assert_eq!(counts(&projection), count_oracle(&waits));
    assert_eq!(waits, original);
    assert_eq!((waits.as_ptr(), waits.capacity()), allocation);
    assert_eq!(
        waits
            .iter()
            .map(|wait| wait.waiting_for.as_ptr())
            .collect::<Vec<_>>(),
        text_pointers
    );
}

#[test]
fn empty_wait_observations_preserve_zero_counts_for_both_queue_adapters() {
    let state = AgentState::new("agent");
    let queue = WorkQueueReadModel::default();
    let now = Utc::now();
    let waits = Vec::with_capacity(37);
    let allocation = (waits.as_ptr(), waits.capacity());
    let borrowed = SchedulerProjection::from_borrowed_observations(
        SchedulerAgentSnapshot::from_state(&state),
        0,
        &queue,
        observations(&waits),
        now,
    );
    let owned = SchedulerProjection::from_observations(
        SchedulerAgentSnapshot::from_state(&state),
        0,
        queue,
        observations(&waits),
        now,
    );
    assert_eq!(borrowed, owned);
    assert_eq!(counts(&borrowed), (0, 0, 0));
    assert_eq!((waits.as_ptr(), waits.capacity()), allocation);
}

#[test]
fn projection_outlives_observed_waits_and_keeps_original_activation_allocation() {
    let state = AgentState::new("agent");
    let now = Utc::now();
    let mut activation = Vec::with_capacity(13);
    activation.push(wait(
        WaitConditionKind::Task,
        WaitConditionStatus::Resolved,
        Some("work"),
        now,
    ));
    let expected_activation = activation.clone();
    let allocation = (activation.as_ptr(), activation.capacity());
    let nested_pointer = activation[0].waiting_for.as_ptr();
    let projection = {
        let waits = vec![
            wait(
                WaitConditionKind::Task,
                WaitConditionStatus::Active,
                None,
                now,
            ),
            wait(
                WaitConditionKind::Operator,
                WaitConditionStatus::Triggered,
                None,
                now,
            ),
        ];
        let mut observed = observations(&waits);
        observed.activation_waits = activation;
        SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&state),
            0,
            WorkQueueReadModel::default(),
            observed,
            now,
        )
    };
    // The source waits have dropped; only their scalar counts can escape.
    assert_eq!(counts(&projection), (2, 0, 1));
    assert_eq!(projection.activation_waits, expected_activation);
    assert_eq!(
        (
            projection.activation_waits.as_ptr(),
            projection.activation_waits.capacity()
        ),
        allocation
    );
    assert_eq!(
        projection.activation_waits[0].waiting_for.as_ptr(),
        nested_pointer
    );
}
