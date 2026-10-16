use std::collections::BTreeSet;

use super::*;
use crate::domain::execution_protocol::{ExecutionAttempt, ExecutionAttemptState};
use crate::types::{
    AdmissionContext, AgentMessageDeliveryRecord, AuthorityClass, MessageBody,
    MessageDeliverySurface, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
    QueueEntryStatus, TaskKind, TaskRecoverySpec, TaskStatus, TurnRecord,
};

fn live_result(
    db: &RuntimeDb,
    index: usize,
    kind: TaskKind,
    parent: &str,
    owner: Option<&str>,
) -> TaskResultSettlementRecord {
    let now = pending_record(index).created_at;
    let task = TaskRecord {
        id: format!("live-task-{index}"),
        agent_id: "agent-a".into(),
        kind,
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        parent_message_id: Some(format!("live-result-{index}")),
        work_item_id: owner.map(str::to_owned),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": format!("live-task-{index}"),
            "rejoin_generation": 1,
            "parent_turn_id": parent,
        })),
        recovery: None,
    };
    let mut message = MessageEnvelope::new(
        &task.agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "result body".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.id = task.parent_message_id.clone().unwrap();
    message.created_at = now;
    message.work_item_id = task.work_item_id.clone();
    message.task_id = Some(task.id.clone());
    // Envelope turn identity is not the durable task's parent fence.
    message.turn_id = Some("synthetic-result-turn".into());
    message.metadata = Some(serde_json::json!({
        "task_id": task.id, "task_kind": task.kind, "task_status": task.status,
        "task_detail": task.detail,
    }));
    db.tasks().upsert(&task).unwrap();
    db.messages().upsert(&message).unwrap();
    queue(db, &message.id, QueueEntryStatus::Queued);
    db.task_result_settlements()
        .ensure_pending(&task, &message, now)
        .unwrap()
        .unwrap()
}

fn queue(db: &RuntimeDb, message_id: &str, status: QueueEntryStatus) {
    db.queue_entries()
        .upsert(&QueueEntryRecord {
            message_id: message_id.into(),
            agent_id: "agent-a".into(),
            priority: Priority::Next,
            status,
            created_at: pending_record(0).created_at,
            updated_at: Utc::now(),
        })
        .unwrap();
}

fn rewrite_task(db: &RuntimeDb, task: &TaskRecord) {
    db.transaction(|tx| {
        tx.execute(
            "UPDATE tasks SET payload_json = ?2 WHERE task_id = ?1",
            params![task.id, serde_json::to_string(task)?],
        )?;
        Ok(())
    })
    .unwrap();
}

fn rewrite_delivery(db: &RuntimeDb, delivery: &AgentMessageDeliveryRecord) {
    db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_message_deliveries SET outcome = ?2, state = ?3, payload_json = ?4
             WHERE delivery_id = ?1",
            params![
                delivery.delivery_id,
                serde_json::to_value(delivery.outcome)?.as_str().unwrap(),
                serde_json::to_value(delivery.state)?.as_str().unwrap(),
                serde_json::to_string(delivery)?,
            ],
        )?;
        Ok(())
    })
    .unwrap();
}

// Deliberately corrupt persisted evidence only for negative fixtures.
fn rewrite_turn(db: &RuntimeDb, turn: &TurnRecord) {
    db.transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE turn_records SET payload_json = ?3 WHERE agent_id = ?1 AND turn_id = ?2",
                params![turn.agent_id, turn.turn_id, serde_json::to_string(turn)?],
            )?,
            1,
        );
        Ok(())
    })
    .unwrap();
}

fn delivery(
    id: &str,
    sender: &str,
    recipient: &str,
    message_id: &str,
) -> AgentMessageDeliveryRecord {
    let now = pending_record(0).created_at;
    serde_json::from_value(serde_json::json!({
        "delivery_id": id, "target_agent_id": recipient, "message_id": message_id,
        "routing_context": {
            "message_id": message_id, "sender_agent_id": sender,
            "recipient_agent_id": recipient,
        },
        "idempotency_scope": id, "idempotency_key_digest": id, "request_digest": id,
        "caller": {
            "caller_principal": sender, "caller_agent_id": sender,
            "principal_kind": "peer_agent", "route": "agent_message",
            "origin": {"kind": "task", "task_id": "sender-task"},
            "authority_class": "runtime_instruction",
            "delivery_surface": "runtime_system", "admission_context": "runtime_owned",
        },
        "outcome": "accepted", "state": "queued", "state_version": 0,
        "admission_evidence": {
            "principal_kind": "peer_agent", "principal_id": sender, "route": "agent_message",
        },
        "retryable": false, "accepted_at": now, "created_at": now, "updated_at": now,
    }))
    .unwrap()
}

fn reply_source(db: &RuntimeDb) -> i64 {
    let request = delivery("live-request-delivery", "agent-a", "peer-b", "live-request");
    let response = delivery("live-response-delivery", "peer-b", "agent-a", "live-reply");
    let boundary = db
        .transaction(|tx| {
            crate::runtime_db::agent_message_delivery::insert_delivery_tx(tx, &request)?;
            let boundary = tx.last_insert_rowid();
            crate::runtime_db::agent_message_delivery::insert_delivery_tx(tx, &response)?;
            Ok(boundary)
        })
        .unwrap();
    let mut reply = MessageEnvelope::new(
        "agent-a",
        MessageKind::InternalFollowup,
        MessageOrigin::Task {
            task_id: "sender-task".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "original reply body".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    reply.id = "live-reply".into();
    reply.created_at = pending_record(0).created_at;
    reply.routing_context = response.routing_context;
    reply.metadata = Some(serde_json::json!({
        "agent_message_delivery": {"delivery_id": "live-response-delivery"},
    }));
    db.messages().upsert(&reply).unwrap();
    queue(db, &reply.id, QueueEntryStatus::Queued);
    boundary
}

fn live_reply(
    db: &RuntimeDb,
    index: usize,
    parent: &str,
    owner: Option<&str>,
    boundary: i64,
) -> TaskResultSettlementRecord {
    let record = live_result(db, index, TaskKind::AgentMessageWait, parent, owner);
    let mut task = db.tasks().latest(&record.task_id).unwrap().unwrap();
    task.recovery = Some(TaskRecoverySpec::AgentMessageWait {
        summary: "wait for peer".into(),
        message: "request".into(),
        target_agent_id: "peer-b".into(),
        authority_class: AuthorityClass::RuntimeInstruction,
        delivery_id: Some("live-request-delivery".into()),
        after_delivery_rowid: Some(boundary),
    });
    let detail = task.detail.as_mut().unwrap();
    detail["message_id"] = serde_json::json!("live-reply");
    detail["response_delivery_id"] = serde_json::json!("live-response-delivery");
    detail["reply_content_source"] = serde_json::json!("original_message");
    detail["message_wait_after_delivery_rowid"] = serde_json::json!(boundary);
    rewrite_task(db, &task);
    let mut message = db
        .messages()
        .by_id(None, &record.message_id)
        .unwrap()
        .unwrap();
    message.metadata.as_mut().unwrap()["task_detail"] = task.detail.unwrap();
    db.messages().upsert(&message).unwrap();
    record
}

fn admit(db: &RuntimeDb, records: &[TaskResultSettlementRecord], activation: &str) -> bool {
    let selected = records
        .iter()
        .map(|record| record.result_identity.clone())
        .collect();
    db.transaction(|tx| {
        admit_live_task_results_tx(
            tx,
            "agent-a",
            "live-parent",
            activation,
            &selected,
            Utc::now(),
            Utc::now(),
        )
    })
    .unwrap()
}

#[test]
fn interrupted_result_selection_requires_its_own_canonical_snapshot() {
    for invalid in [
        "valid",
        "pending",
        "open-attempt",
        "foreign-binding",
        "missing-input",
        "missing-live-snapshot",
        "wrong-identity",
        "wrong-idle-activation",
        "anchor-not-interrupted",
        "revoked-task",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        prepare_recovery(&db, &[record.clone()], false);
        let mut turn = interrupted_turn("live-parent", &[record.clone()], false);
        turn.idle_input_batch = Some(crate::types::IdleInputBatchRecord {
            anchor_message_id: "live-anchor".into(),
            activation_id: Some("live-activation".into()),
            sender_agent_id: "peer".into(),
            cutoff: record.created_at - chrono::Duration::nanoseconds(1),
            max_items: 8,
            max_bytes: 24 * 1024,
            rendered_bytes: 0,
            items: vec![crate::types::IdleInputBatchItem {
                message_id: "live-anchor".into(),
                source_ref: "message:live-anchor".into(),
                rendered: String::new(),
            }],
        });
        queue(&db, "live-anchor", QueueEntryStatus::Interrupted);
        match invalid {
            "valid" => {}
            "pending" => {
                let mut pending = db
                    .task_result_settlements()
                    .latest_for_message(&record.message_id)
                    .unwrap()
                    .unwrap();
                pending.state = TaskResultSettlementState::PersistedPending;
                pending.activation_id = None;
                pending.admitted_at = None;
                db.transaction(|tx| update_tx(tx, &pending)).unwrap();
            }
            "open-attempt" => {
                recovery_attempt(
                    &db,
                    "recovery-a",
                    "recovery-turn-a",
                    ExecutionAttemptState::Interrupted,
                    Some("live-activation"),
                );
                recovery_attempt(
                    &db,
                    "live-activation",
                    "live-parent",
                    ExecutionAttemptState::Open,
                    None,
                );
            }
            "foreign-binding" => {
                let mut attempt = recovery_attempt(
                    &db,
                    "live-activation",
                    "live-parent",
                    ExecutionAttemptState::Interrupted,
                    None,
                );
                attempt.binding = crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                    work_item_id: "foreign".into(),
                };
                save_recovery_attempt(&db, &attempt);
            }
            "missing-input" => turn.input_message_ids.retain(|id| id != &record.message_id),
            "missing-live-snapshot" => turn.live_input_batches.clear(),
            "wrong-identity" => {
                turn.live_input_batches[0].result_identities = vec!["foreign-result".into()]
            }
            "wrong-idle-activation" => {
                turn.idle_input_batch.as_mut().unwrap().activation_id = Some("foreign".into())
            }
            "anchor-not-interrupted" => queue(&db, "live-anchor", QueueEntryStatus::Processed),
            "revoked-task" => {
                let mut task = db.tasks().latest(&record.task_id).unwrap().unwrap();
                task.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
                rewrite_task(&db, &task);
            }
            _ => unreachable!(),
        }
        rewrite_turn(&db, &turn);
        let before = db
            .task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            db.task_result_settlements()
                .interrupted_admitted_result_anchor("agent-a", &record.message_id, Utc::now())
                .unwrap(),
            (invalid == "valid").then(|| "live-anchor".to_owned()),
            "{invalid}",
        );
        assert_eq!(
            db.task_result_settlements()
                .latest_for_message(&record.message_id)
                .unwrap()
                .unwrap(),
            before,
            "selection must not mutate the ledger: {invalid}",
        );
    }
}

