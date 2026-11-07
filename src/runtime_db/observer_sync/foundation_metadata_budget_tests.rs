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
        agent_id: "foundation-metadata".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn cost(value: &str) -> (u64, usize) {
    (
        ((value.len() + std::mem::size_of::<String>()) as u64) * 8,
        8,
    )
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

#[test]
fn metadata_copies_use_exact_original_remainder_and_last_holder() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch("CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value TEXT)")?;
    for value in ["", "runtime_fixture", "epoch_汉字_\"\\\n"] {
        connection.execute(
            "INSERT OR REPLACE INTO runtime_metadata VALUES ('fixture', ?1)",
            [value],
        )?;
        let used = (19 * 8, 2 * 8);
        let exact = add(add(header(), used), cost(value));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(19, 2)?;
            assert_eq!(read_metadata(&connection, "fixture")?, value);
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap(),
            ));
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(phase.finish()?, exact);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(read_metadata(&connection, "fixture")?, value);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}

#[test]
fn real_verifier_rejects_each_metadata_copy_before_any_write() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verified_at = 'sentinel'",
        [],
    )?;
    let count = count_rows(
        &connection,
        "SELECT COUNT(*) FROM observer_sync_capability_verifications",
    )?;
    let used = (29 * 8, 3 * 8);
    let mut before = add(header(), used);
    for key in [
        "runtime_id",
        "event_log_epoch",
        "visibility_policy_generation",
    ] {
        // Independent SQL text lengths, not the production metadata reader.
        let value: String = connection.query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        let exact = add(before, cost(&value));
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
                charge_owned_metadata(29, 3)?;
                let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
                assert!(format!("{error:#}").contains("ResourceExhausted"), "{key}");
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(phase.finish().is_err());
            assert!(connection.is_autocommit());
            assert_eq!(
                count_rows(
                    &connection,
                    "SELECT COUNT(*) FROM observer_sync_capability_verifications
                     WHERE verified_at = 'sentinel'",
                )?,
                count,
            );
            assert_eq!(
                count_rows(
                    &connection,
                    "SELECT COUNT(*) FROM agent_identity_reservations
                     WHERE agent_id = '__observer_sync_reservation_probe__'",
                )?,
                0,
            );
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
fn metadata_sql_key_and_type_errors_do_not_charge_or_disturb_outer_tls() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let outer = ResourceReadMaterial::new(&ledger, &scope())?;
    let outer_holder = outer.guard()?;
    {
        let _outer_entry = outer.enter();
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        {
            let _entry = phase.enter();
            assert!(read_metadata(&connection, "missing-table").is_err());
            connection.execute_batch(
                "CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value);
                 INSERT INTO runtime_metadata VALUES ('blob', X'80'), ('null', NULL)",
            )?;
            let missing = read_metadata(&connection, "absent").unwrap_err();
            assert_eq!(
                missing.to_string(),
                "runtime metadata key absent is missing"
            );
            assert!(read_metadata(&connection, "blob").is_err());
            assert!(read_metadata(&connection, "null").is_err());
            assert_eq!(phase.finish()?, header());
        }
        assert!(Arc::ptr_eq(
            &outer_holder,
            &ResourceReadMaterial::active_guard()?.unwrap(),
        ));
        drop(phase);
        // Child registration consumes the parent's original allowance;
        // dropping the child does not refund its admission.
        assert_eq!(outer.finish()?, add(header(), header()));
        assert_eq!(ledger.report()?.observed_bytes, header().0 * 2);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((outer, outer_holder));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn stack_missing_sources_preserve_all_ordered_detail_combinations() -> Result<()> {
    let sources = ["agent_identities", "agent_states", "agent_deletion_jobs"];
    for mask in 0..8 {
        let (_home, db) = fixture()?;
        let mut connection = db.connection()?;
        let expected: Vec<_> = sources
            .iter()
            .enumerate()
            .filter_map(|(index, table)| (mask & (1 << index) != 0).then_some(*table))
            .collect();
        connection.execute_batch("PRAGMA foreign_keys = OFF")?;
        for table in &expected {
            connection.execute_batch(&format!("DROP TABLE {table}"))?;
        }
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            verify_observer_sync_foundations(&mut connection)?;
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap(),
            ));
        }
        assert!(connection.is_autocommit());
        let (verified, detail): (bool, String) = connection.query_row(
            "SELECT verified, detail FROM observer_sync_capability_verifications
             WHERE capability = ?1",
            [AGENT_IDENTITY_RESERVED],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let detail: serde_json::Value = serde_json::from_str(&detail)?;
        assert_eq!(detail["missing_source_tables"], serde_json::json!(expected));
        assert_eq!(verified, mask == 0);
        let retained = phase.finish()?.0;
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, retained);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}
