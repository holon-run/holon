use super::super::support::*;
use crate::runtime::{WaitForRegistrationOutcome, WaitForWakeKind};
use crate::types::{
    AdmissionContext, MessageEnvelope, QueueEntryRecord, WaitConditionStatus, WorkItemState,
};
use chrono::DateTime;

fn task_result_message(task_id: &str) -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "task completed".into(),
        },
    )
}

fn running_task(
    task_id: &str,
    work_item_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> TaskRecord {
    TaskRecord {
        id: task_id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: Some(work_item_id.into()),
        summary: Some(task_id.into()),
        detail: None,
        recovery: None,
    }
}

fn terminal_task_with_result(
    task: &TaskRecord,
    result_message: &MessageEnvelope,
    now: chrono::DateTime<chrono::Utc>,
) -> TaskRecord {
    TaskRecord {
        status: TaskStatus::Completed,
        updated_at: now,
        parent_message_id: Some(result_message.id.clone()),
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": task.id,
            "rejoin_generation": 1,
            "parent_turn_id": "turn-late-task-parent",
        })),
        ..task.clone()
    }
}

fn late_task_result_message(task_id: &str, work_item_id: &str) -> MessageEnvelope {
    let mut message = task_result_message(task_id).with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.task_id = Some(task_id.into());
    message.work_item_id = Some(work_item_id.into());
    message.turn_id = Some("turn-late-task-result".into());
    message
}

fn persist_open_work_execution(
    harness: &LifecycleHarness,
    work_item: &WorkItemRecord,
    attempt_id: &str,
) {
    use crate::domain::execution_protocol::{
        AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionOrigin,
        ExecutionPriority, ExecutionProtocolState, ExecutionProvenance, ExecutionSource,
        ExecutionSourceIdentity, ExecutionTrust, WorkItemExecutionRecord, WorkItemExecutionState,
    };

    let generation = work_item.revision.max(1);
    let mut execution = ExecutionProtocolState::empty("default");
    execution.work_items.insert(
        work_item.id.clone(),
        WorkItemExecutionRecord {
            source_revision: generation,
            state: WorkItemExecutionState::InFlight {
                generation,
                attempt_id: attempt_id.into(),
            },
        },
    );
    execution.attempts.insert(
        attempt_id.into(),
        ExecutionAttempt {
            attempt_id: attempt_id.into(),
            agent_id: "default".into(),
            source_message_id: Some(format!("message:{attempt_id}")),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::WorkItemContinuation {
                    work_item_id: work_item.id.clone(),
                },
                generation,
            },
            binding: ExecutionBinding::WorkItem {
                work_item_id: work_item.id.clone(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::System,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: generation,
                work_item_source_revision: Some(generation),
                work_item_generation: Some(generation),
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: Some("turn-late-wait".into()),
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: harness.now().to_rfc3339(),
            terminal_at: None,
        },
    );
    harness
        .runtime()
        .inner
        .runtime_db
        .transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &execution))
        .unwrap();
}

async fn persist_waiting_work_execution(
    harness: &LifecycleHarness,
    work_item_id: &str,
    wait_id: &str,
) {
    use crate::domain::execution_protocol::{
        ExecutionProtocolState, WaitReference, WorkItemExecutionRecord, WorkItemExecutionState,
    };

    let work_item = harness
        .runtime()
        .latest_work_item(work_item_id)
        .await
        .unwrap()
        .expect("waiting execution requires a durable WorkItem");
    let generation = work_item.revision.max(1);
    let mut execution = ExecutionProtocolState::empty("default");
    execution.work_items.insert(
        work_item.id,
        WorkItemExecutionRecord {
            source_revision: generation,
            state: WorkItemExecutionState::Waiting {
                generation,
                wait: WaitReference {
                    wait_id: wait_id.into(),
                },
            },
        },
    );
    harness
        .runtime()
        .inner
        .runtime_db
        .transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &execution))
        .unwrap();
}

#[tokio::test]
async fn wait_runtime_path_rolls_back_each_pre_commit_fault() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("wait fault contract".into(), None, None, Vec::new())
            .await
            .unwrap();
        let before = harness.snapshot();
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::External,
                Some("github:holon-run/holon#2258".into()),
                "waiting for lifecycle contract".into(),
                None,
            )
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        let registered = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::External,
                Some("github:holon-run/holon#2258".into()),
                "waiting for lifecycle contract".into(),
                None,
            )
            .await
            .unwrap();
        let after = harness.snapshot();
        let mut expected_condition = registered.condition;
        expected_condition.created_at =
            DateTime::from_timestamp_millis(expected_condition.created_at.timestamp_millis())
                .unwrap();
        expected_condition.updated_at =
            DateTime::from_timestamp_millis(expected_condition.updated_at.timestamp_millis())
                .unwrap();
        assert_eq!(after.wait_conditions, vec![expected_condition]);
        assert_eq!(after.work_items[0].revision, work_item.revision + 1);
        assert_eq!(
            after.index_outbox_high_watermark,
            before.index_outbox_high_watermark + 1
        );
    }
}