fn assert_pending(db: &RuntimeDb, record: &TaskResultSettlementRecord) {
    let actual = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(actual.state, TaskResultSettlementState::PersistedPending);
    assert!(actual.activation_id.is_none());
}

fn recovery_attempt(
    db: &RuntimeDb,
    id: &str,
    turn: &str,
    state: ExecutionAttemptState,
    predecessor: Option<&str>,
) -> ExecutionAttempt {
    let attempt = serde_json::from_value(serde_json::json!({
        "attempt_id": id, "agent_id": "agent-a", "source_message_id": "live-anchor",
        "source": {
            "identity": {"kind": "queue_message", "message_id": "live-anchor"},
            "generation": 1,
        },
        "binding": {"kind": "agent_lifecycle", "agent_id": "agent-a"},
        "provenance": {
            "origin": "operator", "trust": "operator_instruction", "priority": "normal",
        },
        "admitted_fences": {
            "source_revision": 1, "agent_control_revision": 1, "host_registry_revision": 1,
        },
        "state": state, "turn_id": format!("ingress-{turn}"),
        "recovery_of_attempt_id": predecessor,
        "admitted_at": pending_record(0).created_at.to_rfc3339(),
    }))
    .unwrap();
    save_recovery_attempt(db, &attempt);
    attempt
}

fn save_recovery_attempt(db: &RuntimeDb, attempt: &ExecutionAttempt) {
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state, source_identity_json,
               source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, attempt_id) DO UPDATE SET
               lifecycle_state = excluded.lifecycle_state,
               source_identity_json = excluded.source_identity_json,
               source_generation = excluded.source_generation,
               recovery_of_attempt_id = excluded.recovery_of_attempt_id,
               terminal_outcome_id = excluded.terminal_outcome_id,
               payload_json = excluded.payload_json",
            params![
                attempt.agent_id,
                attempt.attempt_id,
                serde_json::to_value(attempt.state)?.as_str().unwrap(),
                serde_json::to_string(&attempt.source.identity)?,
                attempt.source.generation,
                attempt.recovery_of_attempt_id,
                attempt.terminal_outcome_id,
                serde_json::to_string(attempt)?,
            ],
        )?;
        Ok(())
    })
    .unwrap();
}

fn interrupted_attachment(
    db: &RuntimeDb,
    turn_id: &str,
    records: &[TaskResultSettlementRecord],
    has_reply: bool,
) {
    save_interrupted_attachment(db, &interrupted_turn(turn_id, records, has_reply));
}

