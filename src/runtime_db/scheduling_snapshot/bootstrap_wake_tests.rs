use super::super::super::tests::{at, ledger, legacy, scope, timer, wake};
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime_db::evidence::{append_message_tx, upsert_agent_state_tx},
    types::*,
};
fn extra_message(tx: &Transaction<'_>, timer: &str, id: &str, count: u64) -> Result<()> {
    let mut message = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TimerTick,
        MessageOrigin::Timer {
            timer_id: timer.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "original queued timer".into(),
        },
    );
    message.id = id.into();
    message.created_at = at() + chrono::Duration::seconds(1);
    message.metadata = Some(serde_json::json!({"timer_fire_count":count}));
    let (message, _) = append_message_tx(tx, &message)?;
    repositories::upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: id.into(),
            agent_id: scope().agent_id,
            priority: message.priority,
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: message.created_at,
        },
    )?;
    let mut state = AgentState::new(&scope().agent_id);
    let count: u64 = tx.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
        [&scope().agent_id],
        |r| r.get(0),
    )?;
    state.total_message_count = usize::try_from(count)?;
    state.pending = usize::try_from(count)?;
    upsert_agent_state_tx(tx, &state)
}
fn fingerprint(db: &crate::runtime_db::RuntimeDb) -> Result<Vec<Vec<String>>> {
    let c = db.connection()?;
    ["SELECT payload_json FROM timers ORDER BY timer_id","SELECT json_array(timer_id,message_id,fire_count,status,created_at,updated_at,incorporated_at,cancelled_at) FROM timer_wakes ORDER BY timer_id,message_id","SELECT payload_json FROM queue_entries ORDER BY message_id","SELECT payload_json FROM messages ORDER BY message_id","SELECT payload_json FROM agent_states ORDER BY agent_id","SELECT json_array(agent_id,identity_incarnation,revision,coverage_json) FROM scheduling_domain_current ORDER BY agent_id,identity_incarnation","SELECT json_array(agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest) FROM scheduling_domain_records ORDER BY agent_id,identity_incarnation,family,record_id"].into_iter().map(|sql|{let mut q=c.prepare(sql)?;let rows=q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)}).collect()
}
#[test]
fn bootstrap_first_wake_uses_real_fixed_timer_and_queued_message_and_one_sparse_ack() -> Result<()>
{
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Completed))?;
        extra_message(tx, "original", "first-wake", 2)
    })?;
    let budget = ledger()?;
    let ack = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())?;
    assert_eq!(ack.result.created_wakes, 1);
    assert_eq!(ack.result.reactivated_wakes, 0);
    assert!(ack.queue_entries.is_empty());
    assert_eq!(
        ack.scheduling_ack.qualified_actual_keys,
        BTreeSet::from([SchedulingKey::wake("original", "first-wake")])
    );
    let invalidation = ack.scheduling_ack.invalidation.as_ref().unwrap();
    assert_eq!(invalidation.revision, invalidation.base_revision + 1);
    assert_eq!(db.timers().pending_wake("original")?.unwrap().fire_count, 2);
    assert_eq!(db.timers().latest("original")?.unwrap().fire_count, 5);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r
                .get::<_, u64>(0))?,
        0,
        "read-only first Wake proof cannot adopt a Message"
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM dispatch_memory_checkpoints",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    assert!(budget.report()?.observed_bytes > 0);
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn bootstrap_reactivation_preserves_original_fire_count_source_and_actual_exit() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("repeat", TimerStatus::Active))?;
        wake(tx, "repeat", "restored", 2)?;
        tx.execute("UPDATE timer_wakes SET status='incorporated',incorporated_at=?1 WHERE timer_id='repeat'",[repositories::timestamp(at())])?;
        Ok(())
    })?;
    let budget = ledger()?;
    let ack = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())?;
    assert_eq!(ack.result.reactivated_wakes, 1);
    assert_eq!(ack.result.created_wakes, 0);
    assert_eq!(
        ack.scheduling_ack.qualified_actual_keys,
        BTreeSet::from([SchedulingKey::wake("repeat", "restored")])
    );
    assert_eq!(db.timers().pending_wake("repeat")?.unwrap().fire_count, 2);
    assert_eq!(db.connection()?.query_row("SELECT incorporated_at IS NULL AND cancelled_at IS NULL FROM timer_wakes WHERE timer_id='repeat'",[],|r|r.get::<_,bool>(0))?,true);
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn bootstrap_pending_merge_keeps_independent_old_wake_and_drops_duplicate_without_new_grant(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("repeat", TimerStatus::Active))?;
        wake(tx, "repeat", "old-pending", 2)?;
        extra_message(tx, "repeat", "newer-duplicate", 7)
    })?;
    let budget = ledger()?;
    let ack = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())?;
    assert_eq!(ack.result.retained_wakes, 1);
    assert_eq!(ack.result.created_wakes, 0);
    assert_eq!(ack.result.reactivated_wakes, 0);
    assert_eq!(ack.result.dropped_message_ids, vec!["newer-duplicate"]);
    assert_eq!(ack.queue_entries.len(), 1);
    assert!(
        ack.scheduling_ack.qualified_actual_keys.is_empty()
            && ack.scheduling_ack.invalidation.is_none()
    );
    assert_eq!(
        db.timers().pending_wake("repeat")?.unwrap().message_id,
        "old-pending"
    );
    assert_eq!(db.timers().pending_wake("repeat")?.unwrap().fire_count, 2);
    assert_eq!(
        db.agent_states()
            .latest(&scope().agent_id)?
            .unwrap()
            .pending,
        2,
        "original bootstrap recalculates pending in its next phase"
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM timer_wakes WHERE message_id='newer-duplicate'",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_records WHERE family='timer_wake'",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        0
    );
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn bootstrap_orphan_pending_cancellation_keeps_actual_qualified_write_without_message_admission(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("orphan", TimerStatus::Completed))?;
        wake(tx, "orphan", "already-done", 3)?;
        let entry = QueueEntryRecord {
            message_id: "already-done".into(),
            agent_id: scope().agent_id,
            priority: Priority::Next,
            status: QueueEntryStatus::Aborted,
            created_at: at(),
            updated_at: at() + chrono::Duration::seconds(1),
        };
        // Original terminal queue state before fixed cutover, not a current prune.
        repositories::upsert_queue_entry_tx(tx, &entry)?;
        Ok(())
    })?;
    let budget = ledger()?;
    let ack = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())?;
    assert_eq!(ack.result.invalidated_wakes, 1);
    assert!(ack.result.dropped_message_ids.is_empty());
    assert_eq!(
        ack.scheduling_ack.qualified_actual_keys,
        BTreeSet::from([SchedulingKey::wake("orphan", "already-done")])
    );
    assert!(db.timers().pending_wake("orphan")?.is_none());
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn bootstrap_pruned_wake_restore_and_qualifier_fault_roll_back_all_original_business() -> Result<()>
{
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Active))?;
        extra_message(tx, "original", "first-wake", 2)
    })?;
    let budget = ledger()?;
    db.connection()?.execute_batch("CREATE TRIGGER fail_bootstrap_wake_alias BEFORE INSERT ON scheduling_domain_records WHEN NEW.family='timer_wake' BEGIN SELECT RAISE(ABORT,'actual Wake ACK fault');END;")?;
    let before = fingerprint(&db)?;
    assert!(db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())
        .is_err());
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.connection()?
        .execute_batch("DROP TRIGGER fail_bootstrap_wake_alias")?;
    drop(
        db.timers()
            .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())?,
    );
    // Corrupt restore of a pruned current address: body presence cannot revive it.
    db.connection()?.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE family='timer_wake'",[])?;
    let before = fingerprint(&db)?;
    assert!(db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())
        .is_err());
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn bootstrap_corrupt_message_is_budgeted_before_native_origin_decode() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Active))?;
        extra_message(tx, "original", "bad-source", 2)
    })?;
    db.connection()?
        .execute_batch("DROP INDEX idx_messages_agent_trusted_operator;")?;
    db.connection()?.execute(
        "UPDATE messages SET payload_json=?1 WHERE message_id='bad-source'",
        ["{".repeat(65 * 1024)],
    )?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits {
        history_per_agent_bytes: 64 * 1024,
        pending_per_agent_bytes: 64 * 1024,
        ..Default::default()
    })?;
    let before = fingerprint(&db)?;
    let error = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())
        .err()
        .context("expected early material gate")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn bootstrap_fixed_queued_source_cannot_adopt_a_late_raw_timer_as_new_wake_authority() -> Result<()>
{
    let (_home, db) = legacy(true, |tx| {
        extra_message(tx, "late-timer", "original-queued-source", 2)
    })?;
    // Conflicting restore after the true cutover: no accepted Timer writer ran.
    db.transaction(|tx| {
        repositories::upsert_timer_tx(tx, &timer("late-timer", TimerStatus::Active))
    })?;
    let budget = ledger()?;
    let before = fingerprint(&db)?;
    let error = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())
        .err()
        .context("late raw Timer acquired Wake authority")?;
    assert!(
        matches!(error.downcast_ref::<SchedulingSnapshotError>(),Some(SchedulingSnapshotError::NeedsPreparation(key)) if key==&SchedulingKey::new(SchedulingFamily::Timer,"late-timer")),
        "{error:#}"
    );
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn bootstrap_explicit_absence_can_receive_first_actual_wake_but_never_pruned_membership(
) -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Active))?;
        extra_message(tx, "original", "first-wake", 2)
    })?;
    let budget = ledger()?;
    db.transaction(|tx| {
        let mut load = super::super::super::tests::request();
        load.capture
            .required_keys
            .insert(SchedulingKey::wake("original", "first-wake"));
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget)?.require_current()?;
        Ok(())
    })?;
    assert_eq!(
        db.connection()?.query_row(
            "SELECT availability FROM scheduling_domain_records WHERE family='timer_wake'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        "absent"
    );
    let ack = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())?;
    assert_eq!(ack.result.created_wakes, 1);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT availability FROM scheduling_domain_records WHERE family='timer_wake'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        "present"
    );
    drop(ack);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn bootstrap_state_native_nodes_precede_pending_metadata_typed_state_read() -> Result<()> {
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer("original", TimerStatus::Completed))?;
        extra_message(tx, "original", "first-wake", 2)
    })?;
    db.transaction(|tx| {
        let mut state=AgentState::new(&scope().agent_id);state.pending=1;state.total_message_count=1;
        state.working_memory.current_working_memory.pending_followups=(0..4096).map(|n|format!("f{n}")).collect();
        upsert_agent_state_tx(tx,&state)?;
        tx.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending','bad typed count') WHERE agent_id=?1",[&scope().agent_id])?;
        Ok(())
    })?;
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits {
        record_limit: 1024,
        history_per_agent_bytes: 4 * 1024 * 1024,
        pending_per_agent_bytes: 4 * 1024 * 1024,
        ..Default::default()
    })?;
    let before = fingerprint(&db)?;
    let error = db
        .timers()
        .normalize_wakes_for_recovery_in_scope(&scope(), &budget, at())
        .err()
        .context("pending_metadata decoded unadmitted State collections")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(fingerprint(&db)?, before);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
