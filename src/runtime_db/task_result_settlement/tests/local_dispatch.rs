use super::super::local_dispatch::source_was_locally_rejected_tx;
use super::*;
use crate::{
    domain::execution_protocol::{
        ConversationOutcome, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding,
        ExecutionOutcome, ExecutionOutcomeRecord, ExecutionProtocolCommand, ExecutionProtocolState,
        ExecutionSourceIdentity, ExecutionWaitIngress, InterruptExecution, SettleExecution,
        WorkItemExecutionRecord, WorkItemExecutionState, WorkItemOutcome,
    },
    runtime_db::provider_admission::ProviderQueueParking,
    runtime_db::transitions::{
        ExecutionProtocolTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
        TransitionFaultPoint,
    },
    types::{
        AdmissionContext, AuthorityClass, IdleInputBatchRecord, MessageBody,
        MessageDeliverySurface, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
        QueueEntryStatus, TurnOwner, TurnRecord, TurnTerminalKind, TurnTriggerSummary,
        WaitConditionRecord, WorkItemRecord, WorkItemState,
    },
};

fn fixture() -> (
    tempfile::TempDir,
    RuntimeDb,
    TaskResultSettlementRecord,
    QueueTransitionCommand,
    ExecutionProtocolTransition,
) {
    let (dir, db) = runtime_db();
    let mut record = pending_record(0);
    let mut message = reply_result_fixture(&db, &record);
    message.created_at = record.created_at;
    message.work_item_id = record.work_item_id.clone();
    db.messages().upsert(&message).unwrap();
    let now = record.next_recheck_at.unwrap();
    db.transaction(|tx| {
        assert!(charge_recovery_admission_tx(
            tx,
            &record.result_identity,
            "rejected-attempt",
            now,
        )?);
        Ok(())
    })
    .unwrap();
    record = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    record.state = TaskResultSettlementState::CallerAdmitted;
    record.activation_id = Some("rejected-attempt".into());
    record.admitted_at = Some(now);
    db.transaction(|tx| update_tx(tx, &record)).unwrap();
    let attempt: ExecutionAttempt = serde_json::from_value(serde_json::json!({
        "attempt_id": "rejected-attempt", "agent_id": "agent-a",
        "source_message_id": message.id,
        "source": {
            "identity": {"kind": "queue_message", "message_id": message.id},
            "generation": 1,
        },
        "binding": {"kind": "agent_lifecycle", "agent_id": "agent-a"},
        "provenance": {
            "origin": "system", "trust": "runtime_instruction", "priority": "next",
        },
        "admitted_fences": {
            "source_revision": 1, "agent_control_revision": 1, "host_registry_revision": 1,
        },
        "state": "open", "turn_id": "rejected-turn",
        "admitted_at": now.to_rfc3339(),
    }))
    .unwrap();
    let mut bootstrap = ExecutionProtocolState::empty("agent-a");
    bootstrap
        .attempts
        .insert(attempt.attempt_id.clone(), attempt);
    let mut turn = TurnRecord::new("agent-a", "rejected-turn", 1);
    turn.created_at = now;
    turn.owner = Some(TurnOwner::AgentLifecycle {
        agent_id: "agent-a".into(),
    });
    turn.trigger = Some(TurnTriggerSummary::from_message(&message));
    turn.input_message_ids = vec![message.id.clone()];
    turn.idle_input_batch = Some(IdleInputBatchRecord {
        anchor_message_id: message.id.clone(),
        activation_id: record.activation_id.clone(),
        sender_agent_id: "agent-a".into(),
        cutoff: now,
        max_items: 8,
        max_bytes: 1024,
        rendered_bytes: 0,
        items: vec![IdleInputBatchItem {
            message_id: message.id.clone(),
            source_ref: format!("message:{}", message.id),
            rendered: String::new(),
        }],
    });
    turn.terminal = Some(
        serde_json::from_value(serde_json::json!({
            "turn_id": turn.turn_id, "turn_index": 1, "kind": "aborted",
            "completed_at": now, "duration_ms": 0,
        }))
        .unwrap(),
    );
    turn.runtime_failure = Some(
        serde_json::from_value(serde_json::json!({
            "domain": "conflict", "code": "provider_dispatch_rejected", "retryable": false,
            "operator_message": "local dispatch stopped",
        }))
        .unwrap(),
    );
    let mut queue = QueueEntryRecord {
        message_id: message.id,
        agent_id: "agent-a".into(),
        priority: Priority::Next,
        status: QueueEntryStatus::Dequeued,
        created_at: record.created_at,
        updated_at: now,
    };
    db.queue_entries().upsert(&queue).unwrap();
    queue.status = QueueEntryStatus::Aborted;
    let command = QueueTransitionCommand {
        agent_id: "agent-a".into(),
        operation: QueueOperation::Settle,
        mutation: QueueMutation::Upsert(queue),
        scheduler_claim_work_item: None,
        agent_state: None,
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: Some(turn),
        audit_events: Vec::new(),
        notify_scheduler: false,
        fault: None,
        brief_evidence: Vec::new(),
    };
    let execution = ExecutionProtocolTransition {
        bootstrap: Some(bootstrap),
        commands: vec![ExecutionProtocolCommand::Settle(SettleExecution {
            outcome: ExecutionOutcomeRecord {
                outcome_id: "rejected-outcome".into(),
                attempt_id: "rejected-attempt".into(),
                outcome: ExecutionOutcome::Conversation(ConversationOutcome::Paused {
                    reason: "provider_dispatch_rejected:rejected-turn".into(),
                }),
                created_at: now.to_rfc3339(),
            },
        })],
    };
    (dir, db, record, command, execution)
}