fn interrupted_turn(
    turn_id: &str,
    records: &[TaskResultSettlementRecord],
    has_reply: bool,
) -> TurnRecord {
    let mut turn = TurnRecord::new("agent-a", turn_id, 1);
    let mut source = MessageEnvelope::new(
        "agent-a",
        MessageKind::InternalFollowup,
        MessageOrigin::Task {
            task_id: "source-task".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: String::new(),
        },
    );
    source.id = "live-anchor".into();
    turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
    turn.input_message_ids = records
        .iter()
        .map(|record| record.message_id.clone())
        .collect();
    if has_reply {
        turn.input_message_ids.push("live-reply".into());
    }
    let mut batch = crate::types::LiveInputBatchRecord {
        activation_id: if turn_id == "recovery-turn-a" {
            "recovery-a"
        } else {
            "live-activation"
        }
        .into(),
        safe_point: crate::types::LiveInputSafePoint::new("live-activation", 2),
        cutoff: Utc::now(),
        max_items: 8,
        max_bytes: 24 * 1024,
        rendered_bytes: 0,
        result_identities: records
            .iter()
            .map(|record| record.result_identity.clone())
            .collect(),
        ordinary_message_ids: Vec::new(),
        items: turn
            .input_message_ids
            .iter()
            .map(|id| crate::types::IdleInputBatchItem {
                message_id: id.clone(),
                source_ref: format!("message:{id}"),
                rendered: String::new(),
            })
            .collect(),
    };
    batch.rendered_bytes = batch.rendered_text().len();
    turn.live_input_batches.push(batch);
    turn.input_message_ids.insert(0, source.id);
    if turn_id == "recovery-turn-a" {
        turn.replay = Some(crate::types::TurnReplayProvenance {
            source_message_id: "live-anchor".into(),
            source_turn_id: "live-parent".into(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: None,
        });
    }
    turn.terminal = Some(
        serde_json::from_value(serde_json::json!({
            "turn_id": turn_id, "turn_index": 1, "kind": "interrupted",
            "completed_at": Utc::now(), "duration_ms": 0,
        }))
        .unwrap(),
    );
    turn
}

fn save_interrupted_attachment(db: &RuntimeDb, turn: &TurnRecord) {
    db.turn_records().upsert(turn).unwrap();
    for id in &turn.input_message_ids {
        queue(db, id, QueueEntryStatus::Interjected);
    }
}

fn prepare_recovery(db: &RuntimeDb, records: &[TaskResultSettlementRecord], has_reply: bool) {
    prepare_recovery_with_turn(
        db,
        records,
        has_reply,
        &interrupted_turn("live-parent", records, has_reply),
    );
}

fn prepare_recovery_with_turn(
    db: &RuntimeDb,
    records: &[TaskResultSettlementRecord],
    has_reply: bool,
    turn: &TurnRecord,
) {
    assert!(admit(db, records, "live-activation"));
    save_interrupted_attachment(db, turn);
    recovery_attempt(
        db,
        "live-activation",
        "live-parent",
        ExecutionAttemptState::Interrupted,
        None,
    );
    recovery_attempt(
        db,
        "recovery-a",
        "recovery-turn-a",
        ExecutionAttemptState::Open,
        Some("live-activation"),
    );
    if has_reply {
        let mut response = db
            .agent_message_deliveries()
            .latest("live-response-delivery")
            .unwrap()
            .unwrap();
        response.state = crate::types::AgentMessageDeliveryState::Consumed;
        response.activation_id = Some("live-activation".into());
        response.turn_id = Some("live-parent".into());
        rewrite_delivery(db, &response);
    }
}

fn recover(
    db: &RuntimeDb,
    predecessor: &str,
    successor: &str,
    records: &[TaskResultSettlementRecord],
) -> bool {
    let selected = records
        .iter()
        .map(|record| record.result_identity.clone())
        .collect();
    db.transaction(|tx| {
        recover_live_task_results_tx(tx, "agent-a", predecessor, successor, &selected, Utc::now())
    })
    .unwrap()
}

fn assert_admitted_to(db: &RuntimeDb, record: &TaskResultSettlementRecord, activation: &str) {
    let actual = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(actual.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(actual.activation_id.as_deref(), Some(activation));
    assert_eq!(actual.parent_turn_id, record.parent_turn_id);
    assert_eq!(actual.rejoin_generation, record.rejoin_generation);
    assert_eq!(actual.work_item_id, record.work_item_id);
    assert!(actual.admitted_at.is_some());
    assert!(actual.disposition.is_none());
    assert!(actual.settled_at.is_none());
}

fn lifecycle_recovery_binding(db: &RuntimeDb, id: &str) {
    let mut attempt = db
        .transaction(|tx| super::super::reply_admission_attempt_tx(tx, "agent-a", id))
        .unwrap()
        .unwrap();
    attempt.binding = crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle {
        agent_id: "agent-a".into(),
    };
    save_recovery_attempt(db, &attempt);
}

fn admit_initial(db: &RuntimeDb, record: &TaskResultSettlementRecord) {
    assert!(db
        .task_result_settlements()
        .admit_message(
            "agent-a",
            &record.message_id,
            "live-activation",
            record.created_at,
        )
        .unwrap()
        .is_some());
}

#[test]
fn recovery_maps_null_or_different_ingress_without_rebinding_attempts_or_tasks() {
    for ingress in [None, Some("unrelated-ingress")] {
        let (_dir, db) = runtime_db();
        let initial = live_result(&db, 0, TaskKind::CommandTask, "older-parent", None);
        let live = live_result(
            &db,
            1,
            TaskKind::ChildAgentTask,
            "live-parent",
            Some("captured-owner"),
        );
        admit_initial(&db, &initial);
        prepare_recovery(&db, &[live.clone()], false);
        let mut attempts = Vec::new();
        for id in ["live-activation", "recovery-a"] {
            let mut attempt = db
                .transaction(|tx| super::super::reply_admission_attempt_tx(tx, "agent-a", id))
                .unwrap()
                .unwrap();
            attempt.turn_id = ingress.map(str::to_owned);
            save_recovery_attempt(&db, &attempt);
            attempts.push(attempt);
        }
        let task_before = db.tasks().latest(&live.task_id).unwrap().unwrap();
        assert!(recover(
            &db,
            "live-activation",
            "recovery-a",
            &[live.clone()]
        ));
        assert_admitted_to(&db, &initial, "recovery-a");
        assert_admitted_to(&db, &live, "recovery-a");
        assert_eq!(
            db.tasks().latest(&live.task_id).unwrap().unwrap(),
            task_before
        );
        for attempt in attempts {
            let actual = db
                .transaction(|tx| {
                    super::super::reply_admission_attempt_tx(tx, "agent-a", &attempt.attempt_id)
                })
                .unwrap()
                .unwrap();
            assert_eq!(actual, attempt);
        }
    }
}

#[test]
fn canonical_live_turn_lookup_uses_durable_activation_and_known_run_not_ingress() {
    for (ingress, run) in [
        (None, None),
        (Some("unrelated-ingress"), None),
        (None, Some("canonical-run")),
        (Some("unrelated-ingress"), Some("canonical-run")),
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let mut turn = interrupted_turn("live-parent", &[record.clone()], false);
        turn.run_id = Some("canonical-run".into());
        prepare_recovery_with_turn(&db, &[record], false, &turn);
        let mut attempt = db
            .transaction(|tx| {
                super::super::reply_admission_attempt_tx(tx, "agent-a", "live-activation")
            })
            .unwrap()
            .unwrap();
        attempt.turn_id = ingress.map(str::to_owned);
        attempt.run_id = run.map(str::to_owned);
        save_recovery_attempt(&db, &attempt);
        assert_eq!(
            db.transaction(|tx| canonical_live_turn_id_tx(tx, &attempt))
                .unwrap(),
            Some("live-parent".into()),
        );
        assert_eq!(
            db.transaction(|tx| super::super::reply_admission_attempt_tx(
                tx,
                "agent-a",
                "live-activation",
            ))
            .unwrap()
            .unwrap(),
            attempt,
        );
    }
}

#[test]
fn canonical_live_turn_lookup_never_uses_unbound_idle_or_text_only_activation_evidence() {
    for evidence in ["idle-batch", "body-key", "ingress-only"] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        prepare_recovery(&db, &[record], false);
        let mut attempt = db
            .transaction(|tx| {
                super::super::reply_admission_attempt_tx(tx, "agent-a", "live-activation")
            })
            .unwrap()
            .unwrap();
        attempt.turn_id = Some("live-parent".into());
        let mut turn = db
            .turn_records()
            .by_id(Some("agent-a"), "live-parent")
            .unwrap()
            .unwrap();
        let batch = turn.live_input_batches.remove(0);
        match evidence {
            "idle-batch" => {
                turn.idle_input_batch = Some(crate::types::IdleInputBatchRecord {
                    anchor_message_id: "live-anchor".into(),
                    activation_id: Some(attempt.attempt_id.clone()),
                    sender_agent_id: "sender".into(),
                    cutoff: batch.cutoff,
                    max_items: batch.max_items,
                    max_bytes: batch.max_bytes,
                    rendered_bytes: batch.rendered_bytes,
                    items: batch.items,
                });
            }
            "body-key" => {
                turn.input_message_ids.push(attempt.attempt_id.clone());
                turn.trigger.as_mut().unwrap().task_id = Some(attempt.attempt_id.clone());
            }
            "ingress-only" => {}
            _ => unreachable!(),
        }
        db.turn_records().upsert(&turn).unwrap();
        assert!(
            db.transaction(|tx| canonical_live_turn_id_tx(tx, &attempt))
                .unwrap()
                .is_none(),
            "{evidence}",
        );
    }
}

fn initial_only_turn(mut turn: TurnRecord, activation_id: &str) -> TurnRecord {
    let batch = turn.live_input_batches.remove(0);
    turn.idle_input_batch = Some(crate::types::IdleInputBatchRecord {
        anchor_message_id: "live-anchor".into(),
        activation_id: Some(activation_id.into()),
        sender_agent_id: "sender".into(),
        cutoff: batch.cutoff,
        max_items: batch.max_items,
        max_bytes: batch.max_bytes,
        rendered_bytes: batch.rendered_bytes,
        items: turn
            .input_message_ids
            .iter()
            .map(|id| crate::types::IdleInputBatchItem {
                message_id: id.clone(),
                source_ref: format!("message:{id}"),
                rendered: String::new(),
            })
            .collect(),
    });
    turn
}

#[test]
fn canonical_live_turn_lookup_accepts_only_unique_bound_initial_batches() {
    for case in [
        "null-ingress",
        "different-ingress",
        "activation",
        "anchor",
        "first-item",
        "source-ref",
        "trigger",
        "source-input",
        "owner",
        "run-missing",
        "run-mismatch",
        "ambiguous-idle",
        "ambiguous-live",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let original = interrupted_turn("live-parent", &[record.clone()], false);
        let mut turn = initial_only_turn(original.clone(), "live-activation");
        turn.run_id = Some("canonical-run".into());
        prepare_recovery_with_turn(&db, &[record], false, &turn);
        let mut attempt = db
            .transaction(|tx| {
                super::super::reply_admission_attempt_tx(tx, "agent-a", "live-activation")
            })
            .unwrap()
            .unwrap();
        attempt.turn_id = (case == "different-ingress").then(|| "ingress-only".into());
        attempt.run_id = Some("canonical-run".into());
        save_recovery_attempt(&db, &attempt);
        let batch = turn.idle_input_batch.as_mut().unwrap();
        match case {
            "null-ingress" | "different-ingress" => {}
            "activation" => batch.activation_id = Some("foreign".into()),
            "anchor" => batch.anchor_message_id = "foreign".into(),
            "first-item" => batch.items[0].message_id = "foreign".into(),
            "source-ref" => batch.items[0].source_ref = "message:foreign".into(),
            "trigger" => turn.trigger.as_mut().unwrap().message_id = Some("foreign".into()),
            "source-input" => turn.input_message_ids.retain(|id| id != "live-anchor"),
            "owner" => turn.owner = Some(crate::types::TurnOwner::Command),
            "run-missing" => turn.run_id = None,
            "run-mismatch" => turn.run_id = Some("other-run".into()),
            "ambiguous-idle" | "ambiguous-live" => {
                let mut duplicate = if case == "ambiguous-live" {
                    original
                } else {
                    turn.clone()
                };
                duplicate.turn_id = "other-canonical-turn".into();
                duplicate.run_id = Some("canonical-run".into());
                db.turn_records().upsert(&duplicate).unwrap();
            }
            _ => unreachable!(),
        }
        if !matches!(case, "null-ingress" | "different-ingress") {
            rewrite_turn(&db, &turn);
        }
        let expected =
            matches!(case, "null-ingress" | "different-ingress").then(|| "live-parent".to_owned());
        assert_eq!(
            db.transaction(|tx| canonical_live_turn_id_tx(tx, &attempt))
                .unwrap(),
            expected,
            "{case}",
        );
    }
}

#[test]
fn recovery_requires_unique_persisted_batch_source_and_owner_evidence() {
    for defect in [
        "missing-batch",
        "activation",
        "trigger",
        "source-input",
        "owner",
        "ambiguous",
        "run-missing",
        "run-mismatch",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        prepare_recovery(&db, &[record.clone()], false);
        let mut turn = db
            .turn_records()
            .by_id(Some("agent-a"), "live-parent")
            .unwrap()
            .unwrap();
        match defect {
            "missing-batch" => turn.live_input_batches.clear(),
            "activation" => turn.live_input_batches[0].activation_id = "foreign".into(),
            "trigger" => turn.trigger.as_mut().unwrap().message_id = Some("foreign".into()),
            "source-input" => turn.input_message_ids.retain(|id| id != "live-anchor"),
            "owner" => turn.owner = Some(crate::types::TurnOwner::Command),
            "run-missing" | "run-mismatch" => {
                let mut attempt = db
                    .transaction(|tx| {
                        super::super::reply_admission_attempt_tx(tx, "agent-a", "live-activation")
                    })
                    .unwrap()
                    .unwrap();
                attempt.run_id = Some("canonical-run".into());
                save_recovery_attempt(&db, &attempt);
                if defect == "run-mismatch" {
                    turn.run_id = Some("other-run".into());
                }
            }
            "ambiguous" => {
                let mut duplicate = turn.clone();
                duplicate.turn_id = "other-canonical-turn".into();
                db.turn_records().upsert(&duplicate).unwrap();
            }
            _ => unreachable!(),
        }
        rewrite_turn(&db, &turn);
        assert!(
            !recover(&db, "live-activation", "recovery-a", &[record.clone()]),
            "{defect}"
        );
        let attempt = db
            .transaction(|tx| {
                super::super::reply_admission_attempt_tx(tx, "agent-a", "live-activation")
            })
            .unwrap()
            .unwrap();
        assert!(
            db.transaction(|tx| canonical_live_turn_id_tx(tx, &attempt))
                .unwrap()
                .is_none(),
            "{defect}",
        );
        assert_admitted_to(&db, &record, "live-activation");
    }
}

#[test]
fn repeated_recovery_requires_the_persisted_canonical_replay_chain() {
    for defect in ["missing-replay", "source-turn", "source-message"] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        prepare_recovery(&db, &[record.clone()], false);
        assert!(recover(
            &db,
            "live-activation",
            "recovery-a",
            &[record.clone()]
        ));
        interrupted_attachment(&db, "recovery-turn-a", &[record.clone()], false);
        recovery_attempt(
            &db,
            "recovery-a",
            "recovery-turn-a",
            ExecutionAttemptState::Interrupted,
            Some("live-activation"),
        );
        recovery_attempt(
            &db,
            "recovery-b",
            "recovery-turn-b",
            ExecutionAttemptState::Open,
            Some("recovery-a"),
        );
        let mut turn = db
            .turn_records()
            .by_id(Some("agent-a"), "recovery-turn-a")
            .unwrap()
            .unwrap();
        match defect {
            "missing-replay" => turn.replay = None,
            "source-turn" => turn.replay.as_mut().unwrap().source_turn_id = "foreign".into(),
            "source-message" => turn.replay.as_mut().unwrap().source_message_id = "foreign".into(),
            _ => unreachable!(),
        }
        rewrite_turn(&db, &turn);
        assert!(
            !recover(&db, "recovery-a", "recovery-b", &[record.clone()]),
            "{defect}"
        );
        assert_admitted_to(&db, &record, "recovery-a");
    }
}

#[test]
fn an_older_p1_turn_without_live_batches_requires_an_exact_replay_edge() {
    for (valid, idle) in [(true, false), (false, false), (true, true), (false, true)] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let mut turn = interrupted_turn("live-parent", &[record.clone()], false);
        let mut older = turn.clone();
        older.turn_id = "older-p1-turn".into();
        older.input_message_ids = vec!["live-anchor".into()];
        if idle {
            older = initial_only_turn(older, "older-p1-activation");
        } else {
            older.live_input_batches.clear();
        }
        db.turn_records().upsert(&older).unwrap();
        recovery_attempt(
            &db,
            "older-p1-activation",
            "older-p1-turn",
            ExecutionAttemptState::Interrupted,
            None,
        );
        turn.replay = Some(crate::types::TurnReplayProvenance {
            source_message_id: "live-anchor".into(),
            source_turn_id: "older-p1-turn".into(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: None,
        });
        prepare_recovery_with_turn(&db, &[record.clone()], false, &turn);
        recovery_attempt(
            &db,
            "live-activation",
            "live-parent",
            ExecutionAttemptState::Interrupted,
            Some("older-p1-activation"),
        );
        if !valid {
            turn.replay.as_mut().unwrap().source_turn_id = "foreign-turn".into();
            rewrite_turn(&db, &turn);
        }
        assert_eq!(
            recover(&db, "live-activation", "recovery-a", &[record.clone()]),
            valid
        );
        assert_admitted_to(
            &db,
            &record,
            if valid {
                "recovery-a"
            } else {
                "live-activation"
            },
        );
    }
}

