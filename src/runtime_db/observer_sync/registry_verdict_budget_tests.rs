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
        agent_id: "registry-verdict".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn text_cost(text: &str) -> (u64, usize) {
    (((text.len() + std::mem::size_of::<String>()) as u64) * 8, 8)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn assert_budget_error(error: &anyhow::Error) {
    let error = error
        .downcast_ref::<crate::runtime_error::RuntimeError>()
        .unwrap_or_else(|| panic!("expected structured budget error: {error:#}"));
    let descriptor = error.descriptor();
    assert_eq!(
        descriptor.domain,
        crate::runtime_error::RuntimeErrorDomain::Io
    );
    assert_eq!(descriptor.code, "context_resource_exhausted");
    assert!(descriptor.retryable);
}

fn identity() -> AgentIdentityRecord {
    let mut record = AgentIdentityRecord::new(
        PROBE_ID,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    record.status = AgentRegistryStatus::Deleted;
    record.created_at = "2026-10-17T22:00:00Z".parse().unwrap();
    record.updated_at = record.created_at;
    record
}

fn decode_cost(payload: &str) -> (u64, usize) {
    let value: serde_json::Value = serde_json::from_str(payload).unwrap();
    let fields = value.as_object().unwrap();
    assert!(fields.values().all(|v| !v.is_object() && !v.is_array()));
    // Flat identity: braces, key opening quotes/colons, commas, string openings.
    let slots = 2
        + fields.len() * 2
        + fields.len().saturating_sub(1)
        + fields.values().filter(|v| v.is_string()).count();
    (
        (payload.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots * 4 + 16) * 8,
    )
}

/// Fix only the post-write fixture body, not the production probe clock.
/// Probe verdicts test reservation enforcement; equality/continuity is covered
/// by the repository tests with an unmodified incoming/stored identity.
pub(super) fn fixed_probe_decode_cost(connection: &Connection) -> Result<(u64, usize)> {
    let payload = serde_json::to_string(&identity())?;
    connection.execute_batch(&format!(
        "CREATE TRIGGER fixed_probe_payload AFTER INSERT ON agent_identities
         WHEN NEW.agent_id = '{PROBE_ID}'
         BEGIN UPDATE agent_identities SET payload_json = '{}'
         WHERE agent_id = NEW.agent_id; END;",
        payload.replace('\'', "''"),
    ))?;
    Ok(decode_cost(&payload))
}

fn fixture() -> Result<(tempfile::TempDir, crate::runtime_db::RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = crate::runtime_db::RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}

fn snapshot(connection: &Connection) -> Result<Vec<Vec<Vec<rusqlite::types::Value>>>> {
    [
        "agent_identities",
        "agent_identity_reservations",
        "agent_episode_continuity",
        "observer_sync_capability_verifications",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = connection.prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))?;
        let rows = statement
            .query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|index| row.get(index))
                    .collect()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .collect()
}

fn probe_prefix() -> (u64, usize) {
    add(text_cost("2026-10-17T22:00:00.123Z"), text_cost(PROBE_ID))
}

pub(super) fn probe_registry_timestamp_cost() -> (u64, usize) {
    let one = text_cost("2026-10-17T22:00:00.123Z");
    // Both reservation checks see the probe's retired row and format no clock.
    // Only the tombstone's two registry column timestamps allocate.
    add(one, one)
}

#[test]
fn probe_post_write_denial_propagates_before_tombstone_verdict_and_rolls_back() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let decode = fixed_probe_decode_cost(&connection)?;
    let before_sql = snapshot(&connection)?;
    let before = add(
        add(add(header(), (31 * 8, 3 * 8)), probe_prefix()),
        probe_registry_timestamp_cost(),
    );
    let exact = add(before, decode);
    for denied in [Some(0), Some(1), None] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            exact.0 - u64::from(denied == Some(0)),
            exact.1 - usize::from(denied == Some(1)),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            charge_owned_metadata(31, 3)?;
            let result = probe_reservation_enforcement(&mut connection);
            if denied.is_some() {
                let error = result.unwrap_err();
                assert_budget_error(&error);
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(phase.finish().is_err());
            } else {
                assert!(result?);
                assert_eq!(phase.finish()?, exact);
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder,
                ));
            }
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(connection.is_autocommit());
        assert_eq!(snapshot(&connection)?, before_sql);
        let retained = if denied.is_some() { before } else { exact };
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, retained.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    connection.execute_batch("DROP TRIGGER fixed_probe_payload")?;
    assert!(probe_reservation_enforcement(&mut connection)?);
    assert_eq!(snapshot(&connection)?, before_sql);
    Ok(())
}

#[test]
fn probe_pre_read_denial_propagates_before_availability_verdict() -> Result<()> {
    let (_home, db) = fixture()?;
    let record = identity();
    db.agent_identities().upsert(&record)?;
    let mut connection = db.connection()?;
    // Keep a real prior identity but allow the probe's retired reservation INSERT.
    connection.execute(
        "DELETE FROM agent_identity_reservations WHERE agent_id = ?1",
        [PROBE_ID],
    )?;
    let before_sql = snapshot(&connection)?;
    let before = add(add(header(), (31 * 8, 3 * 8)), probe_prefix());
    let exact = add(before, decode_cost(&serde_json::to_string(&record)?));
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
            assert_budget_error(&error);
            assert!(ResourceReadMaterial::active_guard().is_err());
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(phase.finish().is_err());
        assert!(connection.is_autocommit());
        assert_eq!(snapshot(&connection)?, before_sql);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, before.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn verifier_propagates_registry_denial_without_capability_writes() -> Result<()> {
    let (_home, db) = fixture()?;
    let mut connection = db.connection()?;
    let decode = fixed_probe_decode_cost(&connection)?;
    connection.execute(
        "UPDATE observer_sync_capability_verifications SET verified_at = 'sentinel'",
        [],
    )?;
    let before_sql = snapshot(&connection)?;
    let mut before = add(
        add(add(header(), (31 * 8, 3 * 8)), probe_prefix()),
        probe_registry_timestamp_cost(),
    );
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
    let exact = add(before, decode);
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
            let error = verify_observer_sync_foundations(&mut connection).unwrap_err();
            assert_budget_error(&error);
            assert!(ResourceReadMaterial::active_guard().is_err());
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert!(phase.finish().is_err());
        assert!(connection.is_autocommit());
        assert_eq!(snapshot(&connection)?, before_sql);
        let last = holder.clone();
        drop((phase, holder));
        assert_eq!(ledger.report()?.observed_bytes, before.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    connection.execute_batch("DROP TRIGGER fixed_probe_payload")?;
    verify_observer_sync_foundations(&mut connection)?;
    assert!(connection.is_autocommit());
    assert!(probe_reservation_enforcement(&mut connection)?);
    Ok(())
}
