//! Durable observer-sync capability verification.
//!
//! S1 of the observer-sync plan introduces the verification source the S0
//! capability evaluator requires: a capability may be advertised only while
//! its storage and consistency invariants hold for the current database.
//! Verification re-runs on every migration/open, and a failed check records
//! `verified = 0` durably instead of failing the open, so a degraded database
//! degrades advertisement rather than startup.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::runtime_db::evidence::upsert_agent_identity_tx;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
    AgentVisibility,
};

pub(crate) const RUNTIME_IDENTITY_STABLE: &str = "runtime_identity_stable";
pub(crate) const AGENT_IDENTITY_RESERVED: &str = "agent_identity_reserved";

/// Principal and entitlement used to derive the runtime-local public scope
/// for unauthenticated local mode.
pub(crate) const PUBLIC_SCOPE_PRINCIPAL: &str = "public";
pub(crate) const PUBLIC_SCOPE_ENTITLEMENT: &str = "public";

/// Verification rows for the two S1 foundations. The S2-S5 verification
/// families stay absent (and therefore false) until their slices land.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObserverSyncFoundationVerification {
    pub runtime_identity_stable: bool,
    pub agent_identity_reserved: bool,
}

/// Recomputes the S1 foundation verifications and persists the results.
///
/// Must run after `ensure_runtime_identity_metadata` so the runtime id and
/// epoch exist. Structural database errors propagate; invariant violations
/// are recorded as `verified = 0`.
pub(crate) fn verify_observer_sync_foundations(connection: &mut Connection) -> Result<()> {
    if !verification_tables_exist(connection)? {
        return Ok(());
    }

    let runtime_id = read_metadata(connection, "runtime_id")?;
    let event_log_epoch = read_metadata(connection, "event_log_epoch")?;
    let policy_generation: u64 = read_metadata(connection, "visibility_policy_generation")?
        .parse()
        .context("invalid visibility policy generation")?;

    // The derived public (unauthenticated) scope must be deterministic for
    // this installation identity; the roster snapshot serves it verbatim.
    let public_scope = crate::ids::visibility_scope_id(
        &runtime_id,
        PUBLIC_SCOPE_PRINCIPAL,
        PUBLIC_SCOPE_ENTITLEMENT,
        policy_generation,
    );
    let runtime_identity_stable = !runtime_id.is_empty()
        && !event_log_epoch.is_empty()
        && public_scope
            == crate::ids::visibility_scope_id(
                &runtime_id,
                PUBLIC_SCOPE_PRINCIPAL,
                PUBLIC_SCOPE_ENTITLEMENT,
                policy_generation,
            );

    // Name-accepted upgrade paths can leave historical source tables absent;
    // the reservation invariant cannot be verified against a missing source,
    // so the capability stays off instead of failing the open.
    let required_sources = ["agent_identities", "agent_states", "agent_deletion_jobs"];
    let missing_sources: Vec<&str> = required_sources
        .iter()
        .copied()
        .filter(|table| !table_exists(connection, table).unwrap_or(false))
        .collect();

    let (missing_reservations, active_reservations_without_registry, tombstone_state_drift) =
        if missing_sources.is_empty() {
            (
                count_rows(
                    connection,
                    "SELECT COUNT(*) FROM agent_identities i
                     LEFT JOIN agent_identity_reservations r ON r.agent_id = i.agent_id
                     WHERE r.agent_id IS NULL",
                )?,
                count_rows(
                    connection,
                    "SELECT COUNT(*) FROM agent_identity_reservations r
                     LEFT JOIN agent_identities i ON i.agent_id = r.agent_id
                     WHERE r.reservation_state = 'active' AND i.agent_id IS NULL",
                )?,
                count_rows(
                    connection,
                    "SELECT COUNT(*) FROM agent_identities i
                     JOIN agent_identity_reservations r ON r.agent_id = i.agent_id
                     WHERE (i.status = 'deleted') != (r.reservation_state = 'retired')",
                )?,
            )
        } else {
            (0, 0, 0)
        };
    let agent_state_identity_mismatch = if missing_sources.is_empty() {
        count_agent_state_identity_mismatches(connection)?
    } else {
        0
    };
    let completed_deletion_with_available_registry = if missing_sources.is_empty() {
        count_rows(
            connection,
            "SELECT COUNT(*) FROM agent_deletion_jobs d
             JOIN agent_identities i ON i.agent_id = d.agent_id
             WHERE d.status = 'completed' AND i.status != 'deleted'",
        )?
    } else {
        0
    };
    let enforcement_probe_passed = probe_reservation_enforcement(connection)?;

    let agent_identity_reserved = missing_sources.is_empty()
        && missing_reservations == 0
        && active_reservations_without_registry == 0
        && tombstone_state_drift == 0
        && agent_state_identity_mismatch == 0
        && completed_deletion_with_available_registry == 0
        && enforcement_probe_passed;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let identity_detail = serde_json::json!({
        "runtime_id": runtime_id,
        "event_log_epoch": event_log_epoch,
        "visibility_policy_generation": policy_generation,
        "public_scope_id_fingerprint": &public_scope[..public_scope.len().min(16)],
    })
    .to_string();
    let reserved_detail = serde_json::json!({
        "missing_source_tables": missing_sources,
        "missing_reservations": missing_reservations,
        "active_reservations_without_registry": active_reservations_without_registry,
        "tombstone_state_drift": tombstone_state_drift,
        "agent_state_identity_mismatch": agent_state_identity_mismatch,
        "completed_deletion_with_available_registry": completed_deletion_with_available_registry,
        "enforcement_probe": if enforcement_probe_passed { "passed" } else { "failed" },
    })
    .to_string();
    persist_verification(
        connection,
        RUNTIME_IDENTITY_STABLE,
        runtime_identity_stable,
        &now,
        &identity_detail,
    )?;
    persist_verification(
        connection,
        AGENT_IDENTITY_RESERVED,
        agent_identity_reserved,
        &now,
        &reserved_detail,
    )?;
    Ok(())
}

