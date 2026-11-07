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
use chrono::{Datelike, NaiveDate};
use std::sync::Arc;

fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        "legacy-continuity-timestamp",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    Ok(connection)
}

fn seed(
    connection: &Connection,
    identity: &AgentIdentityRecord,
    incarnation: Option<u64>,
    import_required: bool,
) -> Result<ContinuitySeed> {
    connection.execute(
        "INSERT INTO episode_continuity_seeds (
            seed_id, agent_id, policy_version, boundary_id, identity_incarnation, source_kind,
            roots_json, candidate_payloads_json, import_required, captured_at
         ) VALUES ('legacy-timestamp-seed', ?1, 1, 'boundary', ?2, 'upgrade_unbound',
                   '{}', '{}', ?3, 'original')",
        params![
            identity.agent_id,
            incarnation.map(i64::try_from).transpose()?,
            import_required
        ],
    )?;
    Ok(ContinuitySeed {
        id: "legacy-timestamp-seed".into(),
        agent_id: identity.agent_id.clone(),
        incarnation,
        import_required,
        _material: ResourceReadMaterial::active_guard()?,
    })
}

fn binding(connection: &Connection, incarnation: u64) -> Result<(String, String, String)> {
    Ok(connection.query_row(
        "SELECT mode, seed_id, bound_at FROM agent_episode_continuity
         WHERE identity_incarnation = ?1",
        [incarnation],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?)
}

#[test]
fn legacy_timestamp_sql_bytes_match_original_for_wide_years_fractions_and_leap_seconds(
) -> Result<()> {
    let mut connection = connection()?;
    let mut identity = identity();
    let seed = seed(&connection, &identity, None, false)?;
    let mut values = vec![DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC];
    for year in [
        DateTime::<Utc>::MIN_UTC.year(),
        -10000,
        -1,
        0,
        9999,
        10000,
        DateTime::<Utc>::MAX_UTC.year(),
    ] {
        for nanos in [
            0,
            1_000_000,
            1_000,
            1,
            123_456_789,
            1_000_000_000,
            1_123_456_789,
        ] {
            values.push(
                NaiveDate::from_ymd_opt(year, 10, 17)
                    .unwrap()
                    .and_hms_nano_opt(23, 59, 59, nanos)
                    .unwrap()
                    .and_utc(),
            );
        }
    }
    for (index, value) in values.into_iter().enumerate() {
        identity.incarnation = (index + 1) as u64;
        identity.status = if index % 2 == 0 {
            AgentRegistryStatus::Active
        } else {
            AgentRegistryStatus::Deleting
        };
        let tx = connection.transaction()?;
        insert_legacy_binding_tx(&tx, &identity, &seed, value)?;
        assert_eq!(
            binding(&tx, identity.incarnation)?,
            ("legacy_adopted".into(), seed.id.clone(), value.to_rfc3339())
        );
        tx.rollback()?;
    }
    Ok(())
}

#[test]
fn legacy_timestamp_uses_no_remaining_budget_preserves_tls_and_last_holder() -> Result<()> {
    let mut connection = connection()?;
    let mut identity = identity();
    let seed = seed(&connection, &identity, None, false)?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let exact = ((scope.agent_id.len() as u64 + 256 + 31) * 8, 64 + 3 * 8);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        charge_owned_metadata(31, 3)?;
        for incarnation in 1..=2 {
            identity.incarnation = incarnation;
            let tx = connection.transaction()?;
            insert_legacy_binding_tx(&tx, &identity, &seed, identity.updated_at)?;
            tx.rollback()?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            assert_eq!(phase.finish()?, exact);
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
fn legacy_timestamp_preserves_scope_checks_duplicate_errors_and_original_binding() -> Result<()> {
    let mut connection = connection()?;
    let original = identity();
    let mut seed = seed(&connection, &original, Some(original.incarnation), false)?;
    let tx = connection.transaction()?;
    for (status, incarnation, agent_id, seed_incarnation) in [
        (
            AgentRegistryStatus::Deleted,
            1,
            original.agent_id.as_str(),
            Some(1),
        ),
        (
            AgentRegistryStatus::Active,
            0,
            original.agent_id.as_str(),
            Some(1),
        ),
        (AgentRegistryStatus::Active, 1, "different", Some(1)),
        (
            AgentRegistryStatus::Active,
            1,
            original.agent_id.as_str(),
            Some(2),
        ),
    ] {
        let mut invalid = original.clone();
        invalid.status = status;
        invalid.incarnation = incarnation;
        invalid.agent_id = agent_id.to_owned();
        seed.incarnation = seed_incarnation;
        let error =
            insert_legacy_binding_tx(&tx, &invalid, &seed, original.updated_at).unwrap_err();
        assert!(error
            .to_string()
            .contains("legacy Episode binding scope mismatch"));
    }
    let mut overflow = original.clone();
    overflow.incarnation = u64::MAX;
    seed.incarnation = None;
    assert!(insert_legacy_binding_tx(&tx, &overflow, &seed, original.updated_at).is_err());
    assert_eq!(
        tx.query_row("SELECT COUNT(*) FROM agent_episode_continuity", [], |row| {
            row.get::<_, u64>(0)
        })?,
        0
    );
    seed.incarnation = Some(original.incarnation);
    insert_legacy_binding_tx(&tx, &original, &seed, original.updated_at)?;
    let error = insert_legacy_binding_tx(
        &tx,
        &original,
        &seed,
        original.updated_at + chrono::Duration::days(1),
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(_, _))
    ));
    assert_eq!(
        binding(&tx, original.incarnation)?,
        (
            "legacy_adopted".into(),
            seed.id,
            original.updated_at.to_rfc3339()
        )
    );
    tx.commit()?;
    Ok(())
}

#[test]
fn legacy_timestamp_real_import_and_materialization_roll_back_sql_failure_and_keep_binding(
) -> Result<()> {
    for unbound in [false, true] {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let identity = identity();
        let scope = InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope)?;
        let holder = phase.guard()?;
        db.connection()?.execute_batch(
            "CREATE TRIGGER reject_legacy_timestamp BEFORE INSERT ON agent_episode_continuity
             WHEN NEW.mode = 'legacy_adopted'
             BEGIN SELECT RAISE(ABORT, 'legacy timestamp SQL failure'); END;",
        )?;
        let used;
        {
            let _entry = phase.enter();
            let write = |tx: &Transaction<'_>| -> Result<()> {
                tx.execute(
                    "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                     VALUES ('legacy_timestamp_marker', 'rollback', 'original', 'original')",
                    [],
                )?;
                seed(
                    tx,
                    &identity,
                    (!unbound).then_some(identity.incarnation),
                    false,
                )?;
                if unbound {
                    assert!(bind_unbound_episode_continuity_tx(
                        tx,
                        &identity,
                        identity.updated_at
                    )?);
                } else {
                    prepare_imported_identity_continuity_tx(tx, &identity, identity.updated_at)?;
                }
                super::super::evidence::upsert_agent_identity_tx(tx, &identity)
            };
            let error = db.transaction(write).unwrap_err();
            assert!(format!("{error:#}").contains("legacy timestamp SQL failure"));
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
                        |row| row.get::<_, u64>(0),
                    )?,
                    0,
                    "{table}"
                );
            }
            assert_eq!(
                db.connection()?.query_row(
                    "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'legacy_timestamp_marker'",
                    [],
                    |row| row.get::<_, u64>(0),
                )?,
                0
            );
            db.connection()?
                .execute_batch("DROP TRIGGER reject_legacy_timestamp;")?;
            db.transaction(write)?;
            let connection = db.connection()?;
            assert_eq!(
                binding(&connection, identity.incarnation)?,
                (
                    "legacy_adopted".into(),
                    "legacy-timestamp-seed".into(),
                    identity.updated_at.to_rfc3339()
                )
            );
            crate::diagnostics::resource_io::check_active_budget()?;
            used = phase.finish()?.0;
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, used);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn legacy_timestamp_unbound_pending_import_and_missing_seed_remain_fail_closed() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = identity();
    db.transaction(|tx| {
        assert!(!bind_unbound_episode_continuity_tx(
            tx,
            &identity,
            identity.updated_at
        )?);
        let seed = seed(tx, &identity, None, true)?;
        let error =
            bind_unbound_episode_continuity_tx(tx, &identity, identity.updated_at).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<EpisodeSnapshotError>(),
            Some(EpisodeSnapshotError::LegacyImportPending)
        ));
        assert!(!has_any_binding(tx, &identity.agent_id)?);
        assert_eq!(find_seed(tx, &identity.agent_id)?.unwrap().id, seed.id);
        Ok(())
    })?;
    Ok(())
}