fn exact_root_fixture() -> (
    tempfile::TempDir,
    RuntimeDb,
    TaskResultSettlementRecord,
    QueueTransitionCommand,
    ExecutionProtocolTransition,
) {
    exact_root_fixture_with_ingress_status(QueueEntryStatus::Processed)
}

fn exact_root_fixture_with_ingress_status(
    ingress_status: QueueEntryStatus,
) -> (
    tempfile::TempDir,
    RuntimeDb,
    TaskResultSettlementRecord,
    QueueTransitionCommand,
    ExecutionProtocolTransition,
) {
    exact_root_fixture_with_source(ingress_status, false)
}

fn exact_root_fixture_with_source(
    ingress_status: QueueEntryStatus,
    direct: bool,
) -> (
    tempfile::TempDir,
    RuntimeDb,
    TaskResultSettlementRecord,
    QueueTransitionCommand,
    ExecutionProtocolTransition,
) {
    let (dir, db, mut record, mut command, mut execution) = fixture();
    let mut source = MessageEnvelope::new(
        "agent-a",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "task_result_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "resume exact waiter".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    source.work_item_id = Some("work-b".into());
    source.source_refs.extend([
        ("wait_id".into(), "exact-wait".into()),
        ("task_result_message_id".into(), record.message_id.clone()),
        (
            "task_result_identity".into(),
            record.result_identity.clone(),
        ),
        (
            "task_result_parent_turn_id".into(),
            record.parent_turn_id.clone(),
        ),
        (
            "task_result_rejoin_generation".into(),
            record.rejoin_generation.to_string(),
        ),
    ]);
    if direct {
        source = db
            .transaction(|tx| {
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [&record.message_id],
                    |row| row.get(0),
                )?;
                Ok(serde_json::from_str(&payload)?)
            })
            .unwrap();
        record.recovery_attempts = 0;
        record.last_recovery_root_attempt_id = None;
        record.last_recovery_admitted_at = None;
        db.transaction(|tx| {
            tx.execute("DELETE FROM task_result_recovery_admissions", [])?;
            update_tx(tx, &record)
        })
        .unwrap();
    }
    db.messages().upsert(&source).unwrap();
    let wait: WaitConditionRecord = serde_json::from_value(serde_json::json!({
        "id": "exact-wait", "agent_id": "agent-a", "work_item_id": "work-b",
        "status": "resolved", "kind": "task", "source": "WaitFor",
        "subject_ref": record.task_id, "waiting_for": "exact task result",
        "wake_sources": [{"kind": "task_result", "task_id": record.task_id}],
        "trigger_message_id": source.id,
        "created_at": record.created_at, "updated_at": record.updated_at,
        "resolved_at": record.updated_at,
    }))
    .unwrap();
    db.wait_conditions().upsert(&wait).unwrap();
    let mut original = db
        .queue_entries()
        .latest(&record.message_id)
        .unwrap()
        .unwrap();
    original.status = ingress_status;
    db.queue_entries().upsert(&original).unwrap();
    let QueueMutation::Upsert(queue) = &mut command.mutation else {
        unreachable!()
    };
    queue.message_id = source.id.clone();
    let mut dequeued = queue.clone();
    dequeued.status = QueueEntryStatus::Dequeued;
    db.queue_entries().upsert(&dequeued).unwrap();
    let turn = command.turn_record.as_mut().unwrap();
    turn.owner = Some(TurnOwner::WorkItem {
        work_item_id: "work-b".into(),
    });
    turn.current_work_item_id = Some("work-b".into());
    turn.trigger = Some(TurnTriggerSummary::from_message(&source));
    turn.input_message_ids = vec![source.id.clone()];
    turn.idle_input_batch = None;
    let bootstrap = execution.bootstrap.as_mut().unwrap();
    let attempt = bootstrap.attempts.get_mut("rejected-attempt").unwrap();
    attempt.source_message_id = Some(source.id.clone());
    attempt.source.identity = ExecutionSourceIdentity::TriggeredWait {
        wait_id: wait.id,
        trigger_message_id: source.id,
        ingress: (!direct).then_some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
    };
    attempt.binding = ExecutionBinding::WorkItem {
        work_item_id: "work-b".into(),
    };
    attempt.turn_id = None;
    attempt.admitted_fences.work_item_generation = Some(1);
    attempt.admitted_fences.work_item_source_revision = Some(1);
    bootstrap.work_items.insert(
        "work-b".into(),
        WorkItemExecutionRecord {
            source_revision: 1,
            state: WorkItemExecutionState::InFlight {
                generation: 1,
                attempt_id: "rejected-attempt".into(),
            },
        },
    );
    let ExecutionProtocolCommand::Settle(settlement) = &mut execution.commands[0] else {
        unreachable!()
    };
    settlement.outcome.outcome = ExecutionOutcome::WorkItem(WorkItemOutcome::NeedsRepair {
        repair_id: "provider_dispatch_rejected:rejected-turn".into(),
    });
    (dir, db, record, command, execution)
}

fn latest(db: &RuntimeDb, record: &TaskResultSettlementRecord) -> TaskResultSettlementRecord {
    db.task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap()
}

