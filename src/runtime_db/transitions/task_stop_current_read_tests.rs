//! Actual original Result ACKs maintain their frozen Queue/Audit sources.
use super::*;
use crate::runtime::current_read_facts::{
    CurrentReadError, CurrentReadFact, CurrentReadKey, CurrentReadRequest,
};

fn current_request() -> CurrentReadRequest {
    CurrentReadRequest::current(64 * 1024 * 1024, 65536)
}
fn current_version(f: &Fixture) -> Result<(u64, String, u64)> {
    Ok(f.db.connection()?.query_row(
        "SELECT revision,freshness,(SELECT COUNT(*) FROM current_read_members WHERE agent_id=?1 AND identity_incarnation=?2) FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![f.scope.agent_id,f.scope.identity_incarnation],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    )?)
}
fn current_rows(f: &Fixture) -> Result<BTreeMap<String, Vec<String>>> {
    let mut values = rows(f)?;
    let conn = f.db.connection()?;
    for table in ["current_read_current", "current_read_members"] {
        let mut statement = conn.prepare(&format!("SELECT * FROM {table}"))?;
        let columns = statement.column_count();
        let mut records = statement
            .query_map([], |r| {
                (0..columns)
                    .map(|i| r.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|values| format!("{values:?}"))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        records.sort();
        values.insert(table.into(), records);
    }
    Ok(values)
}
fn raw_queue(f: &Fixture, id: &str, status: QueueEntryStatus) -> Result<()> {
    f.db.transaction(|tx| {
        upsert_queue_entry_tx(
            tx,
            &QueueEntryRecord {
                message_id: id.into(),
                agent_id: f.scope.agent_id.clone(),
                priority: Priority::Next,
                status: status.clone(),
                created_at: f.at,
                updated_at: f.at,
            },
        )?;
        Ok(())
    })
}
fn current_member(f: &Fixture, family: &str, id: &str) -> Result<u64> {
    Ok(f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
        params![f.scope.agent_id,f.scope.identity_incarnation,family,id],|r|r.get(0),
    )?)
}

#[test]
fn original_result_queue_and_wait_audit_join_one_current_ack_and_pure_capture() -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, true, true)?;
    let before = current_version(&f)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    let facts = value
        .effects
        .current_read_maintenance
        .as_ref()
        .context("original Result actual Current maintenance missing")?;
    assert!(value.effects.current_read_facts.is_none());
    assert_eq!(
        (facts.base_revision, facts.revision),
        (before.0, before.0 + 1)
    );
    let resident = facts
        .resident()
        .context("qualified Queue namespace remains complete")?;
    resident.validate_state(value.effects.agent_state.as_ref().unwrap().record.as_ref())?;
    let capture = resident.capture(&current_request())?;
    assert!(capture.has_queued());
    assert!(capture.records.iter().any(|r| matches!(&r.fact,
        CurrentReadFact::Queue(q) if q.message_id=="frozen-stop-feedback"
            && q.created_at==source.record.intent.result.as_ref().unwrap().message.created_at)));
    let audit_id = wait_trigger_audit_id("frozen-stop-feedback", WaitTriggerAuditKind::Triggered);
    assert_eq!(current_member(&f, "queue", "frozen-stop-feedback")?, 1);
    assert_eq!(current_member(&f, "event", &audit_id)?, 1);
    assert_eq!(
        current_member(
            &f,
            "event",
            &wait_trigger_audit_id("frozen-stop-feedback", WaitTriggerAuditKind::Stale)
        )?,
        0,
        "unused mutually exclusive event ID grants no member"
    );
    assert!(capture.events.iter().any(|r| matches!(&r.fact,
        CurrentReadFact::Event(e) if e.id==audit_id && e.event_seq>0)));
    let held = current_rows(&f)?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = commit(&f, &p, &l)?
    else {
        panic!("original Result whole receipt must replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(current_rows(&f)?, held);
    // No normal read can reach body SQL after the actual ACK has been captured.
    f.db.connection()?.execute_batch("ALTER TABLE queue_entries RENAME TO stop_queue_forbidden; ALTER TABLE audit_events RENAME TO stop_events_forbidden; ALTER TABLE agent_states RENAME TO stop_state_forbidden; ALTER TABLE agent_identities RENAME TO stop_identity_forbidden;")?;
    let pure = resident.capture(&current_request())?;
    assert_eq!(pure.revision, capture.revision);
    for original in &capture.records {
        let selected = pure
            .records
            .iter()
            .find(|r| r.locator.key == original.locator.key)
            .unwrap();
        assert!(Arc::ptr_eq(original, selected));
    }
    drop((pure, capture, resident, value, p, source, receipt, replay));
    assert_eq!(l.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn unbound_original_result_qualifies_only_actual_queue_without_new_activation_or_result(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, false, false)?;
    let before = current_version(&f)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert!(matches!(
        ack(&receipt).result.as_ref().unwrap().outcome,
        stops::TaskStopResultOutcome::Delivered {
            result_identity: None,
            ..
        }
    ));
    let facts = value.effects.current_read_maintenance.as_ref().unwrap();
    assert_eq!(
        (facts.base_revision, facts.revision),
        (before.0, before.0 + 1)
    );
    assert!(facts
        .resident()
        .context("unbound Message has a real Current Queue")?
        .capture(&current_request())?
        .has_queued());
    assert_eq!(current_member(&f, "queue", "frozen-stop-feedback")?, 1);
    let counts:(u64,u64,u64,u64)=f.db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM task_result_settlements),(SELECT COUNT(*) FROM execution_protocol_attempts),(SELECT COUNT(*) FROM turn_records),(SELECT COUNT(*) FROM current_read_members WHERE family='event')",
        [],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    assert_eq!(counts, (0, 0, 0, 0));
    drop((value, receipt, p, source));
    assert_eq!(l.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_result_receipt_failure_rolls_back_current_members_revision_and_audit_sequence(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, true, true)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    f.db.connection()?.execute_batch("CREATE TRIGGER fail_current_result_whole_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='original-frozen-result-phase' BEGIN SELECT RAISE(ABORT,'current-result-whole-receipt-fault'); END;")?;
    let before = current_rows(&f)?;
    let held = l.report()?.observed_bytes;
    let error = commit(&f, &p, &l)
        .err()
        .context("original whole receipt fault omitted")?;
    assert!(format!("{error:#}").contains("current-result-whole-receipt-fault"));
    assert_eq!(current_rows(&f)?, before);
    assert_eq!(l.report()?.observed_bytes, held);
    f.db.connection()?
        .execute_batch("DROP TRIGGER fail_current_result_whole_receipt;")?;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert_eq!(current_member(&f, "queue", "frozen-stop-feedback")?, 1);
    assert_eq!(
        value
            .effects
            .current_read_maintenance
            .as_ref()
            .unwrap()
            .revision,
        value
            .effects
            .current_read_maintenance
            .as_ref()
            .unwrap()
            .base_revision
            + 1
    );
    assert_eq!(
        ack(&receipt).progress.result,
        stops::TaskStopResultState::Completed
    );
    drop((value, receipt, p, source));
    assert_eq!(l.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_raw_frozen_queue_cleanup_remains_lawful_but_never_adopts_a_member() -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, false, false)?;
    raw_queue(&f, "frozen-stop-feedback", QueueEntryStatus::Queued)?;
    f.db.transaction(|tx| {
        let mut state = agent_state_tx(tx, &f.scope.agent_id)?.unwrap();
        state.pending = 1;
        upsert_agent_state_tx(tx, &state)
    })?;
    let before = current_version(&f)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    let facts = value.effects.current_read_maintenance.as_ref().unwrap();
    assert!(facts.resident().is_none());
    assert_eq!(
        (facts.base_revision, facts.revision),
        (before.0, before.0 + 1)
    );
    assert_eq!(current_member(&f, "queue", "frozen-stop-feedback")?, 0);
    assert_eq!(f.state()?.pending, 1);
    assert!(matches!(
        ack(&receipt).result.as_ref().unwrap().outcome,
        stops::TaskStopResultOutcome::Delivered { .. }
    ));
    assert_eq!(
        f.db.queue_entries()
            .latest("frozen-stop-feedback")?
            .unwrap()
            .created_at,
        source
            .record
            .intent
            .result
            .as_ref()
            .unwrap()
            .message
            .created_at
    );
    drop((value, receipt, p, source));
    assert_eq!(l.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_result_raw_queue_all_columns_are_budgeted_before_corrupt_payload_decode() -> Result<()>
{
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, false, false)?;
    raw_queue(&f, "frozen-stop-feedback", QueueEntryStatus::Queued)?;
    f.db.transaction(|tx| {
        let mut state = agent_state_tx(tx, &f.scope.agent_id)?.unwrap();
        state.pending = 1;
        upsert_agent_state_tx(tx, &state)
    })?;
    // Priority is SQLite TEXT with no length constraint. The corrupted body
    // must never be parsed before its coexisting normalized column admission.
    f.db.connection()?.execute("UPDATE queue_entries SET priority=?1,payload_json='{' WHERE message_id='frozen-stop-feedback'",["x".repeat(9*1024*1024)])?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let before = current_rows(&f)?;
    let held = l.report()?.observed_bytes;
    let error = commit(&f, &p, &l)
        .err()
        .context("oversized raw Queue accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(current_rows(&f)?, before);
    assert_eq!(l.report()?.observed_bytes, held);
    drop((p, source));
    assert_eq!(l.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_result_rejects_foreign_and_pruned_queue_without_message_or_progress_ack() -> Result<()>
{
    for pruned in [false, true] {
        let f = Fixture::new()?;
        let l = ledger()?;
        let source = source(&f, &l, false, false)?;
        if pruned {
            f.db.connection()?.execute(
                "INSERT INTO current_read_members(agent_id,identity_incarnation,family,record_id,availability,content_digest,projection_digest,revision,status,sort_seq,sort_at,created_at) VALUES(?1,?2,'queue','frozen-stop-feedback','pruned',NULL,NULL,1,'pruned',0,?3,?3)",
                params![f.scope.agent_id,f.scope.identity_incarnation,repositories::timestamp(f.at)])?;
        } else {
            raw_queue(&f, "frozen-stop-feedback", QueueEntryStatus::Queued)?;
            f.db.connection()?.execute("UPDATE queue_entries SET agent_id='unrelated-owner',payload_json='{' WHERE message_id='frozen-stop-feedback'",[])?;
        }
        let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
        let before = current_rows(&f)?;
        let held = l.report()?.observed_bytes;
        let error = commit(&f, &p, &l)
            .err()
            .context("foreign/pruned source accepted")?;
        if pruned {
            assert!(
                matches!(error.downcast_ref::<CurrentReadError>(),
                Some(CurrentReadError::Pruned(k)) if k==&CurrentReadKey::queue("frozen-stop-feedback")),
                "{error:#}"
            );
        } else {
            assert!(
                format!("{error:#}").contains("Stop Result cannot requeue a foreign"),
                "{error:#}"
            );
            assert!(!format!("{error:#}").contains("malformed JSON"));
        }
        assert_eq!(current_rows(&f)?, before);
        assert_eq!(l.report()?.observed_bytes, held);
        drop((p, source));
        assert_eq!(l.report()?.observed_bytes, 0);
    }
    Ok(())
}
