//! Coherent Episode reads and explicit, immutable first-upgrade continuity.
//!
//! This module is staged with the D1 schema; it is not a general history loader.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    domain::execution_protocol::{ExecutionAttempt, ExecutionBinding, ExecutionSourceIdentity},
    memory::refs::RuntimeRef,
    types::{
        AgentIdentityRecord, AgentRegistryStatus, AgentState, BriefRecord, ContextEpisodeRecord,
        MessageEnvelope, TaskRecord, ToolExecutionRecord, TurnRecord, WaitConditionRecord,
        WorkItemContinuationFrame, WorkItemRecord, WorkItemRefStatus, WorkItemState,
    },
};

use super::{
    episode_memory::{
        load_receipt, EpisodeMemoryInvocation, EpisodeMemoryKey, EpisodeMemoryReceipt,
        EPISODE_MEMORY_PHASE,
    },
    repositories::{
        decode_agent_identity_payload, decode_agent_state_payload, decode_context_episode_payload,
    },
    RuntimeDb,
};

#[path = "episode_snapshot_task.rs"]
pub(super) mod task_checkpoint;
#[path = "episode_snapshot_work.rs"]
pub(super) mod work_checkpoint;

pub(crate) const EPISODE_CONTINUITY_POLICY: i64 = 1;
pub(crate) const LEGACY_EPISODE_RECENT_CANDIDATES: usize = 12;
// Upgrade is fail-closed rather than silently truncating mandatory lineage.
const CONTINUITY_MAX_RECORDS: usize = 65_536;
const CONTINUITY_MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const CONTINUITY_MAX_REF_DEPTH: usize = 64;
const UPGRADE_BOUNDARY_KEY: &str = "episode_continuity/v1/upgrade_boundary";

/// Installed only by migration v82, before identity creation or runtime activation.
pub(super) const EPISODE_CONTINUITY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS episode_continuity_seeds (
  seed_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  policy_version INTEGER NOT NULL CHECK (policy_version = 1),
  boundary_id TEXT NOT NULL,
  identity_incarnation INTEGER CHECK (identity_incarnation IS NULL OR identity_incarnation > 0),
  source_kind TEXT NOT NULL CHECK (source_kind IN (
    'upgrade_identity', 'upgrade_unbound', 'legacy_identity_import', 'legacy_state_import'
  )),
  identity_digest TEXT,
  roots_json TEXT NOT NULL,
  candidate_payloads_json TEXT NOT NULL,
  import_required INTEGER NOT NULL CHECK (import_required IN (0, 1)),
  captured_at TEXT NOT NULL,
  UNIQUE (agent_id, policy_version)
);
CREATE TABLE IF NOT EXISTS agent_episode_continuity (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK (identity_incarnation > 0),
  policy_version INTEGER NOT NULL CHECK (policy_version = 1),
  mode TEXT NOT NULL CHECK (mode IN ('legacy_adopted', 'fresh_empty')),
  seed_id TEXT,
  bound_at TEXT NOT NULL,
  CHECK ((mode = 'fresh_empty' AND seed_id IS NULL)
      OR (mode = 'legacy_adopted' AND seed_id IS NOT NULL)),
  PRIMARY KEY (agent_id, identity_incarnation, policy_version),
  FOREIGN KEY (seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS episode_continuity_members (
  seed_id TEXT NOT NULL,
  capture_phase TEXT NOT NULL CHECK (capture_phase IN ('upgrade', 'first_import')),
  episode_id TEXT NOT NULL,
  content_digest TEXT NOT NULL,
  qualification_json TEXT NOT NULL,
  PRIMARY KEY (seed_id, capture_phase, episode_id),
  FOREIGN KEY (seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE INDEX IF NOT EXISTS idx_episode_continuity_member_episode
  ON episode_continuity_members(episode_id, seed_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_episode_continuity_seed_binding
  ON agent_episode_continuity(seed_id) WHERE seed_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS episode_continuity_imports (
  seed_id TEXT PRIMARY KEY,
  checkpoint_digest TEXT NOT NULL,
  roots_json TEXT NOT NULL,
  captured_at TEXT NOT NULL,
  FOREIGN KEY (seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeMemoryLoadScope {
    pub agent_id: String,
    pub identity_incarnation: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct EpisodeMemoryCurrentSnapshot {
    pub identity: AgentIdentityRecord,
    pub state: Option<AgentState>,
    pub control_revision: Option<u64>,
    pub episodes: Vec<ContextEpisodeRecord>,
    pub has_more: bool,
    pub continuity: EpisodeContinuityBinding,
    pub receipt: Option<EpisodeMemoryReceipt>,
}

#[derive(Debug, Clone)]
pub(crate) enum EpisodeMemoryBootstrapSnapshot {
    Current(EpisodeMemoryCurrentSnapshot),
    Retired {
        requested_scope: EpisodeMemoryLoadScope,
        current_identity: Option<AgentIdentityRecord>,
        receipt: Option<EpisodeMemoryReceipt>,
    },
}

impl EpisodeMemoryBootstrapSnapshot {
    pub(crate) fn require_current(self) -> Result<EpisodeMemoryCurrentSnapshot> {
        match self {
            Self::Current(current) => Ok(current),
            Self::Retired { .. } => Err(EpisodeSnapshotError::RetiredScope.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum EpisodeContinuityBinding {
    FreshEmpty,
    LegacyAdopted {
        seed_id: String,
        boundary_id: String,
        source_kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EpisodeSnapshotError {
    RetiredScope,
    MissingContinuityBinding,
    LegacyImportPending,
    BindingConflict,
    LegacyContentChanged { episode_id: String },
    ActiveBuilderCollision { episode_id: String },
    ClosureResourceExhausted { limit: &'static str },
}

impl fmt::Display for EpisodeSnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RetiredScope => f.write_str("episode snapshot identity scope is retired"),
            Self::MissingContinuityBinding => {
                f.write_str("episode snapshot has no explicit continuity binding")
            }
            Self::LegacyImportPending => {
                f.write_str("episode continuity first legacy import has not completed")
            }
            Self::BindingConflict => f.write_str("immutable episode continuity binding conflicts"),
            Self::LegacyContentChanged { episode_id } => {
                write!(f, "adopted legacy episode content changed: {episode_id}")
            }
            Self::ActiveBuilderCollision { episode_id } => {
                write!(
                    f,
                    "active episode builder is already archived: {episode_id}"
                )
            }
            Self::ClosureResourceExhausted { limit } => {
                write!(
                    f,
                    "episode continuity mandatory closure exceeds its {limit} bound"
                )
            }
        }
    }
}
impl std::error::Error for EpisodeSnapshotError {}

impl RuntimeDb {
    /// Host construction finishes these fixed legacy scopes before admitting work.
    pub(crate) fn pending_legacy_episode_import_agent_ids(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.agent_id FROM episode_continuity_seeds s
             WHERE s.import_required = 1
               AND NOT EXISTS (SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id = s.seed_id)
               AND NOT EXISTS (SELECT 1 FROM agent_episode_continuity b WHERE b.agent_id = s.agent_id AND b.mode = 'fresh_empty')
             ORDER BY s.agent_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Only the explicit first legacy-default bootstrap actor uses this probe.
    pub(crate) fn has_unbound_episode_continuity_seed(&self, agent_id: &str) -> Result<bool> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM episode_continuity_seeds s
               WHERE s.agent_id = ?1 AND s.identity_incarnation IS NULL AND s.policy_version = ?2)
             AND NOT EXISTS (SELECT 1 FROM agent_episode_continuity WHERE agent_id = ?1)",
            params![agent_id, EPISODE_CONTINUITY_POLICY],
            |row| row.get(0),
        )?)
    }

    /// This fresh read transaction is also the receipt resolver's observation.
    /// Receipt absence alone never authorizes opening a mutation gate.
    pub(crate) fn load_episode_memory_snapshot(
        &self,
        scope: &EpisodeMemoryLoadScope,
        recent_limit: usize,
        witness: Option<(&EpisodeMemoryKey, &str, &EpisodeMemoryInvocation)>,
    ) -> Result<EpisodeMemoryBootstrapSnapshot> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let snapshot = load_episode_memory_snapshot_in(&transaction, scope, recent_limit, witness)?;
        transaction.commit()?;
        Ok(snapshot)
    }
}

pub(crate) fn load_episode_memory_snapshot_in(
    connection: &Connection,
    scope: &EpisodeMemoryLoadScope,
    recent_limit: usize,
    witness: Option<(&EpisodeMemoryKey, &str, &EpisodeMemoryInvocation)>,
) -> Result<EpisodeMemoryBootstrapSnapshot> {
    load_episode_memory_snapshot_inner(connection, scope, recent_limit, witness, false)
}
/// Bounded read by an already accepted original input receipt. It grants no new
/// Episode writer or activation permission to a Deleting identity.
pub(crate) fn load_episode_memory_for_input_recovery_in(
    connection: &Connection,
    scope: &EpisodeMemoryLoadScope,
    recent_limit: usize,
) -> Result<EpisodeMemoryBootstrapSnapshot> {
    load_episode_memory_snapshot_inner(connection, scope, recent_limit, None, true)
}
fn load_episode_memory_snapshot_inner(
    connection: &Connection,
    scope: &EpisodeMemoryLoadScope,
    recent_limit: usize,
    witness: Option<(&EpisodeMemoryKey, &str, &EpisodeMemoryInvocation)>,
    original_recovery: bool,
) -> Result<EpisodeMemoryBootstrapSnapshot> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid Episode scope"
    );
    // The first SELECT fixes the same snapshot for identity, receipt and payloads.
    let identity = load_identity(connection, &scope.agent_id)?;
    let receipt = if let Some((key, digest, invocation)) = witness {
        ensure!(
            key.agent_id == scope.agent_id
                && key.identity_incarnation == scope.identity_incarnation
                && !digest.is_empty(),
            "Episode witness does not match the requested scope"
        );
        load_receipt(connection, key, Some(digest), invocation)?
    } else {
        None
    };
    let Some(identity) = identity.filter(|identity| {
        identity.incarnation == scope.identity_incarnation
            && (identity.status == AgentRegistryStatus::Active
                || (original_recovery && identity.status == AgentRegistryStatus::Deleting))
    }) else {
        return Ok(EpisodeMemoryBootstrapSnapshot::Retired {
            requested_scope: scope.clone(),
            current_identity: load_identity(connection, &scope.agent_id)?,
            receipt,
        });
    };
    let (state, control_revision) = load_state(connection, &scope.agent_id)?;
    validate_active_builder(connection, state.as_ref())?;
    let continuity = load_continuity_binding(connection, scope)?;
    let seed_id = match &continuity {
        EpisodeContinuityBinding::FreshEmpty => None,
        EpisodeContinuityBinding::LegacyAdopted { seed_id, .. } => Some(seed_id.as_str()),
    };
    if recent_limit > 512 {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "recent window count",
        }
        .into());
    }
    let limit = i64::try_from(recent_limit + 1)?;
    let mut statement = connection.prepare(
        "SELECT e.episode_id, e.agent_id, e.work_item_id, e.payload_json
         FROM context_episode_anchors e
         WHERE e.agent_id = ?1 AND (
           EXISTS (SELECT 1 FROM episode_memory_results r
             WHERE r.agent_id = ?1 AND r.identity_incarnation = ?2
               AND r.phase_version = ?3 AND r.finalized_episode_id = e.episode_id)
           OR (?4 IS NOT NULL AND EXISTS (
             SELECT 1 FROM episode_continuity_members m
             WHERE m.seed_id = ?4 AND m.episode_id = e.episode_id)))
         ORDER BY e.ended_at DESC, e.started_at DESC, e.episode_id ASC LIMIT ?5",
    )?;
    let rows = statement.query_map(
        params![
            scope.agent_id,
            i64::try_from(scope.identity_incarnation)?,
            EPISODE_MEMORY_PHASE,
            seed_id,
            limit
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    let mut episodes = Vec::new();
    let mut payload_bytes = 0usize;
    for row in rows {
        let (id, agent_id, work_item_id, payload) = row?;
        payload_bytes = payload_bytes.saturating_add(payload.len());
        if payload_bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
            return Err(EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "payload bytes",
            }
            .into());
        }
        let episode = decode_context_episode_payload(&payload)?;
        ensure!(
            episode.id == id
                && episode.agent_id == agent_id
                && episode.agent_id == scope.agent_id
                && episode.current_work_item_id == work_item_id,
            "Episode columns and payload disagree"
        );
        validate_episode_qualification(connection, scope, seed_id, &episode, &payload)?;
        episodes.push(episode);
    }
    let has_more = episodes.len() > recent_limit;
    episodes.truncate(recent_limit);
    episodes.reverse();
    Ok(EpisodeMemoryBootstrapSnapshot::Current(
        EpisodeMemoryCurrentSnapshot {
            identity,
            state,
            control_revision,
            episodes,
            has_more,
            continuity,
            receipt,
        },
    ))
}

fn load_identity(connection: &Connection, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
    connection
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            let identity = decode_agent_identity_payload(&payload)?;
            ensure!(
                identity.agent_id == agent_id,
                "identity projection and payload disagree"
            );
            Ok(identity)
        })
        .transpose()
}

fn load_state(
    connection: &Connection,
    agent_id: &str,
) -> Result<(Option<AgentState>, Option<u64>)> {
    let row = connection
        .query_row(
            "SELECT payload_json, control_revision FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    row.map(|(payload, revision)| {
        let state = decode_agent_state_payload(&payload)?;
        ensure!(
            state.id == agent_id,
            "State projection and payload disagree"
        );
        Ok((Some(state), Some(u64::try_from(revision)?)))
    })
    .unwrap_or(Ok((None, None)))
}

fn validate_active_builder(connection: &Connection, state: Option<&AgentState>) -> Result<()> {
    let Some(state) = state else {
        return Ok(());
    };
    let memory = &state.working_memory;
    ensure!(
        memory.active_episode_id.as_deref()
            == memory
                .active_episode_builder
                .as_ref()
                .map(|builder| builder.id.as_str()),
        "active Episode alias and builder disagree"
    );
    if let Some(builder) = memory.active_episode_builder.as_ref() {
        let archived: bool = connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM context_episode_anchors WHERE episode_id = ?1)",
            [&builder.id],
            |row| row.get(0),
        )?;
        if archived {
            return Err(EpisodeSnapshotError::ActiveBuilderCollision {
                episode_id: builder.id.clone(),
            }
            .into());
        }
    }
    Ok(())
}

fn load_continuity_binding(
    connection: &Connection,
    scope: &EpisodeMemoryLoadScope,
) -> Result<EpisodeContinuityBinding> {
    let row = connection
        .query_row(
            "SELECT mode, seed_id FROM agent_episode_continuity
         WHERE agent_id = ?1 AND identity_incarnation = ?2 AND policy_version = ?3",
            params![
                scope.agent_id,
                i64::try_from(scope.identity_incarnation)?,
                EPISODE_CONTINUITY_POLICY
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?
        .ok_or(EpisodeSnapshotError::MissingContinuityBinding)?;
    match row {
        (mode, None) if mode == "fresh_empty" => Ok(EpisodeContinuityBinding::FreshEmpty),
        (mode, Some(seed_id)) if mode == "legacy_adopted" => {
            let (agent_id, incarnation, boundary_id, source_kind, required, sealed) = connection.query_row(
                "SELECT s.agent_id, s.identity_incarnation, s.boundary_id, s.source_kind, s.import_required,
                  EXISTS (SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id = s.seed_id)
                 FROM episode_continuity_seeds s WHERE s.seed_id = ?1 AND s.policy_version = ?2",
                params![seed_id, EPISODE_CONTINUITY_POLICY],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, bool>(4)?, row.get::<_, bool>(5)?)),
            )?;
            ensure!(
                agent_id == scope.agent_id
                    && incarnation.is_none_or(
                        |inc| u64::try_from(inc).ok() == Some(scope.identity_incarnation)
                    ),
                "continuity seed scope does not match its binding"
            );
            if required && !sealed {
                return Err(EpisodeSnapshotError::LegacyImportPending.into());
            }
            Ok(EpisodeContinuityBinding::LegacyAdopted {
                seed_id,
                boundary_id,
                source_kind,
            })
        }
        _ => Err(EpisodeSnapshotError::BindingConflict.into()),
    }
}

