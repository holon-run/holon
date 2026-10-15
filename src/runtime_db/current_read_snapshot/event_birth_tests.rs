use super::*;

fn native_event(id: &str) -> AuditEvent {
    let mut event = AuditEvent::legacy("actual-native-event", serde_json::json!({"actual":true}));
    event.id = id.into();
    event.created_at = at();
    event
}
fn commit_native_events(
    tx: &Transaction<'_>,
    state: &AgentState,
    budget: &InputEvidenceBudgetLedger,
    events: &[AuditEvent],
) -> Result<()> {
    let keys = events
        .iter()
        .map(|event| CurrentReadKey::event(&event.id))
        .collect();
    let mut proof = prepare_current_read_write_tx(tx, &scope(), state, &keys, &request(), budget)?;
    let mut actual = Vec::new();
    for event in events {
        let (_, ack) = append_current_read_event_tx(&mut proof, event)?;
        actual.push(ack);
    }
    register_current_read_writes_tx(proof, &actual, state)?;
    Ok(())
}

#[test]
fn original_native_event_birth_frontier_keeps_all_531_members_and_noop_keeps_sequence() -> Result<()>
{
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        assert_eq!(
            writes::original_native_event_cursor_tx(tx, &scope())?,
            Some(0)
        );
        Ok(())
    })?;
    let events = (0..531)
        .map(|n| native_event(&format!("actual-native-{n:03}")))
        .collect::<Vec<_>>();
    for batch in events.chunks(32) {
        db.transaction(|tx| commit_native_events(tx, &state, &budget, batch))?;
    }
    let (count, max_seq, cursor, fixed): (u64,u64,u64,u64) = db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM current_event_native_births),(SELECT MAX(event_seq) FROM audit_events),(SELECT high_water FROM current_event_birth_cursors),(SELECT COUNT(*) FROM current_event_native_births WHERE event_id='original-current-read-event')",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    )?;
    assert_eq!((count, cursor, fixed), (531, max_seq, 0));
    db.transaction(|tx| {
        commit_native_events(tx, &state, &budget, &events[530..])?;
        assert_eq!(
            writes::original_native_event_cursor_tx(tx, &scope())?,
            Some(cursor)
        );
        let count: u64 = tx.query_row(
            "SELECT COUNT(*) FROM current_event_native_births",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 531);
        Ok(())
    })?;
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_native_event_birth_and_member_roll_back_with_business_and_missing_installed_source(
) -> Result<()> {
    for missing in [false, true] {
        let (_home, db, state) = legacy()?;
        let budget = ledger()?;
        db.transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
            Ok(())
        })?;
        let before: u64 =
            db.connection()?
                .query_row("SELECT revision FROM current_read_current", [], |r| {
                    r.get(0)
                })?;
        let failed: Result<()> = db.transaction(|tx| {
            if missing {
                tx.execute_batch("DROP TABLE current_event_native_births")?;
            }
            commit_native_events(
                tx,
                &state,
                &budget,
                &[native_event("rollback-native-event")],
            )?;
            anyhow::bail!("original-native-event-whole-ACK-fault")
        });
        let error = failed
            .err()
            .context("whole native Event transaction must fail")?;
        if missing {
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::NeedsPreparation)
                ),
                "{error:#}"
            );
        } else {
            assert!(format!("{error:#}").contains("original-native-event-whole-ACK-fault"));
        }
        let (revision,cursors,members,body,accepted):(u64,u64,u64,bool,bool)=db.connection()?.query_row(
            "SELECT (SELECT revision FROM current_read_current),(SELECT COUNT(*) FROM current_event_birth_cursors),(SELECT COUNT(*) FROM current_event_native_births),EXISTS(SELECT 1 FROM audit_events WHERE audit_event_id='rollback-native-event'),EXISTS(SELECT 1 FROM current_read_members WHERE family='event' AND record_id='rollback-native-event')",
            [],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        assert_eq!(
            (revision, cursors, members, body, accepted),
            (before, 0, 0, false, false)
        );
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn native_event_id_uses_complete_blob_address_and_pruning_cannot_lower_high_water() -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        commit_native_events(tx,&state,&budget,&[native_event("\0"),native_event("")])?;
        let cursor=writes::original_native_event_cursor_tx(tx,&scope())?.context("installed native source missing")?;
        assert!(cursor>0);
        let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM current_event_native_births WHERE event_id=?1 AND birth_seq>0 AND birth_seq<=?2)",params!["\0",cursor],|r|r.get(0))?;
        assert!(exact);
        tx.execute("UPDATE current_read_members SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE family='event' AND record_id=?1",["\0"])?;
        tx.execute("DELETE FROM audit_events WHERE audit_event_id=?1",["\0"])?;
        assert_eq!(writes::original_native_event_cursor_tx(tx,&scope())?,Some(cursor));
        assert!(tx.execute("INSERT INTO current_event_native_births(agent_id,identity_incarnation,event_id,birth_seq) VALUES(?1,?2,'duplicate-sequence',?3)",params![scope().agent_id,scope().identity_incarnation,cursor]).is_err());
        Ok(())
    })?;
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
