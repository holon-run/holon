use super::*;
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
