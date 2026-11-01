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

const AGENT: &str = "seed-id";

#[path = "episode_snapshot_seed_ownership_tests.rs"]
mod ownership_tests;

#[path = "episode_snapshot_seed_loaded_tests.rs"]
mod loaded_tests;

fn connection(boundary: &str) -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection.execute_batch(
        "CREATE TABLE runtime_metadata (
             key TEXT PRIMARY KEY, value TEXT NOT NULL,
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL
         );
         CREATE TABLE seed_id_markers (value TEXT);",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES (?1, ?2, 'original', 'original')",
        params![UPGRADE_BOUNDARY_KEY, boundary],
    )?;
    Ok(connection)
}

fn insert(tx: &Transaction<'_>, agent: &str) -> Result<ContinuitySeed> {
    insert_seed_with_candidates_tx(
        tx,
        agent,
        None,
        "upgrade_unbound",
        &EpisodeContinuityRoots::default(),
        false,
        "2026-10-18T00:00:00.123456789Z".parse()?,
        &[],
    )
}

fn original_seed_id(boundary: &str, agent: &str) -> Result<String> {
    let preimage = serde_json::to_vec(&(boundary, agent, EPISODE_CONTINUITY_POLICY))?;
    Ok(format!("episode_seed_{:x}", Sha256::digest(&preimage)))
}

#[test]
fn seed_id_stream_preserves_original_json_hash_and_sql_identity() -> Result<()> {
    assert_eq!(
        original_seed_id("fixed-boundary", AGENT)?,
        "episode_seed_4846940fffa62f63588161ff556b8e7c37809ea1a6a84bce605dd02faf0e99cc"
    );
    let controls = (0u8..32).map(char::from).collect::<String>();
    let mut ids = BTreeSet::new();
    for (boundary, agent) in [
        ("fixed-boundary".into(), AGENT.into()),
        (String::new(), String::new()),
        ("a".into(), "bc".into()),
        ("ab".into(), "c".into()),
        (controls, "\"\\\0中文İß\u{2028}\u{2029}".into()),
        ("界\0\"\\\n".repeat(16_384), "agent\0界".repeat(1024)),
    ] {
        let mut connection = connection(&boundary)?;
        let tx = connection.transaction()?;
        let seed = insert(&tx, &agent)?;
        let expected = original_seed_id(&boundary, &agent)?;
        assert_eq!(seed.id, expected);
        assert_eq!(seed.agent_id, agent);
        assert_eq!(seed.incarnation, None);
        assert!(!seed.import_required);
        let stored: (String, String, String, i64) = tx.query_row(
            "SELECT seed_id, agent_id, boundary_id, policy_version
             FROM episode_continuity_seeds",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(
            stored,
            (expected, agent, boundary, EPISODE_CONTINUITY_POLICY)
        );
        assert!(ids.insert(seed.id));
        tx.commit()?;
    }
    Ok(())
}

#[test]
fn seed_id_stream_keeps_exhausted_original_phase_and_last_holder() -> Result<()> {
    let boundary = "界\0\"\\\n".repeat(16_384);
    let mut connection = connection(&boundary)?;
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
                     BEGIN SELECT RAISE(ABORT, 'seed ID insert failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            let result = insert(&tx, AGENT);
            if fail {
                let error = result.err().context("seed INSERT unexpectedly succeeded")?;
                assert!(format!("{error:#}").contains("seed ID insert failure"));
            } else {
                assert_eq!(result?.id, original_seed_id(&boundary, AGENT)?);
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
fn seed_id_stream_conflict_and_sql_error_release_reader_and_rollback() -> Result<()> {
    for conflict in [true, false] {
        let mut connection = connection("fixed-boundary")?;
        let existing_id = if conflict {
            let tx = connection.transaction()?;
            let seed = insert(&tx, AGENT)?;
            tx.commit()?;
            Some(seed.id)
        } else {
            connection.execute_batch(
                "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                 BEGIN SELECT RAISE(ABORT, 'seed ID SQL failure'); END;",
            )?;
            None
        };
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO seed_id_markers VALUES ('rollback')", [])?;
        let error = insert(&tx, AGENT)
            .err()
            .context("seed unexpectedly inserted")?;
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        assert!(format!("{error:#}").contains(if conflict {
            "UNIQUE constraint failed"
        } else {
            "seed ID SQL failure"
        }));
        tx.execute_batch("DROP TABLE runtime_metadata;")?;
        tx.rollback()?;
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM seed_id_markers", [], |row| {
                row.get::<_, u64>(0)
            })?,
            0
        );
        let mut statement = connection.prepare("SELECT seed_id FROM episode_continuity_seeds")?;
        let stored = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(stored, existing_id.into_iter().collect::<Vec<_>>());
        assert_eq!(
            connection.query_row("SELECT value FROM runtime_metadata", [], |row| {
                row.get::<_, String>(0)
            })?,
            "fixed-boundary"
        );
    }
    Ok(())
}

#[test]
fn seed_id_stream_real_identity_import_retains_phase_and_atomic_binding() -> Result<()> {
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
    let boundary = "seed-id-界\0\"\\".repeat(1024);
    assert_eq!(
        db.connection()?.execute(
            "UPDATE runtime_metadata SET value = ?1 WHERE key = ?2",
            params![boundary, UPGRADE_BOUNDARY_KEY],
        )?,
        1
    );
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
         BEGIN SELECT RAISE(ABORT, 'seed ID import failure'); END;",
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
                "INSERT INTO runtime_metadata VALUES ('seed_id_marker', 'x', 'x', 'x')",
                [],
            )?;
            prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)
        };
        let error = db.transaction(write).unwrap_err();
        assert!(format!("{error:#}").contains("seed ID import failure"));
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
                "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'seed_id_marker'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        db.connection()?
            .execute_batch("DROP TRIGGER reject_seed;")?;
        db.transaction(write)?;
        let stored: (String, String, String) = db.connection()?.query_row(
            "SELECT s.seed_id, c.seed_id, s.boundary_id FROM episode_continuity_seeds s
             JOIN agent_episode_continuity c ON c.seed_id = s.seed_id
             JOIN agent_identities i ON i.agent_id = s.agent_id
             WHERE s.agent_id = ?1 AND s.source_kind = 'legacy_identity_import'",
            [AGENT],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let expected = original_seed_id(&boundary, AGENT)?;
        assert_eq!(stored, (expected.clone(), expected, boundary));
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
