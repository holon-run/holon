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

const PROBE_ID: &str = "__observer_sync_reservation_probe__";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "foundation-probe".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn text_cost(text: &str) -> (u64, usize) {
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

fn timestamp_cost() -> (u64, usize) {
    text_cost("2026-10-17T21:24:59.123Z")
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}

fn assert_no_probe(connection: &Connection) -> Result<()> {
    assert!(connection.is_autocommit());
    for table in [
        "agent_identity_reservations",
        "agent_identities",
        "agent_episode_continuity",
    ] {
        let count: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = ?1"),
            [PROBE_ID],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0, "{table}");
    }
    Ok(())
}

#[test]
fn probe_body_uses_original_remainder_and_accumulates_across_rollbacks() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let decode = super::registry_verdict_budget_tests::fixed_probe_decode_cost(&connection)?;
    let used = (31 * 8, 3 * 8);
    let one = add(
        add(add(timestamp_cost(), text_cost(PROBE_ID)), decode),
        super::registry_verdict_budget_tests::probe_registry_timestamp_cost(),
    );
    let before = add(header(), used);
    let exact = add(add(before, one), one);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        for attempt in 1..=2 {
            assert!(probe_reservation_enforcement(&mut connection)?);
            assert_no_probe(&connection)?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder,
            ));
            assert_eq!(ledger.report()?.observed_bytes, before.0 + one.0 * attempt,);
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(phase.finish()?, exact);
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn probe_rejects_each_allocation_dimension_and_rolls_back_before_registry() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let used = (31 * 8, 3 * 8);
    let mut before = add(header(), used);
    for cost in [timestamp_cost(), text_cost(PROBE_ID)] {
        let exact = add(before, cost);
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
                charge_owned_metadata(31, 3)?;
                let error = probe_reservation_enforcement(&mut connection).unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"));
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(phase.finish().is_err());
            assert_no_probe(&connection)?;
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            let last = holder.clone();
            drop((phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        before = exact;
    }
    Ok(())
}

#[test]
fn verifier_propagates_probe_body_denial_before_any_capability_write() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    assert_eq!(
        count_rows(&connection, "SELECT COUNT(*) FROM agent_states")?,
        0
    );
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verified_at = 'sentinel'",
        [],
    )?;
    let count = count_rows(
        &connection,
        "SELECT COUNT(*) FROM observer_sync_capability_verifications",
    )?;
    assert!(count > 0);
    let inventory_row = |connection: &Connection| -> rusqlite::Result<Vec<rusqlite::types::Value>> {
        connection.query_row(
            "SELECT * FROM observer_sync_capability_verifications WHERE capability = ?1",
            [EVENT_PROJECTION_EFFECT_COMPLETE],
            |row| {
                (0..row.as_ref().column_count())
                    .map(|index| row.get(index))
                    .collect()
            },
        )
    };
    assert!(current_event_projection_effect_verification(&connection)?.is_some());
    let cached_inventory = inventory_row(&connection)?;
    let mut before = header();
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
        before = add(before, text_cost(&value));
    }
    before = add(before, timestamp_cost());
    let exact = add(before, text_cost(PROBE_ID));
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
            let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
            assert!(format!("{error:#}").contains("ResourceExhausted"));
            assert!(ResourceReadMaterial::active_guard().is_err());
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(phase.finish().is_err());
        assert_no_probe(&connection)?;
        assert_eq!(
            count_rows(
                &connection,
                "SELECT COUNT(*) FROM observer_sync_capability_verifications
                 WHERE verified_at = 'sentinel'",
            )?,
            count,
        );
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, before.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    verify_observer_sync_foundations(&mut connection)?;
    assert_no_probe(&connection)?;
    // Unchanged inventory reuses its complete proof instead of writing a new timestamp.
    assert_eq!(inventory_row(&connection)?, cached_inventory);
    assert_eq!(
        count_rows(
            &connection,
            "SELECT COUNT(*) FROM observer_sync_capability_verifications",
        )?,
        count,
    );
    assert_eq!(
        count_rows(
            &connection,
            "SELECT COUNT(*) FROM observer_sync_capability_verifications
             WHERE verified_at = 'sentinel'",
        )?,
        1,
    );
    Ok(())
}

#[test]
fn probe_preserves_sql_errors_false_verdicts_and_unqualified_behavior() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    for (table, admitted_body) in [
        ("agent_identity_reservations", false),
        ("agent_identities", true),
    ] {
        connection.execute_batch(&format!(
            "CREATE TRIGGER reject_probe BEFORE INSERT ON {table}
             WHEN NEW.agent_id = '__observer_sync_reservation_probe__'
             BEGIN SELECT RAISE(ABORT, 'probe_sql_fixture'); END;"
        ))?;
        let expected = if admitted_body {
            add(
                add(add(header(), timestamp_cost()), text_cost(PROBE_ID)),
                super::registry_verdict_budget_tests::probe_registry_timestamp_cost(),
            )
        } else {
            // The probe's own INSERT fails before either registry call.
            add(header(), timestamp_cost())
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), expected.0, expected.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let result = probe_reservation_enforcement(&mut connection);
            if admitted_body {
                assert!(!result?);
            } else {
                assert!(format!("{:#}", result.unwrap_err()).contains("probe_sql_fixture"));
            }
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder,
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(phase.finish()?, expected);
        assert_no_probe(&connection)?;
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, expected.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        connection.execute_batch("DROP TRIGGER reject_probe")?;
    }
    assert!(probe_reservation_enforcement(&mut connection)?);
    assert_no_probe(&connection)?;
    Ok(())
}
