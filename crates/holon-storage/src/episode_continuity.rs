//! SQLite Episode continuity and qualification within an existing snapshot.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use holon_domain::{
    AgentIdentityRecord, AgentRegistryStatus, ContextEpisodeRecord, ObservationMaterial,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_registry::{decode_agent_identity_payload, AgentRegistryMaterial};

pub const EPISODE_MEMORY_PHASE: &str = "post_dispatch_memory/v1";
pub const EPISODE_CONTINUITY_POLICY: i64 = 1;
pub const LEGACY_EPISODE_RECENT_CANDIDATES: usize = 12;
pub const UPGRADE_BOUNDARY_KEY: &str = "episode_continuity/v1/upgrade_boundary";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeMemoryLoadScope {
    pub agent_id: String,
    pub identity_incarnation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EpisodeContinuityBinding {
    FreshEmpty,
    LegacyAdopted {
        seed_id: String,
        boundary_id: String,
        source_kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeSnapshotError {
    RetiredScope,
    MissingContinuityBinding,
    LegacyImportPending,
    BindingConflict,
    LegacyContentChanged { episode_id: String },
    ActiveBuilderCollision { episode_id: String },
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
        }
    }
}
impl std::error::Error for EpisodeSnapshotError {}

pub fn pending_legacy_episode_import_agent_ids(connection: &Connection) -> Result<Vec<String>> {
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

pub fn recent_episode_window(
    connection: &Connection,
    scope: &EpisodeMemoryLoadScope,
    continuity: &EpisodeContinuityBinding,
    recent_limit: usize,
    validate_receipt: impl Fn(String, &str, &str, &ContextEpisodeRecord) -> Result<()>,
) -> Result<(Vec<ContextEpisodeRecord>, bool)> {
    let seed_id = match continuity {
        EpisodeContinuityBinding::FreshEmpty => None,
        EpisodeContinuityBinding::LegacyAdopted { seed_id, .. } => Some(seed_id.as_str()),
    };
    let limit = i64::try_from(
        recent_limit
            .checked_add(1)
            .context("Episode window count overflow")?,
    )?;
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
    for row in rows {
        let (id, agent_id, work_item_id, payload) = row?;
        let episode = decode_context_episode_payload(&payload)?;
        ensure!(
            episode.id == id
                && episode.agent_id == agent_id
                && episode.agent_id == scope.agent_id
                && episode.current_work_item_id == work_item_id,
            "Episode columns and payload disagree"
        );
        validate_episode_qualification(
            connection,
            scope,
            seed_id,
            &episode,
            &payload,
            &validate_receipt,
        )?;
        episodes.push(episode);
    }
    let has_more = episodes.len() > recent_limit;
    episodes.truncate(recent_limit);
    episodes.reverse();
    Ok((episodes, has_more))
}

pub fn load_identity(
    connection: &Connection,
    agent_id: &str,
) -> Result<Option<AgentIdentityRecord>> {
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

pub fn episode_is_archived(connection: &Connection, episode_id: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM context_episode_anchors WHERE episode_id = ?1)",
        [episode_id],
        |row| row.get(0),
    )?)
}

pub fn load_continuity_binding(
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
    validate_receipt: &impl Fn(String, &str, &str, &ContextEpisodeRecord) -> Result<()>,
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
        validate_receipt(turn_id, &digest, &invocation, episode)?;
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

pub fn bytes_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub struct SeedIdentityDigest(pub [u8; 64]);

impl rusqlite::types::ToSql for SeedIdentityDigest {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(
            rusqlite::types::ValueRef::Text(&self.0),
        ))
    }
}

pub fn seed_identity_digest(identity: &AgentIdentityRecord) -> Result<SeedIdentityDigest> {
    let mut hasher = Sha256::new();
    serde_json::to_writer(&mut hasher, identity)?;
    let digest = hasher.finalize();
    let mut hex = [0; 64];
    for (byte, pair) in digest.iter().zip(hex.chunks_exact_mut(2)) {
        pair[0] = b"0123456789abcdef"[(byte >> 4) as usize];
        pair[1] = b"0123456789abcdef"[(byte & 15) as usize];
    }
    Ok(SeedIdentityDigest(hex))
}

