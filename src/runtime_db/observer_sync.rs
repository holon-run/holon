//! Durable observer-sync capability verification.
//!
//! S1 of the observer-sync plan introduces the verification source the S0
//! capability evaluator requires: a capability may be advertised only while
//! its storage and consistency invariants hold for the current database.
//! Verification runs after migrations. Projection-effect inventory results
//! carry a durable verifier/epoch/mutation proof so an unchanged database can
//! reuse them instead of scanning the complete event log on every open.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

use crate::runtime_db::evidence::upsert_agent_identity_tx;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
    AgentVisibility, AuditEvent,
};

pub(crate) const RUNTIME_IDENTITY_STABLE: &str = "runtime_identity_stable";
pub(crate) const AGENT_IDENTITY_RESERVED: &str = "agent_identity_reserved";
pub(crate) const EVENT_PROJECTION_EFFECT_COMPLETE: &str = "event_projection_effect_complete";
pub(crate) const BRIEF_ATOMIC_LINKAGE_VERIFIED: &str = "brief_atomic_linkage_verified";
pub(crate) const ROSTER_SNAPSHOT_VERIFIED: &str = "roster_snapshot_verified";
pub(crate) const PROJECTION_SNAPSHOT_VERIFIED: &str = "projection_snapshot_verified";
pub(crate) const EVENT_PROJECTION_EFFECT_VERIFIER_VERSION: i64 = 1;
pub(crate) const AGENT_ROSTER_LATEST_BRIEFS_SQL: &str =
    "SELECT b.agent_id, b.evidence_id, b.created_event_seq, b.created_at, b.preview
     FROM agent_identities i
     JOIN briefs b ON b.evidence_id = (
         SELECT b2.evidence_id FROM briefs b2
         WHERE b2.agent_id = i.agent_id
         ORDER BY b2.created_at DESC, b2.evidence_id DESC
         LIMIT 1
     )
     WHERE i.status = 'active' AND i.visibility = 'public'";

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
    pub oldest_retained_seq: u64,
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

/// Per-Agent canonical projection anchors captured inside one committed
/// database read view. Every field comes from the same view, so a response
/// built from this struct never mixes facts from different commits.
/// Runtime identity metadata is read by the caller inside the same
/// transaction and returned alongside.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentProjectionSnapshotRows {
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_policy_generation: u64,
    /// `None` when the Agent is not an active public member: unknown,
    /// private, or deleted identities are indistinguishable by design.
    pub row: Option<AgentProjectionSnapshotRow>,
}

/// Per-Agent projection anchor row. `event_head_seq` doubles as the
/// snapshot boundary because every display-affecting event family commits
/// its canonical record no later than its event, so a committed view that
/// contains the event also contains the record it describes.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentProjectionSnapshotRow {
    pub agent_id: String,
    pub identity_json: String,
    /// Committed AgentState payload; `None` while the identity has no
    /// persisted state yet (stopped placeholder semantics).
    pub agent_state_json: Option<String>,
    /// Greatest committed `event_seq` for the Agent in this view.
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    /// Current focused WorkItem, or the most recently updated open one.
    pub current_work_item: Option<AgentProjectionWorkItemRow>,
    /// Latest message record id resolvable through the batch API.
    pub latest_message_id: Option<String>,
    /// Latest transcript entry id resolvable through the batch API.
    pub latest_transcript_entry_id: Option<String>,
    pub latest_brief: Option<AgentRosterLatestBriefRow>,
}

/// Current WorkItem anchor row kept in wire-neutral form: `state`,
/// `plan_status`, `revision`, and `updated_at` stay as stored text so an
/// unparsable value fails assembly (all-or-nothing) instead of being
/// silently replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentProjectionWorkItemRow {
    pub work_item_id: String,
    pub state: String,
    pub plan_status: Option<String>,
    /// Raw stored revision; conversion to `u64` happens at assembly so a
    /// corrupt value fails the whole snapshot.
    pub revision: i64,
    pub updated_at: String,
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
    /// The per-Agent projection snapshot read boundary assembles for this
    /// database: membership, committed state, event window, current
    /// WorkItem anchor, conversation anchors, and latest Brief all parse
    /// and satisfy their structural invariants inside one read view.
    pub projection_snapshot_verified: bool,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventProjectionEffectProof {
    verified: bool,
    verification_version: i64,
    event_log_epoch: Option<String>,
    event_generation: i64,
    verified_event_generation: i64,
}