#[tokio::test]
async fn wait_post_commit_fault_recovers_active_wait_after_restart() {
    for (fault, expected_effect) in POST_COMMIT_FAULTS {
        let mut harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("post-commit wait".into(), None, None, Vec::new())
            .await
            .unwrap();
        harness.arm_fault(fault);

        let registered = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::External,
                Some("github:holon-run/holon#2258".into()),
                "waiting for lifecycle recovery".into(),
                None,
            )
            .await
            .unwrap();

        harness.assert_post_commit_warning(expected_effect);
        assert_eq!(
            harness
                .runtime()
                .storage()
                .active_wait_conditions_for_work_item("default", &work_item.id)
                .unwrap(),
            vec![registered.condition.clone()]
        );
        let committed = harness.snapshot();

        harness.restart();

        assert_eq!(harness.snapshot(), committed);
        assert_eq!(
            harness
                .runtime()
                .storage()
                .active_wait_conditions_for_work_item("default", &work_item.id)
                .unwrap(),
            vec![registered.condition]
        );
        assert_eq!(
            harness
                .runtime()
                .latest_work_item(&work_item.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            WorkItemState::Open
        );
    }
}

#[tokio::test]
async fn wait_trigger_and_message_enqueue_are_atomic_across_pre_commit_faults() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("atomic wait trigger".into(), None, None, Vec::new())
            .await
            .unwrap();
        harness
            .runtime()
            .storage()
            .append_task(&running_task(
                "task-atomic-trigger",
                &work_item.id,
                harness.now(),
            ))
            .unwrap();
        let registration = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::TaskResult,
                Some("task-atomic-trigger".into()),
                "waiting for atomic task result".into(),
                None,
            )
            .await
            .unwrap();
        let before = harness.snapshot();
        let message = task_result_message("task-atomic-trigger");
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .enqueue(message.clone())
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);

        let queued = harness.runtime().enqueue(message).await.unwrap();
        let after = harness.snapshot();
        let triggered = after
            .wait_conditions
            .iter()
            .find(|condition| condition.id == registration.condition.id)
            .unwrap();
        assert_eq!(triggered.status, WaitConditionStatus::Triggered);
        assert_eq!(triggered.trigger_message_id(), Some(queued.id.as_str()));
        assert_eq!(triggered.triggered_at(), Some(harness.now()));
        assert!(after.queue_entries.iter().any(|entry| {
            entry.message_id == queued.id && entry.status == QueueEntryStatus::Queued
        }));
        assert!(after.audit_events.iter().any(|event| {
            event.kind == "wait_condition_triggered"
                && event.data["wait_condition_id"].as_str()
                    == Some(registration.condition.id.as_str())
                && event.data["trigger_message_id"].as_str() == Some(queued.id.as_str())
        }));
    }
}

