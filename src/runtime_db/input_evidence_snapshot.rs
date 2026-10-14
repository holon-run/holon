//! D3 qualification and coherent input/evidence preparation. Every API borrows
//! the caller's existing transaction; no connection or file fallback exists.

use super::context_inputs::{ContextDurableRead, ContextScopeFacts};
use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionOutcomeRecord, WorkItemExecutionRecord,
    },
    runtime::resident_input_evidence::{
        project_tool_record, safe_command_preview, CommittedEvidenceRemoval,
        CommittedInputEvidenceDelta, EvidenceLocator, InputEvidenceCoverage,
        InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceScope,
        InputWindowCoverage, InputWindowSelector, PreparedInputEvidence, QualifiedInputEvidence,
        RecoverySourceAbsence, ResidentInputEvidence, ToolContextProjection, INPUT_EVIDENCE_POLICY,
        LEGACY_INPUT_WINDOW, MAX_COMMAND_PREVIEW_INPUT_BYTES, OVERSIZED_COMMAND_PREVIEW,
    },
    types::{
        AgentIdentityRecord, AgentRegistryStatus, BriefRecord, MessageEnvelope,
        ToolExecutionRecord, TranscriptEntry, TurnOwner, TurnRecord,
    },
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[path = "input_evidence_task_output.rs"]
mod task_output;
pub(in crate::runtime_db) use task_output::{
    capture_fixed_task_output_inputs_on, initialize_output_input_relations_tx,
    prepare_authorized_task_output_inputs_tx, read_authorized_task_input_tx,
};
pub(crate) use task_output::{OutputRecallInvalidation, TaskOutputInputRecall};

#[path = "input_evidence_attempt_turn.rs"]
mod attempt_turn;
use attempt_turn::AttemptTurnProof;

#[path = "input_evidence_dispatch_briefs.rs"]
mod dispatch_briefs;
pub(crate) use dispatch_briefs::{
    qualified_dispatch_brief_material_tx, qualified_dispatch_briefs_tx,
};

#[path = "input_evidence_execution.rs"]
mod execution;
#[path = "input_evidence_work_observation.rs"]
mod execution_work;
#[path = "input_evidence_task_cleanup.rs"]
mod task_cleanup;

const BOUNDARY_KEY: &str = "input_evidence/v1/fixed_upgrade";
const MAX_RECORDS: usize = 65_536;
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const TOOL_SUMMARY_CHARS: usize = 4096;