#[doc(hidden)]
pub struct CandidatePayloadDigest(pub sha2::digest::Output<Sha256>);

impl Serialize for CandidatePayloadDigest {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&format_args!("{:x}", self.0))
    }
}

#[derive(Debug)]
pub struct ContinuitySeed {
    pub id: String,
    pub agent_id: String,
    pub incarnation: Option<u64>,
    pub import_required: bool,
    // Drop owned text before releasing its original caller phase.
    pub _material: Option<ObservationMaterial>,
}

/// Runtime accounting is retained by the returned seed for its whole lifetime.
pub struct EpisodeContinuityStorage {
    pub material: AgentRegistryMaterial,
    pub active_material: fn() -> Result<Option<ObservationMaterial>>,
}

pub fn read_upgrade_boundary(connection: &Connection) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM runtime_metadata WHERE key = ?1",
            [UPGRADE_BOUNDARY_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub fn insert_upgrade_boundary_tx(
    tx: &Transaction<'_>,
    boundary: &str,
    captured_at: DateTime<Utc>,
) -> Result<()> {
    let at = captured_at.to_rfc3339();
    tx.execute(
        "INSERT INTO runtime_metadata (key, value, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![UPGRADE_BOUNDARY_KEY, boundary, at],
    )?;
    Ok(())
}

/// The central identity upsert calls this only for a newly created namespace.
/// Existing explicit legacy binding is preserved; a Fresh binding never upgrades.
pub fn ensure_fresh_episode_continuity_tx(
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
    let timestamp = ContinuityTimestamp::encode(bound_at)?;
    let bound_at = timestamp.as_str()?;
    tx.execute(
        "INSERT INTO agent_episode_continuity (agent_id, identity_incarnation, policy_version, mode, seed_id, bound_at)
         VALUES (?1, ?2, ?3, 'fresh_empty', NULL, ?4) ON CONFLICT DO NOTHING",
        params![identity.agent_id, i64::try_from(identity.incarnation)?, EPISODE_CONTINUITY_POLICY, bound_at],
    )?;
    Ok(())
}

struct ContinuityTimestamp {
    // Chrono's signed six-digit year, nanoseconds and UTC offset need at most 38 bytes.
    bytes: [u8; 40],
    len: usize,
}

impl ContinuityTimestamp {
    fn encode(value: DateTime<Utc>) -> Result<Self> {
        let mut timestamp = Self {
            bytes: [0; 40],
            len: 0,
        };
        // Naive formatting avoids the owned offset name in DateTime::format.
        value
            .naive_utc()
            .format("%Y-%m-%dT%H:%M:%S%.f+00:00")
            .write_to(&mut timestamp)?;
        Ok(timestamp)
    }

    fn as_str(&self) -> Result<&str> {
        Ok(std::str::from_utf8(&self.bytes[..self.len])?)
    }
}

impl fmt::Write for ContinuityTimestamp {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let output = self
            .bytes
            .get_mut(self.len..)
            .and_then(|remaining| remaining.get_mut(..value.len()))
            .ok_or(fmt::Error)?;
        output.copy_from_slice(value.as_bytes());
        self.len += value.len();
        Ok(())
    }
}

/// Existing public/fixture/legacy upsert cannot overwrite a qualified Episode.
/// Unqualified writes acquire no continuity membership by themselves.
pub fn validate_immutable_episode_upsert_tx(
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

#[cfg(any(test, feature = "test-support"))]
pub fn episode_import_complete(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM storage_domains WHERE domain = 'context_episode_anchors' AND import_status = 'complete' AND canonical_source = 'db')",
        [], |row| row.get(0),
    )?)
}

pub fn has_any_binding(connection: &Connection, agent_id: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM agent_episode_continuity WHERE agent_id = ?1)",
        [agent_id],
        |row| row.get(0),
    )?)
}

