use super::*;
use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
    MessageOrigin, Priority, TaskKind, TaskRecoverySpec, TaskStatus, WaitConditionKind,
    WaitConditionRecord, WaitConditionStatus, WakeSource,
};
use chrono::TimeZone;
use std::collections::BTreeSet;
use tempfile::tempdir;

fn fixture() -> (tempfile::TempDir, RuntimeDb) {
    let dir = tempdir().unwrap();
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("state/runtime.sqlite"),
        dir.path().join("state/runtime.lock"),
    )
    .unwrap();
    (dir, db)
}
fn at() -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).single().unwrap()
}
fn record(index: usize) -> TaskResultSettlementRecord {
    let now = at() + Duration::seconds(index as i64);
    TaskResultSettlementRecord {
        result_identity: format!("footprint-result-{index:02}"),
        agent_id: "agent-a".into(),
        task_id: format!("footprint-task-{index:02}"),
        message_id: format!("footprint-message-{index:02}"),
        work_item_id: Some("work-a".into()),
        rejoin_generation: 1,
        source: crate::domain::execution_protocol::TaskRejoinSource::ParentTurn {
            parent_turn_id: "turn-parent".into(),
        },
        task_status: "completed".into(),
        state: TaskResultSettlementState::PersistedPending,
        activation_id: None,
        disposition: None,
        created_at: now,
        updated_at: now,
        admitted_at: None,
        settled_at: None,
        deferred_reason: None,
        deferred_at: None,
        next_recheck_at: Some(now + Duration::seconds(30)),
        recovery_epoch: 0,
        recovery_attempts: 0,
        last_recovery_root_attempt_id: None,
        last_recovery_admitted_at: None,
        recovery_paused_at: None,
        recovery_diagnostic_at: None,
        source_projection: None,
        reply_projection: None,
    }
}
fn task(record: &TaskResultSettlementRecord) -> TaskRecord {
    TaskRecord {
        id: record.task_id.clone(),
        agent_id: record.agent_id.clone(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: record.created_at,
        updated_at: record.updated_at,
        parent_message_id: Some(record.message_id.clone()),
        work_item_id: record.work_item_id.clone(),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id":record.task_id,
            "rejoin_generation":record.rejoin_generation,
            "parent_turn_id":record.source.parent_turn_id().unwrap(),
        })),
        recovery: None,
    }
}
fn insert(db: &RuntimeDb, record: &TaskResultSettlementRecord) {
    db.transaction(|tx| {
        super::super::repositories::upsert_task_tx(tx, &task(record))?;
        assert!(upsert_pending_tx(tx, record)?);
        Ok(())
    })
    .unwrap();
}
fn open_activation(tx: &Transaction<'_>, id: &str) -> Result<()> {
    // This leaf only observes the original SQL Open predicate. It does not
    // claim that this row is an execution/physical-completion proof.
    tx.execute(
        "INSERT INTO execution_protocol_attempts (
          agent_id,attempt_id,lifecycle_state,source_identity_json,source_generation,
          recovery_of_attempt_id,terminal_outcome_id,payload_json
        ) VALUES ('agent-a',?1,'open','{}',1,NULL,NULL,'{}')",
        [id],
    )?;
    Ok(())
}
fn result_message(record: &TaskResultSettlementRecord) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        &record.agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: record.task_id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "done".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.id = record.message_id.clone();
    message.work_item_id = record.work_item_id.clone();
    message.task_id = Some(record.task_id.clone());
    message
}
fn stored(db: &RuntimeDb, record: &TaskResultSettlementRecord) -> TaskResultSettlementRecord {
    db.task_result_settlements()
        .latest_for_message(&record.message_id)
        .unwrap()
        .unwrap()
}

