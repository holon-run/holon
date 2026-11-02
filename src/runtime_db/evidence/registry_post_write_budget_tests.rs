use super::{registry_incarnation_budget_tests::*, *};
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
};
use std::sync::Arc;

#[test]
fn post_write_decode_uses_original_remainder_and_accumulates_through_continuity() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut record = identity();
    let one = decode_cost(&serde_json::to_string(&record)?);
    let key = add(name_key_cost(&record), upsert_timestamp_cost(&record));
    let before = add(header(), (29 * 8, 2 * 8));
    // First insert: post-read. New incarnation: pre-read and post-read.
    let exact = add(add(add(add(add(before, one), key), one), key), one);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(29, 2)?;
        db.agent_identities().upsert(&record)?;
        assert_eq!(phase.finish()?, add(add(before, one), key));
        record.incarnation = 2;
        record.updated_at += chrono::Duration::seconds(1);
        assert_eq!(decode_cost(&serde_json::to_string(&record)?), one);
        db.agent_identities().upsert(&record)?;
        assert_eq!(phase.finish()?, exact);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder,
        ));
        let count: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM agent_episode_continuity WHERE agent_id = ?1",
            [&record.agent_id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 2);
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
fn post_write_denial_rolls_back_reservation_identity_marker_and_continuity() -> Result<()> {
    for existing in [false, true] {
        let (_home, db) = fixture()?;
        let mut record = identity();
        let one = decode_cost(&serde_json::to_string(&record)?);
        if existing {
            db.agent_identities().upsert(&record)?;
            record.incarnation = 2;
            record.updated_at += chrono::Duration::seconds(1);
        }
        let before_sql = snapshot(&db)?;
        let before = add(
            add(
                add(header(), (29 * 8, 2 * 8)),
                add(name_key_cost(&record), upsert_timestamp_cost(&record)),
            ),
            if existing { one } else { (0, 0) },
        );
        let exact = add(before, one);
        // A denied post-read must happen before the continuity write.
        db.connection()?.execute_batch(
            "CREATE TRIGGER reject_post_read_continuity BEFORE INSERT ON agent_episode_continuity
             BEGIN SELECT RAISE(ABORT, 'unexpected_continuity_write'); END;",
        )?;
        for dimension in 0..2 {
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
                        tx.execute(
                            "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                             VALUES ('post_read_marker', 'rollback',
                                     '2026-10-17T22:00:00Z', '2026-10-17T22:00:00Z')",
                            [],
                        )?;
                        upsert_agent_identity_tx(tx, &record)
                    })
                    .unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(phase.finish().is_err());
            assert_eq!(snapshot(&db)?, before_sql);
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            let last = holder.clone();
            drop((phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn post_write_preserves_complete_decode_type_and_missing_row_errors() -> Result<()> {
    for corruption in 0..3 {
        let (_home, db) = fixture()?;
        let record = identity();
        let mut invalid = serde_json::to_value(&record)?;
        invalid["status"] = "unknown-status".into();
        let payload = serde_json::to_string(&invalid)?;
        let action = match corruption {
            0 => format!(
                "UPDATE agent_identities SET payload_json = '{}' WHERE agent_id = NEW.agent_id;",
                payload.replace('\'', "''"),
            ),
            1 => "UPDATE agent_identities SET payload_json = x'00' WHERE agent_id = NEW.agent_id;"
                .into(),
            _ => "DELETE FROM agent_identities WHERE agent_id = NEW.agent_id;".into(),
        };
        db.connection()?.execute_batch(&format!(
            "CREATE TRIGGER corrupt_post_read AFTER INSERT ON agent_identities BEGIN {action} END;"
        ))?;
        let before_sql = snapshot(&db)?;
        for denied in [true, false] {
            let cost = if corruption == 0 {
                decode_cost(&payload)
            } else {
                (0, 0)
            };
            let before = add(
                header(),
                add(name_key_cost(&record), upsert_timestamp_cost(&record)),
            );
            let exact = add(before, cost);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(denied && corruption == 0),
                exact.1,
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                let error = db.agent_identities().upsert(&record).unwrap_err();
                if corruption == 0 && denied {
                    assert!(format!("{error:#}").contains("ResourceExhausted"));
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                } else {
                    if corruption == 0 {
                        assert!(error.downcast_ref::<serde_json::Error>().is_some());
                    } else if corruption == 1 {
                        assert!(matches!(
                            error.downcast_ref::<rusqlite::types::FromSqlError>(),
                            Some(rusqlite::types::FromSqlError::InvalidType),
                        ));
                    } else {
                        assert!(matches!(
                            error.downcast_ref::<rusqlite::Error>(),
                            Some(rusqlite::Error::QueryReturnedNoRows),
                        ));
                    }
                    assert_eq!(phase.finish()?, exact);
                    assert!(Arc::ptr_eq(
                        &ResourceReadMaterial::active_guard()?.unwrap(),
                        &holder,
                    ));
                }
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert_eq!(snapshot(&db)?, before_sql);
            let expected = if denied && corruption == 0 {
                before
            } else {
                exact
            };
            let last = holder.clone();
            drop((phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, expected.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        let error = db.agent_identities().upsert(&record).unwrap_err();
        assert!(!format!("{error:#}").contains("ResourceExhausted"));
        assert_eq!(snapshot(&db)?, before_sql);
    }
    Ok(())
}

#[test]
fn post_write_nonmatching_identity_does_not_create_continuity() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    let mut other = record.clone();
    other.name = Some("different-name".into());
    let payload = serde_json::to_string(&other)?;
    db.connection()?.execute_batch(&format!(
        "CREATE TRIGGER replace_post_read AFTER INSERT ON agent_identities
         BEGIN UPDATE agent_identities SET payload_json = '{}'
         WHERE agent_id = NEW.agent_id; END;",
        payload.replace('\'', "''"),
    ))?;
    let exact = add(
        add(
            header(),
            add(name_key_cost(&record), upsert_timestamp_cost(&record)),
        ),
        decode_cost(&payload),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        db.agent_identities().upsert(&record)?;
        assert_eq!(phase.finish()?, exact);
        let count: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM agent_episode_continuity WHERE agent_id = ?1",
            [&record.agent_id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
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
fn post_write_same_incarnation_skips_decode_and_stale_replacement_checks_stored_body() -> Result<()>
{
    let (_home, db) = fixture()?;
    let record = identity();
    db.agent_identities().upsert(&record)?;
    let before_sql = snapshot(&db)?;
    let one = decode_cost(&serde_json::to_string(&record)?);
    let keyed = add(
        header(),
        add(name_key_cost(&record), upsert_timestamp_cost(&record)),
    );
    for stale_replacement in [false, true] {
        let mut incoming = record.clone();
        let exact = if stale_replacement {
            incoming.incarnation = 2;
            incoming.updated_at -= chrono::Duration::seconds(1);
            add(add(keyed, one), one)
        } else {
            add(keyed, one)
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            db.agent_identities().upsert(&incoming)?;
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder,
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