pub fn ensure_no_prior_binding(connection: &Connection, agent_id: &str) -> Result<()> {
    if has_any_binding(connection, agent_id)? {
        return Err(EpisodeSnapshotError::BindingConflict.into());
    }
    Ok(())
}

pub fn insert_legacy_binding_tx(
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
    let timestamp = ContinuityTimestamp::encode(bound_at)?;
    let bound_at = timestamp.as_str()?;
    tx.execute(
        "INSERT INTO agent_episode_continuity (agent_id, identity_incarnation, policy_version, mode, seed_id, bound_at)
         VALUES (?1, ?2, ?3, 'legacy_adopted', ?4, ?5)",
        params![identity.agent_id, i64::try_from(identity.incarnation)?, EPISODE_CONTINUITY_POLICY, seed.id, bound_at],
    )?;
    Ok(())
}

#[doc(hidden)]
pub struct EpisodeRow {
    pub record: ContextEpisodeRecord,
    pub payload: String,
    pub receipted: bool,
}

const MEMBER_QUALIFICATION_JSON_MAX_BYTES: usize =
    b"[\"explicit_live_ref\",\"explicit_upgrade_recent_candidate\",\"live_owner\"]".len();

#[doc(hidden)]
pub struct MemberQualificationJson {
    bytes: [u8; MEMBER_QUALIFICATION_JSON_MAX_BYTES],
    len: usize,
}

impl rusqlite::types::ToSql for MemberQualificationJson {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(
            rusqlite::types::ValueRef::Text(&self.bytes[..self.len]),
        ))
    }
}

#[doc(hidden)]
pub fn member_qualification_json(reasons: &[&str]) -> Result<MemberQualificationJson> {
    let mut json = MemberQualificationJson {
        bytes: [0; MEMBER_QUALIFICATION_JSON_MAX_BYTES],
        len: 0,
    };
    let mut writer = std::io::Cursor::new(json.bytes.as_mut_slice());
    serde_json::to_writer(&mut writer, reasons)?;
    json.len = writer.position() as usize;
    Ok(json)
}

#[doc(hidden)]
pub fn member_content_digest(payload: &[u8]) -> [u8; 64] {
    sha256_hex(Sha256::digest(payload))
}

fn sha256_hex(digest: sha2::digest::Output<Sha256>) -> [u8; 64] {
    let mut hex = [0; 64];
    for (byte, pair) in digest.iter().zip(hex.chunks_exact_mut(2)) {
        pair[0] = b"0123456789abcdef"[(byte >> 4) as usize];
        pair[1] = b"0123456789abcdef"[(byte & 15) as usize];
    }
    hex
}

