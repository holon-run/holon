use super::*;

fn seeded(count: usize) -> Result<(tempfile::TempDir, RuntimeDb)> {
    legacy(true, |tx| {
        for i in 0..count {
            // The lexically last ID is the newest payload time; every row's
            // normalized timestamp still belongs to the same millisecond.
            let name = if i + 1 == count {
                "z-newest".to_owned()
            } else {
                format!("a-{i:03}")
            };
            let time = at() + chrono::Duration::nanoseconds(i64::try_from(i + 1)? * 1000);
            let mut t = timer(&format!("timer-{name}"), TimerStatus::Cancelled);
            t.created_at = time;
            t.status = TimerStatus::Cancelled;
            t.next_fire_at = None;
            t.last_fired_at = None;
            repositories::upsert_timer_tx(tx, &t)?;
            let mut trigger = trigger();
            trigger.external_trigger_id = format!("trigger-{name}");
            trigger.created_at = time;
            trigger.status = ExternalTriggerStatus::Revoked;
            trigger.revoked_at = Some(time);
            repositories::upsert_external_trigger_tx(tx, &trigger)?;
        }
        Ok(())
    })
}

fn read_orders(
    db: &RuntimeDb,
    ledger: &InputEvidenceBudgetLedger,
    cold: bool,
    timer_limit: usize,
    window: usize,
) -> Result<(Vec<String>, Vec<String>, Vec<String>, Vec<String>)> {
    let mut request = request();
    request.record_limit = 65_536;
    request.byte_limit = 64 * 1024 * 1024;
    request.capture.history_limit = window;
    let current = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request, ledger)?.require_current()
    })?;
    let timer_history = &current.resident.coverage().history[&SchedulingFamily::Timer];
    let cold_keys = timer_history
        .keys
        .iter()
        .map(|key| key.id.clone())
        .collect();
    let loaded_keys = timer_history
        .loaded_keys
        .iter()
        .map(|key| key.id.clone())
        .collect();
    let captured = current.resident.capture(&request.capture)?;
    let project = if cold {
        crate::runtime::project_cold_scheduling_records
    } else {
        crate::runtime::project_selected_scheduling_records
    };
    let (timers, triggers, _material) = project(
        captured,
        timer_limit,
        ledger,
        request.byte_limit,
        request.record_limit,
        current.material_usage,
        current._read_material,
    )?;
    Ok((
        timers.iter().map(|t| t.id.clone()).collect(),
        triggers
            .iter()
            .map(|t| t.external_trigger_id.clone())
            .collect(),
        cold_keys,
        loaded_keys,
    ))
}

