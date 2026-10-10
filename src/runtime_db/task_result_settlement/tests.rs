use super::*;
use chrono::{Duration, TimeZone};
use tempfile::tempdir;

fn runtime_db() -> (tempfile::TempDir, RuntimeDb) {
    let dir = tempdir().unwrap();
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    (dir, db)
}

fn pending_record(index: usize) -> TaskResultSettlementRecord {
    let created_at =
        Utc.timestamp_opt(1_700_000_000, 0).single().unwrap() + Duration::seconds(index as i64);
    TaskResultSettlementRecord {
        result_identity: format!("result-{index:02}"),
        agent_id: "agent-a".into(),
        task_id: format!("task-{index:02}"),
        message_id: format!("message-{index:02}"),
        work_item_id: Some("work-a".into()),
        rejoin_generation: 1,
        parent_turn_id: "turn-parent".into(),
        task_status: "completed".into(),
        state: TaskResultSettlementState::PersistedPending,
        activation_id: None,
        disposition: None,
        created_at,
        updated_at: created_at,
        admitted_at: None,
        settled_at: None,
        deferred_reason: None,
        deferred_at: None,
        next_recheck_at: Some(created_at + Duration::seconds(30)),
    }
}

fn insert(db: &RuntimeDb, record: &TaskResultSettlementRecord) {
    insert_with_kind(db, record, crate::types::TaskKind::CommandTask);
}

fn insert_with_kind(
    db: &RuntimeDb,
    record: &TaskResultSettlementRecord,
    kind: crate::types::TaskKind,
) {
    db.tasks()
        .upsert(&TaskRecord {
            id: record.task_id.clone(),
            agent_id: record.agent_id.clone(),
            kind,
            status: crate::types::TaskStatus::Completed,
            created_at: record.created_at,
            updated_at: record.updated_at,
            parent_message_id: Some(record.message_id.clone()),
            work_item_id: record.work_item_id.clone(),
            summary: None,
            detail: Some(serde_json::json!({
                "rejoin_obligation_id": record.task_id,
                "rejoin_generation": record.rejoin_generation,
                "parent_turn_id": record.parent_turn_id,
            })),
            recovery: None,
        })
        .unwrap();
    db.transaction(|tx| {
        assert!(upsert_pending_tx(tx, record)?);
        Ok(())
    })
    .unwrap();
}

#[test]
fn unsettled_for_owner_distinguishes_null_work_item_owner() {
    let (_dir, db) = runtime_db();
    let mut agent_scoped = pending_record(0);
    agent_scoped.work_item_id = None;
    insert(&db, &agent_scoped);
    insert(&db, &pending_record(1));

    let agent_records = db
        .task_result_settlements()
        .unsettled_for_owner("agent-a", None, 8)
        .unwrap();
    assert_eq!(agent_records.len(), 1);
    assert_eq!(agent_records[0].result_identity, "result-00");

    let work_item_records = db
        .task_result_settlements()
        .unsettled_for_owner("agent-a", Some("work-a"), 8)
        .unwrap();
    assert_eq!(work_item_records.len(), 1);
    assert_eq!(work_item_records[0].result_identity, "result-01");
}

#[test]
fn admission_is_bounded_and_leaves_overflow_pending() {
    let (_dir, db) = runtime_db();
    for index in 0..(TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT + 2) {
        insert(&db, &pending_record(index));
    }

    let admitted = db
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-1", Utc::now())
        .unwrap();
    assert_eq!(admitted.len(), TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT);
    assert_eq!(admitted.first().unwrap().task_id, "task-00");
    assert_eq!(admitted.last().unwrap().task_id, "task-07");
    assert_eq!(
        db.task_result_settlements()
            .settle_activation(
                "agent-a",
                "activation-1",
                TaskResultSettlementDisposition::ModelDelivered,
                Utc::now(),
            )
            .unwrap(),
        TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT
    );

    let overflow = db
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-2", Utc::now())
        .unwrap();
    assert_eq!(overflow.len(), 2);
    assert_eq!(overflow[0].task_id, "task-08");
    assert_eq!(overflow[1].task_id, "task-09");
}

#[test]
fn admitted_result_can_be_rebound_after_restart() {
    let dir = tempdir().unwrap();
    let database_path = dir.path().join("state/runtime.sqlite");
    let lock_path = dir.path().join("state/runtime.lock");
    {
        let db = RuntimeDb::open_and_migrate(&database_path, &lock_path).unwrap();
        insert(&db, &pending_record(0));
        let admitted = db
            .task_result_settlements()
            .admit_unsettled("agent-a", Some("work-a"), "activation-before", Utc::now())
            .unwrap();
        assert_eq!(admitted.len(), 1);
    }

    let reopened = RuntimeDb::open_and_migrate(&database_path, &lock_path).unwrap();
    let rebound = reopened
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-after", Utc::now())
        .unwrap();
    assert_eq!(rebound.len(), 1);
    assert_eq!(
        rebound[0].activation_id.as_deref(),
        Some("activation-after")
    );
}