impl EpisodeContinuityStorage {
    /// Sole explicit no-identity legacy bootstrap actor. Call before the first identity
    /// upsert in the SAME identity/relations transaction, after legacy import completes.
    pub fn bind_unbound_episode_continuity_tx(
        &self,
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
        let Some(seed) = self.find_seed(tx, &identity.agent_id)? else {
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

    pub fn find_seed(
        &self,
        connection: &Connection,
        agent_id: &str,
    ) -> Result<Option<ContinuitySeed>> {
        let mut statement = connection.prepare(
            "SELECT seed_id, identity_incarnation, import_required FROM episode_continuity_seeds WHERE agent_id = ?1 AND policy_version = ?2",
        )?;
        let mut rows = statement.query(params![agent_id, EPISODE_CONTINUITY_POLICY])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let id = row.get_ref(0)?.as_str()?;
        let incarnation = row.get::<_, Option<i64>>(1)?;
        let import_required = row.get::<_, bool>(2)?;
        let material = (self.active_material)()?;
        (self.material.charge_owned_metadata)(
            u64::try_from(id.len().saturating_add(agent_id.len()))?,
            2,
        )?;
        Ok(Some(ContinuitySeed {
            id: id.to_owned(),
            agent_id: agent_id.to_owned(),
            incarnation: incarnation.map(u64::try_from).transpose()?,
            import_required,
            _material: material,
        }))
    }

    pub fn insert_seed_tx(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
        identity: Option<&AgentIdentityRecord>,
        source_kind: &str,
        roots: &impl Serialize,
        import_required: bool,
        captured_at: DateTime<Utc>,
        records: &[EpisodeRow],
    ) -> Result<ContinuitySeed> {
        let mut boundary_statement =
            tx.prepare("SELECT value FROM runtime_metadata WHERE key = ?1")?;
        let mut boundary_rows = boundary_statement.query([UPGRADE_BOUNDARY_KEY])?;
        let boundary_row = boundary_rows
            .next()?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let boundary = boundary_row.get_ref(0)?.as_str()?;
        let mut seed_hasher = Sha256::new();
        serde_json::to_writer(
            &mut seed_hasher,
            &(boundary, agent_id, EPISODE_CONTINUITY_POLICY),
        )?;
        let material = (self.active_material)()?;
        (self.material.charge_owned_metadata)(
            u64::try_from("episode_seed_".len() + 64 + agent_id.len())?,
            2,
        )?;
        let seed_id = format!("episode_seed_{:x}", seed_hasher.finalize());
        let incarnation = identity.map(|identity| identity.incarnation);
        let candidates = self.seed_candidate_payloads(records)?;
        let timestamp = ContinuityTimestamp::encode(captured_at)?;
        tx.execute(
            "INSERT INTO episode_continuity_seeds (seed_id, agent_id, policy_version, boundary_id, identity_incarnation, source_kind,
              identity_digest, roots_json, candidate_payloads_json, import_required, captured_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![seed_id, agent_id, EPISODE_CONTINUITY_POLICY, boundary, incarnation.map(i64::try_from).transpose()?, source_kind,
                identity.map(seed_identity_digest).transpose()?, self.continuity_roots_json(roots)?,
                self.seed_candidate_payloads_json(&candidates)?, import_required, timestamp.as_str()?],
        )?;
        drop(boundary_rows);
        drop(boundary_statement);
        Ok(ContinuitySeed {
            id: seed_id,
            agent_id: agent_id.to_owned(),
            incarnation,
            import_required,
            _material: material,
        })
    }

    pub fn continuity_roots_json(&self, roots: &impl Serialize) -> Result<String> {
        self.material.admit_serialized_value(roots)?;
        Ok(serde_json::to_string(roots)?)
    }

    pub fn seed_candidate_payloads_json(
        &self,
        candidates: &BTreeMap<&str, CandidatePayloadDigest>,
    ) -> Result<String> {
        self.material.admit_serialized_value(candidates)?;
        Ok(serde_json::to_string(candidates)?)
    }

    pub fn seed_candidate_payloads<'a>(
        &self,
        records: &'a [EpisodeRow],
    ) -> Result<BTreeMap<&'a str, CandidatePayloadDigest>> {
        let mut candidates = BTreeMap::new();
        for row in records.iter().filter(|row| !row.receipted) {
            let digest = CandidatePayloadDigest(Sha256::digest(row.payload.as_bytes()));
            match candidates.entry(row.record.id.as_str()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    // The phase's eightfold envelope covers the tree node, not borrowed ID text.
                    (self.material.charge_owned_metadata)(256, 1)?;
                    entry.insert(digest);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.insert(digest);
                }
            }
        }
        Ok(candidates)
    }

    pub fn member_has_explicit_ref(
        &self,
        source_refs: &BTreeSet<String>,
        id: &str,
    ) -> Result<bool> {
        if source_refs.is_empty() {
            return Ok(false);
        }
        (self.material.charge_owned_metadata)(
            ("episode:".len() as u64).saturating_add(id.len() as u64),
            1,
        )?;
        Ok(source_refs.contains(&format!("episode:{id}")))
    }

    pub fn capture_upgrade_episodes(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
        seed_id: &str,
        work_item_ids: &BTreeSet<String>,
        source_refs: &BTreeSet<String>,
    ) -> Result<()> {
        // Only IDs/digests and the twelve recent candidates outlive each decoded row.
        // Sort only compact ID/digest metadata, never full historical payloads.
        let mut statement = tx.prepare(
            "SELECT e.episode_id, e.payload_json, EXISTS (
               SELECT 1 FROM episode_memory_results r WHERE r.finalized_episode_id=e.episode_id)
             FROM context_episode_anchors e WHERE e.agent_id=?1",
        )?;
        let mut rows = statement.query([agent_id])?;
        let mut candidates = BTreeMap::new();
        let mut recent = BTreeMap::new();
        let mut count = 0usize;
        let mut bytes = 0u64;
        while let Some(row) = rows.next()? {
            let id = row.get_ref(0)?.as_str()?;
            let payload = row.get_ref(1)?.as_str()?;
            (self.material.admit_json_payload)(payload)?;
            let episode = decode_context_episode_payload(payload)?;
            ensure!(
                episode.id == id && episode.agent_id == agent_id,
                "upgrade Episode columns and payload disagree"
            );
            count = count
                .checked_add(1)
                .context("upgrade Episode count overflow")?;
            bytes = bytes
                .checked_add(u64::try_from(payload.len())?)
                .context("upgrade Episode bytes overflow")?;
            if row.get::<_, bool>(2)? {
                continue;
            }
            let digest = CandidatePayloadDigest(Sha256::digest(payload.as_bytes()));
            candidates.insert(id.to_owned(), digest);
            let live_owner = episode
                .current_work_item_id
                .as_ref()
                .is_some_and(|id| work_item_ids.contains(id));
            let explicit_ref = self.member_has_explicit_ref(source_refs, id)?;
            if live_owner || explicit_ref {
                insert_member(tx, seed_id, id, payload, live_owner, explicit_ref, false)?;
            }
            recent.insert(
                (
                    Reverse(episode.finalized_at),
                    Reverse(episode.created_at),
                    episode.id,
                ),
                (
                    member_content_digest(payload.as_bytes()),
                    live_owner,
                    explicit_ref,
                ),
            );
            if recent.len() > LEGACY_EPISODE_RECENT_CANDIDATES {
                recent.pop_last();
            }
        }
        tx.execute(
            "UPDATE episode_continuity_seeds SET candidate_payloads_json=?2 WHERE seed_id=?1",
            params![seed_id, serde_json::to_string(&candidates)?],
        )?;
        for ((_, _, id), (digest, live_owner, explicit_ref)) in recent {
            write_member(tx, seed_id, &id, &digest, live_owner, explicit_ref, true)?;
        }
        tracing::info!(
            agent_id,
            episode_count = count,
            payload_bytes = bytes,
            "captured upgrade Episode history incrementally"
        );
        Ok(())
    }
}

