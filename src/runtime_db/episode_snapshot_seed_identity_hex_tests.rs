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
use rusqlite::types::{ToSql, ToSqlOutput, ValueRef};
use std::sync::Arc;

const AGENT: &str = "seed-identity-hex";

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
         CREATE TABLE hex_markers (value TEXT);",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES (?1, 'hex-boundary')",
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

fn original_hex(identity: &AgentIdentityRecord) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(identity)?)
    ))
}

fn stored_hex(connection: &Connection) -> Result<Option<String>> {
    Ok(connection.query_row(
        "SELECT identity_digest FROM episode_continuity_seeds",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn seed_identity_hex_is_fixed_borrowed_text_with_original_bytes_and_null() -> Result<()> {
    assert_eq!(std::mem::size_of::<SeedIdentityDigest>(), 64);
    assert!(!std::mem::needs_drop::<SeedIdentityDigest>());
    let mut connection = connection()?;
    let mut identity = identity()?;
    for index in 0..128 {
        identity.revision = index;
        identity.name = (index % 2 == 0).then(|| "\"\\\0\n界İß\u{2028}".repeat(index as usize));
        let digest = seed_identity_digest(&identity)?;
        let ToSqlOutput::Borrowed(ValueRef::Text(text)) = digest.to_sql()? else {
            panic!("identity hex must bind its original bytes as borrowed SQLite TEXT");
        };
        assert_eq!(text.as_ptr(), digest.0.as_ptr());
        assert_eq!(text.len(), 64);
        assert!(text
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)));
        let expected = original_hex(&identity)?;
        assert_eq!(text, expected.as_bytes());
        let tx = connection.transaction()?;
        insert(&tx, Some(&identity))?;
        let (kind, length): (String, usize) = tx.query_row(
            "SELECT typeof(identity_digest), length(identity_digest)
             FROM episode_continuity_seeds",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!((kind.as_str(), length), ("text", 64));
        assert_eq!(stored_hex(&tx)?.as_deref(), Some(expected.as_str()));
        tx.rollback()?;
    }
    let tx = connection.transaction()?;
    insert(&tx, None)?;
    assert_eq!(stored_hex(&tx)?, None);
    assert_eq!(
        tx.query_row(
            "SELECT typeof(identity_digest) FROM episode_continuity_seeds",
            [],
            |row| row.get::<_, String>(0)
        )?,
        "null"
    );
    tx.rollback()?;
    Ok(())
}

#[test]
fn seed_identity_hex_keeps_exhausted_caller_phase_through_sql_and_error() -> Result<()> {
    let identity = identity()?;
    let expected = original_hex(&identity)?;
    let mut connection = connection()?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: identity.incarnation,
    };
    let mut exact = ((scope.agent_id.len() as u64 + 256 + 29) * 8, 64 + 8);
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
        charge_owned_metadata(29, 1)?;
        for fail in [false, true] {
            if fail {
                connection.execute_batch(
                    "CREATE TRIGGER reject_hex BEFORE INSERT ON episode_continuity_seeds
                     BEGIN SELECT RAISE(ABORT, 'hex SQL failure'); END;",
                )?;
            }
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO hex_markers VALUES ('rollback')", [])?;
            let result = insert(&tx, Some(&identity));
            if fail {
                let error = result.err().context("seed unexpectedly inserted")?;
                assert!(error.downcast_ref::<rusqlite::Error>().is_some());
                assert!(format!("{error:#}").contains("hex SQL failure"));
            } else {
                result?;
                assert_eq!(stored_hex(&tx)?.as_deref(), Some(expected.as_str()));
            }
            // Both successful and failed statements must release the boundary reader.
            tx.execute_batch("DROP TABLE runtime_metadata;")?;
            tx.rollback()?;
            for table in ["hex_markers", "episode_continuity_seeds"] {
                assert_eq!(
                    connection
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                            .get::<_, u64>(0))?,
                    0
                );
            }
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
fn seed_identity_hex_duplicate_keeps_committed_digest_and_rolls_back_transaction() -> Result<()> {
    let identity = identity()?;
    let expected = original_hex(&identity)?;
    let mut connection = connection()?;
    let tx = connection.transaction()?;
    let seed = insert(&tx, Some(&identity))?;
    tx.commit()?;
    let tx = connection.transaction()?;
    tx.execute("INSERT INTO hex_markers VALUES ('rollback')", [])?;
    let error = insert(&tx, None).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(code, _))
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    ));
    tx.execute_batch("DROP TABLE runtime_metadata;")?;
    tx.rollback()?;
    assert_eq!(stored_hex(&connection)?.as_deref(), Some(expected.as_str()));
    assert_eq!(find_seed(&connection, AGENT)?.unwrap().id, seed.id);
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM hex_markers", [], |row| {
            row.get::<_, u64>(0)
        })?,
        0
    );
    Ok(())
}

#[test]
fn seed_identity_hex_real_import_rolls_back_late_failure_then_binds_original_text() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = identity()?;
    let expected = original_hex(&identity)?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_identity AFTER INSERT ON agent_identities
         BEGIN SELECT RAISE(ABORT, 'hex late import failure'); END;",
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
                "INSERT INTO runtime_metadata VALUES ('hex_marker', 'x', 'x', 'x')",
                [],
            )?;
            prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)
        };
        let error = db.transaction(write).unwrap_err();
        assert!(format!("{error:#}").contains("hex late import failure"));
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
                "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'hex_marker'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        db.connection()?
            .execute_batch("DROP TRIGGER reject_identity;")?;
        db.transaction(write)?;
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM episode_continuity_seeds AS seed
                 JOIN agent_episode_continuity AS binding ON binding.seed_id = seed.seed_id
                 JOIN agent_identities AS identity ON identity.agent_id = binding.agent_id
                 WHERE seed.agent_id = ?1 AND seed.identity_digest = ?2
                   AND typeof(seed.identity_digest) = 'text'
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
