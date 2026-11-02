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
use chrono::NaiveDate;
use std::sync::Arc;

fn identity() -> AgentIdentityRecord {
    let mut identity = AgentIdentityRecord::new(
        "seed-continuity-timestamp",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    identity.created_at = "2026-10-18T00:00:00.123456789Z".parse().unwrap();
    identity.updated_at = identity.created_at;
    identity
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    connection.execute_batch(
        "CREATE TABLE runtime_metadata (
             key TEXT PRIMARY KEY, value TEXT NOT NULL,
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL
         );",
    )?;
    connection.execute(
        "INSERT INTO runtime_metadata VALUES (?1, 'fixed-boundary', 'original', 'original')",
        [UPGRADE_BOUNDARY_KEY],
    )?;
    Ok(connection)
}

#[test]
fn seed_timestamp_sql_bytes_and_other_columns_match_original() -> Result<()> {
    let mut connection = connection()?;
    let identity = identity();
    let mut roots = EpisodeContinuityRoots::default();
    roots.work_item_ids.insert("work-fixed".into());
    roots.source_refs.insert("message:fixed".into());
    let mut values = vec![DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC];
    for year in [-10000, -1, 0, 9999, 10000, 262000] {
        for nanos in [0, 1_000_000, 1_000, 1, 123_456_789, 1_123_456_789] {
            values.push(
                NaiveDate::from_ymd_opt(year, 10, 18)
                    .unwrap()
                    .and_hms_nano_opt(23, 59, 59, nanos)
                    .unwrap()
                    .and_utc(),
            );
        }
    }
    for value in values {
        for with_identity in [false, true] {
            let tx = connection.transaction()?;
            let seed = insert_seed_with_candidates_tx(
                &tx,
                &identity.agent_id,
                with_identity.then_some(&identity),
                if with_identity {
                    "upgrade_identity"
                } else {
                    "upgrade_unbound"
                },
                &roots,
                with_identity,
                value,
                &[],
            )?;
            let row: (String, String, Option<String>, String, String, bool) = tx.query_row(
                "SELECT captured_at, boundary_id, identity_digest, roots_json,
                        candidate_payloads_json, import_required
                 FROM episode_continuity_seeds WHERE seed_id = ?1",
                [&seed.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
            assert_eq!(row.0, value.to_rfc3339());
            assert_eq!(row.1, "fixed-boundary");
            assert_eq!(
                row.2,
                with_identity.then(|| bytes_digest(&serde_json::to_vec(&identity).unwrap()))
            );
            assert_eq!(row.3, serde_json::to_string(&roots)?);
            assert_eq!(row.4, "{}");
            assert_eq!(row.5, with_identity);
            assert_eq!(seed.agent_id, identity.agent_id);
            assert_eq!(
                seed.incarnation,
                with_identity.then_some(identity.incarnation)
            );
            assert_eq!(seed.import_required, with_identity);
            tx.rollback()?;
        }
    }
    Ok(())
}

#[test]
fn seed_timestamp_adds_no_charge_to_exhausted_original_phase() -> Result<()> {
    let mut connection = connection()?;
    let identity = identity();
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let mut exact = ((scope.agent_id.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
    let json = super::seed_candidate_tests::seed_insert_cost(&identity.agent_id, &[])?;
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
        for with_identity in [false, true] {
            let tx = connection.transaction()?;
            let seed = insert_seed_with_candidates_tx(
                &tx,
                &identity.agent_id,
                with_identity.then_some(&identity),
                "upgrade_unbound",
                &EpisodeContinuityRoots::default(),
                false,
                identity.updated_at,
                &[],
            )?;
            assert_eq!(seed.agent_id, identity.agent_id);
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
fn seed_timestamp_preserves_constraints_duplicate_and_sql_rollback() -> Result<()> {
    for failure in ["identity", "source", "duplicate", "sql"] {
        let mut connection = connection()?;
        let mut identity = identity();
        if failure == "identity" {
            identity.incarnation = 0;
        }
        if failure == "sql" {
            connection.execute_batch(
                "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
                 BEGIN SELECT RAISE(ABORT, 'seed timestamp SQL failure'); END;",
            )?;
        }
        {
            let tx = connection.transaction()?;
            tx.execute(
                "INSERT INTO runtime_metadata VALUES ('marker', 'rollback', 'x', 'x')",
                [],
            )?;
            let write = || {
                insert_seed_with_candidates_tx(
                    &tx,
                    &identity.agent_id,
                    Some(&identity),
                    if failure == "source" {
                        "invalid"
                    } else {
                        "upgrade_identity"
                    },
                    &EpisodeContinuityRoots::default(),
                    false,
                    identity.updated_at,
                    &[],
                )
            };
            if failure == "duplicate" {
                write()?;
            }
            let error = write().err().context("seed write unexpectedly succeeded")?;
            assert!(error.downcast_ref::<rusqlite::Error>().is_some());
            if failure == "sql" {
                assert!(format!("{error:#}").contains("seed timestamp SQL failure"));
            }
            tx.rollback()?;
        }
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM episode_continuity_seeds", [], |row| {
                row.get::<_, u64>(0)
            })?,
            0
        );
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'marker'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
    }
    Ok(())
}

#[test]
fn seed_timestamp_real_identity_import_rolls_back_then_commits_on_original_phase() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = identity();
    db.connection()?.execute_batch(
        "CREATE TRIGGER reject_seed BEFORE INSERT ON episode_continuity_seeds
         BEGIN SELECT RAISE(ABORT, 'seed timestamp import failure'); END;",
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
            tx.execute("INSERT INTO runtime_metadata VALUES ('seed_timestamp_marker', 'rollback', 'x', 'x')", [])?;
            prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
            super::super::evidence::upsert_agent_identity_tx(tx, &identity)
        };
        let error = db.transaction(write).unwrap_err();
        assert!(format!("{error:#}").contains("seed timestamp import failure"));
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        for table in [
            "episode_continuity_seeds",
            "agent_episode_continuity",
            "agent_identities",
            "agent_identity_reservations",
        ] {
            assert_eq!(
                db.connection()?.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = ?1"),
                    [&identity.agent_id],
                    |row| row.get::<_, u64>(0)
                )?,
                0
            );
        }
        assert_eq!(
            db.connection()?.query_row(
                "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'seed_timestamp_marker'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            0
        );
        db.connection()?
            .execute_batch("DROP TRIGGER reject_seed;")?;
        db.transaction(write)?;
        let row: (String, String) = db.connection()?.query_row(
            "SELECT s.captured_at, c.bound_at FROM episode_continuity_seeds s
             JOIN agent_episode_continuity c ON c.seed_id = s.seed_id
             WHERE s.agent_id = ?1 AND s.source_kind = 'legacy_identity_import'",
            [&identity.agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(
            row,
            (
                identity.updated_at.to_rfc3339(),
                identity.updated_at.to_rfc3339()
            )
        );
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        used = phase.finish()?;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, used.0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
