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
    db.tasks()
        .upsert(&TaskRecord {
            id: record.task_id.clone(),
            agent_id: record.agent_id.clone(),
            kind: crate::types::TaskKind::CommandTask,
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
