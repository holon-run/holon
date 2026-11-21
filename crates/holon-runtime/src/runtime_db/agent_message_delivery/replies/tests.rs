use super::*;
use crate::runtime_db::RuntimeDb;

fn fixture(deadline: Option<DateTime<Utc>>) -> (tempfile::TempDir, RuntimeDb, TaskRecord) {
    let dir = tempfile::tempdir().unwrap();
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )
    .unwrap();
    let task = TaskRecord {
        id: "wait-task".into(),
        agent_id: "caller".into(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Queued,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: None,
        recovery: Some(TaskRecoverySpec::AgentMessageWait {
            summary: "wait".into(),
            message: "request".into(),
            target_agent_id: "peer".into(),
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_id: Some("request".into()),
            after_delivery_rowid: Some(0),
            reply_deadline_at: deadline,
            requester_incarnation: Some(1),
        }),
    };
    db.tasks().upsert(&task).unwrap();
    (dir, db, task)
}

fn explicit(db: &RuntimeDb, accepted_at: DateTime<Utc>) {
    let caller = crate::types::AgentMessageCallerContext {
        caller_principal: "agent:peer".into(),
        caller_agent_id: Some("peer".into()),
        principal_kind: AgentMessagePrincipalKind::PeerAgent,
        route: "agent_message".into(),
        origin: MessageOrigin::System {
            subsystem: "agent_message".into(),
        },
        authority_class: AuthorityClass::RuntimeInstruction,
        delivery_surface: MessageDeliverySurface::RuntimeSystem,
        admission_context: AdmissionContext::RuntimeOwned,
        current_turn_id: None,
        current_task_id: None,
        current_work_item_id: None,
        inherited_routing_context: None,
        routing_activation_id: None,
    };
    let mut record = crate::runtime::AgentMessageDeliveryService::prepare(
        crate::types::AgentMessageSendRequest {
            target_agent_id: "caller".into(),
            content: MessageBody::Text {
                text: "answer".into(),
            },
            client_idempotency_key: "explicit".into(),
            correlation_id: None,
            causation_id: None,
            requested_priority: None,
            forward: false,
            reply_context_id: None,
        },
        caller,
    )
    .unwrap()
    .record;
    record.outcome = AgentMessageDeliveryOutcome::Accepted;
    record.state = AgentMessageDeliveryState::Queued;
    record.accepted_at = Some(accepted_at);
    db.transaction(|tx| insert_delivery_tx(tx, &record))
        .unwrap();
}

#[test]
fn message_wait_outcome_accepted_before_deadline_beats_delayed_monitor() {
    let deadline = Utc::now() - chrono::Duration::seconds(1);
    let (_dir, db, task) = fixture(Some(deadline));
    explicit(&db, deadline - chrono::Duration::milliseconds(1));
    let outcome = db
        .agent_message_deliveries()
        .resolve_message_wait(&task.id)
        .unwrap()
        .unwrap();
    assert!(matches!(outcome.reply, MessageWaitReply::Explicit { .. }));
    let readers = (0..4)
        .map(|_| {
            let db = db.clone();
            let task_id = task.id.clone();
            std::thread::spawn(move || {
                db.agent_message_deliveries()
                    .resolve_message_wait(&task_id)
                    .unwrap()
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    for reader in readers {
        assert_eq!(reader.join().unwrap(), outcome);
    }
}

#[test]
fn message_wait_outcome_timeout_is_durable_and_late_explicit_cannot_replace_it() {
    let (_dir, db, task) = fixture(Some(Utc::now() - chrono::Duration::seconds(1)));
    let outcome = db
        .agent_message_deliveries()
        .resolve_message_wait(&task.id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome.reply,
        MessageWaitReply::ReplyTimeout { .. }
    ));
    explicit(&db, Utc::now());
    assert_eq!(
        db.agent_message_deliveries()
            .resolve_message_wait(&task.id)
            .unwrap(),
        Some(outcome)
    );
    assert_eq!(
        db.tasks().latest(&task.id).unwrap().unwrap().status,
        TaskStatus::Queued,
        "outcome arbitration alone must not stop the target or bypass terminal/result settlement"
    );
}

#[test]
fn message_wait_outcome_transaction_rollback_retries_without_losing_responsibility() {
    let (_dir, db, task) = fixture(Some(Utc::now() - chrono::Duration::seconds(1)));
    let failed: Result<()> = db.transaction(|tx| {
        assert!(resolve_message_wait_tx(tx, &task.id)?.is_some());
        anyhow::bail!("fixture fails before commit");
    });
    assert!(failed.is_err());
    assert!(db
        .with_read(|connection| Ok(connection.query_row(
            "SELECT count(*) = 0 FROM agent_message_wait_outcomes",
            [],
            |row| row.get::<_, bool>(0),
        )?))
        .unwrap());
    assert!(matches!(
        db.agent_message_deliveries()
            .resolve_message_wait(&task.id)
            .unwrap()
            .unwrap()
            .reply,
        MessageWaitReply::ReplyTimeout { .. }
    ));
}

#[test]
fn message_wait_outcome_legacy_missing_deadline_stays_unlimited_and_immutable() {
    let (_dir, db, task) = fixture(None);
    let mut historical = serde_json::to_value(&task).unwrap();
    historical["recovery"]
        .as_object_mut()
        .unwrap()
        .remove("reply_deadline_at");
    historical["recovery"]
        .as_object_mut()
        .unwrap()
        .remove("requester_incarnation");
    let task: TaskRecord = serde_json::from_value(historical).unwrap();
    let mut changed = task.clone();
    if let Some(TaskRecoverySpec::AgentMessageWait {
        reply_deadline_at, ..
    }) = changed.recovery.as_mut()
    {
        *reply_deadline_at = Some(Utc::now());
    }
    assert!(crate::runtime_db::repositories::task_transition(&task, &changed).is_err());
    assert!(db
        .agent_message_deliveries()
        .resolve_message_wait(&task.id)
        .unwrap()
        .is_none());
}