#[test]
fn direct_exact_waiter_local_stop_retains_uncharged_original_grant() {
    let (_dir, db, original, command, execution) =
        exact_root_fixture_with_source(QueueEntryStatus::Dequeued, true);
    db.transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .unwrap();
    let parked = latest(&db, &original);
    assert_eq!(parked.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(parked.activation_id, original.activation_id);
    assert_eq!(parked.work_item_id, original.work_item_id);
    assert_eq!(parked.recovery_attempts, 0);
    assert!(parked.recovery_paused_at.is_some());
    assert!(parked.next_recheck_at.is_none());
    assert!(db
        .transaction(|tx| source_was_locally_rejected_tx(tx, &parked))
        .unwrap());
    assert!(db
        .task_result_settlements()
        .rearm_recovery("agent-a", &original.result_identity, Utc::now())
        .unwrap());
    let rearmed = latest(&db, &original);
    assert_eq!(rearmed.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(rearmed.activation_id, original.activation_id);
    assert_eq!(rearmed.work_item_id, original.work_item_id);
    assert_eq!(rearmed.recovery_attempts, 0);
    assert_eq!(rearmed.disposition, None);
    assert_eq!(rearmed.settled_at, None);
    assert_eq!(rearmed.recovery_paused_at, None);
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-b")))
            .unwrap(),
        TaskResultObligation::Recoverable,
    );
    assert_eq!(
        db.transaction(|tx| Ok(tx.query_row(
            "SELECT COUNT(*) FROM task_result_recovery_admissions",
            [],
            |row| row.get::<_, i64>(0),
        )?))
        .unwrap(),
        0,
    );
}

#[test]
fn direct_exact_waiter_rearm_revokes_closed_or_missing_consuming_owner() {
    for missing in [false, true] {
        let (_dir, db, original, command, execution) =
            exact_root_fixture_with_source(QueueEntryStatus::Dequeued, true);
        for owner_id in ["work-a", "work-b"] {
            let mut owner = WorkItemRecord::new("agent-a", owner_id, WorkItemState::Open);
            owner.id = owner_id.into();
            db.work_items().insert_new(&owner).unwrap();
        }
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
        assert!(db
            .task_result_settlements()
            .rearm_recovery("agent-a", &original.result_identity, Utc::now())
            .unwrap());
        let rearmed = latest(&db, &original);
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-b")))
                .unwrap(),
            TaskResultObligation::Recoverable,
        );
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-a")))
                .unwrap(),
            TaskResultObligation::ClaimedByOtherWaiter {
                wait_condition_id: "exact-wait".into(),
                work_item_id: Some("work-b".into()),
            },
        );
        db.transaction(|tx| {
            let changed = if missing {
                tx.execute("DELETE FROM work_items WHERE work_item_id = 'work-b'", [])?
            } else {
                tx.execute(
                    "UPDATE work_items SET state = 'completed',
                     payload_json = json_set(payload_json, '$.state', 'completed')
                     WHERE work_item_id = 'work-b'",
                    [],
                )?
            };
            assert_eq!(changed, 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            db.work_items().latest("work-a").unwrap().unwrap().state,
            WorkItemState::Open,
        );
        // The direct rejection witness survives rearm, but cannot transfer
        // the consuming wait's promise back to the still-open captured owner.
        assert!(db
            .transaction(|tx| source_was_locally_rejected_tx(tx, &rearmed))
            .unwrap());
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-a")))
                .unwrap(),
            TaskResultObligation::InvalidOrStale,
            "missing consuming owner: {missing}",
        );
        assert_eq!(latest(&db, &original), rearmed);
        assert_eq!(
            db.transaction(|tx| Ok(tx.query_row(
                "SELECT COUNT(*) FROM task_result_recovery_admissions",
                [],
                |row| row.get::<_, i64>(0),
            )?))
            .unwrap(),
            0,
        );
    }
}

#[test]
fn direct_exact_waiter_local_stop_rejects_wrong_ingress_and_wait() {
    for corruption in [
        "marker",
        "authority",
        "surface",
        "admission",
        "task",
        "wait",
    ] {
        let (_dir, db, original, command, mut execution) =
            exact_root_fixture_with_source(QueueEntryStatus::Dequeued, true);
        if corruption == "marker" {
            let attempt = execution
                .bootstrap
                .as_mut()
                .unwrap()
                .attempts
                .get_mut("rejected-attempt")
                .unwrap();
            let ExecutionSourceIdentity::TriggeredWait { ingress, .. } =
                &mut attempt.source.identity
            else {
                unreachable!()
            };
            *ingress = Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup);
        } else {
            db.transaction(|tx| {
                let (table, column, id, path, value) = match corruption {
                    "authority" => (
                        "messages",
                        "message_id",
                        original.message_id.as_str(),
                        "$.authority_class",
                        "operator_instruction",
                    ),
                    "surface" => (
                        "messages",
                        "message_id",
                        original.message_id.as_str(),
                        "$.delivery_surface",
                        "runtime_system",
                    ),
                    "admission" => (
                        "messages",
                        "message_id",
                        original.message_id.as_str(),
                        "$.admission_context",
                        "local_process",
                    ),
                    "task" => (
                        "messages",
                        "message_id",
                        original.message_id.as_str(),
                        "$.task_id",
                        "foreign-task",
                    ),
                    "wait" => (
                        "wait_conditions",
                        "wait_condition_id",
                        "exact-wait",
                        "$.subject_ref",
                        "foreign-task",
                    ),
                    _ => unreachable!(),
                };
                tx.execute(
                    &format!(
                        "UPDATE {table} SET payload_json = json_set(payload_json, ?1, ?2)
                     WHERE {column} = ?3"
                    ),
                    params![path, value, id],
                )?;
                Ok(())
            })
            .unwrap();
        }
        assert!(
            db.transitions()
                .commit_queue_terminal(&command, &execution, None, &[], None)
                .is_err(),
            "{corruption}"
        );
        assert_eq!(latest(&db, &original), original, "{corruption}");
    }
}