/// Per-Agent committed event recovery window used by the rich
/// `cursor_not_found` error. All three values come from one SQL statement
/// and therefore one committed read view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEventRecoveryWindow {
    pub event_log_epoch: String,
    /// Retention floor; `0` means no retained prefix has ever been deleted.
    pub oldest_retained_seq: u64,
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
    {
        let transaction = connection.transaction()?;
        if reusable_event_projection_effect_verification(&transaction, &event_log_epoch)?.is_none()
        {
            let inventory = verify_event_projection_effect_inventory(&transaction);
            let inventory_detail = match &inventory {
                Ok(complete) => serde_json::json!({
                    "source": "full_inventory",
                    "registry_kinds": crate::runtime_event::ALL_RUNTIME_EVENT_KINDS.len(),
                    "complete": complete,
                })
                .to_string(),
                Err(error) => serde_json::json!({
                    "source": "full_inventory",
                    "error": format!("{error:#}"),
                })
                .to_string(),
            };
            persist_event_projection_effect_verification(
                &transaction,
                inventory.unwrap_or(false),
                &event_log_epoch,
                &now,
                &inventory_detail,
            )?;
        }
        transaction.commit()?;
    }
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
    let projection = verify_projection_snapshot_view(connection);
    let projection_detail = match &projection {
        Ok(verified) => serde_json::json!({
            "verified": verified,
            "boundary_rule": "event_head_seq",
        })
        .to_string(),
        Err(error) => serde_json::json!({ "error": format!("{error:#}") }).to_string(),
    };
    persist_verification(
        connection,
        PROJECTION_SNAPSHOT_VERIFIED,
        projection.unwrap_or(false),
        &now,
        &projection_detail,
    )?;
    Ok(())
}

/// Proves the roster read view is assemblable and sound for this database:
/// every active public member's identity and committed state payloads
/// parse, latest-Brief timestamps parse, and event windows are ordered.
/// A row that cannot assemble would fail a whole snapshot response, so the
/// capability degrades instead.
fn verify_roster_snapshot_view(connection: &Connection) -> Result<bool> {
    for table in [
        "agent_identities",
        "agent_states",
        "audit_events",
        "audit_event_retention_watermarks",
        "runtime_sequences",
        "briefs",
    ] {
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
            row.event_head_seq.saturating_add(1) >= row.oldest_retained_seq,
            "roster event window for {} is inverted",
            row.agent_id
        );
    }
    Ok(true)
}

