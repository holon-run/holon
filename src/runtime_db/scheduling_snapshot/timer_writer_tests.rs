//! Real original Timer/Queue transactions; no source or domain late grant.
use super::*;
use crate::runtime_db::{
    input_mutation_receipts::InputMutationResult,
    transitions::{
        AgentStateMutation, InputMutationExecution, PreparedInputTransition,
        PreparedQueueTransition, PreparedTimerTransition, QueueMutation, QueueOperation,
        QueueTransitionCommand, TimerMutationOutcome, TimerWakeClaim, TransitionCommit,
        TransitionFaultPoint,
    },
    TimerCancel, TimerFire,
};
fn execute(
    db: &RuntimeDb,
    p: &PreparedInputTransition,
    budget: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    db.transitions_in_scope(&scope())
        .commit_prepared_transition_with_domain_requests(
            p,
            &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 16 * 1024 * 1024,
            },
            budget,
            &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 16 * 1024 * 1024,
            },
            &request(),
        )
}
fn freeze(nonce: &str, command: PreparedTimerTransition) -> Result<PreparedInputTransition> {
    PreparedInputTransition::timer(scope(), nonce.into(), at(), command)
}
fn created(id: &str) -> TimerRecord {
    let mut value = timer(id, TimerStatus::Active);
    value.fire_count = 0;
    value.last_fired_at = None;
    value
}
fn state(db: &RuntimeDb) -> Result<AgentState> {
    db.transaction(|tx| {
        crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope().agent_id)
            .scope_facts()?
            .state
            .context("fixture State")
    })
}
fn fire(db: &RuntimeDb, id: &str, message_id: &str) -> Result<TimerFire> {
    let expected = db.timers().latest(id)?.context("fixture Timer")?;
    let mut record = expected.clone();
    record.fire_count += 1;
    record.last_fired_at = Some(at() + chrono::Duration::seconds(record.fire_count as i64));
    let mut message = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "actual original timer wake".into(),
        },
    );
    message.id = message_id.into();
    message.created_at = at();
    message.metadata =
        Some(serde_json::json!({"timer_id":id,"timer_fire_count":record.fire_count}));
    let expected_state = state(db)?;
    let mut record_state = expected_state.clone();
    record_state.pending += 1;
    record_state.total_message_count += 1;
    let queue = QueueEntryRecord {
        message_id: message.id.clone(),
        agent_id: scope().agent_id,
        priority: message.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: message.created_at,
        updated_at: at(),
    };
    Ok(TimerFire {
        expected,
        record,
        message,
        queue_entry: queue,
        agent_state: (expected_state, record_state),
    })
}
fn rows(db: &RuntimeDb) -> Result<Vec<(String, i64)>> {
    db.transaction(|tx| {
        [
            "timers",
            "timer_wakes",
            "messages",
            "queue_entries",
            "audit_events",
            "input_mutation_receipts",
            "scheduling_domain_current",
            "scheduling_domain_records",
            "work_domain_current",
            "work_domain_records",
            "task_domain_current",
            "task_message_order_headers",
            "input_evidence_current",
            "runtime_index_outbox",
        ]
        .iter()
        .map(|table| {
            Ok((
                table.to_string(),
                tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?,
            ))
        })
        .collect()
    })
}
fn counts(budget: &InputEvidenceBudgetLedger) -> u64 {
    budget.report().unwrap().observed_bytes
}
#[test]
fn timer_create_exact_receipt_replay_keeps_original_ack_after_later_fire() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    let timer = created("created");
    let p = freeze(
        "create",
        PreparedTimerTransition::create(timer.clone(), None),
    )?;
    let InputMutationExecution::Committed { value, receipt } = execute(&db, &p, &budget)? else {
        panic!("first ACK")
    };
    assert_eq!(
        value.effects.scheduling_write_keys,
        BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "created")])
    );
    assert!(
        value
            .effects
            .scheduling_delta
            .as_ref()
            .unwrap()
            .coverage
            .active_timers_complete
    );
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::Timer {outcome} if matches!(outcome.as_ref(),TimerMutationOutcome::Created {record} if record==&timer))
    );
    drop((value, receipt));
    let command = fire(&db, "created", "created-wake")?;
    drop(execute(
        &db,
        &freeze("fire", PreparedTimerTransition::fire(command, None, None))?,
        &budget,
    )?);
    let before = rows(&db)?;
    let InputMutationExecution::Replayed { receipt, .. } = execute(&db, &p, &budget)? else {
        panic!("exact replay")
    };
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::Timer {outcome} if matches!(outcome.as_ref(),TimerMutationOutcome::Created {record} if record.fire_count==0))
    );
    assert_eq!(db.timers().latest("created")?.unwrap().fire_count, 1);
    assert_eq!(rows(&db)?, before);
    let mut changed = timer.clone();
    changed.summary = Some("changed intent".into());
    assert!(execute(
        &db,
        &freeze("create", PreparedTimerTransition::create(changed, None))?,
        &budget
    )
    .is_err());
    drop((receipt, p));
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn timer_new_wake_and_pending_merge_keep_independent_pending_namespace() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("repeat", TimerStatus::Active)).map(|_| ())
    })?;
    let budget = ledger()?;
    let original = fire(&db, "repeat", "original-wake")?;
    let p = freeze(
        "first-fire",
        PreparedTimerTransition::fire(original, None, None),
    )?;
    assert_eq!(p.pending_messages()?.len(), 1);
    let InputMutationExecution::Committed { value, receipt } = execute(&db, &p, &budget)? else {
        panic!("wake ACK")
    };
    assert_eq!(value.effects.messages[0].id, "original-wake");
    assert!(value.effects.messages[0]
        .message_seq
        .is_some_and(|seq| seq > 0));
    assert_eq!(value.effects.scheduling_write_keys.len(), 2);
    assert_eq!(receipt.outcome.scopes[0].install_keys.scheduling.len(), 2);
    assert_eq!(state(&db)?.pending, 1);
    drop((p, value, receipt));
    let pending = db.timers().pending_wake("repeat")?.unwrap();
    let command = fire(&db, "repeat", "unused-second-wake")?;
    let before = state(&db)?;
    let merge = freeze(
        "merged-fire",
        PreparedTimerTransition::fire(command, Some(pending.clone()), None),
    )?;
    assert!(merge.pending_messages()?.is_empty());
    assert!(!merge.dispatch_memory_candidate());
    let InputMutationExecution::Committed { value, receipt } = execute(&db, &merge, &budget)?
    else {
        panic!("merge ACK")
    };
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::Timer {outcome} if matches!(outcome.as_ref(),TimerMutationOutcome::Fired {result} if result.advanced&&!result.wake_created&&result.message.is_none()))
    );
    assert_eq!(
        value.effects.scheduling_write_keys,
        BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "repeat")])
    );
    let capture = value.effects.scheduling_delta.as_ref().unwrap();
    assert!(capture.coverage.pending_wakes_complete);
    assert_eq!(capture.coverage.pending_wakes.len(), 1);
    assert_eq!(db.timers().pending_wake("repeat")?.unwrap(), pending);
    assert_eq!(state(&db)?, before);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r
                .get::<_, usize>(0))?,
        1
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM dispatch_memory_checkpoints",
            [],
            |r| r.get::<_, usize>(0)
        )?,
        0
    );
    drop((merge, value, receipt));
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn timer_cancel_changes_exact_pending_wake_and_queue_in_original_tx() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("cancel", TimerStatus::Active))?;
        wake(tx, "cancel", "cancel-wake", 1)
    })?;
    let budget = ledger()?;
    let expected = db.timers().latest("cancel")?.unwrap();
    let mut record = expected.clone();
    record.status = TimerStatus::Cancelled;
    record.next_fire_at = None;
    let pending = db.timers().pending_wake("cancel")?.unwrap();
    let expected_state = state(&db)?;
    let mut final_state = expected_state.clone();
    final_state.pending = 0;
    let p = freeze(
        "cancel",
        PreparedTimerTransition::cancel(
            TimerCancel {
                expected,
                record,
                agent_state: Some((expected_state, final_state)),
            },
            Some(pending),
            None,
        ),
    )?;
    let InputMutationExecution::Committed { value, receipt } = execute(&db, &p, &budget)? else {
        panic!("cancel ACK")
    };
    assert!(
        matches!(receipt.outcome.result,InputMutationResult::Timer {ref outcome} if matches!(outcome.as_ref(),TimerMutationOutcome::Cancelled {result} if result.cancelled&&result.dropped_message_id.as_deref()==Some("cancel-wake")))
    );
    assert_eq!(value.effects.scheduling_write_keys.len(), 2);
    assert!(value
        .effects
        .scheduling_delta
        .as_ref()
        .unwrap()
        .coverage
        .pending_wakes
        .is_empty());
    let status: String = db.connection()?.query_row(
        "SELECT status FROM queue_entries WHERE message_id='cancel-wake'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(status, "dropped");
    assert_eq!(state(&db)?.pending, 0);
    drop((value, receipt, p));
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn timer_pending_branch_restore_is_conflict_and_no_new_ticket_or_canonical_effect() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("restore", TimerStatus::Active)).map(|_| ())
    })?;
    let budget = ledger()?;
    let command = fire(&db, "restore", "new-wake")?;
    let p = freeze(
        "frozen-none",
        PreparedTimerTransition::fire(command, None, None),
    )?;
    // Explicit conflicting restore after freeze: ordinary coordinated owner cannot race.
    db.transaction(|tx| wake(tx, "restore", "restored-pending", 1))?;
    let before = rows(&db)?;
    let before_state = state(&db)?;
    let error = execute(&db, &p, &budget).unwrap_err();
    assert!(format!("{error:#}").contains("pending branch changed"));
    assert_eq!(rows(&db)?, before);
    assert_eq!(state(&db)?, before_state);
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn timer_whole_receipt_and_scheduling_fault_rollback_all_original_writes() -> Result<()> {
    for receipt_fault in [false, true] {
        let (_home, db) = legacy(true, |tx| {
            repositories::upsert_timer_tx(tx, &timer("fault", TimerStatus::Active)).map(|_| ())
        })?;
        let budget = ledger()?;
        if !receipt_fault {
            db.connection()?.execute_batch("CREATE TRIGGER fail_scheduling BEFORE INSERT ON scheduling_domain_current BEGIN SELECT RAISE(ABORT,'injected final coverage'); END;")?;
        }
        let p = freeze(
            "fault",
            PreparedTimerTransition::fire(
                fire(&db, "fault", "fault-wake")?,
                None,
                receipt_fault.then_some(TransitionFaultPoint::BeforeCommit),
            ),
        )?;
        let before = rows(&db)?;
        let before_state = state(&db)?;
        let before_timer = db.timers().latest("fault")?;
        assert!(execute(&db, &p, &budget).is_err());
        assert_eq!(rows(&db)?, before);
        assert_eq!(state(&db)?, before_state);
        assert_eq!(db.timers().latest("fault")?, before_timer);
        assert_eq!(counts(&budget), 0);
    }
    Ok(())
}
#[test]
fn timer_stale_noop_ignores_unused_new_wake_collision_but_does_not_ack_later_mutation() -> Result<()>
{
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("stale", TimerStatus::Active))?;
        wake(tx, "stale", "already-pending", 1)
    })?;
    let budget = ledger()?;
    let command = fire(&db, "stale", "already-pending")?;
    let first = freeze(
        "advance",
        PreparedTimerTransition::fire(command.clone(), db.timers().pending_wake("stale")?, None),
    )?;
    drop(execute(&db, &first, &budget)?);
    // The old expected Timer no longer matches; the unused frozen None branch
    // and its colliding Message cannot turn original stale success into failure.
    let stale = freeze(
        "stale-noop",
        PreparedTimerTransition::fire(command, None, None),
    )?;
    let InputMutationExecution::Committed { value, receipt } = execute(&db, &stale, &budget)?
    else {
        panic!("noop receipt")
    };
    assert!(!value.applied);
    assert!(value.effects.scheduling_write_keys.is_empty());
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::Timer {outcome} if matches!(outcome.as_ref(),TimerMutationOutcome::Fired {result} if !result.advanced))
    );
    drop((value, receipt));
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn timer_new_ordinary_command_rejects_deleting_identity() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    db.transaction(|tx| {
        let mut id = identity();
        id.status = crate::types::AgentRegistryStatus::Deleting;
        tx.execute(
            "UPDATE agent_identities SET status='deleting',payload_json=?1 WHERE agent_id=?2",
            params![serde_json::to_string(&id)?, scope().agent_id],
        )?;
        Ok(())
    })?;
    let before = rows(&db)?;
    assert!(execute(
        &db,
        &freeze(
            "deleting-create",
            PreparedTimerTransition::create(created("forbidden"), None)
        )?,
        &budget
    )
    .is_err());
    assert_eq!(rows(&db)?, before);
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn queue_claim_incorporates_only_original_wake_with_completed_timer_owner() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("completed", TimerStatus::Completed))?;
        wake(tx, "completed", "incorporate", 1)
    })?;
    let budget = ledger()?;
    let expected = state(&db)?;
    let mut next = expected.clone();
    next.pending = 0;
    next.current_run_id = Some("original-queue-claim".into());
    next.status = crate::types::AgentStatus::AwakeRunning;
    let mut queue = PreparedQueueTransition::new(QueueTransitionCommand {
        agent_id: scope().agent_id,
        operation: QueueOperation::Claim,
        mutation: QueueMutation::Consume(QueueEntryRecord {
            message_id: "incorporate".into(),
            agent_id: scope().agent_id,
            priority: Priority::Next,
            status: QueueEntryStatus::Dequeued,
            created_at: at(),
            updated_at: at(),
        }),
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(expected)),
            record: Box::new(next),
        }),
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: None,
        audit_events: Vec::new(),
        notify_scheduler: false,
        fault: None,
        brief_evidence: Vec::new(),
    });
    queue.timer_wake_claim = Some(TimerWakeClaim {
        timer_id: "completed".into(),
        message_id: "incorporate".into(),
        fire_count: 1,
    });
    let prepared = PreparedInputTransition::queue(scope(), "actual-claim".into(), at(), queue)?;
    let InputMutationExecution::Committed { value, receipt } = execute(&db, &prepared, &budget)?
    else {
        panic!("claim ACK")
    };
    assert!(value.applied);
    assert_eq!(
        value.effects.scheduling_write_keys,
        BTreeSet::from([SchedulingKey::wake("completed", "incorporate")])
    );
    assert!(value
        .effects
        .scheduling_delta
        .as_ref()
        .unwrap()
        .coverage
        .pending_wakes
        .is_empty());
    assert_eq!(
        db.timers().latest("completed")?.unwrap().status,
        TimerStatus::Completed
    );
    let tuple: (String, String, String) = db.connection()?.query_row(
        "SELECT status,updated_at,incorporated_at FROM timer_wakes WHERE message_id='incorporate'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(tuple.0, "incorporated");
    assert_eq!(tuple.1, tuple.2);
    assert_eq!(receipt.outcome.scopes[0].install_keys.scheduling.len(), 1);
    drop((value, receipt));
    let before = rows(&db)?;
    assert!(matches!(
        execute(&db, &prepared, &budget)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(rows(&db)?, before);
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn timer_writer_observation_budgets_malformed_normalized_address_before_decode() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("observe", TimerStatus::Active))?;
        wake(tx, "observe", "observed-wake", 1)
    })?;
    let budget = ledger()?;
    // Explicit corrupt restore of a normalized tuple; no plain writer can create it.
    db.connection()?.execute(
        "UPDATE timer_wakes SET updated_at=?1 WHERE timer_id='observe'",
        ["x".repeat(65536)],
    )?;
    let mut small = request();
    small.byte_limit = 4096;
    let error = db
        .timers()
        .pending_wake_for_writer(&scope(), "observe", &small, &budget)
        .unwrap_err();
    assert!(format!("{error:#}").to_lowercase().contains("resource"));
    assert_eq!(counts(&budget), 0);
    db.connection()?.execute(
        "UPDATE timer_wakes SET updated_at=?1 WHERE timer_id='observe'",
        [repositories::timestamp(at())],
    )?;
    let observed = db
        .timers()
        .pending_wake_for_writer(&scope(), "observe", &request(), &budget)?;
    assert!(observed.record.is_some());
    assert!(counts(&budget) > 0);
    drop(observed);
    assert_eq!(counts(&budget), 0);
    Ok(())
}
#[test]
fn timer_fire_keeps_actual_wait_source_metadata_and_all_domain_acks_in_one_tx() -> Result<()> {
    use crate::runtime::resident_work::{WorkFamily, WorkKey};
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("wait-owner", TimerStatus::Active)).map(|_| ())
    })?;
    let budget = ledger()?;
    let mut command = fire(&db, "wait-owner", "qualified-timer-source")?;
    let mut work = crate::types::WorkItemRecord::new(
        &scope().agent_id,
        "original timer owner",
        crate::types::WorkItemState::Open,
    );
    work.id = "timer-work".into();
    work.created_at = at();
    work.updated_at = at();
    command.message.work_item_id = Some(work.id.clone());
    let wait: crate::types::WaitConditionRecord = serde_json::from_value(serde_json::json!({
        "id":"timer-wait","agent_id":scope().agent_id,"work_item_id":work.id,
        "status":"active","kind":"timer","source":"WaitFor","subject_ref":"wait-owner",
        "waiting_for":"original timer wake","wake_sources":[{"kind":"timer","wake_at":command.expected.next_fire_at.unwrap()}],
        "created_at":at(),"updated_at":at(),
    }))?;
    let work_request = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    };
    db.transaction(|tx| {
        let keys = BTreeSet::from([
            WorkKey::new(WorkFamily::Work, &work.id),
            WorkKey::new(WorkFamily::Wait, &wait.id),
        ]);
        let proof = crate::runtime_db::work_snapshot::prepare_work_write_tx(tx, &scope(), &keys)?;
        repositories::insert_new_work_item_tx(tx, &work)?;
        repositories::upsert_wait_condition_with_writes_tx(tx, &wait)?;
        crate::runtime_db::work_snapshot::register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &command.agent_state.0,
            &work_request,
        )?;
        Ok(())
    })?;
    let task_request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    };
    drop(db.transaction(|tx| {
        crate::runtime_db::task_snapshot::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &scope(),
            &task_request,
            &budget,
        )?
        .require_current()
    })?);
    let (work_base, task_base): (u64, u64) = db.transaction(|tx| {
        Ok((
            tx.query_row(
                "SELECT revision FROM work_domain_current WHERE agent_id=?1",
                [scope().agent_id],
                |r| r.get(0),
            )?,
            tx.query_row(
                "SELECT revision FROM task_domain_current WHERE agent_id=?1",
                [scope().agent_id],
                |r| r.get(0),
            )?,
        ))
    })?;
    let p = freeze(
        "actual-wait-fire",
        PreparedTimerTransition::fire(command, None, None),
    )?;
    let InputMutationExecution::Committed { value, receipt } = execute(&db, &p, &budget)? else {
        panic!("whole fire ACK")
    };
    assert!(value
        .effects
        .wait_condition_writes
        .wait_ids
        .contains("timer-wait"));
    assert_eq!(
        value.effects.work_delta.as_ref().unwrap().revision,
        work_base + 1
    );
    assert_eq!(
        value.effects.task_delta.as_ref().unwrap().revision,
        task_base + 1
    );
    assert!(!value.effects.audit_events.is_empty());
    assert_eq!(
        receipt.outcome.scopes[0].audit_events,
        value.effects.audit_events
    );
    assert_eq!(value.effects.messages[0].id, "qualified-timer-source");
    let source_headers:usize=db.connection()?.query_row("SELECT COUNT(*) FROM task_message_order_headers WHERE agent_id=?1 AND evidence_id='qualified-timer-source'",[scope().agent_id],|r|r.get(0))?;
    assert_eq!(source_headers, 1);
    let (status, pending): (String, usize) = db.transaction(|tx| {
        Ok((
            tx.query_row(
                "SELECT status FROM wait_conditions WHERE wait_condition_id='timer-wait'",
                [],
                |r| r.get(0),
            )?,
            tx.query_row(
                "SELECT COUNT(*) FROM timer_wakes WHERE status='pending'",
                [],
                |r| r.get(0),
            )?,
        ))
    })?;
    assert_eq!(status, "triggered");
    assert_eq!(pending, 1);
    drop((value, receipt));
    assert_eq!(counts(&budget), 0);
    Ok(())
}
