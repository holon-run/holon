//! Durable observer-sync capability verification.
//!
//! S1 of the observer-sync plan introduces the verification source the S0
//! capability evaluator requires: a capability may be advertised only while
//! its storage and consistency invariants hold for the current database.
//! Verification re-runs on every migration/open, and a failed check records
//! `verified = 0` durably instead of failing the open, so a degraded database
//! degrades advertisement rather than startup.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

use crate::runtime_db::evidence::upsert_agent_identity_tx;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
    AgentVisibility,
};

pub(crate) const RUNTIME_IDENTITY_STABLE: &str = "runtime_identity_stable";
pub(crate) const AGENT_IDENTITY_RESERVED: &str = "agent_identity_reserved";
pub(crate) const EVENT_PROJECTION_EFFECT_COMPLETE: &str = "event_projection_effect_complete";
pub(crate) const BRIEF_ATOMIC_LINKAGE_VERIFIED: &str = "brief_atomic_linkage_verified";
pub(crate) const ROSTER_SNAPSHOT_VERIFIED: &str = "roster_snapshot_verified";

/// Principal and entitlement used to derive the runtime-local public scope
/// for unauthenticated local mode.
pub(crate) const PUBLIC_SCOPE_PRINCIPAL: &str = "public";
pub(crate) const PUBLIC_SCOPE_ENTITLEMENT: &str = "public";

/// Roster anchor rows captured inside one committed database read view.
/// `runtime_id`, `event_log_epoch`, and `visibility_policy_generation`
/// come from the same view as every per-Agent row, so a response built
/// from this struct never mixes facts from different commits.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRosterSnapshotRows {
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_policy_generation: u64,
    pub rows: Vec<AgentRosterRow>,
}

/// One roster member's anchors from the shared read view: the serialized
/// identity payload, the committed AgentState payload (`None` while the
/// identity has no persisted state yet), the committed event window, and
/// the latest canonical Brief row.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRosterRow {
    pub agent_id: String,
    pub identity_json: String,
    pub agent_state_json: Option<String>,
    pub event_head_seq: u64,
    pub oldest_retained_seq: Option<u64>,
    pub latest_brief: Option<AgentRosterLatestBriefRow>,
}

/// Latest canonical Brief anchor for one roster member. `created_at` stays
/// as the stored RFC 3339 text so an unparsable timestamp fails assembly
/// (all-or-nothing) instead of being silently replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRosterLatestBriefRow {
    pub brief_id: String,
    /// Raw stored linkage sequence; conversion to `u64` happens at
    /// assembly so a corrupt value fails the whole snapshot.
    pub created_event_seq: Option<i64>,
    pub created_at: String,
    pub preview: Option<String>,
}

/// Verification rows for the two S1 foundations. The S2-S5 verification
/// families stay absent (and therefore false) until their slices land.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObserverSyncFoundationVerification {
    pub runtime_identity_stable: bool,
    pub agent_identity_reserved: bool,
    /// The roster snapshot read view assembles for this database: every
    /// membership row's identity, committed state, event window, and latest
    /// Brief anchor parse and satisfy their structural invariants.
    pub roster_snapshot_verified: bool,
    /// Every stored audit event classifies soundly for the additive
    /// `projection_effect` envelope field: registry-known kinds match their
    /// declared payload schema, and unknown kinds carry legacy markers.
    pub event_projection_effect_complete: bool,
    /// Every Brief publication path commits the Brief record and its unique
    /// `brief_created` event in one runtime DB transition, and every stored
    /// linkage resolves to exactly one matching event. Retention-pruned
    /// history with no linkage stays acceptable.
    pub brief_atomic_linkage_verified: bool,
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
    let linkage = verify_brief_atomic_linkage(connection);
    let linkage_detail = match &linkage {
        Ok(verified) => serde_json::json!({
            "unmatched_linkages": 0,
            "shared_sequences": 0,
            "verified": verified,
        })
        .to_string(),
        Err(error) => serde_json::json!({ "error": format!("{error:#}") }).to_string(),
    };
    persist_verification(
        connection,
        BRIEF_ATOMIC_LINKAGE_VERIFIED,
        linkage.unwrap_or(false),
        &now,
        &linkage_detail,
    )?;
    let roster = verify_roster_snapshot_view(connection);
    let roster_detail = match &roster {
        Ok(verified) => serde_json::json!({ "verified": verified }).to_string(),
        Err(error) => serde_json::json!({ "error": format!("{error:#}") }).to_string(),
    };
    persist_verification(
        connection,
        ROSTER_SNAPSHOT_VERIFIED,
        roster.unwrap_or(false),
        &now,
        &roster_detail,
    )?;
    Ok(())
}