/// Proves the per-Agent projection snapshot read boundary is assemblable
/// and sound for this database: every active public member's projection
/// anchors parse (identity, committed state, current WorkItem, latest
/// Brief) and event windows are ordered. A row that cannot assemble would
/// fail a whole snapshot response, so the capability degrades instead.
fn verify_projection_snapshot_view(connection: &Connection) -> Result<bool> {
    for table in [
        "agent_identities",
        "agent_states",
        "audit_events",
        "audit_event_retention_watermarks",
        "runtime_sequences",
        "briefs",
        "work_items",
        "messages",
        "transcript_entries",
    ] {
        if !table_exists(connection, table)? {
            return Ok(false);
        }
    }
    let snapshot = collect_agent_roster_rows(connection)?;
    for roster_row in &snapshot.rows {
        let rows = collect_agent_projection_anchors(connection, &roster_row.agent_id)?;
        let Some(row) = rows.row else {
            // Membership cannot change between the two collects on one
            // connection; treat absence as a failed verification.
            anyhow::bail!(
                "projection snapshot member {} disappeared inside one read view",
                roster_row.agent_id
            );
        };
        serde_json::from_str::<AgentIdentityRecord>(&row.identity_json).with_context(|| {
            format!(
                "unreadable projection identity payload for {}",
                row.agent_id
            )
        })?;
        if let Some(state_json) = row.agent_state_json.as_deref() {
            serde_json::from_str::<crate::types::AgentState>(state_json).with_context(|| {
                format!(
                    "unreadable projection agent state payload for {}",
                    row.agent_id
                )
            })?;
        }
        if let Some(work_item) = row.current_work_item.as_ref() {
            parse_work_item_state(&work_item.state)
                .with_context(|| format!("unreadable work item state for {}", row.agent_id))?;
            if let Some(plan_status) = work_item.plan_status.as_deref() {
                parse_work_item_plan_status(plan_status).with_context(|| {
                    format!("unreadable work item plan status for {}", row.agent_id)
                })?;
            }
            anyhow::ensure!(
                work_item.revision >= 0,
                "negative work item revision for {}",
                row.agent_id
            );
            chrono_datetime(&work_item.updated_at)
                .with_context(|| format!("unreadable work item timestamp for {}", row.agent_id))?;
        }
        if let Some(brief) = row.latest_brief.as_ref() {
            chrono_datetime(&brief.created_at).with_context(|| {
                format!("unreadable latest brief timestamp for {}", row.agent_id)
            })?;
        }
        anyhow::ensure!(
            row.event_head_seq.saturating_add(1) >= row.oldest_retained_seq,
            "projection event window for {} is inverted",
            row.agent_id
        );
    }
    Ok(true)
}

pub(crate) fn parse_work_item_state(value: &str) -> Result<crate::types::WorkItemState> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|error| anyhow::anyhow!("invalid work item state {value}: {error}"))
}

pub(crate) fn parse_work_item_plan_status(value: &str) -> Result<crate::types::WorkItemPlanStatus> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|error| anyhow::anyhow!("invalid work item plan status {value}: {error}"))
}