fn validate_episode_qualification(
    connection: &Connection,
    scope: &EpisodeMemoryLoadScope,
    seed_id: Option<&str>,
    episode: &ContextEpisodeRecord,
    payload: &str,
) -> Result<()> {
    let mut receipt_statement = connection.prepare(
        "SELECT turn_id, command_digest, invocation_json FROM episode_memory_results
         WHERE agent_id = ?1 AND identity_incarnation = ?2 AND phase_version = ?3 AND finalized_episode_id = ?4",
    )?;
    let rows = receipt_statement.query_map(
        params![
            scope.agent_id,
            i64::try_from(scope.identity_incarnation)?,
            EPISODE_MEMORY_PHASE,
            episode.id
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    let mut qualified = false;
    for row in rows {
        let (turn_id, digest, invocation) = row?;
        let key = EpisodeMemoryKey {
            agent_id: scope.agent_id.clone(),
            identity_incarnation: scope.identity_incarnation,
            actual_dispatch_turn_id: turn_id,
        };
        let invocation = serde_json::from_str(&invocation)?;
        let receipt = load_receipt(connection, &key, Some(&digest), &invocation)?
            .context("qualified Episode receipt disappeared inside read snapshot")?;
        ensure!(
            receipt.outcome.finalized_episode_id.as_deref() == Some(episode.id.as_str()),
            "Episode receipt payload does not qualify its projection"
        );
        qualified = true;
    }
    if let Some(seed_id) = seed_id {
        let mut statement = connection.prepare("SELECT content_digest FROM episode_continuity_members WHERE seed_id = ?1 AND episode_id = ?2")?;
        let digests =
            statement.query_map(params![seed_id, episode.id], |row| row.get::<_, String>(0))?;
        for digest in digests {
            if digest? != bytes_digest(payload.as_bytes()) {
                return Err(EpisodeSnapshotError::LegacyContentChanged {
                    episode_id: episode.id.clone(),
                }
                .into());
            }
            qualified = true;
        }
    }
    ensure!(qualified, "Episode has no current scope qualification");
    Ok(())
}

fn bytes_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct EpisodeContinuityRoots {
    work_item_ids: BTreeSet<String>,
    source_refs: BTreeSet<String>,
    /// Exact canonical records, not a claim about their original incarnation.
    record_digests: BTreeMap<String, String>,
    unavailable_refs: BTreeSet<String>,
    /// Frozen by the same explicit legacy actor; absence is not observed None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_domain_checkpoint: Option<work_checkpoint::WorkContinuityCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_domain_checkpoint: Option<task_checkpoint::TaskContinuityCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduling_domain_checkpoint:
        Option<super::scheduling_snapshot::SchedulingContinuityCheckpoint>,
}

#[derive(Debug)]
struct ContinuitySeed {
    id: String,
    agent_id: String,
    incarnation: Option<u64>,
    import_required: bool,
}

/// Called once inside migration v82, under the existing DB migration lease.
/// The boundary and each initial membership set are frozen in that transaction.
/// Existing Deleting scopes retain their admitted cleanup provenance; this does
/// not authorize activation or new input admission.
pub(super) fn capture_episode_continuity_upgrade_tx(
    tx: &Transaction<'_>,
    captured_at: DateTime<Utc>,
) -> Result<()> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [UPGRADE_BOUNDARY_KEY],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(());
    }
    task_checkpoint::queries::ensure_schema(tx)?;
    let boundary = crate::ids::runtime_id("episode_cutover");
    let at = captured_at.to_rfc3339();
    tx.execute(
        "INSERT INTO runtime_metadata (key, value, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![UPGRADE_BOUNDARY_KEY, boundary, at],
    )?;
    // Released name-accepted histories may lack an original source table.
    // Freeze only rows that exist at this exact upgrade checkpoint; the fixed
    // boundary below never recaptures a table or row introduced later.
    let tables = {
        let mut statement = tx.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<BTreeSet<_>>>()?
    };
    let inventory_sources = [
        ("agent_identities", "SELECT agent_id FROM agent_identities"),
        ("agent_states", "SELECT agent_id FROM agent_states"),
        ("work_items", "SELECT agent_id FROM work_items WHERE state IN ('open', 'completing') OR EXISTS (SELECT 1 FROM json_each(work_items.payload_json, '$.work_refs') r WHERE json_extract(r.value, '$.status') = 'active')"),
        ("wait_conditions", "SELECT agent_id FROM wait_conditions WHERE status IN ('active', 'triggered')"),
        ("queue_entries", "SELECT agent_id FROM queue_entries WHERE status IN ('queued', 'dequeued', 'interrupted', 'interjected')"),
        ("tasks", "SELECT owner_agent_id FROM tasks WHERE status IN ('queued', 'running', 'cancelling')"),
        ("work_item_continuations", "SELECT agent_id FROM work_item_continuations WHERE state = 'active'"),
        ("task_result_settlements", "SELECT agent_id FROM task_result_settlements WHERE state != 'settled'"),
        ("execution_protocol_attempts", "SELECT agent_id FROM execution_protocol_attempts WHERE lifecycle_state != 'settled'"),
        ("execution_protocol_work_items", "SELECT agent_id FROM execution_protocol_work_items WHERE lifecycle_state != 'terminal'"),
        ("turn_records", "SELECT agent_id FROM turn_records WHERE json_extract(payload_json, '$.terminal') IS NULL"),
        ("timers", "SELECT agent_id FROM timers WHERE status IN ('active','scheduled')"),
        ("timer_wakes", "SELECT t.agent_id FROM timer_wakes w JOIN timers t ON t.timer_id=w.timer_id WHERE w.status='pending'"),
        ("external_triggers", "SELECT target_agent_id FROM external_triggers WHERE status='active'"),
    ];
    let queries = inventory_sources
        .into_iter()
        .filter_map(|(table, query)| {
            (tables.contains(table) && (table != "timer_wakes" || tables.contains("timers")))
                .then_some(query)
        })
        .collect::<Vec<_>>();
    let agent_ids = if queries.is_empty() {
        Vec::new()
    } else {
        let first_column = if queries[0].starts_with("SELECT owner_agent_id") {
            "owner_agent_id"
        } else if queries[0].starts_with("SELECT target_agent_id") {
            "target_agent_id"
        } else {
            "agent_id"
        };
        let selected = format!("{} ORDER BY 1", queries.join(" UNION "));
        let (count, bytes): (usize, usize) = tx.query_row(
            &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST({first_column} AS BLOB))),0) FROM ({selected})"),
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            count <= CONTINUITY_MAX_RECORDS && bytes <= CONTINUITY_MAX_PAYLOAD_BYTES,
            EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "scope inventory"
            }
        );
        let mut statement = tx.prepare(&selected)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let import_required = !tables.contains("storage_domains") || !episode_import_complete(tx)?;
    for agent_id in agent_ids {
        let identity = if tables.contains("agent_identities") {
            load_identity(tx, &agent_id)?
        } else {
            None
        };
        if identity
            .as_ref()
            .is_some_and(|identity| identity.status == AgentRegistryStatus::Deleted)
        {
            continue;
        }
        let roots = collect_live_episode_roots_with_schema(tx, &agent_id, Some(&tables))?;
        let records = if tables.contains("context_episode_anchors") {
            all_episode_rows(tx, &agent_id)?
        } else {
            Vec::new()
        };
        let seed = insert_seed_with_candidates_tx(
            tx,
            &agent_id,
            identity.as_ref(),
            if identity.is_some() {
                "upgrade_identity"
            } else {
                "upgrade_unbound"
            },
            &roots,
            import_required,
            captured_at,
            &records,
        )?;
        capture_members_tx(tx, &seed.id, "upgrade", &roots, records)?;
        if let Some(identity) = identity.as_ref() {
            insert_legacy_binding_tx(tx, identity, &seed, captured_at)?;
        }
    }
    Ok(())
}

/// The central identity upsert calls this only for a newly created namespace.
/// Existing explicit legacy binding is preserved; a Fresh binding never upgrades.
pub(super) fn ensure_fresh_episode_continuity_tx(
    tx: &Transaction<'_>,
    identity: &AgentIdentityRecord,
    bound_at: DateTime<Utc>,
) -> Result<()> {
    ensure!(identity.incarnation > 0, "invalid fresh Episode namespace");
    // Earlier migrations also use the common identity upsert helper.
    let installed: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_episode_continuity')", [], |row| row.get(0))?;
    if !installed {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO agent_episode_continuity (agent_id, identity_incarnation, policy_version, mode, seed_id, bound_at)
         VALUES (?1, ?2, ?3, 'fresh_empty', NULL, ?4) ON CONFLICT DO NOTHING",
        params![identity.agent_id, i64::try_from(identity.incarnation)?, EPISODE_CONTINUITY_POLICY, bound_at.to_rfc3339()],
    )?;
    Ok(())
}

/// Explicit legacy identity import, called before its first canonical upsert.
/// Ordinary identity creation does not call this actor. Imported Deleting scopes
/// may retain their original cleanup provenance, pending the first-import seal.
pub(super) fn prepare_imported_identity_continuity_tx(
    tx: &Transaction<'_>,
    identity: &AgentIdentityRecord,
    captured_at: DateTime<Utc>,
) -> Result<()> {
    if identity.status == AgentRegistryStatus::Deleted {
        return Ok(());
    }
    if let Some(current) = load_identity(tx, &identity.agent_id)? {
        ensure!(
            current.incarnation == identity.incarnation,
            "legacy identity import conflicts with current namespace"
        );
        // Existing canonical identities were classified by the fixed migration.
        return Ok(());
    }
    ensure_no_prior_binding(tx, &identity.agent_id)?;
    let seed = match find_seed(tx, &identity.agent_id)? {
        Some(seed) => seed,
        None => {
            let roots = collect_live_episode_roots(tx, &identity.agent_id)?;
            let seed = insert_seed_tx(
                tx,
                &identity.agent_id,
                Some(identity),
                "legacy_identity_import",
                &roots,
                !episode_import_complete(tx)?,
                captured_at,
            )?;
            capture_members_tx(
                tx,
                &seed.id,
                "upgrade",
                &roots,
                all_episode_rows(tx, &identity.agent_id)?,
            )?;
            seed
        }
    };
    insert_legacy_binding_tx(tx, identity, &seed, captured_at)
}

/// Called from the first State JSON import transaction after the exact State write.
/// A new Fresh identity or any previous binding makes this path ineligible.
pub(super) fn prepare_imported_state_continuity_tx(
    tx: &Transaction<'_>,
    state: &AgentState,
    captured_at: DateTime<Utc>,
) -> Result<()> {
    if load_identity(tx, &state.id)?.is_some() || has_any_binding(tx, &state.id)? {
        return Ok(());
    }
    if find_seed(tx, &state.id)?.is_none() {
        let roots = collect_live_episode_roots(tx, &state.id)?;
        let seed = insert_seed_tx(
            tx,
            &state.id,
            None,
            "legacy_state_import",
            &roots,
            !episode_import_complete(tx)?,
            captured_at,
        )?;
        capture_members_tx(
            tx,
            &seed.id,
            "upgrade",
            &roots,
            all_episode_rows(tx, &state.id)?,
        )?;
    }
    Ok(())
}

/// Sole explicit no-identity legacy bootstrap actor. Call before the first identity
/// upsert in the SAME identity/relations transaction, after legacy import completes.
pub(super) fn bind_unbound_episode_continuity_tx(
    tx: &Transaction<'_>,
    identity: &AgentIdentityRecord,
    bound_at: DateTime<Utc>,
) -> Result<bool> {
    ensure!(
        identity.status == AgentRegistryStatus::Active,
        "legacy materialization requires an Active identity"
    );
    ensure!(
        load_identity(tx, &identity.agent_id)?.is_none(),
        "legacy materialization requires no current identity"
    );
    ensure_no_prior_binding(tx, &identity.agent_id)?;
    let Some(seed) = find_seed(tx, &identity.agent_id)? else {
        return Ok(false);
    };
    ensure!(
        seed.incarnation.is_none(),
        "legacy materialization cannot adopt a different identity scope"
    );
    if seed.import_required {
        let sealed: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM episode_continuity_imports WHERE seed_id = ?1)",
            [&seed.id],
            |row| row.get(0),
        )?;
        if !sealed {
            return Err(EpisodeSnapshotError::LegacyImportPending.into());
        }
    }
    insert_legacy_binding_tx(tx, identity, &seed, bound_at)?;
    Ok(true)
}

/// The first Episode JSON import adds one immutable checkpoint, in the same
/// transaction as the imported records. Eligibility was fixed before that import.
/// Callers must still be preactivation, with the existing bootstrap authority.
pub(super) fn seal_imported_episode_continuity_tx(
    tx: &Transaction<'_>,
    imported: &[ContextEpisodeRecord],
    captured_at: DateTime<Utc>,
) -> Result<()> {
    let seeds = {
        let mut statement = tx.prepare(
            "SELECT seed_id, agent_id, identity_incarnation, import_required FROM episode_continuity_seeds s
             WHERE import_required = 1 AND NOT EXISTS (SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id = s.seed_id)",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (id, agent_id, incarnation, import_required) = row?;
            Ok(ContinuitySeed {
                id,
                agent_id,
                incarnation: incarnation.map(u64::try_from).transpose()?,
                import_required,
            })
        })
        .collect::<Result<Vec<_>>>()?
    };
    for seed in seeds {
        if let Some(current) = load_identity(tx, &seed.agent_id)? {
            if seed
                .incarnation
                .is_some_and(|incarnation| incarnation != current.incarnation)
                || current.status == AgentRegistryStatus::Deleted
            {
                continue;
            }
            let mode: Option<String> = tx.query_row(
                "SELECT mode FROM agent_episode_continuity WHERE agent_id = ?1 AND identity_incarnation = ?2 AND policy_version = ?3",
                params![seed.agent_id, i64::try_from(current.incarnation)?, EPISODE_CONTINUITY_POLICY], |row| row.get(0),
            ).optional()?;
            if mode.as_deref() == Some("fresh_empty") {
                continue;
            }
        } else if has_any_binding(tx, &seed.agent_id)? {
            continue;
        }
        // The original seed owns optional history. Import seals only its new
        // mandatory roots, without rescanning a later non-open Work tail.
        let (frozen_roots, candidates): (String, String) = tx.query_row(
            "SELECT roots_json, candidate_payloads_json FROM episode_continuity_seeds WHERE seed_id = ?1",
            [&seed.id], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let frozen_roots: EpisodeContinuityRoots = serde_json::from_str(&frozen_roots)?;
        let work_import = work_checkpoint::imports::ImportAuthority::load(
            tx,
            &seed.agent_id,
            frozen_roots.clone(),
        )?;
        let mut roots = collect_live_episode_roots_with_work_authority(
            tx,
            &seed.agent_id,
            None,
            false,
            Some(&work_import),
            frozen_roots.task_domain_checkpoint.as_ref(),
        )?;
        roots.scheduling_domain_checkpoint =
            Some(super::scheduling_snapshot::capture_fixed_checkpoint_on(
                tx,
                &seed.agent_id,
                None,
                Some(
                    frozen_roots
                        .scheduling_domain_checkpoint
                        .as_ref()
                        .context("first Scheduling import lacks its original fixed checkpoint")?,
                ),
            )?);
        roots.work_item_ids.extend(frozen_roots.work_item_ids);
        roots.source_refs.extend(frozen_roots.source_refs);
        roots.unavailable_refs.extend(frozen_roots.unavailable_refs);
        let candidates: BTreeMap<String, String> = serde_json::from_str(&candidates)?;
        let mut rows = Vec::new();
        let mut imported_ids = BTreeSet::new();
        let mut imported_digests = BTreeMap::new();
        for record in imported
            .iter()
            .filter(|record| record.agent_id == seed.agent_id)
        {
            imported_ids.insert(record.id.clone());
        }
        for id in &imported_ids {
            if let Some(row) = episode_row(tx, id)? {
                ensure!(
                    row.record.agent_id == seed.agent_id,
                    "legacy Episode import id belongs to another agent"
                );
                imported_digests.insert(id.clone(), bytes_digest(row.payload.as_bytes()));
                rows.push(row);
            }
        }
        // Frozen upgrade candidates can become reachable through an explicitly
        // imported legacy root. Ordinary post-cutover rows are not candidates.
        for (id, digest) in candidates {
            if imported_ids.contains(&id) {
                continue;
            }
            if let Some(row) = episode_row(tx, &id)? {
                if bytes_digest(row.payload.as_bytes()) == digest {
                    rows.push(row);
                } else {
                    roots.unavailable_refs.insert(format!("episode:{id}"));
                }
            }
        }
        let checkpoint =
            serde_json::json!({ "episode_payloads": imported_digests, "roots": roots });
        let checkpoint_digest = bytes_digest(&serde_json::to_vec(&checkpoint)?);
        capture_members_tx(tx, &seed.id, "first_import", &roots, rows)?;
        tx.execute(
            "INSERT INTO episode_continuity_imports (seed_id, checkpoint_digest, roots_json, captured_at) VALUES (?1, ?2, ?3, ?4)",
            params![seed.id, checkpoint_digest, serde_json::to_string(&roots)?, captured_at.to_rfc3339()],
        )?;
        super::input_evidence_snapshot::capture_input_evidence_seed_tx(
            tx,
            &seed.id,
            "first_import",
            false,
        )?;
        super::work_snapshot::capture_work_domain_seed_if_ready_tx(tx, &seed.id, "first_import")?;
        super::task_snapshot::capture_task_domain_seed_if_ready_tx(tx, &seed.id, "first_import")?;
        super::scheduling_snapshot::capture_scheduling_seed_if_ready_tx(
            tx,
            &seed.id,
            "first_import",
        )?;
    }
    Ok(())
}