pub(super) const INPUT_EVIDENCE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS input_evidence_captures (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL, policy_version INTEGER NOT NULL CHECK(policy_version=1),
 includes_tails INTEGER NOT NULL CHECK(includes_tails IN (0,1)), metadata_digest TEXT NOT NULL,
 PRIMARY KEY(seed_id,capture_phase), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_eligibility (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL CHECK(capture_phase IN ('upgrade','first_import','legacy_import')),
 family TEXT NOT NULL, record_id TEXT NOT NULL, fingerprint_json TEXT NOT NULL,
 mandatory INTEGER NOT NULL CHECK(mandatory IN (0,1)),
 PRIMARY KEY(seed_id,capture_phase,family,record_id),
 FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_seed_sources (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL, source_message_id TEXT NOT NULL,
 metadata_json TEXT NOT NULL, content_digest TEXT NOT NULL,
 PRIMARY KEY(seed_id,source_message_id), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_seed_source_recovery (
 seed_id TEXT NOT NULL, source_message_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 metadata_json TEXT NOT NULL, content_digest TEXT NOT NULL,
 PRIMARY KEY(seed_id,source_message_id), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_current_sources (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 source_message_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 metadata_json TEXT NOT NULL, content_digest TEXT NOT NULL,
 PRIMARY KEY(agent_id,identity_incarnation,source_message_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_execution_seed_work (
 seed_id TEXT NOT NULL REFERENCES episode_continuity_seeds(seed_id), work_item_id TEXT NOT NULL,
 capture_phase TEXT NOT NULL, presence TEXT NOT NULL CHECK(presence IN ('present','absent')),
 PRIMARY KEY(seed_id,work_item_id));
CREATE TABLE IF NOT EXISTS input_evidence_execution_current_work (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0), work_item_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0), presence TEXT NOT NULL CHECK(presence IN ('present','absent','pruned')),
 PRIMARY KEY(agent_id,identity_incarnation,work_item_id));
CREATE INDEX IF NOT EXISTS idx_execution_protocol_source_message
 ON execution_protocol_attempts(agent_id,json_extract(payload_json,'$.source_message_id'));
CREATE TABLE IF NOT EXISTS input_evidence_seed_attempt_turns (
 seed_id TEXT NOT NULL, capture_phase TEXT NOT NULL, attempt_id TEXT NOT NULL,
 proof_json TEXT NOT NULL, PRIMARY KEY(seed_id,capture_phase,attempt_id),
 FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE INDEX IF NOT EXISTS idx_input_evidence_eligibility_lookup
 ON input_evidence_eligibility(seed_id,family,record_id);
CREATE TABLE IF NOT EXISTS input_evidence_current (
 agent_id TEXT NOT NULL, identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation>0),
 family TEXT NOT NULL, record_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 content_digest TEXT, fingerprint_json TEXT NOT NULL, attempt_turn_proof_json TEXT,
 availability TEXT NOT NULL CHECK(availability IN ('present','pruned')),
 qualification TEXT NOT NULL CHECK(qualification IN ('legacy_prepared','committed_writer')),
 seed_id TEXT, PRIMARY KEY(agent_id,identity_incarnation,family,record_id),
 FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id)
);
CREATE TABLE IF NOT EXISTS input_evidence_seed_recovery_current (
 seed_id TEXT NOT NULL, family TEXT NOT NULL, record_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0), fingerprint_json TEXT NOT NULL,
 origin_kind TEXT NOT NULL CHECK(origin_kind IN ('fixed_member','recovery_created','maintenance_created')),
 origin_attempt_id TEXT, command_kind TEXT, command_identity TEXT, command_hash TEXT, maintenance_proof_json TEXT,
 PRIMARY KEY(seed_id,family,record_id), FOREIGN KEY(seed_id) REFERENCES episode_continuity_seeds(seed_id),
 CHECK ((origin_kind='fixed_member' AND origin_attempt_id IS NULL AND command_kind IS NULL AND command_identity IS NULL AND command_hash IS NULL AND maintenance_proof_json IS NULL)
     OR (origin_kind='recovery_created' AND origin_attempt_id IS NOT NULL AND command_kind IS NOT NULL AND command_identity IS NOT NULL AND command_hash IS NOT NULL AND maintenance_proof_json IS NULL)
     OR (origin_kind='maintenance_created' AND origin_attempt_id IS NULL AND command_kind IS NULL AND command_identity IS NULL AND command_hash IS NULL AND maintenance_proof_json IS NOT NULL))
);
CREATE VIEW IF NOT EXISTS input_evidence_seed_membership AS
 SELECT e.seed_id,e.capture_phase,e.family,e.record_id,e.fingerprint_json,e.mandatory
 FROM input_evidence_eligibility e WHERE NOT EXISTS(
   SELECT 1 FROM input_evidence_seed_recovery_current r WHERE r.seed_id=e.seed_id AND r.family=e.family AND r.record_id=e.record_id)
 UNION ALL
 SELECT r.seed_id,'startup_recovery',r.family,r.record_id,r.fingerprint_json,1
 FROM input_evidence_seed_recovery_current r;
"#;

#[path = "input_evidence_maintenance.rs"]
pub(crate) mod maintenance;

#[path = "input_evidence_legacy_recovery.rs"]
pub(crate) mod legacy_recovery;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InputEvidenceSnapshotError {
    #[error("input evidence scope is retired")]
    RetiredScope,
    #[error("input evidence needs qualified preparation: {0}")]
    NeedsPreparation(String),
    #[error("input history limit {requested} exceeds fixed qualified limit {maximum}")]
    PolicyOutsideQualification { requested: usize, maximum: usize },
    #[error("input evidence resource exhausted: {0}")]
    ResourceExhausted(&'static str),
    #[error("input evidence canonical projection changed without qualified writer: {0}")]
    ProjectionChanged(String),
}
#[derive(Debug, Clone)]
pub(crate) struct InputWindowRequest {
    pub selector: InputWindowSelector,
    pub limit: usize,
}
#[derive(Debug, Clone)]
pub(crate) struct InputEvidenceLoadRequest {
    pub windows: Vec<InputWindowRequest>,
    /// Exact accepted input, current binding and validated recovery IDs from the
    /// owner, not arbitrary refs discovered in message body text.
    pub mandatory_roots: BTreeSet<Key>,
    /// Explicit planner preparation, never an ordinary lookup fallback.
    pub interrupted_sources: BTreeSet<String>,
    /// Named completion projection heads; owning input paths remain strict.
    pub selected_completion_brief_ids: BTreeSet<String>,
    pub record_limit: usize,
    pub byte_limit: usize,
}
#[derive(Debug, Clone)]
pub(crate) struct InputEvidenceCurrentSnapshot {
    pub facts: ContextScopeFacts,
    pub resident: ResidentInputEvidence,
}
#[derive(Debug, Clone)]
pub(crate) enum InputEvidenceBootstrapSnapshot {
    Current(InputEvidenceCurrentSnapshot),
    Retired {
        requested_scope: InputEvidenceScope,
        current_identity: Option<AgentIdentityRecord>,
    },
}

/// Full records are accepted only at the canonical SQL writer seam and are not
/// retained by the resident Tool domain.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CanonicalInputEvidence {
    Message(MessageEnvelope),
    Turn(TurnRecord),
    ToolExecution(ToolExecutionRecord),
    Brief(BriefRecord),
    Transcript(TranscriptEntry),
    Attempt(ExecutionAttempt),
    Outcome(ExecutionOutcomeRecord),
    WorkExecution {
        work_item_id: String,
        record: WorkItemExecutionRecord,
    },
}
impl CanonicalInputEvidence {
    fn key(&self) -> Key {
        match self {
            Self::Message(x) => Key::new(Family::Message, &x.id),
            Self::Turn(x) => Key::new(Family::Turn, &x.turn_id),
            Self::ToolExecution(x) => Key::new(Family::ToolExecution, &x.id),
            Self::Brief(x) => Key::new(Family::Brief, &x.id),
            Self::Transcript(x) => Key::new(Family::Transcript, &x.id),
            Self::Attempt(x) => Key::new(Family::Attempt, &x.attempt_id),
            Self::Outcome(x) => Key::new(Family::Outcome, &x.outcome_id),
            Self::WorkExecution { work_item_id, .. } => {
                Key::new(Family::WorkExecution, work_item_id)
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Fingerprint {
    content_hash: Option<String>,
    sequence: Option<u64>,
    turn_index: Option<u64>,
    created_at: String,
    owner_kind: Option<String>,
    owner_id: Option<String>,
    revision_marker: String,
    payload_bytes: usize,
    related_turn_id: Option<String>,
    /// Fixed non-Tool rows without a canonical hash need this separate body
    /// proof. Ordinary metadata reads never load that body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frozen_payload_digest: Option<String>,
}
impl Fingerprint {
    fn same_metadata(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.frozen_payload_digest = None;
        right.frozen_payload_digest = None;
        left == right
    }
}
#[derive(Debug, Clone)]
struct Binding {
    seed_id: Option<String>,
}
#[derive(Debug, Clone)]
struct Alias {
    revision: u64,
    digest: Option<String>,
    fingerprint: Fingerprint,
    present: bool,
    attempt_turn_proof: Option<AttemptTurnProof>,
}

/// Called once by v84 after the same-release v82/v83, under the migration lease. It
/// freezes IDs and canonical metadata. Selected non-Tool bodies without a
/// canonical content_hash are hashed; optional tool bodies/stdout are not read.
pub(super) fn capture_input_evidence_upgrade_tx(
    tx: &Transaction<'_>,
    at: DateTime<Utc>,
) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime_metadata WHERE key=?1)",
        [BOUNDARY_KEY],
        |row| row.get(0),
    )?;
    if exists {
        return Ok(());
    }
    let seeds = {
        let mut s = tx.prepare("SELECT seed_id FROM episode_continuity_seeds ORDER BY seed_id")?;
        let rows = s.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for seed in seeds {
        capture_input_evidence_seed_tx(tx, &seed, "upgrade", true)?;
    }
    tx.execute(
        "INSERT INTO runtime_metadata(key,value,created_at,updated_at) VALUES(?1,'1',?2,?2)",
        params![BOUNDARY_KEY, at.to_rfc3339()],
    )?;
    Ok(())
}
/// Explicit legacy seed creation/import only. A Fresh binding has no seed and
/// cannot call this actor. First State import extends mandatory closure only;
/// its optional window remains the fixed upgrade/explicit legacy boundary.
pub(super) fn capture_input_evidence_seed_tx(
    tx: &Transaction<'_>,
    seed_id: &str,
    phase: &str,
    include_tails: bool,
) -> Result<()> {
    ensure!(
        matches!(phase, "upgrade" | "first_import" | "legacy_import"),
        "invalid qualification phase"
    );
    let installed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='input_evidence_eligibility')",[],|r|r.get(0))?;
    if !installed {
        return Ok(());
    }
    let captured:Option<(i64,bool)>=tx.query_row("SELECT policy_version,includes_tails FROM input_evidence_captures WHERE seed_id=?1 AND capture_phase=?2",params![seed_id,phase],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((policy, tails)) = captured {
        ensure!(
            policy == INPUT_EVIDENCE_POLICY && tails == include_tails,
            "qualification checkpoint policy mismatch"
        );
        return Ok(());
    }
    ensure!(
        phase != "first_import" || !include_tails,
        "first import cannot extend optional history eligibility"
    );
    let (agent_id, roots_json): (String, String) = if phase == "first_import" {
        tx.query_row("SELECT s.agent_id,i.roots_json FROM episode_continuity_seeds s JOIN episode_continuity_imports i USING(seed_id) WHERE s.seed_id=?1",[seed_id],|r|Ok((r.get(0)?,r.get(1)?)))?
    } else {
        tx.query_row(
            "SELECT agent_id,roots_json FROM episode_continuity_seeds WHERE seed_id=?1",
            [seed_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
    };
    if roots_json.len() > MAX_METADATA_BYTES {
        return Err(InputEvidenceSnapshotError::ResourceExhausted("root metadata bytes").into());
    }
    let roots: serde_json::Value = serde_json::from_str(&roots_json)?;
    // Released partial schemas can legitimately lack a historical table. Freeze
    // that checkpoint's absence; normal preparation and later actors stay strict.
    let captured_tables = if phase == "upgrade" {
        let mut statement = tx.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Some(rows.collect::<rusqlite::Result<BTreeSet<_>>>()?)
    } else {
        None
    };
    let window_keys = |selector: &InputWindowSelector, limit: usize| {
        let family = window_query(selector).0;
        if captured_tables
            .as_ref()
            .is_some_and(|tables| !tables.contains(table(family).0))
        {
            Ok(Vec::new())
        } else {
            raw_window_keys(tx, &agent_id, selector, limit)
        }
    };
    let mut mandatory = BTreeSet::new();
    let mut mandatory_bytes = 0usize;
    if let Some(records) = roots
        .get("record_digests")
        .and_then(serde_json::Value::as_object)
    {
        for reference in records.keys() {
            if let Some((family, id)) = reference.split_once(':') {
                if let Ok(family) = Family::from_str(family) {
                    extend_metadata_selection(
                        &mut mandatory,
                        &mut mandatory_bytes,
                        [Key::new(family, id)],
                    )?;
                }
            }
        }
    }
    let mut selected = mandatory.clone();
    let mut selected_bytes = mandatory_bytes;
    if include_tails {
        extend_metadata_selection(
            &mut selected,
            &mut selected_bytes,
            window_keys(&InputWindowSelector::GlobalTurns, LEGACY_INPUT_WINDOW)?,
        )?;
        for selector in [
            InputWindowSelector::RecentBriefs,
            InputWindowSelector::RecentTools,
            InputWindowSelector::RecentTranscript,
            InputWindowSelector::TrustedOperator,
        ] {
            extend_metadata_selection(
                &mut selected,
                &mut selected_bytes,
                window_keys(
                    &selector,
                    if selector == InputWindowSelector::TrustedOperator {
                        1
                    } else {
                        LEGACY_INPUT_WINDOW
                    },
                )?,
            )?;
        }
        let mut owners = BTreeSet::new();
        if let Some(ids) = roots
            .get("work_item_ids")
            .and_then(serde_json::Value::as_array)
        {
            for id in ids.iter().filter_map(serde_json::Value::as_str) {
                owners.insert(id.to_owned());
            }
        }
        for id in owners {
            extend_metadata_selection(
                &mut selected,
                &mut selected_bytes,
                window_keys(
                    &InputWindowSelector::OwnerTurns {
                        owner: TurnOwner::WorkItem { work_item_id: id },
                    },
                    LEGACY_INPUT_WINDOW,
                )?,
            )?;
        }
        extend_metadata_selection(
            &mut selected,
            &mut selected_bytes,
            window_keys(
                &InputWindowSelector::OwnerTurns {
                    owner: TurnOwner::AgentLifecycle {
                        agent_id: agent_id.clone(),
                    },
                },
                LEGACY_INPUT_WINDOW,
            )?,
        )?;
    }
    let root_work_ids = roots
        .get("work_item_ids")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect();
    let (work_keys, execution_work_digest, execution_work_count, execution_work_bytes) =
        execution_work::capture_seed_tx(tx, seed_id, phase, &agent_id, &mandatory, &root_work_ids)?;
    selected.extend(work_keys);
    let selected = metadata_closure(tx, &agent_id, selected, captured_tables.as_ref())?;
    let selected_count = selected.len();
    let mut bytes = 0usize;
    let mut manifest = Vec::new();
    let mut fixed_body_bytes = 0usize;
    for key in selected {
        if captured_tables
            .as_ref()
            .is_some_and(|tables| !tables.contains(table(key.family).0))
        {
            continue;
        }
        let Some(mut fingerprint) = fingerprint_on(tx, &agent_id, &key)? else {
            continue;
        };
        let fixed_payload = if requires_frozen_body_digest(&key, &fingerprint) {
            if fingerprint.payload_bytes > MAX_METADATA_BYTES.saturating_sub(fixed_body_bytes) {
                return Err(InputEvidenceSnapshotError::ResourceExhausted(
                    "fixed input digest body bytes",
                )
                .into());
            }
            fixed_body_bytes += fingerprint.payload_bytes;
            let payload = payload_on(tx, &agent_id, &key)?.context("fixed input disappeared")?;
            fingerprint.frozen_payload_digest = Some(bytes_digest(payload.as_bytes()));
            Some(payload)
        } else {
            None
        };
        let attempt_proof_json = if key.family == Family::Attempt {
            let payload = fixed_payload
                .as_deref()
                .context("fixed Attempt digest body missing")?;
            let attempt: ExecutionAttempt = serde_json::from_str(payload)?;
            Some(serde_json::to_string(&attempt_turn::capture(
                tx, seed_id, phase, &attempt,
            )?)?)
        } else {
            None
        };
        let json = serde_json::to_string(&fingerprint)?;
        bytes = bytes
            .saturating_add(json.len())
            .saturating_add(attempt_proof_json.as_ref().map_or(0, String::len));
        if bytes > MAX_METADATA_BYTES {
            return Err(InputEvidenceSnapshotError::ResourceExhausted(
                "eligibility metadata bytes",
            )
            .into());
        }
        tx.execute("INSERT INTO input_evidence_eligibility(seed_id,capture_phase,family,record_id,fingerprint_json,mandatory) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",params![seed_id,phase,key.family.as_str(),key.id,json,mandatory.contains(&key)])?;
        let actual:String=tx.query_row("SELECT fingerprint_json FROM input_evidence_eligibility WHERE seed_id=?1 AND capture_phase=?2 AND family=?3 AND record_id=?4",params![seed_id,phase,key.family.as_str(),key.id],|r|r.get(0))?;
        ensure!(
            actual == json,
            "fixed evidence eligibility write/readback mismatch"
        );
        manifest.push((key, json, attempt_proof_json));
    }
    let (execution_digest, _, _) = execution::capture_sources_tx(
        tx,
        seed_id,
        phase,
        &agent_id,
        &mandatory,
        selected_count.saturating_add(execution_work_count),
        bytes.saturating_add(execution_work_bytes),
    )?;
    tx.execute("INSERT INTO input_evidence_captures(seed_id,capture_phase,policy_version,includes_tails,metadata_digest) VALUES(?1,?2,?3,?4,?5)",params![seed_id,phase,INPUT_EVIDENCE_POLICY,include_tails,bytes_digest(&serde_json::to_vec(&(manifest,execution_digest,execution_work_digest))?)])?;
    Ok(())
}

/// The exact same SQL transaction writes canonical evidence and registers its
/// actual readback. Neither command arguments nor notifications qualify data.
pub(crate) fn register_committed_input_evidence_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    records: &[CanonicalInputEvidence],
) -> Result<CommittedInputEvidenceDelta> {
    register_committed_input_evidence_inner_tx(tx, scope, records, None)
}

/// Minted before the original typed cleanup writes. It authorizes only its
/// qualified admission roots and finite derived terminal footprint.
pub(crate) struct ExistingAdmissionInputProof {
    scope: InputEvidenceScope,
    allowed: BTreeSet<Key>,
    derived: BTreeSet<Key>,
    derived_work_owners: BTreeMap<Key, String>,
    derived_task_messages: BTreeMap<Key, task_cleanup::DerivedTaskMessage>,
}
impl ExistingAdmissionInputProof {
    pub(crate) fn prepare_tx(
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        roots: BTreeSet<Key>,
        derived: BTreeSet<Key>,
    ) -> Result<Self> {
        require_original_input_recovery_scope(tx, scope)?;
        ensure!(
            derived.is_empty() || !roots.is_empty(),
            "cleanup derives evidence without an admitted root"
        );
        ensure!(
            roots.len().saturating_add(derived.len()) <= MAX_RECORDS
                && roots
                    .iter()
                    .chain(&derived)
                    .try_fold(0usize, |bytes, key| bytes.checked_add(key.id.len()))
                    .is_some_and(|bytes| bytes <= MAX_METADATA_BYTES),
            "cleanup footprint exceeds fixed input preparation bounds"
        );
        for key in &roots {
            ensure!(
                fingerprint_on(tx, &scope.agent_id, key)?.is_some(),
                "cleanup admission root is missing"
            );
            validate_existing_input_evidence_inner_tx(tx, scope, key)?;
        }
        for key in &derived {
            // An existing ID is still subject to exact original qualification.
            validate_existing_input_evidence_inner_tx(tx, scope, key)?;
        }
        Ok(Self {
            scope: scope.clone(),
            allowed: roots.union(&derived).cloned().collect(),
            derived,
            derived_work_owners: BTreeMap::new(),
            derived_task_messages: BTreeMap::new(),
        })
    }
    /// The same original transaction observed these qualified Work bodies
    /// before business SQL. This grants only their exact Brief/Tool footprint.
    pub(crate) fn prepare_from_work_tx(
        tx: &Transaction<'_>,
        work: &super::work_snapshot::WorkWriteProof,
        roots: BTreeSet<Key>,
        derived: BTreeMap<Key, String>,
    ) -> Result<Self> {
        ensure!(
            !roots.is_empty() || !derived.is_empty(),
            "cleanup has no existing admission"
        );
        ensure!(
            roots.len().saturating_add(derived.len()) <= MAX_RECORDS,
            "Work cleanup footprint exceeds record bound"
        );
        let metadata_bytes = roots
            .iter()
            .map(|key| key.id.len())
            .chain(
                derived
                    .iter()
                    .map(|(key, owner)| key.id.len().saturating_add(owner.len())),
            )
            .try_fold(0usize, usize::checked_add)
            .context("Work cleanup footprint size overflow")?;
        ensure!(
            metadata_bytes <= MAX_METADATA_BYTES,
            "Work cleanup footprint exceeds byte bound"
        );
        let scope = work.scope();
        let admitted = work.admitted_work_ids();
        let mut proof = Self::prepare_tx(tx, scope, roots, BTreeSet::new())?;
        for (key, owner) in &derived {
            ensure!(
                matches!(key.family, Family::Brief | Family::ToolExecution)
                    && !key.id.is_empty()
                    && admitted.contains(owner),
                "derived cleanup input is outside the original Work admission"
            );
            work.validate_admission_tx(tx, owner)?;
            validate_existing_input_evidence_inner_tx(tx, scope, key)?;
            if key.family == Family::ToolExecution {
                // Retrying an original Work cleanup must not materialize a
                // Tool's full stdout merely to validate its owner.
                let existing: Option<(Option<String>, Option<String>)> = tx.query_row(
                    "SELECT work_item_id,json_extract(payload_json,'$.work_item_id') FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
                    params![scope.agent_id,key.id],
                    |row| Ok((row.get(0)?,row.get(1)?)),
                ).optional()?;
                if let Some((normalized, payload)) = existing {
                    ensure!(
                        normalized.as_deref() == Some(owner.as_str()) && payload == normalized,
                        "existing derived input belongs to another Work"
                    );
                }
            } else if let Some(payload) = payload_on(tx, &scope.agent_id, key)? {
                let existing = decode_canonical(key, &payload)?;
                ensure!(
                    canonical_input_work_id(&existing) == Some(owner.as_str()),
                    "existing derived input belongs to another Work"
                );
            }
            proof.allowed.insert(key.clone());
            proof.derived.insert(key.clone());
        }
        proof.derived_work_owners = derived;
        Ok(proof)
    }
    /// Add only the terminal fact of an original admitted execution command,
    /// before that command writes. The initial roots were frozen before cleanup.
    pub(crate) fn allow_execution_recovery_tx(
        &mut self,
        tx: &Transaction<'_>,
        command: &crate::domain::execution_protocol::ExecutionProtocolCommand,
    ) -> Result<()> {
        use crate::domain::execution_protocol::ExecutionProtocolCommand as Command;
        let (attempt_id, outcome_id) = match command {
            Command::Interrupt(x) => (&x.attempt_id, &x.outcome_id),
            Command::RecoverInterruptedTaskResultClaim(x) => (&x.attempt_id, &x.outcome_id),
            Command::RecoverUnadvancedTaskResultClaim(x) => (&x.attempt_id, &x.outcome_id),
            _ => anyhow::bail!("cleanup cannot grant an ordinary execution command"),
        };
        self.validate_scope_tx(tx, &self.scope)?;
        ensure!(
            self.allowed
                .contains(&Key::new(Family::Attempt, attempt_id)),
            "cleanup recovery predecessor was not originally qualified"
        );
        let key = Key::new(Family::Outcome, outcome_id);
        validate_existing_input_evidence_inner_tx(tx, &self.scope, &key)?;
        let mut allowed = self.allowed.clone();
        allowed.insert(key.clone());
        ensure!(
            allowed.len() <= MAX_RECORDS
                && allowed
                    .iter()
                    .try_fold(0usize, |bytes, key| bytes.checked_add(key.id.len()))
                    .is_some_and(|bytes| bytes <= MAX_METADATA_BYTES),
            "cleanup recovery outcome exceeds fixed preparation bounds"
        );
        self.allowed = allowed;
        self.derived.insert(key);
        Ok(())
    }
    pub(crate) fn validate_scope_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        ensure!(
            scope == &self.scope,
            "cleanup proof differs from its original namespace"
        );
        require_original_input_recovery_scope(tx, scope)?;
        Ok(())
    }
    pub(crate) fn validate_key_tx(&self, tx: &Transaction<'_>, key: &Key) -> Result<()> {
        self.validate_scope_tx(tx, &self.scope)?;
        ensure!(
            self.allowed.contains(key),
            "input is outside the original cleanup footprint"
        );
        ensure!(
            self.derived.contains(key) || fingerprint_on(tx, &self.scope.agent_id, key)?.is_some(),
            "cleanup cannot recreate a missing admission root"
        );
        validate_existing_input_evidence_inner_tx(tx, &self.scope, key)
    }
}

pub(crate) fn register_committed_cleanup_input_evidence_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    records: &[CanonicalInputEvidence],
    proof: &ExistingAdmissionInputProof,
) -> Result<CommittedInputEvidenceDelta> {
    register_committed_input_evidence_inner_tx(tx, scope, records, Some(proof))
}

fn canonical_input_work_id(record: &CanonicalInputEvidence) -> Option<&str> {
    match record {
        CanonicalInputEvidence::Brief(brief) => brief.work_item_id.as_deref(),
        CanonicalInputEvidence::ToolExecution(tool) => tool.work_item_id.as_deref(),
        _ => None,
    }
}

fn register_committed_input_evidence_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    records: &[CanonicalInputEvidence],
    cleanup: Option<&ExistingAdmissionInputProof>,
) -> Result<CommittedInputEvidenceDelta> {
    if let Some(proof) = cleanup {
        proof.validate_scope_tx(tx, scope)?;
        ensure!(
            records
                .iter()
                .all(|record| proof.allowed.contains(&record.key())),
            "committed facts exceed the original cleanup footprint"
        );
    } else {
        require_current_scope(tx, scope)?;
    }
    let binding = binding_on(tx, scope)?;
    let supplied_keys = records
        .iter()
        .map(CanonicalInputEvidence::key)
        .collect::<BTreeSet<_>>();
    let mut actual_records = records
        .iter()
        .map(std::borrow::Cow::Borrowed)
        .collect::<Vec<_>>();
    for key in affected_projection_keys_tx(tx, scope, records)? {
        if supplied_keys.contains(&key) {
            continue;
        }
        // Refreshing projection metadata never grants a previously unqualified
        // canonical record membership in this incarnation.
        if is_existing_member_on(tx, scope, &binding, &key)? {
            let payload = payload_on(tx, &scope.agent_id, &key)?
                .context("qualified related projection canonical row disappeared")?;
            actual_records.push(std::borrow::Cow::Owned(decode_canonical(&key, &payload)?));
        }
    }
    let mut final_records = BTreeMap::new();
    for supplied in &actual_records {
        let supplied = supplied.as_ref();
        let key = supplied.key();
        let payload = payload_on(tx, &scope.agent_id, &key)?
            .context("committed input evidence is missing")?;
        let actual = decode_canonical(&key, &payload)?;
        ensure!(
            &actual == supplied,
            "input evidence commit differs from canonical SQL readback"
        );
        if let Some(owner) = cleanup.and_then(|proof| proof.derived_work_owners.get(&key)) {
            ensure!(
                canonical_input_work_id(&actual) == Some(owner.as_str()),
                "committed derived input differs from its original Work admission"
            );
        }
        if let Some(proof) = cleanup {
            proof.validate_derived_task_message_tx(tx, &actual)?;
        }
        let fingerprint = fingerprint_on(tx, &scope.agent_id, &key)?
            .context("committed evidence metadata disappeared")?;
        let digest = bytes_digest(payload.as_bytes());
        if let Some(hash) = fingerprint.content_hash.as_deref() {
            ensure!(
                hash == super::evidence::content_hash(&payload),
                "evidence normalized content_hash differs from actual payload"
            )
        }
        let attempt_turn_proof = match &actual {
            CanonicalInputEvidence::Attempt(attempt) => {
                Some(attempt_turn::for_current(tx, scope, &binding, attempt)?)
            }
            _ => None,
        };
        let revision = upsert_alias_tx(
            tx,
            scope,
            &key,
            &fingerprint,
            Some(&digest),
            true,
            attempt_turn_proof.as_ref(),
            "committed_writer",
            binding.seed_id.as_deref(),
        )?;
        let record = prepare_record_on(tx, &scope.agent_id, &key, Some(actual))?;
        final_records.insert(
            key.clone(),
            Arc::new(qualified(
                scope,
                key,
                revision,
                digest,
                fingerprint,
                record,
                attempt_turn_proof,
            )?),
        );
    }
    let mut turn_transcript_keys = BTreeMap::new();
    let mut message_turn_keys = BTreeMap::new();
    for record in final_records.values() {
        super::task_snapshot::register_message_source_tx(tx, record)?;
        if let PreparedInputEvidence::Message(message) = &record.record {
            message_turn_keys.insert(
                message.id.clone(),
                actual_input_turn_on(tx, &scope.agent_id, &message.id)?,
            );
        }
        if let PreparedInputEvidence::Turn(turn) = &record.record {
            turn_transcript_keys.insert(
                turn.turn_id.clone(),
                qualified_turn_transcript_keys(tx, scope, &binding, &turn.turn_id, MAX_RECORDS)?,
            );
        }
    }
    Ok(CommittedInputEvidenceDelta {
        output_recall_invalidation: None,
        execution: execution::register_tx(tx, scope, &binding, records)?,
        records: final_records.into_values().collect(),
        removals: Vec::new(),
        turn_transcript_keys,
        message_turn_keys,
    })
}

/// Exact surviving projection keys touched by the caller's original transaction,
/// e.g. retention removal. This refreshes only existing scope membership; passing
/// a canonical record directly would instead represent an actual writer grant.
pub(crate) fn refresh_related_input_evidence_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &[Key],
) -> Result<CommittedInputEvidenceDelta> {
    require_current_scope(tx, scope)?;
    let binding = binding_on(tx, scope)?;
    let mut selected = BTreeSet::new();
    let mut bytes = 0usize;
    extend_metadata_selection(&mut selected, &mut bytes, keys.iter().cloned())?;
    let mut records = Vec::new();
    for key in selected {
        ensure!(
            matches!(key.family, Family::Message | Family::Turn),
            "related projection refresh only accepts Message/Turn keys"
        );
        if is_existing_member_on(tx, scope, &binding, &key)? {
            let payload = payload_on(tx, &scope.agent_id, &key)?
                .context("qualified related projection canonical row disappeared")?;
            records.push(decode_canonical(&key, &payload)?);
        }
    }
    register_committed_input_evidence_tx(tx, scope, &records)
}
fn is_existing_member_on(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    key: &Key,
) -> Result<bool> {
    Ok(match alias_on(tx, scope, key)? {
        Some(alias) => alias.present,
        None => match binding.seed_id.as_deref() {
            Some(seed) => {
                legacy_recovery::validate_seed_overlay_on(tx, seed, key)?;
                tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM input_evidence_seed_membership WHERE seed_id=?1 AND family=?2 AND record_id=?3)",
                params![seed, key.family.as_str(), key.id], |row| row.get::<_, bool>(0),
            )?
            }
            None => false,
        },
    })
}