#[tokio::test]
async fn late_task_result_queue_and_execution_settlement_are_atomic() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("late task result atomicity".into(), None, None, Vec::new())
            .await
            .unwrap();
        let running = running_task("task-late-atomic", &work_item.id, harness.now());
        harness
            .runtime()
            .persist_task_transition(&running, "task_created")
            .await
            .unwrap();
        let result_message = late_task_result_message("task-late-atomic", &work_item.id);
        let terminal = terminal_task_with_result(&running, &result_message, harness.now());
        harness
            .runtime()
            .persist_task_transition_with_message(&terminal, "task_status_updated", &result_message)
            .await
            .unwrap();
        persist_open_work_execution(&harness, &work_item, "attempt-late-atomic");
        let before = harness.snapshot();
        let execution_before = harness
            .runtime()
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap()
            .unwrap();
        harness.arm_fault(fault);

        let error = harness
            .runtime()
            .register_wait_for_outcome(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::TaskResult,
                Some(terminal.id.clone()),
                "late result should continue".into(),
                None,
            )
            .await
            .unwrap_err();

        assert_injected_transition_fault(&error);
        harness.assert_unchanged(&before);
        assert_eq!(
            harness
                .runtime()
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized("default")
                .unwrap()
                .unwrap(),
            execution_before
        );

        let outcome = harness
            .runtime()
            .register_wait_for_outcome(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::TaskResult,
                Some(terminal.id.clone()),
                "late result should continue".into(),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            WaitForRegistrationOutcome::TaskResultQueued {
                task_id,
                result_message_id,
            } if task_id == terminal.id && result_message_id == result_message.id
        ));
        let after = harness.snapshot();
        assert!(after.wait_conditions.is_empty());
        assert!(after.queue_entries.iter().any(|entry| {
            entry.message_id == result_message.id && entry.status == QueueEntryStatus::Queued
        }));
        assert_eq!(
            after
                .messages
                .iter()
                .filter(|message| message.id == result_message.id)
                .count(),
            1
        );
        let execution = harness
            .runtime()
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap()
            .unwrap();
        let attempt = &execution.attempts["attempt-late-atomic"];
        assert_eq!(
            attempt.state,
            crate::domain::execution_protocol::ExecutionAttemptState::Settled
        );
        assert!(matches!(
            &execution.outcomes[attempt.terminal_outcome_id.as_deref().unwrap()].outcome,
            crate::domain::execution_protocol::ExecutionOutcome::WorkItem(
                crate::domain::execution_protocol::WorkItemOutcome::Continue
            )
        ));
    }
}

#[tokio::test]
async fn late_task_result_terminal_queue_states_are_already_consumed() {
    for status in [
        QueueEntryStatus::Dequeued,
        QueueEntryStatus::Interjected,
        QueueEntryStatus::Processed,
        QueueEntryStatus::Aborted,
        QueueEntryStatus::Dropped,
        QueueEntryStatus::Quarantined,
    ] {
        let harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("late task consumed".into(), None, None, Vec::new())
            .await
            .unwrap();
        let running = running_task("task-late-consumed", &work_item.id, harness.now());
        let result_message = late_task_result_message("task-late-consumed", &work_item.id);
        let terminal = terminal_task_with_result(&running, &result_message, harness.now());
        harness
            .runtime()
            .persist_task_transition_with_message(&terminal, "task_status_updated", &result_message)
            .await
            .unwrap();
        harness
            .runtime()
            .storage()
            .append_queue_entry(&QueueEntryRecord {
                message_id: result_message.id.clone(),
                agent_id: "default".into(),
                priority: result_message.priority.clone(),
                status: status.clone(),
                created_at: result_message.created_at,
                updated_at: harness.now(),
            })
            .unwrap();
        let before = harness.snapshot();

        let outcome = harness
            .runtime()
            .register_wait_for_outcome(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::TaskResult,
                Some(terminal.id.clone()),
                "already consumed".into(),
                None,
            )
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            WaitForRegistrationOutcome::TaskResultAlreadyConsumed {
                task_id,
                result_message_id,
            } if task_id == terminal.id && result_message_id == result_message.id
        ));
        harness.assert_unchanged(&before);
    }
}

#[tokio::test]
async fn late_task_result_missing_message_evidence_is_validation_only() {
    let harness = LifecycleHarness::new();
    let work_item = harness
        .runtime()
        .create_work_item("late task missing evidence".into(), None, None, Vec::new())
        .await
        .unwrap();
    let mut terminal = running_task("task-late-missing", &work_item.id, harness.now());
    terminal.status = TaskStatus::Completed;
    terminal.parent_message_id = Some("message-late-missing".into());
    harness
        .runtime()
        .persist_task_transition(&terminal, "task_status_updated")
        .await
        .unwrap();
    let before = harness.snapshot();

    let error = harness
        .runtime()
        .register_wait_for_outcome(
            "default",
            Some(work_item.id),
            WaitForWakeKind::TaskResult,
            Some(terminal.id),
            "missing evidence".into(),
            None,
        )
        .await
        .unwrap_err();

    assert_eq!(
        crate::runtime_error::describe_runtime_error(&error).code,
        "task_result_evidence_missing"
    );
    harness.assert_unchanged(&before);
}

