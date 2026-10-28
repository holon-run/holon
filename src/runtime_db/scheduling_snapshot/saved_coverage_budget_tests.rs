use super::*;

fn saved_fixture() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    InputEvidenceBudgetLedger,
    SchedulingCoverage,
)> {
    let (home, db) = legacy(true, |_| Ok(()))?;
    let budget = ledger()?;
    let initial = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    let coverage = initial.resident.coverage().clone();
    drop(initial);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok((home, db, budget, coverage))
}

fn large_saved_collection(field: &str, coverage: &mut SchedulingCoverage) {
    let family = match field {
        "pending_wakes" => SchedulingFamily::TimerWake,
        "active_triggers" => SchedulingFamily::ExternalTrigger,
        _ => SchedulingFamily::Timer,
    };
    let keys: Vec<_> = (0..600)
        .map(|index| {
            if family == SchedulingFamily::TimerWake {
                SchedulingKey::wake(&format!("timer-{index}"), &format!("message-{index}"))
            } else {
                SchedulingKey::new(family, format!("saved-{index}"))
            }
        })
        .collect();
    match field {
        "active_timers" => coverage.active_timers = keys.into_iter().collect(),
        "pending_wakes" => coverage.pending_wakes = keys.into_iter().collect(),
        "active_triggers" => coverage.active_triggers = keys.into_iter().collect(),
        "required_keys" => coverage.required_keys = keys.into_iter().collect(),
        "observations" => {
            coverage.observations = keys
                .into_iter()
                .map(|key| SchedulingObservation {
                    key,
                    availability: SchedulingAvailability::Absent,
                })
                .collect();
        }
        "history" => {
            coverage
                .history
                .get_mut(&SchedulingFamily::Timer)
                .unwrap()
                .keys = keys;
        }
        _ => unreachable!(),
    }
}

#[test]
fn saved_scheduling_collections_exhaust_records_before_serde_revision_validation() -> Result<()> {
    for field in [
        "active_timers",
        "pending_wakes",
        "active_triggers",
        "required_keys",
        "observations",
        "history",
    ] {
        let (_home, db, budget, mut coverage) = saved_fixture()?;
        large_saved_collection(field, &mut coverage);
        let raw = serde_json::to_string(&coverage)?;
        // Deliberately corrupt saved metadata: the collection must be admitted
        // before Serde or its later normalized/JSON revision check can run.
        let wrong_revision = coverage.revision + 1;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE scheduling_domain_current SET coverage_json=?3,revision=?4
                 WHERE agent_id=?1 AND identity_incarnation=?2",
                params![
                    scope().agent_id,
                    scope().identity_incarnation,
                    raw,
                    wrong_revision
                ],
            )?;
            Ok(())
        })?;
        let mut small = request();
        small.record_limit = 256;
        let error = db
            .transaction(|tx| load_scheduling_domain_snapshot_tx(tx, &scope(), &small, &budget))
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SchedulingSnapshotError>(),
                Some(SchedulingSnapshotError::ResourceExhausted(_))
            ),
            "{field}: {error:#}"
        );
        assert_eq!(budget.report()?.observed_bytes, 0, "{field}");
        let saved: (u64, String, usize) = db.transaction(|tx| {
            Ok(tx.query_row(
                "SELECT revision,coverage_json,
                    (SELECT COUNT(*) FROM scheduling_domain_records)
                 FROM scheduling_domain_current
                 WHERE agent_id=?1 AND identity_incarnation=?2",
                params![scope().agent_id, scope().identity_incarnation],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })?;
        assert_eq!(saved, (wrong_revision, raw, 0));
        // Adequate material allowance reaches the original corruption check;
        // the new resource gate is not a blanket corruption suppression.
        let mut adequate = request();
        adequate.record_limit = 65536;
        let error = db
            .transaction(|tx| load_scheduling_domain_snapshot_tx(tx, &scope(), &adequate, &budget))
            .unwrap_err();
        assert!(format!("{error:#}").contains("Scheduling coverage revision differs"));
        assert_eq!(budget.report()?.observed_bytes, 0, "{field}");
    }
    Ok(())
}

#[test]
fn valid_saved_scheduling_metadata_holds_its_material_until_snapshot_drop() -> Result<()> {
    let (_home, db, budget, coverage) = saved_fixture()?;
    let loaded = db.transaction(|tx| {
        load_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget)?.require_current()
    })?;
    assert_eq!(loaded.resident.coverage(), &coverage);
    assert!(budget.report()?.observed_bytes > 0);
    assert!(loaded.input_roots.is_empty());
    drop(loaded);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn retired_scheduling_scope_does_not_read_successor_state_or_saved_coverage() -> Result<()> {
    let (_home, db, budget, _coverage) = saved_fixture()?;
    let mut successor = identity();
    successor.incarnation += 1;
    successor.updated_at += chrono::Duration::seconds(1);
    db.transaction(|tx| {
        // Negative fixture: the predecessor's stored coverage is corrupt, and
        // the canonical current State belongs to a successor incarnation.
        tx.execute(
            "UPDATE agent_identities SET payload_json=?2 WHERE agent_id=?1",
            params![scope().agent_id, serde_json::to_string(&successor)?],
        )?;
        tx.execute(
            "UPDATE agent_states SET payload_json='poison-successor-state' WHERE agent_id=?1",
            [&scope().agent_id],
        )?;
        tx.execute(
            "UPDATE scheduling_domain_current SET coverage_json='poison-old-coverage'
             WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        Ok(())
    })?;
    let observed =
        db.transaction(|tx| load_scheduling_domain_snapshot_tx(tx, &scope(), &request(), &budget))?;
    assert!(matches!(
        observed,
        SchedulingBootstrapSnapshot::Retired { .. }
    ));
    assert!(budget.report()?.observed_bytes > 0);
    drop(observed);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
