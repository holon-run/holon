//! Real original repository kernels; the existing Core tests cover whole ACK/finalizers.
use super::*;
use crate::runtime_db::{transitions::TransitionFaultPoint, TimerCancel, TimerFire};

fn fire(db: &RuntimeDb, id: &str, message_id: &str) -> Result<TimerFire> {
    let expected = db.timers().latest(id)?.context("fixture Timer")?;
    let mut record = expected.clone();
    record.fire_count += 1;
    record.last_fired_at = Some(at() + chrono::Duration::seconds(1));
    let mut message = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "real original wake".into(),
        },
    );
    message.id = message_id.into();
    message.created_at = at();
    message.metadata =
        Some(serde_json::json!({"timer_id":id,"timer_fire_count":record.fire_count}));
    let expected_state = db.transaction(|tx| {
        crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope().agent_id)
            .scope_facts()?
            .state
            .context("fixture State")
    })?;
    let mut record_state = expected_state.clone();
    record_state.pending += 1;
    record_state.total_message_count += 1;
    let queue_entry = QueueEntryRecord {
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
        queue_entry,
        agent_state: (expected_state, record_state),
    })
}
fn saved(db: &RuntimeDb) -> Result<Vec<String>> {
    db.transaction(|tx| {
        [
            "timers",
            "timer_wakes",
            "messages",
            "queue_entries",
            "agent_states",
            "audit_events",
            "runtime_index_outbox",
        ]
        .into_iter()
        .map(|table| {
            let mut statement = tx.prepare(&format!("SELECT * FROM {table} ORDER BY 1"))?;
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    let mut value = Vec::new();
                    for col in 0..columns {
                        value.push(format!("{:?}", row.get_ref(col)?));
                    }
                    Ok(value.join("|"))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(format!("{table}:{rows:?}"))
        })
        .collect()
    })
}
#[test]
fn borrowed_timer_fire_and_cancel_roll_back_with_the_actual_outer_transaction() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("kernel", TimerStatus::Active))
    })?;
    let command = fire(&db, "kernel", "kernel-message")?;
    let original = saved(&db)?;
    let result: Result<()> = db.transaction(|tx| {
        let ack = db.timers().fire_with_wait_tx(
            tx,
            &command,
            Some(TransitionFaultPoint::AfterCanonicalWrites),
            None,
            true,
            &mut repositories::WaitConditionWrites::default(),
            &mut Vec::new(),
            None,
        )?;
        assert!(ack.record.wake_created);
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(saved(&db)?, original);
    let ack = db.timers().fire(&command)?;
    assert!(ack.advanced && ack.wake_created);
    let expected = db.timers().latest("kernel")?.unwrap();
    let mut record = expected.clone();
    record.status = TimerStatus::Cancelled;
    let state = db.transaction(|tx| {
        crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope().agent_id)
            .scope_facts()?
            .state
            .context("fixture State")
    })?;
    let mut after = state.clone();
    after.pending -= 1;
    let cancel = TimerCancel {
        expected,
        record,
        agent_state: Some((state, after)),
    };
    let before_cancel = saved(&db)?;
    let result: Result<()> = db.transaction(|tx| {
        let ack = db.timers().cancel_tx(tx, &cancel)?;
        assert!(ack.cancelled && ack.dropped_message_id.as_deref() == Some("kernel-message"));
        Err(anyhow::anyhow!("fixture outer transaction rollback"))
    });
    assert!(result.is_err());
    assert_eq!(saved(&db)?, before_cancel);
    let ack = db.timers().cancel(&cancel)?;
    assert!(ack.cancelled);
    assert_eq!(
        db.timers().latest("kernel")?.unwrap().status,
        TimerStatus::Cancelled
    );
    assert_eq!(
        db.queue_entries().latest("kernel-message")?.unwrap().status,
        QueueEntryStatus::Dropped
    );
    Ok(())
}
#[test]
fn original_timer_merge_kernel_keeps_existing_pending_identity_and_no_new_source() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("merge-kernel", TimerStatus::Active))?;
        wake(tx, "merge-kernel", "original-pending", 5)
    })?;
    let baseline: (usize, usize) = db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(baseline, (1, 1)); // The real legacy wake owns its original source.
    let command = fire(&db, "merge-kernel", "must-not-enqueue")?;
    let budget = ledger()?;
    let observed =
        db.timers()
            .pending_wake_for_writer(&scope(), "merge-kernel", &request(), &budget)?;
    assert_eq!(
        observed.record.as_ref().unwrap().message_id,
        "original-pending"
    );
    let state = command.agent_state.0.clone();
    let mut waits = repositories::WaitConditionWrites::default();
    let mut audits = Vec::new();
    let ack = db.transaction(|tx| {
        db.timers().fire_with_wait_tx(
            tx,
            &command,
            None,
            None,
            true,
            &mut waits,
            &mut audits,
            None,
        )
    })?;
    assert!(ack.record.advanced && !ack.record.wake_created && ack.record.message.is_none());
    assert!(waits.wait_ids.is_empty() && audits.is_empty());
    assert_eq!(
        db.timers()
            .pending_wake("merge-kernel")?
            .unwrap()
            .message_id,
        "original-pending"
    );
    let actual = db.transaction(|tx| {
        crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope().agent_id)
            .scope_facts()?
            .state
            .context("fixture State")
    })?;
    assert_eq!(actual, state);
    let counts: (usize, usize) = db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(counts, baseline);
    assert!(budget.report()?.observed_bytes > 0);
    drop((ack, observed));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn pending_timer_observation_keeps_global_none_and_checks_normalized_owner_before_decode(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("owner-check", TimerStatus::Active))?;
        wake(tx, "owner-check", "owned-wake", 5)
    })?;
    let budget = ledger()?;
    let absent =
        db.timers()
            .pending_wake_for_writer(&scope(), "globally-missing", &request(), &budget)?;
    assert!(absent.record.is_none());
    drop(absent);
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.connection()?.execute(
        "UPDATE timers SET agent_id='foreign' WHERE timer_id='owner-check'",
        [],
    )?;
    db.connection()?.execute(
        "UPDATE timer_wakes SET updated_at='malformed-time' WHERE timer_id='owner-check'",
        [],
    )?;
    let error = db
        .timers()
        .pending_wake_for_writer(&scope(), "owner-check", &request(), &budget)
        .unwrap_err();
    assert!(format!("{error:#}").contains("owner differs"), "{error:#}");
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