fn assert_parked(db: &RuntimeDb, original: &TaskResultSettlementRecord) {
    let record = latest(db, original);
    assert_eq!(record.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(record.activation_id, original.activation_id);
    assert_eq!(record.work_item_id, original.work_item_id);
    assert_eq!(record.admitted_at, original.admitted_at);
    assert_eq!(record.disposition, None);
    assert_eq!(record.settled_at, None);
    assert_eq!(record.recovery_epoch, original.recovery_epoch);
    assert_eq!(record.recovery_attempts, original.recovery_attempts);
    assert_eq!(
        record.last_recovery_root_attempt_id,
        original.last_recovery_root_attempt_id,
    );
    assert!(record.recovery_paused_at.is_some());
    assert!(record.next_recheck_at.is_none());
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &record, None, Some("work-a")))
            .unwrap(),
        TaskResultObligation::Paused,
    );
    assert!(db
        .task_result_settlements()
        .due_deferred("agent-a", Utc::now() + Duration::days(30), 8)
        .unwrap()
        .iter()
        .all(|due| due.result_identity != record.result_identity));
    assert!(!db
        .transaction(|tx| charge_recovery_admission_tx(
            tx,
            &record.result_identity,
            "unrequested-new-root",
            Utc::now() + Duration::days(30),
        ))
        .unwrap());
    assert_eq!(
        db.transaction(|tx| Ok(tx.query_row(
            "SELECT COUNT(*) FROM task_result_recovery_admissions",
            [],
            |row| row.get::<_, i64>(0),
        )?))
        .unwrap(),
        1,
    );
}

#[test]
fn batchless_exact_waiter_local_stop_parks_original_grant_across_restart() {
    let (dir, db, original, command, execution) = exact_root_fixture();
    let turn = command.turn_record.as_ref().unwrap();
    assert!(turn.idle_input_batch.is_none());
    assert!(turn.live_input_batches.is_empty());
    assert_ne!(
        turn.trigger.as_ref().unwrap().message_id.as_ref(),
        Some(&original.message_id),
    );
    db.transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .unwrap();
    let mut expected = original.clone();
    expected.recovery_paused_at = Some(original.updated_at);
    expected.deferred_at = Some(original.updated_at);
    expected.deferred_reason = Some("provider_dispatch_rejected".into());
    expected.next_recheck_at = None;
    assert_eq!(latest(&db, &original), expected);
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &expected, None, Some("work-b")))
            .unwrap(),
        TaskResultObligation::Paused,
    );
    let protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")
        .unwrap()
        .unwrap();
    assert_eq!(
        protocol.attempts["rejected-attempt"].state,
        ExecutionAttemptState::Settled,
    );
    assert!(matches!(
        protocol.work_items["work-b"].state,
        WorkItemExecutionState::NeedsRepair { .. }
    ));
    drop(db);
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    db.recover_interrupted_runtime_state_at_startup().unwrap();
    assert_eq!(latest(&db, &original), expected);
}