#[test]
fn selected_owner_batch_accounts_for_invalid_rows_absent_from_admitted_value() {
    let (_dir, db) = fixture();
    for i in 0..10 {
        insert(&db, &record(i));
    }
    // A legitimate later Task generation makes the original result stale.
    db.transaction(|tx| {
        let mut next = task(&record(0));
        next.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
        next.parent_message_id = Some("next-generation-result".into());
        tx.execute(
            "UPDATE tasks SET payload_json=?1,last_message_id=?2 WHERE task_id=?3",
            params![
                serde_json::to_string(&next)?,
                next.parent_message_id,
                next.id
            ],
        )?;
        Ok(())
    })
    .unwrap();
    db.transaction(|tx| {
        let selection = select_admit_unsettled_tx(
            tx,
            "agent-a",
            Some("work-a"),
            "activation-1",
            at() + Duration::hours(1),
            TaskResultWriteLimits::default(),
        )?;
        assert_eq!(selection.material_estimate().records, 16); // eight results + eight Tasks
        let prepared = selection.materialize(TaskResultWriteLimits::default())?;
        assert_eq!(prepared.candidate_result_ids().len(), 8);
        assert_eq!(prepared.dependency_task_ids().len(), 8);
        let ack = prepared.execute()?;
        assert_eq!(
            ack.actual_result_ids,
            (0..8).map(|i| record(i).result_identity).collect()
        );
        let admitted = ack.into_records()?;
        assert_eq!(
            admitted
                .iter()
                .map(|x| x.task_id.clone())
                .collect::<Vec<_>>(),
            (1..8).map(|i| record(i).task_id).collect::<Vec<_>>()
        );
        // Admission retains the original recheck deadline; it only clears defer fields.
        assert_eq!(admitted[0].next_recheck_at, record(1).next_recheck_at);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        stored(&db, &record(0)).disposition,
        Some(TaskResultSettlementDisposition::InvalidOrStale)
    );
    assert_eq!(
        stored(&db, &record(8)).state,
        TaskResultSettlementState::PersistedPending
    );
}

#[test]
fn owner_readmission_preserves_first_admitted_at_and_original_result_binding() {
    let (_dir, db) = fixture();
    let original = record(0);
    insert(&db, &original);
    let first_at = at() + Duration::seconds(1);
    let next_at = first_at + Duration::seconds(1);
    db.transaction(|tx| {
        let first = select_admit_unsettled_tx(
            tx,
            "agent-a",
            Some("work-a"),
            "first-activation",
            first_at,
            TaskResultWriteLimits::default(),
        )?
        .materialize(TaskResultWriteLimits::default())?
        .execute()?
        .into_records()?;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].admitted_at, Some(first_at));
        let next = select_admit_unsettled_tx(
            tx,
            "agent-a",
            Some("work-a"),
            "next-activation",
            next_at,
            TaskResultWriteLimits::default(),
        )?
        .materialize(TaskResultWriteLimits::default())?
        .execute()?
        .into_records()?;
        assert_eq!(next.len(), 1);
        let mut expected = first[0].clone();
        expected.activation_id = Some("next-activation".into());
        expected.updated_at = next_at;
        assert_eq!(next[0], expected);
        Ok(())
    })
    .unwrap();
    assert_eq!(stored(&db, &original).admitted_at, Some(first_at));
}