#[test]
fn mark_deferred_preserves_admission_state_on_recheck_rewrites() {
    let (_dir, db) = runtime_db();
    insert(&db, &pending_record(0));
    let admitted = db
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-1", Utc::now())
        .unwrap();
    assert_eq!(admitted.len(), 1);

    // The recovery loop re-defers due rows on every recheck interval. The
    // narrowed defer UPDATE must leave admission columns untouched so a
    // recheck rewrite cannot silently drop the record back to pending.
    let now = Utc::now();
    let deferred = db
        .task_result_settlements()
        .mark_deferred(
            "message-00",
            "owner_has_unresolved_wait",
            now,
            now + Duration::seconds(30),
        )
        .unwrap()
        .expect("settlement row exists");
    assert_eq!(deferred.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(deferred.activation_id.as_deref(), Some("activation-1"));
    assert_eq!(
        deferred.deferred_reason.as_deref(),
        Some("owner_has_unresolved_wait")
    );
    assert_eq!(deferred.deferred_at, Some(now));
    assert_eq!(deferred.next_recheck_at, Some(now + Duration::seconds(30)));

    let reread = db
        .task_result_settlements()
        .latest_for_message("message-00")
        .unwrap()
        .expect("settlement row exists");
    assert_eq!(reread.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(reread.activation_id.as_deref(), Some("activation-1"));
    assert_eq!(reread.admitted_at, admitted[0].admitted_at);
    assert_eq!(reread.disposition, None);
    assert_eq!(
        reread.deferred_reason.as_deref(),
        Some("owner_has_unresolved_wait")
    );
    assert_eq!(reread.deferred_at, Some(now));
    assert_eq!(reread.next_recheck_at, Some(now + Duration::seconds(30)));
    assert_eq!(reread, deferred);
}

#[test]
fn waking_deferred_owner_rechecks_all_unsettled_results_for_that_owner() {
    let (_dir, db) = runtime_db();
    let first = pending_record(0);
    let mut other_owner = pending_record(1);
    other_owner.work_item_id = Some("work-other".into());
    insert(&db, &first);
    insert(&db, &other_owner);

    let deferred_at = Utc::now();
    let next_recheck = deferred_at + Duration::minutes(5);
    db.task_result_settlements()
        .mark_deferred(
            &first.message_id,
            "owner_not_runnable",
            deferred_at,
            next_recheck,
        )
        .unwrap();
    db.task_result_settlements()
        .mark_deferred(
            &other_owner.message_id,
            "owner_not_runnable",
            deferred_at,
            next_recheck,
        )
        .unwrap();

    let wake_at = deferred_at + Duration::minutes(1);
    assert_eq!(
        db.task_result_settlements()
            .wake_deferred_for_owner("agent-a", "work-a", wake_at)
            .unwrap(),
        1
    );
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&first.message_id)
            .unwrap()
            .unwrap()
            .next_recheck_at,
        Some(wake_at)
    );
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&other_owner.message_id)
            .unwrap()
            .unwrap()
            .next_recheck_at,
        Some(next_recheck)
    );
}

#[test]
fn active_admission_is_not_stolen_by_another_activation() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    insert(&db, &record);
    let admitted = db
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-active", Utc::now())
        .unwrap();
    assert_eq!(admitted.len(), 1);
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state, source_identity_json,
               source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, 'open', '{}', 1, NULL, NULL, '{}')",
            ["agent-a", "activation-active"],
        )?;
        Ok(())
    })
    .unwrap();

    assert!(db
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-other", Utc::now())
        .unwrap()
        .is_empty());
    let latest = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.activation_id.as_deref(), Some("activation-active"));
}