fn insert_member(
    tx: &Transaction<'_>,
    seed: &str,
    id: &str,
    payload: &str,
    live_owner: bool,
    explicit_ref: bool,
    recent: bool,
) -> Result<()> {
    write_member(
        tx,
        seed,
        id,
        &member_content_digest(payload.as_bytes()),
        live_owner,
        explicit_ref,
        recent,
    )
}

fn write_member(
    tx: &Transaction<'_>,
    seed: &str,
    id: &str,
    digest: &[u8; 64],
    live_owner: bool,
    explicit_ref: bool,
    recent: bool,
) -> Result<()> {
    let mut reasons = Vec::new();
    if live_owner {
        reasons.push("live_owner");
    }
    if explicit_ref {
        reasons.push("explicit_live_ref");
    }
    if recent {
        reasons.push("explicit_upgrade_recent_candidate");
    }
    reasons.sort_unstable();
    tx.execute(
        "INSERT INTO episode_continuity_members
         (seed_id,capture_phase,episode_id,content_digest,qualification_json)
         VALUES (?1,'upgrade',?2,?3,?4)
         ON CONFLICT(seed_id,capture_phase,episode_id) DO UPDATE
         SET qualification_json=excluded.qualification_json",
        params![
            seed,
            id,
            std::str::from_utf8(digest)?,
            member_qualification_json(&reasons)?
        ],
    )?;
    Ok(())
}

pub fn decode_context_episode_payload(payload: &str) -> Result<ContextEpisodeRecord> {
    serde_json::from_str(payload).context("decoding context episode payload from runtime db")
}

#[cfg(test)]
mod tests;