/// Collects one Agent's canonical projection anchors from one connection.
/// The caller decides the transaction boundary; the snapshot read path
/// wraps this plus the runtime identity metadata in one deferred read
/// transaction so the response forms a single committed view.
///
/// Returns `row: None` when the Agent is not an active public member, so
/// unknown, private-child, and deleted identities stay indistinguishable
/// to callers and no membership metadata leaks through the endpoint.
fn collect_agent_projection_anchors(
    connection: &Connection,
    agent_id: &str,
) -> Result<AgentProjectionSnapshotRows> {
    let runtime_id = read_metadata(connection, "runtime_id")?;
    let event_log_epoch = read_metadata(connection, "event_log_epoch")?;
    let visibility_policy_generation: u64 =
        read_metadata(connection, "visibility_policy_generation")?
            .parse()
            .context("invalid visibility policy generation")?;

    let membership = connection
        .query_row(
            "SELECT i.payload_json,
                    (SELECT s.payload_json FROM agent_states s WHERE s.agent_id = i.agent_id)
             FROM agent_identities i
             WHERE i.agent_id = ?1 AND i.status = 'active' AND i.visibility = 'public'",
            [agent_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let Some((identity_json, agent_state_json)) = membership else {
        return Ok(AgentProjectionSnapshotRows {
            runtime_id,
            event_log_epoch,
            visibility_policy_generation,
            row: None,
        });
    };

    let (oldest, head): (i64, i64) = connection.query_row(
        "SELECT
           COALESCE((
             SELECT oldest_retained_seq FROM audit_event_retention_watermarks
             WHERE scope_key = 'agent:' || ?1
           ), 0),
           COALESCE((
             SELECT last_value FROM runtime_sequences
             WHERE domain = 'audit_event' AND scope_key = 'agent:' || ?1
           ), 0)",
        [agent_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let to_seq = |value: i64| -> Result<u64> {
        u64::try_from(value).context("stored audit event sequence is negative")
    };

    // Current WorkItem anchor: the focused row wins; without focus the
    // most recently updated open WorkItem keeps the card meaningful. Both
    // orders are deterministic on ties via work_item_id.
    let current_work_item = connection
        .query_row(
            "SELECT work_item_id, state, plan_status, revision, updated_at FROM work_items
             WHERE agent_id = ?1 AND current_focus = 1
             ORDER BY updated_at DESC, work_item_id DESC LIMIT 1",
            [agent_id],
            work_item_anchor_row,
        )
        .optional()?;
    let current_work_item = match current_work_item {
        Some(row) => Some(row),
        None => connection
            .query_row(
                "SELECT work_item_id, state, plan_status, revision, updated_at FROM work_items
                 WHERE agent_id = ?1 AND state = 'open'
                 ORDER BY updated_at DESC, work_item_id DESC LIMIT 1",
                [agent_id],
                work_item_anchor_row,
            )
            .optional()?,
    };

    let latest_message_id: Option<String> = connection
        .query_row(
            "SELECT evidence_id FROM messages WHERE agent_id = ?1
             ORDER BY created_at DESC, evidence_id DESC LIMIT 1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let latest_transcript_entry_id: Option<String> = connection
        .query_row(
            "SELECT evidence_id FROM transcript_entries WHERE agent_id = ?1
             ORDER BY created_at DESC, evidence_id DESC LIMIT 1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let latest_brief = connection
        .query_row(
            "SELECT evidence_id, created_event_seq, created_at, preview FROM briefs
             WHERE agent_id = ?1
             ORDER BY created_at DESC, evidence_id DESC LIMIT 1",
            [agent_id],
            |row| {
                Ok(AgentRosterLatestBriefRow {
                    brief_id: row.get(0)?,
                    created_event_seq: row.get(1)?,
                    created_at: row.get(2)?,
                    preview: row.get(3)?,
                })
            },
        )
        .optional()?;

    Ok(AgentProjectionSnapshotRows {
        runtime_id,
        event_log_epoch,
        visibility_policy_generation,
        row: Some(AgentProjectionSnapshotRow {
            agent_id: agent_id.to_string(),
            identity_json,
            agent_state_json,
            event_head_seq: to_seq(head)?,
            oldest_retained_seq: to_seq(oldest)?,
            current_work_item,
            latest_message_id,
            latest_transcript_entry_id,
            latest_brief,
        }),
    })
}

fn work_item_anchor_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentProjectionWorkItemRow> {
    Ok(AgentProjectionWorkItemRow {
        work_item_id: row.get(0)?,
        state: row.get(1)?,
        plan_status: row.get(2)?,
        revision: row.get(3)?,
        updated_at: row.get(4)?,
    })
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
            "SELECT i.agent_id,
                    i.payload_json,
                    COALESCE(sequences.last_value, 0),
                    COALESCE(watermarks.oldest_retained_seq, 0)
             FROM agent_identities i
             LEFT JOIN runtime_sequences sequences
               ON sequences.domain = 'audit_event'
              AND sequences.scope_key = 'agent:' || i.agent_id
             LEFT JOIN audit_event_retention_watermarks watermarks
               ON watermarks.scope_key = 'agent:' || i.agent_id
             WHERE i.status = 'active' AND i.visibility = 'public'
             ORDER BY i.agent_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            identities.push(row?);
        }
    }

    let mut latest_briefs: HashMap<String, AgentRosterLatestBriefRow> = HashMap::new();
    {
        // Drive the lookup from the bounded public roster so SQLite selects
        // one latest Brief per member instead of repeating the selector for
        // every historical Brief belonging to that member.
        let mut statement = connection.prepare(AGENT_ROSTER_LATEST_BRIEFS_SQL)?;
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

    let to_seq = |value: i64| -> Result<u64> {
        u64::try_from(value).context("stored audit event sequence is negative")
    };
    let rows = identities
        .into_iter()
        .map(|(agent_id, identity_json, head, oldest)| {
            Ok(AgentRosterRow {
                event_head_seq: to_seq(head)?,
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
/// and no Agent-scoped sequence is claimed by two Briefs. Events whose
/// records were pruned by retention carry no linkage and therefore stay
/// acceptable.
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
             AND COALESCE(e.agent_id, json_extract(e.data_json, '$.data.agent_id')) = b.agent_id
             AND json_extract(e.data_json, '$.data.brief_id') = b.evidence_id
         ) != 1",
        [],
        |row| row.get(0),
    )?;
    let shared_sequences: i64 = connection.query_row(
        "SELECT COUNT(*) FROM (
           SELECT agent_id, created_event_seq FROM briefs
           WHERE created_event_seq IS NOT NULL
           GROUP BY agent_id, created_event_seq HAVING COUNT(*) > 1
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn insert_linked_brief(
        connection: &Connection,
        agent_id: &str,
        brief_id: &str,
        event_id: &str,
        event_seq: i64,
    ) -> Result<()> {
        connection.execute(
            "INSERT INTO briefs (
               evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
             ) VALUES (?1, ?2, '2026-01-01T00:00:00.000Z', 'result', 'preview', '{}', ?3)",
            params![brief_id, agent_id, event_seq],
        )?;
        connection.execute(
            "INSERT INTO audit_events (
               audit_event_id, event_seq, agent_id, kind, created_at, data_json
             ) VALUES (
               ?1, ?2, ?3, 'brief_created', '2026-01-01T00:00:00.000Z',
               json_object('data', json_object('agent_id', ?3, 'brief_id', ?4))
             )",
            params![event_id, event_seq, agent_id, brief_id],
        )?;
        Ok(())
    }

    #[test]
    fn brief_atomic_linkage_scopes_shared_sequences_by_agent() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let db = crate::runtime_db::RuntimeDb::open_and_migrate(
            temp_dir.path().join("state/runtime.sqlite"),
            temp_dir.path().join("state/runtime.lock"),
        )?;
        let connection = db.connection()?;

        insert_linked_brief(&connection, "alpha", "brief_alpha", "event_alpha", 1)?;
        insert_linked_brief(&connection, "beta", "brief_beta", "event_beta", 1)?;

        assert!(verify_brief_atomic_linkage(&connection)?);
        Ok(())
    }

    #[test]
    fn brief_atomic_linkage_rejects_same_agent_shared_sequence() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let db = crate::runtime_db::RuntimeDb::open_and_migrate(
            temp_dir.path().join("state/runtime.sqlite"),
            temp_dir.path().join("state/runtime.lock"),
        )?;
        let connection = db.connection()?;
        connection.execute_batch(
            "DROP INDEX briefs_agent_created_event_seq;
             DROP INDEX idx_audit_events_agent_event_seq_unique;",
        )?;

        insert_linked_brief(&connection, "alpha", "brief_one", "event_one", 1)?;
        insert_linked_brief(&connection, "alpha", "brief_two", "event_two", 1)?;

        assert!(!verify_brief_atomic_linkage(&connection)?);
        Ok(())
    }
}

/// Proves every stored audit event classifies soundly under the registry
/// classification used for the additive `projection_effect` field. Exact typed
/// and recognizable legacy events are both soundly classified.
/// Unsupported typed shapes cannot be inventoried by this binary, so
/// `events.projection-effect.v1` stays unadvertised.
fn verify_event_projection_effect_inventory(connection: &Connection) -> Result<bool> {
    if !table_exists(connection, "audit_events")? {
        return Ok(false);
    }
    let mut statement = connection.prepare(
        "SELECT kind,
                COALESCE(json_extract(data_json, '$.payload_schema'), ''),
                COALESCE(json_extract(data_json, '$.payload_schema_version'), 0),
                COALESCE(json_extract(data_json, '$.contract_version'), 1),
                COUNT(*)
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
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut unclassified = Vec::new();
    for row in rows {
        let (kind, payload_schema, payload_schema_version, contract_version, count) = row?;
        let classification = match (
            u32::try_from(contract_version),
            u32::try_from(payload_schema_version),
        ) {
            (Ok(contract_version), Ok(payload_schema_version)) => {
                crate::runtime_event::classify_projection_effect(
                    &kind,
                    contract_version,
                    &payload_schema,
                    payload_schema_version,
                )
            }
            _ => crate::runtime_event::ProjectionEffectClassification::Unsupported(
                crate::runtime_event::UnsupportedProjectionEvent::InvalidMetadata,
            ),
        };
        if let crate::runtime_event::ProjectionEffectClassification::Unsupported(reason) =
            classification
        {
            unclassified.push(format!(
                "{kind}@{payload_schema}v{payload_schema_version}/cv{contract_version}\
                 /reason={reason:?}/count={count}"
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

fn event_projection_effect_proof(
    connection: &Connection,
) -> Result<Option<EventProjectionEffectProof>> {
    connection
        .query_row(
            "SELECT verified, verification_version, event_log_epoch,
                    event_generation, verified_event_generation
             FROM observer_sync_capability_verifications
             WHERE capability = ?1",
            [EVENT_PROJECTION_EFFECT_COMPLETE],
            |row| {
                Ok(EventProjectionEffectProof {
                    verified: row.get::<_, i64>(0)? != 0,
                    verification_version: row.get(1)?,
                    event_log_epoch: row.get(2)?,
                    event_generation: row.get(3)?,
                    verified_event_generation: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn reusable_event_projection_effect_verification(
    connection: &Connection,
    event_log_epoch: &str,
) -> Result<Option<bool>> {
    Ok(
        event_projection_effect_proof(connection)?.and_then(|proof| {
            (proof.verification_version == EVENT_PROJECTION_EFFECT_VERIFIER_VERSION
                && proof.event_log_epoch.as_deref() == Some(event_log_epoch)
                && proof.event_generation >= 0
                && proof.event_generation == proof.verified_event_generation)
                .then_some(proof.verified)
        }),
    )
}

fn persist_event_projection_effect_verification(
    connection: &Connection,
    verified: bool,
    event_log_epoch: &str,
    verified_at: &str,
    detail: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO observer_sync_capability_verifications (
             capability, verified, verified_at, detail, verification_version,
             event_log_epoch, event_generation, verified_event_generation
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0)
         ON CONFLICT(capability) DO UPDATE SET
           verified = excluded.verified,
           verified_at = excluded.verified_at,
           detail = excluded.detail,
           verification_version = excluded.verification_version,
           event_log_epoch = excluded.event_log_epoch,
           verified_event_generation =
             observer_sync_capability_verifications.event_generation",
        rusqlite::params![
            EVENT_PROJECTION_EFFECT_COMPLETE,
            verified,
            verified_at,
            detail,
            EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
            event_log_epoch,
        ],
    )?;
    Ok(())
}

/// Advances the durable projection-effect proof for one event inserted by the
/// trusted append path. The insert trigger has already advanced
/// `event_generation`; only a proof that covered the immediately preceding
/// generation may advance. Writes outside the trusted helper therefore leave
/// a stale proof and force fail-closed full re-verification on next open.
pub(crate) fn record_appended_event_projection_effect(
    connection: &Connection,
    event: &AuditEvent,
) -> Result<()> {
    if !table_exists(connection, "observer_sync_capability_verifications")? {
        return Ok(());
    }
    let Some(proof) = event_projection_effect_proof(connection)? else {
        return Ok(());
    };
    if proof.verification_version != EVENT_PROJECTION_EFFECT_VERIFIER_VERSION
        || proof.event_log_epoch.as_deref() != Some(event.event_log_epoch.as_str())
        || proof.event_generation <= 0
        || proof.verified_event_generation != proof.event_generation - 1
    {
        return Ok(());
    }

    let classification = crate::runtime_event::classify_projection_effect(
        &event.kind,
        event.contract_version,
        &event.payload_schema,
        event.payload_schema_version,
    );
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let updated = match classification {
        crate::runtime_event::ProjectionEffectClassification::Exact(_)
        | crate::runtime_event::ProjectionEffectClassification::ConservativeLegacy(_) => connection
            .execute(
                "UPDATE observer_sync_capability_verifications
                 SET verified_event_generation = event_generation,
                     verified_at = ?1
                 WHERE capability = ?2
                   AND verification_version = ?3
                   AND event_log_epoch = ?4
                   AND event_generation = ?5
                   AND verified_event_generation = ?6",
                rusqlite::params![
                    now,
                    EVENT_PROJECTION_EFFECT_COMPLETE,
                    EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
                    event.event_log_epoch,
                    proof.event_generation,
                    proof.verified_event_generation,
                ],
            )?,
        crate::runtime_event::ProjectionEffectClassification::Unsupported(reason) => {
            let detail = serde_json::json!({
                "source": "trusted_append",
                "complete": false,
                "kind": event.kind,
                "reason": format!("{reason:?}"),
            })
            .to_string();
            connection.execute(
                "UPDATE observer_sync_capability_verifications
                 SET verified = 0,
                     verified_at = ?1,
                     detail = ?2,
                     verified_event_generation = event_generation
                 WHERE capability = ?3
                   AND verification_version = ?4
                   AND event_log_epoch = ?5
                   AND event_generation = ?6
                   AND verified_event_generation = ?7",
                rusqlite::params![
                    now,
                    detail,
                    EVENT_PROJECTION_EFFECT_COMPLETE,
                    EVENT_PROJECTION_EFFECT_VERIFIER_VERSION,
                    event.event_log_epoch,
                    proof.event_generation,
                    proof.verified_event_generation,
                ],
            )?
        }
    };
    if updated == 0 {
        tracing::debug!(
            event_id = %event.id,
            event_generation = proof.event_generation,
            verified_event_generation = proof.verified_event_generation,
            "projection-effect proof did not advance after trusted append"
        );
    }
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
                    .get("id")
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
        let verified = |capability: &str| -> bool {
            match connection
                .query_row(
                    "SELECT verified FROM observer_sync_capability_verifications
                     WHERE capability = ?1",
                    [capability],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
            {
                Ok(value) => value.is_some_and(|value| value != 0),
                Err(error) => {
                    tracing::debug!(
                        capability,
                        %error,
                        "failed to load observer sync capability verification"
                    );
                    false
                }
            }
        };
        let event_projection_effect_complete = read_metadata(&connection, "event_log_epoch")
            .and_then(|event_log_epoch| {
                reusable_event_projection_effect_verification(&connection, &event_log_epoch)
            })
            .unwrap_or_else(|error| {
                tracing::debug!(
                    %error,
                    "failed to load observer sync event projection verification proof"
                );
                None
            })
            .unwrap_or(false);
        Ok(ObserverSyncFoundationVerification {
            runtime_identity_stable: verified(RUNTIME_IDENTITY_STABLE),
            agent_identity_reserved: verified(AGENT_IDENTITY_RESERVED),
            roster_snapshot_verified: verified(ROSTER_SNAPSHOT_VERIFIED),
            projection_snapshot_verified: verified(PROJECTION_SNAPSHOT_VERIFIED),
            event_projection_effect_complete,
            brief_atomic_linkage_verified: verified(BRIEF_ATOMIC_LINKAGE_VERIFIED),
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

    /// Reads one Agent's canonical projection anchors — membership,
    /// committed state, event window, current WorkItem, conversation
    /// anchors, and latest Brief — plus the runtime identity metadata
    /// inside one deferred read transaction, so all values share one
    /// committed database view and the boundary equals that view's
    /// per-Agent event head.
    pub fn agent_projection_snapshot_rows(
        &self,
        agent_id: &str,
    ) -> Result<AgentProjectionSnapshotRows> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let rows = collect_agent_projection_anchors(&transaction, agent_id)?;
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
        let scope_key = crate::runtime_db::evidence::audit_event_sequence_scope(agent_id);
        let (epoch, oldest, head): (String, i64, i64) = connection.query_row(
            "SELECT
               (SELECT value FROM runtime_metadata WHERE key = 'event_log_epoch'),
               COALESCE((
                 SELECT oldest_retained_seq FROM audit_event_retention_watermarks
                 WHERE scope_key = ?1
               ), 0),
               COALESCE((
                 SELECT last_value FROM runtime_sequences
                 WHERE domain = 'audit_event' AND scope_key = ?1
               ), 0)",
            [scope_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(AgentEventRecoveryWindow {
            event_log_epoch: epoch,
            oldest_retained_seq: u64::try_from(oldest)
                .context("stored audit retention watermark is negative")?,
            event_head_seq: u64::try_from(head)
                .context("stored audit event sequence is negative")?,
        })
    }
}
