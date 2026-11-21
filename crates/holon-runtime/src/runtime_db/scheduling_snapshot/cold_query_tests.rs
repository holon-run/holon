use super::*;

#[test]
fn legacy_complete_empty_no_state_summary_requires_all_original_domain_proofs() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&scope().agent_id],
    )?;
    let budget = ledger()?;
    let read =
        db.transaction(|tx| crate::runtime_db::cold_summary::prepare_tx(tx, &scope(), &budget))?;
    assert!(read.domains.is_none());
    assert_eq!(read.identity, identity());
    assert!(db.agent_states().latest(&scope().agent_id)?.is_none());
    drop(read);
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.connection()?
        .execute("DELETE FROM task_domain_captures", [])?;
    let error = db
        .transaction(|tx| crate::runtime_db::cold_summary::prepare_tx(tx, &scope(), &budget))
        .err()
        .context("missing Task proof must refuse NoState Summary")?;
    assert!(
        format!("{error:#}").contains("complete empty-domain continuity"),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn no_state_summary_rejects_pruned_or_foreign_incarnation_members_as_metadata() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&scope().agent_id],
    )?;
    let budget = ledger()?;
    // A leftover domain member is not absence, even when its body has been
    // pruned or it belongs to a different incarnation of this Agent ID.
    db.connection()?.execute(
        "INSERT INTO task_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability)
         VALUES(?1,?2,'task','old-pruned-task',1,'pruned')",
        params![scope().agent_id, scope().identity_incarnation + 1],
    )?;
    let error = db
        .transaction(|tx| crate::runtime_db::cold_summary::prepare_tx(tx, &scope(), &budget))
        .err()
        .context("foreign pruned membership must refuse NoState")?;
    assert!(
        format!("{error:#}").contains("unproved task_domain_records members"),
        "{error:#}"
    );
    assert!(db.agent_states().latest(&scope().agent_id)?.is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_fixed_empty_no_state_query_stays_metadata_and_does_not_adopt_later_row() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [&scope().agent_id],
        )?;
        Ok(())
    })?;
    let budget = ledger()?;
    let read =
        db.transaction(|tx| prepare_cold_scheduling_query_tx(tx, &scope(), &request(), &budget))?;
    assert!(matches!(
        read,
        ColdSchedulingQuery::ProvenEmptyNoState { .. }
    ));
    assert!(budget.report()?.observed_bytes > 0);
    drop(read);
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        repositories::upsert_timer_tx(tx, &timer("late-raw-no-state", TimerStatus::Active))
    })?;
    let error = db
        .transaction(|tx| prepare_cold_scheduling_query_tx(tx, &scope(), &request(), &budget))
        .err()
        .context("fixed empty cannot adopt a later canonical body")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::IncompleteLive)
        ),
        "{error:#}"
    );
    let counts:(usize,usize,usize)=db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM agent_states),(SELECT COUNT(*) FROM scheduling_domain_current),(SELECT COUNT(*) FROM scheduling_domain_records)",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    assert_eq!(counts, (0, 0, 0));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