#[test]
fn loaded_and_cold_history_keep_original_nanosecond_and_sql_orders() -> Result<()> {
    let (_home, db) = seeded(2)?;
    let ledger = ledger()?;
    let loaded = read_orders(&db, &ledger, false, 1, 2)?;
    let cold = read_orders(&db, &ledger, true, 1, 2)?;
    assert_eq!(loaded.0, vec!["timer-z-newest"]);
    assert_eq!(cold.0, vec!["timer-a-000"]);
    assert_eq!(loaded.1, vec!["trigger-z-newest", "trigger-a-000"]);
    assert_eq!(cold.1, vec!["trigger-a-000", "trigger-z-newest"]);
    let cold_repository = db.timers().recent_for_agent(&scope().agent_id, 1)?;
    assert_eq!(
        cold.0,
        cold_repository
            .into_iter()
            .map(|t| t.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        cold.1,
        db.external_triggers()
            .latest_for_agent(&scope().agent_id)?
            .into_iter()
            .map(|t| t.external_trigger_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn loaded_history_selects_original_top_k_beyond_cold_same_millisecond_cutoff() -> Result<()> {
    let window = SCHEDULING_HISTORY_WINDOW;
    let (_home, db) = seeded(window + 2)?;
    let ledger = ledger()?;
    let loaded = read_orders(&db, &ledger, false, window, window)?;
    let cold = read_orders(&db, &ledger, true, window, window)?;
    assert_eq!(loaded.0.len(), window);
    assert_eq!(cold.0.len(), window);
    assert!(loaded.0.iter().any(|id| id == "timer-z-newest"));
    assert!(!cold.0.iter().any(|id| id == "timer-z-newest"));
    assert!(!loaded.0.iter().any(|id| id == "timer-a-000"));
    assert!(cold.0.iter().any(|id| id == "timer-a-000"));
    assert!(loaded.3.iter().any(|id| id == "timer-z-newest"));
    assert!(!loaded.2.iter().any(|id| id == "timer-z-newest"));
    assert_eq!(loaded.1.len(), window);
    assert_eq!(cold.1.len(), window);
    assert!(loaded.1.iter().any(|id| id == "trigger-z-newest"));
    assert!(!cold.1.iter().any(|id| id == "trigger-z-newest"));
    let repository = db.timers().recent_for_agent(&scope().agent_id, window)?;
    assert_eq!(
        cold.0,
        repository.into_iter().map(|t| t.id).collect::<Vec<_>>()
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.connection()?.execute("UPDATE scheduling_domain_records SET created_at='1900-01-01T00:00:00.000Z' WHERE family='timer' AND record_id='timer-z-newest'",[])?;
    let error = read_orders(&db, &ledger, false, window, window).unwrap_err();
    assert!(
        matches!(error.downcast_ref::<SchedulingSnapshotError>(),Some(SchedulingSnapshotError::CanonicalChanged(key)) if key.id=="timer-z-newest"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn loaded_history_boundary_bucket_budget_precedes_malformed_body_header() -> Result<()> {
    let (_home, db) = seeded(2)?;
    let ledger = ledger()?;
    let mut original_request = request();
    original_request.byte_limit = 4 * 1024 * 1024;
    drop(db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &original_request, &ledger)
    })?);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        tx.execute(
            "UPDATE timers SET payload_json=?1 WHERE timer_id='timer-z-newest'",
            ["x".repeat(1024 * 1024)],
        )?;
        let mut request = request();
        request.byte_limit = 4 * 1024 * 1024;
        let error =
            prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request, &ledger).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SchedulingSnapshotError>(),
                Some(SchedulingSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn saved_loaded_history_collections_are_admitted_before_revision_validation() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let mut request = request();
    request.record_limit = 65_536;
    let current = db.transaction(|tx| {
        prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request, &ledger)?.require_current()
    })?;
    let material_record_baseline = current.material_usage.1;
    let mut coverage = serde_json::to_value(current.resident.coverage())?;
    drop(current);
    coverage["history"]["timer"]["loaded_keys"] = serde_json::to_value(
        (0..1000)
            .map(|i| SchedulingKey::new(SchedulingFamily::Timer, format!("saved-{i}")))
            .collect::<Vec<_>>(),
    )?;
    coverage["revision"] = serde_json::json!(0);
    db.transaction(|tx| {tx.execute("UPDATE scheduling_domain_current SET coverage_json=?1 WHERE agent_id=?2 AND identity_incarnation=?3",
        params![serde_json::to_string(&coverage)?,scope().agent_id,scope().identity_incarnation])?;Ok(())})?;
    request.record_limit = material_record_baseline + 512;
    let error = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope(), &request, &ledger))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn accepted_history_head_rejects_normalized_birth_drift_in_fixed_and_current_members() -> Result<()>
{
    for (current_alias, body_drift, head_drift) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (true, false, true),
    ] {
        let (_home, db) = seeded(2)?;
        let ledger = ledger()?;
        if current_alias {
            drop(read_orders(&db, &ledger, false, 1, 2)?);
        }
        let before: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_records",
            [],
            |r| r.get(0),
        )?;
        if head_drift {
            db.connection()?.execute("UPDATE scheduling_domain_records SET created_at='1900-01-01T00:00:00.000Z' WHERE family='timer' AND record_id='timer-z-newest'",[])?;
        } else if body_drift {
            db.connection()?.execute("UPDATE timers SET payload_json=json_set(payload_json,'$.created_at','2026-10-01T00:00:00.123000001Z') WHERE timer_id='timer-z-newest'",[])?;
        } else {
            db.connection()?.execute("UPDATE timers SET created_at='1900-01-01T00:00:00.000Z' WHERE timer_id='timer-z-newest'",[])?;
        }
        let error = read_orders(&db, &ledger, false, 1, 2).unwrap_err();
        assert!(
            matches!(error.downcast_ref::<SchedulingSnapshotError>(),Some(SchedulingSnapshotError::CanonicalChanged(key)) if key.id=="timer-z-newest"),
            "{error:#}"
        );
        let after: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_records",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(after, before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn history_keeps_late_raw_tail_unqualified_and_rejects_foreign_missing_or_pruned_before_json(
) -> Result<()> {
    for corruption in ["late_tail", "foreign", "missing", "pruned"] {
        let (_home, db) = seeded(2)?;
        let ledger = ledger()?;
        drop(read_orders(&db, &ledger, false, 1, 2)?);
        db.transaction(|tx| {
            match corruption {
                "late_tail"=>{
                    let mut t=timer("late-tail",TimerStatus::Cancelled);t.created_at=at()+chrono::Duration::days(1);
                    repositories::upsert_timer_tx(tx,&t)?;
                    tx.execute("UPDATE timers SET payload_json='bad-unqualified-body' WHERE timer_id='late-tail'",[])?;
                }
                "foreign"=>{tx.execute("UPDATE timers SET agent_id='foreign',payload_json='bad-foreign-body' WHERE timer_id='timer-z-newest'",[])?;}
                "missing"=>{tx.execute("DELETE FROM timers WHERE timer_id='timer-z-newest'",[])?;}
                "pruned"=>{
                    tx.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE family='timer' AND record_id='timer-z-newest'",[])?;
                    tx.execute("UPDATE timers SET payload_json='bad-pruned-body' WHERE timer_id='timer-z-newest'",[])?;
                }
                _=>unreachable!(),
            }
            Ok(())
        })?;
        let read = read_orders(&db, &ledger, false, 1, 2);
        if corruption == "late_tail" {
            assert_eq!(read?.0, vec!["timer-z-newest"]);
            let adopted:bool=db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM scheduling_domain_records WHERE record_id='late-tail')",[],|r|r.get(0))?;
            assert!(!adopted);
        } else if corruption == "pruned" {
            let read = read?;
            // Original accepted metadata/order survives without the bad body.
            assert_eq!(read.0, vec!["timer-a-000"]);
            assert_eq!(read.2, vec!["timer-a-000", "timer-z-newest"]);
            assert_eq!(read.3, vec!["timer-z-newest", "timer-a-000"]);
            let mut exact = request();
            exact.capture.history_limit = 0;
            exact.capture.required_keys.insert(SchedulingKey::new(
                SchedulingFamily::Timer,
                "timer-z-newest",
            ));
            let error = db
                .transaction(|tx| {
                    prepare_scheduling_domain_snapshot_tx(tx, &scope(), &exact, &ledger)
                })
                .unwrap_err();
            assert!(
                matches!(error.downcast_ref::<SchedulingReadError>(),
                Some(SchedulingReadError::Pruned(key)) if key.id == "timer-z-newest"),
                "{error:#}"
            );
            let unchanged: (String, String) = db.connection()?.query_row(
                "SELECT r.availability,c.payload_json FROM scheduling_domain_records r JOIN timers c ON c.timer_id=r.record_id WHERE r.family='timer' AND r.record_id='timer-z-newest'",
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(unchanged, ("pruned".into(), "bad-pruned-body".into()));
        } else {
            let error = read.unwrap_err();
            assert!(
                matches!(
                    (corruption, error.downcast_ref::<SchedulingSnapshotError>()),
                    ("foreign", Some(SchedulingSnapshotError::ForeignRecord(_)))
                        | (
                            "missing",
                            Some(SchedulingSnapshotError::CanonicalChanged(_))
                        )
                ),
                "{error:#}"
            );
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
