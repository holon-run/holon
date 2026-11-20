use super::*;
use crate::domain::execution_protocol::{ExecutionAttempt, ExecutionAttemptState};
use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
    MessageOrigin, Priority,
};

fn save_attempt(db: &RuntimeDb, source: &MessageEnvelope, id: &str, state: ExecutionAttemptState) {
    let attempt: ExecutionAttempt = serde_json::from_value(serde_json::json!({
        "attempt_id": id, "agent_id": "agent-a", "source_message_id": source.id,
        "source": {
            "identity": {"kind": "queue_message", "message_id": source.id},
            "generation": 1,
        },
        "binding": {"kind": "work_item", "work_item_id": "work-a"},
        "provenance": {
            "origin": "system", "trust": "runtime_instruction", "priority": "next",
        },
        "admitted_fences": {
            "source_revision": 1, "agent_control_revision": 1, "host_registry_revision": 1,
        },
        "state": state, "turn_id": id,
        "admitted_at": pending_record(0).created_at.to_rfc3339(),
    }))
    .unwrap();
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state, source_identity_json,
               source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, 1, NULL, NULL, ?5)",
            params![
                attempt.agent_id,
                attempt.attempt_id,
                serde_json::to_value(state)?.as_str().unwrap(),
                serde_json::to_string(&attempt.source.identity)?,
                serde_json::to_string(&attempt)?,
            ],
        )?;
        Ok(())
    })
    .unwrap();
}

fn save_record(db: &RuntimeDb, record: &TaskResultSettlementRecord) {
    db.transaction(|tx| update_tx(tx, record)).unwrap();
}

#[test]
fn current_recovery_wake_can_admit_but_does_not_release_another_activation() {
    for prior_state in [
        None,
        Some(ExecutionAttemptState::Settled),
        Some(ExecutionAttemptState::Interrupted),
    ] {
        let (_dir, db) = runtime_db();
        let mut record = pending_record(0);
        let mut result = reply_result_fixture(&db, &record);
        result.work_item_id = record.work_item_id.clone();
        db.messages().upsert(&result).unwrap();
        if let Some(state) = prior_state {
            save_attempt(&db, &result, "prior", state);
            record.state = TaskResultSettlementState::CallerAdmitted;
            record.activation_id = Some("prior".into());
            record.admitted_at = Some(record.created_at);
            save_record(&db, &record);
        }
        let mut recovery = MessageEnvelope::new(
            "agent-a",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "task_result_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "recover task result".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        recovery.work_item_id = record.work_item_id.clone();
        recovery.source_refs.insert(
            "task_result_identity".into(),
            record.result_identity.clone(),
        );
        db.messages().upsert(&recovery).unwrap();
        save_attempt(
            &db,
            &recovery,
            "current-recovery",
            ExecutionAttemptState::Open,
        );

        let repository = db.task_result_settlements();
        for activation_id in [None, Some("other-activation")] {
            assert_eq!(
                repository
                    .classify_obligation(&record, activation_id, Some("work-a"))
                    .unwrap(),
                TaskResultObligation::InFlight {
                    activation_id: "current-recovery".into(),
                },
            );
        }
        assert_eq!(
            repository
                .classify_obligation(&record, Some("current-recovery"), Some("work-a"))
                .unwrap(),
            TaskResultObligation::Recoverable,
        );

        record.recovery_paused_at = Some(record.created_at);
        save_record(&db, &record);
        assert_eq!(
            repository
                .classify_obligation(&record, Some("current-recovery"), Some("work-a"))
                .unwrap(),
            TaskResultObligation::Paused,
        );

        record.recovery_paused_at = None;
        record.state = TaskResultSettlementState::CallerAdmitted;
        record.activation_id = Some("current-recovery".into());
        record.admitted_at = Some(record.created_at);
        save_record(&db, &record);
        assert_eq!(
            repository
                .classify_obligation(&record, Some("current-recovery"), Some("work-a"))
                .unwrap(),
            TaskResultObligation::AlreadyAdmitted {
                activation_id: "current-recovery".into(),
            },
        );
    }
}

#[test]
fn original_result_source_is_still_in_flight_before_snapshot_admission() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    let mut result = reply_result_fixture(&db, &record);
    result.work_item_id = record.work_item_id.clone();
    db.messages().upsert(&result).unwrap();
    save_attempt(&db, &result, "original", ExecutionAttemptState::Open);

    assert_eq!(
        db.task_result_settlements()
            .classify_obligation(&record, Some("original"), Some("work-a"))
            .unwrap(),
        TaskResultObligation::InFlight {
            activation_id: "original".into(),
        },
    );
}