/// Proves the creation guard is wired into the registry write path: an
/// Active identity write for a retired reservation must be rejected, while
/// re-asserting the tombstone stays legal. Runs inside a transaction that is
/// always rolled back.
fn probe_reservation_enforcement(connection: &mut Connection) -> Result<bool> {
    let transaction = connection.transaction()?;
    let probe_id = "__observer_sync_reservation_probe__";
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    transaction.execute(
        "INSERT INTO agent_identity_reservations (agent_id, reservation_state, reserved_at, retired_at, source)
         VALUES (?1, 'retired', ?2, NULL, 'verification_probe')",
        rusqlite::params![probe_id, now],
    )?;
    let mut probe = AgentIdentityRecord::new(
        probe_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let rejected_availability_write = upsert_agent_identity_tx(&transaction, &probe).is_err();
    probe.status = AgentRegistryStatus::Deleted;
    let accepted_tombstone_write = upsert_agent_identity_tx(&transaction, &probe).is_ok();
    transaction.rollback()?;
    Ok(rejected_availability_write && accepted_tombstone_write)
}

fn count_agent_state_identity_mismatches(connection: &Connection) -> Result<u64> {
    let mut statement = connection.prepare("SELECT agent_id, payload_json FROM agent_states")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut mismatches = 0u64;
    for row in rows {
        let (row_agent_id, payload_json) = row?;
        let payload_agent_id = serde_json::from_str::<serde_json::Value>(&payload_json)
            .ok()
            .and_then(|payload| {
                payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        if payload_agent_id.as_deref() != Some(row_agent_id.as_str()) {
            mismatches += 1;
        }
    }
    Ok(mismatches)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn verification_tables_exist(connection: &Connection) -> Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
         AND name IN ('agent_identity_reservations', 'observer_sync_capability_verifications')",
        [],
        |row| row.get(0),
    )?;
    Ok(count == 2)
}

fn read_metadata(connection: &Connection, key: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("runtime metadata key {key} is missing"))
}

fn count_rows(connection: &Connection, sql: &str) -> Result<u64> {
    let count: i64 = connection.query_row(sql, [], |row| row.get(0))?;
    Ok(count.max(0) as u64)
}

fn persist_verification(
    connection: &Connection,
    capability: &str,
    verified: bool,
    verified_at: &str,
    detail: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO observer_sync_capability_verifications (capability, verified, verified_at, detail)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(capability) DO UPDATE SET
           verified = excluded.verified,
           verified_at = excluded.verified_at,
           detail = excluded.detail",
        rusqlite::params![capability, verified, verified_at, detail],
    )?;
    Ok(())
}

impl crate::runtime_db::RuntimeDb {
    /// Loads the durable S1 foundation verification results. Missing rows
    /// read as false; load errors should degrade, not fail, the caller.
    pub fn observer_sync_foundations(&self) -> Result<ObserverSyncFoundationVerification> {
        let connection = self.connection()?;
        let verified = |capability: &str| -> Result<bool> {
            Ok(connection
                .query_row(
                    "SELECT verified FROM observer_sync_capability_verifications
                     WHERE capability = ?1",
                    [capability],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .is_some_and(|value| value != 0))
        };
        Ok(ObserverSyncFoundationVerification {
            runtime_identity_stable: verified(RUNTIME_IDENTITY_STABLE)?,
            agent_identity_reserved: verified(AGENT_IDENTITY_RESERVED)?,
        })
    }
}
