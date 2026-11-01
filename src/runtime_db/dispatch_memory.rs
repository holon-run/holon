//! Focused, transaction-bound material for one actual dispatch's D1 checkpoint.
//! A seed or obligation is never a business ACK or current AgentState authority.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::{
    memory::episode_transition::FrozenEpisodeMemoryValues,
    runtime::{
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope},
        resident_work::WorkKey,
    },
    types::{
        AgentIdentityRecord, AgentRegistryStatus, AgentState, ClosureDecision, MessageEnvelope,
        MessageKind, TurnRecord, WorkingMemoryState,
    },
};

use super::{
    context_inputs::ContextDurableRead,
    episode_memory::{self, EpisodeMemoryInvocation, EpisodeMemoryKey},
    input_evidence_snapshot::ExistingAdmissionInputProof,
    input_mutation_receipts::{
        canonical_command_digest, InputMutationFamily, PreparedInputMutation,
    },
    work_snapshot::WorkWriteProof,
};

pub(crate) const DISPATCH_MEMORY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dispatch_memory_checkpoints (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  turn_id TEXT NOT NULL,
  source_message_id TEXT NOT NULL,
  turn_index INTEGER NOT NULL CHECK(turn_index > 0),
  state TEXT NOT NULL CHECK(state IN ('started', 'pending', 'done')),
  seed_digest TEXT NOT NULL CHECK(length(seed_digest) = 64),
  record_digest TEXT NOT NULL CHECK(length(record_digest) = 64),
  payload_json TEXT NOT NULL,
  PRIMARY KEY(agent_id, identity_incarnation, turn_id)
);
CREATE TABLE IF NOT EXISTS dispatch_memory_legacy_dispatches (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  turn_id TEXT NOT NULL,
  source_message_id TEXT NOT NULL,
  turn_index INTEGER NOT NULL CHECK(turn_index > 0),
  PRIMARY KEY(agent_id, identity_incarnation, turn_id)
);
CREATE TABLE IF NOT EXISTS dispatch_memory_retired_handoffs (
  agent_id TEXT NOT NULL,
  identity_incarnation INTEGER NOT NULL CHECK(identity_incarnation > 0),
  turn_id TEXT NOT NULL,
  source_message_id TEXT NOT NULL,
  turn_index INTEGER NOT NULL CHECK(turn_index > 0),
  seed_digest TEXT NOT NULL CHECK(length(seed_digest) = 64),
  checkpoint_digest TEXT NOT NULL CHECK(length(checkpoint_digest) = 64),
  control_command_id TEXT NOT NULL,
  control_digest TEXT NOT NULL CHECK(length(control_digest) = 64),
  run_id TEXT NOT NULL,
  runtime_lease_id TEXT NOT NULL,
  record_digest TEXT NOT NULL CHECK(length(record_digest) = 64),
  payload_json TEXT NOT NULL,
  PRIMARY KEY(agent_id, identity_incarnation, turn_id)
);
CREATE TABLE IF NOT EXISTS dispatch_memory_auto_commands (
  command_nonce TEXT PRIMARY KEY CHECK(length(command_nonce) > 0),
  base_command_digest TEXT NOT NULL CHECK(length(base_command_digest) = 64),
  final_command_digest TEXT NOT NULL CHECK(length(final_command_digest) = 64),
  metadata_digest TEXT NOT NULL CHECK(length(metadata_digest) = 64),
  payload_json TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_dispatch_memory_one_unfinished
  ON dispatch_memory_checkpoints(agent_id, identity_incarnation) WHERE state != 'done';
CREATE INDEX IF NOT EXISTS idx_dispatch_memory_unfinished
  ON dispatch_memory_checkpoints(agent_id, identity_incarnation, turn_index, turn_id)
  WHERE state != 'done';
"#;

const MAX_METADATA_RECORDS: usize = 65_536;
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum DispatchMemoryError {
    #[error("actual dispatch has no original memory start")]
    MissingStart,
    #[error("dispatch memory start conflicts with its original frozen payload")]
    StartConflict,
    #[error("dispatch memory member conflicts with its original command")]
    MemberConflict,
    #[error("dispatch memory baseline has advanced; original checkpoint requires repair")]
    BaselineAdvanced,
    #[error("dispatch memory responsibility must finish before a new dispatch")]
    Unfinished,
    #[error("dispatch memory cannot finish without its original D1 receipt")]
    MissingReceipt,
    #[error("dispatch memory already has its original D1 receipt")]
    AlreadyFinalized,
    #[error("dispatch memory metadata exceeds its preparation budget")]
    ResourceExhausted,
}

/// Only the trigger kind influences the existing planner. Large source bodies
/// remain canonical evidence; this record freezes their actual ID/sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchMemoryStart {
    key: EpisodeMemoryKey,
    invocation: EpisodeMemoryInvocation,
    source_sequence: u64,
    source_kind: MessageKind,
    prior_closure: ClosureDecision,
    baseline: WorkingMemoryState,
    digest: String,
}
impl DispatchMemoryStart {
    pub(crate) fn new(
        key: EpisodeMemoryKey,
        invocation: EpisodeMemoryInvocation,
        source: &MessageEnvelope,
        prior_closure: ClosureDecision,
        baseline: WorkingMemoryState,
    ) -> Result<Self> {
        validate_key(&key, &invocation)?;
        ensure!(
            source.agent_id == key.agent_id && source.id == invocation.message_id,
            "dispatch memory source differs from its original invocation"
        );
        let mut start = Self {
            key,
            invocation,
            source_sequence: source
                .message_seq
                .filter(|seq| *seq > 0)
                .context("dispatch memory requires the actual canonical source sequence")?,
            source_kind: source.kind.clone(),
            prior_closure,
            baseline,
            digest: String::new(),
        };
        start.digest = start.compute_digest()?;
        bounded_json(&start)?;
        Ok(start)
    }
    pub(crate) fn key(&self) -> &EpisodeMemoryKey {
        &self.key
    }
    pub(crate) fn invocation(&self) -> &EpisodeMemoryInvocation {
        &self.invocation
    }
    #[cfg(test)]
    pub(crate) fn prior_closure(&self) -> &ClosureDecision {
        &self.prior_closure
    }
    #[cfg(test)]
    pub(crate) fn baseline(&self) -> &WorkingMemoryState {
        &self.baseline
    }
    pub(crate) fn input_roots(&self) -> BTreeSet<InputEvidenceKey> {
        BTreeSet::from([
            InputEvidenceKey::new(InputEvidenceFamily::Message, &self.invocation.message_id),
            InputEvidenceKey::new(InputEvidenceFamily::Turn, &self.key.actual_dispatch_turn_id),
        ])
    }
    fn compute_digest(&self) -> Result<String> {
        canonical_command_digest(&(
            "dispatch_memory_start/v1",
            &self.key,
            &self.invocation,
            self.source_sequence,
            &self.source_kind,
            &self.prior_closure,
            &self.baseline,
        ))
    }
    fn validate(&self) -> Result<()> {
        validate_key(&self.key, &self.invocation)?;
        ensure!(
            self.source_sequence > 0 && self.compute_digest()? == self.digest,
            "dispatch memory seed digest differs from its frozen payload"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DispatchMemoryMember {
    original: PreparedInputMutation,
    work_keys: BTreeSet<WorkKey>,
    input_refs: BTreeSet<InputEvidenceKey>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchMemoryObligation {
    frozen: FrozenEpisodeMemoryValues,
    members: BTreeMap<String, DispatchMemoryMember>,
}
impl DispatchMemoryObligation {
    #[cfg(test)]
    pub(crate) fn frozen(&self) -> &FrozenEpisodeMemoryValues {
        &self.frozen
    }
    pub(crate) fn input_refs(&self) -> BTreeSet<InputEvidenceKey> {
        self.members
            .values()
            .flat_map(|member| member.input_refs.iter().cloned())
            .collect()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchMemoryDone {
    key: EpisodeMemoryKey,
    invocation: EpisodeMemoryInvocation,
    seed_digest: String,
    receipt_digest: String,
    committed_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum DispatchMemoryRecord {
    Started {
        start: DispatchMemoryStart,
        original_start: PreparedInputMutation,
    },
    Pending {
        start: DispatchMemoryStart,
        original_start: PreparedInputMutation,
        obligation: DispatchMemoryObligation,
    },
    Done {
        completed: DispatchMemoryDone,
    },
}
impl DispatchMemoryRecord {
    pub(crate) fn start(&self) -> Option<&DispatchMemoryStart> {
        match self {
            Self::Started { start, .. } | Self::Pending { start, .. } => Some(start),
            Self::Done { .. } => None,
        }
    }
    fn original_start(&self) -> Option<&PreparedInputMutation> {
        match self {
            Self::Started { original_start, .. } | Self::Pending { original_start, .. } => {
                Some(original_start)
            }
            Self::Done { .. } => None,
        }
    }
    pub(crate) fn obligation(&self) -> Option<&DispatchMemoryObligation> {
        match self {
            Self::Pending { obligation, .. } => Some(obligation),
            _ => None,
        }
    }
    pub(crate) fn input_roots(&self) -> BTreeSet<InputEvidenceKey> {
        let mut roots = self
            .start()
            .map(DispatchMemoryStart::input_roots)
            .unwrap_or_default();
        if let Some(obligation) = self.obligation() {
            roots.extend(obligation.input_refs());
        }
        roots
    }
    fn key(&self) -> &EpisodeMemoryKey {
        match self {
            Self::Done { completed } => &completed.key,
            _ => &self.start().expect("unfinished start").key,
        }
    }
    fn invocation(&self) -> &EpisodeMemoryInvocation {
        match self {
            Self::Done { completed } => &completed.invocation,
            _ => &self.start().expect("unfinished start").invocation,
        }
    }
    fn seed_digest(&self) -> &str {
        match self {
            Self::Done { completed } => &completed.seed_digest,
            _ => &self.start().expect("unfinished start").digest,
        }
    }
    fn state(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Pending { .. } => "pending",
            Self::Done { .. } => "done",
        }
    }
    fn validate(&self) -> Result<()> {
        validate_key(self.key(), self.invocation())?;
        if let Some(start) = self.start() {
            start.validate()?;
            let original = self
                .original_start()
                .context("dispatch start omitted its original Posture descriptor")?;
            validate_descriptor(original)?;
            ensure!(
                original.family() == InputMutationFamily::AgentPosture
                    && original.scopes().contains(&scope(&start.key)),
                "dispatch start differs from its original Posture command"
            );
        }
        if let Some(obligation) = self.obligation() {
            ensure!(
                !obligation.members.is_empty(),
                "empty dispatch memory obligation"
            );
            validate_frozen(&obligation.frozen)?;
            bound_members(&obligation.members)?;
            for (nonce, member) in &obligation.members {
                validate_descriptor(&member.original)?;
                ensure!(
                    nonce == member.original.command_nonce()
                        && member.original.scopes().contains(&scope(self.key()))
                        && matches!(
                            member.original.family(),
                            InputMutationFamily::WorkItem
                                | InputMutationFamily::Focus
                                | InputMutationFamily::Timer
                                | InputMutationFamily::Wait
                                | InputMutationFamily::Task
                                | InputMutationFamily::Queue
                        )
                        && !member.work_keys.is_empty(),
                    "dispatch member address/scope mismatch"
                );
            }
        }
        if let Self::Done { completed } = self {
            ensure!(
                valid_digest(&completed.seed_digest) && valid_digest(&completed.receipt_digest),
                "invalid completed dispatch binding"
            );
        }
        Ok(())
    }
}

/// Called after canonical StartedTurn/assignment/input qualification, inside
/// that same original Posture transaction. No ordinary read may mint a seed.
pub(crate) fn install_dispatch_memory_start_tx(
    tx: &Transaction<'_>,
    start: &DispatchMemoryStart,
    original_start: &PreparedInputMutation,
) -> Result<()> {
    start.validate()?;
    validate_descriptor(original_start)?;
    ensure!(
        original_start.family() == InputMutationFamily::AgentPosture
            && original_start.scopes().contains(&scope(&start.key)),
        "dispatch start requires its original Posture descriptor"
    );
    require_identity(tx, &start.key, false)?;
    let state = validate_original_dispatch(tx, start)?;
    ensure!(
        state.working_memory == start.baseline,
        DispatchMemoryError::BaselineAdvanced
    );
    ensure!(
        state.current_turn_id.as_deref() == Some(start.key.actual_dispatch_turn_id.as_str())
            && state.turn_index == start.invocation.turn_index,
        "memory start is not this actual StartedTurn State"
    );
    let turn = turn(tx, start)?;
    let binding = state
        .current_execution_binding
        .as_ref()
        .context("actual StartedTurn has no original execution binding")?;
    ensure!(
        binding.source_message_id == start.invocation.message_id
            && binding.turn_id == start.key.actual_dispatch_turn_id
            && turn.run_id == state.current_run_id,
        "dispatch memory start differs from its actual source/Turn/run binding"
    );
    ensure!(
        turn.terminal.is_none(),
        "cannot install memory start for a terminal Turn"
    );
    if let Some(stored) = load_dispatch_memory_tx(tx, &start.key, &start.invocation)? {
        ensure!(
            stored.start() == Some(start) && stored.original_start() == Some(original_start),
            DispatchMemoryError::StartConflict
        );
        return Ok(());
    }
    ensure!(
        episode_memory::load_receipt(tx, &start.key, None, &start.invocation)?.is_none(),
        DispatchMemoryError::AlreadyFinalized
    );
    validate_new_dispatch_tx(tx, &scope(&start.key))?;
    // A newly seeded actual dispatch may never later fall back to pre-D4
    // semantics if its seed is missing/corrupted in a conflicting restore.
    tx.execute("DELETE FROM dispatch_memory_legacy_dispatches WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
        params![start.key.agent_id,start.key.identity_incarnation,start.key.actual_dispatch_turn_id])?;
    persist_record(
        tx,
        &DispatchMemoryRecord::Started {
            start: start.clone(),
            original_start: original_start.clone(),
        },
    )
}

/// Private proof binds the original TX and frozen command. It grants no new
/// Work/evidence qualification and cannot be reconstructed after an unknown.
pub(crate) struct ValidatedDetachedMemory<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    record: DispatchMemoryRecord,
    original: PreparedInputMutation,
    frozen: FrozenEpisodeMemoryValues,
}

pub(crate) fn prepare_detached_memory_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    work: &WorkWriteProof,
    original: &PreparedInputMutation,
    frozen: &FrozenEpisodeMemoryValues,
) -> Result<Option<ValidatedDetachedMemory<'tx, 'connection>>> {
    validate_descriptor(original)?;
    ensure!(
        matches!(
            original.family(),
            InputMutationFamily::WorkItem
                | InputMutationFamily::Focus
                | InputMutationFamily::Timer
                | InputMutationFamily::Wait
                | InputMutationFamily::Task
                | InputMutationFamily::Queue
        ) && original.scopes().contains(work.scope()),
        "memory obligation requires its original Work whole command"
    );
    let facts = ContextDurableRead::new(tx, &work.scope().agent_id).scope_facts()?;
    let state = facts
        .state
        .context("dispatch memory requires current State")?;
    let Some(binding) = state.current_execution_binding.as_ref() else {
        // Claim ACK may create a run before StartedTurn. A run ID or a source
        // payload hint alone is not an actual dispatch or a memory seed.
        let Some(turn_id) = state.current_turn_id.as_deref() else {
            return Ok(None);
        };
        let actual = ContextDurableRead::new(tx, &work.scope().agent_id)
            .turn_by_id(turn_id)?
            .context("current actual Turn missing")?;
        ensure!(
            actual.terminal.is_some() && state.current_run_id.is_none(),
            "actual started producer has no original execution binding"
        );
        return Ok(None);
    };
    let read = ContextDurableRead::new(tx, &work.scope().agent_id);
    let actual_turn = read
        .turn_by_id(&binding.turn_id)?
        .context("actual bound Turn missing")?;
    if actual_turn.terminal.is_some() || state.current_turn_id.is_none() {
        return Ok(None);
    }
    ensure!(
        state.current_turn_id.as_deref() == Some(binding.turn_id.as_str())
            && state.turn_index == actual_turn.turn_index
            && actual_turn.run_id == state.current_run_id,
        "dispatch memory actual producer binding differs"
    );
    let key = EpisodeMemoryKey {
        agent_id: work.scope().agent_id.clone(),
        identity_incarnation: work.scope().identity_incarnation,
        actual_dispatch_turn_id: actual_turn.turn_id,
    };
    let invocation = EpisodeMemoryInvocation {
        message_id: binding.source_message_id.clone(),
        turn_index: state.turn_index,
    };
    let record = load_dispatch_memory_tx(tx, &key, &invocation)?
        .context(DispatchMemoryError::MissingStart)?;
    let start = record
        .start()
        .context(DispatchMemoryError::AlreadyFinalized)?;
    require_identity(tx, &key, true)?;
    validate_original_dispatch(tx, start)?;
    ensure!(
        state.working_memory == start.baseline,
        DispatchMemoryError::BaselineAdvanced
    );
    ensure!(
        episode_memory::load_receipt(tx, &key, None, &invocation)?.is_none(),
        DispatchMemoryError::AlreadyFinalized
    );
    // A late seed cannot borrow a Work body that changed before its proof.
    for work_id in work.admitted_work_ids() {
        work.validate_admission_tx(tx, &work_id)?;
    }
    validate_frozen(frozen)?;
    Ok(Some(ValidatedDetachedMemory {
        tx,
        record,
        original: original.clone(),
        frozen: frozen.clone(),
    }))
}

pub(crate) fn record_detached_memory_tx(
    validated: ValidatedDetachedMemory<'_, '_>,
    actual_work_keys: BTreeSet<WorkKey>,
    actual_input_refs: BTreeSet<InputEvidenceKey>,
) -> Result<DispatchMemoryRecord> {
    let ValidatedDetachedMemory {
        tx,
        record,
        original,
        frozen,
    } = validated;
    ensure!(
        !actual_work_keys.is_empty(),
        "memory obligation requires an actual Work write footprint"
    );
    let current = load_dispatch_memory_tx(tx, record.key(), record.invocation())?
        .context(DispatchMemoryError::MissingStart)?;
    ensure!(
        current == record,
        "dispatch memory changed within its original business transaction"
    );
    let start = record.start().expect("prepared unfinished start").clone();
    // Only already-qualified actual facts become retention/preparation refs.
    // The original reducer/finalizer still validates its actual write footprint.
    let mut roots = start.input_roots();
    roots.extend(actual_input_refs.iter().cloned());
    ExistingAdmissionInputProof::prepare_tx(tx, &scope(&start.key), roots, BTreeSet::new())?;
    let actual = validate_original_dispatch(tx, &start)?;
    ensure!(
        actual.working_memory == start.baseline && actual.turn_index == start.invocation.turn_index,
        DispatchMemoryError::BaselineAdvanced
    );
    bound_footprint(&actual_work_keys, &actual_input_refs)?;
    let member = DispatchMemoryMember {
        original,
        work_keys: actual_work_keys,
        input_refs: actual_input_refs,
    };
    let mut obligation = record
        .obligation()
        .cloned()
        .unwrap_or(DispatchMemoryObligation {
            frozen,
            members: BTreeMap::new(),
        });
    let nonce = member.original.command_nonce().to_string();
    if let Some(prior) = obligation.members.get(&nonce) {
        ensure!(prior == &member, DispatchMemoryError::MemberConflict);
    } else {
        // Bound before inserting; a discarded oversized candidate never builds
        // a second unbounded accumulated member map.
        bound_members_with(&obligation.members, &member)?;
        obligation.members.insert(nonce, member);
    }
    let result = DispatchMemoryRecord::Pending {
        start,
        original_start: record
            .original_start()
            .expect("prepared original Start")
            .clone(),
        obligation,
    };
    persist_record(tx, &result)?;
    Ok(result)
}

pub(crate) fn load_dispatch_memory_tx(
    tx: &Transaction<'_>,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
) -> Result<Option<DispatchMemoryRecord>> {
    load_dispatch_memory_in(tx, key, invocation)
}

fn load_dispatch_memory_in(
    tx: &Connection,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
) -> Result<Option<DispatchMemoryRecord>> {
    validate_key(key, invocation)?;
    let row: Option<(String, u64, String, String, String, usize)> = tx.query_row(
        "SELECT source_message_id,turn_index,state,seed_digest,record_digest,length(CAST(payload_json AS BLOB)) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
        params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).optional()?;
    let Some((source, index, status, seed_digest, record_digest, bytes)) = row else {
        return Ok(None);
    };
    ensure!(
        bytes <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    let payload: String = tx.query_row(
        "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
        params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id], |row| row.get(0),
    )?;
    let record: DispatchMemoryRecord = serde_json::from_str(&payload)?;
    record.validate()?;
    ensure!(
        record.key() == key
            && record.invocation() == invocation
            && source == invocation.message_id
            && index == invocation.turn_index
            && status == record.state()
            && seed_digest == record.seed_digest()
            && record_digest == canonical_command_digest(&record)?,
        "dispatch memory columns and payload disagree"
    );
    Ok(Some(record))
}

#[cfg(test)]
pub(crate) fn unfinished_dispatch_memory_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<Vec<DispatchMemoryRecord>> {
    let (count, total_bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND state!='done'",
        params![scope.agent_id,scope.identity_incarnation], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    ensure!(
        count <= MAX_METADATA_RECORDS && total_bytes <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    let mut query = tx.prepare("SELECT turn_id,source_message_id,turn_index FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND state!='done' ORDER BY turn_index,turn_id LIMIT ?3")?;
    let rows = query.query_map(
        params![
            scope.agent_id,
            scope.identity_incarnation,
            MAX_METADATA_RECORDS + 1
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        },
    )?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        ensure!(
            result.len() < MAX_METADATA_RECORDS,
            DispatchMemoryError::ResourceExhausted
        );
        let (turn, source, index) = row?;
        let record = load_dispatch_memory_tx(
            tx,
            &EpisodeMemoryKey {
                agent_id: scope.agent_id.clone(),
                identity_incarnation: scope.identity_incarnation,
                actual_dispatch_turn_id: turn,
            },
            &EpisodeMemoryInvocation {
                message_id: source,
                turn_index: index,
            },
        )?
        .context("unfinished dispatch disappeared")?;
        bytes = bytes
            .checked_add(bounded_json(&record)?.len())
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            bytes <= MAX_METADATA_BYTES,
            DispatchMemoryError::ResourceExhausted
        );
        result.push(record);
    }
    Ok(result)
}

/// Claim/new-Turn only. Same-actual-Turn tools never call this admission gate.
pub(crate) fn validate_new_dispatch_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<()> {
    let unfinished: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND state!='done')",
        params![scope.agent_id,scope.identity_incarnation], |row| row.get(0))?;
    ensure!(!unfinished, DispatchMemoryError::Unfinished);
    Ok(())
}

/// A fresh reader must prove every referenced whole Work receipt, rather than
/// treating the mutable obligation row as ACK for those commands.
pub(crate) fn validate_dispatch_memory_members_tx(
    tx: &Transaction<'_>,
    record: &DispatchMemoryRecord,
) -> Result<()> {
    validate_dispatch_memory_members_in(tx, record)
}

fn validate_dispatch_memory_members_in(
    tx: &Connection,
    record: &DispatchMemoryRecord,
) -> Result<()> {
    if let Some(original) = record.original_start() {
        super::input_mutation_receipts::lookup_input_mutation_receipt_in(tx, original)?
            .context("dispatch start has no original whole-Posture receipt")?;
    }
    if let Some(obligation) = record.obligation() {
        for member in obligation.members.values() {
            super::input_mutation_receipts::lookup_input_mutation_receipt_in(tx, &member.original)?
                .context("dispatch memory member has no original whole-transaction receipt")?;
        }
    }
    Ok(())
}

/// Original D1 receipt is mandatory. Pending->Done and its compact metadata are
/// written in the very TX that writes memory, never by a later best-effort ACK.
pub(crate) fn mark_dispatch_memory_done_tx(
    tx: &Transaction<'_>,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
) -> Result<DispatchMemoryRecord> {
    let record =
        load_dispatch_memory_tx(tx, key, invocation)?.context(DispatchMemoryError::MissingStart)?;
    let receipt = episode_memory::load_receipt(tx, key, None, invocation)?
        .context(DispatchMemoryError::MissingReceipt)?;
    let digest = canonical_command_digest(&receipt)?;
    if let DispatchMemoryRecord::Done { completed } = &record {
        ensure!(
            completed.receipt_digest == digest && completed.committed_at == receipt.committed_at,
            "completed dispatch and original D1 receipt differ"
        );
        return Ok(record);
    }
    validate_dispatch_memory_members_tx(tx, &record)?;
    let completed = DispatchMemoryDone {
        key: key.clone(),
        invocation: invocation.clone(),
        seed_digest: record.seed_digest().to_owned(),
        receipt_digest: digest,
        committed_at: receipt.committed_at,
    };
    let done = DispatchMemoryRecord::Done { completed };
    persist_record(tx, &done)?;
    Ok(done)
}

fn persist_record(tx: &Transaction<'_>, record: &DispatchMemoryRecord) -> Result<()> {
    record.validate()?;
    let payload = bounded_json(record)?;
    tx.execute("INSERT INTO dispatch_memory_checkpoints(agent_id,identity_incarnation,turn_id,source_message_id,turn_index,state,seed_digest,record_digest,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(agent_id,identity_incarnation,turn_id) DO UPDATE SET state=excluded.state,record_digest=excluded.record_digest,payload_json=excluded.payload_json",
        params![record.key().agent_id,record.key().identity_incarnation,record.key().actual_dispatch_turn_id,
            record.invocation().message_id,record.invocation().turn_index,record.state(),record.seed_digest(),
            canonical_command_digest(record)?,payload])?;
    ensure!(
        load_dispatch_memory_tx(tx, record.key(), record.invocation())?.as_ref() == Some(record),
        "dispatch memory write/readback mismatch"
    );
    Ok(())
}
fn require_identity(tx: &Transaction<'_>, key: &EpisodeMemoryKey, cleanup: bool) -> Result<()> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT status,payload_json FROM agent_identities WHERE agent_id=?1",
            [&key.agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (status, payload) = row.context("dispatch memory identity missing")?;
    let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
    ensure!(
        identity.agent_id == key.agent_id
            && identity.incarnation == key.identity_incarnation
            && serde_json::to_value(identity.status.clone())?.as_str() == Some(status.as_str())
            && (identity.status == AgentRegistryStatus::Active
                || (cleanup && identity.status == AgentRegistryStatus::Deleting)),
        "dispatch memory original identity retired or replaced"
    );
    Ok(())
}
fn validate_original_dispatch(
    tx: &Transaction<'_>,
    start: &DispatchMemoryStart,
) -> Result<AgentState> {
    let read = ContextDurableRead::new(tx, &start.key.agent_id);
    let source = read
        .message_by_id(&start.invocation.message_id)?
        .context("dispatch source missing")?;
    ensure!(
        source.message_seq == Some(start.source_sequence) && source.kind == start.source_kind,
        "dispatch source identity changed"
    );
    let turn = turn(tx, start)?;
    ensure!(
        turn.turn_index == start.invocation.turn_index
            && (turn
                .input_message_ids
                .contains(&start.invocation.message_id)
                || turn
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
                    == Some(start.invocation.message_id.as_str())),
        "actual dispatch does not own its original source"
    );
    ExistingAdmissionInputProof::prepare_tx(
        tx,
        &scope(&start.key),
        start.input_roots(),
        BTreeSet::new(),
    )?;
    read.scope_facts()?
        .into_owned_state()?
        .context("dispatch State missing")
}
fn turn(tx: &Transaction<'_>, start: &DispatchMemoryStart) -> Result<TurnRecord> {
    ContextDurableRead::new(tx, &start.key.agent_id)
        .turn_by_id(&start.key.actual_dispatch_turn_id)?
        .context("actual dispatch Turn missing")
}
fn scope(key: &EpisodeMemoryKey) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: key.agent_id.clone(),
        identity_incarnation: key.identity_incarnation,
    }
}
fn validate_key(key: &EpisodeMemoryKey, invocation: &EpisodeMemoryInvocation) -> Result<()> {
    ensure!(
        !key.agent_id.trim().is_empty()
            && !key.actual_dispatch_turn_id.trim().is_empty()
            && key.identity_incarnation > 0
            && key.identity_incarnation <= i64::MAX as u64
            && !invocation.message_id.trim().is_empty()
            && invocation.turn_index > 0
            && invocation.turn_index <= i64::MAX as u64,
        "invalid original dispatch memory address"
    );
    Ok(())
}
fn validate_descriptor(original: &PreparedInputMutation) -> Result<()> {
    let canonical = PreparedInputMutation::new(
        original.family(),
        original.command_nonce().to_owned(),
        original.scopes().to_vec(),
        original.command_digest().to_owned(),
        original.prepared_at(),
    )?;
    ensure!(
        canonical == *original,
        "dispatch member descriptor is not canonical"
    );
    Ok(())
}
fn validate_frozen(frozen: &FrozenEpisodeMemoryValues) -> Result<()> {
    ensure!(
        [
            &frozen.initial_builder_id,
            &frozen.fallback_builder_id,
            &frozen.next_builder_id,
            &frozen.audit_event_id
        ]
        .into_iter()
        .all(|id| !id.trim().is_empty()),
        "empty frozen dispatch memory ID"
    );
    Ok(())
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn bounded_json(value: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string(value)?;
    ensure!(
        json.len() <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    Ok(json)
}
fn bound_members_with(
    members: &BTreeMap<String, DispatchMemoryMember>,
    member: &DispatchMemoryMember,
) -> Result<()> {
    ensure!(
        members.len() < MAX_METADATA_RECORDS,
        DispatchMemoryError::ResourceExhausted
    );
    let mut bytes = bounded_json(member)?
        .len()
        .checked_add(member.original.command_nonce().len())
        .context(DispatchMemoryError::ResourceExhausted)?;
    let mut count = member
        .work_keys
        .len()
        .saturating_add(member.input_refs.len())
        .saturating_add(1);
    for existing in members.values() {
        bytes = bytes
            .checked_add(bounded_json(existing)?.len())
            .and_then(|value| value.checked_add(existing.original.command_nonce().len()))
            .context(DispatchMemoryError::ResourceExhausted)?;
        count = count
            .checked_add(existing.work_keys.len())
            .and_then(|value| value.checked_add(existing.input_refs.len()))
            .and_then(|value| value.checked_add(1))
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            bytes <= MAX_METADATA_BYTES && count <= MAX_METADATA_RECORDS,
            DispatchMemoryError::ResourceExhausted
        );
    }
    ensure!(
        bytes <= MAX_METADATA_BYTES && count <= MAX_METADATA_RECORDS,
        DispatchMemoryError::ResourceExhausted
    );
    Ok(())
}
fn bound_footprint(work: &BTreeSet<WorkKey>, inputs: &BTreeSet<InputEvidenceKey>) -> Result<()> {
    let count = work
        .len()
        .checked_add(inputs.len())
        .context(DispatchMemoryError::ResourceExhausted)?;
    let bytes = work
        .iter()
        .map(|key| key.id.len())
        .chain(inputs.iter().map(|key| key.id.len()))
        .try_fold(0usize, |bytes, next| bytes.checked_add(next))
        .context(DispatchMemoryError::ResourceExhausted)?;
    ensure!(
        count <= MAX_METADATA_RECORDS
            && bytes <= MAX_METADATA_BYTES
            && work.iter().all(|key| !key.id.trim().is_empty())
            && inputs.iter().all(|key| !key.id.trim().is_empty()),
        DispatchMemoryError::ResourceExhausted
    );
    Ok(())
}
fn bound_members(members: &BTreeMap<String, DispatchMemoryMember>) -> Result<()> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for member in members.values() {
        bound_footprint(&member.work_keys, &member.input_refs)?;
        bytes = bytes
            .checked_add(bounded_json(member)?.len())
            .and_then(|value| value.checked_add(member.original.command_nonce().len()))
            .context(DispatchMemoryError::ResourceExhausted)?;
        count = count
            .checked_add(member.work_keys.len())
            .and_then(|value| value.checked_add(member.input_refs.len()))
            .and_then(|value| value.checked_add(1))
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            bytes <= MAX_METADATA_BYTES && count <= MAX_METADATA_RECORDS,
            DispatchMemoryError::ResourceExhausted
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "dispatch_memory_tests.rs"]
mod tests;

#[path = "dispatch_memory_auto.rs"]
mod automatic;
pub(in crate::runtime_db) use automatic::insert_original_auto_memory_tx;
pub(crate) use automatic::{
    load_original_auto_memory_tx, OriginalAutoMemory, OriginalAutomaticMemory,
};

#[path = "dispatch_memory_binding.rs"]
mod binding;
#[path = "dispatch_memory_retired.rs"]
mod retired;
#[cfg(test)]
pub(crate) use retired::RetiredDispatchMemoryHandoff;
pub(crate) use retired::{
    load_retired_dispatch_memory_binding_tx, prepare_retired_physical_handoff_tx,
    retired_physical_handoff_exists_tx, RetiredPhysicalStopAddress,
};

#[path = "dispatch_memory_budget.rs"]
mod budget;
pub(in crate::runtime_db) use budget::DispatchAuditMaterial;
pub(crate) use budget::{AdmittedFinalWork, DispatchMemoryTransactionBudget};

pub(crate) use binding::{
    dispatch_memory_retention_roots_in, finish_dispatch_memory_tx,
    load_dispatch_memory_binding_by_turn_tx, load_dispatch_memory_binding_tx,
    prepare_dispatch_memory_finish_tx, validate_detached_binding, DispatchMemoryBinding,
    DispatchMemoryCommandPolicy, DispatchMemoryIntent, ValidatedDispatchMemoryFinish,
};

pub(super) fn validate_existing_dispatch_identity_tx(
    tx: &Transaction<'_>,
    key: &EpisodeMemoryKey,
) -> Result<()> {
    require_identity(tx, key, true)
}

/// Fixed v87 upgrade only. Ordinary identity creation/activation cannot call
/// this actor, and it stores only original current-dispatch address metadata.
pub(super) fn capture_legacy_dispatches_at_upgrade_tx(tx: &Transaction<'_>) -> Result<()> {
    // Released partial schemas may lack a dispatch source table. This fixed
    // migration seals no legacy dispatch; later activation never recaptures it.
    for table in [
        "agent_states",
        "agent_identities",
        "turn_records",
        "messages",
    ] {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(());
        }
    }
    let count:usize=tx.query_row("SELECT COUNT(*) FROM agent_states s JOIN agent_identities i ON i.agent_id=s.agent_id WHERE i.status IN ('active','deleting') AND json_type(s.payload_json,'$.current_execution_binding')='object' AND json_type(s.payload_json,'$.current_turn_id')='text'",[],|r|r.get(0))?;
    ensure!(
        count <= MAX_METADATA_RECORDS,
        DispatchMemoryError::ResourceExhausted
    );
    tx.execute("INSERT INTO dispatch_memory_legacy_dispatches(agent_id,identity_incarnation,turn_id,source_message_id,turn_index)
      SELECT s.agent_id,json_extract(i.payload_json,'$.incarnation'),t.turn_id,m.evidence_id,t.turn_index
      FROM agent_states s JOIN agent_identities i ON i.agent_id=s.agent_id
      JOIN turn_records t ON t.agent_id=s.agent_id AND t.turn_id=json_extract(s.payload_json,'$.current_turn_id')
      JOIN messages m ON m.agent_id=s.agent_id AND m.evidence_id=json_extract(s.payload_json,'$.current_execution_binding.source_message_id')
      WHERE i.status IN ('active','deleting') AND json_extract(i.payload_json,'$.agent_id')=i.agent_id
      AND json_extract(i.payload_json,'$.status')=i.status AND json_extract(i.payload_json,'$.incarnation')>0
      AND t.turn_index=s.turn_index AND json_extract(s.payload_json,'$.turn_index')=s.turn_index
      AND json_extract(s.payload_json,'$.current_execution_binding.turn_id')=t.turn_id
      AND json_extract(t.payload_json,'$.run_id') IS s.current_run_id
      AND json_extract(s.payload_json,'$.current_run_id') IS s.current_run_id
      AND (EXISTS(SELECT 1 FROM json_each(json_extract(t.payload_json,'$.input_message_ids')) WHERE value=m.evidence_id)
           OR json_extract(t.payload_json,'$.trigger.message_id')=m.evidence_id)",[])?;
    let bytes:usize=tx.query_row("SELECT COALESCE(SUM(length(agent_id)+length(turn_id)+length(source_message_id)+32),0) FROM dispatch_memory_legacy_dispatches",[],|r|r.get(0))?;
    ensure!(
        bytes <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    Ok(())
}

pub(crate) fn is_legacy_dispatch_tx(
    tx: &Transaction<'_>,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
) -> Result<bool> {
    validate_key(key, invocation)?;
    tx.query_row("SELECT EXISTS(SELECT 1 FROM dispatch_memory_legacy_dispatches WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3 AND source_message_id=?4 AND turn_index=?5)",params![key.agent_id,key.identity_incarnation,key.actual_dispatch_turn_id,invocation.message_id,invocation.turn_index],|r|r.get(0)).map_err(Into::into)
}
