use super::{registry_incarnation_budget_tests::*, *};
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
};
use std::sync::Arc;

fn cost(value: chrono::DateTime<Utc>) -> (u64, usize) {
    let original = value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    (
        ((original.len() + std::mem::size_of::<String>()) as u64) * 8,
        8,
    )
}

#[test]
fn timestamp_bytes_cover_signed_years_fraction_and_leap_second_on_original_phase() -> Result<()> {
    let values = [
        "0000-01-01T00:00:00Z",
        "9999-12-31T23:59:59.999999999Z",
        "+10000-01-01T00:00:00Z",
        "+262000-01-01T00:00:00Z",
        "-0001-01-01T00:00:00.123456789Z",
        "-10000-01-01T00:00:00Z",
        "2016-12-31T23:59:60.123456789Z",
    ]
    .map(|text| text.parse::<chrono::DateTime<Utc>>().unwrap());
    let expected = values.map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    for (value, expected) in values.into_iter().zip(&expected) {
        assert_eq!(registry_timestamp(value)?, *expected);
    }
    let before = add(header(), (31 * 8, 3 * 8));
    let exact = values.into_iter().map(cost).fold(before, add);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        let mut used = before;
        for (value, expected) in values.into_iter().zip(&expected) {
            assert_eq!(registry_timestamp(value)?, *expected);
            used = add(used, cost(value));
            assert_eq!(phase.finish()?, used);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder,
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
fn timestamp_each_sql_argument_denies_before_write_and_rolls_back_whole_transaction() -> Result<()>
{
    for existing in [false, true] {
        for deleted in [false, true] {
            let (_home, db) = fixture()?;
            let original = identity();
            if existing {
                db.agent_identities().upsert(&original)?;
            }
            let mut record = original.clone();
            if deleted {
                record.status = AgentRegistryStatus::Deleted;
                record.deleted_at = Some(record.updated_at);
            }
            let before_sql = snapshot(&db)?;
            db.connection()?.execute_batch(
                "CREATE TRIGGER reject_timestamp_write BEFORE INSERT ON agent_identities
                 BEGIN SELECT RAISE(ABORT, 'unexpected_timestamp_registry_write'); END;",
            )?;
            let mut before = add(
                add(add(header(), (29 * 8, 2 * 8)), clock_timestamp_cost()),
                add(
                    name_key_cost(&record),
                    if existing {
                        decode_cost(&serde_json::to_string(&original)?)
                    } else {
                        (0, 0)
                    },
                ),
            );
            for value in [
                Some(record.created_at),
                Some(record.updated_at),
                record.deleted_at,
            ]
            .into_iter()
            .flatten()
            {
                let exact = add(before, cost(value));
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
                                     VALUES ('timestamp_marker', 'rollback',
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
                before = exact;
            }
        }
    }
    Ok(())
}

#[test]
fn timestamp_sql_error_preserves_phase_and_rolls_back_reservation_and_marker() -> Result<()> {
    for existing in [false, true] {
        let (_home, db) = fixture()?;
        let original = identity();
        if existing {
            db.agent_identities().upsert(&original)?;
        }
        let mut record = original.clone();
        record.status = AgentRegistryStatus::Deleted;
        record.deleted_at = Some(record.updated_at);
        let before_sql = snapshot(&db)?;
        db.connection()?.execute_batch(
            "CREATE TRIGGER reject_timestamp_sql BEFORE INSERT ON agent_identities
             BEGIN SELECT RAISE(ABORT, 'registry_timestamp_sql_failure'); END;",
        )?;
        let exact = add(
            add(header(), (29 * 8, 2 * 8)),
            add(
                add(name_key_cost(&record), upsert_timestamp_cost(&record)),
                if existing {
                    decode_cost(&serde_json::to_string(&original)?)
                } else {
                    (0, 0)
                },
            ),
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
                         VALUES ('timestamp_sql_marker', 'rollback',
                                 '2026-10-17T22:00:00Z', '2026-10-17T22:00:00Z')",
                        [],
                    )?;
                    upsert_agent_identity_tx(tx, &record)
                })
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("registry_timestamp_sql_failure"),
                "{error:#}"
            );
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

#[test]
fn timestamp_upsert_preserves_sql_bytes_null_and_cumulative_original_holder() -> Result<()> {
    for deleted in [false, true] {
        let (_home, db) = fixture()?;
        let mut record = identity();
        record.created_at = "-0001-01-01T00:00:00.123456789Z".parse()?;
        if deleted {
            record.status = AgentRegistryStatus::Deleted;
            record.deleted_at = Some(record.updated_at);
        }
        let expected = (
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.deleted_at.map(timestamp),
        );
        let before = add(header(), (31 * 8, 3 * 8));
        let one = add(
            decode_cost(&serde_json::to_string(&record)?),
            add(name_key_cost(&record), timestamp_cost(&record)),
        );
        let first = add(one, clock_timestamp_cost());
        let second = if deleted { one } else { first };
        let exact = add(add(before, first), second);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(31, 3)?;
            let mut used = before;
            for cost in [first, second] {
                db.agent_identities().upsert(&record)?;
                used = add(used, cost);
                assert_eq!(phase.finish()?, used);
                let actual: (String, String, Option<String>) = db.connection()?.query_row(
                    "SELECT created_at, updated_at, archived_at
                     FROM agent_identities WHERE agent_id = ?1",
                    [&record.agent_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                assert_eq!(actual, expected);
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder,
                ));
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