#[test]
fn batchless_recovery_ancestor_requires_bound_unambiguous_interrupted_evidence() {
    for case in [
        "valid",
        "legacy-no-terminal",
        "missing-replay",
        "source-turn",
        "source-message",
        "terminal",
        "aborted",
        "agent",
        "trigger",
        "source-input",
        "run",
        "owner",
        "cycle",
        "ambiguous-canonical",
        "invalid-canonical",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let mut turn = interrupted_turn("live-parent", &[record.clone()], false);
        let mut older = interrupted_turn("older-p1-turn", &[], false);
        older.live_input_batches.clear();
        older.run_id = Some("older-run".into());
        older.owner = Some(crate::types::TurnOwner::AgentLifecycle {
            agent_id: "agent-a".into(),
        });
        let mut attempt = recovery_attempt(
            &db,
            "older-p1-activation",
            "older-p1-turn",
            ExecutionAttemptState::Interrupted,
            None,
        );
        attempt.run_id = Some("older-run".into());
        turn.replay = Some(crate::types::TurnReplayProvenance {
            source_message_id: "live-anchor".into(),
            source_turn_id: older.turn_id.clone(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: None,
        });
        db.turn_records().upsert(&older).unwrap();
        match case {
            "valid" => {}
            "legacy-no-terminal" => older.terminal = None,
            "missing-replay" => turn.replay = None,
            "source-turn" => {
                turn.replay.as_mut().unwrap().source_turn_id = "foreign-turn".into();
            }
            "source-message" => {
                turn.replay.as_mut().unwrap().source_message_id = "foreign-message".into();
            }
            "terminal" => {
                older.terminal.as_mut().unwrap().kind = crate::types::TurnTerminalKind::Completed;
            }
            "aborted" => {
                older.terminal.as_mut().unwrap().kind = crate::types::TurnTerminalKind::Aborted;
            }
            "agent" => older.agent_id = "foreign-agent".into(),
            "trigger" => {
                older.trigger.as_mut().unwrap().message_id = Some("foreign-message".into());
            }
            "source-input" => older.input_message_ids.clear(),
            "run" => older.run_id = Some("foreign-run".into()),
            "owner" => older.owner = Some(crate::types::TurnOwner::Command),
            "cycle" => attempt.recovery_of_attempt_id = Some("live-activation".into()),
            "ambiguous-canonical" | "invalid-canonical" => {
                let count = if case == "ambiguous-canonical" { 2 } else { 1 };
                for index in 0..count {
                    let mut attached = interrupted_turn("other-canonical", &[], false);
                    attached.turn_id = format!("other-canonical-{index}");
                    attached.run_id = Some(
                        if case == "invalid-canonical" {
                            "foreign-run"
                        } else {
                            "older-run"
                        }
                        .into(),
                    );
                    attached.live_input_batches[0].activation_id = attempt.attempt_id.clone();
                    db.turn_records().upsert(&attached).unwrap();
                }
            }
            _ => unreachable!(),
        }
        // Corrupt payload evidence without changing the persisted input's agent.
        db.transaction(|tx| {
            assert_eq!(
                tx.execute(
                    "UPDATE turn_records SET payload_json = ?2
                     WHERE agent_id = 'agent-a' AND turn_id = ?1",
                    params![older.turn_id, serde_json::to_string(&older)?],
                )?,
                1,
            );
            Ok(())
        })
        .unwrap();
        save_recovery_attempt(&db, &attempt);
        prepare_recovery_with_turn(&db, &[record.clone()], false, &turn);
        recovery_attempt(
            &db,
            "live-activation",
            "live-parent",
            ExecutionAttemptState::Interrupted,
            Some("older-p1-activation"),
        );
        let valid = matches!(case, "valid" | "legacy-no-terminal");
        assert_eq!(
            recover(&db, "live-activation", "recovery-a", &[record.clone()]),
            valid,
            "{case}",
        );
        assert_admitted_to(
            &db,
            &record,
            if valid {
                "recovery-a"
            } else {
                "live-activation"
            },
        );
    }
}

#[test]
fn attached_recovery_ancestors_require_an_interrupted_turn_terminal() {
    for idle in [false, true] {
        for terminal in [
            None,
            Some(crate::types::TurnTerminalKind::Completed),
            Some(crate::types::TurnTerminalKind::Aborted),
        ] {
            let (_dir, db) = runtime_db();
            let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
            let mut turn = interrupted_turn("live-parent", &[record.clone()], false);
            if idle {
                turn = initial_only_turn(turn, "live-activation");
            }
            if let Some(kind) = terminal {
                turn.terminal.as_mut().unwrap().kind = kind;
            } else {
                turn.terminal = None;
            }
            prepare_recovery_with_turn(&db, &[record.clone()], false, &turn);
            assert!(
                !recover(&db, "live-activation", "recovery-a", &[record.clone()]),
                "idle={idle}, terminal={terminal:?}",
            );
            assert_admitted_to(&db, &record, "live-activation");
        }
    }
}

#[test]
fn consumed_reply_witness_uses_persisted_batch_not_attempt_ingress() {
    for reply_queue_status in [QueueEntryStatus::Processed, QueueEntryStatus::Interjected] {
        for case in [
            "null-ingress",
            "different-ingress",
            "anchor-null",
            "anchor-different",
            "missing-batch",
            "unbound-missing-batch",
            "turn-only-missing-batch",
            "activation",
            "owner",
            "trigger",
            "foreign-turn",
            "missing-reply",
            "incomplete-turn",
            "interrupted-turn",
            "aborted-turn",
        ] {
            let (_dir, db) = runtime_db();
            let boundary = reply_source(&db);
            let initial = live_reply(&db, 0, "older-parent", None, boundary);
            let sibling = live_reply(&db, 1, "older-parent", None, boundary);
            let live = live_result(&db, 2, TaskKind::CommandTask, "live-parent", None);
            admit_initial(&db, &initial);
            prepare_recovery(&db, &[live.clone()], false);
            let (mut completed, mut attempt) = completed_reply_source(&db);
            if matches!(case, "null-ingress" | "anchor-null") {
                attempt.turn_id = None;
            }
            match case {
                "anchor-null" | "anchor-different" => {
                    attempt.source_message_id = Some("live-reply".into());
                    attempt.source.identity =
                        crate::domain::execution_protocol::ExecutionSourceIdentity::QueueMessage {
                            message_id: "live-reply".into(),
                        };
                    completed.trigger.as_mut().unwrap().message_id = Some("live-reply".into());
                    completed.input_message_ids = vec!["live-reply".into()];
                    completed.live_input_batches.clear();
                }
                "missing-batch" | "unbound-missing-batch" | "turn-only-missing-batch" => {
                    completed.live_input_batches.clear()
                }
                "activation" => completed.live_input_batches[0].activation_id = "foreign".into(),
                "owner" => {
                    completed.current_work_item_id = Some("other-owner".into());
                    attempt.binding =
                        crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                            work_item_id: "other-owner".into(),
                        };
                }
                "trigger" => {
                    completed.trigger.as_mut().unwrap().message_id = Some("foreign".into())
                }
                "missing-reply" => completed.live_input_batches[0].items.clear(),
                "incomplete-turn" => completed.terminal = None,
                "interrupted-turn" => {
                    completed.terminal.as_mut().unwrap().kind =
                        crate::types::TurnTerminalKind::Interrupted;
                }
                "aborted-turn" => {
                    completed.terminal.as_mut().unwrap().kind =
                        crate::types::TurnTerminalKind::Aborted;
                }
                _ => {}
            }
            save_recovery_attempt(&db, &attempt);
            db.turn_records().upsert(&completed).unwrap();
            queue(&db, "live-reply", reply_queue_status.clone());
            let mut response = db
                .agent_message_deliveries()
                .latest("live-response-delivery")
                .unwrap()
                .unwrap();
            response.state = crate::types::AgentMessageDeliveryState::Consumed;
            // Live followers do not pass through Claim, so both bindings
            // legitimately remain NULL. Keep P1's bound delivery coverage.
            if reply_queue_status == QueueEntryStatus::Processed
                && !matches!(case, "unbound-missing-batch" | "turn-only-missing-batch")
            {
                response.activation_id = Some(attempt.attempt_id.clone());
                response.turn_id = Some(completed.turn_id.clone());
            }
            if case == "turn-only-missing-batch" {
                response.turn_id = Some(completed.turn_id.clone());
            }
            if case == "foreign-turn" {
                response.turn_id = Some("foreign".into());
            }
            rewrite_delivery(&db, &response);
            let queue_before = db.queue_entries().latest("live-reply").unwrap().unwrap();
            let valid = matches!(case, "null-ingress" | "different-ingress",)
                || (reply_queue_status == QueueEntryStatus::Processed
                    && matches!(case, "anchor-null" | "anchor-different"));
            assert_eq!(
                db.task_result_settlements()
                    .reply_source_is_recoverable(&sibling)
                    .unwrap(),
                valid,
                "reply recovery ({reply_queue_status:?}): {case}",
            );
            assert_eq!(
                recover(&db, "live-activation", "recovery-a", &[live.clone()]),
                valid,
                "{case}"
            );
            let activation = if valid {
                "recovery-a"
            } else {
                "live-activation"
            };
            assert_admitted_to(&db, &initial, activation);
            assert_admitted_to(&db, &live, activation);
            assert_pending(&db, &sibling);
            assert_eq!(
                db.agent_message_deliveries()
                    .latest("live-response-delivery")
                    .unwrap()
                    .unwrap(),
                response
            );
            assert_eq!(
                db.queue_entries().latest("live-reply").unwrap().unwrap(),
                queue_before
            );
        }
    }
}

fn completed_reply_source(db: &RuntimeDb) -> (TurnRecord, ExecutionAttempt) {
    let mut turn = interrupted_turn("completed-reply-source", &[], true);
    turn.trigger.as_mut().unwrap().message_id = Some("consumed-anchor".into());
    turn.input_message_ids = vec!["consumed-anchor".into(), "live-reply".into()];
    turn.terminal.as_mut().unwrap().kind = crate::types::TurnTerminalKind::Completed;
    let batch = &mut turn.live_input_batches[0];
    batch.activation_id = "consumed-activation".into();
    batch.safe_point = crate::types::LiveInputSafePoint::new("consumed-activation", 2);
    batch.ordinary_message_ids = vec!["live-reply".into()];
    let mut attempt = recovery_attempt(
        db,
        "consumed-activation",
        &turn.turn_id,
        ExecutionAttemptState::Settled,
        None,
    );
    attempt.source_message_id = Some("consumed-anchor".into());
    attempt.source.identity =
        crate::domain::execution_protocol::ExecutionSourceIdentity::QueueMessage {
            message_id: "consumed-anchor".into(),
        };
    (turn, attempt)
}

