use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "foundation-timestamp".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn timestamp_cost() -> (u64, usize) {
    // Independent current-year UTC millisecond text plus its String header.
    (
        (("2026-10-17T20:38:16.123Z".len() + std::mem::size_of::<String>()) as u64) * 8,
        8,
    )
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn probe_body_cost() -> (u64, usize) {
    (
        (("__observer_sync_reservation_probe__".len() + std::mem::size_of::<String>()) as u64) * 8,
        8,
    )
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}

fn assert_probe_rolled_back(connection: &Connection) -> Result<()> {
    assert!(connection.is_autocommit());
    for table in ["agent_identity_reservations", "agent_identities"] {
        let count: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = ?1"),
            ["__observer_sync_reservation_probe__"],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0, "{table}");
    }
    Ok(())
}

#[test]
fn real_probe_admits_timestamp_on_used_phase_and_always_rolls_back() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let decode = super::registry_verdict_budget_tests::fixed_probe_decode_cost(&connection)?;
    let used = (37 * 8, 2 * 8);
    let before = add(header(), used);
    let exact = add(before, timestamp_cost());
    for denied in [None, Some(0), Some(1)] {
        let limit = if denied.is_none() {
            add(
                add(add(exact, probe_body_cost()), decode),
                super::registry_verdict_budget_tests::probe_registry_timestamp_cost(),
            )
        } else {
            exact
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            limit.0 - u64::from(denied == Some(0)),
            limit.1 - usize::from(denied == Some(1)),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(37, 2)?;
            let result = probe_reservation_enforcement(&mut connection);
            if denied.is_none() {
                assert!(result?);
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder,
                ));
            } else {
                assert!(format!("{:#}", result.unwrap_err()).contains("ResourceExhausted"));
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_probe_rolled_back(&connection)?;
        let expected = if denied.is_none() { limit } else { before };
        if denied.is_none() {
            assert_eq!(phase.finish()?, expected);
        } else {
            assert!(phase.finish().is_err());
        }
        assert_eq!(ledger.report()?.observed_bytes, expected.0);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, expected.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn real_verifier_rejects_each_timestamp_before_any_verification_write() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let decode = super::registry_verdict_budget_tests::fixed_probe_decode_cost(&connection)?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verified_at = 'sentinel'",
        [],
    )?;
    let count = count_rows(
        &connection,
        "SELECT COUNT(*) FROM observer_sync_capability_verifications",
    )?;
    // The probe's ID copy and post-write decode intervene between timestamps.
    // The three metadata copies precede both timestamps.
    let mut metadata = (0, 0);
    for key in [
        "runtime_id",
        "event_log_epoch",
        "visibility_policy_generation",
    ] {
        let value: String = connection.query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        metadata = add(
            metadata,
            (
                ((value.len() + std::mem::size_of::<String>()) as u64) * 8,
                8,
            ),
        );
    }
    for gate in 0..2 {
        let admitted = if gate == 0 {
            (0, 0)
        } else {
            add(
                add(add(timestamp_cost(), probe_body_cost()), decode),
                super::registry_verdict_budget_tests::probe_registry_timestamp_cost(),
            )
        };
        let before = add(add(header(), metadata), admitted);
        let exact = add(before, timestamp_cost());
        for dimension in 0..2 {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                exact.0 - u64::from(dimension == 0),
                exact.1 - usize::from(dimension == 1),
            )?;
            {
                let _entry = phase.enter();
                let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(phase.finish().is_err());
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            assert_probe_rolled_back(&connection)?;
            assert_eq!(
                count_rows(
                    &connection,
                    "SELECT COUNT(*) FROM observer_sync_capability_verifications
                     WHERE verified_at = 'sentinel'",
                )?,
                count,
            );
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn real_verifier_shares_millisecond_sql_timestamp_and_last_whole_holder() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications
         SET verification_version = 0 WHERE capability = ?1",
        [EVENT_PROJECTION_EFFECT_COMPLETE],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let started = chrono::Utc::now();
    {
        let _entry = phase.enter();
        verify_observer_sync_foundations(&mut connection)?;
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder,
        ));
    }
    let finished = chrono::Utc::now();
    assert_probe_rolled_back(&connection)?;
    let mut statement = connection.prepare(
        "SELECT verified_at FROM observer_sync_capability_verifications ORDER BY capability",
    )?;
    let timestamps = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    assert_eq!(timestamps.len(), 7);
    assert!(timestamps.iter().all(|value| value == &timestamps[0]));
    let wire = &timestamps[0];
    let parsed = chrono::DateTime::parse_from_rfc3339(wire)?;
    assert_eq!(
        parsed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        *wire,
    );
    assert!(parsed.timestamp_millis() >= started.timestamp_millis());
    assert!(parsed.timestamp_millis() <= finished.timestamp_millis());
    assert_eq!(wire.len(), 24);
    let retained = phase.finish()?.0;
    assert!(retained >= header().0 + timestamp_cost().0 * 2);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, retained);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // Unqualified callers keep the existing behavior, without creating a phase.
    verify_observer_sync_foundations(&mut connection)?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn sql_failure_and_missing_tables_restore_tls_and_release_original_material() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    connection.execute_batch(
        "CREATE TEMP TRIGGER reject_timestamp_verification
         BEFORE INSERT ON observer_sync_capability_verifications
         BEGIN SELECT RAISE(ABORT, 'timestamp-verification-fixture'); END;",
    )?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let outer = ResourceReadMaterial::new(&ledger, &scope())?;
    let outer_holder = outer.guard()?;
    {
        let _outer_entry = outer.enter();
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        {
            let _entry = phase.enter();
            let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
            assert!(format!("{error:#}").contains("timestamp-verification-fixture"));
        }
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &outer_holder,
        ));
        assert!(phase.finish()?.0 >= header().0 + timestamp_cost().0 * 2);
        assert_probe_rolled_back(&connection)?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, outer.finish()?.0);
        let mut empty = Connection::open_in_memory()?;
        verify_observer_sync_foundations(&mut empty)?;
        // Nested ledger registration consumes the outer original allowance.
        assert_eq!(outer.finish()?, add(header(), header()));
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((outer, outer_holder));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
