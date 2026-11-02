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

const AGENT: &str = "seed-boundary";

fn connection(boundary: &str) -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection.execute_batch(
        "CREATE TABLE runtime_metadata (
             key TEXT PRIMARY KEY, value TEXT NOT NULL,
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL
         );
         CREATE TABLE boundary_markers (value TEXT);",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES (?1, ?2, 'original', 'original')",
        params![UPGRADE_BOUNDARY_KEY, boundary],
    )?;
    Ok(connection)
}

fn insert(tx: &Transaction<'_>) -> Result<ContinuitySeed> {
    insert_seed_with_candidates_tx(
        tx,
        AGENT,
        None,
        "upgrade_unbound",
        &EpisodeContinuityRoots::default(),
        false,
        "2026-10-18T00:00:00.123456789Z".parse()?,
        &[],
    )
}

fn original_seed_id(boundary: &str, agent: &str) -> Result<String> {
    Ok(format!(
        "episode_seed_{}",
        bytes_digest(&serde_json::to_vec(&(
            boundary,
            agent,
            EPISODE_CONTINUITY_POLICY
        ))?)
    ))
}

#[test]
fn seed_boundary_borrow_preserves_original_id_and_sql_bytes() -> Result<()> {
    for boundary in ["".into(), "\"\\\n\0中文İß".into(), "界\0".repeat(16_384)] {
        let mut connection = connection(&boundary)?;
        let tx = connection.transaction()?;
        let seed = insert(&tx)?;
        assert_eq!(seed.id, original_seed_id(&boundary, AGENT)?);
        assert_eq!(seed.agent_id, AGENT);
        assert_eq!(seed.incarnation, None);
        assert!(!seed.import_required);
        let stored: (String, String, String, String) = tx.query_row(
            "SELECT seed_id, boundary_id, candidate_payloads_json, captured_at
             FROM episode_continuity_seeds WHERE agent_id = ?1",
            [AGENT],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(stored.0, seed.id);
        assert_eq!(stored.1.as_bytes(), boundary.as_bytes());
        assert_eq!(stored.2, "{}");
        assert_eq!(stored.3, "2026-10-18T00:00:00.123456789+00:00");
        // The synchronous boundary reader must not survive the seed INSERT.
        tx.execute_batch("DROP TABLE runtime_metadata;")?;
        tx.rollback()?;
    }
    Ok(())
}

#[test]
fn seed_boundary_borrow_uses_no_remaining_original_phase_budget() -> Result<()> {
    let mut connection = connection(&"界\0".repeat(16_384))?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let mut exact = ((scope.agent_id.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
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
        charge_owned_metadata(31, 3)?;
        for fail in [false, true] {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'boundary insert failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            let result = insert(&tx);
            if fail {
                let error = result.err().context("seed INSERT unexpectedly succeeded")?;
                assert!(format!("{error:#}").contains("boundary insert failure"));
            } else {
                assert_eq!(result?.agent_id, AGENT);
            }
            tx.rollback()?;
            exact.0 += json.0;
            exact.1 += json.1;
            assert_eq!(phase.finish()?, exact);
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
fn seed_boundary_read_errors_release_reader_and_rollback_original_transaction() -> Result<()> {
    for failure in ["missing", "schema", "type", "insert"] {
        let mut connection = connection("fixed-boundary")?;
        match failure {
            "missing" => connection.execute_batch("DELETE FROM runtime_metadata;")?,
            "schema" => connection.execute_batch("DROP TABLE runtime_metadata;")?,
            "type" => connection.execute_batch("UPDATE runtime_metadata SET value = x'01';")?,
            "insert" => connection.execute_batch(
                "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                 BEGIN SELECT RAISE(ABORT, 'boundary SQL failure'); END;",
            )?,
            _ => unreachable!(),
        }
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO boundary_markers VALUES ('rollback')", [])?;
        let error = insert(&tx).err().context("seed unexpectedly inserted")?;
        match failure {
            "missing" => assert!(matches!(
                error.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::QueryReturnedNoRows)
            )),
            "type" => assert!(matches!(
                error.downcast_ref::<rusqlite::types::FromSqlError>(),
                Some(rusqlite::types::FromSqlError::InvalidType)
            )),
            _ => assert!(error.downcast_ref::<rusqlite::Error>().is_some()),
        }
        if failure == "insert" {
            assert!(format!("{error:#}").contains("boundary SQL failure"));
        }
        if failure != "schema" {
            tx.execute_batch("DROP TABLE runtime_metadata;")?;
        }
        tx.rollback()?;
        for table in ["boundary_markers", "episode_continuity_seeds"] {
            assert_eq!(
                connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, u64>(0)
                })?,
                0
            );
        }
    }
    Ok(())
}

#[test]
fn seed_boundary_real_identity_import_retains_original_phase_and_atomicity() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = AgentIdentityRecord::new(
        AGENT,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let boundary = "boundary-界\0\"\\".repeat(1024);
    assert_eq!(
        db.connection()?.execute(
            "UPDATE runtime_metadata SET value = ?1 WHERE key = ?2",
            params![boundary, UPGRADE_BOUNDARY_KEY],
        )?,
        1
    );
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
         BEGIN SELECT RAISE(ABORT, 'boundary import failure'); END;",
    )?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
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
                "INSERT INTO runtime_metadata VALUES ('boundary_marker', 'x', 'x', 'x')",
                [],
            )?;
            prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)
        };
        let error = db.transaction(write).unwrap_err();
        assert!(format!("{error:#}").contains("boundary import failure"));
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
                "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'boundary_marker'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        db.connection()?
            .execute_batch("DROP TRIGGER reject_seed;")?;
        db.transaction(write)?;
        let stored: (String, String) = db.connection()?.query_row(
            "SELECT s.seed_id, s.boundary_id FROM episode_continuity_seeds s
             JOIN agent_episode_continuity c ON c.seed_id = s.seed_id
             JOIN agent_identities i ON i.agent_id = s.agent_id
             WHERE s.agent_id = ?1 AND s.source_kind = 'legacy_identity_import'",
            [AGENT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored.0, original_seed_id(&boundary, AGENT)?);
        assert_eq!(stored.1, boundary);
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