/// A startup cleanup can advance only a record already in the current scope;
/// rewriting same-name historical rows is not a grant to a Fresh successor.
pub(crate) fn existing_input_evidence_member_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
) -> Result<bool> {
    require_current_scope(tx, scope)?;
    is_existing_member_on(tx, scope, &binding_on(tx, scope)?, key)
}

/// Original exclusive startup cleanup retains only fixed/current members of
/// the same non-deleted incarnation. It never enables ordinary activation.
pub(crate) fn existing_cleanup_input_evidence_member_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
) -> Result<bool> {
    require_original_input_recovery_scope(tx, scope)?;
    is_existing_member_on(tx, scope, &binding_on(tx, scope)?, key)
}

/// Validate an existing row before any scoped rewrite or append retry. A no-op
/// on an old incarnation is not a current-incarnation canonical write.
pub(crate) fn validate_existing_input_evidence_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
) -> Result<()> {
    require_current_scope(tx, scope)?;
    validate_existing_input_evidence_inner_tx(tx, scope, key)
}
fn validate_existing_input_evidence_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
) -> Result<()> {
    // The tombstone is authoritative even after retention removed the body.
    // An absent canonical row alone does not make this a fresh evidence ID.
    let alias = alias_on(tx, scope, key)?;
    if let Some(alias) = &alias {
        ensure!(
            alias.present,
            "scoped writer cannot reuse a pruned evidence ID"
        );
    }
    let (table, id) = table(key.family);
    let agent: Option<String> = tx
        .query_row(
            &format!("SELECT agent_id FROM {table} WHERE {id}=?1"),
            [&key.id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(agent) = agent else { return Ok(()) };
    ensure!(
        agent == scope.agent_id,
        "existing evidence ID belongs to another agent"
    );
    let binding = binding_on(tx, scope)?;
    let fingerprint = fingerprint_on(tx, &scope.agent_id, key)?
        .context("existing evidence metadata disappeared")?;
    if let Some(alias) = alias {
        ensure!(
            alias.fingerprint == fingerprint,
            "existing evidence changed without an acknowledged scoped writer"
        );
        // Tool projection trusts its canonical normalized hash and never
        // materializes full stdout. Every other alias validates its raw body.
        if key.family != Family::ToolExecution {
            validate_alias_body(
                tx,
                &scope.agent_id,
                key,
                &fingerprint,
                alias.digest.as_deref(),
            )?;
        }
    } else {
        require_eligible(tx, &binding, key, &fingerprint)?;
    }
    Ok(())
}

/// Only the finite typed footprint of the acknowledged writer is inspected.
/// It refreshes existing aliases; it is never an adoption scan.
fn affected_projection_keys_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    records: &[CanonicalInputEvidence],
) -> Result<BTreeSet<Key>> {
    affected_projection_keys_for_agent_tx(tx, &scope.agent_id, records)
}
fn affected_projection_keys_for_agent_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    records: &[CanonicalInputEvidence],
) -> Result<BTreeSet<Key>> {
    let mut keys = BTreeSet::new();
    let mut bytes = 0usize;
    for record in records {
        let turn_id = match record {
            CanonicalInputEvidence::Message(x) => {
                if let Some(turn) = actual_input_turn_on(tx, agent_id, &x.id)? {
                    extend_metadata_selection(
                        &mut keys,
                        &mut bytes,
                        [Key::new(Family::Turn, turn)],
                    )?;
                }
                // Message.turn_id is only a preallocation/replay hint. Only
                // the canonical assignment proves a Turn relationship.
                None
            }
            CanonicalInputEvidence::Turn(x) => {
                for message_id in &x.input_message_ids {
                    extend_metadata_selection(
                        &mut keys,
                        &mut bytes,
                        [Key::new(Family::Message, message_id)],
                    )?;
                    let assigned:Option<String> = tx.query_row(
                        "SELECT turn_id FROM conversation_input_assignments WHERE agent_id=?1 AND message_id=?2",
                        params![agent_id,message_id],|r|r.get(0)).optional()?;
                    if let Some(turn) = assigned {
                        extend_metadata_selection(
                            &mut keys,
                            &mut bytes,
                            [Key::new(Family::Turn, turn)],
                        )?;
                    }
                }
                Some(x.turn_id.as_str())
            }
            CanonicalInputEvidence::Transcript(x) => {
                x.data.get("turn_id").and_then(serde_json::Value::as_str)
            }
            CanonicalInputEvidence::ToolExecution(x) => x.turn_id.as_deref(),
            CanonicalInputEvidence::Brief(x) => x.turn_id.as_deref(),
            _ => None,
        };
        if let Some(turn) = turn_id {
            extend_metadata_selection(&mut keys, &mut bytes, [Key::new(Family::Turn, turn)])?;
        }
    }
    let mut related = keys.clone();
    related.extend(
        records
            .iter()
            .map(CanonicalInputEvidence::key)
            .filter(|key| matches!(key.family, Family::Message | Family::Turn)),
    );
    for key in related {
        let path = if key.family == Family::Message {
            "$.source_message_id"
        } else if key.family == Family::Turn {
            "$.turn_id"
        } else {
            continue;
        };
        let mut statement=tx.prepare(&format!("SELECT attempt_id FROM execution_protocol_attempts WHERE agent_id=?1 AND json_extract(payload_json,'{path}')=?2 ORDER BY attempt_id LIMIT ?3"))?;
        let attempts = statement
            .query_map(params![agent_id, key.id, MAX_RECORDS as i64 + 1], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in attempts {
            extend_metadata_selection(&mut keys, &mut bytes, [Key::new(Family::Attempt, id)])?;
        }
    }
    Ok(keys)
}
/// Activation/OwnerChange/PolicyExpansion only: selected legacy IDs already
/// belong to the frozen eligibility set. This never extends that set by scanning
/// current agent history and never rewrites canonical payloads.
pub(crate) fn prepare_input_evidence_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
) -> Result<InputEvidenceBootstrapSnapshot> {
    snapshot_tx(tx, scope, request, true, false)
}
/// A fresh coherent read transaction, requiring previously prepared membership.
#[cfg(test)]
pub(crate) fn load_input_evidence_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
) -> Result<InputEvidenceBootstrapSnapshot> {
    snapshot_tx(tx, scope, request, false, false)
}
/// Only a positive original receipt resolver may prepare a Deleting scope.
/// This reads existing qualification; it cannot adopt an unrelated incarnation.
pub(crate) fn prepare_original_input_recovery_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
) -> Result<InputEvidenceBootstrapSnapshot> {
    snapshot_tx(tx, scope, request, true, true)
}
fn snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
    prepare: bool,
    original_recovery: bool,
) -> Result<InputEvidenceBootstrapSnapshot> {
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    if !facts.identity.as_ref().is_some_and(|x| {
        x.agent_id == scope.agent_id
            && x.incarnation == scope.identity_incarnation
            && (x.status == AgentRegistryStatus::Active
                || (original_recovery && x.status == AgentRegistryStatus::Deleting))
    }) {
        return Ok(InputEvidenceBootstrapSnapshot::Retired {
            requested_scope: scope.clone(),
            current_identity: facts.identity,
        });
    }
    let binding = binding_on(tx, scope)?;
    ensure!(
        request.record_limit > 0 && request.byte_limit > 0,
        "input evidence budget is empty"
    );
    let mut windows = Vec::new();
    let mut mandatory_roots = request.mandatory_roots.clone();

    mandatory_roots.extend(qualified_live_protocol_keys(
        tx,
        scope,
        &binding,
        request.record_limit.min(MAX_RECORDS),
    )?);
    let mut selected = mandatory_roots.clone();
    selected.extend(
        request
            .selected_completion_brief_ids
            .iter()
            .map(|id| Key::new(Family::Brief, id)),
    );
    for requested in &request.windows {
        if requested.limit > requested.selector.maximum_candidates() {
            return Err(InputEvidenceSnapshotError::PolicyOutsideQualification {
                requested: requested.limit,
                maximum: requested.selector.maximum_candidates(),
            }
            .into());
        }
        let mut keys = qualified_window_keys(
            tx,
            scope,
            &binding,
            &requested.selector,
            requested.limit.saturating_add(1),
        )?;
        let exhaustive = keys.len() <= requested.limit;
        keys.truncate(requested.limit);
        selected.extend(keys.iter().cloned());
        windows.push(InputWindowCoverage {
            selector: requested.selector.clone(),
            limit: requested.limit,
            keys,
            is_exhaustive: exhaustive,
        });
    }
    let mut execution_sources = mandatory_roots
        .iter()
        .filter(|key| key.family == Family::Message)
        .map(|key| key.id.clone())
        .collect::<BTreeSet<_>>();
    execution_sources.extend(request.interrupted_sources.iter().cloned());
    for key in mandatory_roots
        .iter()
        .filter(|key| key.family == Family::Attempt)
    {
        let source:Option<String> = tx.query_row("SELECT json_extract(payload_json,'$.source_message_id') FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",params![scope.agent_id,key.id],|r|r.get(0)).optional()?.flatten();
        if let Some(source) = source {
            execution_sources.insert(source);
        }
    }
    let mut provider_lineage_messages = execution_sources.clone();
    let mut execution_coverage = execution::load_coverage_tx(
        tx,
        scope,
        &binding,
        &execution_sources,
        request.record_limit,
        request.byte_limit,
    )?;
    execution_coverage.work_items =
        execution_work::load_tx(tx, scope, &binding, &mandatory_roots, &execution_sources)?;
    let mut execution_bodies = execution_coverage.required_body_keys(&mandatory_roots);
    execution_bodies
        .extend(execution_coverage.required_interrupted_body_keys(&request.interrupted_sources)?);
    execution_bodies.extend(
        execution_coverage
            .work_items
            .iter()
            .filter(|(_, x)| {
                x.presence
                    == crate::runtime::resident_input_evidence::ExecutionWorkPresence::Present
            })
            .map(|(id, _)| Key::new(Family::WorkExecution, id)),
    );
    mandatory_roots.extend(execution_bodies.iter().cloned());
    selected.extend(execution_bodies);
    use crate::runtime::completion_sources::{completion_references, CompletionInputPath};
    let mut pending = VecDeque::from_iter(selected.iter().cloned().map(|key| {
        let path = if key.family == Family::Brief
            && request.selected_completion_brief_ids.contains(&key.id)
        {
            CompletionInputPath::Head(key.id.clone())
        } else {
            CompletionInputPath::Strict
        };
        (key, 0usize, path)
    }));
    // Window membership is an independent strict owning path, even when a
    // named completion head or Transcript also occurs in that window.
    pending.extend(
        mandatory_roots
            .iter()
            .cloned()
            .chain(
                windows
                    .iter()
                    .flat_map(|window| window.keys.iter().cloned()),
            )
            .map(|key| (key, 0, CompletionInputPath::Strict)),
    );
    let mut expanded_paths = BTreeSet::new();
    let mut completion_source_observations = Vec::new();
    let mut records: BTreeMap<Key, Arc<QualifiedInputEvidence>> = BTreeMap::new();
    let mut turn_transcript_keys = BTreeMap::new();
    let mut message_turn_keys = BTreeMap::new();
    let mut unavailable = BTreeSet::new();
    let mut recovery_source_absences = BTreeMap::new();
    let mut recovery_lineage_messages = BTreeSet::new();
    // Only qualified recovery Messages can name another lineage source. Its
    // body must qualify before that source's complete index is acquired.
    let mut provider_sources =
        BTreeMap::<String, (String, crate::types::TurnTerminalKind, usize)>::new();
    let mut expanded_provider_sources = BTreeSet::new();
    let mut bytes = 0usize;
    while let Some((key, depth, path)) = pending.pop_front() {
        if !expanded_paths.insert((key.clone(), path.clone())) {
            continue;
        }
        if expanded_paths.len() > request.record_limit.min(MAX_RECORDS).saturating_mul(3) {
            return Err(InputEvidenceSnapshotError::ResourceExhausted("reference paths").into());
        }
        if depth > MAX_DEPTH {
            return Err(InputEvidenceSnapshotError::ResourceExhausted("reference depth").into());
        }
        if let Some(record) = records.get(&key) {
            let head = match &path {
                CompletionInputPath::Strict => None,
                CompletionInputPath::Head(_) => Some(record.as_ref()),
                CompletionInputPath::Transcript(id) => {
                    records.get(&Key::new(Family::Brief, id)).map(Arc::as_ref)
                }
            };
            if !matches!(path, CompletionInputPath::Strict) {
                for proof in super::work_snapshot::completion_source_observations_tx(
                    tx,
                    scope,
                    &request.selected_completion_brief_ids,
                    &key,
                )? {
                    if !completion_source_observations.contains(&proof) {
                        completion_source_observations.push(proof);
                    }
                }
            }
            for (child, child_path) in completion_references(
                record,
                &path,
                head,
                &completion_source_observations,
                record.references(),
            )? {
                selected.insert(child.clone());
                pending.push_back((child, depth + 1, child_path));
            }
            continue;
        }
        if unavailable.contains(&key) {
            continue;
        }
        if depth > MAX_DEPTH
            || records.len() + unavailable.len() >= request.record_limit.min(MAX_RECORDS)
        {
            return Err(InputEvidenceSnapshotError::ResourceExhausted("record count/depth").into());
        }
        let Some(fingerprint) = fingerprint_on(tx, &scope.agent_id, &key)? else {
            if key.family == Family::WorkExecution && execution_coverage.work_items.get(&key.id)
                .is_some_and(|x| x.presence == crate::runtime::resident_input_evidence::ExecutionWorkPresence::Absent) {
                unavailable.insert(key);
                continue;
            }
            if alias_on(tx, scope, &key)?.is_some_and(|x| !x.present) {
                unavailable.insert(key);
                continue;
            }
            return Err(InputEvidenceSnapshotError::NeedsPreparation(format!(
                "{}:{} unavailable without tombstone",
                key.family.as_str(),
                key.id
            ))
            .into());
        };
        let alias = alias_on(tx, scope, &key)?;
        let mut fixed_body_digests = BTreeSet::new();
        if let Some(alias) = alias.as_ref() {
            ensure!(
                alias.present,
                "pruned evidence reappeared without committed writer"
            );
            if alias.fingerprint != fingerprint {
                return Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into());
            }
        } else {
            if !prepare {
                return Err(InputEvidenceSnapshotError::NeedsPreparation(key.id.clone()).into());
            }
            fixed_body_digests = require_eligible_metadata(tx, &binding, &key, &fingerprint)?;
        }
        if key.family != Family::ToolExecution
            && fingerprint.payload_bytes > request.byte_limit.saturating_sub(bytes)
        {
            return Err(
                InputEvidenceSnapshotError::ResourceExhausted("prepared body bytes").into(),
            );
        }
        // One materialization per selected body, reused for decode and digest.
        let payload = if key.family == Family::ToolExecution {
            None
        } else {
            Some(payload_on(tx, &scope.agent_id, &key)?.context("prepared evidence disappeared")?)
        };
        let actual_digest = match payload.as_ref() {
            Some(payload) => bytes_digest(payload.as_bytes()),
            None => normalized_payload_digest(
                fingerprint
                    .content_hash
                    .as_deref()
                    .context("Tool projection needs canonical normalized content hash")?,
            )?,
        };
        if key.family != Family::ToolExecution {
            validate_canonical_payload_digest(&key, &fingerprint, &actual_digest)?;
            if alias.is_none() && requires_frozen_body_digest(&key, &fingerprint) {
                validate_fixed_body_digest(&key, &fixed_body_digests, &actual_digest)?;
            }
        }
        let canonical = payload
            .as_ref()
            .map(|payload| decode_canonical(&key, payload))
            .transpose()?;
        let attempt_turn_proof = match canonical.as_ref() {
            Some(CanonicalInputEvidence::Attempt(attempt)) => {
                Some(attempt_turn::for_current(tx, scope, &binding, attempt)?)
            }
            _ => None,
        };
        let revision = if let Some(alias) = alias {
            ensure!(
                alias.digest.as_deref() == Some(actual_digest.as_str()),
                "prepared evidence raw/normalized digest mismatch"
            );
            if alias.attempt_turn_proof != attempt_turn_proof {
                if !prepare {
                    return Err(InputEvidenceSnapshotError::NeedsPreparation(format!(
                        "Attempt Turn relation {} advanced",
                        key.id
                    ))
                    .into());
                }
                upsert_alias_tx(
                    tx,
                    scope,
                    &key,
                    &fingerprint,
                    Some(&actual_digest),
                    true,
                    attempt_turn_proof.as_ref(),
                    "legacy_prepared",
                    binding.seed_id.as_deref(),
                )?
            } else {
                alias.revision
            }
        } else {
            upsert_alias_tx(
                tx,
                scope,
                &key,
                &fingerprint,
                Some(&actual_digest),
                true,
                attempt_turn_proof.as_ref(),
                "legacy_prepared",
                binding.seed_id.as_deref(),
            )?
        };
        let record = prepare_record_on(tx, &scope.agent_id, &key, canonical)?;
        let digest = actual_digest;
        let qualified = Arc::new(qualified(
            scope,
            key.clone(),
            revision,
            digest,
            fingerprint,
            record,
            attempt_turn_proof,
        )?);
        bytes = bytes.saturating_add(qualified.resident_bytes);
        if bytes > request.byte_limit {
            return Err(InputEvidenceSnapshotError::ResourceExhausted("resident bytes").into());
        }
        let mut refs = qualified.references();
        if let PreparedInputEvidence::Message(message) = &qualified.record {
            let assigned = actual_input_turn_on(tx, &scope.agent_id, &message.id)?;
            if let Some(turn) = assigned.as_ref() {
                refs.insert(Key::new(Family::Turn, turn));
            }
            message_turn_keys.insert(message.id.clone(), assigned);
            if let Some((turn, source, terminal)) = provider_lineage_messages
                .contains(&message.id)
                .then(|| crate::runtime::provider_recovery::continuity_sources(message))
                .and_then(|source| source.ok().flatten())
            {
                recovery_lineage_messages.insert(message.id.clone());
                // Asserted IDs are observed only after their incoming Message
                // qualified, on this original coherent transaction.
                let (children, absent) = recovery_source_children_tx(
                    tx,
                    scope,
                    &qualified,
                    &turn,
                    &source,
                    terminal,
                    &mut recovery_source_absences,
                    request,
                )?;
                if !absent {
                    provider_lineage_messages.insert(source.clone());
                    if let Some((previous_turn, previous_terminal, _)) =
                        provider_sources.get(&source)
                    {
                        ensure!(
                            previous_turn == &turn && previous_terminal == &terminal,
                            "provider recovery lineage source has conflicting Turn facts"
                        );
                    } else {
                        provider_sources
                            .insert(source.clone(), (turn.clone(), terminal, depth + 1));
                    }
                }
                for child in children {
                    mandatory_roots.insert(child.clone());
                    refs.insert(child);
                }
            }
        }
        if let PreparedInputEvidence::Turn(turn) = &qualified.record {
            let keys = qualified_turn_transcript_keys(
                tx,
                scope,
                &binding,
                &turn.turn_id,
                request.record_limit.min(MAX_RECORDS),
            )?;
            refs.extend(keys.iter().cloned());
            turn_transcript_keys.insert(turn.turn_id.clone(), keys);
        }
        let head = match &path {
            CompletionInputPath::Strict => None,
            CompletionInputPath::Head(_) => Some(qualified.as_ref()),
            CompletionInputPath::Transcript(id) => {
                records.get(&Key::new(Family::Brief, id)).map(Arc::as_ref)
            }
        };
        if !matches!(path, CompletionInputPath::Strict) {
            for proof in super::work_snapshot::completion_source_observations_tx(
                tx,
                scope,
                &request.selected_completion_brief_ids,
                &key,
            )? {
                if !completion_source_observations.contains(&proof) {
                    completion_source_observations.push(proof);
                }
            }
        }
        for (child, child_path) in completion_references(
            &qualified,
            &path,
            head,
            &completion_source_observations,
            refs,
        )? {
            selected.insert(child.clone());
            pending.push_back((child, depth + 1, child_path));
        }
        records.insert(key, qualified);
        loop {
            let ready_sources = provider_sources
                .iter()
                .filter(|(source, _)| {
                    !expanded_provider_sources.contains(*source)
                        && records.contains_key(&Key::new(Family::Message, *source))
                })
                .map(|(source, facts)| (source.clone(), facts.clone()))
                .collect::<Vec<_>>();
            if ready_sources.is_empty() {
                break;
            }
            for (source, (turn_id, terminal, source_depth)) in ready_sources {
                if source_depth > MAX_DEPTH {
                    return Err(InputEvidenceSnapshotError::ResourceExhausted(
                        "provider lineage depth",
                    )
                    .into());
                }
                let source_key = Key::new(Family::Message, &source);
                let source_record = records
                    .get(&source_key)
                    .context("qualified lineage source disappeared")?;
                let PreparedInputEvidence::Message(source_message) = &source_record.record else {
                    anyhow::bail!("provider lineage source key has a different family");
                };
                let turn = ContextDurableRead::new(tx, &scope.agent_id)
                    .turn_by_id(&turn_id)?
                    .context("provider recovery source Turn missing")?;
                ensure!(
                    source_message.agent_id == scope.agent_id
                        && turn.agent_id == scope.agent_id
                        && message_turn_keys
                            .get(&source)
                            .and_then(|turn| turn.as_deref())
                            == Some(turn_id.as_str())
                        && turn
                            .trigger
                            .as_ref()
                            .and_then(|trigger| trigger.message_id.as_deref())
                            == Some(source.as_str())
                        && turn.terminal.as_ref().map(|summary| summary.kind) == Some(terminal),
                    "provider recovery lineage source lacks its actual terminal Turn assignment"
                );
                if let Ok(Some((prior_turn, prior_source, prior_terminal))) =
                    crate::runtime::provider_recovery::continuity_sources(source_message)
                {
                    recovery_lineage_messages.insert(source_message.id.clone());
                    let (children, absent) = recovery_source_children_tx(
                        tx,
                        scope,
                        source_record,
                        &prior_turn,
                        &prior_source,
                        prior_terminal,
                        &mut recovery_source_absences,
                        request,
                    )?;
                    if !absent {
                        if let Some((previous_turn, previous_terminal, _)) =
                            provider_sources.get(&prior_source)
                        {
                            ensure!(
                                previous_turn == &prior_turn
                                    && previous_terminal == &prior_terminal,
                                "provider recovery lineage source has conflicting Turn facts"
                            );
                        } else {
                            provider_sources.insert(
                                prior_source.clone(),
                                (prior_turn.clone(), prior_terminal, source_depth + 1),
                            );
                        }
                        provider_lineage_messages.insert(prior_source);
                    }
                    for child in children {
                        mandatory_roots.insert(child.clone());
                        selected.insert(child.clone());
                        pending.push_back((child, source_depth + 1, CompletionInputPath::Strict));
                    }
                }
                execution_sources.insert(source.clone());
                let fresh = execution::load_coverage_tx(
                    tx,
                    scope,
                    &binding,
                    &execution_sources,
                    request.record_limit,
                    request.byte_limit,
                )?;
                // The original read transaction fixes these frontiers. Keep the
                // independently prepared Work observations and replace sources.
                ensure!(
                    fresh.initialized == execution_coverage.initialized
                        && fresh.open_attempt_ids == execution_coverage.open_attempt_ids
                        && fresh.nonterminal_work_item_ids
                            == execution_coverage.nonterminal_work_item_ids,
                    "provider lineage changed the coherent execution frontier"
                );
                execution_coverage.sources = fresh.sources;
                execution_coverage.source(&source)?;
                let candidates = execution_coverage.required_body_keys(&mandatory_roots);
                for child in candidates {
                    mandatory_roots.insert(child.clone());
                    selected.insert(child.clone());
                    pending.push_back((child, source_depth + 1, CompletionInputPath::Strict));
                }
                expanded_provider_sources.insert(source);
            }
        }
    }
    let resident = ResidentInputEvidence::bootstrap(
        scope.clone(),
        records.into_values().collect(),
        InputEvidenceCoverage {
            selected_completion_brief_ids: request.selected_completion_brief_ids.clone(),
            completion_source_observations,
            recovery_source_absences,
            recovery_lineage_messages,
            execution: execution_coverage,
            windows,
            mandatory_roots,
            turn_transcript_keys,
            message_turn_keys,
            unavailable,
            record_limit: request.record_limit.min(MAX_RECORDS),
            byte_limit: request.byte_limit,
        },
    )?;
    Ok(InputEvidenceBootstrapSnapshot::Current(
        InputEvidenceCurrentSnapshot { facts, resident },
    ))
}
/// A missing asserted source is authority evidence only if no canonical row
/// (including a foreign agent), prior qualification or pruning witness exists.
/// This observes metadata; it never adopts or materializes an unqualified body.
fn recovery_source_children_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    recovery: &QualifiedInputEvidence,
    turn: &str,
    source: &str,
    terminal: crate::types::TurnTerminalKind,
    observations: &mut BTreeMap<String, RecoverySourceAbsence>,
    request: &InputEvidenceLoadRequest,
) -> Result<(Vec<Key>, bool)> {
    let children = [
        Key::new(Family::Message, source),
        Key::new(Family::Turn, turn),
    ];
    let mut absent = BTreeSet::new();
    for key in &children {
        let (table, id) = table(key.family);
        let canonical: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {id}=?1)"),
            [&key.id],
            |row| row.get(0),
        )?;
        let qualified: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE family=?1 AND record_id=?2)
              OR EXISTS(SELECT 1 FROM input_evidence_seed_membership m
                        WHERE m.family=?1 AND m.record_id=?2)",
            params![key.family.as_str(), key.id],
            |row| row.get(0),
        )?;
        if !canonical && !qualified {
            absent.insert(key.clone());
        }
    }
    let has_absence = !absent.is_empty();
    if has_absence {
        ensure!(
            recovery.locator.scope == *scope && recovery.locator.key.family == Family::Message,
            "recovery absence observation scope differs"
        );
        observations.insert(
            recovery.locator.key.id.clone(),
            RecoverySourceAbsence {
                recovery_message: recovery.locator.clone(),
                source_message_id: source.into(),
                source_turn_id: turn.into(),
                terminal_kind: terminal,
                absent: absent.clone(),
            },
        );
        if observations.len() > request.record_limit.min(MAX_RECORDS)
            || serde_json::to_vec(observations)?.len() > request.byte_limit.min(MAX_METADATA_BYTES)
        {
            return Err(InputEvidenceSnapshotError::ResourceExhausted(
                "recovery source absence metadata",
            )
            .into());
        }
    }
    Ok((
        children
            .into_iter()
            .filter(|key| !absent.contains(key))
            .collect(),
        has_absence,
    ))
}