/// Existing public/fixture/legacy upsert cannot overwrite a qualified Episode.
/// Unqualified writes acquire no continuity membership by themselves.
pub(super) fn validate_immutable_episode_upsert_tx(
    tx: &Transaction<'_>,
    record: &ContextEpisodeRecord,
) -> Result<bool> {
    let qualified: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM episode_memory_results WHERE finalized_episode_id = ?1)
          OR EXISTS (SELECT 1 FROM episode_continuity_members WHERE episode_id = ?1)",
        [&record.id],
        |row| row.get(0),
    )?;
    if qualified {
        let existing: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM context_episode_anchors WHERE episode_id = ?1",
                [&record.id],
                |row| row.get(0),
            )
            .optional()?;
        let existing = existing.context("qualified Episode payload is missing")?;
        ensure!(
            decode_context_episode_payload(&existing)? == *record,
            "qualified Episode is immutable: {}",
            record.id
        );
        // Preserve the exact old bytes, including legacy field order/defaults.
        return Ok(true);
    }
    Ok(false)
}

fn episode_import_complete(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM storage_domains WHERE domain = 'context_episode_anchors' AND import_status = 'complete' AND canonical_source = 'db')",
        [], |row| row.get(0),
    )?)
}

fn find_seed(connection: &Connection, agent_id: &str) -> Result<Option<ContinuitySeed>> {
    connection.query_row(
        "SELECT seed_id, identity_incarnation, import_required FROM episode_continuity_seeds WHERE agent_id = ?1 AND policy_version = ?2",
        params![agent_id, EPISODE_CONTINUITY_POLICY],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, bool>(2)?)),
    ).optional()?.map(|(id, incarnation, import_required)| Ok(ContinuitySeed {
        id, agent_id: agent_id.to_owned(), incarnation: incarnation.map(u64::try_from).transpose()?, import_required,
    })).transpose()
}

fn has_any_binding(connection: &Connection, agent_id: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM agent_episode_continuity WHERE agent_id = ?1)",
        [agent_id],
        |row| row.get(0),
    )?)
}

fn ensure_no_prior_binding(connection: &Connection, agent_id: &str) -> Result<()> {
    if has_any_binding(connection, agent_id)? {
        return Err(EpisodeSnapshotError::BindingConflict.into());
    }
    Ok(())
}

fn insert_seed_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    identity: Option<&AgentIdentityRecord>,
    source_kind: &str,
    roots: &EpisodeContinuityRoots,
    import_required: bool,
    captured_at: DateTime<Utc>,
) -> Result<ContinuitySeed> {
    let records = all_episode_rows(tx, agent_id)?;
    insert_seed_with_candidates_tx(
        tx,
        agent_id,
        identity,
        source_kind,
        roots,
        import_required,
        captured_at,
        &records,
    )
}

fn insert_seed_with_candidates_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    identity: Option<&AgentIdentityRecord>,
    source_kind: &str,
    roots: &EpisodeContinuityRoots,
    import_required: bool,
    captured_at: DateTime<Utc>,
    records: &[EpisodeRow],
) -> Result<ContinuitySeed> {
    let boundary: String = tx.query_row(
        "SELECT value FROM runtime_metadata WHERE key = ?1",
        [UPGRADE_BOUNDARY_KEY],
        |row| row.get(0),
    )?;
    let seed_id = format!(
        "episode_seed_{}",
        bytes_digest(&serde_json::to_vec(&(
            boundary.as_str(),
            agent_id,
            EPISODE_CONTINUITY_POLICY
        ))?)
    );
    let incarnation = identity.map(|identity| identity.incarnation);
    let candidates = records
        .iter()
        .filter(|row| !row.receipted)
        .map(|row| (row.record.id.clone(), bytes_digest(row.payload.as_bytes())))
        .collect::<BTreeMap<_, _>>();
    tx.execute(
        "INSERT INTO episode_continuity_seeds (seed_id, agent_id, policy_version, boundary_id, identity_incarnation, source_kind,
          identity_digest, roots_json, candidate_payloads_json, import_required, captured_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![seed_id, agent_id, EPISODE_CONTINUITY_POLICY, boundary, incarnation.map(i64::try_from).transpose()?, source_kind,
            identity.map(serde_json::to_vec).transpose()?.map(|bytes| bytes_digest(&bytes)), serde_json::to_string(roots)?,
            serde_json::to_string(&candidates)?, import_required, captured_at.to_rfc3339()],
    )?;
    if matches!(
        source_kind,
        "legacy_identity_import" | "legacy_state_import"
    ) {
        super::input_evidence_snapshot::capture_input_evidence_seed_tx(
            tx,
            &seed_id,
            "legacy_import",
            true,
        )?;
        super::work_snapshot::capture_work_domain_seed_if_ready_tx(tx, &seed_id, "legacy_import")?;
        super::task_snapshot::capture_task_domain_seed_if_ready_tx(tx, &seed_id, "legacy_import")?;
        super::scheduling_snapshot::capture_scheduling_seed_if_ready_tx(
            tx,
            &seed_id,
            "legacy_import",
        )?;
    }
    Ok(ContinuitySeed {
        id: seed_id,
        agent_id: agent_id.to_owned(),
        incarnation,
        import_required,
    })
}

fn insert_legacy_binding_tx(
    tx: &Transaction<'_>,
    identity: &AgentIdentityRecord,
    seed: &ContinuitySeed,
    bound_at: DateTime<Utc>,
) -> Result<()> {
    ensure!(
        matches!(
            identity.status,
            AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
        ) && identity.incarnation > 0
            && identity.agent_id == seed.agent_id
            && seed
                .incarnation
                .is_none_or(|incarnation| incarnation == identity.incarnation),
        "legacy Episode binding scope mismatch"
    );
    tx.execute(
        "INSERT INTO agent_episode_continuity (agent_id, identity_incarnation, policy_version, mode, seed_id, bound_at)
         VALUES (?1, ?2, ?3, 'legacy_adopted', ?4, ?5)",
        params![identity.agent_id, i64::try_from(identity.incarnation)?, EPISODE_CONTINUITY_POLICY, seed.id, bound_at.to_rfc3339()],
    )?;
    Ok(())
}

struct EpisodeRow {
    record: ContextEpisodeRecord,
    payload: String,
    receipted: bool,
}

fn episode_row(connection: &Connection, episode_id: &str) -> Result<Option<EpisodeRow>> {
    connection.query_row(
        "SELECT e.payload_json, EXISTS (SELECT 1 FROM episode_memory_results r WHERE r.finalized_episode_id = e.episode_id)
         FROM context_episode_anchors e WHERE e.episode_id = ?1", [episode_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
    ).optional()?.map(|(payload, receipted)| Ok(EpisodeRow { record: decode_context_episode_payload(&payload)?, payload, receipted })).transpose()
}

fn all_episode_rows(connection: &Connection, agent_id: &str) -> Result<Vec<EpisodeRow>> {
    let mut statement = connection.prepare(
        "SELECT e.payload_json, EXISTS (SELECT 1 FROM episode_memory_results r WHERE r.finalized_episode_id = e.episode_id)
         FROM context_episode_anchors e WHERE e.agent_id = ?1 ORDER BY e.ended_at DESC, e.started_at DESC, e.episode_id ASC",
    )?;
    let rows = statement.query_map([agent_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
    })?;
    let mut records = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        let (payload, receipted) = row?;
        bytes = bytes.saturating_add(payload.len());
        if records.len() >= CONTINUITY_MAX_RECORDS || bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
            return Err(EpisodeSnapshotError::ClosureResourceExhausted {
                limit: if bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
                    "payload bytes"
                } else {
                    "record count"
                },
            }
            .into());
        }
        records.push(EpisodeRow {
            record: decode_context_episode_payload(&payload)?,
            payload,
            receipted,
        });
    }
    Ok(records)
}