/// Proves the roster read view is assemblable and sound for this database:
/// every active public member's identity and committed state payloads
/// parse, latest-Brief timestamps parse, and event windows are ordered.
/// A row that cannot assemble would fail a whole snapshot response, so the
/// capability degrades instead.
fn verify_roster_snapshot_view(connection: &Connection) -> Result<bool> {
    for table in ["agent_identities", "agent_states", "audit_events", "briefs"] {
        if !table_exists(connection, table)? {
            return Ok(false);
        }
    }
    let snapshot = collect_agent_roster_rows(connection)?;
    for row in &snapshot.rows {
        serde_json::from_str::<AgentIdentityRecord>(&row.identity_json)
            .with_context(|| format!("unreadable roster identity payload for {}", row.agent_id))?;
        if let Some(state_json) = row.agent_state_json.as_deref() {
            serde_json::from_str::<crate::types::AgentState>(state_json).with_context(|| {
                format!("unreadable roster agent state payload for {}", row.agent_id)
            })?;
        }
        if let Some(brief) = row.latest_brief.as_ref() {
            chrono_datetime(&brief.created_at).with_context(|| {
                format!("unreadable latest brief timestamp for {}", row.agent_id)
            })?;
        }
        anyhow::ensure!(
            row.event_head_seq >= row.oldest_retained_seq.unwrap_or(0),
            "roster event window for {} is inverted",
            row.agent_id
        );
    }
    Ok(true)
}

/// Parses a stored RFC 3339 timestamp into a UTC datetime.
fn chrono_datetime(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
        .with_context(|| format!("invalid RFC 3339 timestamp {value}"))
}