#[test]
fn consumed_reply_owner_grants_use_the_canonical_turn_and_resolved_exact_task() {
    for status in [QueueEntryStatus::Processed, QueueEntryStatus::Interjected] {
        for exact_waiter in [false, true] {
            let (_dir, db) = runtime_db();
            let boundary = reply_source(&db);
            let initial = live_reply(&db, 0, "older-parent", None, boundary);
            let sibling = live_reply(&db, 1, "older-parent", None, boundary);
            let live = live_result(&db, 2, TaskKind::CommandTask, "live-parent", None);
            admit_initial(&db, &initial);
            prepare_recovery(&db, &[live.clone()], false);
            let captured_task = db.tasks().latest(&initial.task_id).unwrap().unwrap();
            let (mut turn, mut attempt) = completed_reply_source(&db);
            // This completed turn mentions the same reply and has a compatible
            // owner, but is not the delivery's canonical consumption witness.
            let mut other = turn.clone();
            other.turn_id = "other-completed-owner".into();
            other.live_input_batches.clear();
            db.turn_records().upsert(&other).unwrap();
            turn.current_work_item_id = Some("consuming-owner".into());
            attempt.binding = crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                work_item_id: "consuming-owner".into(),
            };
            if exact_waiter {
                // An already resolved wait names this original reply and only
                // this captured task; it does not grant its shared-reply sibling.
                let wait = serde_json::from_value(serde_json::json!({
                    "id": "consuming-exact-wait", "agent_id": "agent-a",
                    "work_item_id": "consuming-owner",
                    "status": "resolved", "kind": "task", "waiting_for": "reply",
                    "wake_sources": [{"kind": "task_result", "task_id": initial.task_id}],
                    "trigger_message_id": "live-reply",
                    "created_at": Utc::now(), "updated_at": Utc::now(),
                }))
                .unwrap();
                db.wait_conditions().upsert(&wait).unwrap();
            }
            save_recovery_attempt(&db, &attempt);
            db.turn_records().upsert(&turn).unwrap();
            let mut response = db
                .agent_message_deliveries()
                .latest("live-response-delivery")
                .unwrap()
                .unwrap();
            response.state = crate::types::AgentMessageDeliveryState::Consumed;
            response.activation_id = Some(attempt.attempt_id.clone());
            response.turn_id = Some(turn.turn_id.clone());
            rewrite_delivery(&db, &response);
            queue(&db, "live-reply", status.clone());
            let queue_before = db.queue_entries().latest("live-reply").unwrap().unwrap();
            let repository = db.task_result_settlements();
            assert!(repository.reply_source_is_consumed(&initial).unwrap());
            assert!(repository.reply_source_is_consumed(&sibling).unwrap());
            assert_eq!(
                repository
                    .reply_source_is_consumed_for_owner(&initial)
                    .unwrap(),
                exact_waiter,
                "owner grant ({status:?}, exact waiter: {exact_waiter})",
            );
            assert_eq!(
                repository.reply_source_is_recoverable(&initial).unwrap(),
                exact_waiter,
            );
            assert!(!repository.reply_source_is_recoverable(&sibling).unwrap());
            assert_eq!(
                recover(&db, "live-activation", "recovery-a", &[live.clone()]),
                exact_waiter,
                "transaction admission ({status:?}, exact waiter: {exact_waiter})",
            );
            let activation = if exact_waiter {
                "recovery-a"
            } else {
                "live-activation"
            };
            assert_admitted_to(&db, &initial, activation);
            assert_admitted_to(&db, &live, activation);
            assert_pending(&db, &sibling);
            assert_eq!(
                db.tasks()
                    .latest(&initial.task_id)
                    .unwrap()
                    .unwrap()
                    .work_item_id,
                captured_task.work_item_id,
            );
            assert_eq!(
                db.agent_message_deliveries()
                    .latest("live-response-delivery")
                    .unwrap()
                    .unwrap(),
                response,
            );
            assert_eq!(
                db.queue_entries().latest("live-reply").unwrap().unwrap(),
                queue_before,
            );
        }
    }
}

#[test]
fn interjected_reply_requires_one_completed_canonical_attachment_even_without_delivery_bindings() {
    for case in [
        "null-bindings",
        "bound-activation",
        "bound-turn",
        "bound-both",
        "idle",
        "replayed",
        "foreign-activation",
        "foreign-turn",
        "not-consumed",
        "not-accepted",
        "wrong-delivery-source",
        "missing-attempt",
        "open-attempt",
        "interrupted-attempt",
        "wrong-source",
        "wrong-run",
        "wrong-binding",
        "wrong-source-ref",
        "result-attached",
        "missing-item",
        "duplicate-item",
        "duplicate-batch",
        "missing-input",
        "wrong-turn-id",
        "incomplete",
        "interrupted",
        "aborted",
        "idle-unbound",
        "idle-wrong-anchor",
        "foreign-owner",
        "exact-waiter",
        "other-waiter",
        "wrong-wait-trigger",
        "active-waiter",
        "duplicate-completed",
        "unrelated-completed-owner",
    ] {
        let (_dir, db) = runtime_db();
        let boundary = reply_source(&db);
        let waiter = live_reply(&db, 0, "older-parent", None, boundary);
        let (mut turn, mut attempt) = completed_reply_source(&db);
        let mut response = db
            .agent_message_deliveries()
            .latest("live-response-delivery")
            .unwrap()
            .unwrap();
        response.state = crate::types::AgentMessageDeliveryState::Consumed;
        assert!(response.activation_id.is_none());
        assert!(response.turn_id.is_none());
        match case {
            "bound-activation" | "bound-both" => {
                response.activation_id = Some(attempt.attempt_id.clone())
            }
            "foreign-activation" => response.activation_id = Some("foreign".into()),
            "foreign-turn" => response.turn_id = Some("foreign".into()),
            "not-consumed" => response.state = crate::types::AgentMessageDeliveryState::Queued,
            "not-accepted" => {
                response.outcome = crate::types::AgentMessageDeliveryOutcome::Rejected
            }
            "wrong-delivery-source" => response.message_id = Some("foreign".into()),
            "open-attempt" => attempt.state = ExecutionAttemptState::Open,
            "interrupted-attempt" => attempt.state = ExecutionAttemptState::Interrupted,
            "wrong-source" => attempt.source_message_id = Some("foreign".into()),
            "wrong-run" => attempt.run_id = Some("foreign".into()),
            "wrong-binding" => {
                attempt.binding = crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                    work_item_id: "foreign".into(),
                }
            }
            "wrong-source-ref" => {
                turn.live_input_batches[0].items[0].source_ref = "message:foreign".into()
            }
            "result-attached" => turn.live_input_batches[0].ordinary_message_ids.clear(),
            "missing-item" => turn.live_input_batches[0].items.clear(),
            "duplicate-item" => {
                let item = turn.live_input_batches[0].items[0].clone();
                turn.live_input_batches[0].items.push(item);
            }
            "duplicate-batch" => turn
                .live_input_batches
                .push(turn.live_input_batches[0].clone()),
            "missing-input" => turn.input_message_ids.retain(|id| id != "live-reply"),
            "incomplete" => turn.terminal = None,
            "interrupted" => {
                turn.terminal.as_mut().unwrap().kind = crate::types::TurnTerminalKind::Interrupted
            }
            "aborted" => {
                turn.terminal.as_mut().unwrap().kind = crate::types::TurnTerminalKind::Aborted
            }
            "idle" | "idle-unbound" | "idle-wrong-anchor" => {
                let items = turn.live_input_batches.remove(0).items;
                turn.idle_input_batch = Some(crate::types::IdleInputBatchRecord {
                    anchor_message_id: if case == "idle-wrong-anchor" {
                        "foreign".into()
                    } else {
                        "consumed-anchor".into()
                    },
                    activation_id: (case != "idle-unbound").then(|| attempt.attempt_id.clone()),
                    sender_agent_id: "peer-b".into(),
                    cutoff: Utc::now(),
                    max_items: 8,
                    max_bytes: 24 * 1024,
                    rendered_bytes: 0,
                    items: std::iter::once(crate::types::IdleInputBatchItem {
                        message_id: "consumed-anchor".into(),
                        source_ref: "message:consumed-anchor".into(),
                        rendered: String::new(),
                    })
                    .chain(items)
                    .collect(),
                });
            }
            "replayed" => {
                let mut predecessor = turn.clone();
                predecessor.turn_id = "interrupted-reply-source".into();
                let terminal = predecessor.terminal.as_mut().unwrap();
                terminal.kind = crate::types::TurnTerminalKind::Interrupted;
                predecessor.live_input_batches[0].activation_id = "prior-activation".into();
                predecessor.live_input_batches[0].safe_point =
                    crate::types::LiveInputSafePoint::new("prior-activation", 2);
                let mut prior = attempt.clone();
                prior.attempt_id = "prior-activation".into();
                prior.state = ExecutionAttemptState::Interrupted;
                save_recovery_attempt(&db, &prior);
                db.turn_records().upsert(&predecessor).unwrap();
                attempt.recovery_of_attempt_id = Some(prior.attempt_id);
                turn.live_input_batches[0].safe_point =
                    predecessor.live_input_batches[0].safe_point.clone();
                turn.replay = Some(crate::types::TurnReplayProvenance {
                    source_message_id: "consumed-anchor".into(),
                    source_turn_id: predecessor.turn_id,
                    reason: "interrupted_queue_claim_reentry".into(),
                    prior_terminal: None,
                });
            }
            "foreign-owner" | "exact-waiter" | "other-waiter" | "wrong-wait-trigger"
            | "active-waiter" => {
                turn.current_work_item_id = Some("consuming-owner".into());
                attempt.binding = crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                    work_item_id: "consuming-owner".into(),
                };
                if case != "foreign-owner" {
                    let wait = serde_json::from_value(serde_json::json!({
                        "id": "consuming-exact-wait", "agent_id": "agent-a",
                        "work_item_id": "consuming-owner",
                        "status": if case == "active-waiter" { "active" } else { "resolved" },
                        "kind": "task", "waiting_for": "reply",
                        "wake_sources": [{
                            "kind": "task_result",
                            "task_id": if case == "other-waiter" { "foreign" } else { &waiter.task_id },
                        }],
                        "trigger_message_id": if case == "wrong-wait-trigger" {
                            "foreign"
                        } else {
                            "live-reply"
                        },
                        "created_at": Utc::now(), "updated_at": Utc::now(),
                    }))
                    .unwrap();
                    db.wait_conditions().upsert(&wait).unwrap();
                }
            }
            "duplicate-completed" | "unrelated-completed-owner" => {
                let mut other = turn.clone();
                other.turn_id = "other-completed".into();
                if case == "unrelated-completed-owner" {
                    // This turn satisfies the old broad owner SQL, but never
                    // canonically attached the reply that the delivery consumed.
                    other.live_input_batches.clear();
                    turn.current_work_item_id = Some("foreign-owner".into());
                    attempt.binding =
                        crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                            work_item_id: "foreign-owner".into(),
                        };
                }
                db.turn_records().upsert(&other).unwrap();
            }
            _ => {}
        }
        if matches!(case, "bound-turn" | "bound-both") {
            response.turn_id = Some(turn.turn_id.clone());
        }
        save_recovery_attempt(&db, &attempt);
        if case == "missing-attempt" {
            db.transaction(|tx| {
                tx.execute(
                    "DELETE FROM execution_protocol_attempts WHERE agent_id = ?1 AND attempt_id = ?2",
                    params!["agent-a", attempt.attempt_id],
                )?;
                Ok(())
            })
            .unwrap();
        }
        db.turn_records().upsert(&turn).unwrap();
        if case == "wrong-turn-id" {
            db.transaction(|tx| {
                assert_eq!(
                    tx.execute(
                        "UPDATE turn_records
                         SET payload_json = json_set(payload_json, '$.turn_id', 'foreign')
                         WHERE agent_id = ?1 AND turn_id = ?2",
                        params![turn.agent_id, turn.turn_id],
                    )?,
                    1,
                );
                Ok(())
            })
            .unwrap();
        }
        rewrite_delivery(&db, &response);
        queue(&db, "live-reply", QueueEntryStatus::Interjected);
        let expected = matches!(
            case,
            "null-bindings"
                | "result-attached"
                | "bound-activation"
                | "bound-turn"
                | "bound-both"
                | "idle"
                | "replayed"
                | "exact-waiter"
        );
        assert_eq!(
            db.task_result_settlements()
                .reply_source_is_recoverable(&waiter)
                .unwrap(),
            expected,
            "{case}",
        );
        if case == "exact-waiter" {
            let sibling = live_reply(&db, 1, "older-parent", None, boundary);
            assert!(!db
                .task_result_settlements()
                .reply_source_is_recoverable(&sibling)
                .unwrap());
            assert_pending(&db, &sibling);
        }
        assert_pending(&db, &waiter);
        assert_eq!(
            db.agent_message_deliveries()
                .latest("live-response-delivery")
                .unwrap()
                .unwrap(),
            response,
            "recovery must not claim or reconsume the delivery: {case}",
        );
    }
}

