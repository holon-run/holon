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
        "fresh-continuity-timestamp",
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
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    Ok(connection)
}

fn bound_at(connection: &Connection, incarnation: u64) -> Result<String> {
    Ok(connection.query_row(
        "SELECT bound_at FROM agent_episode_continuity WHERE identity_incarnation = ?1",
        [incarnation],
        |row| row.get(0),
    )?)
}

#[test]
fn fresh_timestamp_sql_bytes_match_original_for_wide_years_fractions_and_leap_seconds() -> Result<()>
{
    let mut connection = connection()?;
    let mut identity = identity();
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
            123_000_000,
            123_456_000,
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
        let tx = connection.transaction()?;
        ensure_fresh_episode_continuity_tx(&tx, &identity, value)?;
        tx.commit()?;
        assert_eq!(
            bound_at(&connection, identity.incarnation)?,
            value.to_rfc3339()
        );
    }
    let mut buffer = ContinuityTimestamp {
        bytes: [0; 40],
        len: 0,
    };
    use std::fmt::Write;
    assert!(buffer.write_str(&"x".repeat(41)).is_err());
    assert_eq!(buffer.len, 0);
    buffer.write_str("valid")?;
    assert_eq!(&buffer.bytes[..buffer.len], b"valid");
    Ok(())
}

#[test]
fn fresh_timestamp_needs_no_remaining_budget_and_preserves_original_phase_and_last_holder(
) -> Result<()> {
    let mut connection = connection()?;
    let identity = identity();
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
        for _ in 0..2 {
            let tx = connection.transaction()?;
            ensure_fresh_episode_continuity_tx(&tx, &identity, identity.updated_at)?;
            tx.commit()?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            assert_eq!(phase.finish()?, exact);
        }
        assert_eq!(
            bound_at(&connection, identity.incarnation)?,
            identity.updated_at.to_rfc3339()
        );
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
fn fresh_timestamp_preserves_missing_schema_invalid_incarnation_and_existing_bindings() -> Result<()>
{
    let mut connection = Connection::open_in_memory()?;
    let mut identity = identity();
    {
        let tx = connection.transaction()?;
        ensure_fresh_episode_continuity_tx(&tx, &identity, identity.updated_at)?;
        identity.incarnation = 0;
        assert!(ensure_fresh_episode_continuity_tx(&tx, &identity, identity.updated_at).is_err());
        tx.commit()?;
    }
    identity.incarnation = 1;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    let tx = connection.transaction()?;
    ensure_fresh_episode_continuity_tx(&tx, &identity, identity.updated_at)?;
    ensure_fresh_episode_continuity_tx(
        &tx,
        &identity,
        identity.updated_at + chrono::Duration::days(1),
    )?;
    assert_eq!(bound_at(&tx, 1)?, identity.updated_at.to_rfc3339());
    tx.execute(
        "INSERT INTO episode_continuity_seeds (
            seed_id, agent_id, policy_version, boundary_id, identity_incarnation, source_kind,
            roots_json, candidate_payloads_json, import_required, captured_at
         ) VALUES ('original-seed', ?1, 1, 'boundary', 2, 'legacy_identity_import', '{}', '[]', 0, 'original')",
        [&identity.agent_id],
    )?;
    tx.execute(
        "INSERT INTO agent_episode_continuity VALUES (?1, 2, 1, 'legacy_adopted', 'original-seed', 'original')",
        [&identity.agent_id],
    )?;
    identity.incarnation = 2;
    ensure_fresh_episode_continuity_tx(&tx, &identity, identity.updated_at)?;
    let existing: (String, String, String) = tx.query_row(
        "SELECT mode, seed_id, bound_at FROM agent_episode_continuity WHERE identity_incarnation = 2",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        existing,
        (
            "legacy_adopted".into(),
            "original-seed".into(),
            "original".into()
        )
    );
    tx.commit()?;
    Ok(())
}

#[test]
fn fresh_timestamp_sql_failure_rolls_back_registry_group_and_releases_original_holder() -> Result<()>
{
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
        "CREATE TRIGGER reject_fresh_timestamp BEFORE INSERT ON agent_episode_continuity
         BEGIN SELECT RAISE(ABORT, 'fresh timestamp SQL failure'); END;",
    )?;
    let used;
    {
        let _entry = phase.enter();
        let error = db
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                 VALUES ('fresh_timestamp_marker', 'rollback', 'original', 'original')",
                    [],
                )?;
                super::super::evidence::upsert_agent_identity_tx(tx, &identity)
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("fresh timestamp SQL failure"));
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        crate::diagnostics::resource_io::check_active_budget()?;
        used = phase.finish()?.0;
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let connection = db.connection()?;
    for table in [
        "agent_identities",
        "agent_identity_reservations",
        "agent_episode_continuity",
    ] {
        let count: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE agent_id = ?1"),
            [&identity.agent_id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0, "{table}");
    }
    let marker: u64 = connection.query_row(
        "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'fresh_timestamp_marker'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(marker, 0);
    let last = holder.clone();
    drop((phase, holder));
    assert_eq!(ledger.report()?.observed_bytes, used);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