#[test]
fn batchless_exact_waiter_synthetic_rearm_keeps_charged_root_across_restart() {
    let (dir, db, original, command, execution) =
        exact_root_fixture_with_ingress_status(QueueEntryStatus::Aborted);
    for _ in 0..2 {
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
    }
    let parked = latest(&db, &original);
    assert_eq!(parked.work_item_id.as_deref(), Some("work-a"));
    assert_eq!(parked.activation_id, original.activation_id);
    assert_eq!(parked.recovery_attempts, original.recovery_attempts);
    assert!(parked.recovery_paused_at.is_some());
    assert!(db
        .transaction(|tx| source_was_locally_rejected_tx(tx, &parked))
        .unwrap());
    let QueueMutation::Upsert(synthetic_queue) = &command.mutation else {
        unreachable!()
    };
    assert_eq!(
        db.queue_entries()
            .latest(&synthetic_queue.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Aborted,
    );
    // An aborted immutable ingress must be justified by the synthetic root,
    // not a Turn or activation batch attached to that original message.
    let ingress = db
        .queue_entries()
        .latest(&original.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(ingress.status, QueueEntryStatus::Aborted);
    assert!(db
        .task_result_settlements()
        .rearm_recovery("agent-a", &original.result_identity, Utc::now())
        .unwrap());
    let rearmed = latest(&db, &original);
    assert_eq!(rearmed.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(rearmed.work_item_id, original.work_item_id);
    assert_eq!(rearmed.activation_id, original.activation_id);
    assert_eq!(rearmed.admitted_at, original.admitted_at);
    assert_eq!(rearmed.recovery_epoch, original.recovery_epoch + 1);
    assert_eq!(rearmed.recovery_attempts, 0);
    assert_eq!(rearmed.last_recovery_root_attempt_id, None);
    assert_eq!(rearmed.recovery_paused_at, None);
    assert_eq!(rearmed.disposition, None);
    assert_eq!(rearmed.settled_at, None);
    assert!(db
        .transaction(|tx| source_was_locally_rejected_tx(tx, &rearmed))
        .unwrap());
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-b")))
            .unwrap(),
        TaskResultObligation::Recoverable,
    );
    assert_eq!(
        db.transaction(|tx| Ok(tx.query_row(
            "SELECT COUNT(*) FROM task_result_recovery_admissions
             WHERE result_identity = ?1 AND recovery_epoch = ?2 AND root_attempt_id = ?3",
            params![
                original.result_identity,
                original.recovery_epoch,
                original.activation_id,
            ],
            |row| row.get::<_, i64>(0),
        )?))
        .unwrap(),
        1,
    );
    drop(db);
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    db.recover_interrupted_runtime_state_at_startup().unwrap();
    assert_eq!(latest(&db, &original), rearmed);
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-b")))
            .unwrap(),
        TaskResultObligation::Recoverable,
    );
    // Corrupt the durable witness directly; terminal API rewrites are forbidden.
    for status in ["dropped", "quarantined"] {
        db.transaction(|tx| {
            assert_eq!(
                tx.execute(
                    "UPDATE queue_entries
                     SET status = ?1, payload_json = json_set(payload_json, '$.status', ?1)
                     WHERE message_id = ?2",
                    params![status, ingress.message_id],
                )?,
                1,
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-b")))
                .unwrap(),
            TaskResultObligation::InvalidOrStale,
        );
    }
}

#[test]
fn batchless_exact_waiter_local_stop_keeps_recorded_run_strict() {
    for run_id in [Some("selected-run"), None, Some("foreign-run")] {
        let (_dir, db, original, mut command, mut execution) = exact_root_fixture();
        let attempt = execution
            .bootstrap
            .as_mut()
            .unwrap()
            .attempts
            .get_mut("rejected-attempt")
            .unwrap();
        attempt.run_id = Some("selected-run".into());
        attempt.turn_id = Some("rejected-turn".into());
        command.turn_record.as_mut().unwrap().run_id = run_id.map(str::to_owned);
        let result = db
            .transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None);
        if run_id == Some("selected-run") {
            result.unwrap();
            let parked = latest(&db, &original);
            assert!(parked.recovery_paused_at.is_some());
            assert!(db
                .transaction(|tx| source_was_locally_rejected_tx(tx, &parked))
                .unwrap());
        } else {
            assert!(result.is_err(), "{run_id:?}");
            assert_eq!(latest(&db, &original), original);
            assert!(db
                .transitions()
                .load_execution_protocol_state_if_initialized("agent-a")
                .unwrap()
                .is_none());
        }
    }
}

#[test]
fn batchless_exact_waiter_local_stop_rolls_back_all_witness_writes() {
    let (_dir, db, original, mut command, execution) = exact_root_fixture();
    command.fault = Some(TransitionFaultPoint::AfterCanonicalWrites);
    assert!(db
        .transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .is_err());
    assert_eq!(latest(&db, &original), original);
    let QueueMutation::Upsert(queue) = &command.mutation else {
        unreachable!()
    };
    assert_eq!(
        db.queue_entries()
            .latest(&queue.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Dequeued,
    );
    assert_eq!(
        db.queue_entries()
            .latest(&original.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Processed,
    );
    assert!(db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")
        .unwrap()
        .is_none());
    assert!(!db
        .transaction(|tx| source_was_locally_rejected_tx(tx, &original))
        .unwrap());
    assert_eq!(
        db.transaction(|tx| Ok(tx
            .query_row("SELECT COUNT(*) FROM turn_records", [], |row| row
                .get::<_, i64>(0),)?))
            .unwrap(),
        0,
    );
    command.fault = None;
    db.transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .unwrap();
    let parked = latest(&db, &original);
    assert!(parked.recovery_paused_at.is_some());
    assert!(db
        .transaction(|tx| source_was_locally_rejected_tx(tx, &parked))
        .unwrap());
}

#[test]
fn batchless_exact_waiter_local_stop_keeps_source_owner_turn_and_wait_fences() {
    for corruption in [
        "source",
        "owner",
        "run",
        "input",
        "wait_owner",
        "wait_status",
        "identity",
        "ingress",
        "recovery",
        "duplicate_turn",
    ] {
        let (_dir, db, original, mut command, mut execution) = exact_root_fixture();
        let turn = command.turn_record.as_mut().unwrap();
        let attempt = execution
            .bootstrap
            .as_mut()
            .unwrap()
            .attempts
            .get_mut("rejected-attempt")
            .unwrap();
        match corruption {
            "source" => attempt.source_message_id = Some("foreign-source".into()),
            "owner" => {
                turn.owner = Some(TurnOwner::WorkItem {
                    work_item_id: "work-c".into(),
                });
                turn.current_work_item_id = Some("work-c".into());
            }
            "run" => attempt.run_id = Some("foreign-run".into()),
            "input" => turn.input_message_ids.clear(),
            "wait_owner" | "wait_status" => db
                .transaction(|tx| {
                    let (field, value) = if corruption == "wait_owner" {
                        ("$.work_item_id", "work-c")
                    } else {
                        ("$.status", "triggered")
                    };
                    tx.execute(
                        "UPDATE wait_conditions SET payload_json = json_set(payload_json, ?1, ?2)
                         WHERE wait_condition_id = 'exact-wait'",
                        params![field, value],
                    )?;
                    Ok(())
                })
                .unwrap(),
            "identity" => db
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE messages
                         SET payload_json = json_set(
                           payload_json, '$.source_refs.task_result_identity', 'sibling-result')
                         WHERE message_id = ?1",
                        [attempt.source_message_id.as_deref().unwrap()],
                    )?;
                    Ok(())
                })
                .unwrap(),
            "ingress" => {
                let ExecutionSourceIdentity::TriggeredWait { ingress, .. } =
                    &mut attempt.source.identity
                else {
                    unreachable!()
                };
                *ingress = None;
            }
            "recovery" => attempt.recovery_of_attempt_id = Some("foreign-root".into()),
            "duplicate_turn" => {
                let mut duplicate = turn.clone();
                duplicate.turn_id = "duplicate-root".into();
                db.turn_records().upsert(&duplicate).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            db.transitions()
                .commit_queue_terminal(&command, &execution, None, &[], None)
                .is_err(),
            "{corruption}",
        );
        assert_eq!(latest(&db, &original), original, "{corruption}");
    }
}

#[test]
fn batchless_exact_waiter_rearm_rejects_corrupt_durable_witnesses() {
    use serde_json::json;

    for (target, path, value) in [
        ("attempt", "$.attempt_id", json!("foreign-attempt")),
        ("attempt", "$.agent_id", json!("foreign-agent")),
        ("attempt", "$.state", json!("interrupted")),
        ("attempt", "$.turn_id", json!("foreign-turn")),
        ("attempt", "$.run_id", json!("foreign-run")),
        ("attempt", "$.source_message_id", json!("foreign-source")),
        (
            "attempt",
            "$.source.identity.trigger_message_id",
            json!("foreign-source"),
        ),
        (
            "attempt",
            "$.source.identity.wait_id",
            json!("foreign-wait"),
        ),
        ("attempt", "$.source.identity.ingress", json!(null)),
        ("attempt", "$.binding.work_item_id", json!("foreign-owner")),
        ("attempt", "$.recovery_of_attempt_id", json!("foreign-root")),
        ("source", "$.id", json!("foreign-source")),
        ("source", "$.agent_id", json!("foreign-agent")),
        ("source", "$.work_item_id", json!("foreign-owner")),
        ("source", "$.kind", json!("task_result")),
        ("source", "$.origin.subsystem", json!("foreign-subsystem")),
        ("source", "$.authority_class", json!("external_evidence")),
        ("source", "$.delivery_surface", json!(null)),
        ("source", "$.admission_context", json!(null)),
        ("source", "$.source_refs.wait_id", json!("foreign-wait")),
        (
            "source",
            "$.source_refs.task_result_message_id",
            json!("foreign-result-message"),
        ),
        (
            "source",
            "$.source_refs.task_result_identity",
            json!("foreign-result"),
        ),
        (
            "source",
            "$.source_refs.task_result_parent_turn_id",
            json!("foreign-parent"),
        ),
        (
            "source",
            "$.source_refs.task_result_rejoin_generation",
            json!("2"),
        ),
        ("original", "$.id", json!("foreign-result-message")),
        ("original", "$.agent_id", json!("foreign-agent")),
        ("original", "$.work_item_id", json!("foreign-owner")),
        ("original", "$.task_id", json!("foreign-task")),
        ("original", "$.kind", json!("internal_followup")),
        ("original", "$.origin.task_id", json!("foreign-task")),
        ("original", "$.authority_class", json!("external_evidence")),
        ("original", "$.delivery_surface", json!(null)),
        ("original", "$.admission_context", json!(null)),
        ("queue", "$.message_id", json!("foreign-source")),
        ("queue", "$.agent_id", json!("foreign-agent")),
        ("queue", "$.status", json!("processed")),
        ("wait", "$.id", json!("foreign-wait")),
        ("wait", "$.agent_id", json!("foreign-agent")),
        ("wait", "$.work_item_id", json!("foreign-owner")),
        ("wait", "$.status", json!("triggered")),
        ("wait", "$.source", json!("foreign-source")),
        ("wait", "$.subject_ref", json!("foreign-task")),
        ("wait", "$.wake_sources", json!([])),
        ("wait", "$.trigger_message_id", json!("foreign-source")),
        ("turn", "$.turn_id", json!("foreign-turn")),
        ("turn", "$.agent_id", json!("foreign-agent")),
        ("turn", "$.owner.work_item_id", json!("foreign-owner")),
        ("turn", "$.current_work_item_id", json!("foreign-owner")),
        ("turn", "$.trigger.message_id", json!("foreign-source")),
        ("turn", "$.input_message_ids", json!([])),
        ("turn", "$.terminal.kind", json!("interrupted")),
        ("turn", "$.runtime_failure.code", json!("other_failure")),
        ("turn", "$.runtime_failure.retryable", json!(true)),
        ("task", "$.detail.rejoin_generation", json!(2)),
    ] {
        let (_dir, db, original, command, execution) =
            exact_root_fixture_with_ingress_status(QueueEntryStatus::Aborted);
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
        assert!(db
            .task_result_settlements()
            .rearm_recovery("agent-a", &original.result_identity, Utc::now())
            .unwrap());
        let rearmed = latest(&db, &original);
        assert!(db
            .transaction(|tx| source_was_locally_rejected_tx(tx, &rearmed))
            .unwrap());
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-b")))
                .unwrap(),
            TaskResultObligation::Recoverable,
        );
        let QueueMutation::Upsert(queue) = &command.mutation else {
            unreachable!()
        };
        let (table, key, id) = match target {
            "attempt" => (
                "execution_protocol_attempts",
                "attempt_id",
                "rejected-attempt",
            ),
            "source" => ("messages", "message_id", queue.message_id.as_str()),
            "original" => ("messages", "message_id", original.message_id.as_str()),
            "queue" => ("queue_entries", "message_id", queue.message_id.as_str()),
            "wait" => ("wait_conditions", "wait_condition_id", "exact-wait"),
            "turn" => ("turn_records", "turn_id", "rejected-turn"),
            "task" => ("tasks", "task_id", original.task_id.as_str()),
            _ => unreachable!(),
        };
        db.transaction(|tx| {
            assert_eq!(
                tx.execute(
                    &format!(
                        "UPDATE {table} SET payload_json = json_set(payload_json, ?1, json(?2))
                         WHERE {key} = ?3"
                    ),
                    params![path, serde_json::to_string(&value)?, id],
                )?,
                1,
                "{target}:{path}",
            );
            Ok(())
        })
        .unwrap();
        assert!(
            !db.transaction(|tx| source_was_locally_rejected_tx(tx, &rearmed))
                .unwrap(),
            "{target}:{path}",
        );
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-b")))
                .unwrap(),
            TaskResultObligation::InvalidOrStale,
            "{target}:{path}",
        );
        assert_eq!(latest(&db, &original), rearmed, "{target}:{path}");
    }
}