#[test]
fn single_message_none_and_same_activation_do_not_hide_actual_write_semantics() {
    let (_dir, db) = fixture();
    let valid = record(0);
    let invalid = record(1);
    insert(&db, &valid);
    insert(&db, &invalid);
    db.transaction(|tx| {
        tx.execute("DELETE FROM tasks WHERE task_id=?1", [&invalid.task_id])?;
        let ack = prepare_admit_message_tx(
            tx,
            "agent-a",
            &invalid.message_id,
            "a",
            at(),
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert_eq!(
            ack.actual_result_ids,
            BTreeSet::from([invalid.result_identity.clone()])
        );
        assert!(ack.into_record()?.is_none());
        let first = prepare_admit_message_tx(
            tx,
            "agent-a",
            &valid.message_id,
            "a",
            at(),
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert_eq!(first.actual_result_ids.len(), 1);
        assert!(first.into_record()?.is_some());
        let same = prepare_admit_message_tx(
            tx,
            "agent-a",
            &valid.message_id,
            "a",
            at() + Duration::seconds(1),
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert!(same.actual_result_ids.is_empty());
        assert!(same.into_record()?.is_some());
        let other = prepare_admit_message_tx(
            tx,
            "agent-a",
            &valid.message_id,
            "other",
            at(),
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert!(other.actual_result_ids.is_empty());
        assert!(other.into_record()?.is_none());
        let settled = prepare_mark_deferred_tx(
            tx,
            &invalid.message_id,
            "ignored",
            at(),
            at() + Duration::seconds(5),
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert!(settled.actual_result_ids.is_empty());
        assert_eq!(
            settled.into_record()?.unwrap().state,
            TaskResultSettlementState::Settled
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn reply_selection_preserves_exact_resolved_wait_and_open_activation_filters() {
    let (_dir, db) = fixture();
    for i in 0..3 {
        let mut result = record(i);
        result.work_item_id = Some("other-work".into());
        let mut task = task(&result);
        task.kind = TaskKind::AgentMessageWait;
        db.tasks().upsert(&task).unwrap();
        db.transaction(|tx| {
            upsert_pending_tx(tx, &result)?;
            Ok(())
        })
        .unwrap();
        let mut message = result_message(&result);
        message.metadata = Some(
            serde_json::json!({"task_kind":"agent_message_wait","task_status":"completed","task_detail":{"message_id":"original-reply","reply_content_source":"original_message"}}),
        );
        db.messages().upsert(&message).unwrap();
    }
    db.transaction(|tx| {
        let mut wait = WaitConditionRecord {
            id: "reply-wait".into(),
            agent_id: "agent-a".into(),
            work_item_id: Some("work-a".into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: None,
            waiting_for: "exact reply tasks".into(),
            wake_sources: vec![
                WakeSource::TaskResult {
                    task_id: record(0).task_id,
                },
                WakeSource::TaskResult {
                    task_id: record(1).task_id,
                },
            ],
            continuation: None,
            created_at: at(),
            updated_at: at(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        super::super::repositories::upsert_wait_condition_tx(tx, &wait)?;
        wait.status = WaitConditionStatus::Resolved;
        wait.updated_at = at() + Duration::seconds(1);
        wait.resolved_at = Some(wait.updated_at);
        wait.trigger_message_id = Some("original-reply".into());
        super::super::repositories::upsert_wait_condition_tx(tx, &wait)?;
        let mut held = latest_for_message_tx(tx, &record(1).message_id)?.unwrap();
        held.state = TaskResultSettlementState::CallerAdmitted;
        held.activation_id = Some("open-holder".into());
        held.admitted_at = Some(at());
        update_tx(tx, &held)?;
        open_activation(tx, "open-holder")?;
        let selection = select_admit_reply_message_tx(
            tx,
            "agent-a",
            Some("work-a"),
            "original-reply",
            "reply-activation",
            at() + Duration::seconds(2),
            TaskResultWriteLimits::default(),
        )?;
        let prepared = selection.materialize(TaskResultWriteLimits::default())?;
        assert_eq!(
            prepared.candidate_result_ids(),
            &BTreeSet::from([record(0).result_identity, record(1).result_identity])
        );
        let ack = prepared.execute()?;
        assert_eq!(
            ack.actual_result_ids,
            BTreeSet::from([record(0).result_identity])
        );
        ack.into_unit()?;
        // Original-message observations remain excluded from the owner retry batch.
        let owner = select_admit_unsettled_tx(
            tx,
            "agent-a",
            Some("other-work"),
            "unrelated",
            at(),
            TaskResultWriteLimits::default(),
        )?
        .materialize(TaskResultWriteLimits::default())?;
        assert!(owner.candidate_result_ids().is_empty());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        stored(&db, &record(1)).activation_id.as_deref(),
        Some("open-holder")
    );
    assert_eq!(
        stored(&db, &record(2)).state,
        TaskResultSettlementState::PersistedPending
    );
}

#[test]
fn wake_and_activation_settlement_include_open_results_and_roll_back_together() {
    let (_dir, db) = fixture();
    for i in 0..3 {
        insert(&db, &record(i));
    }
    db.transaction(|tx| {
        for i in 0..3 {
            prepare_admit_message_tx(
                tx,
                "agent-a",
                &record(i).message_id,
                "open-holder",
                at(),
                TaskResultWriteLimits::default(),
            )?
            .execute()?;
        }
        open_activation(tx, "open-holder")?;
        let ack = select_wake_deferred_for_owner_tx(
            tx,
            "agent-a",
            "work-a",
            at() + Duration::hours(1),
            TaskResultWriteLimits::default(),
        )?
        .materialize(TaskResultWriteLimits::default())?
        .execute()?;
        assert_eq!(ack.actual_result_ids.len(), 3);
        assert_eq!(ack.into_count()?, 3);
        Ok(())
    })
    .unwrap();
    let before: Vec<_> = (0..3).map(|i| stored(&db, &record(i))).collect();
    db.transaction(|tx| { tx.execute_batch("CREATE TRIGGER reject_second_result BEFORE UPDATE ON task_result_settlements WHEN NEW.result_identity='footprint-result-01' AND NEW.state='settled' BEGIN SELECT RAISE(ABORT,'forced result failure'); END;")?;Ok(()) }).unwrap();
    let error = db
        .transaction(|tx| {
            let prepared = select_settle_activation_tx(
                tx,
                "agent-a",
                "open-holder",
                TaskResultSettlementDisposition::ModelDelivered,
                at() + Duration::hours(2),
                TaskResultWriteLimits::default(),
            )?
            .materialize(TaskResultWriteLimits::default())?;
            assert_eq!(prepared.candidate_result_ids().len(), 3);
            prepared.execute()?.into_count()
        })
        .unwrap_err();
    assert!(error.to_string().contains("forced result failure"));
    assert_eq!(
        before,
        (0..3).map(|i| stored(&db, &record(i))).collect::<Vec<_>>()
    );
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_second_result")?;
        let ack = prepare_settle_activation_tx(
            tx,
            "agent-a",
            "open-holder",
            TaskResultSettlementDisposition::ModelDelivered,
            at() + Duration::hours(2),
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert_eq!(ack.actual_result_ids.len(), 3);
        assert_eq!(ack.into_count()?, 3);
        for index in 0..3 {
            let settled = latest_for_message_tx(tx, &record(index).message_id)?.unwrap();
            assert_eq!(settled.next_recheck_at, Some(at() + Duration::hours(1)));
        }
        Ok(())
    })
    .unwrap();
}

fn insert_shared_reply_result(db: &RuntimeDb, record: &TaskResultSettlementRecord, reply: &str) {
    let mut task = task(record);
    task.kind = TaskKind::AgentMessageWait;
    db.tasks().upsert(&task).unwrap();
    let mut message = result_message(record);
    message.metadata = Some(serde_json::json!({
        "task_kind": "agent_message_wait", "task_status": "completed",
        "task_detail": {"message_id": reply, "reply_content_source": "original_message"}
    }));
    db.messages().upsert(&message).unwrap();
    db.transaction(|tx| {
        assert!(upsert_pending_tx(tx, record)?);
        // Pending insertion initializes recovery columns; transitions update them.
        assert!(update_tx(tx, record)?);
        Ok(())
    })
    .unwrap();
}

fn delivered_reply_batch_record() -> TaskResultSettlementRecord {
    let mut result = record(0);
    result.state = TaskResultSettlementState::CallerAdmitted;
    result.activation_id = Some("reply-batch".into());
    result.admitted_at = Some(at());
    result.reply_projection = Some(IdleInputBatchItem {
        message_id: "shared-reply".into(),
        source_ref: "message:shared-reply".into(),
        rendered: "original reply".into(),
    });
    result
}

#[test]
fn shared_reply_batch_progress_wakes_only_matching_untried_remainders() {
    let (_dir, db) = fixture();
    let now = at() + Duration::seconds(10);
    let original = delivered_reply_batch_record();
    insert_shared_reply_result(&db, &original, "shared-reply");
    let mut excluded = Vec::new();
    for index in 1..=14 {
        let mut result = record(index);
        match index {
            2 => result.work_item_id = Some("other-work".into()),
            3 => result.agent_id = "other-agent".into(),
            5 => result.recovery_paused_at = Some(at()),
            6 => result.recovery_epoch = 1,
            7 => result.recovery_attempts = 1,
            8 => {
                result.state = TaskResultSettlementState::CallerAdmitted;
                result.activation_id = Some("other-batch".into());
                result.admitted_at = Some(at());
            }
            9 => {
                result.state = TaskResultSettlementState::Settled;
                result.disposition = Some(TaskResultSettlementDisposition::ModelDelivered);
                result.settled_at = Some(at());
            }
            10 => result.next_recheck_at = None,
            11 => result.next_recheck_at = Some(at()),
            12 => result.work_item_id = None,
            _ => {}
        }
        insert_shared_reply_result(
            &db,
            &result,
            if index == 4 {
                "other-reply"
            } else {
                "shared-reply"
            },
        );
        if index >= 13 {
            let mut message = result_message(&result);
            message.metadata = Some(serde_json::json!({
                "task_kind": if index == 13 { "command_task" } else { "agent_message_wait" },
                "task_status": "completed",
                "task_detail": {"message_id": "shared-reply", "reply_content_source": "inline"}
            }));
            db.messages().upsert(&message).unwrap();
        }
        if index != 1 {
            excluded.push(stored(&db, &result));
        }
    }
    db.transaction(|tx| {
        let prepared = prepare_settle_activation_tx(
            tx,
            "agent-a",
            "reply-batch",
            TaskResultSettlementDisposition::ModelDelivered,
            now,
            TaskResultWriteLimits::default(),
        )?;
        assert_eq!(prepared.candidate_result_ids().len(), 2);
        let ack = prepared.execute()?;
        assert_eq!(ack.actual_result_ids.len(), 2);
        assert_eq!(ack.into_count()?, 1, "probe progress is not delivery");
        Ok(())
    })
    .unwrap();
    assert_eq!(
        stored(&db, &original).state,
        TaskResultSettlementState::Settled
    );
    let mut expected = record(1);
    expected.next_recheck_at = Some(now);
    expected.updated_at = now;
    assert_eq!(stored(&db, &expected), expected);
    for result in excluded {
        assert_eq!(stored(&db, &result), result);
    }
}

#[test]
fn shared_reply_batch_progress_is_bounded_atomic_and_not_replayed() {
    let (_dir, db) = fixture();
    let original = delivered_reply_batch_record();
    insert_shared_reply_result(&db, &original, "shared-reply");
    for index in 1..=70 {
        insert_shared_reply_result(&db, &record(index), "shared-reply");
    }
    let before: Vec<_> = (0..=70).map(|i| stored(&db, &record(i))).collect();
    let now = at() + Duration::seconds(10);
    db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_reply_progress BEFORE UPDATE ON task_result_settlements
             WHEN NEW.result_identity='footprint-result-64'
             BEGIN SELECT RAISE(ABORT,'forced reply progress failure'); END;",
        )?;
        Ok(())
    })
    .unwrap();
    let error = db
        .transaction(|tx| {
            prepare_settle_activation_tx(
                tx,
                "agent-a",
                "reply-batch",
                TaskResultSettlementDisposition::ModelDelivered,
                now,
                TaskResultWriteLimits::default(),
            )?
            .execute()?
            .into_count()
        })
        .unwrap_err();
    assert!(error.to_string().contains("forced reply progress failure"));
    assert_eq!(
        before,
        (0..=70)
            .map(|i| stored(&db, &record(i)))
            .collect::<Vec<_>>()
    );
    db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_reply_progress")?;
        let prepared = prepare_settle_activation_tx(
            tx,
            "agent-a",
            "reply-batch",
            TaskResultSettlementDisposition::ModelDelivered,
            now,
            TaskResultWriteLimits::default(),
        )?;
        assert_eq!(prepared.candidate_result_ids().len(), 65);
        let ack = prepared.execute()?;
        assert_eq!(ack.actual_result_ids.len(), 65);
        assert_eq!(ack.into_count()?, 1);
        let repeated = prepare_settle_activation_tx(
            tx,
            "agent-a",
            "reply-batch",
            TaskResultSettlementDisposition::ModelDelivered,
            now,
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert!(repeated.actual_result_ids.is_empty());
        assert_eq!(repeated.into_count()?, 0);
        Ok(())
    })
    .unwrap();
    for index in 1..=70 {
        let mut expected = record(index);
        if index <= 64 {
            expected.next_recheck_at = Some(now);
            expected.updated_at = now;
        }
        assert_eq!(stored(&db, &expected), expected);
    }
}

#[test]
fn shared_reply_batch_progress_requires_delivery_and_its_original_projection() {
    for owner in [None, Some("work-a".to_owned())] {
        for (disposition, has_projection, should_wake) in [
            (TaskResultSettlementDisposition::ModelDelivered, true, true),
            (TaskResultSettlementDisposition::OwnerClosed, true, false),
            (
                TaskResultSettlementDisposition::ModelDelivered,
                false,
                false,
            ),
        ] {
            let (_dir, db) = fixture();
            let mut original = delivered_reply_batch_record();
            original.work_item_id = owner.clone();
            if !has_projection {
                original.reply_projection = None;
            }
            insert_shared_reply_result(&db, &original, "shared-reply");
            let mut pending = record(1);
            pending.work_item_id = owner.clone();
            insert_shared_reply_result(&db, &pending, "shared-reply");
            let now = at() + Duration::seconds(10);
            db.transaction(|tx| {
                let ack = prepare_settle_activation_tx(
                    tx,
                    "agent-a",
                    "reply-batch",
                    disposition,
                    now,
                    TaskResultWriteLimits::default(),
                )?
                .execute()?;
                assert_eq!(ack.actual_result_ids.len(), 1 + usize::from(should_wake));
                assert_eq!(ack.into_count()?, 1);
                Ok(())
            })
            .unwrap();
            if should_wake {
                pending.next_recheck_at = Some(now);
                pending.updated_at = now;
            }
            assert_eq!(stored(&db, &pending), pending);
        }
    }
}

#[test]
fn new_task_pending_is_frozen_before_task_write_and_validates_actual_slim_task() {
    let (_dir, db) = fixture();
    let result = record(0);
    let mut command = task(&result);
    command.detail.as_mut().unwrap()["output_path"] = serde_json::json!("x".repeat(10_000));
    db.transaction(|tx| {
        let estimate = upsert_pending_material_estimate_tx(tx, &command, &result)?;
        assert!(estimate.payload_bytes >= 12 * 10_000);
        let prepared =
            prepare_upsert_pending_tx(tx, &command, &result, TaskResultWriteLimits::default())?;
        assert_eq!(
            prepared.candidate_result_ids(),
            &BTreeSet::from([result.result_identity.clone()])
        );
        assert!(!tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1)",
            [&command.id],
            |r| r.get::<_, bool>(0)
        )?);
        super::super::repositories::upsert_task_tx(tx, &command)?;
        let ack = prepared.execute()?;
        assert_eq!(ack.actual_result_ids.len(), 1);
        assert!(ack.into_changed()?);
        Ok(())
    })
    .unwrap();
    let different = record(1);
    let frozen = task(&different);
    let error = db
        .transaction(|tx| {
            let prepared = prepare_upsert_pending_tx(
                tx,
                &frozen,
                &different,
                TaskResultWriteLimits::default(),
            )?;
            let mut wrong = frozen.clone();
            wrong.summary = Some("different actual command".into());
            super::super::repositories::upsert_task_tx(tx, &wrong)?;
            prepared.execute()?;
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("actual Task differs"));
    assert!(db.tasks().latest(&different.task_id).unwrap().is_none());
    db.transaction(|tx| {tx.execute_batch("CREATE TRIGGER reject_new_pending BEFORE INSERT ON task_result_settlements BEGIN SELECT RAISE(ABORT,'forced pending failure'); END;")?;Ok(())}).unwrap();
    let error = db
        .transaction(|tx| {
            let prepared = prepare_upsert_pending_tx(
                tx,
                &frozen,
                &different,
                TaskResultWriteLimits::default(),
            )?;
            super::super::repositories::upsert_task_tx(tx, &frozen)?;
            prepared.execute()?;
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("forced pending failure"));
    assert!(db.tasks().latest(&different.task_id).unwrap().is_none());
    assert!(db
        .task_result_settlements()
        .latest_for_message(&different.message_id)
        .unwrap()
        .is_none());
}

#[test]
fn original_selected_result_and_ensure_task_cannot_change_before_execute() {
    let (_dir, db) = fixture();
    let original = record(0);
    insert(&db, &original);
    let error=db.transaction(|tx| {
        let prepared=prepare_admit_message_tx(tx,"agent-a",&original.message_id,"activation",at(),TaskResultWriteLimits::default())?;
        tx.execute("UPDATE task_result_settlements SET payload_json=json_set(payload_json,'$.updated_at','2024-01-01T00:00:00Z') WHERE result_identity=?1",[&original.result_identity])?;
        prepared.execute()?;Ok(())
    }).unwrap_err();
    assert!(error.to_string().contains("selection changed"));
    assert_eq!(stored(&db, &original), original);
    let message = result_message(&original);
    let error=db.transaction(|tx| {
        let prepared=prepare_ensure_pending_tx(tx,&original.task_id,&message,at(),TaskResultWriteLimits::default())?;
        tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.detail.rejoin_generation',2) WHERE task_id=?1",[&original.task_id])?;
        prepared.execute()?;Ok(())
    }).unwrap_err();
    assert!(error.to_string().contains("Task/fence changed"));
    assert_eq!(stored(&db, &original), original);
}

#[test]
fn lengths_only_preflight_counts_full_recovery_and_normalized_projection_before_decode() {
    let (_dir, db) = fixture();
    let original = record(0);
    let mut large = task(&original);
    large.kind = TaskKind::AgentMessageWait;
    large.detail.as_mut().unwrap()["input_target"] = serde_json::json!("p".repeat(80_000));
    large.recovery = Some(TaskRecoverySpec::AgentMessageWait {
        summary: "recovery".into(),
        message: "m".repeat(96_000),
        target_agent_id: "peer".into(),
        authority_class: AuthorityClass::RuntimeInstruction,
        delivery_id: None,
        after_delivery_rowid: None,
        reply_deadline_at: None,
        requester_incarnation: None,
    });
    db.tasks().upsert(&large).unwrap();
    db.transaction(|tx| {
        upsert_pending_tx(tx, &original)?;
        Ok(())
    })
    .unwrap();
    db.transaction(|tx| {
        let selection = select_admit_message_tx(
            tx,
            "agent-a",
            &original.message_id,
            "activation",
            at(),
            TaskResultWriteLimits::default(),
        )?;
        let estimate = selection.material_estimate();
        assert!(estimate.source_material_bytes > 176_000);
        assert_eq!(estimate.payload_bytes, 12 * estimate.source_material_bytes);
        assert_eq!(estimate.records, 2);
        let pending =
            ensure_pending_material_estimate_tx(tx, &original.task_id, &original.message_id)?;
        assert!(pending.source_material_bytes > 176_000);
        let error = match select_admit_message_tx(
            tx,
            "agent-a",
            &original.message_id,
            "activation",
            at(),
            TaskResultWriteLimits {
                record_limit: 10,
                byte_limit: 100_000,
            },
        ) {
            Ok(_) => panic!("oversize selection must fail before body decode"),
            Err(error) => error,
        };
        assert!(error
            .downcast_ref::<TaskResultFootprintExhausted>()
            .is_some());
        // Corrupt only for this negative fixture: metadata/length selection still
        // succeeds; typed body decode fails after caller would register its guard.
        tx.execute(
            "UPDATE task_result_settlements SET payload_json='not-json' WHERE result_identity=?1",
            [&original.result_identity],
        )?;
        let selected = select_admit_message_tx(
            tx,
            "agent-a",
            &original.message_id,
            "activation",
            at(),
            TaskResultWriteLimits::default(),
        )?;
        assert!(selected
            .materialize(TaskResultWriteLimits::default())
            .is_err());
        Ok(())
    })
    .unwrap();
    assert_eq!(stored_task_count(&db), 1);
}
fn stored_task_count(db: &RuntimeDb) -> usize {
    db.connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn ensure_pending_preserves_first_generation_message_and_successful_empty_footprint() {
    let (_dir, db) = fixture();
    let first = record(0);
    let mut original_task = task(&first);
    original_task.parent_message_id = None;
    db.tasks().upsert(&original_task).unwrap();
    db.transaction(|tx| {
        upsert_pending_tx(tx, &first)?;
        Ok(())
    })
    .unwrap();
    let mut other = result_message(&first);
    other.id = "another-message-same-task-generation".into();
    db.transaction(|tx| {
        let prepared = prepare_ensure_pending_tx(
            tx,
            &first.task_id,
            &other,
            at() + Duration::seconds(5),
            TaskResultWriteLimits::default(),
        )?;
        assert!(prepared.candidate_result_ids().is_empty());
        let ack = prepared.execute()?;
        assert!(ack.actual_result_ids.is_empty());
        assert!(ack.into_record()?.is_none());
        let missing = prepare_ensure_pending_tx(
            tx,
            "missing-task",
            &other,
            at(),
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert!(missing.actual_result_ids.is_empty());
        assert!(missing.into_record()?.is_none());
        Ok(())
    })
    .unwrap();
    assert_eq!(stored(&db, &first), first);
    assert!(db
        .task_result_settlements()
        .latest_for_message(&other.id)
        .unwrap()
        .is_none());
}
