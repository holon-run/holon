use super::*;

fn original_stored_fixture(
    family: SchedulingFamily,
) -> Result<(tempfile::TempDir, RuntimeDb, SchedulingKey)> {
    let key = match family {
        SchedulingFamily::Timer => SchedulingKey::new(family, "raw-first-timer"),
        SchedulingFamily::TimerWake => {
            SchedulingKey::wake("raw-first-wake-timer", "raw-first-wake-message")
        }
        SchedulingFamily::ExternalTrigger => SchedulingKey::new(family, "original-trigger"),
    };
    let (home, db) = legacy(true, |tx| {
        match family {
            SchedulingFamily::Timer => {
                repositories::upsert_timer_tx(tx, &timer("raw-first-timer", TimerStatus::Active))?;
            }
            SchedulingFamily::TimerWake => {
                repositories::upsert_timer_tx(
                    tx,
                    &timer("raw-first-wake-timer", TimerStatus::Active),
                )?;
                wake(tx, "raw-first-wake-timer", "raw-first-wake-message", 5)?;
            }
            SchedulingFamily::ExternalTrigger => {
                repositories::upsert_external_trigger_tx(tx, &trigger())?;
            }
        }
        Ok(())
    })?;
    Ok((home, db, key))
}
fn change_normalized_blob(tx: &Transaction<'_>, key: &SchedulingKey, bytes: usize) -> Result<()> {
    let blob = vec![0xffu8; bytes];
    let changed = match key.family {
        SchedulingFamily::Timer => tx.execute(
            "UPDATE timers SET duration_ms=?2 WHERE timer_id=?1",
            params![key.id, blob],
        )?,
        SchedulingFamily::TimerWake => {
            let pair = key.wake_pair()?;
            tx.execute(
                "UPDATE timer_wakes SET fire_count=?3 WHERE timer_id=?1 AND message_id=?2",
                params![pair.0, pair.1, blob],
            )?
        }
        SchedulingFamily::ExternalTrigger => tx.execute(
            "UPDATE external_triggers SET delivery_count=?2 WHERE external_trigger_id=?1",
            params![key.id, blob],
        )?,
    };
    assert_eq!(changed, 1);
    Ok(())
}

#[test]
fn scheduling_all_actual_normalized_scalars_are_guarded_before_json_projection() -> Result<()> {
    for family in [
        SchedulingFamily::Timer,
        SchedulingFamily::TimerWake,
        SchedulingFamily::ExternalTrigger,
    ] {
        let (_home, db, key) = original_stored_fixture(family)?;
        let budget = ledger()?;
        let mut full = request();
        full.byte_limit = MAX_BYTES;
        full.record_limit = 65536;
        db.transaction(|tx| {
            let original_scope = scope();
            let mut material = Material::new(&original_scope, &full, &budget);
            material.input_memory = true;
            let stored =
                stored::read_stored(tx, &key, &original_scope.agent_id, Some(&mut material))?
                    .context("original fixed scalar row")?;
            assert_eq!(stored.fact.key(), key);
            assert!(budget.report()?.observed_bytes > 0);
            Ok(())
        })?;
        assert_eq!(budget.report()?.observed_bytes, 0);
        db.transaction(|tx| change_normalized_blob(tx, &key, 128 * 1024))?;
        // SQLite affinity permits this actual BLOB. json_object rejects it, so
        // the old size query errored before reaching its guard at all.
        for input_memory in [false, true] {
            let mut small = full.clone();
            small.byte_limit = 64 * 1024;
            let error = db
                .transaction(|tx| {
                    let original_scope = scope();
                    let mut material = Material::new(&original_scope, &small, &budget);
                    material.input_memory = input_memory;
                    stored::read_stored(tx, &key, &original_scope.agent_id, Some(&mut material))
                })
                .unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<SchedulingSnapshotError>(),
                    Some(SchedulingSnapshotError::ResourceExhausted(_))
                ),
                "{family:?}: {error:#}"
            );
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
        let error = db
            .transaction(|tx| {
                let original_scope = scope();
                let mut material = Material::new(&original_scope, &full, &budget);
                stored::read_stored(tx, &key, &original_scope.agent_id, Some(&mut material))
            })
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("JSON cannot hold BLOB"),
            "{error:#}"
        );
        assert_eq!(budget.report()?.observed_bytes, 0);
        let (current, aliases): (usize, usize) = db.transaction(|tx| Ok(tx.query_row(
            "SELECT (SELECT COUNT(*) FROM scheduling_domain_current),(SELECT COUNT(*) FROM scheduling_domain_records)", [],
            |row| Ok((row.get(0)?, row.get(1)?)))?))?;
        assert_eq!((current, aliases), (0, 0));
    }
    Ok(())
}

#[test]
fn scheduling_fixed_single_record_and_foreign_wake_reject_before_projection() -> Result<()> {
    for family in [
        SchedulingFamily::Timer,
        SchedulingFamily::TimerWake,
        SchedulingFamily::ExternalTrigger,
    ] {
        let (_home, db, key) = original_stored_fixture(family)?;
        let budget = ledger()?;
        db.transaction(|tx| change_normalized_blob(tx, &key, 2 * 1024 * 1024))?;
        let error = db
            .transaction(|tx| stored::read_stored(tx, &key, &scope().agent_id, None))
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SchedulingSnapshotError>(),
                Some(SchedulingSnapshotError::ResourceExhausted(
                    "fixed single record"
                ))
            ),
            "{error:#}"
        );
        let mut tiny = request();
        tiny.record_limit = 8;
        tiny.byte_limit = 1;
        let error = db
            .transaction(|tx| {
                let original_scope = scope();
                let mut material = Material::new(&original_scope, &tiny, &budget);
                assert!(stored::has_foreign_owner(tx, &key, "another-owner")?);
                stored::read_stored(tx, &key, "another-owner", Some(&mut material))
            })
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ForeignRecord(actual)) if actual == &key),
            "{error:#}"
        );
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