#[test]
fn batchless_exact_waiter_rearm_requires_identity_epoch_root_charge() {
    for corruption in ["missing", "identity", "root", "future_epoch"] {
        let (_dir, db, original, command, execution) = exact_root_fixture();
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
        assert!(db
            .task_result_settlements()
            .rearm_recovery("agent-a", &original.result_identity, Utc::now())
            .unwrap());
        let rearmed = latest(&db, &original);
        assert!(db
            .transaction(|tx| source_was_locally_rejected_tx(tx, &rearmed))
            .unwrap());
        let sibling = pending_record(1);
        insert(&db, &sibling);
        db.transaction(|tx| {
            let changed = match corruption {
                "missing" => tx.execute(
                    "DELETE FROM task_result_recovery_admissions WHERE result_identity = ?1",
                    [&original.result_identity],
                )?,
                "identity" => tx.execute(
                    "UPDATE task_result_recovery_admissions SET result_identity = ?2
                     WHERE result_identity = ?1",
                    params![original.result_identity, sibling.result_identity],
                )?,
                "root" => tx.execute(
                    "UPDATE task_result_recovery_admissions SET root_attempt_id = 'foreign-root'
                     WHERE result_identity = ?1",
                    [&original.result_identity],
                )?,
                "future_epoch" => tx.execute(
                    "UPDATE task_result_recovery_admissions SET recovery_epoch = ?2
                     WHERE result_identity = ?1",
                    params![original.result_identity, rearmed.recovery_epoch + 1],
                )?,
                _ => unreachable!(),
            };
            assert_eq!(changed, 1, "{corruption}");
            Ok(())
        })
        .unwrap();
        assert!(
            !db.transaction(|tx| source_was_locally_rejected_tx(tx, &rearmed))
                .unwrap(),
            "{corruption}",
        );
        assert_eq!(latest(&db, &original), rearmed, "{corruption}");
    }
}

