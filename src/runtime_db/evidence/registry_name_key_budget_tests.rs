use super::{registry_incarnation_budget_tests::*, *};
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
};
use std::sync::Arc;

#[test]
fn name_key_preserves_unicode_bytes_null_and_original_phase_last_holder() -> Result<()> {
    for (name, expected) in [
        (None, None),
        (Some(""), Some("")),
        (Some("Mixed-ASCII"), Some("mixed-ascii")),
        (Some("Straße"), Some("strasse")),
        (Some("İΣςﬃ"), Some("i\u{307}σσffi")),
        (Some(" 名字 É E\u{301} "), Some(" 名字 é e\u{301} ")),
    ] {
        let (_home, db) = fixture()?;
        let mut record = identity();
        record.name = name.map(str::to_owned);
        let key = expected.map_or((0, 0), |value| {
            (
                ((value.len() + std::mem::size_of::<String>()) as u64) * 8,
                8,
            )
        });
        assert_eq!(name_key_cost(&record), key);
        let one = add(
            decode_cost(&serde_json::to_string(&record)?),
            add(key, upsert_timestamp_cost(&record)),
        );
        let before = add(header(), (31 * 8, 3 * 8));
        let exact = add(add(before, one), one);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(31, 3)?;
            for attempt in 1..=2 {
                db.agent_identities().upsert(&record)?;
                assert_eq!(
                    phase.finish()?,
                    (
                        before.0 + one.0 * attempt,
                        before.1 + one.1 * attempt as usize
                    ),
                );
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder,
                ));
                let actual: Option<String> = db.connection()?.query_row(
                    "SELECT name_key FROM agent_identities WHERE agent_id = ?1",
                    [&record.agent_id],
                    |row| row.get(0),
                )?;
                assert_eq!(actual.as_deref(), expected);
            }
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn name_key_denial_precedes_registry_sql_and_rolls_back_whole_transaction() -> Result<()> {
    for existing in [false, true] {
        for name in ["", "Straße", "İΣςﬃ"] {
            let (_home, db) = fixture()?;
            let mut record = identity();
            record.name = Some(name.into());
            if existing {
                db.agent_identities().upsert(&record)?;
            }
            let before_sql = snapshot(&db)?;
            db.connection()?.execute_batch(
                "CREATE TRIGGER reject_name_key_write BEFORE INSERT ON agent_identities
                 BEGIN SELECT RAISE(ABORT, 'unexpected_registry_write'); END;",
            )?;
            let before = add(
                add(add(header(), (29 * 8, 2 * 8)), clock_timestamp_cost()),
                if existing {
                    decode_cost(&serde_json::to_string(&record)?)
                } else {
                    (0, 0)
                },
            );
            let exact = add(before, name_key_cost(&record));
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
                             VALUES ('name_key_marker', 'rollback',
                                     '2026-10-17T22:00:00Z', '2026-10-17T22:00:00Z')",
                                [],
                            )?;
                            upsert_agent_identity_tx(tx, &record)
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
fn name_key_sql_failure_preserves_original_phase_and_rolls_back_all_writes() -> Result<()> {
    for existing in [false, true] {
        let (_home, db) = fixture()?;
        let record = identity();
        if existing {
            db.agent_identities().upsert(&record)?;
        }
        let before_sql = snapshot(&db)?;
        db.connection()?.execute_batch(
            "CREATE TRIGGER reject_name_key_sql BEFORE INSERT ON agent_identities
             BEGIN SELECT RAISE(ABORT, 'name_key_sql_failure'); END;",
        )?;
        let before = add(header(), (29 * 8, 2 * 8));
        let exact = add(
            add(
                before,
                add(name_key_cost(&record), upsert_timestamp_cost(&record)),
            ),
            if existing {
                decode_cost(&serde_json::to_string(&record)?)
            } else {
                (0, 0)
            },
        );
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(29, 2)?;
            let error = db
                .transaction(|tx| {
                    tx.execute(
                        "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                     VALUES ('name_key_sql_marker', 'rollback',
                             '2026-10-17T22:00:00Z', '2026-10-17T22:00:00Z')",
                        [],
                    )?;
                    upsert_agent_identity_tx(tx, &record)
                })
                .unwrap_err();
            assert!(format!("{error:#}").contains("name_key_sql_failure"));
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
fn name_key_stale_write_and_retired_short_circuit_preserve_identity() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    db.agent_identities().upsert(&record)?;
    let before_sql = snapshot(&db)?;
    let mut stale = record.clone();
    stale.name = Some("Straße".into());
    stale.incarnation = 2;
    stale.updated_at -= chrono::Duration::seconds(1);
    let read = decode_cost(&serde_json::to_string(&record)?);
    let exact = add(
        add(
            add(header(), read),
            add(name_key_cost(&stale), upsert_timestamp_cost(&stale)),
        ),
        read,
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        db.agent_identities().upsert(&stale)?;
        assert_eq!(phase.finish()?, exact);
        assert_eq!(snapshot(&db)?, before_sql);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    let mut deleted = record.clone();
    deleted.status = AgentRegistryStatus::Deleted;
    deleted.updated_at += chrono::Duration::seconds(1);
    db.agent_identities().upsert(&deleted)?;
    let before_sql = snapshot(&db)?;
    let exact = add(header(), decode_cost(&serde_json::to_string(&deleted)?));
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        let error = db.agent_identities().upsert(&record).unwrap_err();
        assert!(format!("{error:#}").contains("retired"));
        assert_eq!(phase.finish()?, exact);
        assert_eq!(snapshot(&db)?, before_sql);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