#[tokio::test(start_paused = true)]
async fn duplicate_historical_waits_do_not_reject_current_trigger_message() {
    let harness = LifecycleHarness::new();
    let historical_work = harness
        .runtime()
        .create_work_item("historical task wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    let current_work = harness
        .runtime()
        .create_work_item("current task wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    harness
        .runtime()
        .storage()
        .append_task(&running_task(
            "task-duplicate-wait",
            &current_work.id,
            harness.now(),
        ))
        .unwrap();
    let current = harness
        .runtime()
        .register_wait_for(
            "default",
            Some(current_work.id),
            WaitForWakeKind::TaskResult,
            Some("task-duplicate-wait".into()),
            "current wait".into(),
            None,
        )
        .await
        .unwrap();
    let mut historical = current.condition.clone();
    historical.id = format!("{}-historical", current.condition.id);
    historical.work_item_id = Some(historical_work.id);
    historical.waiting_for = "historical wait".into();
    historical.created_at -= chrono::Duration::milliseconds(1);
    historical.updated_at = historical.created_at;
    harness
        .runtime()
        .storage()
        .append_wait_condition(&historical)
        .unwrap();

    let queued = harness
        .runtime()
        .enqueue(task_result_message("task-duplicate-wait"))
        .await
        .unwrap();

    let waits = harness.snapshot().wait_conditions;
    let historical = waits
        .iter()
        .find(|condition| condition.id == historical.id)
        .unwrap();
    let current = waits
        .iter()
        .find(|condition| condition.id == current.condition.id)
        .unwrap();
    assert_eq!(historical.status, WaitConditionStatus::Active);
    assert_eq!(current.status, WaitConditionStatus::Triggered);
    assert_eq!(current.trigger_message_id(), Some(queued.id.as_str()));
}

#[tokio::test]
async fn wait_resolution_and_execution_admission_are_atomic_across_pre_commit_faults() {
    for fault in PRE_COMMIT_FAULTS {
        let harness = LifecycleHarness::new();
        let work_item = harness
            .runtime()
            .create_work_item("atomic wait admission".into(), None, None, Vec::new())
            .await
            .unwrap();
        let registration = harness
            .runtime()
            .register_wait_for(
                "default",
                Some(work_item.id.clone()),
                WaitForWakeKind::OperatorInput,
                None,
                "waiting for operator admission".into(),
                None,
            )
            .await
            .unwrap();
        persist_waiting_work_execution(&harness, &work_item.id, &registration.condition.id).await;
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("control".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "resume atomically".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::HttpControlPrompt,
            AdmissionContext::ControlAuthenticated,
        );
        message.work_item_id = Some(work_item.id.clone());
        let queued = harness.runtime().enqueue(message).await.unwrap();
        let triggered = harness
            .snapshot()
            .wait_conditions
            .into_iter()
            .find(|condition| condition.id == registration.condition.id)
            .unwrap();
        assert_eq!(triggered.status, WaitConditionStatus::Triggered);
        let before_claim = harness.snapshot();
        harness.arm_fault(fault);

        let error = match super::super::super::scheduler_executor::SchedulerDecisionExecutor::new(
            harness.runtime(),
        )
        .poll()
        .await
        {
            Ok(_) => panic!("faulted claim should fail before commit"),
            Err(error) => error,
        };

        assert_injected_transition_fault(&error);
        let after_fault = harness.snapshot();
        assert_eq!(after_fault.agent_state, before_claim.agent_state);
        assert_eq!(after_fault.work_items, before_claim.work_items);
        assert_eq!(
            after_fault.work_item_continuations,
            before_claim.work_item_continuations
        );
        assert_eq!(after_fault.wait_conditions, before_claim.wait_conditions);
        assert_eq!(after_fault.queue_entries, before_claim.queue_entries);
        assert_eq!(after_fault.tasks, before_claim.tasks);
        assert_eq!(after_fault.messages, before_claim.messages);
        assert_eq!(after_fault.briefs, before_claim.briefs);
        assert_eq!(
            after_fault.transcript_entries,
            before_claim.transcript_entries
        );
        assert_eq!(
            after_fault.index_outbox_high_watermark,
            before_claim.index_outbox_high_watermark
        );
        assert!(after_fault
            .audit_events
            .iter()
            .skip(before_claim.audit_events.len())
            .all(|event| event.kind == "scheduling_advisory"));
        let execution_after_fault = harness
            .runtime()
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap()
            .expect("failed claim must preserve waiting execution authority");
        assert!(execution_after_fault.attempts.is_empty());
        assert!(matches!(
            &execution_after_fault.work_items[&work_item.id].state,
            crate::domain::execution_protocol::WorkItemExecutionState::Waiting { wait, .. }
                if wait.wait_id == registration.condition.id
        ));

        let poll = super::super::super::scheduler_executor::SchedulerDecisionExecutor::new(
            harness.runtime(),
        )
        .poll()
        .await
        .unwrap();
        let super::super::super::scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
            panic!("retry should atomically admit the triggered wait");
        };
        assert_eq!(scheduled.message.id, queued.id);
        let after_claim = harness.snapshot();
        let resolved = after_claim
            .wait_conditions
            .iter()
            .find(|condition| condition.id == registration.condition.id)
            .unwrap();
        assert_eq!(resolved.status, WaitConditionStatus::Resolved);
        assert_eq!(resolved.trigger_message_id(), Some(queued.id.as_str()));
        assert_eq!(after_claim.work_items[0].blocked_by, None);
        assert!(after_claim.queue_entries.iter().any(|entry| {
            entry.message_id == queued.id && entry.status == QueueEntryStatus::Dequeued
        }));
        let execution = harness
            .runtime()
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap()
            .expect("successful claim should initialize execution protocol");
        assert!(execution
            .attempts
            .values()
            .any(|attempt| attempt.source_message_id.as_deref() == Some(queued.id.as_str())));
    }
}

#[tokio::test]
async fn terminal_task_replay_repairs_wait_once_across_restart() {
    let mut harness = LifecycleHarness::new();
    let work = harness
        .runtime()
        .create_work_item("repair task wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    let running = running_task("task-replay", &work.id, harness.now());
    harness.runtime().storage().append_task(&running).unwrap();
    let registration = harness
        .runtime()
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::TaskResult,
            Some("task-replay".into()),
            "waiting for task-replay".into(),
            None,
        )
        .await
        .unwrap();
    persist_waiting_work_execution(&harness, &work.id, &registration.condition.id).await;
    let mut message = task_result_message("task-replay").with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.task_id = Some("task-replay".into());
    message.work_item_id = Some(work.id.clone());
    let terminal = terminal_task_with_result(&running, &message, harness.now());
    harness.runtime().storage().append_task(&terminal).unwrap();
    let before = harness
        .runtime()
        .inner
        .runtime_db
        .runtime_index_outbox()
        .high_watermark_for_agent("default")
        .unwrap();

    harness
        .runtime()
        .commit_terminal_task_result(&terminal, "task_result_received", &message)
        .await
        .unwrap();
    let repaired = harness.snapshot();
    let triggered = repaired
        .wait_conditions
        .iter()
        .find(|condition| condition.id == registration.condition.id)
        .unwrap();
    assert_eq!(triggered.status, WaitConditionStatus::Triggered);
    assert_eq!(triggered.trigger_message_id(), Some(message.id.as_str()));
    assert_eq!(
        repaired.work_items[0].blocked_by.as_deref(),
        Some("waiting for task-replay")
    );
    harness.restart();
    harness
        .runtime()
        .commit_terminal_task_result(&terminal, "task_result_received", &message)
        .await
        .unwrap();
    let replayed = harness.snapshot();
    assert_eq!(replayed.tasks, repaired.tasks);
    assert_eq!(replayed.messages, repaired.messages);
    assert_eq!(replayed.wait_conditions, repaired.wait_conditions);
    assert_eq!(replayed.audit_events, repaired.audit_events);
    assert_eq!(replayed.queue_entries.len(), 1);
    assert_eq!(replayed.queue_entries[0].message_id, message.id);
    assert_eq!(
        replayed.queue_entries[0].status,
        repaired.queue_entries[0].status
    );
    let wait_transition = harness
        .runtime()
        .wait_resolution_transition_for_message(&message)
        .unwrap()
        .expect("replayed TaskResult should plan exact wait settlement");
    assert_eq!(wait_transition.record.status, WaitConditionStatus::Resolved);
    assert!(
        wait_transition.work_item.is_some(),
        "exact wait settlement should clear the matching WorkItem blocker"
    );
    let poll =
        super::super::super::scheduler_executor::SchedulerDecisionExecutor::new(harness.runtime())
            .poll()
            .await
            .unwrap();
    let super::super::super::scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
        panic!("replayed task result should be claimed after restart");
    };
    assert_eq!(scheduled.message.id, message.id);

    let latest = harness
        .runtime()
        .latest_work_item(&work.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.blocked_by, None);
    assert_eq!(
        harness
            .snapshot()
            .wait_conditions
            .iter()
            .find(|condition| condition.id == registration.condition.id)
            .unwrap()
            .status,
        WaitConditionStatus::Resolved
    );
    let changes = harness
        .runtime()
        .inner
        .runtime_db
        .runtime_index_outbox()
        .read_after("default", before, 20)
        .unwrap();
    assert!(changes.iter().all(|change| change.source_kind != "task"));
    assert_eq!(
        harness
            .snapshot()
            .audit_events
            .iter()
            .filter(|event| event.kind == "task_result_received")
            .count(),
        1
    );
}