#[test]
fn local_dispatch_terminal_parks_selected_obligation_across_repeats_and_restart() {
    let (dir, db, record, command, execution) = fixture();
    let sibling = pending_record(1);
    insert(&db, &sibling);
    for _ in 0..2 {
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
        assert_parked(&db, &record);
        assert_eq!(latest(&db, &sibling), sibling);
    }
    let parked = latest(&db, &record);
    drop(db);
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    assert_eq!(
        db.recover_interrupted_runtime_state_at_startup()
            .unwrap()
            .interrupted_execution_attempts,
        0,
    );
    assert_parked(&db, &record);
    assert_eq!(latest(&db, &record), parked);
    db.transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .unwrap();
    assert_eq!(latest(&db, &record), parked);
}

#[test]
fn interrupted_cooling_preserves_recoverable_source_and_original_parking() {
    let (dir, db, record, mut command, mut execution) = fixture();
    command
        .turn_record
        .as_mut()
        .unwrap()
        .terminal
        .as_mut()
        .unwrap()
        .kind = TurnTerminalKind::Interrupted;
    let QueueMutation::Upsert(queue) = &mut command.mutation else {
        unreachable!()
    };
    queue.status = QueueEntryStatus::Interrupted;
    let parking = ProviderQueueParking {
        agent_id: record.agent_id.clone(),
        message_id: record.message_id.clone(),
        chain_fingerprint: "original-chain".into(),
        episode_key: "original-episode".into(),
        not_before: queue.updated_at + Duration::minutes(5),
        reason: "chain_cooling".into(),
    };
    db.park_provider_queue(&parking).unwrap();
    execution.commands = vec![ExecutionProtocolCommand::Interrupt(InterruptExecution {
        attempt_id: record.activation_id.clone().unwrap(),
        outcome_id: "cooling-interruption".into(),
        reason: "provider chain cooling".into(),
        interrupted_at: queue.updated_at.to_rfc3339(),
    })];
    for _ in 0..2 {
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
        assert_eq!(latest(&db, &record), record);
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &record, None, Some("work-a")))
                .unwrap(),
            TaskResultObligation::Recoverable,
        );
    }
    drop(db);
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    db.recover_interrupted_runtime_state_at_startup().unwrap();
    assert_eq!(latest(&db, &record), record);
    assert_eq!(
        db.provider_queue_parking(&record.agent_id, &record.message_id)
            .unwrap(),
        Some(parking.clone()),
    );
    assert_eq!(
        db.next_provider_queue_recheck_at(&record.agent_id).unwrap(),
        Some(parking.not_before),
    );
    let protocol = db
        .transitions()
        .load_execution_protocol_state_if_initialized(&record.agent_id)
        .unwrap()
        .unwrap();
    let attempt = &protocol.attempts[record.activation_id.as_ref().unwrap()];
    assert_eq!(attempt.state, ExecutionAttemptState::Interrupted);
    assert_eq!(attempt.source_message_id.as_ref(), Some(&record.message_id));
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &record, None, Some("work-a")))
            .unwrap(),
        TaskResultObligation::Recoverable,
    );
}

