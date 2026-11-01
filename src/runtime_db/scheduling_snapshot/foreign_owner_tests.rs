use super::*;

#[test]
fn exact_foreign_owner_precedes_oversize_corrupt_body_without_granting_raw_tail() -> Result<()> {
    for (id, is_foreign) in [("raw-foreign-selected", true), ("raw-own-selected", false)] {
        let (_home, db) = legacy(true, |_| Ok(()))?;
        let budget = ledger()?;
        let mut raw = timer(id, TimerStatus::Active);
        if is_foreign {
            raw.agent_id = "another-owner".into();
        }
        db.transaction(|tx| {
            repositories::upsert_timer_tx(tx, &raw)?;
            tx.execute(
                "UPDATE timers SET payload_json=?1 WHERE timer_id=?2",
                params!["{".repeat(1024 * 1024), raw.id],
            )?;
            Ok(())
        })?;
        let counts = || -> Result<(usize, usize)> {
            Ok(db.connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM scheduling_domain_records),
                        (SELECT COUNT(*) FROM scheduling_domain_current)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        };
        let before = counts()?;
        let key = SchedulingKey::new(SchedulingFamily::Timer, id);
        let mut load = request();
        load.byte_limit = 128 * 1024;
        load.capture.required_keys.insert(key.clone());
        let error = db
            .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &load, &budget))
            .err()
            .context("unqualified selected body must never be admitted")?;
        assert!(
            matches!(error.downcast_ref::<SchedulingSnapshotError>(),
                Some(SchedulingSnapshotError::ForeignRecord(actual)) if is_foreign && actual == &key)
                || matches!(error.downcast_ref::<SchedulingSnapshotError>(),
                Some(SchedulingSnapshotError::NeedsPreparation(actual)) if !is_foreign && actual == &key),
            "{error:#}"
        );
        assert_eq!(budget.report()?.observed_bytes, 0);
        assert_eq!(
            counts()?,
            before,
            "rejected raw tail cannot mint aliases or coverage"
        );
    }
    Ok(())
}