fn capture_members_tx(
    tx: &Transaction<'_>,
    seed_id: &str,
    phase: &str,
    roots: &EpisodeContinuityRoots,
    mut records: Vec<EpisodeRow>,
) -> Result<()> {
    records.sort_by(|left, right| {
        right
            .record
            .finalized_at
            .cmp(&left.record.finalized_at)
            .then_with(|| right.record.created_at.cmp(&left.record.created_at))
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
    let recent_ids = records
        .iter()
        .filter(|row| !row.receipted)
        .take(LEGACY_EPISODE_RECENT_CANDIDATES)
        .map(|row| row.record.id.clone())
        .collect::<BTreeSet<_>>();
    for row in records.into_iter().filter(|row| !row.receipted) {
        let mut reasons = BTreeSet::new();
        if row
            .record
            .current_work_item_id
            .as_ref()
            .is_some_and(|id| roots.work_item_ids.contains(id))
        {
            reasons.insert("live_owner");
        }
        if roots
            .source_refs
            .contains(&format!("episode:{}", row.record.id))
        {
            reasons.insert("explicit_live_ref");
        }
        if recent_ids.contains(&row.record.id) {
            reasons.insert("explicit_upgrade_recent_candidate");
        }
        if reasons.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT INTO episode_continuity_members (seed_id, capture_phase, episode_id, content_digest, qualification_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![seed_id, phase, row.record.id, bytes_digest(row.payload.as_bytes()), serde_json::to_string(&reasons)?],
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct RootCollector {
    roots: EpisodeContinuityRoots,
    pending: VecDeque<(String, String, usize)>,
    queued: BTreeSet<(String, String)>,
    visited: BTreeSet<(String, String)>,
    current_depth: usize,
    payload_bytes: usize,
    budget_error: Option<&'static str>,
}

impl RootCollector {
    fn reference(&mut self, kind: &str, id: &str) {
        if id.is_empty() || self.budget_error.is_some() {
            return;
        }
        let key = (kind.to_owned(), id.to_owned());
        if self.queued.contains(&key) {
            return;
        }
        if self.current_depth > CONTINUITY_MAX_REF_DEPTH {
            self.budget_error = Some("reference depth");
        } else if self.queued.len() >= CONTINUITY_MAX_RECORDS {
            self.budget_error = Some("record count");
        } else {
            self.queued.insert(key.clone());
            self.pending.push_back((key.0, key.1, self.current_depth));
        }
    }

    fn work_item(&mut self, id: Option<&str>) {
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            self.roots.work_item_ids.insert(id.to_owned());
            self.reference("work_item", id);
        }
    }

    fn optional_ref(&mut self, kind: &str, id: Option<&str>) {
        if let Some(id) = id {
            self.reference(kind, id);
        }
    }

    fn source_ref(&mut self, value: &str) {
        let Ok(reference) = RuntimeRef::parse(value) else {
            return;
        };
        self.roots.source_refs.insert(reference.source_ref());
        match reference {
            RuntimeRef::Episode { id } => self.reference("episode", &id),
            RuntimeRef::Message { id } => self.reference("message", &id),
            RuntimeRef::Turn { id } => self.reference("turn", &id),
            RuntimeRef::WorkItem { id } => self.work_item(Some(&id)),
            RuntimeRef::Task { id } => self.reference("task", &id),
            RuntimeRef::Brief { id } => self.reference("brief", &id),
            RuntimeRef::ToolExecution { id, .. } => self.reference("tool_execution", &id),
            RuntimeRef::AgentMemory { .. } | RuntimeRef::WorkspaceProfile { .. } => {}
        }
    }

    fn ensure_budget(&self) -> Result<()> {
        if let Some(limit) = self.budget_error {
            return Err(EpisodeSnapshotError::ClosureResourceExhausted { limit }.into());
        }
        Ok(())
    }

    fn digest(&mut self, kind: &str, id: &str, payload: &str) -> Result<()> {
        self.ensure_budget()?;
        let key = format!("{kind}:{id}");
        if self.roots.record_digests.contains_key(&key) {
            return Ok(());
        }
        self.payload_bytes = self.payload_bytes.saturating_add(payload.len());
        if self.roots.record_digests.len() >= CONTINUITY_MAX_RECORDS
            || self.payload_bytes > CONTINUITY_MAX_PAYLOAD_BYTES
        {
            return Err(EpisodeSnapshotError::ClosureResourceExhausted {
                limit: if self.payload_bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
                    "payload bytes"
                } else {
                    "record count"
                },
            }
            .into());
        }
        self.roots
            .record_digests
            .insert(key, bytes_digest(payload.as_bytes()));
        Ok(())
    }

    fn work_record(&mut self, record: &WorkItemRecord) {
        self.optional_ref("turn", record.turn_id.as_deref());
        self.optional_ref("wait", record.blocked_by_wait_id.as_deref());
        self.optional_ref("brief", record.result_brief_id.as_deref());
        for reference in record
            .work_refs
            .iter()
            .filter(|reference| reference.status == WorkItemRefStatus::Active)
        {
            if let Some(source_ref) = reference.source_ref.as_deref() {
                self.source_ref(source_ref);
            }
            self.source_ref(&reference.ref_id);
        }
        if let Some(intent) = record.completion_intent.as_ref() {
            self.optional_ref("turn", intent.source_turn_id.as_deref());
            self.optional_ref("message", intent.source_message_id.as_deref());
            self.optional_ref("brief", intent.result_brief_id.as_deref());
        }
    }

    fn task_record(&mut self, record: &TaskRecord) {
        self.work_item(record.effective_work_item_id());
        self.optional_ref("message", record.parent_message_id.as_deref());
        if let Some(detail) = record.detail.as_ref() {
            self.optional_ref(
                "turn",
                detail
                    .get("parent_turn_id")
                    .and_then(serde_json::Value::as_str),
            );
        }
    }

    fn wait_record(&mut self, record: &WaitConditionRecord) {
        self.work_item(record.work_item_id.as_deref());
        self.optional_ref("turn", record.turn_id.as_deref());
        self.optional_ref("message", record.trigger_message_id.as_deref());
        if let Some(source_ref) = record.subject_ref.as_deref() {
            self.source_ref(source_ref);
        }
        for source in &record.wake_sources {
            if let crate::types::WakeSource::TaskResult { task_id } = source {
                self.reference("task", task_id);
            }
        }
    }

    fn attempt_record(&mut self, record: &ExecutionAttempt) {
        self.optional_ref("attempt", record.recovery_of_attempt_id.as_deref());
        if let ExecutionBinding::WorkItem { work_item_id } = &record.binding {
            self.work_item(Some(work_item_id));
        }
        self.optional_ref("message", record.source_message_id.as_deref());
        self.optional_ref("turn", record.turn_id.as_deref());
        if let Some(fence) = record.admitted_fences.rejoin.as_ref() {
            self.reference("task", &fence.obligation_id);
            self.reference("turn", &fence.parent_turn_id);
        }
        match &record.source.identity {
            ExecutionSourceIdentity::QueueMessage { message_id }
            | ExecutionSourceIdentity::InternalFollowup { message_id } => {
                self.reference("message", message_id)
            }
            ExecutionSourceIdentity::TaskResult {
                task_id,
                result_message_id,
            }
            | ExecutionSourceIdentity::ChildResult {
                task_id,
                result_message_id,
                ..
            } => {
                self.reference("task", task_id);
                self.reference("message", result_message_id);
            }
            ExecutionSourceIdentity::TriggeredWait {
                wait_id,
                trigger_message_id,
                ..
            } => {
                self.reference("wait", wait_id);
                self.reference("message", trigger_message_id);
            }
            ExecutionSourceIdentity::WorkItemContinuation { work_item_id } => {
                self.work_item(Some(work_item_id))
            }
            ExecutionSourceIdentity::TargetedContinuation {
                source_work_item_id,
                target_work_item_id,
                ..
            } => {
                self.work_item(Some(source_work_item_id));
                self.work_item(Some(target_work_item_id));
            }
            ExecutionSourceIdentity::RuntimeRecovery { .. } => {}
        }
    }
}

/// Capture all canonical live owner/obligation roots before bounded payload reads.
/// The one-time closure follows by-ID references, not arbitrary message body text.
fn collect_live_episode_roots(
    connection: &Connection,
    agent_id: &str,
) -> Result<EpisodeContinuityRoots> {
    collect_live_episode_roots_with_schema(connection, agent_id, None)
}

// Only the fixed upgrade actor supplies a table inventory. Normal import and
// current readers remain strict; malformed existing rows are never skipped.
fn collect_live_episode_roots_with_schema(
    connection: &Connection,
    agent_id: &str,
    tables: Option<&BTreeSet<String>>,
) -> Result<EpisodeContinuityRoots> {
    collect_live_episode_roots_with_work_policy(connection, agent_id, tables, true)
}

fn collect_live_episode_roots_with_work_policy(
    connection: &Connection,
    agent_id: &str,
    tables: Option<&BTreeSet<String>>,
    include_nonopen: bool,
) -> Result<EpisodeContinuityRoots> {
    collect_live_episode_roots_with_work_authority(
        connection,
        agent_id,
        tables,
        include_nonopen,
        None,
        None,
    )
}

fn collect_live_episode_roots_with_work_authority(
    connection: &Connection,
    agent_id: &str,
    tables: Option<&BTreeSet<String>>,
    include_nonopen: bool,
    work_import: Option<&work_checkpoint::imports::ImportAuthority>,
    task_original: Option<&task_checkpoint::TaskContinuityCheckpoint>,
) -> Result<EpisodeContinuityRoots> {
    let mut collector = RootCollector::default();
    let state = if root_table_available(tables, "agent_states") {
        load_state(connection, agent_id)?.0
    } else {
        None
    };
    if let Some(state) = state {
        let payload: String = connection.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?;
        collector.digest("state", agent_id, &payload)?;
        collector.work_item(state.current_work_item_id.as_deref());
        collector.work_item(state.current_turn_work_item_id.as_deref());
        collector.work_item(
            state
                .working_memory
                .current_working_memory
                .current_work_item_id
                .as_deref(),
        );
        collector.optional_ref("turn", state.current_turn_id.as_deref());
        if let Some(binding) = state.current_execution_binding.as_ref() {
            collector.work_item(binding.work_item_id.as_deref());
            collector.work_item(
                binding
                    .owner
                    .as_ref()
                    .and_then(crate::types::TurnOwner::work_item_id),
            );
            collector.reference("message", &binding.source_message_id);
            collector.reference("turn", &binding.turn_id);
        }
        if let Some(builder) = state.working_memory.active_episode_builder.as_ref() {
            collector.work_item(builder.current_work_item_id.as_deref());
            for id in &builder.source_turn_ids {
                collector.reference("turn", id);
            }
        }
    }
    for payload in root_payloads(connection, tables, "work_items", "SELECT payload_json FROM work_items WHERE agent_id = ?1 AND (state IN ('open', 'completing') OR EXISTS (SELECT 1 FROM json_each(work_items.payload_json, '$.work_refs') r WHERE json_extract(r.value, '$.status') = 'active'))", agent_id)? {
        let record: WorkItemRecord = serde_json::from_str(&payload)?;
        ensure!(record.agent_id == agent_id, "WorkItem scope projection mismatch");
        collector.digest("work_item", &record.id, &payload)?;
        if matches!(record.state, WorkItemState::Open | WorkItemState::Completing) { collector.work_item(Some(&record.id)); }
        // Even a terminal owner's explicitly Active references remain roots.
        collector.work_record(&record);
    }
    for payload in root_payloads(
        connection,
        tables,
        "work_item_continuations",
        "SELECT payload_json FROM work_item_continuations WHERE agent_id = ?1 AND state = 'active'",
        agent_id,
    )? {
        let record: WorkItemContinuationFrame = serde_json::from_str(&payload)?;
        ensure!(
            record.agent_id == agent_id,
            "continuation scope projection mismatch"
        );
        collector.digest("continuation", &record.id, &payload)?;
        collector.work_item(Some(&record.suspended_work_item_id));
        collector.work_item(Some(&record.active_work_item_id));
        collector.optional_ref("turn", record.turn_id.as_deref());
    }
    for payload in root_payloads(connection, tables, "wait_conditions", "SELECT payload_json FROM wait_conditions WHERE agent_id = ?1 AND status IN ('active', 'triggered')", agent_id)? {
        let record: WaitConditionRecord = serde_json::from_str(&payload)?;
        ensure!(record.agent_id == agent_id, "wait scope projection mismatch");
        collector.digest("wait", &record.id, &payload)?;
        collector.wait_record(&record);
    }
    for payload in root_payloads(connection, tables, "tasks", "SELECT payload_json FROM tasks WHERE owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')", agent_id)? {
        let record: TaskRecord = serde_json::from_str(&payload)?;
        ensure!(record.agent_id == agent_id, "task scope projection mismatch");
        collector.digest("task", &record.id, &payload)?;
        collector.task_record(&record);
    }
    // No terminal_reentry shortcut: queue/settlement/attempt references prove the
    // terminal task's remaining obligation, regardless of that historical flag.
    for payload in root_payloads(connection, tables, "task_result_settlements", "SELECT payload_json FROM task_result_settlements WHERE agent_id = ?1 AND state IN ('persisted_pending', 'caller_admitted')", agent_id)? {
        let record: super::task_result_settlement::TaskResultSettlementRecord = serde_json::from_str(&payload)?;
        ensure!(record.agent_id == agent_id, "settlement scope projection mismatch");
        collector.digest("settlement", &record.result_identity, &payload)?;
        collector.work_item(record.work_item_id.as_deref());
        collector.reference("task", &record.task_id);
        collector.reference("message", &record.message_id);
        collector.reference("turn", &record.parent_turn_id);
    }
    for payload in root_payloads(connection, tables, "execution_protocol_attempts", "SELECT payload_json FROM execution_protocol_attempts WHERE agent_id = ?1 AND lifecycle_state != 'settled'", agent_id)? {
        let record: ExecutionAttempt = serde_json::from_str(&payload)?;
        ensure!(record.agent_id == agent_id, "execution attempt scope projection mismatch");
        collector.digest("attempt", &record.attempt_id, &payload)?;
        collector.attempt_record(&record);
    }
    if root_table_available(tables, "execution_protocol_work_items") {
        let mut statement = connection.prepare("SELECT work_item_id, payload_json FROM execution_protocol_work_items WHERE agent_id = ?1 AND lifecycle_state != 'terminal'")?;
        let records = statement.query_map([agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in records {
            let (id, payload) = row?;
            let record: crate::domain::execution_protocol::WorkItemExecutionRecord =
                serde_json::from_str(&payload)?;
            collector.digest("work_execution", &id, &payload)?;
            collector.work_item(Some(&id));
            use crate::domain::execution_protocol::WorkItemExecutionState;
            match record.state {
                WorkItemExecutionState::Runnable { recovery_ref, .. } => {
                    if let Some(source_ref) = recovery_ref {
                        collector.source_ref(&source_ref);
                    }
                }
                WorkItemExecutionState::InFlight { attempt_id, .. } => {
                    collector.reference("attempt", &attempt_id)
                }
                WorkItemExecutionState::Waiting { wait, .. } => {
                    collector.reference("wait", &wait.wait_id)
                }
                // Repair identifiers can be diagnostic keys, not RuntimeRefs.
                WorkItemExecutionState::NeedsRepair { repair_id, .. } => {
                    collector.source_ref(&repair_id)
                }
                WorkItemExecutionState::Paused { .. } | WorkItemExecutionState::Terminal { .. } => {
                }
            }
        }
    }
    if root_table_available(tables, "queue_entries") {
        let mut statement = connection.prepare("SELECT message_id, payload_json FROM queue_entries WHERE agent_id = ?1 AND status IN ('queued', 'interrupted', 'dequeued', 'interjected')")?;
        for row in statement.query_map([agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, payload) = row?;
            collector.digest("queue", &id, &payload)?;
            collector.reference("message", &id);
        }
    }
    for payload in root_payloads(connection, tables, "turn_records", "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND json_extract(payload_json, '$.terminal') IS NULL", agent_id)? {
        let record: TurnRecord = serde_json::from_str(&payload)?;
        collector.reference("turn", &record.turn_id);
    }
    expand_episode_root_refs(connection, agent_id, &mut collector, tables)?;
    let (work, input_digests) = work_checkpoint::capture_with_import(
        connection,
        agent_id,
        tables,
        &collector.roots,
        collector.payload_bytes,
        include_nonopen,
        work_import,
    )?;
    // D4 owning refs qualify D3 bodies, but must not widen D1 Episode ownership
    // or explicit-live-ref membership through Brief/Turn owner back-edges.
    collector.roots.record_digests = input_digests;
    collector.roots.work_domain_checkpoint = Some(work);
    collector.roots.task_domain_checkpoint = Some(task_checkpoint::capture(
        connection,
        agent_id,
        tables,
        &collector.roots,
        include_nonopen,
        task_original,
        work_import,
    )?);
    if include_nonopen {
        collector.roots.scheduling_domain_checkpoint =
            Some(super::scheduling_snapshot::capture_fixed_checkpoint_on(
                connection, agent_id, tables, None,
            )?);
    }
    Ok(collector.roots)
}

fn root_table_available(tables: Option<&BTreeSet<String>>, table: &str) -> bool {
    tables.is_none_or(|tables| tables.contains(table))
}

fn root_payloads(
    connection: &Connection,
    tables: Option<&BTreeSet<String>>,
    table: &str,
    sql: &str,
    agent_id: &str,
) -> Result<Vec<String>> {
    if !root_table_available(tables, table) {
        return Ok(Vec::new());
    }
    payloads(connection, sql, agent_id)
}

fn payloads(connection: &Connection, sql: &str, agent_id: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([agent_id], |row| row.get::<_, String>(0))?;
    let mut payloads = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        let payload = row?;
        bytes = bytes.saturating_add(payload.len());
        if payloads.len() >= CONTINUITY_MAX_RECORDS || bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
            return Err(EpisodeSnapshotError::ClosureResourceExhausted {
                limit: if bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
                    "payload bytes"
                } else {
                    "record count"
                },
            }
            .into());
        }
        payloads.push(payload);
    }
    Ok(payloads)
}

fn expand_episode_root_refs(
    connection: &Connection,
    agent_id: &str,
    collector: &mut RootCollector,
    tables: Option<&BTreeSet<String>>,
) -> Result<()> {
    collector.ensure_budget()?;
    while let Some((kind, id, depth)) = collector.pending.pop_front() {
        collector.ensure_budget()?;
        if !collector.visited.insert((kind.clone(), id.clone())) {
            continue;
        }
        collector.current_depth = depth.saturating_add(1);
        let (table, sql) = match kind.as_str() {
            "work_item" => ("work_items", "SELECT payload_json FROM work_items WHERE agent_id = ?1 AND work_item_id = ?2"),
            "task" => ("tasks", "SELECT payload_json FROM tasks WHERE owner_agent_id = ?1 AND task_id = ?2"),
            "wait" => ("wait_conditions", "SELECT payload_json FROM wait_conditions WHERE agent_id = ?1 AND wait_condition_id = ?2"),
            "message" => ("messages", "SELECT payload_json FROM messages WHERE agent_id = ?1 AND evidence_id = ?2"),
            "turn" => ("turn_records", "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2"),
            "brief" => ("briefs", "SELECT payload_json FROM briefs WHERE agent_id = ?1 AND evidence_id = ?2"),
            "tool_execution" => ("tool_executions", "SELECT payload_json FROM tool_executions WHERE agent_id = ?1 AND evidence_id = ?2"),
            "episode" => ("context_episode_anchors", "SELECT payload_json FROM context_episode_anchors WHERE agent_id = ?1 AND episode_id = ?2"),
            "attempt" => ("execution_protocol_attempts", "SELECT payload_json FROM execution_protocol_attempts WHERE agent_id = ?1 AND attempt_id = ?2"),
            _ => bail!("unsupported canonical continuity reference: {kind}"),
        };
        if !root_table_available(tables, table) {
            collector
                .roots
                .unavailable_refs
                .insert(format!("{kind}:{id}"));
            continue;
        }
        let payload: Option<String> = connection
            .query_row(sql, params![agent_id, id], |row| row.get(0))
            .optional()?;
        let Some(payload) = payload else {
            collector
                .roots
                .unavailable_refs
                .insert(format!("{kind}:{id}"));
            continue;
        };
        collector.digest(&kind, &id, &payload)?;
        match kind.as_str() {
            "work_item" => {
                let record: WorkItemRecord = serde_json::from_str(&payload)?;
                ensure!(
                    record.agent_id == agent_id && record.id == id,
                    "WorkItem continuity projection mismatch"
                );
                collector.work_record(&record);
            }
            "task" => {
                let record: TaskRecord = serde_json::from_str(&payload)?;
                ensure!(
                    record.agent_id == agent_id && record.id == id,
                    "task continuity projection mismatch"
                );
                collector.task_record(&record);
            }
            "wait" => {
                let record: WaitConditionRecord = serde_json::from_str(&payload)?;
                ensure!(
                    record.agent_id == agent_id && record.id == id,
                    "wait continuity projection mismatch"
                );
                collector.wait_record(&record);
            }
            "attempt" => {
                let record: ExecutionAttempt = serde_json::from_str(&payload)?;
                ensure!(
                    record.agent_id == agent_id && record.attempt_id == id,
                    "attempt continuity projection mismatch"
                );
                collector.attempt_record(&record);
            }
            "message" => {
                let message: MessageEnvelope = serde_json::from_str(&payload)?;
                ensure!(
                    message.agent_id == agent_id && message.id == id,
                    "message continuity projection mismatch"
                );
                collector.work_item(message.work_item_id.as_deref());
                // A preallocated/replay hint grants no old Turn. Historical
                // schemas without assignments have no proven owning edge.
                if root_table_available(tables, "conversation_input_assignments") {
                    let assigned: Option<(String, String)> = connection
                        .query_row(
                            "SELECT agent_id,turn_id FROM conversation_input_assignments WHERE message_id=?1",
                            [&message.id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?;
                    if let Some((assigned_agent, turn_id)) = assigned {
                        ensure!(
                            assigned_agent == agent_id,
                            "message continuity assignment scope mismatch"
                        );
                        collector.reference("turn", &turn_id);
                    }
                }
                collector.optional_ref("task", message.task_id.as_deref());
                for source_ref in message.source_refs.values() {
                    collector.source_ref(source_ref);
                }
                // A typed Message may carry invalid recovery authority. Its
                // scheduler rejection must remain reachable after cutover.
                if let Ok(Some((source_turn_id, source_message_id, terminal_kind))) =
                    crate::runtime::provider_recovery::continuity_sources(&message)
                {
                    collect_provider_recovery_refs(
                        connection,
                        &message,
                        &source_turn_id,
                        &source_message_id,
                        terminal_kind,
                        collector,
                    )?;
                }
            }
            "turn" => {
                let turn: TurnRecord = serde_json::from_str(&payload)?;
                ensure!(
                    turn.agent_id == agent_id && turn.turn_id == id,
                    "turn continuity projection mismatch"
                );
                collector.work_item(turn.current_work_item_id.as_deref());
                collector.work_item(
                    turn.owner
                        .as_ref()
                        .and_then(crate::types::TurnOwner::work_item_id),
                );
                for id in turn.input_message_ids {
                    collector.reference("message", &id);
                }
                for id in turn.tool_execution_ids {
                    collector.reference("tool_execution", &id);
                }
                for id in turn.produced_brief_ids {
                    collector.reference("brief", &id);
                }
                for id in turn.completed_work_item_ids {
                    collector.work_item(Some(&id));
                }
                for id in turn.waiting_condition_ids {
                    collector.reference("wait", &id);
                }
            }
            "brief" => {
                let brief: BriefRecord = serde_json::from_str(&payload)?;
                ensure!(
                    brief.agent_id == agent_id && brief.id == id,
                    "brief continuity projection mismatch"
                );
                collector.work_item(brief.work_item_id.as_deref());
                collector.optional_ref("turn", brief.turn_id.as_deref());
                collector.optional_ref("message", brief.related_message_id.as_deref());
                collector.optional_ref("task", brief.related_task_id.as_deref());
            }
            "tool_execution" => {
                let record: ToolExecutionRecord = serde_json::from_str(&payload)?;
                ensure!(
                    record.agent_id == agent_id && record.id == id,
                    "tool continuity projection mismatch"
                );
                collector.work_item(record.work_item_id.as_deref());
                collector.optional_ref("turn", record.turn_id.as_deref());
            }
            "episode" => {
                let record = decode_context_episode_payload(&payload)?;
                ensure!(
                    record.agent_id == agent_id && record.id == id,
                    "Episode continuity projection mismatch"
                );
                for source_ref in record.source_refs {
                    collector.source_ref(&source_ref);
                }
                for id in record.source_turn_ids {
                    collector.reference("turn", &id);
                }
            }
            _ => unreachable!("canonical continuity reference matched above"),
        }
    }
    collector.ensure_budget()
}

// Trusted directive fields are raw IDs, unlike generic RuntimeRef source values.
// Resolve their canonical linkage on this same migration/import read snapshot.
fn collect_provider_recovery_refs(
    connection: &Connection,
    recovery: &MessageEnvelope,
    source_turn_id: &str,
    source_message_id: &str,
    terminal_kind: crate::types::TurnTerminalKind,
    collector: &mut RootCollector,
) -> Result<()> {
    let source_message_payload: String = connection
        .query_row(
            "SELECT payload_json FROM messages WHERE agent_id = ?1 AND evidence_id = ?2",
            params![recovery.agent_id, source_message_id],
            |row| row.get(0),
        )
        .context("provider recovery continuity source message is missing")?;
    let source_message: MessageEnvelope = serde_json::from_str(&source_message_payload)?;
    let source_turn_payload: String = connection
        .query_row(
            "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2",
            params![recovery.agent_id, source_turn_id],
            |row| row.get(0),
        )
        .context("provider recovery continuity source turn is missing")?;
    let source_turn: TurnRecord = serde_json::from_str(&source_turn_payload)?;
    ensure!(
        source_message.agent_id == recovery.agent_id
            && source_message.id == source_message_id
            && source_message.turn_id.as_deref() == Some(source_turn_id)
            && source_turn.agent_id == recovery.agent_id
            && source_turn.turn_id == source_turn_id
            && source_turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                == Some(source_message_id)
            && source_turn.terminal.as_ref().map(|terminal| terminal.kind) == Some(terminal_kind),
        "provider recovery continuity source linkage mismatch"
    );
    let mut statement = connection.prepare(
        "SELECT a.payload_json FROM execution_protocol_attempts a
         WHERE a.agent_id = ?1 AND a.lifecycle_state IN ('settled', 'interrupted')
           AND json_extract(a.payload_json, '$.source_message_id') = ?2
           AND json_extract(a.payload_json, '$.turn_id') = ?3
           AND EXISTS (SELECT 1 FROM execution_protocol_outcomes o
             WHERE o.agent_id = a.agent_id AND o.attempt_id = a.attempt_id
               AND o.outcome_id = a.terminal_outcome_id)",
    )?;
    let rows = statement.query_map(
        params![recovery.agent_id, source_message_id, source_turn_id],
        |row| row.get::<_, String>(0),
    )?;
    let mut predecessors = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        predecessors.len() == 1,
        "provider recovery continuity predecessor is missing or ambiguous"
    );
    let predecessor: ExecutionAttempt = serde_json::from_str(&predecessors.remove(0))?;
    let owner = match &predecessor.binding {
        ExecutionBinding::WorkItem { work_item_id } => crate::types::TurnOwner::WorkItem {
            work_item_id: work_item_id.clone(),
        },
        ExecutionBinding::Conversation { interaction_id } => {
            crate::types::TurnOwner::Conversation {
                interaction_id: interaction_id.clone(),
            }
        }
        ExecutionBinding::AgentLifecycle { agent_id } => crate::types::TurnOwner::AgentLifecycle {
            agent_id: agent_id.clone(),
        },
        ExecutionBinding::Command => crate::types::TurnOwner::Command,
    };
    ensure!(
        predecessor.agent_id == recovery.agent_id
            && predecessor.source_message_id.as_deref() == Some(source_message_id)
            && predecessor.turn_id.as_deref() == Some(source_turn_id)
            && source_turn.owner.as_ref() == Some(&owner)
            && recovery.work_item_id.as_deref() == owner.work_item_id(),
        "provider recovery continuity binding mismatch"
    );
    collector.reference("attempt", &predecessor.attempt_id);
    collector.reference("turn", source_turn_id);
    collector.reference("message", source_message_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::episode_transition::{EpisodeMemoryOutcome, EpisodeMemoryPlan},
        runtime_db::{
            episode_memory::{EpisodeMemoryCommit, EpisodeMemoryTransitionCommand},
            evidence::{upsert_agent_identity_tx, upsert_agent_state_tx, upsert_message_tx},
            migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        },
        types::{
            AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility, EpisodeBoundaryReason,
        },
    };

    struct Fixture {
        _directory: tempfile::TempDir,
        db: RuntimeDb,
    }

    impl Fixture {
        fn legacy(prepare: impl FnOnce(&Transaction<'_>)) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("runtime.sqlite");
            let mut connection = Connection::open(&path).unwrap();
            ensure_migration_table(&connection).unwrap();
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 81)
            {
                apply_migration(&mut connection, migration).unwrap();
            }
            {
                let tx = connection.transaction().unwrap();
                prepare(&tx);
                tx.commit().unwrap();
            }
            drop(connection);
            let db =
                RuntimeDb::open_and_migrate(path, directory.path().join("runtime.lock")).unwrap();
            Self {
                _directory: directory,
                db,
            }
        }

        fn current(
            &self,
            agent_id: &str,
            incarnation: u64,
            limit: usize,
        ) -> EpisodeMemoryCurrentSnapshot {
            self.db
                .load_episode_memory_snapshot(&scope(agent_id, incarnation), limit, None)
                .unwrap()
                .require_current()
                .unwrap()
        }
    }

    fn time() -> DateTime<Utc> {
        "2026-10-12T00:00:00Z".parse().unwrap()
    }
    fn scope(agent_id: &str, incarnation: u64) -> EpisodeMemoryLoadScope {
        EpisodeMemoryLoadScope {
            agent_id: agent_id.into(),
            identity_incarnation: incarnation,
        }
    }
    fn identity(agent_id: &str) -> AgentIdentityRecord {
        AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        )
    }
    fn state(agent_id: &str) -> AgentState {
        let mut state = AgentState::new(agent_id);
        state.turn_index = 1;
        state
    }
    fn episode(agent_id: &str, id: &str, index: u64, owner: Option<&str>) -> ContextEpisodeRecord {
        serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": agent_id, "created_at": time(),
            "finalized_at": time() + chrono::Duration::seconds(i64::try_from(index).unwrap()),
            "start_turn_index": index, "end_turn_index": index, "start_message_count": 0,
            "end_message_count": 1, "boundary_reason": "wait_boundary", "current_work_item_id": owner,
        })).unwrap()
    }
    fn insert_episode(tx: &Transaction<'_>, record: &ContextEpisodeRecord) {
        tx.execute(
            "INSERT INTO context_episode_anchors (episode_id, agent_id, workspace_id, work_item_id, boundary_reason,
               start_turn_index, end_turn_index, started_at, ended_at, payload_json) VALUES (?1, ?2, ?3, ?4, 'wait_boundary', ?5, ?6, ?7, ?8, ?9)",
            params![record.id, record.agent_id, record.workspace_id, record.current_work_item_id,
                i64::try_from(record.start_turn_index).unwrap(), i64::try_from(record.end_turn_index).unwrap(),
                record.created_at.to_rfc3339(), record.finalized_at.to_rfc3339(), serde_json::to_string(record).unwrap()],
        ).unwrap();
    }
    fn import_complete(tx: &Transaction<'_>) {
        tx.execute("INSERT INTO storage_domains (domain, schema_version, import_status, canonical_source, updated_at)
                    VALUES ('context_episode_anchors', 1, 'complete', 'db', ?1)", [time().to_rfc3339()]).unwrap();
    }
    fn insert_work(tx: &Transaction<'_>, work: &WorkItemRecord) {
        tx.execute("INSERT INTO work_items (work_item_id, agent_id, state, objective, revision, created_at, updated_at, payload_json)
           VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5, ?6)",
           params![work.id, work.agent_id, serde_json::to_value(&work.state).unwrap().as_str().unwrap(), work.objective,
               work.created_at.to_rfc3339(), serde_json::to_string(work).unwrap()]).unwrap();
    }

    #[test]
    fn upgrade_freezes_all_live_owners_and_recent_candidates_once() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            let mut work =
                WorkItemRecord::new("agent-a", "later resume B", WorkItemState::Completing);
            work.id = "work-b".into();
            insert_work(tx, &work);
            for index in 1..=18 {
                insert_episode(
                    tx,
                    &episode(
                        "agent-a",
                        &format!("episode-{index}"),
                        index,
                        (index == 1).then_some("work-b"),
                    ),
                );
            }
            import_complete(tx);
        });
        let current = fixture.current("agent-a", 1, 30);
        assert_eq!(current.episodes.len(), LEGACY_EPISODE_RECENT_CANDIDATES + 1);
        assert!(current
            .episodes
            .iter()
            .any(|episode| episode.id == "episode-1"));
        assert!(!current
            .episodes
            .iter()
            .any(|episode| episode.id == "episode-2"));
        fixture
            .db
            .transaction(|tx| {
                insert_episode(tx, &episode("agent-a", "late-unqualified", 100, None));
                capture_episode_continuity_upgrade_tx(tx, time())
            })
            .unwrap();
        assert!(!fixture
            .current("agent-a", 1, 30)
            .episodes
            .iter()
            .any(|episode| episode.id == "late-unqualified"));
    }

    #[test]
    fn fresh_namespace_cannot_consume_an_unbound_seed_later() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            insert_episode(tx, &episode("agent-a", "old-episode", 1, None));
            import_complete(tx);
        });
        fixture
            .db
            .agent_identities()
            .upsert(&identity("agent-a"))
            .unwrap();
        assert_eq!(
            fixture.current("agent-a", 1, 12).continuity,
            EpisodeContinuityBinding::FreshEmpty
        );
        assert!(fixture.current("agent-a", 1, 12).episodes.is_empty());
        fixture
            .db
            .transaction(|tx| {
                tx.execute(
                    "DELETE FROM agent_identities WHERE agent_id = 'agent-a'",
                    [],
                )?;
                let error = bind_unbound_episode_continuity_tx(tx, &identity("agent-a"), time())
                    .unwrap_err();
                assert_eq!(
                    error.downcast_ref::<EpisodeSnapshotError>(),
                    Some(&EpisodeSnapshotError::BindingConflict)
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn explicit_legacy_materialization_binds_only_the_fixed_unbound_seed() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            insert_episode(tx, &episode("agent-a", "old-episode", 1, None));
            import_complete(tx);
        });
        fixture
            .db
            .transaction(|tx| {
                let identity = identity("agent-a");
                assert!(bind_unbound_episode_continuity_tx(tx, &identity, time())?);
                upsert_agent_identity_tx(tx, &identity)
            })
            .unwrap();
        assert!(matches!(
            fixture.current("agent-a", 1, 12).continuity,
            EpisodeContinuityBinding::LegacyAdopted { .. }
        ));
        assert_eq!(
            fixture.current("agent-a", 1, 12).episodes[0].id,
            "old-episode"
        );
    }

    #[test]
    fn active_builder_collision_is_checked_outside_the_window_even_at_zero_limit() {
        let fixture = Fixture::legacy(|tx| {
            let mut state = state("agent-a");
            let builder = crate::types::ActiveEpisodeBuilder::new(&state, &Default::default(), 0);
            state.working_memory.active_episode_id = Some(builder.id.clone());
            state.working_memory.active_episode_builder = Some(builder.clone());
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state).unwrap();
            insert_episode(tx, &episode("agent-a", &builder.id, 1, None));
            import_complete(tx);
        });
        let error = fixture
            .db
            .load_episode_memory_snapshot(&scope("agent-a", 1), 0, None)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<EpisodeSnapshotError>(),
            Some(EpisodeSnapshotError::ActiveBuilderCollision { .. })
        ));
    }

    #[test]
    fn window_uses_qualified_k_plus_one_and_preserves_old_order() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            for index in 1..=4 {
                insert_episode(
                    tx,
                    &episode("agent-a", &format!("episode-{index}"), index, None),
                );
            }
            import_complete(tx);
        });
        let current = fixture.current("agent-a", 1, 2);
        assert!(current.has_more);
        assert_eq!(
            current
                .episodes
                .iter()
                .map(|episode| episode.id.as_str())
                .collect::<Vec<_>>(),
            ["episode-3", "episode-4"]
        );
        assert!(fixture.current("agent-a", 1, 0).has_more);
        assert!(fixture.current("agent-a", 1, 0).episodes.is_empty());
    }

    #[test]
    fn typed_equal_legacy_import_preserves_the_seeded_payload_bytes_and_digest() {
        let original = episode("agent-a", "old-episode", 1, None);
        let mut old_json = serde_json::to_value(&original).unwrap();
        old_json.as_object_mut().unwrap().remove("workspace_id");
        old_json["unknown_legacy_metadata"] = serde_json::json!({ "kept": true });
        let raw = serde_json::to_string_pretty(&old_json).unwrap();
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            insert_episode(tx, &original);
            tx.execute("UPDATE context_episode_anchors SET payload_json = ?1 WHERE episode_id = 'old-episode'", [&raw]).unwrap();
        });
        fixture
            .db
            .context_episodes()
            .import_legacy(vec![original.clone()])
            .unwrap();
        let connection = fixture.db.connection().unwrap();
        let stored: String = connection
            .query_row(
                "SELECT payload_json FROM context_episode_anchors WHERE episode_id = 'old-episode'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, raw);
        let mut statement = connection.prepare("SELECT content_digest FROM episode_continuity_members WHERE episode_id = 'old-episode'").unwrap();
        let digests = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(digests.len(), 2);
        assert!(digests
            .iter()
            .all(|digest| *digest == bytes_digest(raw.as_bytes())));
        assert_eq!(fixture.current("agent-a", 1, 12).episodes[0], original);
        let mut changed = original.clone();
        changed.work_summary = Some("replacement".into());
        assert!(fixture.db.context_episodes().upsert(&changed).is_err());
        assert_eq!(fixture.current("agent-a", 1, 12).episodes[0], original);
    }

    #[test]
    fn first_import_seed_is_explicit_sealed_once_and_never_adopts_fresh() {
        let fixture = Fixture::legacy(|_| {});
        // An ordinary newly created identity keeps its empty marker, even if a
        // caller later imports old records using the same textual agent id.
        fixture
            .db
            .agent_identities()
            .upsert(&identity("fresh-agent"))
            .unwrap();
        fixture
            .db
            .transaction(|tx| {
                let old = state("legacy-agent");
                upsert_agent_state_tx(tx, &old)?;
                prepare_imported_state_continuity_tx(tx, &old, time())?;
                let fresh = state("fresh-agent");
                upsert_agent_state_tx(tx, &fresh)?;
                prepare_imported_state_continuity_tx(tx, &fresh, time())?;
                let legacy_episode = episode("legacy-agent", "legacy-episode", 1, None);
                let fresh_episode = episode("fresh-agent", "foreign-old-episode", 1, None);
                insert_episode(tx, &legacy_episode);
                insert_episode(tx, &fresh_episode);
                seal_imported_episode_continuity_tx(tx, &[legacy_episode, fresh_episode], time())?;
                let identity = identity("legacy-agent");
                assert!(bind_unbound_episode_continuity_tx(tx, &identity, time())?);
                upsert_agent_identity_tx(tx, &identity)
            })
            .unwrap();
        assert_eq!(
            fixture.current("legacy-agent", 1, 12).episodes[0].id,
            "legacy-episode"
        );
        assert!(fixture.current("fresh-agent", 1, 12).episodes.is_empty());
        fixture
            .db
            .transaction(|tx| {
                let late = episode("legacy-agent", "late-episode", 2, None);
                insert_episode(tx, &late);
                seal_imported_episode_continuity_tx(tx, &[late], time())
            })
            .unwrap();
        assert_eq!(fixture.current("legacy-agent", 1, 12).episodes.len(), 1);
    }

    fn receipt_command(db: &RuntimeDb, episode_id: Option<&str>) -> EpisodeMemoryTransitionCommand {
        let mut state = db.agent_states().latest("agent-a").unwrap().unwrap();
        state.turn_index = 1;
        db.agent_states().upsert_for_fixture(&state).unwrap();
        let mut turn = TurnRecord::new("agent-a", "dispatch-1", 1);
        turn.input_message_ids.push("message-1".into());
        db.turn_records().upsert(&turn).unwrap();
        let mut candidate = state.clone();
        candidate.pending = 42;
        let finalized = episode_id.map(|id| {
            let mut record = episode("agent-a", id, 1, None);
            record.source_turn_ids.push(turn.turn_id.clone());
            record.source_refs.push(format!("turn:{}", turn.turn_id));
            record
        });
        let outcome = EpisodeMemoryOutcome {
            episode_memory_changed: finalized.is_some(),
            business_fields_changed: true,
            boundary_reason: finalized
                .as_ref()
                .map(|_| EpisodeBoundaryReason::WaitBoundary),
            finalized_episode_id: finalized.as_ref().map(|episode| episode.id.clone()),
            previous_builder_id: None,
            active_builder_id: None,
            compression_epoch_before: 0,
            compression_epoch_after: 0,
            archived_episode_count_before: 0,
            archived_episode_count_after: 0,
        };
        EpisodeMemoryTransitionCommand::new(
            EpisodeMemoryKey {
                agent_id: "agent-a".into(),
                identity_incarnation: 1,
                actual_dispatch_turn_id: turn.turn_id,
            },
            EpisodeMemoryInvocation {
                message_id: "message-1".into(),
                turn_index: 1,
            },
            state,
            EpisodeMemoryPlan {
                candidate,
                finalized_episode: finalized,
                audit_events: vec![],
                outcome,
            },
            time(),
        )
        .unwrap()
    }

    #[test]
    fn receipt_and_payloads_share_the_same_sqlite_snapshot() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            import_complete(tx);
        });
        let command = receipt_command(&fixture.db, Some("new-episode"));
        let mut reader = fixture.db.connection().unwrap();
        let read = reader
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        assert!(load_identity(&read, "agent-a").unwrap().is_some());
        assert!(matches!(
            fixture.db.commit_episode_memory(&command).unwrap(),
            EpisodeMemoryCommit::Applied { .. }
        ));
        let before = load_episode_memory_snapshot_in(
            &read,
            &scope("agent-a", 1),
            12,
            Some((
                command.key(),
                command.command_digest(),
                command.invocation(),
            )),
        )
        .unwrap()
        .require_current()
        .unwrap();
        assert_eq!(before.state.unwrap().pending, 0);
        assert!(before.episodes.is_empty());
        assert!(before.receipt.is_none());
        read.commit().unwrap();
        let after = fixture
            .db
            .load_episode_memory_snapshot(
                &scope("agent-a", 1),
                12,
                Some((
                    command.key(),
                    command.command_digest(),
                    command.invocation(),
                )),
            )
            .unwrap()
            .require_current()
            .unwrap();
        assert_eq!(after.state.unwrap().pending, 42);
        assert_eq!(after.episodes[0].id, "new-episode");
        assert!(after.receipt.is_some());
    }

    #[test]
    fn retired_scope_returns_original_receipt_without_any_state_or_episode() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            import_complete(tx);
        });
        let command = receipt_command(&fixture.db, None);
        fixture.db.commit_episode_memory(&command).unwrap();
        fixture
            .db
            .transaction(|tx| {
                let mut next = identity("agent-a");
                next.incarnation = 2;
                tx.execute(
                    "UPDATE agent_identities SET payload_json = ?1 WHERE agent_id = 'agent-a'",
                    [serde_json::to_string(&next)?],
                )?;
                ensure_fresh_episode_continuity_tx(tx, &next, time())
            })
            .unwrap();
        match fixture
            .db
            .load_episode_memory_snapshot(
                &scope("agent-a", 1),
                12,
                Some((
                    command.key(),
                    command.command_digest(),
                    command.invocation(),
                )),
            )
            .unwrap()
        {
            EpisodeMemoryBootstrapSnapshot::Retired {
                current_identity,
                receipt,
                ..
            } => {
                assert_eq!(current_identity.unwrap().incarnation, 2);
                assert_eq!(receipt.unwrap().key, *command.key());
            }
            _ => panic!("retired scope must not return any State or Episode payload"),
        }
        assert!(fixture.current("agent-a", 2, 12).episodes.is_empty());
    }

    #[test]
    fn pending_queue_source_refs_close_over_a_terminal_owner_without_terminal_reentry() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            let mut work = WorkItemRecord::new(
                "agent-a",
                "terminal but result is pending",
                WorkItemState::Completed,
            );
            work.id = "terminal-owner".into();
            insert_work(tx, &work);
            let mut message = MessageEnvelope::new(
                "agent-a",
                crate::types::MessageKind::InternalFollowup,
                crate::types::MessageOrigin::System {
                    subsystem: "fixture".into(),
                },
                crate::types::AuthorityClass::RuntimeInstruction,
                crate::types::Priority::Normal,
                crate::types::MessageBody::Text {
                    text: "pending".into(),
                },
            );
            message.id = "pending-message".into();
            message.work_item_id = Some(work.id.clone());
            message
                .source_refs
                .insert("old_context".into(), "episode:explicit-episode".into());
            upsert_message_tx(tx, &message).unwrap();
            tx.execute("INSERT INTO queue_entries (message_id, agent_id, priority, status, created_at, updated_at, payload_json)
               VALUES ('pending-message', 'agent-a', 'normal', 'interjected', ?1, ?1, '{}')", [time().to_rfc3339()]).unwrap();
            insert_episode(tx, &episode("agent-a", "owner-episode", 1, Some(&work.id)));
            insert_episode(tx, &episode("agent-a", "explicit-episode", 2, None));
            for index in 3..=20 {
                insert_episode(
                    tx,
                    &episode("agent-a", &format!("recent-{index}"), index, None),
                );
            }
            import_complete(tx);
        });
        let current = fixture.current("agent-a", 1, 30);
        assert!(current
            .episodes
            .iter()
            .any(|episode| episode.id == "owner-episode"));
        assert!(current
            .episodes
            .iter()
            .any(|episode| episode.id == "explicit-episode"));
    }
    fn insert_attempt(tx: &Transaction<'_>, attempt: &ExecutionAttempt) {
        tx.execute("INSERT INTO execution_protocol_attempts (agent_id, attempt_id, lifecycle_state,
           source_identity_json, source_generation, recovery_of_attempt_id, terminal_outcome_id, payload_json)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
           params![attempt.agent_id, attempt.attempt_id, serde_json::to_value(attempt.state).unwrap().as_str().unwrap(),
             serde_json::to_string(&attempt.source.identity).unwrap(), i64::try_from(attempt.source.generation).unwrap(), attempt.recovery_of_attempt_id,
             attempt.terminal_outcome_id, serde_json::to_string(attempt).unwrap()]).unwrap();
    }

    fn recovery_fixture(queued_only: bool) -> Fixture {
        Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            let mut root = MessageEnvelope::new(
                "agent-a",
                crate::types::MessageKind::InternalFollowup,
                crate::types::MessageOrigin::System {
                    subsystem: "fixture".into(),
                },
                crate::types::AuthorityClass::RuntimeInstruction,
                crate::types::Priority::Normal,
                crate::types::MessageBody::Text {
                    text: "original command".into(),
                },
            );
            root.id = "root-message".into();
            root.turn_id = Some("root-turn".into());
            root.source_refs
                .insert("context".into(), "episode:recovery-root-episode".into());
            let root = crate::runtime_db::evidence::append_message_tx(tx, &root)
                .unwrap()
                .0;
            let mut turn = TurnRecord::new("agent-a", "root-turn", 1);
            turn.owner = Some(crate::types::TurnOwner::Command);
            turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&root));
            turn.input_message_ids.push(root.id.clone());
            turn.terminal = Some(crate::types::TurnTerminalSummary {
                kind: crate::types::TurnTerminalKind::DeferredToFallback,
                reason: None,
                no_brief_reason: None,
                completed_at: time(),
                duration_ms: 0,
            });
            crate::runtime_db::repositories::upsert_turn_record_tx(tx, &turn).unwrap();
            let predecessor: ExecutionAttempt = serde_json::from_value(serde_json::json!({
                "attempt_id": "settled-predecessor", "agent_id": "agent-a", "source_message_id": root.id,
                "source": { "identity": { "kind": "queue_message", "message_id": root.id }, "generation": 1 },
                "binding": { "kind": "command" },
                "provenance": { "origin": "system", "trust": "runtime_instruction", "priority": "normal" },
                "admitted_fences": { "source_revision": 1, "agent_control_revision": 1, "host_registry_revision": 1 },
                "state": "settled", "turn_id": turn.turn_id, "terminal_outcome_id": "root-outcome", "admitted_at": time().to_rfc3339(),
            })).unwrap();
            insert_attempt(tx, &predecessor);
            let outcome = crate::domain::execution_protocol::ExecutionOutcomeRecord {
                outcome_id: "root-outcome".into(),
                attempt_id: predecessor.attempt_id.clone(),
                outcome: crate::domain::execution_protocol::ExecutionOutcome::Command(
                    crate::domain::execution_protocol::CommandResult::Applied {
                        references: Vec::new(),
                    },
                ),
                created_at: time().to_rfc3339(),
            };
            tx.execute("INSERT INTO execution_protocol_outcomes (agent_id, outcome_id, attempt_id, payload_json)
               VALUES ('agent-a', 'root-outcome', 'settled-predecessor', ?1)", [serde_json::to_string(&outcome).unwrap()]).unwrap();
            let mut recovery = MessageEnvelope::new(
                "agent-a",
                crate::types::MessageKind::InternalFollowup,
                crate::types::MessageOrigin::System {
                    subsystem: "model_lineage_recovery".into(),
                },
                crate::types::AuthorityClass::RuntimeInstruction,
                crate::types::Priority::Next,
                crate::types::MessageBody::Text {
                    text: "recover".into(),
                },
            )
            .with_admission(
                crate::types::MessageDeliverySurface::RuntimeSystem,
                crate::types::AdmissionContext::RuntimeOwned,
            );
            recovery.id = if queued_only {
                "queued-recovery"
            } else {
                "live-recovery-message"
            }
            .into();
            recovery.causation_id = Some(root.id.clone());
            recovery
                .source_refs
                .insert("source_message_id".into(), root.id.clone());
            recovery
                .source_refs
                .insert("source_turn_id".into(), turn.turn_id.clone());
            recovery.metadata = Some(serde_json::json!({ "provider_recovery": {
                "fallback_model_ref": "openai/gpt-5.4", "source_message_id": root.id,
                "source_turn_id": turn.turn_id, "source_terminal_kind": "deferred_to_fallback", "source_round": 1,
            }}));
            let recovery = crate::runtime_db::evidence::append_message_tx(tx, &recovery)
                .unwrap()
                .0;
            if queued_only {
                tx.execute("INSERT INTO queue_entries (message_id, agent_id, priority, status, created_at, updated_at, payload_json)
                    VALUES ('queued-recovery', 'agent-a', 'next', 'queued', ?1, ?1, '{}')", [time().to_rfc3339()]).unwrap();
            } else {
                let mut live = predecessor.clone();
                live.attempt_id = "live-recovery".into();
                live.source_message_id = Some(recovery.id.clone());
                live.turn_id = Some("live-recovery-turn".into());
                live.source.identity = ExecutionSourceIdentity::RuntimeRecovery {
                    recovery_id: recovery.id.clone(),
                };
                live.source.generation = recovery.message_seq.unwrap();
                live.admitted_fences.source_revision = live.source.generation;
                let mut started = TurnRecord::new("agent-a", "live-recovery-turn", 2);
                started.owner = Some(crate::types::TurnOwner::Command);
                started.trigger = Some(crate::types::TurnTriggerSummary::from_message(&recovery));
                started.input_message_ids = vec![recovery.id.clone()];
                crate::runtime_db::repositories::upsert_turn_record_tx(tx, &started).unwrap();
                live.recovery_of_attempt_id = Some(predecessor.attempt_id.clone());
                live.state = crate::domain::execution_protocol::ExecutionAttemptState::Open;
                live.terminal_outcome_id = None;
                insert_attempt(tx, &live);
            }
            insert_episode(tx, &episode("agent-a", "recovery-root-episode", 1, None));
            for index in 2..=20 {
                insert_episode(
                    tx,
                    &episode("agent-a", &format!("recent-{index}"), index, None),
                );
            }
            import_complete(tx);
        })
    }

    #[test]
    fn live_unbound_recovery_follows_its_settled_predecessor_outside_the_recent_window() {
        let fixture = recovery_fixture(false);
        assert!(fixture
            .current("agent-a", 1, 30)
            .episodes
            .iter()
            .any(|episode| episode.id == "recovery-root-episode"));
    }

    #[test]
    fn queued_unbound_provider_recovery_resolves_typed_raw_source_ids_before_admission() {
        let fixture = recovery_fixture(true);
        assert!(fixture
            .current("agent-a", 1, 30)
            .episodes
            .iter()
            .any(|episode| episode.id == "recovery-root-episode"));
    }

    #[test]
    fn mandatory_closure_budget_fails_explicitly_instead_of_truncating() {
        let mut collector = RootCollector {
            current_depth: CONTINUITY_MAX_REF_DEPTH + 1,
            ..Default::default()
        };
        collector.reference("message", "must-keep");
        let error = collector.ensure_budget().unwrap_err();
        assert_eq!(
            error.downcast_ref::<EpisodeSnapshotError>(),
            Some(&EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "reference depth"
            })
        );
        let mut collector = RootCollector::default();
        collector.payload_bytes = CONTINUITY_MAX_PAYLOAD_BYTES;
        let error = collector.digest("message", "must-keep", "x").unwrap_err();
        assert_eq!(
            error.downcast_ref::<EpisodeSnapshotError>(),
            Some(&EpisodeSnapshotError::ClosureResourceExhausted {
                limit: "payload bytes"
            })
        );
    }
    #[test]
    fn first_global_import_closes_over_all_fixed_scoped_state_roots_before_sealing() {
        let fixture = Fixture::legacy(|tx| {
            for id in ["agent-a", "agent-b"] {
                upsert_agent_identity_tx(tx, &identity(id)).unwrap();
            }
            let mut work = WorkItemRecord::new(
                "agent-b",
                "terminal owner retained by the legacy turn snapshot",
                WorkItemState::Completed,
            );
            work.id = "old-b-owner".into();
            insert_work(tx, &work);
            insert_episode(tx, &episode("agent-b", "old-b-episode", 1, Some(&work.id)));
            for index in 2..=20 {
                insert_episode(
                    tx,
                    &episode("agent-b", &format!("b-recent-{index}"), index, None),
                );
            }
        });
        let mut b = state("agent-b");
        // A terminal owner can remain in the turn snapshot; current focus must
        // instead reference an Open WorkItem and is not this fixture's root.
        b.current_turn_work_item_id = Some("old-b-owner".into());
        fixture
            .db
            .agent_states()
            .import_legacy_many(vec![state("agent-a"), b.clone()])
            .unwrap();
        // The global Episode import can be entered by A; B roots must already
        // be canonical when it seals all fixed pending scope checkpoints.
        fixture
            .db
            .context_episodes()
            .import_legacy(Vec::new())
            .unwrap();
        let current = fixture.current("agent-b", 1, 30);
        assert_eq!(current.state, Some(b));
        assert!(current
            .episodes
            .iter()
            .any(|episode| episode.id == "old-b-episode"));
        assert!(fixture
            .db
            .pending_legacy_episode_import_agent_ids()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn upgrade_missing_historical_table_freezes_absence_without_late_adoption() {
        let mut connection = Connection::open_in_memory().unwrap();
        ensure_migration_table(&connection).unwrap();
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 81)
        {
            apply_migration(&mut connection, migration).unwrap();
        }
        {
            let tx = connection.transaction().unwrap();
            upsert_agent_identity_tx(&tx, &identity("agent-a")).unwrap();
            let mut original = state("agent-a");
            original.current_turn_id = Some("late-turn".into());
            upsert_agent_state_tx(&tx, &original).unwrap();
            import_complete(&tx);
            tx.execute_batch("DROP TABLE turn_records").unwrap();
            tx.commit().unwrap();
        }
        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.version == 82)
            .unwrap();
        apply_migration(&mut connection, migration).unwrap();
        let original: (String, String) = connection.query_row(
            "SELECT roots_json,candidate_payloads_json FROM episode_continuity_seeds WHERE agent_id='agent-a'",
            [], |row|Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        let roots: EpisodeContinuityRoots = serde_json::from_str(&original.0).unwrap();
        assert!(roots.unavailable_refs.contains("turn:late-turn"));
        assert!(
            collect_live_episode_roots(&connection, "agent-a").is_err(),
            "normal root preparation must not inherit upgrade-only missing-table tolerance"
        );
        {
            let tx = connection.transaction().unwrap();
            tx.execute_batch(
                MIGRATIONS
                    .iter()
                    .find(|migration| migration.version == 8)
                    .unwrap()
                    .sql,
            )
            .unwrap();
            let turn = TurnRecord::new("agent-a", "late-turn", 1);
            tx.execute("INSERT INTO turn_records(turn_id,turn_index,agent_id,created_at,payload_json) VALUES(?1,?2,?3,?4,?5)",
                params![turn.turn_id, i64::try_from(turn.turn_index).unwrap(), turn.agent_id, turn.created_at.to_rfc3339(), serde_json::to_string(&turn).unwrap()]).unwrap();
            insert_episode(&tx, &episode("agent-a", "late-episode", 1, None));
            capture_episode_continuity_upgrade_tx(&tx, time()).unwrap();
            tx.commit().unwrap();
        }
        let after: (String, String) = connection.query_row(
            "SELECT roots_json,candidate_payloads_json FROM episode_continuity_seeds WHERE agent_id='agent-a'",
            [], |row|Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        assert_eq!(after, original);
        let membership: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM episode_continuity_members WHERE episode_id='late-episode'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(membership, 0);
        let loaded = load_episode_memory_snapshot_in(&connection, &scope("agent-a", 1), 12, None)
            .unwrap()
            .require_current()
            .unwrap();
        assert!(loaded.episodes.is_empty());
    }

    #[test]
    fn upgrade_existing_malformed_canonical_payload_still_rolls_back_fixed_boundary() {
        let mut connection = Connection::open_in_memory().unwrap();
        ensure_migration_table(&connection).unwrap();
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 81)
        {
            apply_migration(&mut connection, migration).unwrap();
        }
        connection.execute("INSERT INTO agent_states(agent_id,status,updated_at,payload_json) VALUES('broken','stopped',?1,'{}')", [time().to_rfc3339()]).unwrap();
        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.version == 82)
            .unwrap();
        let error = apply_migration(&mut connection, migration).unwrap_err();
        assert!(format!("{error:#}").contains("decoding agent state payload"));
        assert_eq!(
            crate::runtime_db::migrations::current_schema_version(&connection).unwrap(),
            81
        );
        let boundary: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM runtime_metadata WHERE key=?1",
                [UPGRADE_BOUNDARY_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(boundary, 0);
        let seeds_table:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='episode_continuity_seeds')",[],|row|row.get(0)).unwrap();
        assert!(!seeds_table);
    }

    fn work_checkpoint(
        fixture: &Fixture,
        phase: &str,
    ) -> work_checkpoint::WorkContinuityCheckpoint {
        fixture
            .db
            .transaction(|tx| {
                let seed: String = tx.query_row(
                    "SELECT seed_id FROM episode_continuity_seeds WHERE agent_id='agent-a'",
                    [],
                    |row| row.get(0),
                )?;
                work_checkpoint::load(tx, &seed, phase)
            })
            .unwrap()
    }

    #[test]
    fn fixed_work_checkpoint_keeps_latest_nonempty_result_without_expanding_episode_owner() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            let mut work = WorkItemRecord::new("agent-a", "closed owner", WorkItemState::Completed);
            work.id = "completed-owner".into();
            work.created_at = time();
            work.updated_at = time();
            insert_work(tx, &work);
            let mut source = MessageEnvelope::new(
                "agent-a",
                crate::types::MessageKind::OperatorPrompt,
                crate::types::MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                crate::types::AuthorityClass::OperatorInstruction,
                crate::types::Priority::Normal,
                crate::types::MessageBody::Text {
                    text: "completion source".into(),
                },
            );
            source.id = "completion-source".into();
            source.created_at = time();
            let source = super::super::evidence::append_message_tx(tx, &source)
                .unwrap()
                .0;
            let mut transcript = crate::types::TranscriptEntry::new(
                "agent-a",
                crate::types::TranscriptEntryKind::IncomingMessage,
                None,
                Some(source.id),
                serde_json::json!({"text":"completion source"}),
            );
            transcript.id = "completion-transcript".into();
            transcript.created_at = time();
            let transcript = super::super::evidence::append_transcript_entry_tx(tx, &transcript)
                .unwrap()
                .0;
            for (id, kind, text, offset) in [
                ("result-a", crate::types::BriefKind::Result, "older tie", 1),
                ("result-b", crate::types::BriefKind::Result, "newer tie", 1),
                ("result-z", crate::types::BriefKind::Result, "", 2),
                (
                    "failure-z",
                    crate::types::BriefKind::Failure,
                    "later failure",
                    3,
                ),
            ] {
                let mut brief = BriefRecord::new("agent-a", kind, text, None, None);
                brief.id = id.into();
                brief.work_item_id = Some(work.id.clone());
                brief.created_at = time() + chrono::Duration::seconds(offset);
                brief.content_source = crate::types::BriefContentSource::TranscriptEntry {
                    entry_id: transcript.id.clone(),
                    relation: Default::default(),
                };
                super::super::evidence::insert_brief_evidence_tx(tx, &brief).unwrap();
            }
            insert_episode(
                tx,
                &episode("agent-a", "old-completed-owner-episode", 1, Some(&work.id)),
            );
            for index in 2..=20 {
                insert_episode(
                    tx,
                    &episode("agent-a", &format!("recent-{index}"), index, None),
                );
            }
            import_complete(tx);
        });
        let checkpoint = work_checkpoint(&fixture, "upgrade");
        let result = checkpoint
            .latest_results
            .get("completed-owner")
            .unwrap()
            .as_ref()
            .unwrap();
        assert_eq!(result.brief_id, "result-b");
        assert_eq!(
            result.transcript_entry_id.as_deref(),
            Some("completion-transcript")
        );
        assert_eq!(
            result.source_message_id.as_deref(),
            Some("completion-source")
        );
        fixture.db.transaction(|tx| {
            let payload: String = tx.query_row("SELECT roots_json FROM episode_continuity_seeds WHERE agent_id='agent-a'", [], |row|row.get(0))?;
            let roots: EpisodeContinuityRoots = serde_json::from_str(&payload)?;
            assert!(!roots.work_item_ids.contains("completed-owner"));
            assert!(roots.record_digests.contains_key("brief:result-b"));
            assert!(roots.record_digests.contains_key("transcript:completion-transcript"));
            let adopted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM episode_continuity_members WHERE episode_id='old-completed-owner-episode')", [], |row|row.get(0))?;
            assert!(!adopted, "D4 optional Work back-edges cannot grant D1 live-owner Episodes");
            let mut late = BriefRecord::new("agent-a", crate::types::BriefKind::Result, "late unqualified result", None, None);
            late.id = "late-result".into();
            late.work_item_id = Some("completed-owner".into());
            late.created_at = time() + chrono::Duration::days(1);
            super::super::evidence::insert_brief_evidence_tx(tx, &late)?;
            capture_episode_continuity_upgrade_tx(tx, time())?;
            Ok(())
        }).unwrap();
        assert_eq!(work_checkpoint(&fixture, "upgrade"), checkpoint);
    }

    #[test]
    fn fixed_work_checkpoint_bounds_nonopen_tail_but_keeps_live_and_frame_endpoints() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            for index in 0..130 {
                let mut work =
                    WorkItemRecord::new("agent-a", "closed history", WorkItemState::Completed);
                work.id = format!("work-{index:03}");
                work.created_at = time();
                work.updated_at = time();
                insert_work(tx, &work);
            }
            let mut completing =
                WorkItemRecord::new("agent-a", "still completing", WorkItemState::Completing);
            completing.id = "old-completing".into();
            completing.created_at = time() - chrono::Duration::days(1);
            completing.updated_at = completing.created_at;
            insert_work(tx, &completing);
            let mut frame = WorkItemContinuationFrame::new_on_completed(
                "agent-a",
                "work-129",
                &completing.id,
                None,
            );
            frame.id = "active-frame".into();
            frame.created_at = time();
            frame.updated_at = time();
            super::super::repositories::upsert_work_item_continuation_tx(tx, &frame).unwrap();
            import_complete(tx);
        });
        let checkpoint = work_checkpoint(&fixture, "upgrade");
        let window = checkpoint.nonopen.as_ref().unwrap();
        assert_eq!(window.work_item_ids.len(), 128);
        assert!(!window.is_exhaustive);
        assert_eq!(
            window.work_item_ids.first().map(String::as_str),
            Some("work-000")
        );
        assert_eq!(
            window.work_item_ids.last().map(String::as_str),
            Some("work-127")
        );
        for key in [
            "work_item:old-completing",
            "work_item:work-129",
            "continuation:active-frame",
        ] {
            assert!(checkpoint.record_digests.contains_key(key), "{key}");
        }
        assert_eq!(checkpoint.latest_results.get("old-completing"), Some(&None));
        assert_eq!(checkpoint.latest_results.get("work-129"), Some(&None));
    }

    #[test]
    fn first_import_work_checkpoint_does_not_rescan_optional_history() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            let mut old = WorkItemRecord::new("agent-a", "old optional", WorkItemState::Completed);
            old.id = "old-optional".into();
            old.created_at = time();
            old.updated_at = time();
            insert_work(tx, &old);
        });
        let fixed = work_checkpoint(&fixture, "upgrade");
        fixture
            .db
            .transaction(|tx| {
                let mut late =
                    WorkItemRecord::new("agent-a", "late optional", WorkItemState::Completed);
                late.id = "late-optional".into();
                late.created_at = time() + chrono::Duration::days(1);
                late.updated_at = late.created_at;
                insert_work(tx, &late);
                let mut current =
                    WorkItemRecord::new("agent-a", "imported current", WorkItemState::Open);
                current.id = "imported-current".into();
                current.created_at = time();
                current.updated_at = time();
                insert_work(tx, &current);
                Ok(())
            })
            .unwrap();
        let mut imported = state("agent-a");
        imported.current_work_item_id = Some("imported-current".into());
        fixture
            .db
            .agent_states()
            .import_legacy_many(vec![imported])
            .unwrap();
        fixture
            .db
            .context_episodes()
            .import_legacy(Vec::new())
            .unwrap();
        let import = work_checkpoint(&fixture, "first_import");
        assert!(import.nonopen.is_none());
        assert!(import
            .record_digests
            .contains_key("work_item:imported-current"));
        assert!(!import
            .record_digests
            .contains_key("work_item:late-optional"));
        assert!(!import.latest_results.contains_key("late-optional"));
        assert_eq!(work_checkpoint(&fixture, "upgrade"), fixed);
    }

    #[test]
    fn fixed_work_completion_records_optional_absence_but_never_foreign_or_pruned_none() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            import_complete(tx);
        });
        fixture
            .db
            .transaction(|tx| {
                let mut work = WorkItemRecord::new(
                    "agent-a",
                    "dangling optional completion",
                    WorkItemState::Completed,
                );
                work.id = "optional-absent-work".into();
                work.created_at = time();
                work.updated_at = time();
                insert_work(tx, &work);
                let mut brief = BriefRecord::new(
                    "agent-a",
                    crate::types::BriefKind::Result,
                    "kept completion",
                    None,
                    None,
                );
                brief.id = "optional-absent-brief".into();
                brief.work_item_id = Some(work.id.clone());
                brief.content_source = crate::types::BriefContentSource::TranscriptEntry {
                    entry_id: "optional-source".into(),
                    relation: Default::default(),
                };
                super::super::evidence::insert_brief_evidence_tx(tx, &brief)?;
                Ok(())
            })
            .unwrap();
        fixture.db.transaction(|tx| {
            let (checkpoint, inputs) = work_checkpoint::capture(tx, "agent-a", None, &EpisodeContinuityRoots::default(), 0, true)?;
            let head = checkpoint.latest_results["optional-absent-work"].as_ref().unwrap();
            assert_eq!(head.transcript_entry_id, None);
            let observation = &checkpoint.completion_sources["optional-absent-brief"];
            assert_eq!(observation.transcript.as_ref().unwrap().availability, work_checkpoint::CompletionSourceAvailability::GlobalAbsent);
            assert!(checkpoint.unavailable_refs.contains("transcript:optional-source"));
            assert!(inputs.contains_key("brief:optional-absent-brief"));
            let mut foreign = crate::types::TranscriptEntry::new("agent-other", crate::types::TranscriptEntryKind::AssistantRound, None, None, serde_json::json!({"text":"foreign"}));
            foreign.id = "optional-source".into();
            super::super::evidence::append_transcript_entry_tx(tx, &foreign)?;
            let error = work_checkpoint::capture(tx, "agent-a", None, &EpisodeContinuityRoots::default(), 0, true).unwrap_err();
            assert!(format!("{error:#}").contains("scope/key mismatch"));
            tx.execute("DELETE FROM transcript_entries WHERE evidence_id='optional-source'", [])?;
            tx.execute("INSERT INTO input_evidence_current(agent_id,identity_incarnation,family,record_id,revision,content_digest,fingerprint_json,availability,qualification) VALUES('agent-other',1,'transcript','optional-source',1,NULL,'{}','pruned','committed_writer')", [])?;
            let error = work_checkpoint::capture(tx, "agent-a", None, &EpisodeContinuityRoots::default(), 0, true).unwrap_err();
            assert!(format!("{error:#}").contains("qualified or pruned"));
            Ok(())
        }).unwrap();
    }

    // v81 has not run the current conditional blocker/recheck backfill.
    fn insert_work_for_import(tx: &Transaction<'_>, work: &WorkItemRecord) {
        insert_work(tx, work);
        let stamp = |at: DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        tx.execute(
            "UPDATE work_items SET plan_status=?1,created_at=?2,updated_at=?3,completed_at=?4,plan_artifact_path=?5,last_turn_id=?6 WHERE work_item_id=?7",
            params![serde_json::to_value(&work.plan_status).unwrap().as_str().unwrap(),
                stamp(work.created_at),stamp(work.updated_at),
                (work.state == WorkItemState::Completed).then(||stamp(work.updated_at)),
                work.plan_artifact.as_ref().map(|artifact|artifact.path.display().to_string()),
                work.turn_id,work.id],
        ).unwrap();
    }

    // These fixtures intentionally stop before the conditional normalized backfill.
    fn insert_wait_v81(tx: &Transaction<'_>, wait: &WaitConditionRecord) {
        let stamp = |at: DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        tx.execute(
            "INSERT INTO wait_conditions (
                wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,
                waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,
                last_turn_id,trigger_message_id,triggered_at,payload_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                wait.id,
                wait.agent_id,
                wait.work_item_id,
                serde_json::to_value(&wait.status)
                    .unwrap()
                    .as_str()
                    .unwrap(),
                serde_json::to_value(&wait.kind).unwrap().as_str().unwrap(),
                wait.source,
                wait.subject_ref,
                wait.waiting_for,
                stamp(wait.created_at),
                stamp(wait.updated_at),
                wait.expires_at.map(stamp),
                wait.resolved_at.map(stamp),
                wait.cancelled_at.map(stamp),
                wait.turn_id,
                wait.trigger_message_id(),
                wait.triggered_at().map(stamp),
                serde_json::to_string(wait).unwrap()
            ],
        )
        .unwrap();
    }

    #[test]
    fn fixed_work_checkpoint_includes_complete_current_turn_wait_relation() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            let mut current = state("agent-a");
            current.current_turn_id = Some("current-wait-turn".into());
            upsert_agent_state_tx(tx, &current).unwrap();
            for (id, status, turn) in [
                ("wait-resolved", "resolved", "current-wait-turn"),
                ("wait-cancelled", "cancelled", "current-wait-turn"),
                ("wait-unrelated", "resolved", "other-turn"),
            ] {
                let wait: WaitConditionRecord = serde_json::from_value(serde_json::json!({
                    "id":id,"agent_id":"agent-a","status":status,"kind":"operator",
                    "waiting_for":"operator","wake_sources":[],"created_at":time(),
                    "updated_at":time(),"turn_id":turn
                }))
                .unwrap();
                insert_wait_v81(tx, &wait);
            }
            import_complete(tx);
        });
        let checkpoint = work_checkpoint(&fixture, "upgrade");
        assert_eq!(
            checkpoint.turn_wait_keys["current-wait-turn"],
            BTreeSet::from(["wait-resolved".into(), "wait-cancelled".into()])
        );
        assert!(checkpoint.record_digests.contains_key("wait:wait-resolved"));
        assert!(checkpoint
            .record_digests
            .contains_key("wait:wait-cancelled"));
        assert!(!checkpoint
            .record_digests
            .contains_key("wait:wait-unrelated"));
    }

    #[test]
    fn first_import_fixed_live_work_never_adopts_a_late_unreceived_result() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            let mut current = state("agent-a");
            current.current_work_item_id = Some("live-fixed".into());
            let mut work = WorkItemRecord::new("agent-a", "fixed live", WorkItemState::Open);
            work.id = "live-fixed".into();
            work.created_at = time();
            work.updated_at = time();
            insert_work_for_import(tx, &work);
            upsert_agent_state_tx(tx, &current).unwrap();
        });
        assert_eq!(
            work_checkpoint(&fixture, "upgrade").latest_results["live-fixed"],
            None
        );
        fixture
            .db
            .transaction(|tx| {
                let mut late = BriefRecord::new(
                    "agent-a",
                    crate::types::BriefKind::Result,
                    "not received by import",
                    None,
                    None,
                );
                late.id = "raw-late-head".into();
                late.work_item_id = Some("live-fixed".into());
                super::super::evidence::insert_brief_evidence_tx(tx, &late)?;
                Ok(())
            })
            .unwrap();
        fixture
            .db
            .evidence()
            .import_legacy(Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
            .unwrap();
        fixture
            .db
            .context_episodes()
            .import_legacy(Vec::new())
            .unwrap();
        let imported = work_checkpoint(&fixture, "first_import");
        assert_eq!(imported.latest_results["live-fixed"], None);
        fixture.db.transaction(|tx| {
            let eligible: i64 = tx.query_row("SELECT COUNT(*) FROM input_evidence_eligibility WHERE family='brief' AND record_id='raw-late-head'", [], |row|row.get(0))?;
            assert_eq!(eligible, 0);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn first_import_legacy_equal_result_preserves_bytes_and_excludes_unreceived_newer_head() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            let mut current = state("agent-a");
            current.current_work_item_id = Some("live-fixed".into());
            let mut work = WorkItemRecord::new("agent-a", "fixed live", WorkItemState::Open);
            work.id = "live-fixed".into();
            work.created_at = time();
            work.updated_at = time();
            insert_work_for_import(tx, &work);
            upsert_agent_state_tx(tx, &current).unwrap();
        });
        let mut received = BriefRecord::new(
            "agent-a",
            crate::types::BriefKind::Result,
            "original import equal",
            None,
            None,
        );
        received.id = "legacy-received-head".into();
        received.work_item_id = Some("live-fixed".into());
        received.created_at = time();
        let pretty = serde_json::to_string_pretty(&received).unwrap();
        fixture
            .db
            .transaction(|tx| {
                super::super::evidence::insert_brief_evidence_tx(tx, &received)?;
                tx.execute(
                    "UPDATE briefs SET payload_json=?1,content_hash=?2 WHERE evidence_id=?3",
                    params![
                        pretty,
                        super::super::evidence::content_hash(&pretty),
                        received.id
                    ],
                )?;
                let mut late = BriefRecord::new(
                    "agent-a",
                    crate::types::BriefKind::Result,
                    "unreceived later result",
                    None,
                    None,
                );
                late.id = "late-after-received".into();
                late.work_item_id = Some("live-fixed".into());
                late.created_at = time() + chrono::Duration::days(1);
                super::super::evidence::insert_brief_evidence_tx(tx, &late)?;
                Ok(())
            })
            .unwrap();
        fixture
            .db
            .evidence()
            .import_legacy(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![received.clone()],
                Vec::new(),
            )
            .unwrap();
        fixture
            .db
            .transaction(|tx| {
                let actual: String = tx.query_row(
                    "SELECT payload_json FROM briefs WHERE evidence_id=?1",
                    [&received.id],
                    |row| row.get(0),
                )?;
                assert_eq!(actual, pretty);
                let checkpoint: String = tx.query_row(
                    "SELECT source_checkpoint_json FROM storage_domains WHERE domain='evidence'",
                    [],
                    |row| row.get(0),
                )?;
                let checkpoint: serde_json::Value = serde_json::from_str(&checkpoint)?;
                let observed = &checkpoint["work_domain_actual_writes"]["records"]
                    ["brief:legacy-received-head"];
                assert_eq!(observed["origin"], "legacy_import_equal");
                assert_eq!(observed["raw_digest"], bytes_digest(pretty.as_bytes()));
                Ok(())
            })
            .unwrap();
        fixture
            .db
            .context_episodes()
            .import_legacy(Vec::new())
            .unwrap();
        let imported = work_checkpoint(&fixture, "first_import");
        assert_eq!(
            imported.latest_results["live-fixed"]
                .as_ref()
                .unwrap()
                .brief_id,
            received.id
        );
        fixture.db.transaction(|tx| {
            let eligible:i64=tx.query_row("SELECT COUNT(*) FROM input_evidence_eligibility WHERE family='brief' AND record_id='late-after-received'",[],|row|row.get(0))?;
            assert_eq!(eligible,0);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn first_import_advances_only_fixed_work_actual_versions_and_preserves_equal_raw_bytes() {
        let mut original =
            WorkItemRecord::new("agent-a", "fixed optional", WorkItemState::Completed);
        original.id = "fixed-optional".into();
        original.created_at = time();
        original.updated_at = time();
        let pretty = serde_json::to_string_pretty(&original).unwrap();
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            insert_work_for_import(tx, &original);
            tx.execute(
                "UPDATE work_items SET payload_json=?1 WHERE work_item_id=?2",
                params![pretty, original.id],
            )
            .unwrap();
        });
        let fixed = work_checkpoint(&fixture, "upgrade");
        fixture
            .db
            .work_items()
            .import_legacy(vec![original.clone()])
            .unwrap();
        fixture
            .db
            .transaction(|tx| {
                let actual: String = tx.query_row(
                    "SELECT payload_json FROM work_items WHERE work_item_id=?1",
                    [&original.id],
                    |row| row.get(0),
                )?;
                assert_eq!(actual, pretty);
                Ok(())
            })
            .unwrap();
        // A new fixture exercises a true newer imported version of the same fixed member.
        let changed_fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            insert_work_for_import(tx, &original);
        });
        let mut changed = original.clone();
        changed.revision += 1;
        changed.objective = "original file newer version".into();
        changed.updated_at += chrono::Duration::seconds(1);
        let mut late = WorkItemRecord::new("agent-a", "late nonopen", WorkItemState::Completed);
        late.id = "late-imported-optional".into();
        late.created_at = time();
        late.updated_at = time();
        changed_fixture
            .db
            .work_items()
            .import_legacy(vec![changed.clone(), late])
            .unwrap();
        changed_fixture
            .db
            .context_episodes()
            .import_legacy(Vec::new())
            .unwrap();
        let imported = work_checkpoint(&changed_fixture, "first_import");
        assert!(imported.nonopen.is_none());
        assert_eq!(
            imported.record_digests["work_item:fixed-optional"],
            bytes_digest(serde_json::to_string(&changed).unwrap().as_bytes())
        );
        assert!(!imported
            .record_digests
            .contains_key("work_item:late-imported-optional"));
        assert_eq!(work_checkpoint(&fixture, "upgrade"), fixed);
    }

    #[test]
    fn first_import_wait_footprint_includes_implicitly_cancelled_fixed_owner() {
        let old: WaitConditionRecord = serde_json::from_value(serde_json::json!({
            "id":"old-fixed-wait","agent_id":"agent-a","status":"active","kind":"operator",
            "waiting_for":"operator","created_at":time(),"updated_at":time()
        }))
        .unwrap();
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            insert_wait_v81(tx, &old);
        });
        let mut newer = old.clone();
        newer.id = "new-imported-wait".into();
        newer.updated_at += chrono::Duration::seconds(1);
        fixture
            .db
            .wait_conditions()
            .import_legacy(vec![newer])
            .unwrap();
        let cancelled = fixture.db.transaction(|tx| {
            let checkpoint: String=tx.query_row("SELECT source_checkpoint_json FROM storage_domains WHERE domain='wait_conditions'",[],|row|row.get(0))?;
            let checkpoint:serde_json::Value=serde_json::from_str(&checkpoint)?;
            assert!(checkpoint["work_domain_actual_writes"]["records"].get("wait:old-fixed-wait").is_some());
            assert!(checkpoint["work_domain_actual_writes"]["records"].get("wait:new-imported-wait").is_some());
            let payload:String=tx.query_row("SELECT payload_json FROM wait_conditions WHERE wait_condition_id='old-fixed-wait'",[],|row|row.get(0))?;
            let record:WaitConditionRecord=serde_json::from_str(&payload)?;
            assert_eq!(record.status,crate::types::WaitConditionStatus::Cancelled);
            Ok(bytes_digest(payload.as_bytes()))
        }).unwrap();
        fixture
            .db
            .context_episodes()
            .import_legacy(Vec::new())
            .unwrap();
        let imported = work_checkpoint(&fixture, "first_import");
        assert_eq!(imported.record_digests["wait:old-fixed-wait"], cancelled);
    }

    #[test]
    fn absent_work_manifest_is_not_observed_no_result_and_combined_body_budget_is_strict() {
        let fixture = Fixture::legacy(|tx| {
            upsert_agent_identity_tx(tx, &identity("agent-a")).unwrap();
            upsert_agent_state_tx(tx, &state("agent-a")).unwrap();
            let mut work = WorkItemRecord::new("agent-a", "body budget", WorkItemState::Completed);
            work.id = "budget-work".into();
            work.created_at = time();
            work.updated_at = time();
            insert_work(tx, &work);
            import_complete(tx);
        });
        fixture.db.transaction(|tx| {
            let error = work_checkpoint::capture(tx, "agent-a", None, &EpisodeContinuityRoots::default(), CONTINUITY_MAX_PAYLOAD_BYTES, true).unwrap_err();
            assert_eq!(error.downcast_ref::<EpisodeSnapshotError>(), Some(&EpisodeSnapshotError::ClosureResourceExhausted { limit: "payload bytes" }));
            let (seed, payload): (String, String) = tx.query_row("SELECT seed_id,roots_json FROM episode_continuity_seeds WHERE agent_id='agent-a'", [], |row|Ok((row.get(0)?,row.get(1)?)))?;
            let mut old: serde_json::Value = serde_json::from_str(&payload)?;
            old.as_object_mut().unwrap().remove("work_domain_checkpoint");
            tx.execute("UPDATE episode_continuity_seeds SET roots_json=?1 WHERE seed_id=?2", params![serde_json::to_string(&old)?,seed])?;
            let error = work_checkpoint::load(tx, &seed, "upgrade").unwrap_err();
            assert!(matches!(error.downcast_ref::<work_checkpoint::WorkCheckpointReadError>(), Some(work_checkpoint::WorkCheckpointReadError::Missing { .. })));
            Ok(())
        }).unwrap();
    }
}

#[cfg(test)]
#[path = "episode_snapshot_deleting_upgrade_tests.rs"]
mod deleting_upgrade_tests;