#[test]
fn interrupted_attempt_is_not_a_settled_local_repair_witness() {
    let (_dir, db, record, command, mut execution) = fixture();
    execution.commands = vec![ExecutionProtocolCommand::Interrupt(InterruptExecution {
        attempt_id: record.activation_id.clone().unwrap(),
        outcome_id: "not-a-repair-outcome".into(),
        reason: "interrupted without a repair settlement".into(),
        interrupted_at: record.updated_at.to_rfc3339(),
    })];
    db.transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .unwrap();
    assert_eq!(latest(&db, &record), record);
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &record, None, Some("work-a")))
            .unwrap(),
        TaskResultObligation::InvalidOrStale,
    );
}

#[test]
fn local_dispatch_ledger_pause_rolls_back_with_queue_turn_and_execution() {
    let (_dir, db, record, mut command, execution) = fixture();
    command.fault = Some(TransitionFaultPoint::AfterCanonicalWrites);
    assert!(db
        .transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .is_err());
    assert_eq!(latest(&db, &record), record);
    assert_eq!(
        db.queue_entries().latest_all().unwrap()[0].status,
        QueueEntryStatus::Dequeued,
    );
    assert!(db
        .transitions()
        .load_execution_protocol_state_if_initialized("agent-a")
        .unwrap()
        .is_none());
    assert_eq!(
        db.transaction(|tx| Ok(
            tx.query_row("SELECT COUNT(*) FROM turn_records", [], |row| {
                row.get::<_, i64>(0)
            })?
        ))
        .unwrap(),
        0,
    );
    command.fault = None;
    db.transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .unwrap();
    assert_parked(&db, &record);
}

#[test]
fn local_dispatch_rejection_cannot_claim_result_delivery() {
    let (_dir, db, record, command, execution) = fixture();
    let delivery = TaskResultActivationSettlement {
        activation_id: record.activation_id.clone().unwrap(),
        disposition: TaskResultSettlementDisposition::ModelDelivered,
        settled_at: command
            .turn_record
            .as_ref()
            .unwrap()
            .terminal
            .as_ref()
            .unwrap()
            .completed_at,
    };
    assert!(db
        .transitions()
        .commit_queue_terminal(&command, &execution, None, &[], Some(&delivery))
        .is_err());
    assert_eq!(latest(&db, &record), record);
    assert_eq!(
        db.queue_entries().latest_all().unwrap()[0].status,
        QueueEntryStatus::Dequeued,
    );
}

#[test]
fn operator_rearm_can_recover_the_exact_locally_rejected_source() {
    let (_dir, db, record, command, execution) = fixture();
    db.transitions()
        .commit_queue_terminal(&command, &execution, None, &[], None)
        .unwrap();
    assert_parked(&db, &record);
    assert!(db
        .task_result_settlements()
        .rearm_recovery("agent-a", &record.result_identity, Utc::now())
        .unwrap());
    let rearmed = latest(&db, &record);
    assert_eq!(rearmed.activation_id, record.activation_id);
    assert_eq!(rearmed.work_item_id, record.work_item_id);
    assert_eq!(rearmed.recovery_epoch, record.recovery_epoch + 1);
    assert_eq!(
        db.transaction(|tx| classify_obligation_tx(tx, &rearmed, None, Some("work-a")))
            .unwrap(),
        TaskResultObligation::Recoverable,
    );
}

#[test]
fn unrelated_aborted_or_dropped_sources_are_not_recoverable_local_stops() {
    for (code, status) in [
        ("provider_dispatch_rejected", QueueEntryStatus::Dropped),
        ("other_failure", QueueEntryStatus::Aborted),
    ] {
        let (_dir, db, record, mut command, execution) = fixture();
        command
            .turn_record
            .as_mut()
            .unwrap()
            .runtime_failure
            .as_mut()
            .unwrap()
            .code = code.into();
        let QueueMutation::Upsert(queue) = &mut command.mutation else {
            unreachable!()
        };
        queue.status = status;
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
        let current = latest(&db, &record);
        assert!(current.recovery_paused_at.is_none());
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &current, None, Some("work-a")))
                .unwrap(),
            TaskResultObligation::InvalidOrStale,
        );
    }
}

#[test]
fn local_dispatch_pause_does_not_bypass_task_or_canonical_attachment_fences() {
    for corruption in ["task_fence", "missing_attachment"] {
        let (_dir, db, record, command, execution) = fixture();
        db.transitions()
            .commit_queue_terminal(&command, &execution, None, &[], None)
            .unwrap();
        assert_parked(&db, &record);
        db.transaction(|tx| {
            if corruption == "task_fence" {
                tx.execute(
                    "UPDATE tasks
                     SET payload_json = json_set(payload_json, '$.detail.rejoin_generation', 2)
                     WHERE task_id = ?1",
                    [&record.task_id],
                )?;
            } else {
                tx.execute(
                    "UPDATE turn_records
                     SET payload_json = json_remove(payload_json, '$.idle_input_batch')
                     WHERE turn_id = 'rejected-turn'",
                    [],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let current = latest(&db, &record);
        assert_eq!(
            db.transaction(|tx| classify_obligation_tx(tx, &current, None, Some("work-a")))
                .unwrap(),
            TaskResultObligation::InvalidOrStale,
            "{corruption}",
        );
    }
}