#[test]
fn recovery_deadline_excludes_open_attempt_but_recovers_closed_or_missing_attempt() {
    let (_dir, db) = runtime_db();
    let now = Utc.timestamp_opt(1_700_001_000, 0).single().unwrap();

    for index in 0..3 {
        let mut record = pending_record(index);
        record.next_recheck_at = Some(now - Duration::seconds(1));
        insert(&db, &record);
        db.task_result_settlements()
            .admit_message(
                "agent-a",
                &record.message_id,
                &format!("activation-{index}"),
                now - Duration::seconds(30),
            )
            .unwrap();
    }
    db.transaction(|tx| {
        for (attempt_id, lifecycle_state) in
            [("activation-0", "open"), ("activation-1", "interrupted")]
        {
            tx.execute(
                "INSERT INTO execution_protocol_attempts (
                   agent_id, attempt_id, lifecycle_state, source_identity_json,
                   source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json
                 ) VALUES (?1, ?2, ?3, '{}', 1, NULL, NULL, '{}')",
                params!["agent-a", attempt_id, lifecycle_state],
            )?;
        }
        Ok(())
    })
    .unwrap();

    let due = db
        .task_result_settlements()
        .due_deferred("agent-a", now, 8)
        .unwrap();
    assert_eq!(
        due.iter()
            .map(|record| record.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["message-01", "message-02"]
    );
    assert_eq!(
        db.task_result_settlements()
            .next_recheck_at("agent-a")
            .unwrap(),
        Some(now - Duration::seconds(1))
    );

    db.transaction(|tx| {
        tx.execute(
            "UPDATE execution_protocol_attempts
             SET lifecycle_state = 'interrupted'
             WHERE agent_id = 'agent-a' AND attempt_id = 'activation-0'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        db.task_result_settlements()
            .due_deferred("agent-a", now, 8)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn settlement_rolls_back_with_its_enclosing_terminal_transaction() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    insert(&db, &record);
    db.task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-rollback", Utc::now())
        .unwrap();

    let result: Result<()> = db.transaction(|tx| {
        assert_eq!(
            settle_activation_tx(
                tx,
                "agent-a",
                "activation-rollback",
                TaskResultSettlementDisposition::ModelDelivered,
                Utc::now(),
            )?,
            1
        );
        bail!("inject terminal transaction failure")
    });
    assert!(result.is_err());
    let latest = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.state, TaskResultSettlementState::CallerAdmitted);
    assert_eq!(latest.disposition, None);
}

#[test]
fn duplicate_result_does_not_resurrect_owner_unavailable_settlement() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    insert(&db, &record);
    assert!(db
        .task_result_settlements()
        .settle_owner_unavailable(
            &record.message_id,
            TaskResultSettlementDisposition::OwnerClosed,
            Utc::now(),
        )
        .unwrap());
    assert!(!db
        .task_result_settlements()
        .settle_owner_unavailable(
            &record.message_id,
            TaskResultSettlementDisposition::OwnerClosed,
            Utc::now(),
        )
        .unwrap());
    db.transaction(|tx| {
        assert!(!upsert_pending_tx(tx, &record)?);
        Ok(())
    })
    .unwrap();

    let latest = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.state, TaskResultSettlementState::Settled);
    assert_eq!(
        latest.disposition,
        Some(TaskResultSettlementDisposition::OwnerClosed)
    );
}

#[test]
fn stale_generation_is_settled_instead_of_admitted() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    insert(&db, &record);
    let mut reused = db.tasks().latest(&record.task_id).unwrap().unwrap();
    reused.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    reused.parent_message_id = Some("message-new".into());
    db.transaction(|tx| {
        tx.execute(
            "UPDATE tasks SET payload_json = ?1, last_message_id = ?2 WHERE task_id = ?3",
            params![
                serde_json::to_string(&reused)?,
                reused.parent_message_id,
                reused.id,
            ],
        )?;
        Ok(())
    })
    .unwrap();

    assert!(db
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "activation-stale", Utc::now())
        .unwrap()
        .is_empty());
    let latest = db
        .task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.state, TaskResultSettlementState::Settled);
    assert_eq!(
        latest.disposition,
        Some(TaskResultSettlementDisposition::InvalidOrStale)
    );
}

fn reply_result_fixture(db: &RuntimeDb, record: &TaskResultSettlementRecord) -> MessageEnvelope {
    use crate::types::{
        AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
        MessageOrigin, Priority, TaskKind,
    };
    insert_with_kind(db, record, TaskKind::AgentMessageWait);
    let mut message = MessageEnvelope::new(
        "agent-a",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: record.task_id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "peer output".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.id = record.message_id.clone();
    message.task_id = Some(record.task_id.clone());
    message.metadata = Some(serde_json::json!({
        "task_id": record.task_id, "task_kind":"agent_message_wait", "task_status":"completed",
        "task_detail":{"message_id":"original-reply"},
    }));
    message
}

#[test]
fn legacy_copied_reply_result_remains_admissible() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    let message = reply_result_fixture(&db, &record);
    assert!(crate::wake_contract::agent_message_reply_reference(&message).is_none());
    db.messages().upsert(&message).unwrap();
    let admitted = db
        .task_result_settlements()
        .admit_unsettled("agent-a", Some("work-a"), "legacy-activation", Utc::now())
        .unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(
        admitted[0].activation_id.as_deref(),
        Some("legacy-activation")
    );
}

#[test]
fn reply_observations_wait_for_original_admission_instead_of_unrelated_owner_activation() {
    let (_dir, db) = runtime_db();
    let record = pending_record(0);
    let mut message = reply_result_fixture(&db, &record);
    message.metadata.as_mut().unwrap()["task_detail"]["reply_content_source"] =
        serde_json::json!("original_message");
    assert_eq!(
        crate::wake_contract::agent_message_reply_reference(&message),
        Some("original-reply")
    );
    db.messages().upsert(&message).unwrap();
    assert!(db
        .task_result_settlements()
        .admit_unsettled(
            "agent-a",
            Some("work-a"),
            "unrelated-activation",
            Utc::now()
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap()
            .state,
        TaskResultSettlementState::PersistedPending
    );
    db.task_result_settlements()
        .admit_reply_message(
            "agent-a",
            Some("work-a"),
            "original-reply",
            "reply-activation",
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        db.task_result_settlements()
            .latest_for_message(&record.message_id)
            .unwrap()
            .unwrap()
            .activation_id
            .as_deref(),
        Some("reply-activation")
    );
}
