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

fn fixture(epoch: &str) -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    // No affinity on value: exercise the reader's actual SQL type boundary.
    connection.execute_batch(
        "CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value);
         CREATE TABLE observer_sync_capability_verifications (
           capability TEXT PRIMARY KEY, verified INTEGER, verification_version INTEGER,
           event_log_epoch TEXT, event_generation INTEGER, verified_event_generation INTEGER
         );",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES ('event_log_epoch', ?1)",
        [epoch],
    )?;
    connection.execute(
        "INSERT INTO observer_sync_capability_verifications VALUES (?1, 1, ?2, ?3, 0, 0)",
        rusqlite::params![
            EVENT_PROJECTION_EFFECT_COMPLETE,
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            epoch,
        ],
    )?;
    Ok(connection)
}

#[test]
fn current_epoch_borrow_preserves_empty_unicode_and_large_identity() -> Result<()> {
    for epoch in [String::new(), "epoch_路\\\"".into(), "x".repeat(128 * 1024)] {
        let connection = fixture(&epoch)?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            Some(true)
        );
        connection.execute(
            "UPDATE runtime_metadata SET value = value || 'different'",
            [],
        )?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            None
        );
        connection.execute("UPDATE runtime_metadata SET value = ?1", [&epoch])?;
        connection.execute(
            "UPDATE observer_sync_capability_verifications SET verified = 0",
            [],
        )?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            Some(false)
        );
        assert!(connection.is_autocommit());
    }
    Ok(())
}

#[test]
fn current_epoch_errors_release_rows_and_statements_without_hiding_sql_failures() -> Result<()> {
    let connection = fixture("epoch")?;
    for value in ["NULL", "17", "X'65706f6368'"] {
        connection.execute(&format!("UPDATE runtime_metadata SET value = {value}"), [])?;
        assert!(current_event_projection_effect_verification(&connection).is_err());
        // A failed borrowed read must not retain a statement or read transaction.
        connection.execute("UPDATE runtime_metadata SET value = 'epoch'", [])?;
        assert_eq!(
            current_event_projection_effect_verification(&connection)?,
            Some(true)
        );
        assert!(connection.is_autocommit());
    }
    connection.execute("DELETE FROM runtime_metadata", [])?;
    assert_eq!(
        current_event_projection_effect_verification(&connection)
            .unwrap_err()
            .to_string(),
        "runtime metadata key event_log_epoch is missing"
    );
    connection.execute(
        "INSERT INTO runtime_metadata VALUES ('event_log_epoch', 'epoch')",
        [],
    )?;
    connection.execute("DELETE FROM observer_sync_capability_verifications", [])?;
    assert_eq!(
        current_event_projection_effect_verification(&connection)?,
        None
    );
    connection.execute_batch("DROP TABLE observer_sync_capability_verifications")?;
    assert!(current_event_projection_effect_verification(&connection).is_err());
    connection.execute_batch("DROP TABLE runtime_metadata")?;
    assert!(current_event_projection_effect_verification(&connection).is_err());
    Ok(())
}

#[test]
fn scalar_epoch_read_reuses_exhausted_caller_phase_and_restores_tls() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "foundation-agent".into(),
        identity_incarnation: 1,
    };
    let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
    let exact = (header.0 * 2 + 17 * 8, header.1 * 2 + 8);
    let connection = fixture(&"epoch_路".repeat(8192))?;
    for maximum in [exact, (exact.0, exact.1 + 8), (exact.0 + 8, exact.1)] {
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&budget, &scope, maximum.0, maximum.1)?;
        let transaction = phase.transaction()?;
        {
            let _outer = phase.enter();
            {
                let _inner = transaction.enter();
                charge_owned_metadata(17, 1)?;
                assert_eq!(
                    current_event_projection_effect_verification(&connection)?,
                    Some(true)
                );
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &transaction.guard()?,
                ));
                connection.execute("UPDATE runtime_metadata SET value = NULL", [])?;
                assert!(current_event_projection_effect_verification(&connection).is_err());
                connection.execute(
                    "UPDATE runtime_metadata SET value =
                     (SELECT event_log_epoch FROM observer_sync_capability_verifications)",
                    [],
                )?;
            }
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &phase.guard()?,
            ));
            assert_eq!(phase.finish()?, exact);
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        charge_owned_metadata(1, 1)?;
        assert_eq!(phase.finish()?, exact);
        assert_eq!(
            transaction.finish()?,
            (exact.0 - header.0, exact.1 - header.1)
        );
        let first = Arc::new(vec![phase.guard()?, transaction.guard()?]);
        let last = first.clone();
        drop((phase, transaction));
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(first);
        assert_eq!(budget.report()?.observed_bytes, exact.0);
        drop(last);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn public_foundations_degrade_only_epoch_proof_and_recover_after_read_errors() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    {
        let connection = db.connection()?;
        connection.execute(
            "UPDATE observer_sync_capability_verifications
             SET verified = CASE WHEN capability IN (?1, ?2, ?3) THEN 1 ELSE 0 END",
            rusqlite::params![
                RUNTIME_IDENTITY_STABLE,
                ROSTER_SNAPSHOT_VERIFIED,
                CONVERSATION_READ_VERIFIED,
            ],
        )?;
        connection.execute(
            "UPDATE observer_sync_capability_verifications
             SET verified = 1, verification_version = ?1,
                 event_log_epoch = (SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'),
                 event_generation = 0, verified_event_generation = 0
             WHERE capability = ?2",
            rusqlite::params![
                EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
                EVENT_PROJECTION_EFFECT_COMPLETE,
            ],
        )?;
    }
    let check = |proof| -> Result<()> {
        let actual = db.observer_sync_foundations()?;
        assert_eq!(actual.event_projection_effect_complete, proof);
        assert!(actual.runtime_identity_stable);
        assert!(!actual.agent_identity_reserved);
        assert!(actual.roster_snapshot_verified);
        assert!(!actual.projection_snapshot_verified);
        assert!(!actual.brief_atomic_linkage_verified);
        assert!(actual.conversation_read_verified);
        Ok(())
    };
    check(true)?;
    {
        let connection = db.connection()?;
        connection.execute("ALTER TABLE runtime_metadata RENAME TO saved_metadata", [])?;
    }
    check(false)?;
    {
        let connection = db.connection()?;
        connection.execute("ALTER TABLE saved_metadata RENAME TO runtime_metadata", [])?;
    }
    check(true)?;
    db.connection()?.execute(
        "UPDATE runtime_metadata SET value = X'65706f6368' WHERE key = 'event_log_epoch'",
        [],
    )?;
    check(false)?;
    db.connection()?.execute(
        "DELETE FROM runtime_metadata WHERE key = 'event_log_epoch'",
        [],
    )?;
    check(false)?;
    Ok(())
}