fn qualified(
    scope: &InputEvidenceScope,
    key: Key,
    revision: u64,
    digest: String,
    fingerprint: Fingerprint,
    record: PreparedInputEvidence,
    attempt_turn_proof: Option<AttemptTurnProof>,
) -> Result<QualifiedInputEvidence> {
    ensure!(
        record.key() == key,
        "prepared evidence key projection mismatch"
    );
    let attempt_turn_keys = attempt_turn_proof.map(|proof| proof.turn_keys);
    ensure!(
        matches!(record, PreparedInputEvidence::Attempt(_)) == attempt_turn_keys.is_some(),
        "Attempt relation metadata differs from record family"
    );
    let resident_bytes = serde_json::to_vec(&record)?
        .len()
        .saturating_add(serde_json::to_vec(&attempt_turn_keys)?.len());
    if matches!(record, PreparedInputEvidence::ToolExecution(_)) && resident_bytes > 32 * 1024 {
        return Err(
            InputEvidenceSnapshotError::ResourceExhausted("bounded Tool projection bytes").into(),
        );
    }
    if let PreparedInputEvidence::Transcript(entry) = &record {
        ensure!(
            entry
                .data
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                == fingerprint.related_turn_id.as_deref(),
            "Transcript payload and normalized Turn linkage differ"
        );
    }
    let owner = match &record {
        PreparedInputEvidence::Turn(x) => Some(x.effective_owner()),
        _ => None,
    };
    Ok(QualifiedInputEvidence {
        locator: EvidenceLocator {
            scope: scope.clone(),
            key,
            revision,
            content_digest: digest,
        },
        record,
        created_at: fingerprint.created_at,
        order_sequence: fingerprint.sequence,
        turn_index: fingerprint.turn_index,
        owner,
        resident_bytes,
        attempt_turn_keys,
    })
}
fn require_current_scope(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<ContextScopeFacts> {
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    if !facts.identity.as_ref().is_some_and(|x| {
        x.incarnation == scope.identity_incarnation && x.status == AgentRegistryStatus::Active
    }) {
        return Err(InputEvidenceSnapshotError::RetiredScope.into());
    }
    Ok(facts)
}
pub(crate) fn require_original_input_recovery_scope(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<ContextScopeFacts> {
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    ensure!(
        facts
            .identity
            .as_ref()
            .is_some_and(|identity| identity.agent_id == scope.agent_id
                && identity.incarnation == scope.identity_incarnation
                && matches!(
                    identity.status,
                    AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
                )),
        "original input recovery requires its current non-deleted incarnation"
    );
    Ok(facts)
}
fn binding_on(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<Binding> {
    let (mode,seed):(String,Option<String>)=tx.query_row("SELECT mode,seed_id FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        (mode == "fresh_empty" && seed.is_none()) || (mode == "legacy_adopted" && seed.is_some()),
        "invalid input continuity binding"
    );
    if let Some(seed_id) = seed.as_deref() {
        let(import_required,sealed):(bool,bool)=tx.query_row("SELECT import_required,EXISTS(SELECT 1 FROM episode_continuity_imports i WHERE i.seed_id=s.seed_id) FROM episode_continuity_seeds s WHERE s.seed_id=?1",[seed_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            !import_required || sealed,
            "input evidence legacy import is not sealed"
        );
    }
    Ok(Binding { seed_id: seed })
}
/// Metadata qualification can run before the request's body budget check.
/// The returned digest set is checked after its one selected materialization.
fn require_eligible_metadata(
    tx: &Transaction<'_>,
    binding: &Binding,
    key: &Key,
    current: &Fingerprint,
) -> Result<BTreeSet<String>> {
    let seed = binding
        .seed_id
        .as_deref()
        .ok_or_else(|| InputEvidenceSnapshotError::NeedsPreparation(key.id.clone()))?;
    legacy_recovery::validate_seed_overlay_on(tx, seed, key)?;
    let mut statement=tx.prepare("SELECT fingerprint_json FROM input_evidence_seed_membership WHERE seed_id=?1 AND family=?2 AND record_id=?3")?;
    let rows = statement.query_map(params![seed, key.family.as_str(), key.id], |r| {
        r.get::<_, String>(0)
    })?;
    let mut matched = false;
    let mut digests = BTreeSet::new();
    for row in rows {
        let frozen: Fingerprint = serde_json::from_str(&row?)?;
        if frozen.same_metadata(current) {
            matched = true;
            if let Some(digest) = frozen.frozen_payload_digest {
                digests.insert(digest);
            }
        }
    }
    if !matched {
        return Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into());
    }
    if requires_frozen_body_digest(key, current) && digests.is_empty() {
        return Err(InputEvidenceSnapshotError::NeedsPreparation(format!(
            "fixed input {} has no frozen body digest",
            key.id
        ))
        .into());
    }
    Ok(digests)
}
fn validate_fixed_body_digest(key: &Key, digests: &BTreeSet<String>, actual: &str) -> Result<()> {
    if !digests.contains(actual) {
        return Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into());
    }
    Ok(())
}
/// Writers and unbound/offline recovery validate before rewriting the row.
/// Their finite proof is not a request materialization, so use the fixed actor
/// bound rather than bypassing a consumer's metadata-first body preflight.
fn require_eligible(
    tx: &Transaction<'_>,
    binding: &Binding,
    key: &Key,
    current: &Fingerprint,
) -> Result<()> {
    let digests = require_eligible_metadata(tx, binding, key, current)?;
    if key.family != Family::ToolExecution {
        let agent: String = tx.query_row(
            "SELECT agent_id FROM episode_continuity_seeds WHERE seed_id=?1",
            [binding
                .seed_id
                .as_deref()
                .context("fixed input seed missing")?],
            |row| row.get(0),
        )?;
        let digest = bounded_body_digest(tx, &agent, key, current)?;
        validate_canonical_payload_digest(key, current, &digest)?;
        if requires_frozen_body_digest(key, current) {
            validate_fixed_body_digest(key, &digests, &digest)?;
        }
    }
    Ok(())
}
fn bounded_body_digest(
    tx: &Transaction<'_>,
    agent: &str,
    key: &Key,
    fingerprint: &Fingerprint,
) -> Result<String> {
    if fingerprint.payload_bytes > MAX_METADATA_BYTES {
        return Err(
            InputEvidenceSnapshotError::ResourceExhausted("input validation body bytes").into(),
        );
    }
    let payload = payload_on(tx, agent, key)?.context("qualified input body missing")?;
    Ok(bytes_digest(payload.as_bytes()))
}
fn validate_alias_body(
    tx: &Transaction<'_>,
    agent: &str,
    key: &Key,
    fingerprint: &Fingerprint,
    expected: Option<&str>,
) -> Result<()> {
    let actual = bounded_body_digest(tx, agent, key, fingerprint)?;
    validate_canonical_payload_digest(key, fingerprint, &actual)?;
    if expected != Some(actual.as_str()) {
        return Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into());
    }
    Ok(())
}
fn requires_frozen_body_digest(key: &Key, fingerprint: &Fingerprint) -> bool {
    key.family != Family::ToolExecution && fingerprint.content_hash.is_none()
}
fn validate_canonical_payload_digest(
    key: &Key,
    fingerprint: &Fingerprint,
    actual: &str,
) -> Result<()> {
    if let Some(hash) = fingerprint.content_hash.as_deref() {
        if normalized_payload_digest(hash)? != actual {
            return Err(InputEvidenceSnapshotError::ProjectionChanged(key.id.clone()).into());
        }
    }
    Ok(())
}
fn alias_on(tx: &Transaction<'_>, scope: &InputEvidenceScope, key: &Key) -> Result<Option<Alias>> {
    tx.query_row("SELECT revision,content_digest,fingerprint_json,availability,attempt_turn_proof_json FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.family.as_str(),key.id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?))).optional()?.map(|(revision,digest,fingerprint,availability,proof)|Ok(Alias{revision:u64::try_from(revision)?,digest,fingerprint:serde_json::from_str(&fingerprint)?,present:availability=="present",attempt_turn_proof:proof.map(|json|serde_json::from_str(&json)).transpose()?})).transpose()
}
fn upsert_alias_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &Key,
    fingerprint: &Fingerprint,
    digest: Option<&str>,
    present: bool,
    attempt_turn_proof: Option<&AttemptTurnProof>,
    qualification: &str,
    seed: Option<&str>,
) -> Result<u64> {
    let old = alias_on(tx, scope, key)?;
    if let Some(old) = old.as_ref() {
        if old.fingerprint == *fingerprint
            && old.digest.as_deref() == digest
            && old.present == present
            && old.attempt_turn_proof.as_ref() == attempt_turn_proof
        {
            task_output::register_current_input_relation_tx(
                tx,
                scope,
                key,
                old.revision,
                fingerprint.related_turn_id.as_deref(),
            )?;
            return Ok(old.revision);
        }
    }
    let revision = old.map_or(Ok(1), |old| {
        old.revision
            .checked_add(1)
            .context("input evidence revision overflow")
    })?;
    tx.execute("INSERT INTO input_evidence_current(agent_id,identity_incarnation,family,record_id,revision,content_digest,fingerprint_json,availability,qualification,seed_id,attempt_turn_proof_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(agent_id,identity_incarnation,family,record_id) DO UPDATE SET revision=excluded.revision,content_digest=excluded.content_digest,fingerprint_json=excluded.fingerprint_json,availability=excluded.availability,qualification=excluded.qualification,seed_id=excluded.seed_id,attempt_turn_proof_json=excluded.attempt_turn_proof_json",params![scope.agent_id,i64::try_from(scope.identity_incarnation)?,key.family.as_str(),key.id,i64::try_from(revision)?,digest,serde_json::to_string(fingerprint)?,if present{"present"}else{"pruned"},qualification,seed,attempt_turn_proof.map(serde_json::to_string).transpose()?])?;
    task_output::register_current_input_relation_tx(
        tx,
        scope,
        key,
        revision,
        fingerprint.related_turn_id.as_deref(),
    )?;
    let stored = alias_on(tx, scope, key)?.context("input evidence alias missing after commit")?;
    ensure!(
        stored.revision == revision
            && stored.digest.as_deref() == digest
            && stored.fingerprint == *fingerprint
            && stored.present == present
            && stored.attempt_turn_proof.as_ref() == attempt_turn_proof,
        "input evidence alias exact readback mismatch"
    );
    Ok(revision)
}
fn normalized_payload_digest(hash: &str) -> Result<String> {
    let digest = hash
        .strip_prefix("sha256:")
        .context("canonical content hash has an unknown algorithm")?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "canonical content hash is not a SHA256 digest"
    );
    Ok(digest.to_owned())
}
fn bytes_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn table(family: Family) -> (&'static str, &'static str) {
    match family {
        Family::Message => ("messages", "evidence_id"),
        Family::Turn => ("turn_records", "turn_id"),
        Family::ToolExecution => ("tool_executions", "evidence_id"),
        Family::Brief => ("briefs", "evidence_id"),
        Family::Transcript => ("transcript_entries", "evidence_id"),
        Family::Attempt => ("execution_protocol_attempts", "attempt_id"),
        Family::Outcome => ("execution_protocol_outcomes", "outcome_id"),
        Family::WorkExecution => ("execution_protocol_work_items", "work_item_id"),
    }
}
fn payload_on(tx: &rusqlite::Connection, agent: &str, key: &Key) -> Result<Option<String>> {
    let (table, id) = table(key.family);
    Ok(tx
        .query_row(
            &format!("SELECT payload_json FROM {table} WHERE agent_id=?1 AND {id}=?2"),
            params![agent, key.id],
            |r| r.get(0),
        )
        .optional()?)
}
/// Original fixed actor only. This freezes a source header/body version; it
/// does not mint a current alias or ordinary read authority.
pub(in crate::runtime_db) fn fixed_task_query_message_on(
    connection: &rusqlite::Connection,
    agent: &str,
    id: &str,
) -> Result<(crate::types::MessageEnvelope, String, String)> {
    let bytes: usize = connection.query_row(
        "SELECT length(CAST(m.payload_json AS BLOB))+length(CAST(m.evidence_id AS BLOB))+length(CAST(m.agent_id AS BLOB))+COALESCE(length(CAST(m.message_id AS BLOB)),0)+length(CAST(m.created_at AS BLOB))+COALESCE(length(CAST(m.content_hash AS BLOB)),0)+COALESCE((SELECT length(CAST(a.turn_id AS BLOB)) FROM conversation_input_assignments a WHERE a.agent_id=m.agent_id AND a.message_id=m.evidence_id),0)+1024 FROM messages m WHERE m.agent_id=?1 AND m.evidence_id=?2",
        params![agent,id], |row| row.get(0))?;
    ensure!(
        bytes
            .checked_mul(8)
            .is_some_and(|n| n <= MAX_METADATA_BYTES),
        InputEvidenceSnapshotError::ResourceExhausted("fixed Task query Message material")
    );
    let key = Key::new(Family::Message, id);
    let mut fingerprint =
        fingerprint_on(connection, agent, &key)?.context("fixed Task query Message missing")?;
    let raw =
        payload_on(connection, agent, &key)?.context("fixed Task query Message body missing")?;
    let message = crate::runtime_db::repositories::decode_message_payload(&raw)?;
    ensure!(
        message.id == id
            && message.agent_id == agent
            && message.message_seq == fingerprint.sequence
            && crate::runtime_db::repositories::timestamp(message.created_at)
                == fingerprint.created_at,
        "fixed Task query Message normalized header differs"
    );
    let digest = bytes_digest(raw.as_bytes());
    // Match the original Input seed representation: a canonical hash already
    // proves the body; unhashed legacy rows need the independent frozen digest.
    if requires_frozen_body_digest(&key, &fingerprint) {
        fingerprint.frozen_payload_digest = Some(digest.clone());
    }
    Ok((message, digest, serde_json::to_string(&fingerprint)?))
}
fn fingerprint_on(
    tx: &rusqlite::Connection,
    agent: &str,
    key: &Key,
) -> Result<Option<Fingerprint>> {
    let (table, id) = table(key.family);
    let (hash,seq,index,created,owner_kind,owner_id,revision)=match key.family {
 Family::Turn=>("NULL","NULL","r.turn_index","r.created_at","r.owner_kind","r.owner_id","'canonical_body'"),
 Family::Message=>("r.content_hash","r.message_seq","NULL","r.created_at","NULL","NULL","'canonical_body'"),
 Family::Transcript=>("r.content_hash","r.transcript_seq","NULL","r.created_at","NULL","NULL","'canonical_body'"),
 Family::ToolExecution=>("r.content_hash","NULL","NULL","r.created_at","NULL","NULL","'canonical_body'"),
 Family::Brief=>("r.content_hash","NULL","NULL","r.created_at","NULL","NULL","CAST(COALESCE(r.created_event_seq,0) AS TEXT)"),
 Family::Attempt=>("NULL","NULL","NULL","COALESCE(json_extract(r.payload_json,'$.admitted_at'),'')","NULL","NULL","r.lifecycle_state||':'||r.source_generation||':'||COALESCE(r.terminal_outcome_id,'')||':'||COALESCE(r.recovery_of_attempt_id,'')"),
 Family::Outcome=>("NULL","NULL","NULL","COALESCE(json_extract(r.payload_json,'$.created_at'),'')","NULL","NULL","r.attempt_id"),
 Family::WorkExecution=>("NULL","NULL","NULL","''","NULL","NULL","r.source_revision||':'||r.generation||':'||r.lifecycle_state"),
 };
    let related_turn = match key.family {
        Family::Transcript => "r.turn_id",
        Family::Message => "(SELECT a.turn_id FROM conversation_input_assignments a WHERE a.agent_id=r.agent_id AND a.message_id=r.evidence_id)",
        _ => "NULL",
    };
    let sql=format!("SELECT {hash},{seq},{index},{created},{owner_kind},{owner_id},{revision},length(CAST(r.payload_json AS BLOB)),{related_turn} FROM {table} r WHERE r.agent_id=?1 AND r.{id}=?2");
    tx.query_row(&sql, params![agent, key.id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, Option<String>>(8)?,
        ))
    })
    .optional()?
    .map(
        |(
            content_hash,
            sequence,
            turn_index,
            created_at,
            owner_kind,
            owner_id,
            revision_marker,
            payload_bytes,
            related_turn_id,
        )| {
            Ok(Fingerprint {
                content_hash,
                sequence: sequence.map(u64::try_from).transpose()?,
                turn_index: turn_index.map(u64::try_from).transpose()?,
                created_at,
                owner_kind,
                owner_id,
                revision_marker,
                payload_bytes: usize::try_from(payload_bytes)?,
                related_turn_id,
                frozen_payload_digest: None,
            })
        },
    )
    .transpose()
}
fn window_query(
    selector: &InputWindowSelector,
) -> (Family, String, String, Vec<rusqlite::types::Value>) {
    let mut values = Vec::new();
    let (family,filter,order)=match selector {
 InputWindowSelector::GlobalTurns=>(Family::Turn,"1".to_owned(),"r.turn_index DESC,r.created_at DESC,r.turn_id ASC"),
 InputWindowSelector::OwnerTurns{owner}=>{let(kind,id)=owner.index_parts();values.push(rusqlite::types::Value::Text(kind.into()));values.push(id.map_or(rusqlite::types::Value::Null,|id|rusqlite::types::Value::Text(id.into())));(Family::Turn,"r.owner_kind=?3 AND r.owner_id IS ?4".into(),"r.turn_index DESC,r.created_at DESC,r.turn_id ASC")},
 InputWindowSelector::RecentBriefs=>(Family::Brief,"1".into(),"r.created_at DESC,r.evidence_id DESC"),
 InputWindowSelector::RecentTools=>(Family::ToolExecution,"1".into(),"r.created_at DESC,r.evidence_id DESC"),
 InputWindowSelector::RecentTranscript=>(Family::Transcript,"1".into(),"COALESCE(r.transcript_seq,9223372036854775807) DESC,r.created_at DESC,r.evidence_id ASC"),
 InputWindowSelector::TrustedOperator=>(Family::Message,"r.message_seq IS NOT NULL AND json_extract(r.payload_json,'$.authority_class') IN ('operator_instruction','trusted_operator') AND json_extract(r.payload_json,'$.origin.kind')='operator'".into(),"r.message_seq DESC,r.created_at DESC,r.evidence_id DESC"),
 };
    (family, filter, order.into(), values)
}
fn raw_window_keys(
    tx: &Transaction<'_>,
    agent: &str,
    selector: &InputWindowSelector,
    limit: usize,
) -> Result<Vec<Key>> {
    let (family, filter, order, extra) = window_query(selector);
    let (table, id) = table(family);
    let mut values = vec![
        rusqlite::types::Value::Text(agent.into()),
        rusqlite::types::Value::Integer(i64::try_from(limit)?),
    ];
    values.extend(extra);
    let sql = format!(
        "SELECT r.{id} FROM {table} r WHERE r.agent_id=?1 AND {filter} ORDER BY {order} LIMIT ?2"
    );
    let mut statement = tx.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(values), |r| {
        r.get::<_, String>(0)
    })?;
    rows.map(|row| Ok(Key::new(family, row?))).collect()
}
fn qualified_window_keys(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    selector: &InputWindowSelector,
    limit: usize,
) -> Result<Vec<Key>> {
    let (family, filter, order, extra) = window_query(selector);
    let (table, id) = table(family);
    let mut values = vec![
        rusqlite::types::Value::Text(scope.agent_id.clone()),
        rusqlite::types::Value::Integer(i64::try_from(limit)?),
    ];
    values.extend(extra);
    let inc_index = values.len() + 1;
    values.push(rusqlite::types::Value::Integer(i64::try_from(
        scope.identity_incarnation,
    )?));
    let seed_index = values.len() + 1;
    values.push(
        binding
            .seed_id
            .clone()
            .map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
    );
    let current=format!("c.agent_id=r.agent_id AND c.identity_incarnation=?{inc_index} AND c.family='{}' AND c.record_id=r.{id}",family.as_str());
    let qualification=format!("(EXISTS(SELECT 1 FROM input_evidence_current c WHERE {current} AND c.availability='present') OR (NOT EXISTS(SELECT 1 FROM input_evidence_current c WHERE {current}) AND EXISTS(SELECT 1 FROM input_evidence_seed_membership e WHERE e.seed_id=?{seed_index} AND e.family='{}' AND e.record_id=r.{id})))",family.as_str());
    let sql=format!("SELECT r.{id} FROM {table} r WHERE r.agent_id=?1 AND {filter} AND {qualification} ORDER BY {order} LIMIT ?2");
    let mut statement = tx.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(values), |r| {
        r.get::<_, String>(0)
    })?;
    rows.map(|row| Ok(Key::new(family, row?))).collect()
}
/// Relations come only from this incarnation's current aliases or the fixed
/// legacy metadata checkpoint. Deleted entries remain explicit tombstone edges.
fn actual_input_turn_on(
    tx: &Transaction<'_>,
    agent: &str,
    message_id: &str,
) -> Result<Option<String>> {
    Ok(tx.query_row("SELECT turn_id FROM conversation_input_assignments WHERE agent_id=?1 AND message_id=?2",
        params![agent,message_id],|row|row.get(0)).optional()?)
}
/// SQL scalar equivalent of `wake_contract::agent_message_reply_reference`.
/// No arbitrary body text or untrusted metadata reference grants membership.
fn task_reply_reference_on(tx: &Transaction<'_>, agent: &str, id: &str) -> Result<Option<String>> {
    Ok(tx.query_row("SELECT json_extract(payload_json,'$.metadata.task_detail.message_id') FROM messages
        WHERE agent_id=?1 AND evidence_id=?2
        AND json_extract(payload_json,'$.kind')='task_result'
        AND json_extract(payload_json,'$.delivery_surface')='task_rejoin'
        AND json_extract(payload_json,'$.admission_context')='runtime_owned'
        AND json_extract(payload_json,'$.authority_class')='runtime_instruction'
        AND json_extract(payload_json,'$.origin.kind')='task'
        AND json_extract(payload_json,'$.origin.task_id')=json_extract(payload_json,'$.task_id')
        AND json_extract(payload_json,'$.metadata.task_id')=json_extract(payload_json,'$.task_id')
        AND json_extract(payload_json,'$.metadata.task_kind')=?3
        AND json_extract(payload_json,'$.metadata.task_status')='completed'
        AND json_extract(payload_json,'$.metadata.task_detail.reply_content_source')='original_message'",
        params![agent,id,crate::types::AGENT_MESSAGE_WAIT_TASK_KIND],|row|row.get(0)).optional()?.flatten())
}
fn qualified_turn_transcript_keys(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    turn_id: &str,
    limit: usize,
) -> Result<BTreeSet<Key>> {
    let mut statement = tx.prepare(
        "SELECT record_id FROM input_evidence_current
         WHERE agent_id=?1 AND identity_incarnation=?2 AND family='transcript'
           AND json_extract(fingerprint_json,'$.related_turn_id')=?4
         UNION
         SELECT e.record_id FROM input_evidence_seed_membership e
         WHERE e.seed_id=?3 AND e.family='transcript'
           AND json_extract(e.fingerprint_json,'$.related_turn_id')=?4
           AND NOT EXISTS(SELECT 1 FROM input_evidence_current c
             WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.family=e.family AND c.record_id=e.record_id)
         ORDER BY record_id LIMIT ?5")?;
    let rows = statement.query_map(
        params![
            scope.agent_id,
            i64::try_from(scope.identity_incarnation)?,
            binding.seed_id,
            turn_id,
            i64::try_from(limit.saturating_add(1))?
        ],
        |r| r.get::<_, String>(0),
    )?;
    let keys = rows
        .map(|row| Ok(Key::new(Family::Transcript, row?)))
        .collect::<Result<BTreeSet<_>>>()?;
    if keys.len() > limit {
        return Err(
            InputEvidenceSnapshotError::ResourceExhausted("Turn transcript closure").into(),
        );
    }
    Ok(keys)
}
fn qualified_live_protocol_keys(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    binding: &Binding,
    limit: usize,
) -> Result<Vec<Key>> {
    let mut result = Vec::new();
    for (family, filter) in [
        (Family::Attempt, "r.lifecycle_state!='settled'"),
        (Family::WorkExecution, "r.lifecycle_state!='terminal'"),
    ] {
        let (table, id) = table(family);
        let matching=format!("c.agent_id=r.agent_id AND c.identity_incarnation=?2 AND c.family='{}' AND c.record_id=r.{id}",family.as_str());
        let qualified=format!("(EXISTS(SELECT 1 FROM input_evidence_current c WHERE {matching} AND c.availability='present') OR (NOT EXISTS(SELECT 1 FROM input_evidence_current c WHERE {matching}) AND EXISTS(SELECT 1 FROM input_evidence_seed_membership e WHERE e.seed_id=?3 AND e.family='{}' AND e.record_id=r.{id})))",family.as_str());
        let sql=format!("SELECT r.{id} FROM {table} r WHERE r.agent_id=?1 AND {filter} AND {qualified} ORDER BY r.{id} LIMIT ?4");
        let mut statement = tx.prepare(&sql)?;
        let rows = statement.query_map(
            params![
                scope.agent_id,
                i64::try_from(scope.identity_incarnation)?,
                binding.seed_id,
                i64::try_from(limit.saturating_add(1))?
            ],
            |r| r.get::<_, String>(0),
        )?;
        for row in rows {
            result.push(Key::new(family, row?));
            if result.len() > limit {
                return Err(
                    InputEvidenceSnapshotError::ResourceExhausted("live protocol count").into(),
                );
            }
        }
    }
    Ok(result)
}
fn extend_metadata_selection(
    selected: &mut BTreeSet<Key>,
    bytes: &mut usize,
    keys: impl IntoIterator<Item = Key>,
) -> Result<()> {
    for key in keys {
        if !selected.contains(&key) {
            let next_bytes = bytes
                .saturating_add(key.id.len())
                .saturating_add(key.family.as_str().len());
            if selected.len() >= MAX_RECORDS || next_bytes > MAX_METADATA_BYTES {
                return Err(InputEvidenceSnapshotError::ResourceExhausted(
                    "eligibility selection count/bytes",
                )
                .into());
            }
            selected.insert(key);
            *bytes = next_bytes;
        }
    }
    Ok(())
}
fn enqueue_metadata_key(
    selected: &mut BTreeSet<Key>,
    pending: &mut VecDeque<(Key, usize)>,
    bytes: &mut usize,
    key: Key,
    depth: usize,
) -> Result<()> {
    if selected.contains(&key) {
        return Ok(());
    }
    if depth > MAX_DEPTH {
        return Err(
            InputEvidenceSnapshotError::ResourceExhausted("eligibility closure depth").into(),
        );
    }
    extend_metadata_selection(selected, bytes, [key.clone()])?;
    pending.push_back((key, depth));
    Ok(())
}
/// Metadata-only BFS follows the same typed lineage edges needed by actual
/// input/evidence consumers. It never scans arbitrary body strings.
fn metadata_closure(
    tx: &Transaction<'_>,
    agent: &str,
    roots: BTreeSet<Key>,
    captured_tables: Option<&BTreeSet<String>>,
) -> Result<BTreeSet<Key>> {
    let mut result = BTreeSet::new();
    let mut pending = VecDeque::new();
    let mut bytes = 0usize;
    for key in roots {
        enqueue_metadata_key(&mut result, &mut pending, &mut bytes, key, 0)?;
    }
    while let Some((key, depth)) = pending.pop_front() {
        let (table, id) = table(key.family);
        if captured_tables.is_some_and(|tables| !tables.contains(table)) {
            continue;
        }
        let paths: &[(&str, Family, bool)] = match key.family {
            Family::Turn => &[
                ("$.input_message_ids", Family::Message, true),
                ("$.trigger.message_id", Family::Message, false),
                ("$.tool_execution_ids", Family::ToolExecution, true),
                ("$.produced_brief_ids", Family::Brief, true),
                ("$.replay.source_message_id", Family::Message, false),
            ],
            Family::Brief => &[
                ("$.related_message_id", Family::Message, false),
                ("$.content_source.entry_id", Family::Transcript, false),
            ],
            Family::Message => &[],
            Family::Transcript => &[("$.related_message_id", Family::Message, false)],
            Family::Attempt => &[
                ("$.source_message_id", Family::Message, false),
                ("$.recovery_of_attempt_id", Family::Attempt, false),
                ("$.terminal_outcome_id", Family::Outcome, false),
                ("$.binding.work_item_id", Family::WorkExecution, false),
            ],
            Family::Outcome => &[("$.attempt_id", Family::Attempt, false)],
            Family::WorkExecution => &[("$.state.attempt_id", Family::Attempt, false)],
            Family::ToolExecution => &[],
        };
        for (path, family, array) in paths {
            let expression = if *array {
                format!("json_extract(payload_json,'{path}')")
            } else {
                format!("json_array(json_extract(payload_json,'{path}'))")
            };
            let value: Option<String> = tx
                .query_row(
                    &format!("SELECT {expression} FROM {table} WHERE agent_id=?1 AND {id}=?2"),
                    params![agent, key.id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            if let Some(value) = value {
                bytes = bytes.saturating_add(value.len());
                if bytes > MAX_METADATA_BYTES {
                    return Err(InputEvidenceSnapshotError::ResourceExhausted(
                        "eligibility reference metadata bytes",
                    )
                    .into());
                }
                let ids: Vec<Option<String>> = serde_json::from_str(&value)?;
                for id in ids.into_iter().flatten() {
                    enqueue_metadata_key(
                        &mut result,
                        &mut pending,
                        &mut bytes,
                        Key::new(*family, id),
                        depth + 1,
                    )?;
                }
            }
        }
        if key.family == Family::Message {
            if !captured_tables
                .is_some_and(|tables| !tables.contains("conversation_input_assignments"))
            {
                if let Some(turn) = actual_input_turn_on(tx, agent, &key.id)? {
                    enqueue_metadata_key(
                        &mut result,
                        &mut pending,
                        &mut bytes,
                        Key::new(Family::Turn, turn),
                        depth + 1,
                    )?;
                }
            }
            if let Some(reply) = task_reply_reference_on(tx, agent, &key.id)? {
                enqueue_metadata_key(
                    &mut result,
                    &mut pending,
                    &mut bytes,
                    Key::new(Family::Message, reply),
                    depth + 1,
                )?;
            }
        }
        if key.family == Family::Attempt {
            let payload = payload_on(tx, agent, &key)?.context("fixed Attempt disappeared")?;
            let attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
            let proof = attempt_turn::observe(tx, None, &attempt, None)?;
            for turn in proof.turn_keys {
                enqueue_metadata_key(&mut result, &mut pending, &mut bytes, turn, depth + 1)?;
            }
        }
        // Full transcript for selected/recovery Turns is part of the existing history
        // contract. Only IDs are frozen here, not provider/tool result bodies.
        if key.family == Family::Turn
            && !captured_tables.is_some_and(|tables| !tables.contains("transcript_entries"))
        {
            let mut statement=tx.prepare("SELECT evidence_id FROM transcript_entries WHERE agent_id=?1 AND turn_id=?2 ORDER BY transcript_seq,created_at,evidence_id")?;
            for row in statement.query_map(params![agent, key.id], |r| r.get::<_, String>(0))? {
                enqueue_metadata_key(
                    &mut result,
                    &mut pending,
                    &mut bytes,
                    Key::new(Family::Transcript, row?),
                    depth + 1,
                )?;
            }
        }
    }
    Ok(result)
}
fn decode_canonical(key: &Key, payload: &str) -> Result<CanonicalInputEvidence> {
    Ok(match key.family {
        Family::Message => CanonicalInputEvidence::Message(serde_json::from_str(payload)?),
        Family::Turn => CanonicalInputEvidence::Turn(serde_json::from_str(payload)?),
        Family::ToolExecution => {
            CanonicalInputEvidence::ToolExecution(serde_json::from_str(payload)?)
        }
        Family::Brief => CanonicalInputEvidence::Brief(serde_json::from_str(payload)?),
        Family::Transcript => CanonicalInputEvidence::Transcript(serde_json::from_str(payload)?),
        Family::Attempt => CanonicalInputEvidence::Attempt(serde_json::from_str(payload)?),
        Family::Outcome => CanonicalInputEvidence::Outcome(serde_json::from_str(payload)?),
        Family::WorkExecution => CanonicalInputEvidence::WorkExecution {
            work_item_id: key.id.clone(),
            record: serde_json::from_str(payload)?,
        },
    })
}
fn prepare_record_on(
    tx: &Transaction<'_>,
    agent: &str,
    key: &Key,
    canonical: Option<CanonicalInputEvidence>,
) -> Result<PreparedInputEvidence> {
    if key.family == Family::ToolExecution {
        let projected = match canonical {
            Some(CanonicalInputEvidence::ToolExecution(actual)) => {
                ensure!(
                    actual.agent_id == agent && actual.id == key.id,
                    "Tool payload scope/ID mismatch"
                );
                project_tool_record(&actual)?
            }
            Some(_) => anyhow::bail!("wrong canonical Tool family"),
            None => tool_projection_on(tx, agent, &key.id)?,
        };
        return Ok(PreparedInputEvidence::ToolExecution(projected));
    }
    let canonical = match canonical {
        Some(record) => record,
        None => decode_canonical(
            key,
            &payload_on(tx, agent, key)?.context("prepared evidence payload missing")?,
        )?,
    };
    let record = match canonical {
        CanonicalInputEvidence::Message(x) => {
            ensure!(x.agent_id == agent, "message payload scope mismatch");
            PreparedInputEvidence::Message(x)
        }
        CanonicalInputEvidence::Turn(x) => {
            ensure!(x.agent_id == agent, "Turn payload scope mismatch");
            PreparedInputEvidence::Turn(x)
        }
        CanonicalInputEvidence::Brief(x) => {
            ensure!(x.agent_id == agent, "Brief payload scope mismatch");
            PreparedInputEvidence::Brief(x)
        }
        CanonicalInputEvidence::Transcript(x) => {
            ensure!(x.agent_id == agent, "transcript payload scope mismatch");
            PreparedInputEvidence::Transcript(x)
        }
        CanonicalInputEvidence::Attempt(x) => {
            ensure!(x.agent_id == agent, "attempt payload scope mismatch");
            PreparedInputEvidence::Attempt(x)
        }
        CanonicalInputEvidence::Outcome(x) => PreparedInputEvidence::Outcome(x),
        CanonicalInputEvidence::WorkExecution {
            work_item_id,
            record,
        } => PreparedInputEvidence::WorkExecution {
            work_item_id,
            record,
        },
        CanonicalInputEvidence::ToolExecution(_) => unreachable!("Tool projection handled above"),
    };
    ensure!(record.key() == *key, "evidence payload ID mismatch");
    Ok(record)
}
/// Read only bounded scalar projections into Rust. SQLite may inspect a large
/// JSON value, but stdout/full input/output never become a retained Arc record.
fn tool_projection_on(
    tx: &Transaction<'_>,
    agent: &str,
    id: &str,
) -> Result<ToolContextProjection> {
    let output="COALESCE(json_extract(payload_json,'$.output.result'),json_extract(payload_json,'$.output.envelope.result'),json_extract(payload_json,'$.output'))";
    let sql=format!("SELECT json_object('id',evidence_id,'agent_id',agent_id,'work_item_id',work_item_id,'turn_id',turn_id,'turn_index',COALESCE(json_extract(payload_json,'$.turn_index'),0),'tool_name',json_extract(payload_json,'$.tool_name'),'created_at',created_at,'authority_class',COALESCE(json_extract(payload_json,'$.authority_class'),json_extract(payload_json,'$.trust')),'status',json_extract(payload_json,'$.status'),'summary',substr(COALESCE(json_extract(payload_json,'$.summary'),''),1,{TOOL_SUMMARY_CHARS}),'command',json_object('command_preview',CASE WHEN length(CAST(json_extract(payload_json,'$.input.cmd') AS BLOB))>{MAX_COMMAND_PREVIEW_INPUT_BYTES} THEN '{OVERSIZED_COMMAND_PREVIEW}' ELSE json_extract(payload_json,'$.input.cmd') END,'disposition',json_extract({output},'$.disposition'),'task_id',json_extract({output},'$.task_handle.task_id'),'exit_status',COALESCE(json_extract({output},'$.exit_status'),json_extract({output},'$.exit_code')),'has_output_evidence',json(CASE WHEN json_type({output},'$.stdout_preview') IS NOT NULL OR json_type({output},'$.stderr_preview') IS NOT NULL OR json_type({output},'$.initial_output_preview') IS NOT NULL OR json_type({output},'$.stdout_artifact') IS NOT NULL OR json_type({output},'$.stderr_artifact') IS NOT NULL THEN 'true' ELSE 'false' END),'truncated',json(CASE WHEN COALESCE(json_extract({output},'$.truncated'),0) OR COALESCE(json_extract({output},'$.initial_output_truncated'),0) OR COALESCE(json_extract({output},'$.stdout_truncated'),0) OR COALESCE(json_extract({output},'$.stderr_truncated'),0) OR COALESCE(json_extract({output},'$.output_truncated'),0) THEN 'true' ELSE 'false' END),'has_artifact',json(CASE WHEN COALESCE(json_array_length({output},'$.artifacts'),0)>0 OR json_type({output},'$.artifact') IS NOT NULL OR json_type({output},'$.stdout_artifact') IS NOT NULL OR json_type({output},'$.stderr_artifact') IS NOT NULL OR json_type({output},'$.output_artifact') IS NOT NULL THEN 'true' ELSE 'false' END)),'batch_total',COALESCE(json_array_length(payload_json,'$.input.items'),0),'batch_succeeded',0,'batch_failed',0,'batch_promoted',0,'batch_unknown',0) FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2 AND COALESCE(json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.session_id'))=?1 AND json_extract(payload_json,'$.id')=?2");
    let json: String = tx.query_row(&sql, params![agent, id], |r| r.get(0))?;
    let mut record: ToolContextProjection = serde_json::from_str(&json)?;
    record.command.command_preview = record
        .command
        .command_preview
        .as_deref()
        .map(safe_command_preview);
    if record.tool_name == "ExecCommandBatch" {
        // SQLite emits only disposition/status scalars for each batch item.
        let sql=format!("SELECT json_extract(COALESCE(json_extract(j.value,'$.result'),j.value),'$.disposition'),COALESCE(json_extract(COALESCE(json_extract(j.value,'$.result'),j.value),'$.exit_status'),json_extract(COALESCE(json_extract(j.value,'$.result'),j.value),'$.exit_code')) FROM tool_executions r,json_each({output},'$.items') j WHERE r.agent_id=?1 AND r.evidence_id=?2 LIMIT ?3");
        let mut statement = tx.prepare(&sql)?;
        let rows = statement.query_map(
            params![agent, id, i64::try_from(record.batch_total)?],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?)),
        )?;
        let mut seen = 0usize;
        for row in rows {
            let (disposition, status) = row?;
            seen += 1;
            if matches!(
                disposition.as_deref(),
                Some("promoted_to_task" | "already_running")
            ) {
                record.batch_promoted += 1
            } else {
                match status {
                    Some(0) => record.batch_succeeded += 1,
                    Some(_) => record.batch_failed += 1,
                    None => record.batch_unknown += 1,
                }
            }
        }
        record.batch_unknown += record.batch_total.saturating_sub(seen);
    }
    Ok(record)
}

/// Called after actual retention/removal SQL in that same transaction. No
/// ignored delete is reported as a removal, and old RAM pins cannot repersist it.
pub(crate) fn register_pruned_input_evidence_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &[Key],
) -> Result<CommittedInputEvidenceDelta> {
    require_current_scope(tx, scope)?;
    let binding = binding_on(tx, scope)?;
    let mut removals = BTreeMap::new();
    let mut output_recall_invalidation = None;
    for key in keys {
        ensure!(
            fingerprint_on(tx, &scope.agent_id, key)?.is_none(),
            "pruned evidence remains in canonical SQL"
        );
        if let Some(invalidation) = task_output::register_pruned_focused_input_tx(tx, scope, key)? {
            output_recall_invalidation = Some(invalidation);
        }
        let old = alias_on(tx, scope, key)?;
        let (fingerprint, digest, attempt_turn_proof) = if let Some(old) = old {
            if !old.present {
                continue;
            }
            (old.fingerprint, old.digest, old.attempt_turn_proof)
        } else {
            let Some(seed) = binding.seed_id.as_deref() else {
                continue;
            };
            let json:Option<String>=tx.query_row("SELECT fingerprint_json FROM input_evidence_seed_membership WHERE seed_id=?1 AND family=?2 AND record_id=?3 ORDER BY CASE capture_phase WHEN 'first_import' THEN 0 ELSE 1 END LIMIT 1",params![seed,key.family.as_str(),key.id],|r|r.get(0)).optional()?;
            let Some(json) = json else { continue };
            let fingerprint: Fingerprint = serde_json::from_str(&json)?;
            let digest = if requires_frozen_body_digest(key, &fingerprint) {
                fingerprint.frozen_payload_digest.clone()
            } else {
                fingerprint
                    .content_hash
                    .as_deref()
                    .map(normalized_payload_digest)
                    .transpose()?
            };
            (fingerprint, digest, None)
        };
        let revision = upsert_alias_tx(
            tx,
            scope,
            key,
            &fingerprint,
            digest.as_deref(),
            false,
            attempt_turn_proof.as_ref(),
            "legacy_prepared",
            binding.seed_id.as_deref(),
        )?;
        removals.insert(
            key.clone(),
            CommittedEvidenceRemoval {
                scope: scope.clone(),
                key: key.clone(),
                revision,
                previous_content_digest: digest,
            },
        );
    }
    Ok(CommittedInputEvidenceDelta {
        output_recall_invalidation,
        execution: execution::prune_tx(tx, scope, &binding, keys)?,
        records: Vec::new(),
        removals: removals.into_values().collect(),
        turn_transcript_keys: BTreeMap::new(),
        message_turn_keys: BTreeMap::new(),
    })
}

#[cfg(test)]
#[path = "input_evidence_snapshot_tests.rs"]
mod tests;
