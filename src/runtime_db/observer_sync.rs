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
pub(crate) const EVENT_PROJECTION_EFFECT_COMPLETE: &str = "event_projection_effect_complete";

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
    /// Every stored audit event classifies soundly for the additive
    /// `projection_effect` envelope field: registry-known kinds match their
    /// declared payload schema, and unknown kinds carry legacy markers.
    pub event_projection_effect_complete: bool,
}

/// Per-Agent committed event recovery window used by the rich
/// `cursor_not_found` error. All three values come from one SQL statement
/// and therefore one committed read view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEventRecoveryWindow {
    pub event_log_epoch: String,
    /// First raw event still replayable; `None` when the Agent has no events.
    pub oldest_retained_seq: Option<u64>,
    /// Greatest committed `event_seq` in the same read view.
    pub event_head_seq: u64,
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
    let inventory = verify_event_projection_effect_inventory(connection);
    let inventory_detail = match &inventory {
        Ok(complete) => serde_json::json!({
            "registry_kinds": crate::runtime_event::ALL_RUNTIME_EVENT_KINDS.len(),
            "complete": complete,
        })
        .to_string(),
        Err(error) => serde_json::json!({ "error": format!("{error:#}") }).to_string(),
    };
    persist_verification(
        connection,
        EVENT_PROJECTION_EFFECT_COMPLETE,
        inventory.unwrap_or(false),
        &now,
        &inventory_detail,
    )?;
    Ok(())
}

/// Proves every stored audit event classifies soundly under the registry
/// classification used for the additive `projection_effect` field. A
/// registry-known kind must match its declared payload schema identity and
/// carry a payload schema version this binary knows; an unknown kind must
/// be recognizably legacy. Events that are neither cannot be inventoried by
/// this binary, so `events.projection-effect.v1` stays unadvertised.
fn verify_event_projection_effect_inventory(connection: &Connection) -> Result<bool> {
    if !table_exists(connection, "audit_events")? {
        return Ok(false);
    }
    let mut statement = connection.prepare(
        "SELECT kind,
                COALESCE(json_extract(data_json, '$.payload_schema'), ''),
                COALESCE(json_extract(data_json, '$.payload_schema_version'), 0),
                COALESCE(json_extract(data_json, '$.contract_version'), 1)
         FROM audit_events
         GROUP BY kind,
                  COALESCE(json_extract(data_json, '$.payload_schema'), ''),
                  COALESCE(json_extract(data_json, '$.payload_schema_version'), 0),
                  COALESCE(json_extract(data_json, '$.contract_version'), 1)",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut unclassified = Vec::new();
    for row in rows {
        let (kind, payload_schema, payload_schema_version, contract_version) = row?;
        let sound = match crate::runtime_event::RuntimeEventKind::from_wire_name(&kind) {
            Some(event_kind) => {
                let descriptor = event_kind.descriptor();
                payload_schema_version >= 0
                    && (payload_schema_version as u64)
                        <= u64::from(descriptor.payload_schema_version)
                    && descriptor.payload_schema == payload_schema
            }
            None => crate::runtime_event::is_legacy_event_shape(
                &payload_schema,
                u32::try_from(contract_version.max(0)).unwrap_or(u32::MAX),
            ),
        };
        if !sound {
            unclassified.push(format!(
                "{kind}@{payload_schema}v{payload_schema_version}/cv{contract_version}"
            ));
        }
    }
    if unclassified.is_empty() {
        return Ok(true);
    }
    // Logged, not failed: the capability degrades, startup does not.
    tracing::warn!(
        unclassified = %unclassified.join(", "),
        "audit events outside the projection-effect inventory"
    );
    Ok(false)
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
    /// Loads the durable observer-sync verification results. Missing rows
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
            event_projection_effect_complete: verified(EVENT_PROJECTION_EFFECT_COMPLETE)?,
        })
    }

    /// Reads one Agent's committed event recovery window — epoch, oldest
    /// retained `event_seq`, and head `event_seq` — from a single SQL
    /// statement so all three values share one committed read view. Rich
    /// `cursor_not_found` errors and snapshot anchors must use this view,
    /// never a watcher or the sequence allocator's next value.
    pub fn agent_event_recovery_window(
        &self,
        agent_id: Option<&str>,
    ) -> Result<AgentEventRecoveryWindow> {
        let connection = self.connection()?;
        // Scoped storages read their own agent rows while unscoped storages
        // read the runtime-level rows (`agent_id IS NULL`), matching the
        // retention queries. A plain `agent_id = ?1` would silently match no
        // rows for a NULL scope; folding both cases into one
        // `OR (?1 IS NULL ...)` predicate would defeat the index seek that
        // backs the MIN/MAX subselects.
        let window_sql = |predicate: &str| {
            format!(
                "SELECT
                    (SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'),
                    (SELECT MIN(event_seq) FROM audit_events WHERE {predicate}),
                    (SELECT MAX(event_seq) FROM audit_events WHERE {predicate})"
            )
        };
        let window_row = |row: &rusqlite::Row<'_>| Ok((row.get(0)?, row.get(1)?, row.get(2)?));
        let (epoch, oldest, head): (String, Option<i64>, Option<i64>) = match agent_id {
            Some(agent_id) => connection.query_row(
                window_sql("agent_id = ?1").as_str(),
                [agent_id],
                window_row,
            )?,
            None => {
                connection.query_row(window_sql("agent_id IS NULL").as_str(), [], window_row)?
            }
        };
        let to_seq = |value: Option<i64>| -> Result<Option<u64>> {
            value
                .map(|seq| u64::try_from(seq).context("stored audit event sequence is negative"))
                .transpose()
        };
        Ok(AgentEventRecoveryWindow {
            event_log_epoch: epoch,
            oldest_retained_seq: to_seq(oldest)?,
            event_head_seq: to_seq(head)?.unwrap_or(0),
        })
    }
}
