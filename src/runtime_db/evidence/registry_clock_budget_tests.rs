use super::{registry_incarnation_budget_tests::*, *};
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
};
use std::sync::Arc;

fn marker(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
         VALUES ('clock_marker', 'rollback', '2026-10-17T22:00:00Z', '2026-10-17T22:00:00Z')",
        [],
    )?;
    Ok(())
}

#[test]
fn clocks_accumulate_on_original_phase_and_preserve_sql_bytes_and_last_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut record = identity();
    let before = add(header(), (31 * 8, 3 * 8));
    let one = clock_timestamp_cost();
    let exact = add(before, (one.0 * 4, one.1 * 4));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        let mut first_reserved = None;
        for operation in 0..4 {
            let start = Utc::now().timestamp_millis();
            db.transaction(|tx| {
                if operation == 3 {
                    release_agent_identity_reservation_tx(tx, &record.agent_id)
                } else {
                    if operation == 2 {
                        record.status = AgentRegistryStatus::Deleted;
                    }
                    reserve_agent_identity_tx(tx, &record)
                }
            })?;
            let end = Utc::now().timestamp_millis();
            let row: (String, String, Option<String>, String) = db.connection()?.query_row(
                "SELECT reservation_state, reserved_at, retired_at, source
                 FROM agent_identity_reservations WHERE agent_id = ?1",
                [&record.agent_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            assert_eq!(row.0, if operation == 2 { "retired" } else { "active" });
            assert_eq!(
                row.3,
                if operation == 3 {
                    "reincarnation"
                } else {
                    "agent_registry"
                }
            );
            if operation == 0 {
                first_reserved = Some(row.1.clone());
            } else if operation < 3 {
                assert_eq!(Some(&row.1), first_reserved.as_ref());
            }
            let actual = if operation == 2 {
                row.2.as_deref().unwrap()
            } else {
                assert!(row.2.is_none());
                &row.1
            };
            let parsed = actual.parse::<chrono::DateTime<Utc>>()?;
            assert_eq!(actual, timestamp(parsed));
            if operation != 1 {
                assert!((start..=end).contains(&parsed.timestamp_millis()));
            }
            assert_eq!(
                phase.finish()?,
                add(
                    before,
                    (one.0 * (operation + 1), one.1 * (operation as usize + 1))
                ),
            );
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn both_clock_gates_deny_each_dimension_before_sql_and_roll_back_whole_transaction() -> Result<()> {
    for release in [false, true] {
        for deleted in [false, true] {
            for dimension in 0..2 {
                let (_home, db) = fixture()?;
                let mut record = identity();
                if release {
                    record.status = AgentRegistryStatus::Deleted;
                    db.transaction(|tx| reserve_agent_identity_tx(tx, &record))?;
                } else if deleted {
                    record.status = AgentRegistryStatus::Deleted;
                }
                let before_sql = snapshot(&db)?;
                db.connection()?.execute_batch(
                    "CREATE TRIGGER deny_clock_insert BEFORE INSERT ON agent_identity_reservations
                     BEGIN SELECT RAISE(ABORT, 'unexpected_clock_sql'); END;
                     CREATE TRIGGER deny_clock_update BEFORE UPDATE ON agent_identity_reservations
                     BEGIN SELECT RAISE(ABORT, 'unexpected_clock_sql'); END;",
                )?;
                let before = add(header(), (29 * 8, 2 * 8));
                let exact = add(before, clock_timestamp_cost());
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    exact.0 - u64::from(dimension == 0),
                    exact.1 - usize::from(dimension == 1),
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    charge_owned_metadata(29, 2)?;
                    let error = db
                        .transaction(|tx| {
                            marker(tx)?;
                            if release {
                                release_agent_identity_reservation_tx(tx, &record.agent_id)
                            } else {
                                reserve_agent_identity_tx(tx, &record)
                            }
                        })
                        .unwrap_err();
                    assert!(
                        format!("{error:#}").contains("ResourceExhausted"),
                        "{error:#}"
                    );
                    assert!(ResourceReadMaterial::active_guard().is_err());
                    assert!(phase.finish().is_err());
                }
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                assert_eq!(snapshot(&db)?, before_sql);
                let last = holder.clone();
                drop((phase, holder));
                assert_eq!(ledger.report()?.observed_bytes, before.0);
                drop(last);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn real_sql_errors_retain_original_phase_and_roll_back_prior_writes() -> Result<()> {
    for release in [false, true] {
        let (_home, db) = fixture()?;
        let mut record = identity();
        if release {
            record.status = AgentRegistryStatus::Deleted;
            db.transaction(|tx| reserve_agent_identity_tx(tx, &record))?;
        }
        let before_sql = snapshot(&db)?;
        db.connection()?.execute_batch(
            "CREATE TRIGGER reject_clock_insert BEFORE INSERT ON agent_identity_reservations
             BEGIN SELECT RAISE(ABORT, 'clock_sql_failure'); END;
             CREATE TRIGGER reject_clock_update BEFORE UPDATE ON agent_identity_reservations
             BEGIN SELECT RAISE(ABORT, 'clock_sql_failure'); END;",
        )?;
        let exact = add(add(header(), (29 * 8, 2 * 8)), clock_timestamp_cost());
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(29, 2)?;
            let error = db
                .transaction(|tx| {
                    marker(tx)?;
                    if release {
                        release_agent_identity_reservation_tx(tx, &record.agent_id)
                    } else {
                        reserve_agent_identity_tx(tx, &record)
                    }
                })
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("clock_sql_failure"),
                "{error:#}"
            );
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(snapshot(&db)?, before_sql);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn retired_short_circuits_stay_free_and_no_match_release_keeps_business_error() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut record = identity();
    record.status = AgentRegistryStatus::Deleted;
    db.transaction(|tx| reserve_agent_identity_tx(tx, &record))?;
    let before_sql = snapshot(&db)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        db.transaction(|tx| reserve_agent_identity_tx(tx, &record))?;
        record.status = AgentRegistryStatus::Active;
        let error = db
            .transaction(|tx| reserve_agent_identity_tx(tx, &record))
            .unwrap_err();
        assert!(format!("{error:#}").contains("retired agent ids are never reused"));
        assert_eq!(phase.finish()?, header());
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(snapshot(&db)?, before_sql);
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, 0);

    // Neither an absent row nor an active row satisfies the release predicate.
    for missing in [true, false] {
        let target = if missing {
            "missing-clock-id"
        } else {
            &record.agent_id
        };
        if !missing {
            db.transaction(|tx| release_agent_identity_reservation_tx(tx, target))?;
        }
        let before_sql = snapshot(&db)?;
        let exact = add(header(), clock_timestamp_cost());
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = db
                .transaction(|tx| {
                    marker(tx)?;
                    release_agent_identity_reservation_tx(tx, target)
                })
                .unwrap_err();
            assert!(format!("{error:#}").contains("identity reservation is not retired"));
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(snapshot(&db)?, before_sql);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    db.connection()?
        .execute_batch("DROP TABLE agent_identity_reservations")?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _entry = phase.enter();
        let error = db
            .transaction(|tx| reserve_agent_identity_tx(tx, &record))
            .unwrap_err();
        assert!(format!("{error:#}").contains("no such table"));
        assert_eq!(phase.finish()?, header());
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
