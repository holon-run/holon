use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use std::sync::Arc;

const AGENT: &str = "seed-identity-preimage";

fn identity() -> Result<AgentIdentityRecord> {
    let mut identity = AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    identity.created_at = "2026-10-18T00:00:00.123456789Z".parse()?;
    identity.updated_at = identity.created_at;
    Ok(identity)
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection.execute_batch(
        "CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE identity_markers (value TEXT);",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES (?1, 'identity-boundary')",
        [UPGRADE_BOUNDARY_KEY],
    )?;
    Ok(connection)
}

fn insert(tx: &Transaction<'_>, identity: Option<&AgentIdentityRecord>) -> Result<ContinuitySeed> {
    insert_seed_with_candidates_tx(
        tx,
        AGENT,
        identity,
        "upgrade_unbound",
        &EpisodeContinuityRoots::default(),
        false,
        "2026-10-18T00:00:00Z".parse()?,
        &[],
    )
}

fn original_digest(identity: &AgentIdentityRecord) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(identity)?)
    ))
}

fn stored_digest(connection: &Connection) -> Result<Option<String>> {
    Ok(connection.query_row(
        "SELECT identity_digest FROM episode_continuity_seeds",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn seed_identity_stream_preserves_full_preimage_and_sql_null() -> Result<()> {
    let mut connection = connection()?;
    let base = identity()?;
    let mut detailed = base.clone();
    detailed.name = Some("\"\\\0\n界İß\u{2028}".repeat(4096));
    detailed.parent_agent_id = Some("parent\0\"\\".into());
    detailed.lineage_parent_agent_id = Some("lineage-界".into());
    detailed.delegated_from_task_id = Some("task\n".into());
    detailed.revision = u64::MAX;
    detailed.incarnation = 42;
    detailed.status = AgentRegistryStatus::Deleting;
    detailed.updated_at = "2026-10-18T00:00:01.000001Z".parse()?;
    detailed.deleted_at = Some(detailed.updated_at);
    let mut sparse = detailed.clone();
    sparse.name = None;
    sparse.ownership = None;
    sparse.profile_preset = None;
    sparse.durability = None;
    sparse.parent_agent_id = None;
    sparse.lineage_parent_agent_id = None;
    sparse.delegated_from_task_id = None;
    sparse.deleted_at = None;
    for identity in [None, Some(&base), Some(&detailed), Some(&sparse)] {
        let expected = identity.map(original_digest).transpose()?;
        if let Some(identity) = identity {
            assert_eq!(
                std::str::from_utf8(&seed_identity_digest(identity)?.0)?,
                expected.as_deref().unwrap()
            );
        }
        let tx = connection.transaction()?;
        insert(&tx, identity)?;
        assert_eq!(stored_digest(&tx)?, expected);
        let incarnation: Option<i64> = tx.query_row(
            "SELECT identity_incarnation FROM episode_continuity_seeds",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            incarnation,
            identity.map(|identity| identity.incarnation as i64)
        );
        tx.rollback()?;
    }
    Ok(())
}

#[test]
fn seed_identity_stream_keeps_exhausted_original_phase_and_sql_error_rollback() -> Result<()> {
    let identity = identity()?;
    let original = serde_json::to_vec(&identity)?;
    let expected = original_digest(&identity)?;
    let mut connection = connection()?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: identity.incarnation,
    };
    let mut exact = ((scope.agent_id.len() as u64 + 256 + 23) * 8, 64 + 8);
    let json = super::seed_candidate_tests::seed_insert_cost(AGENT, &[])?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        exact.0 + 2 * json.0,
        exact.1 + 2 * json.1,
    )?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(23, 1)?;
        for fail in [false, true] {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'identity preimage SQL failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO identity_markers VALUES ('rollback')", [])?;
            let result = insert(&tx, Some(&identity));
            if fail {
                let error = result.err().context("seed unexpectedly inserted")?;
                assert!(error.downcast_ref::<rusqlite::Error>().is_some());
                assert!(format!("{error:#}").contains("identity preimage SQL failure"));
            } else {
                result?;
                assert_eq!(stored_digest(&tx)?.as_deref(), Some(expected.as_str()));
            }
            tx.execute_batch("DROP TABLE runtime_metadata;")?;
            tx.rollback()?;
            for table in ["identity_markers", "episode_continuity_seeds"] {
                let count: u64 =
                    connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 0);
            }
            exact.0 += json.0;
            exact.1 += json.1;
            assert_eq!(phase.finish()?, exact);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            assert_eq!(serde_json::to_vec(&identity)?, original);
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
fn seed_identity_stream_preserves_duplicate_constraint_and_original_transaction() -> Result<()> {
    let identity = identity()?;
    let expected = original_digest(&identity)?;
    let mut connection = connection()?;
    let tx = connection.transaction()?;
    let seed = insert(&tx, Some(&identity))?;
    tx.commit()?;
    let tx = connection.transaction()?;
    tx.execute("INSERT INTO identity_markers VALUES ('rollback')", [])?;
    let error = insert(&tx, None).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::ConstraintViolation
    ));
    assert_eq!(stored_digest(&tx)?.as_deref(), Some(expected.as_str()));
    tx.execute_batch("DROP TABLE runtime_metadata;")?;
    tx.rollback()?;
    assert_eq!(
        stored_digest(&connection)?.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        connection.query_row("SELECT seed_id FROM episode_continuity_seeds", [], |row| {
            row.get::<_, String>(0)
        })?,
        seed.id
    );
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM identity_markers", [], |row| {
            row.get::<_, u64>(0)
        })?,
        0
    );
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM runtime_metadata", [], |row| {
            row.get::<_, u64>(0)
        })?,
        1
    );
    Ok(())
}

#[test]
fn seed_identity_stream_real_import_keeps_digest_phase_and_atomic_binding() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = identity()?;
    let expected = original_digest(&identity)?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
         BEGIN SELECT RAISE(ABORT, 'identity preimage import failure'); END;",
    )?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: identity.incarnation,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let holder = phase.guard()?;
    let used;
    {
        let _entry = phase.enter();
        let write = |tx: &Transaction<'_>| -> Result<()> {
            tx.execute(
                "INSERT INTO runtime_metadata VALUES ('identity_marker', 'x', 'x', 'x')",
                [],
            )?;
            prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)
        };
        let error = db.transaction(write).unwrap_err();
        assert!(format!("{error:#}").contains("identity preimage import failure"));
        for table in [
            "episode_continuity_seeds",
            "agent_episode_continuity",
            "agent_identities",
            "agent_identity_reservations",
        ] {
            assert_eq!(
                db.connection()?.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = ?1"),
                    [AGENT],
                    |row| row.get::<_, u64>(0)
                )?,
                0
            );
        }
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'identity_marker'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        db.connection()?
            .execute_batch("DROP TRIGGER reject_seed;")?;
        db.transaction(write)?;
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM episode_continuity_seeds AS seed
                 JOIN agent_episode_continuity AS binding ON binding.seed_id = seed.seed_id
                 JOIN agent_identities AS identity ON identity.agent_id = binding.agent_id
                 WHERE seed.agent_id = ?1 AND seed.identity_digest = ?2
                   AND binding.identity_incarnation = ?3 AND seed.identity_incarnation = ?3",
                params![AGENT, expected, identity.incarnation],
                |row| row.get::<_, u64>(0)
            )?,
            1
        );
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        used = phase.finish()?;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert!(used.0 > 0 && used.1 > 0);
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