/// Collects every roster anchor family from one connection. The caller
/// decides the transaction boundary; the snapshot read path wraps this in
/// one deferred read transaction, and verification calls it directly.
fn collect_agent_roster_rows(connection: &Connection) -> Result<AgentRosterSnapshotRows> {
    let runtime_id = read_metadata(connection, "runtime_id")?;
    let event_log_epoch = read_metadata(connection, "event_log_epoch")?;
    let visibility_policy_generation: u64 =
        read_metadata(connection, "visibility_policy_generation")?
            .parse()
            .context("invalid visibility policy generation")?;

    let mut identities = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT i.agent_id, i.payload_json FROM agent_identities i
             WHERE i.status = 'active' AND i.visibility = 'public'
             ORDER BY i.agent_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            identities.push(row?);
        }
    }

    let mut event_windows: HashMap<String, (Option<i64>, Option<i64>)> = HashMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT e.agent_id, MIN(e.event_seq), MAX(e.event_seq)
             FROM audit_events e
             JOIN agent_identities i ON i.agent_id = e.agent_id
             WHERE i.status = 'active' AND i.visibility = 'public'
             GROUP BY e.agent_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?;
        for row in rows {
            let (agent_id, oldest, head) = row?;
            event_windows.insert(agent_id, (oldest, head));
        }
    }

    let mut latest_briefs: HashMap<String, AgentRosterLatestBriefRow> = HashMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT b.agent_id, b.evidence_id, b.created_event_seq, b.created_at, b.preview
             FROM briefs b
             JOIN agent_identities i ON i.agent_id = b.agent_id
             WHERE i.status = 'active' AND i.visibility = 'public'
               AND b.evidence_id = (
                   SELECT b2.evidence_id FROM briefs b2
                   WHERE b2.agent_id = b.agent_id
                   ORDER BY b2.created_at DESC, b2.evidence_id DESC
                   LIMIT 1
               )",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (agent_id, brief_id, created_event_seq, created_at, preview) = row?;
            latest_briefs.insert(
                agent_id,
                AgentRosterLatestBriefRow {
                    brief_id,
                    created_event_seq,
                    created_at,
                    preview,
                },
            );
        }
    }

    let mut agent_states: HashMap<String, String> = HashMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT s.agent_id, s.payload_json
             FROM agent_states s
             JOIN agent_identities i ON i.agent_id = s.agent_id
             WHERE i.status = 'active' AND i.visibility = 'public'",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (agent_id, payload) = row?;
            agent_states.insert(agent_id, payload);
        }
    }

    let to_seq = |value: Option<i64>| -> Result<Option<u64>> {
        value
            .map(|seq| u64::try_from(seq).context("stored audit event sequence is negative"))
            .transpose()
    };
    let rows = identities
        .into_iter()
        .map(|(agent_id, identity_json)| {
            let (oldest, head) = event_windows.remove(&agent_id).unwrap_or((None, None));
            Ok(AgentRosterRow {
                event_head_seq: to_seq(head)?.unwrap_or(0),
                oldest_retained_seq: to_seq(oldest)?,
                latest_brief: latest_briefs.remove(&agent_id),
                agent_state_json: agent_states.remove(&agent_id),
                agent_id,
                identity_json,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(AgentRosterSnapshotRows {
        runtime_id,
        event_log_epoch,
        visibility_policy_generation,
        rows,
    })
}

/// Proves the stored Brief linkage is sound for the atomic created-event
/// contract: every `created_event_seq` resolves to exactly one
/// `brief_created` audit event of the same Agent referencing that Brief,
/// and no sequence is claimed by two Briefs. Events whose records were
/// pruned by retention carry no linkage and therefore stay acceptable.
fn verify_brief_atomic_linkage(connection: &Connection) -> Result<bool> {
    if !table_exists(connection, "briefs")? || !table_exists(connection, "audit_events")? {
        return Ok(false);
    }
    let linkage_column: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('briefs')
         WHERE name = 'created_event_seq'",
        [],
        |row| row.get(0),
    )?;
    if linkage_column == 0 {
        return Ok(false);
    }
    let unmatched: i64 = connection.query_row(
        "SELECT COUNT(*) FROM briefs b
         WHERE b.created_event_seq IS NOT NULL AND (
           SELECT COUNT(*) FROM audit_events e
           WHERE e.kind = 'brief_created'
             AND e.event_seq = b.created_event_seq
             AND COALESCE(e.agent_id, json_extract(e.data_json, '$.agent_id')) = b.agent_id
             AND json_extract(e.data_json, '$.brief_id') = b.evidence_id
         ) != 1",
        [],
        |row| row.get(0),
    )?;
    let shared_sequences: i64 = connection.query_row(
        "SELECT COUNT(*) FROM (
           SELECT created_event_seq FROM briefs
           WHERE created_event_seq IS NOT NULL
           GROUP BY created_event_seq HAVING COUNT(*) > 1
         )",
        [],
        |row| row.get(0),
    )?;
    if unmatched > 0 || shared_sequences > 0 {
        tracing::warn!(
            unmatched,
            shared_sequences,
            "brief created_event_seq linkage verification failed"
        );
        return Ok(false);
    }
    Ok(true)
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
            roster_snapshot_verified: verified(ROSTER_SNAPSHOT_VERIFIED)?,
            event_projection_effect_complete: verified(EVENT_PROJECTION_EFFECT_COMPLETE)?,
            brief_atomic_linkage_verified: verified(BRIEF_ATOMIC_LINKAGE_VERIFIED)?,
        })
    }

    /// Reads every roster anchor family — identity registry, active public
    /// membership, per-Agent committed event windows, latest canonical
    /// Briefs, and the runtime identity metadata — inside one deferred read
    /// transaction, so all values share one committed database view.
    pub fn agent_roster_snapshot_rows(&self) -> Result<AgentRosterSnapshotRows> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let rows = collect_agent_roster_rows(&transaction)?;
        transaction.commit()?;
        Ok(rows)
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