#[test]
fn mixed_initial_live_recovery_restores_entire_ledger_twice() {
    let (_dir, db) = runtime_db();
    let command = live_result(&db, 0, TaskKind::CommandTask, "older-command-parent", None);
    let child = live_result(&db, 1, TaskKind::ChildAgentTask, "older-child-parent", None);
    let live = live_result(
        &db,
        2,
        TaskKind::CommandTask,
        "live-parent",
        Some("captured-live"),
    );
    admit_initial(&db, &command);
    admit_initial(&db, &child);
    prepare_recovery(&db, &[live.clone()], false);
    lifecycle_recovery_binding(&db, "live-activation");
    lifecycle_recovery_binding(&db, "recovery-a");
    assert!(recover(
        &db,
        "live-activation",
        "recovery-a",
        &[live.clone()]
    ));
    for record in [&command, &child, &live] {
        assert_admitted_to(&db, record, "recovery-a");
    }
    assert_eq!(
        db.queue_entries()
            .latest(&command.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    interrupted_attachment(&db, "recovery-turn-a", &[live.clone()], false);
    recovery_attempt(
        &db,
        "recovery-a",
        "recovery-turn-a",
        ExecutionAttemptState::Interrupted,
        Some("live-activation"),
    );
    recovery_attempt(
        &db,
        "recovery-b",
        "recovery-turn-b",
        ExecutionAttemptState::Open,
        Some("recovery-a"),
    );
    lifecycle_recovery_binding(&db, "recovery-a");
    lifecycle_recovery_binding(&db, "recovery-b");
    assert!(recover(&db, "recovery-a", "recovery-b", &[live.clone()]));
    for record in [&command, &child, &live] {
        assert_admitted_to(&db, record, "recovery-b");
        assert_eq!(
            db.tasks()
                .latest(&record.task_id)
                .unwrap()
                .unwrap()
                .work_item_id,
            record.work_item_id
        );
    }
}

#[test]
fn initial_recovery_invalid_fence_or_owner_blocks_all_live_transfers() {
    for invalid in ["generation", "parent", "provenance", "owner"] {
        let (_dir, db) = runtime_db();
        let initial = live_result(
            &db,
            0,
            TaskKind::CommandTask,
            "older-parent",
            (invalid == "owner").then_some("unrelated-owner"),
        );
        let live = live_result(&db, 1, TaskKind::ChildAgentTask, "live-parent", None);
        admit_initial(&db, &initial);
        prepare_recovery(&db, &[live.clone()], false);
        lifecycle_recovery_binding(&db, "live-activation");
        lifecycle_recovery_binding(&db, "recovery-a");
        match invalid {
            "generation" | "parent" => {
                let mut task = db.tasks().latest(&initial.task_id).unwrap().unwrap();
                let detail = task.detail.as_mut().unwrap();
                if invalid == "generation" {
                    detail["rejoin_generation"] = serde_json::json!(2);
                } else {
                    detail["parent_turn_id"] = serde_json::json!("replacement-parent");
                }
                rewrite_task(&db, &task);
            }
            "provenance" => {
                let mut message = db
                    .messages()
                    .by_id(None, &initial.message_id)
                    .unwrap()
                    .unwrap();
                message.authority_class = AuthorityClass::OperatorInstruction;
                db.messages().upsert(&message).unwrap();
            }
            _ => {}
        }
        let before = db
            .task_result_settlements()
            .admitted_for_activation("agent-a", "live-activation")
            .unwrap();
        assert!(
            !recover(&db, "live-activation", "recovery-a", &[live.clone()]),
            "{invalid}"
        );
        assert_eq!(
            db.task_result_settlements()
                .admitted_for_activation("agent-a", "live-activation",)
                .unwrap(),
            before,
            "{invalid}"
        );
        assert!(db
            .task_result_settlements()
            .admitted_for_activation("agent-a", "recovery-a",)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn initial_recovery_does_not_reconsume_or_admit_a_shared_reply_sibling() {
    let (_dir, db) = runtime_db();
    let boundary = reply_source(&db);
    let initial = live_reply(&db, 0, "older-parent", None, boundary);
    let sibling = live_reply(&db, 1, "older-parent", Some("other-owner"), boundary);
    let live = live_result(
        &db,
        2,
        TaskKind::CommandTask,
        "live-parent",
        Some("captured-live"),
    );
    admit_initial(&db, &initial);
    prepare_recovery(&db, &[live.clone()], false);
    lifecycle_recovery_binding(&db, "live-activation");
    lifecycle_recovery_binding(&db, "recovery-a");
    let (completed, attempt) = completed_reply_source(&db);
    save_recovery_attempt(&db, &attempt);
    db.turn_records().upsert(&completed).unwrap();
    queue(&db, "live-reply", QueueEntryStatus::Processed);
    let mut response = db
        .agent_message_deliveries()
        .latest("live-response-delivery")
        .unwrap()
        .unwrap();
    response.state = crate::types::AgentMessageDeliveryState::Consumed;
    response.turn_id = Some(completed.turn_id);
    rewrite_delivery(&db, &response);
    let before_queue = db.queue_entries().latest("live-reply").unwrap().unwrap();
    assert!(recover(
        &db,
        "live-activation",
        "recovery-a",
        &[live.clone()]
    ));
    assert_admitted_to(&db, &initial, "recovery-a");
    assert_admitted_to(&db, &live, "recovery-a");
    assert_pending(&db, &sibling);
    assert_eq!(
        db.agent_message_deliveries()
            .latest("live-response-delivery")
            .unwrap()
            .unwrap(),
        response
    );
    assert_eq!(
        db.queue_entries().latest("live-reply").unwrap().unwrap(),
        before_queue
    );
}

#[test]
fn empty_live_identity_set_recovers_only_the_initial_admitted_ledger() {
    let (_dir, db) = runtime_db();
    let initial = live_result(&db, 0, TaskKind::CommandTask, "older-parent", None);
    let pending = live_result(&db, 1, TaskKind::ChildAgentTask, "older-parent", None);
    admit_initial(&db, &initial);
    prepare_recovery(&db, &[], false);
    lifecycle_recovery_binding(&db, "live-activation");
    lifecycle_recovery_binding(&db, "recovery-a");
    assert!(recover(&db, "live-activation", "recovery-a", &[]));
    assert_admitted_to(&db, &initial, "recovery-a");
    assert_pending(&db, &pending);
}

#[test]
fn recovery_transfers_three_sources_and_survives_a_second_interruption() {
    let (_dir, db) = runtime_db();
    let boundary = reply_source(&db);
    let records = vec![
        live_result(
            &db,
            0,
            TaskKind::CommandTask,
            "live-parent",
            Some("captured-a"),
        ),
        live_result(&db, 1, TaskKind::ChildAgentTask, "live-parent", None),
        live_reply(&db, 2, "live-parent", Some("captured-b"), boundary),
    ];
    prepare_recovery(&db, &records, true);
    let admitted_at = db
        .task_result_settlements()
        .latest_for_message(&records[0].message_id)
        .unwrap()
        .unwrap()
        .admitted_at;
    assert!(recover(&db, "live-activation", "recovery-a", &records));
    for record in &records {
        assert_admitted_to(&db, record, "recovery-a");
        let task = db.tasks().latest(&record.task_id).unwrap().unwrap();
        assert_eq!(task.work_item_id, record.work_item_id);
        assert_eq!(task.rejoin_fence().unwrap().parent_turn_id, "live-parent");
    }
    assert!(!recover(&db, "live-activation", "recovery-a", &records));
    interrupted_attachment(&db, "recovery-turn-a", &records, true);
    recovery_attempt(
        &db,
        "recovery-a",
        "recovery-turn-a",
        ExecutionAttemptState::Interrupted,
        Some("live-activation"),
    );
    recovery_attempt(
        &db,
        "recovery-b",
        "recovery-turn-b",
        ExecutionAttemptState::Open,
        Some("recovery-a"),
    );
    assert!(recover(&db, "recovery-a", "recovery-b", &records));
    for record in &records {
        assert_admitted_to(&db, record, "recovery-b");
    }
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&records[0].message_id)
            .unwrap()
            .unwrap()
            .admitted_at,
        admitted_at
    );
    let response = db
        .agent_message_deliveries()
        .latest("live-response-delivery")
        .unwrap()
        .unwrap();
    assert_eq!(response.activation_id.as_deref(), Some("live-activation"));
    assert_eq!(response.turn_id.as_deref(), Some("live-parent"));
}

#[test]
fn recovery_never_grants_a_shared_reply_sibling_that_was_not_admitted() {
    let (_dir, db) = runtime_db();
    let boundary = reply_source(&db);
    let admitted = live_reply(&db, 0, "live-parent", Some("captured-a"), boundary);
    let pending = live_reply(&db, 1, "live-parent", Some("captured-b"), boundary);
    prepare_recovery(&db, &[admitted.clone()], true);
    assert!(!recover(
        &db,
        "live-activation",
        "recovery-a",
        &[admitted.clone(), pending.clone()]
    ));
    assert_admitted_to(&db, &admitted, "live-activation");
    assert_pending(&db, &pending);
    assert!(recover(
        &db,
        "live-activation",
        "recovery-a",
        &[admitted.clone()]
    ));
    assert_admitted_to(&db, &admitted, "recovery-a");
    assert_pending(&db, &pending);
}

#[test]
fn recovery_rejects_pending_settled_unknown_and_competing_identity_selections_atomically() {
    for invalid in ["pending", "settled", "unknown", "competing"] {
        let (_dir, db) = runtime_db();
        let first = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let second = live_result(&db, 1, TaskKind::CommandTask, "live-parent", None);
        prepare_recovery(&db, &[first.clone(), second.clone()], false);
        let mut altered = db
            .task_result_settlements()
            .latest_for_message(&second.message_id)
            .unwrap()
            .unwrap();
        match invalid {
            "pending" => {
                altered.state = TaskResultSettlementState::PersistedPending;
                altered.activation_id = None;
                altered.admitted_at = None;
            }
            "settled" => {
                altered.state = TaskResultSettlementState::Settled;
                altered.disposition =
                    Some(super::super::TaskResultSettlementDisposition::ModelDelivered);
                altered.settled_at = Some(Utc::now());
            }
            "unknown" => {}
            "competing" => altered.activation_id = Some("other-activation".into()),
            _ => unreachable!(),
        }
        db.transaction(|tx| update_tx(tx, &altered)).unwrap();
        let mut selected = BTreeSet::from([
            first.result_identity.clone(),
            second.result_identity.clone(),
        ]);
        if invalid == "unknown" {
            selected.insert("unknown-result".into());
        }
        assert!(
            !db.transaction(|tx| recover_live_task_results_tx(
                tx,
                "agent-a",
                "live-activation",
                "recovery-a",
                &selected,
                Utc::now(),
            ))
            .unwrap(),
            "{invalid}"
        );
        assert_admitted_to(&db, &first, "live-activation");
        assert_eq!(
            db.task_result_settlements()
                .latest_for_message(&second.message_id)
                .unwrap()
                .unwrap(),
            altered
        );
    }
}

#[test]
fn recovery_requires_an_interrupted_predecessor_with_the_same_source_and_binding() {
    for invalid in [
        "closed",
        "prior-open",
        "successor-closed",
        "source",
        "binding",
        "reference",
        "cycle",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        prepare_recovery(&db, &[record.clone()], false);
        let target = if matches!(invalid, "closed" | "prior-open" | "cycle") {
            "live-activation"
        } else {
            "recovery-a"
        };
        let mut attempt = db
            .transaction(|tx| super::super::reply_admission_attempt_tx(tx, "agent-a", target))
            .unwrap()
            .unwrap();
        match invalid {
            "closed" | "successor-closed" => attempt.state = ExecutionAttemptState::Settled,
            "prior-open" => attempt.state = ExecutionAttemptState::Open,
            "source" => attempt.source_message_id = Some("other-source".into()),
            "binding" => {
                attempt.binding = crate::domain::execution_protocol::ExecutionBinding::Command
            }
            "reference" => attempt.recovery_of_attempt_id = Some("other-attempt".into()),
            "cycle" => attempt.recovery_of_attempt_id = Some("live-activation".into()),
            _ => unreachable!(),
        }
        if invalid == "prior-open" {
            // Keep the unique canonical open successor; corrupt only the
            // predecessor payload to test rejection of contradictory evidence.
            db.transaction(|tx| {
                assert_eq!(
                    tx.execute(
                        "UPDATE execution_protocol_attempts SET payload_json = ?3
                         WHERE agent_id = ?1 AND attempt_id = ?2",
                        params![
                            attempt.agent_id,
                            attempt.attempt_id,
                            serde_json::to_string(&attempt)?,
                        ],
                    )?,
                    1,
                );
                Ok(())
            })
            .unwrap();
        } else {
            save_recovery_attempt(&db, &attempt);
        }
        assert!(
            !recover(&db, "live-activation", "recovery-a", &[record.clone()]),
            "{invalid}"
        );
        assert_admitted_to(&db, &record, "live-activation");
    }
}

#[test]
fn recovery_rechecks_original_fences_provenance_attachment_and_reply_claims() {
    for invalid in [
        "parent",
        "generation",
        "provenance",
        "missing-source",
        "missing-attachment",
        "foreign-attachment",
        "reply-claim",
        "result-generation",
    ] {
        let (_dir, db) = runtime_db();
        let boundary = reply_source(&db);
        let record = live_reply(&db, 0, "live-parent", Some("captured-owner"), boundary);
        prepare_recovery(&db, &[record.clone()], true);
        match invalid {
            "parent" | "generation" => {
                let mut task = db.tasks().latest(&record.task_id).unwrap().unwrap();
                let detail = task.detail.as_mut().unwrap();
                if invalid == "parent" {
                    detail["parent_turn_id"] = serde_json::json!("foreign-parent");
                } else {
                    detail["rejoin_generation"] = serde_json::json!(2);
                }
                rewrite_task(&db, &task);
            }
            "provenance" => {
                let mut message = db
                    .messages()
                    .by_id(None, &record.message_id)
                    .unwrap()
                    .unwrap();
                message.authority_class = AuthorityClass::ExternalEvidence;
                db.messages().upsert(&message).unwrap();
            }
            "missing-source" => {
                db.transaction(|tx| {
                    tx.execute("DELETE FROM messages WHERE message_id = 'live-reply'", [])?;
                    Ok(())
                })
                .unwrap();
            }
            "missing-attachment" => {
                let mut turn = db
                    .turn_records()
                    .by_id(Some("agent-a"), "live-parent")
                    .unwrap()
                    .unwrap();
                turn.input_message_ids.retain(|id| id != &record.message_id);
                db.turn_records().upsert(&turn).unwrap();
            }
            "foreign-attachment" => {
                interrupted_attachment(&db, "foreign-turn", &[record.clone()], true)
            }
            "reply-claim" => {
                let mut response = db
                    .agent_message_deliveries()
                    .latest("live-response-delivery")
                    .unwrap()
                    .unwrap();
                response.activation_id = Some("competing-activation".into());
                rewrite_delivery(&db, &response);
            }
            "result-generation" => {
                let mut altered = db
                    .task_result_settlements()
                    .latest_for_message(&record.message_id)
                    .unwrap()
                    .unwrap();
                altered.rejoin_generation = 2;
                db.transaction(|tx| update_tx(tx, &altered)).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            !recover(&db, "live-activation", "recovery-a", &[record.clone()]),
            "{invalid}"
        );
        assert_eq!(
            db.task_result_settlements()
                .latest_for_message(&record.message_id)
                .unwrap()
                .unwrap()
                .activation_id
                .as_deref(),
            Some("live-activation")
        );
    }
}

#[test]
fn a_closed_older_ancestor_is_not_a_grant_or_a_blocker_for_later_live_sources() {
    let (_dir, db) = runtime_db();
    let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
    prepare_recovery(&db, &[record.clone()], false);
    recovery_attempt(
        &db,
        "closed-ancestor",
        "closed-turn",
        ExecutionAttemptState::Settled,
        None,
    );
    recovery_attempt(
        &db,
        "live-activation",
        "live-parent",
        ExecutionAttemptState::Interrupted,
        Some("closed-ancestor"),
    );
    assert!(!recover(
        &db,
        "closed-ancestor",
        "recovery-a",
        &[record.clone()]
    ));
    assert_admitted_to(&db, &record, "live-activation");
    assert!(recover(
        &db,
        "live-activation",
        "recovery-a",
        &[record.clone()]
    ));
    assert_admitted_to(&db, &record, "recovery-a");
}

#[test]
fn recovery_source_transfer_rolls_back_with_the_parent_transaction() {
    let (_dir, db) = runtime_db();
    let record = live_result(
        &db,
        0,
        TaskKind::CommandTask,
        "live-parent",
        Some("captured-owner"),
    );
    prepare_recovery(&db, &[record.clone()], false);
    let before = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    let selected = BTreeSet::from([record.result_identity.clone()]);
    let result: Result<()> = db.transaction(|tx| {
        assert!(recover_live_task_results_tx(
            tx,
            "agent-a",
            "live-activation",
            "recovery-a",
            &selected,
            Utc::now(),
        )?);
        anyhow::bail!("fault after source transfer")
    });
    assert!(result.is_err());
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap(),
        before
    );
    assert!(recover(&db, "live-activation", "recovery-a", &[record]));
}

#[test]
fn same_parent_admits_three_sources_without_migrating_captured_owners() {
    let (_dir, db) = runtime_db();
    let command = live_result(
        &db,
        0,
        TaskKind::CommandTask,
        "live-parent",
        Some("old-owner"),
    );
    let child = live_result(&db, 1, TaskKind::ChildAgentTask, "live-parent", None);
    let boundary = reply_source(&db);
    let reply = live_reply(&db, 2, "live-parent", Some("third-owner"), boundary);
    let records = vec![command, child, reply];
    let candidates = db
        .task_result_settlements()
        .same_turn_candidates("agent-a", "live-parent", Utc::now(), 64)
        .unwrap();
    assert_eq!(candidates.len(), 3);
    assert!(admit(&db, &records, "live-activation"));
    for record in &records {
        let actual = db
            .task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(actual.state, TaskResultSettlementState::CallerAdmitted);
        assert_eq!(actual.activation_id.as_deref(), Some("live-activation"));
        assert_eq!(actual.work_item_id, record.work_item_id);
        assert_eq!(
            db.tasks()
                .latest(&record.task_id)
                .unwrap()
                .unwrap()
                .work_item_id,
            record.work_item_id
        );
    }
}

#[test]
fn live_candidates_are_bounded_by_cutoff_and_not_p1_activation_slots() {
    let (_dir, db) = runtime_db();
    let records: Vec<_> = (0..66)
        .map(|index| {
            live_result(
                &db,
                index,
                TaskKind::CommandTask,
                "live-parent",
                Some("work-a"),
            )
        })
        .collect();
    let repository = db.task_result_settlements();
    assert_eq!(
        repository
            .same_turn_candidates("agent-a", "live-parent", Utc::now(), usize::MAX)
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        repository
            .same_turn_candidates("agent-a", "live-parent", Utc::now(), 3)
            .unwrap()
            .len(),
        3
    );
    assert!(repository
        .same_turn_candidates("agent-a", "live-parent", Utc::now(), 0)
        .unwrap()
        .is_empty());
    assert_eq!(
        repository
            .same_turn_candidates("agent-a", "live-parent", records[2].created_at, 64)
            .unwrap()
            .len(),
        3
    );
    assert!(admit(&db, &records[..9], "live-activation"));
    assert!(admit(&db, &records[9..18], "live-activation"));
    assert_eq!(
        repository
            .admitted_for_activation("agent-a", "live-activation")
            .unwrap()
            .len(),
        18
    );
    assert!(!admit(&db, &records, "another-activation"));
}

#[test]
fn forged_task_parent_generation_and_identity_reject_the_entire_selection() {
    for field in [
        "parent_turn_id",
        "rejoin_generation",
        "rejoin_obligation_id",
    ] {
        let (_dir, db) = runtime_db();
        let first = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let forged = live_result(&db, 1, TaskKind::ChildAgentTask, "live-parent", None);
        let mut task = db.tasks().latest(&forged.task_id).unwrap().unwrap();
        task.detail.as_mut().unwrap()[field] = if field == "rejoin_generation" {
            serde_json::json!(2)
        } else {
            serde_json::json!("forged")
        };
        rewrite_task(&db, &task);
        assert!(
            !admit(&db, &[first.clone(), forged.clone()], "live-activation"),
            "{field}"
        );
        assert_pending(&db, &first);
        assert_pending(&db, &forged);
        assert_eq!(
            db.task_result_settlements()
                .same_turn_candidates("agent-a", "live-parent", Utc::now(), 64)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn forged_result_provenance_or_fence_is_not_a_same_parent_grant() {
    for field in [
        "origin",
        "authority",
        "surface",
        "context",
        "parent",
        "generation",
        "status",
        "task",
    ] {
        let (_dir, db) = runtime_db();
        let record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let mut message = db
            .messages()
            .by_id(None, &record.message_id)
            .unwrap()
            .unwrap();
        match field {
            "origin" => {
                message.origin = MessageOrigin::System {
                    subsystem: "forged".into(),
                }
            }
            "authority" => message.authority_class = AuthorityClass::ExternalEvidence,
            "surface" => message.delivery_surface = Some(MessageDeliverySurface::RuntimeSystem),
            "context" => message.admission_context = None,
            "parent" => {
                message.metadata.as_mut().unwrap()["task_detail"]["parent_turn_id"] =
                    serde_json::json!("forged")
            }
            "generation" => {
                message.metadata.as_mut().unwrap()["task_detail"]["rejoin_generation"] =
                    serde_json::json!(2)
            }
            "status" => {
                message.metadata.as_mut().unwrap()["task_status"] = serde_json::json!("running")
            }
            "task" => message.task_id = Some("another-task".into()),
            _ => unreachable!(),
        }
        db.messages().upsert(&message).unwrap();
        assert!(!admit(&db, &[record.clone()], "live-activation"), "{field}");
        assert_pending(&db, &record);
    }
}

#[test]
fn selection_revalidation_rejects_unknown_consumed_and_competing_claims() {
    for competing in ["unknown", "settlement", "queue", "turn"] {
        let (_dir, db) = runtime_db();
        let first = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        let second = live_result(&db, 1, TaskKind::ChildAgentTask, "live-parent", None);
        assert_eq!(
            db.task_result_settlements()
                .same_turn_candidates("agent-a", "live-parent", Utc::now(), 64)
                .unwrap()
                .len(),
            2
        );
        let mut selected: BTreeSet<_> = [&first, &second]
            .iter()
            .map(|record| record.result_identity.clone())
            .collect();
        match competing {
            "unknown" => {
                selected.insert("missing-identity".into());
            }
            "settlement" => {
                assert!(admit(&db, &[second.clone()], "other-activation"));
            }
            "queue" => queue(&db, &second.message_id, QueueEntryStatus::Dequeued),
            "turn" => {
                let mut turn = TurnRecord::new("agent-a", "other-turn", 1);
                turn.input_message_ids.push(second.message_id.clone());
                db.turn_records().upsert(&turn).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            !db.transaction(|tx| admit_live_task_results_tx(
                tx,
                "agent-a",
                "live-parent",
                "live-activation",
                &selected,
                Utc::now(),
                Utc::now(),
            ))
            .unwrap(),
            "{competing}"
        );
        assert_pending(&db, &first);
        if competing != "settlement" {
            assert_pending(&db, &second);
        }
    }
}

#[test]
fn shared_reply_body_grants_only_selected_same_parent_waiter_identities() {
    let (_dir, db) = runtime_db();
    let boundary = reply_source(&db);
    let first = live_reply(&db, 0, "live-parent", Some("owner-a"), boundary);
    let second = live_reply(&db, 1, "live-parent", Some("owner-b"), boundary);
    let foreign = live_reply(&db, 2, "other-parent", Some("owner-a"), boundary);
    assert_eq!(
        db.task_result_settlements()
            .same_turn_candidates("agent-a", "live-parent", Utc::now(), 64)
            .unwrap()
            .len(),
        2
    );
    assert!(!admit(
        &db,
        &[first.clone(), foreign.clone()],
        "live-activation"
    ));
    assert_pending(&db, &first);
    assert_pending(&db, &foreign);
    assert!(admit(&db, &[first.clone()], "live-activation"));
    assert_pending(&db, &second);
    let mut turn = TurnRecord::new("agent-a", "live-parent", 1);
    turn.input_message_ids.push("live-reply".into());
    db.turn_records().upsert(&turn).unwrap();
    queue(&db, "live-reply", QueueEntryStatus::Processed);
    assert!(admit(&db, &[second], "live-activation"));
    assert_pending(&db, &foreign);
    assert!(!admit(&db, &[first], "restarted-activation"));
}

#[test]
fn reply_receipt_sender_body_and_claim_are_rechecked() {
    for invalid in [
        "boundary",
        "sender",
        "reference",
        "body-source",
        "delivery",
        "consumed",
        "claimed",
        "rejected",
        "delivery-sender",
        "delivery-claim",
        "delivery-consumed",
        "reply-provenance",
        "missing-body",
    ] {
        let (_dir, db) = runtime_db();
        let boundary = reply_source(&db);
        let record = live_reply(&db, 0, "live-parent", None, boundary);
        match invalid {
            "boundary" | "sender" => {
                let mut task = db.tasks().latest(&record.task_id).unwrap().unwrap();
                if let Some(TaskRecoverySpec::AgentMessageWait {
                    target_agent_id,
                    after_delivery_rowid,
                    ..
                }) = task.recovery.as_mut()
                {
                    if invalid == "boundary" {
                        *after_delivery_rowid = Some(boundary + 1);
                    } else {
                        *target_agent_id = "forged-peer".into();
                    }
                }
                rewrite_task(&db, &task);
            }
            "reference" | "body-source" | "delivery" => {
                let mut message = db
                    .messages()
                    .by_id(None, &record.message_id)
                    .unwrap()
                    .unwrap();
                let field = match invalid {
                    "reference" => "message_id",
                    "body-source" => "reply_content_source",
                    _ => "response_delivery_id",
                };
                message.metadata.as_mut().unwrap()["task_detail"][field] =
                    serde_json::json!("forged");
                db.messages().upsert(&message).unwrap();
            }
            "consumed" => queue(&db, "live-reply", QueueEntryStatus::Processed),
            "claimed" => queue(&db, "live-reply", QueueEntryStatus::Dequeued),
            "rejected" | "delivery-sender" | "delivery-claim" | "delivery-consumed" => {
                let mut delivery = db
                    .agent_message_deliveries()
                    .latest("live-response-delivery")
                    .unwrap()
                    .unwrap();
                match invalid {
                    "rejected" => {
                        delivery.outcome = crate::types::AgentMessageDeliveryOutcome::Rejected
                    }
                    "delivery-sender" => {
                        delivery.caller.caller_agent_id = Some("forged-peer".into())
                    }
                    "delivery-claim" => {
                        delivery.activation_id = Some("other-activation".into());
                        delivery.turn_id = Some("other-turn".into());
                    }
                    "delivery-consumed" => {
                        delivery.state = crate::types::AgentMessageDeliveryState::Consumed
                    }
                    _ => unreachable!(),
                }
                rewrite_delivery(&db, &delivery);
            }
            "reply-provenance" => {
                let mut reply = db.messages().by_id(None, "live-reply").unwrap().unwrap();
                reply.origin = MessageOrigin::System {
                    subsystem: "forged".into(),
                };
                db.messages().upsert(&reply).unwrap();
            }
            "missing-body" => {
                db.transaction(|tx| {
                    tx.execute("DELETE FROM messages WHERE message_id = 'live-reply'", [])?;
                    Ok(())
                })
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            !admit(&db, &[record.clone()], "live-activation"),
            "{invalid}"
        );
        assert_pending(&db, &record);
    }
}

#[test]
fn live_grant_rejects_a_forged_ledger_identity_and_missing_selected_source() {
    for invalid in [
        "identity",
        "missing-task",
        "missing-result",
        "missing-queue",
        "non-terminal",
        "unsupported-kind",
    ] {
        let (_dir, db) = runtime_db();
        let mut record = live_result(&db, 0, TaskKind::CommandTask, "live-parent", None);
        match invalid {
            "identity" => {
                let original = record.result_identity.clone();
                record.result_identity = "forged-result-identity".into();
                db.transaction(|tx| {
                    tx.execute(
                        "UPDATE task_result_settlements SET result_identity = ?2, payload_json = ?3
                         WHERE result_identity = ?1",
                        params![
                            original,
                            record.result_identity,
                            serde_json::to_string(&record)?
                        ],
                    )?;
                    Ok(())
                })
                .unwrap();
            }
            "missing-task" => {
                db.transaction(|tx| {
                    tx.execute("DELETE FROM tasks WHERE task_id = ?1", [&record.task_id])?;
                    Ok(())
                })
                .unwrap();
            }
            "missing-result" => {
                db.transaction(|tx| {
                    tx.execute(
                        "DELETE FROM messages WHERE message_id = ?1",
                        [&record.message_id],
                    )?;
                    Ok(())
                })
                .unwrap();
            }
            "missing-queue" => {
                db.transaction(|tx| {
                    tx.execute(
                        "DELETE FROM queue_entries WHERE message_id = ?1",
                        [&record.message_id],
                    )?;
                    Ok(())
                })
                .unwrap();
            }
            "non-terminal" | "unsupported-kind" => {
                let mut task = db.tasks().latest(&record.task_id).unwrap().unwrap();
                if invalid == "non-terminal" {
                    task.status = TaskStatus::Running;
                } else {
                    task.kind = TaskKind::ActorInvocation;
                }
                rewrite_task(&db, &task);
            }
            _ => unreachable!(),
        }
        assert!(
            !admit(&db, &[record.clone()], "live-activation"),
            "{invalid}"
        );
        assert_pending(&db, &record);
    }
}
