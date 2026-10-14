//! Atomic post-dispatch memory transitions and their immutable command results.

use std::{collections::BTreeSet, fmt};

use anyhow::{anyhow, bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    memory::{
        episode_transition::{EpisodeMemoryOutcome, EpisodeMemoryPlan},
        refs::RuntimeRef,
    },
    runtime_db::{
        evidence::{
            append_audit_event_tx, insert_runtime_index_changes_at_tx, upsert_agent_state_at_tx,
        },
        transitions::{AgentStateMutation, PostCommitEffects, TransitionCommit},
        RuntimeDb, RuntimeIndexChange, RuntimeIndexOperation, RuntimeStateTransitionConflict,
    },
    types::{
        AgentIdentityRecord, AgentRegistryStatus, AgentState, ContextEpisodeRecord, TurnRecord,
    },
};

pub(crate) const EPISODE_MEMORY_PHASE: &str = "post_dispatch_memory/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpisodeMemoryKey {
    pub agent_id: String,
    pub identity_incarnation: u64,
    pub actual_dispatch_turn_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpisodeMemoryInvocation {
    pub message_id: String,
    pub turn_index: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct EpisodeMemoryTransitionCommand {
    key: EpisodeMemoryKey,
    invocation: EpisodeMemoryInvocation,
    expected_state: Box<AgentState>,
    plan: EpisodeMemoryPlan,
    committed_at: DateTime<Utc>,
    command_digest: String,
    #[cfg(test)]
    fault: Option<EpisodeMemoryFaultPoint>,
}

impl EpisodeMemoryTransitionCommand {
    pub(crate) fn new(
        key: EpisodeMemoryKey,
        invocation: EpisodeMemoryInvocation,
        expected_state: AgentState,
        mut plan: EpisodeMemoryPlan,
        committed_at: DateTime<Utc>,
    ) -> Result<Self> {
        validate_key(&key, &invocation)?;
        plan.outcome.business_fields_changed = expected_state != plan.candidate;
        let digest_input = serde_json::json!({
            "phase": EPISODE_MEMORY_PHASE,
            "key": key,
            "invocation": invocation,
            "expected_state": expected_state,
            "candidate_state": plan.candidate,
            "episode": plan.finalized_episode,
            "audit_events": plan.audit_events,
            "outcome": plan.outcome,
            "committed_at": committed_at,
        });
        let command_digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&canonical_json(digest_input))?)
        );
        Ok(Self {
            key,
            invocation,
            expected_state: Box::new(expected_state),
            plan,
            committed_at,
            command_digest,
            #[cfg(test)]
            fault: None,
        })
    }

    pub(crate) fn key(&self) -> &EpisodeMemoryKey {
        &self.key
    }
    pub(crate) fn invocation(&self) -> &EpisodeMemoryInvocation {
        &self.invocation
    }
    pub(crate) fn command_digest(&self) -> &str {
        &self.command_digest
    }
    #[cfg(test)]
    pub(crate) fn candidate(&self) -> &AgentState {
        &self.plan.candidate
    }

    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: EpisodeMemoryFaultPoint) -> Self {
        self.fault = Some(fault);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpisodeMemoryAuditBinding {
    pub audit_event_id: String,
    pub event_seq: u64,
    pub event_log_epoch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpisodeMemoryIndexBinding {
    pub change_seq: i64,
    pub source_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpisodeMemoryReceipt {
    pub phase: String,
    pub key: EpisodeMemoryKey,
    pub invocation: EpisodeMemoryInvocation,
    pub command_digest: String,
    pub outcome: EpisodeMemoryOutcome,
    pub agent_control_revision: u64,
    pub audit_bindings: Vec<EpisodeMemoryAuditBinding>,
    pub index_bindings: Vec<EpisodeMemoryIndexBinding>,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) enum EpisodeMemoryCommit {
    Applied {
        receipt: EpisodeMemoryReceipt,
        transition: TransitionCommit,
        episode: Option<ContextEpisodeRecord>,
    },
    Replayed {
        receipt: EpisodeMemoryReceipt,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EpisodeMemoryError {
    CommandConflict,
    InvocationConflict,
    EmbeddedPhaseCollision,
    LegacyEpisodeCollision { episode_id: String },
    ImmutableEpisodeConflict { episode_id: String },
}

impl fmt::Display for EpisodeMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandConflict => {
                f.write_str("episode memory command identity has a different payload")
            }
            Self::InvocationConflict => {
                f.write_str("episode memory invocation binding does not match")
            }
            Self::EmbeddedPhaseCollision => {
                f.write_str("episode memory phase exists without the enclosing command receipt")
            }
            Self::LegacyEpisodeCollision { episode_id } => write!(
                f,
                "legacy episode collision without a complete command receipt: {episode_id}"
            ),
            Self::ImmutableEpisodeConflict { episode_id } => {
                write!(f, "immutable episode content conflict: {episode_id}")
            }
        }
    }
}
impl std::error::Error for EpisodeMemoryError {}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpisodeMemoryFaultPoint {
    AfterValidation,
    AfterEpisodeWrite,
    AfterStateWrite,
    AfterAuditWrites,
    AfterIndexWrites,
    AfterReceiptWrite,
    AfterCommitBeforeAck,
}

impl RuntimeDb {
    pub(crate) fn commit_episode_memory(
        &self,
        command: &EpisodeMemoryTransitionCommand,
    ) -> Result<EpisodeMemoryCommit> {
        let committed = self.transaction(|tx| commit_episode_memory_tx(tx, command))?;
        #[cfg(test)]
        if command.fault == Some(EpisodeMemoryFaultPoint::AfterCommitBeforeAck)
            && matches!(&committed, EpisodeMemoryCommit::Applied { .. })
        {
            return Err(super::persistence::mark_persistence_failure(
                anyhow!("injected episode memory lost commit acknowledgement"),
                super::PersistenceCertainty::Indeterminate,
                super::PersistenceFailurePhase::Commit,
                None,
            ));
        }
        Ok(committed)
    }

    /// `None` digest is reserved for a trusted original dispatch/outcome query;
    /// invocation and durable incarnation are still mandatory and exact.
    pub(crate) fn episode_memory_receipt(
        &self,
        key: &EpisodeMemoryKey,
        command_digest: Option<&str>,
        invocation: &EpisodeMemoryInvocation,
    ) -> Result<Option<EpisodeMemoryReceipt>> {
        validate_key(key, invocation)?;
        load_receipt(&self.connection()?, key, command_digest, invocation)
    }
}

/// Apply the original Episode command within the caller's existing transaction.
/// This does not commit, publish effects, or own the outer commit acknowledgement.
/// Historical replay and the original lifecycle/State OCC checks remain exact.
pub(crate) fn commit_episode_memory_tx(
    tx: &Transaction<'_>,
    command: &EpisodeMemoryTransitionCommand,
) -> Result<EpisodeMemoryCommit> {
    // Replay precedes business OCC and lifecycle validation. The entrypoint
    // must authenticate and authorize the requested historical scope.
    if let Some(receipt) = load_receipt(
        tx,
        &command.key,
        Some(&command.command_digest),
        &command.invocation,
    )? {
        return Ok(EpisodeMemoryCommit::Replayed { receipt });
    }
    validate_new_command(tx, command)?;
    #[cfg(test)]
    inject_fault(command, EpisodeMemoryFaultPoint::AfterValidation)?;

    write_validated_episode_memory_tx(
        tx,
        command,
        &command.plan.candidate,
        command.plan.outcome.business_fields_changed,
    )
}

// Only this module can mint the token. It borrows the original transaction and
// is consumed once; possession does not prove the enclosing transaction commits.
pub(crate) struct ValidatedEpisodeMemory<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    key: &'tx EpisodeMemoryKey,
    invocation: &'tx EpisodeMemoryInvocation,
    expected_state: &'tx AgentState,
    existing_dispatch: bool,
}

/// Call only after the enclosing command's receipt lookup reports absence.
/// A pre-existing phase cannot acknowledge or skip the enclosing business work.
#[cfg(test)]
pub(crate) fn prepare_embedded_episode_memory_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    key: &'tx EpisodeMemoryKey,
    invocation: &'tx EpisodeMemoryInvocation,
    expected_state: &'tx AgentState,
) -> Result<ValidatedEpisodeMemory<'tx, 'connection>> {
    validate_key(key, invocation)?;
    if load_receipt(tx, key, None, invocation)?.is_some() {
        return Err(EpisodeMemoryError::EmbeddedPhaseCollision.into());
    }
    validate_identity_tx(tx, key)?;
    ensure!(
        expected_state.id == key.agent_id,
        "episode memory state identity mismatch"
    );
    validate_expected_state_tx(tx, key, expected_state)?;
    ensure!(
        expected_state.turn_index == invocation.turn_index,
        "episode memory dispatch turn index mismatch"
    );
    validate_dispatch_tx(tx, key, invocation)?;
    validate_unfinalized_builder_tx(tx, expected_state)?;
    Ok(ValidatedEpisodeMemory {
        tx,
        key,
        invocation,
        expected_state,
        existing_dispatch: false,
    })
}

/// The original enclosing command validates its State OCC once. This token
/// can only borrow the pre-business State captured by the dispatch proof in
/// this same transaction; ordinary D1 callers retain their Active-only OCC.
pub(crate) fn prepare_existing_dispatch_episode_memory_tx<'tx, 'connection>(
    proof: &'tx super::dispatch_memory::ValidatedDispatchMemoryFinish<'_, 'connection>,
) -> Result<ValidatedEpisodeMemory<'tx, 'connection>> {
    let (tx, key, invocation, expected_state) = proof.embedded_inputs();
    validate_key(key, invocation)?;
    if load_receipt(tx, key, None, invocation)?.is_some() {
        return Err(EpisodeMemoryError::EmbeddedPhaseCollision.into());
    }
    super::dispatch_memory::validate_existing_dispatch_identity_tx(tx, key)?;
    ensure!(
        expected_state.id == key.agent_id && expected_state.turn_index == invocation.turn_index,
        "existing dispatch memory pre-state differs"
    );
    validate_dispatch_tx(tx, key, invocation)?;
    validate_unfinalized_builder_tx(tx, expected_state)?;
    Ok(ValidatedEpisodeMemory {
        tx,
        key,
        invocation,
        expected_state,
        existing_dispatch: true,
    })
}

/// Finish after business writes and the final count/State epilogue. The caller
/// must not write State again after this receipt captures its control revision.
pub(crate) fn write_embedded_episode_memory_tx(
    validated: ValidatedEpisodeMemory<'_, '_>,
    command: &EpisodeMemoryTransitionCommand,
    final_business_state: &AgentState,
) -> Result<EpisodeMemoryCommit> {
    ensure!(
        command.key == *validated.key
            && command.invocation == *validated.invocation
            && command.expected_state.as_ref() == validated.expected_state,
        "embedded episode command differs from its validated pre-state"
    );
    let actual = state_tx(validated.tx, &validated.key.agent_id)?
        .context("embedded episode final business State is missing")?;
    ensure!(
        actual == *final_business_state,
        "embedded episode final business State was not read back exactly"
    );
    ensure!(
        actual.working_memory == validated.expected_state.working_memory,
        "embedded episode business writes changed the memory baseline"
    );
    ensure!(
        actual.turn_index == validated.invocation.turn_index,
        "embedded episode business State advanced another dispatch"
    );

    // Compare every non-memory field, including future AgentState additions.
    // No stale focus/control/count/workspace field may be copied from the plan.
    let mut planned_business = command.plan.candidate.clone();
    planned_business.working_memory = actual.working_memory.clone();
    ensure!(
        planned_business == actual,
        "embedded episode plan changed a non-memory business field"
    );
    let mut candidate = actual.clone();
    candidate.working_memory = command.plan.candidate.working_memory.clone();
    // Business work may touch the actual Turn; recheck its final ownership and
    // the original identity without rerunning the old State OCC.
    if validated.existing_dispatch {
        super::dispatch_memory::validate_existing_dispatch_identity_tx(
            validated.tx,
            validated.key,
        )?;
    } else {
        validate_identity_tx(validated.tx, validated.key)?;
    }
    validate_dispatch_tx(validated.tx, validated.key, validated.invocation)?;
    validate_memory_plan_tx(validated.tx, command)?;
    #[cfg(test)]
    inject_fault(command, EpisodeMemoryFaultPoint::AfterValidation)?;
    let persist_state = candidate != actual;
    write_validated_episode_memory_tx(validated.tx, command, &candidate, persist_state)
}

fn write_validated_episode_memory_tx(
    tx: &Transaction<'_>,
    command: &EpisodeMemoryTransitionCommand,
    candidate: &AgentState,
    persist_state: bool,
) -> Result<EpisodeMemoryCommit> {
    if let Some(episode) = command.plan.finalized_episode.as_ref() {
        insert_immutable_episode(tx, episode)?;
    }
    #[cfg(test)]
    inject_fault(command, EpisodeMemoryFaultPoint::AfterEpisodeWrite)?;

    if persist_state {
        upsert_agent_state_at_tx(tx, candidate, command.committed_at)?;
        let stored_payload: String = tx.query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [&command.key.agent_id],
            |row| row.get(0),
        )?;
        let stored: AgentState = serde_json::from_str(&stored_payload)?;
        ensure!(
            stored == *candidate,
            "episode memory candidate was not persisted exactly"
        );
    }
    #[cfg(test)]
    inject_fault(command, EpisodeMemoryFaultPoint::AfterStateWrite)?;

    let mut audit_events = Vec::new();
    let mut audit_bindings = Vec::new();
    for event in &command.plan.audit_events {
        let (event, inserted) = append_audit_event_tx(tx, Some(&command.key.agent_id), event)?;
        audit_bindings.push(EpisodeMemoryAuditBinding {
            audit_event_id: event.id.clone(),
            event_seq: event.event_seq,
            event_log_epoch: event.event_log_epoch.clone(),
        });
        if inserted {
            audit_events.push(event);
        }
    }
    #[cfg(test)]
    inject_fault(command, EpisodeMemoryFaultPoint::AfterAuditWrites)?;

    let mut index_bindings = Vec::new();
    if let Some(episode) = command.plan.finalized_episode.as_ref() {
        let change = RuntimeIndexChange {
            agent_id: episode.agent_id.clone(),
            source_kind: "context_episode".into(),
            source_id: episode.id.clone(),
            source_ref: format!("episode:{}", episode.id),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(episode.finalized_at),
            reason: "context_episode_written".into(),
        };
        insert_runtime_index_changes_at_tx(tx, &[change.clone()], command.committed_at)?;
        let change_seq = tx.query_row(
            "SELECT produced_change_seq FROM runtime_index_outbox_watermarks WHERE agent_id = ?1",
            [&command.key.agent_id],
            |row| row.get::<_, i64>(0),
        )?;
        index_bindings.push(EpisodeMemoryIndexBinding {
            change_seq,
            source_ref: change.source_ref,
        });
    }
    #[cfg(test)]
    inject_fault(command, EpisodeMemoryFaultPoint::AfterIndexWrites)?;

    let control_revision: i64 = tx.query_row(
        "SELECT control_revision FROM agent_states WHERE agent_id = ?1",
        [&command.key.agent_id],
        |row| row.get(0),
    )?;
    let receipt = EpisodeMemoryReceipt {
        phase: EPISODE_MEMORY_PHASE.into(),
        key: command.key.clone(),
        invocation: command.invocation.clone(),
        command_digest: command.command_digest.clone(),
        outcome: command.plan.outcome.clone(),
        agent_control_revision: u64::try_from(control_revision)
            .context("invalid agent control revision")?,
        audit_bindings,
        index_bindings,
        committed_at: command.committed_at,
    };
    tx.execute(
        "INSERT INTO episode_memory_results
         (agent_id, identity_incarnation, turn_id, phase_version, command_digest,
          invocation_json, finalized_episode_id, result_json, committed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            receipt.key.agent_id,
            i64::try_from(receipt.key.identity_incarnation)?,
            receipt.key.actual_dispatch_turn_id,
            EPISODE_MEMORY_PHASE,
            receipt.command_digest,
            serde_json::to_string(&receipt.invocation)?,
            receipt.outcome.finalized_episode_id,
            serde_json::to_string(&receipt)?,
            receipt.committed_at.to_rfc3339(),
        ],
    )?;
    #[cfg(test)]
    inject_fault(command, EpisodeMemoryFaultPoint::AfterReceiptWrite)?;

    Ok(EpisodeMemoryCommit::Applied {
        receipt,
        // Even a business no-op has committed a new durable receipt.
        transition: TransitionCommit {
            applied: true,
            effects: Box::new(PostCommitEffects {
                agent_state: Some(AgentStateMutation {
                    expected: Some(command.expected_state.clone()),
                    record: Box::new(candidate.clone()),
                }),
                audit_events,
                notify_memory_index: command.plan.finalized_episode.is_some(),
                ..Default::default()
            }),
            ..Default::default()
        },
        episode: command.plan.finalized_episode.clone(),
    })
}

fn validate_key(key: &EpisodeMemoryKey, invocation: &EpisodeMemoryInvocation) -> Result<()> {
    ensure!(
        !key.agent_id.trim().is_empty() && !key.actual_dispatch_turn_id.trim().is_empty(),
        "episode memory key is empty"
    );
    ensure!(
        key.identity_incarnation > 0,
        "episode memory requires a durable identity incarnation"
    );
    ensure!(
        !invocation.message_id.trim().is_empty() && invocation.turn_index > 0,
        "episode memory invocation is empty"
    );
    Ok(())
}

pub(super) fn load_receipt(
    connection: &Connection,
    key: &EpisodeMemoryKey,
    command_digest: Option<&str>,
    invocation: &EpisodeMemoryInvocation,
) -> Result<Option<EpisodeMemoryReceipt>> {
    let row = connection
        .query_row(
            "SELECT command_digest, invocation_json, finalized_episode_id, result_json, committed_at
         FROM episode_memory_results
         WHERE agent_id = ?1 AND identity_incarnation = ?2 AND turn_id = ?3 AND phase_version = ?4",
            params![
                key.agent_id,
                i64::try_from(key.identity_incarnation)?,
                key.actual_dispatch_turn_id,
                EPISODE_MEMORY_PHASE
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_digest, stored_invocation, finalized_episode_id, payload, committed_at)) = row
    else {
        return Ok(None);
    };
    if command_digest.is_some_and(|digest| digest != stored_digest) {
        return Err(EpisodeMemoryError::CommandConflict.into());
    }
    if serde_json::from_str::<EpisodeMemoryInvocation>(&stored_invocation)? != *invocation {
        return Err(EpisodeMemoryError::InvocationConflict.into());
    }
    let receipt: EpisodeMemoryReceipt = serde_json::from_str(&payload)?;
    ensure!(
        receipt.key == *key
            && receipt.phase == EPISODE_MEMORY_PHASE
            && receipt.command_digest == stored_digest
            && receipt.invocation == *invocation
            && receipt.outcome.finalized_episode_id == finalized_episode_id
            && receipt.committed_at.to_rfc3339() == committed_at,
        "episode memory receipt columns and payload disagree"
    );
    Ok(Some(receipt))
}

fn validate_new_command(
    tx: &Transaction<'_>,
    command: &EpisodeMemoryTransitionCommand,
) -> Result<()> {
    let key = &command.key;
    validate_identity_tx(tx, key)?;
    ensure!(
        command.expected_state.id == key.agent_id && command.plan.candidate.id == key.agent_id,
        "episode memory state identity mismatch"
    );
    validate_expected_state_tx(tx, key, command.expected_state.as_ref())?;
    ensure!(
        command.plan.candidate.turn_index == command.invocation.turn_index
            && command.expected_state.turn_index == command.invocation.turn_index,
        "episode memory dispatch turn index mismatch"
    );
    validate_dispatch_tx(tx, key, &command.invocation)?;
    validate_unfinalized_builder_tx(tx, command.expected_state.as_ref())?;
    validate_memory_plan_tx(tx, command)
}

fn validate_identity_tx(tx: &Transaction<'_>, key: &EpisodeMemoryKey) -> Result<()> {
    let identity = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [&key.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<AgentIdentityRecord>(&payload))
        .transpose()?;
    ensure!(
        identity
            .as_ref()
            .is_some_and(|identity| identity.agent_id == key.agent_id
                && identity.incarnation == key.identity_incarnation
                && identity.status == AgentRegistryStatus::Active),
        "episode memory durable identity incarnation is no longer active"
    );
    Ok(())
}

fn state_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<AgentState>> {
    tx.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
        [agent_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
    .transpose()
}

fn validate_expected_state_tx(
    tx: &Transaction<'_>,
    key: &EpisodeMemoryKey,
    expected_state: &AgentState,
) -> Result<()> {
    let actual = state_tx(tx, &key.agent_id)?;
    if actual.as_ref() != Some(expected_state) {
        return Err(RuntimeStateTransitionConflict::concurrent_mutation(
            "agent_state",
            &key.agent_id,
        )
        .into());
    }
    Ok(())
}

fn validate_dispatch_tx(
    tx: &Transaction<'_>,
    key: &EpisodeMemoryKey,
    invocation: &EpisodeMemoryInvocation,
) -> Result<()> {
    let dispatch = turn_tx(tx, &key.actual_dispatch_turn_id)?
        .ok_or_else(|| anyhow!("episode memory dispatch turn is missing"))?;
    ensure!(
        dispatch.agent_id == key.agent_id && dispatch.turn_index == invocation.turn_index,
        "episode memory dispatch turn belongs to another scope"
    );
    ensure!(
        dispatch.input_message_ids.contains(&invocation.message_id)
            || dispatch
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                == Some(invocation.message_id.as_str()),
        "episode memory dispatch does not own its invocation message"
    );
    Ok(())
}

fn validate_unfinalized_builder_tx(
    tx: &Transaction<'_>,
    expected_state: &AgentState,
) -> Result<()> {
    if let Some(builder) = expected_state
        .working_memory
        .active_episode_builder
        .as_ref()
    {
        if episode_tx(tx, &builder.id)?.is_some() {
            return Err(EpisodeMemoryError::LegacyEpisodeCollision {
                episode_id: builder.id.clone(),
            }
            .into());
        }
    }
    Ok(())
}

fn validate_memory_plan_tx(
    tx: &Transaction<'_>,
    command: &EpisodeMemoryTransitionCommand,
) -> Result<()> {
    let key = &command.key;
    if let Some(builder) = command
        .plan
        .candidate
        .working_memory
        .active_episode_builder
        .as_ref()
    {
        ensure!(
            command
                .plan
                .candidate
                .working_memory
                .active_episode_id
                .as_deref()
                == Some(builder.id.as_str()),
            "episode memory active builder alias mismatch"
        );
        validate_turn_sources(
            tx,
            &key.agent_id,
            builder.start_turn_index,
            builder.latest_turn_index,
            &builder.source_turn_ids,
        )?;
    } else {
        ensure!(
            command
                .plan
                .candidate
                .working_memory
                .active_episode_id
                .is_none(),
            "episode memory active alias has no builder"
        );
    }
    if let Some(episode) = command.plan.finalized_episode.as_ref() {
        ensure!(
            episode.agent_id == key.agent_id,
            "episode memory archive identity mismatch"
        );
        validate_turn_sources(
            tx,
            &key.agent_id,
            episode.start_turn_index,
            episode.end_turn_index,
            &episode.source_turn_ids,
        )?;
        let mut refs = BTreeSet::new();
        for source_ref in &episode.source_refs {
            let RuntimeRef::Turn { id } = RuntimeRef::parse(source_ref).map_err(|error| {
                anyhow!("invalid episode source ref: {}", error.validation_error())
            })?
            else {
                bail!("episode memory source ref is not a turn");
            };
            ensure!(
                episode.source_turn_ids.contains(&id) && refs.insert(id),
                "episode source refs do not match turn sources"
            );
        }
        ensure!(
            refs.len() == episode.source_turn_ids.len(),
            "episode source refs omit a turn source"
        );
    }
    Ok(())
}

fn validate_turn_sources(
    tx: &Transaction<'_>,
    agent_id: &str,
    start: u64,
    end: u64,
    sources: &[String],
) -> Result<()> {
    ensure!(start > 0 && end >= start, "episode turn range is invalid");
    let mut unique = BTreeSet::new();
    for source in sources {
        ensure!(unique.insert(source), "episode has duplicate turn sources");
        let turn = turn_tx(tx, source)?
            .ok_or_else(|| anyhow!("episode source turn is missing: {source}"))?;
        ensure!(
            turn.agent_id == agent_id && (start..=end).contains(&turn.turn_index),
            "episode source turn is outside its agent/range: {source}"
        );
    }
    Ok(())
}

fn turn_tx(tx: &Transaction<'_>, turn_id: &str) -> Result<Option<TurnRecord>> {
    tx.query_row(
        "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
        [turn_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
    .transpose()
}

fn episode_tx(tx: &Transaction<'_>, episode_id: &str) -> Result<Option<ContextEpisodeRecord>> {
    tx.query_row(
        "SELECT payload_json FROM context_episode_anchors WHERE episode_id = ?1",
        [episode_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
    .transpose()
}

fn insert_immutable_episode(tx: &Transaction<'_>, episode: &ContextEpisodeRecord) -> Result<()> {
    if let Some(existing) = episode_tx(tx, &episode.id)? {
        return Err(if existing == *episode {
            EpisodeMemoryError::LegacyEpisodeCollision {
                episode_id: episode.id.clone(),
            }
        } else {
            EpisodeMemoryError::ImmutableEpisodeConflict {
                episode_id: episode.id.clone(),
            }
        }
        .into());
    }
    let boundary = serde_json::to_value(episode.boundary_reason)?;
    let boundary_reason = boundary
        .as_str()
        .context("invalid episode boundary reason")?;
    tx.execute(
        "INSERT INTO context_episode_anchors
         (episode_id, agent_id, workspace_id, work_item_id, boundary_reason,
          start_turn_index, end_turn_index, started_at, ended_at, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            episode.id,
            episode.agent_id,
            episode.workspace_id,
            episode.current_work_item_id,
            boundary_reason,
            i64::try_from(episode.start_turn_index)?,
            i64::try_from(episode.end_turn_index)?,
            episode.created_at.to_rfc3339(),
            episode.finalized_at.to_rfc3339(),
            serde_json::to_string(episode)?
        ],
    )?;
    Ok(())
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => {
            let sorted = object
                .into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect::<std::collections::BTreeMap<_, _>>();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical_json).collect())
        }
        other => other,
    }
}

#[cfg(test)]
fn inject_fault(
    command: &EpisodeMemoryTransitionCommand,
    point: EpisodeMemoryFaultPoint,
) -> Result<()> {
    ensure!(
        command.fault != Some(point),
        "injected episode memory transaction fault at {point:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::episode_transition::{
            plan_episode_memory, EpisodeMemoryPlanInput, FrozenEpisodeMemoryValues,
        },
        types::{
            AgentKind, AgentOwnership, AgentProfilePreset, AgentStatus, AgentVisibility,
            AuthorityClass, ClosureDecision, ClosureOutcome, MessageBody, MessageEnvelope,
            MessageKind, MessageOrigin, Priority, RuntimePosture, WaitingReason,
            WorkingMemorySnapshot,
        },
    };

    struct Fixture {
        _directory: tempfile::TempDir,
        db: RuntimeDb,
        state: AgentState,
        message: MessageEnvelope,
        timestamp: DateTime<Utc>,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let db = RuntimeDb::open_and_migrate(
                directory.path().join("runtime.sqlite"),
                directory.path().join("runtime.lock"),
            )
            .unwrap();
            let timestamp = "2026-10-12T00:00:00Z".parse().unwrap();
            let identity = AgentIdentityRecord::new(
                "agent-a",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            db.agent_identities().upsert(&identity).unwrap();
            let mut state = AgentState::new("agent-a");
            state.status = AgentStatus::AwakeIdle;
            state.turn_index = 1;
            state.current_turn_id = Some("turn-1".into());
            let mut message = MessageEnvelope::new(
                "agent-a",
                MessageKind::InternalFollowup,
                MessageOrigin::System {
                    subsystem: "episode-test".into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "continue".into(),
                },
            );
            message.id = "message-1".into();
            let mut turn = TurnRecord::new(&state.id, "turn-1", 1);
            turn.created_at = timestamp;
            turn.input_message_ids.push(message.id.clone());
            // Started turns are valid episode sources; terminal is deliberately None.
            db.turn_records().upsert(&turn).unwrap();
            db.agent_states().upsert_for_fixture(&state).unwrap();
            Self {
                _directory: directory,
                db,
                state,
                message,
                timestamp,
            }
        }

        fn command(&self, finalize: bool) -> EpisodeMemoryTransitionCommand {
            let snapshot = if finalize {
                WorkingMemorySnapshot {
                    objective: Some("finish objective".into()),
                    work_summary: Some("progress".into()),
                    ..Default::default()
                }
            } else {
                WorkingMemorySnapshot::default()
            };
            let prior = closure(false);
            let current = closure(finalize);
            let frozen = FrozenEpisodeMemoryValues {
                timestamp: self.timestamp,
                initial_builder_id: "episode-1".into(),
                fallback_builder_id: "episode-fallback".into(),
                next_builder_id: "episode-2".into(),
                audit_event_id: "episode-audit-1".into(),
            };
            let plan = plan_episode_memory(
                EpisodeMemoryPlanInput {
                    agent: &self.state,
                    actual_dispatch_turn_id: "turn-1",
                    trigger: &self.message,
                    prior_closure: &prior,
                    current_closure: &current,
                    previous_snapshot: &snapshot,
                    current_snapshot: &snapshot,
                },
                &frozen,
            );
            self.command_from_plan(plan)
        }

        fn command_from_plan(&self, plan: EpisodeMemoryPlan) -> EpisodeMemoryTransitionCommand {
            EpisodeMemoryTransitionCommand::new(
                EpisodeMemoryKey {
                    agent_id: self.state.id.clone(),
                    identity_incarnation: 1,
                    actual_dispatch_turn_id: "turn-1".into(),
                },
                EpisodeMemoryInvocation {
                    message_id: self.message.id.clone(),
                    turn_index: 1,
                },
                self.state.clone(),
                plan,
                self.timestamp,
            )
            .unwrap()
        }

        fn count(&self, table: &str) -> i64 {
            self.db
                .connection()
                .unwrap()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        }

        fn durable_state(&self) -> AgentState {
            self.db
                .agent_states()
                .latest(&self.state.id)
                .unwrap()
                .unwrap()
        }

        fn revision(&self) -> i64 {
            self.db
                .connection()
                .unwrap()
                .query_row(
                    "SELECT control_revision FROM agent_states WHERE agent_id = 'agent-a'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        }

        fn sequences(&self) -> Vec<(String, String, i64)> {
            let connection = self.db.connection().unwrap();
            let mut statement = connection.prepare("SELECT domain, scope_key, last_value FROM runtime_sequences ORDER BY domain, scope_key").unwrap();
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        }
    }

    fn closure(waiting: bool) -> ClosureDecision {
        ClosureDecision {
            outcome: if waiting {
                ClosureOutcome::Waiting
            } else {
                ClosureOutcome::Completed
            },
            waiting_reason: waiting.then_some(WaitingReason::AwaitingOperatorInput),
            work_signal: None,
            runtime_posture: if waiting {
                RuntimePosture::Sleeping
            } else {
                RuntimePosture::Awake
            },
            evidence: Vec::new(),
        }
    }

    fn embedded_business_state_tx(fixture: &Fixture, tx: &Transaction<'_>) -> AgentState {
        let work = crate::types::WorkItemRecord::new(
            &fixture.state.id,
            "actual final business owner",
            crate::types::WorkItemState::Open,
        );
        super::super::repositories::insert_new_work_item_tx(tx, &work).unwrap();
        let mut state = fixture.state.clone();
        state.status = AgentStatus::Stopped;
        state.current_run_id = None;
        state.current_turn_id = None;
        state.current_work_item_id = Some(work.id);
        state.pending = 3;
        state.total_message_count = 7;
        state.total_input_tokens = 11;
        state.total_output_tokens = 13;
        state.last_wake_reason = Some("actual compound settlement".into());
        upsert_agent_state_at_tx(tx, &state, fixture.timestamp).unwrap();
        state
    }

    fn embedded_command(
        fixture: &Fixture,
        final_business_state: &AgentState,
    ) -> EpisodeMemoryTransitionCommand {
        let snapshot = WorkingMemorySnapshot {
            current_work_item_id: final_business_state.current_work_item_id.clone(),
            objective: Some("actual compound final memory".into()),
            work_summary: Some("complete original dispatch".into()),
            ..Default::default()
        };
        let mut refreshed = final_business_state.clone();
        refreshed.working_memory.current_working_memory = snapshot.clone();
        let frozen = FrozenEpisodeMemoryValues {
            timestamp: fixture.timestamp,
            initial_builder_id: "episode-embedded".into(),
            fallback_builder_id: "episode-embedded-fallback".into(),
            next_builder_id: "episode-embedded-next".into(),
            audit_event_id: "episode-embedded-audit".into(),
        };
        let plan = plan_episode_memory(
            EpisodeMemoryPlanInput {
                agent: &refreshed,
                actual_dispatch_turn_id: "turn-1",
                trigger: &fixture.message,
                prior_closure: &closure(false),
                current_closure: &closure(true),
                previous_snapshot: &snapshot,
                current_snapshot: &snapshot,
            },
            &frozen,
        );
        fixture.command_from_plan(plan)
    }

    #[test]
    fn embedded_memory_preserves_actual_final_business_state_and_final_control_revision() {
        let fixture = Fixture::new();
        let template = fixture.command(false);
        let receipt = fixture
            .db
            .transaction(|tx| {
                let validated = prepare_embedded_episode_memory_tx(
                    tx,
                    template.key(),
                    template.invocation(),
                    &fixture.state,
                )?;
                let final_business = embedded_business_state_tx(&fixture, tx);
                let command = embedded_command(&fixture, &final_business);
                let EpisodeMemoryCommit::Applied {
                    receipt,
                    transition,
                    ..
                } = write_embedded_episode_memory_tx(validated, &command, &final_business)?
                else {
                    panic!("a new embedded phase must commit")
                };
                let stored = state_tx(tx, &fixture.state.id)?.unwrap();
                let mut non_memory = stored.clone();
                non_memory.working_memory = fixture.state.working_memory.clone();
                assert_eq!(non_memory, final_business);
                assert_eq!(stored, *command.candidate());
                assert_eq!(
                    stored
                        .working_memory
                        .active_episode_builder
                        .as_ref()
                        .unwrap()
                        .latest_message_count,
                    final_business.total_message_count
                );
                assert_eq!(
                    transition.effects.agent_state.unwrap().record.as_ref(),
                    &stored
                );
                let revision: i64 = tx.query_row(
                    "SELECT control_revision FROM agent_states WHERE agent_id=?1",
                    [&fixture.state.id],
                    |row| row.get(0),
                )?;
                assert_eq!(receipt.agent_control_revision, revision as u64);
                Ok(receipt)
            })
            .unwrap();
        assert_eq!(fixture.revision() as u64, receipt.agent_control_revision);
        assert_eq!(fixture.count("episode_memory_results"), 1);
        assert_eq!(fixture.count("context_episode_anchors"), 1);
    }

    #[test]
    fn embedded_memory_rejects_stale_non_memory_plan_and_rolls_back_business_writes() {
        for stale in ["focus", "count", "posture", "turn"] {
            let fixture = Fixture::new();
            let template = fixture.command(false);
            let error = fixture
                .db
                .transaction(|tx| {
                    let validated = prepare_embedded_episode_memory_tx(
                        tx,
                        template.key(),
                        template.invocation(),
                        &fixture.state,
                    )?;
                    let final_business = embedded_business_state_tx(&fixture, tx);
                    let mut command = embedded_command(&fixture, &final_business);
                    match stale {
                        "focus" => command.plan.candidate.current_work_item_id = None,
                        "count" => command.plan.candidate.total_message_count = 0,
                        "posture" => command.plan.candidate.status = AgentStatus::AwakeIdle,
                        "turn" => command.plan.candidate.turn_index += 1,
                        _ => unreachable!(),
                    }
                    write_embedded_episode_memory_tx(validated, &command, &final_business)
                })
                .unwrap_err();
            assert!(error.to_string().contains("non-memory business field"));
            assert_eq!(fixture.durable_state(), fixture.state);
            for table in [
                "work_items",
                "context_episode_anchors",
                "audit_events",
                "runtime_index_outbox",
                "episode_memory_results",
            ] {
                assert_eq!(fixture.count(table), 0, "{stale}: {table}");
            }
        }
    }

    #[test]
    fn embedded_memory_rejects_changed_memory_baseline_and_inexact_final_readback() {
        for invalid in ["baseline", "readback"] {
            let fixture = Fixture::new();
            let template = fixture.command(false);
            let error = fixture
                .db
                .transaction(|tx| {
                    let validated = prepare_embedded_episode_memory_tx(
                        tx,
                        template.key(),
                        template.invocation(),
                        &fixture.state,
                    )?;
                    let mut final_business = embedded_business_state_tx(&fixture, tx);
                    if invalid == "baseline" {
                        final_business.working_memory.compression_epoch += 1;
                        upsert_agent_state_at_tx(tx, &final_business, fixture.timestamp)?;
                    } else {
                        final_business.total_message_count += 1;
                    }
                    let command = embedded_command(&fixture, &final_business);
                    write_embedded_episode_memory_tx(validated, &command, &final_business)
                })
                .unwrap_err();
            assert!(error.to_string().contains(if invalid == "baseline" {
                "changed the memory baseline"
            } else {
                "was not read back exactly"
            }));
            assert_eq!(fixture.durable_state(), fixture.state);
            assert_eq!(fixture.count("work_items"), 0);
            assert_eq!(fixture.count("episode_memory_results"), 0);
        }
    }

    #[test]
    fn embedded_memory_phase_collision_precedes_state_occ_and_never_acknowledges_outer_work() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        fixture.db.commit_episode_memory(&command).unwrap();
        let durable = fixture.durable_state();
        let error = fixture
            .db
            .transaction(|tx| {
                let _validated = prepare_embedded_episode_memory_tx(
                    tx,
                    command.key(),
                    command.invocation(),
                    &fixture.state,
                )?;
                panic!("an existing inner phase cannot skip the outer command receipt");
                #[allow(unreachable_code)]
                Ok(())
            })
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<EpisodeMemoryError>(),
            Some(&EpisodeMemoryError::EmbeddedPhaseCollision)
        );
        assert_eq!(fixture.durable_state(), durable);
        assert_eq!(fixture.count("episode_memory_results"), 1);
    }

    #[test]
    fn embedded_memory_rechecks_final_dispatch_ownership_and_identity() {
        for changed in ["source", "identity"] {
            let fixture = Fixture::new();
            let template = fixture.command(false);
            let error = fixture
                .db
                .transaction(|tx| {
                    let validated = prepare_embedded_episode_memory_tx(
                        tx,
                        template.key(),
                        template.invocation(),
                        &fixture.state,
                    )?;
                    let final_business = embedded_business_state_tx(&fixture, tx);
                    if changed == "source" {
                        let mut turn = turn_tx(tx, "turn-1")?.unwrap();
                        turn.input_message_ids.clear();
                        tx.execute(
                            "UPDATE turn_records SET payload_json=?1 WHERE turn_id='turn-1'",
                            [serde_json::to_string(&turn)?],
                        )?;
                    } else {
                        let payload: String = tx.query_row(
                            "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                            [&fixture.state.id],
                            |row| row.get(0),
                        )?;
                        let mut identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
                        identity.status = AgentRegistryStatus::Deleting;
                        super::super::evidence::upsert_agent_identity_tx(tx, &identity)?;
                    }
                    let command = embedded_command(&fixture, &final_business);
                    write_embedded_episode_memory_tx(validated, &command, &final_business)
                })
                .unwrap_err();
            assert!(error.to_string().contains(if changed == "source" {
                "does not own its invocation message"
            } else {
                "incarnation is no longer active"
            }));
            assert_eq!(fixture.durable_state(), fixture.state);
            assert_eq!(fixture.count("work_items"), 0);
            assert_eq!(fixture.count("episode_memory_results"), 0);
        }
    }

    #[test]
    fn embedded_memory_fault_rolls_back_outer_work_state_and_every_memory_write() {
        for point in [
            EpisodeMemoryFaultPoint::AfterValidation,
            EpisodeMemoryFaultPoint::AfterEpisodeWrite,
            EpisodeMemoryFaultPoint::AfterStateWrite,
            EpisodeMemoryFaultPoint::AfterAuditWrites,
            EpisodeMemoryFaultPoint::AfterIndexWrites,
            EpisodeMemoryFaultPoint::AfterReceiptWrite,
        ] {
            let fixture = Fixture::new();
            let template = fixture.command(false);
            let before_revision = fixture.revision();
            assert!(fixture
                .db
                .transaction(|tx| {
                    let validated = prepare_embedded_episode_memory_tx(
                        tx,
                        template.key(),
                        template.invocation(),
                        &fixture.state,
                    )?;
                    let final_business = embedded_business_state_tx(&fixture, tx);
                    let command = embedded_command(&fixture, &final_business).with_fault(point);
                    write_embedded_episode_memory_tx(validated, &command, &final_business)
                })
                .is_err());
            assert_eq!(fixture.durable_state(), fixture.state);
            assert_eq!(fixture.revision(), before_revision);
            for table in [
                "work_items",
                "context_episode_anchors",
                "audit_events",
                "runtime_index_outbox",
                "episode_memory_results",
            ] {
                assert_eq!(fixture.count(table), 0, "{point:?}: {table}");
            }
        }
    }

    #[test]
    fn embedded_memory_noop_receipt_uses_final_business_revision_without_rewriting_state() {
        let fixture = Fixture::new();
        let template = fixture.command(false);
        fixture
            .db
            .transaction(|tx| {
                let validated = prepare_embedded_episode_memory_tx(
                    tx,
                    template.key(),
                    template.invocation(),
                    &fixture.state,
                )?;
                let final_business = embedded_business_state_tx(&fixture, tx);
                let mut plan = template.plan.clone();
                plan.candidate = final_business.clone();
                let command = fixture.command_from_plan(plan);
                let before: i64 = tx.query_row(
                    "SELECT control_revision FROM agent_states WHERE agent_id=?1",
                    [&fixture.state.id],
                    |row| row.get(0),
                )?;
                let EpisodeMemoryCommit::Applied { receipt, .. } =
                    write_embedded_episode_memory_tx(validated, &command, &final_business)?
                else {
                    panic!("a new no-op phase still commits a receipt")
                };
                assert_eq!(receipt.agent_control_revision, before as u64);
                let after: i64 = tx.query_row(
                    "SELECT control_revision FROM agent_states WHERE agent_id=?1",
                    [&fixture.state.id],
                    |row| row.get(0),
                )?;
                assert_eq!(before, after);
                Ok(())
            })
            .unwrap();
        assert_eq!(fixture.count("episode_memory_results"), 1);
    }

    #[test]
    fn every_write_stage_rolls_back_the_complete_episode_write_set() {
        for fault in [
            EpisodeMemoryFaultPoint::AfterValidation,
            EpisodeMemoryFaultPoint::AfterEpisodeWrite,
            EpisodeMemoryFaultPoint::AfterStateWrite,
            EpisodeMemoryFaultPoint::AfterAuditWrites,
            EpisodeMemoryFaultPoint::AfterIndexWrites,
            EpisodeMemoryFaultPoint::AfterReceiptWrite,
        ] {
            let fixture = Fixture::new();
            let revision = fixture.revision();
            let sequences = fixture.sequences();
            let command = fixture.command(true).with_fault(fault);
            assert!(
                fixture.db.commit_episode_memory(&command).is_err(),
                "{fault:?}"
            );
            assert_eq!(fixture.durable_state(), fixture.state, "{fault:?}");
            assert_eq!(fixture.revision(), revision, "{fault:?}");
            for table in [
                "context_episode_anchors",
                "audit_events",
                "runtime_index_outbox",
                "runtime_index_outbox_watermarks",
                "episode_memory_results",
            ] {
                assert_eq!(fixture.count(table), 0, "{fault:?}: {table}");
            }
            assert_eq!(fixture.sequences(), sequences, "{fault:?}");
        }
    }

    #[test]
    fn business_noop_commits_an_applied_receipt_without_advancing_state_revision() {
        let fixture = Fixture::new();
        let revision = fixture.revision();
        let command = fixture.command(false);
        let EpisodeMemoryCommit::Applied {
            receipt,
            transition,
            episode,
        } = fixture.db.commit_episode_memory(&command).unwrap()
        else {
            panic!("first no-op must be Applied");
        };
        assert!(transition.applied);
        assert!(!receipt.outcome.business_fields_changed);
        assert!(!receipt.outcome.episode_memory_changed);
        assert!(episode.is_none());
        assert_eq!(fixture.revision(), revision);
        assert_eq!(fixture.count("episode_memory_results"), 1);
        assert!(matches!(
            fixture.db.commit_episode_memory(&command).unwrap(),
            EpisodeMemoryCommit::Replayed { .. }
        ));
        assert_eq!(fixture.revision(), revision);
    }

    #[test]
    fn finalized_episode_audit_index_watermark_and_state_share_the_receipt() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        let EpisodeMemoryCommit::Applied {
            receipt,
            transition,
            episode,
        } = fixture.db.commit_episode_memory(&command).unwrap()
        else {
            panic!("first finalize must be Applied");
        };
        assert_eq!(fixture.durable_state(), *command.candidate());
        assert_eq!(
            receipt.outcome.finalized_episode_id.as_deref(),
            Some("episode-1")
        );
        assert_eq!(episode.unwrap().source_turn_ids, ["turn-1"]);
        assert_eq!(receipt.audit_bindings.len(), 1);
        assert_eq!(receipt.index_bindings.len(), 1);
        assert!(receipt.audit_bindings[0].event_seq > 0);
        assert!(!receipt.audit_bindings[0].event_log_epoch.is_empty());
        assert!(transition.effects.notify_memory_index);
        assert_eq!(fixture.count("context_episode_anchors"), 1);
        assert_eq!(fixture.count("audit_events"), 1);
        assert_eq!(fixture.count("runtime_index_outbox"), 1);
        assert_eq!(fixture.count("episode_memory_results"), 1);
        let connection = fixture.db.connection().unwrap();
        let (produced, updated_at): (i64, String) = connection.query_row(
            "SELECT produced_change_seq, updated_at FROM runtime_index_outbox_watermarks WHERE agent_id = 'agent-a'", [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(produced, receipt.index_bindings[0].change_seq);
        assert_eq!(
            DateTime::parse_from_rfc3339(&updated_at)
                .unwrap()
                .with_timezone(&Utc),
            fixture.timestamp
        );
    }

    #[test]
    fn replay_after_later_state_advance_returns_original_outcome_without_reinstalling_old_state() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        let EpisodeMemoryCommit::Applied { receipt, .. } =
            fixture.db.commit_episode_memory(&command).unwrap()
        else {
            panic!("expected Applied");
        };
        let mut later = command.candidate().clone();
        later.turn_index = 2;
        later.current_turn_id = Some("turn-2".into());
        later.working_memory.compression_epoch += 1;
        fixture
            .db
            .agent_states()
            .upsert_for_fixture(&later)
            .unwrap();
        let later_revision = fixture.revision();
        // Consuming the index outbox does not remove historical outcome proof.
        fixture
            .db
            .connection()
            .unwrap()
            .execute("DELETE FROM runtime_index_outbox", [])
            .unwrap();
        let EpisodeMemoryCommit::Replayed { receipt: replay } =
            fixture.db.commit_episode_memory(&command).unwrap()
        else {
            panic!("expected historical replay");
        };
        assert_eq!(replay, receipt);
        assert_eq!(fixture.durable_state(), later);
        assert_eq!(fixture.revision(), later_revision);
        assert_eq!(fixture.count("runtime_index_outbox"), 0);
        assert_eq!(fixture.count("audit_events"), 1);
    }

    #[test]
    fn committed_lost_ack_is_resolved_by_original_digest_and_invocation() {
        let fixture = Fixture::new();
        let command = fixture
            .command(true)
            .with_fault(EpisodeMemoryFaultPoint::AfterCommitBeforeAck);
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert_eq!(
            super::super::persistence_certainty(&error),
            Some(super::super::PersistenceCertainty::Indeterminate)
        );
        let receipt = fixture
            .db
            .episode_memory_receipt(
                command.key(),
                Some(command.command_digest()),
                command.invocation(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(receipt.outcome.compression_epoch_after, 1);
        assert_eq!(fixture.durable_state(), *command.candidate());
        assert!(matches!(
            fixture.db.commit_episode_memory(&command).unwrap(),
            EpisodeMemoryCommit::Replayed { .. }
        ));
        assert_eq!(fixture.count("context_episode_anchors"), 1);
        assert_eq!(fixture.count("audit_events"), 1);
        assert_eq!(fixture.count("runtime_index_outbox"), 1);
        let wrong = EpisodeMemoryInvocation {
            message_id: "message-other".into(),
            ..command.invocation().clone()
        };
        let error = fixture
            .db
            .episode_memory_receipt(command.key(), None, &wrong)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<EpisodeMemoryError>(),
            Some(&EpisodeMemoryError::InvocationConflict)
        );
    }

    #[test]
    fn same_dispatch_different_command_conflicts_before_current_state_occ() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        fixture.db.commit_episode_memory(&command).unwrap();
        let different = fixture.command(false);
        let error = fixture.db.commit_episode_memory(&different).unwrap_err();
        assert_eq!(
            error.downcast_ref::<EpisodeMemoryError>(),
            Some(&EpisodeMemoryError::CommandConflict)
        );
        assert_eq!(fixture.count("episode_memory_results"), 1);
    }

    #[test]
    fn missing_durable_identity_and_wrong_incarnation_do_not_create_receipts() {
        let fixture = Fixture::new();
        let mut wrong = fixture.command(false);
        wrong.key.identity_incarnation = 2;
        assert!(fixture.db.commit_episode_memory(&wrong).is_err());
        fixture
            .db
            .connection()
            .unwrap()
            .execute("DELETE FROM agent_identities", [])
            .unwrap();
        assert!(fixture
            .db
            .commit_episode_memory(&fixture.command(false))
            .is_err());
        assert_eq!(fixture.count("episode_memory_results"), 0);
        assert_eq!(fixture.durable_state(), fixture.state);
    }

    #[test]
    fn source_refs_must_exist_in_the_same_agent_and_declared_range() {
        for invalid in ["missing", "foreign", "range", "refs"] {
            let fixture = Fixture::new();
            let mut plan = fixture.command(true).plan;
            let episode = plan.finalized_episode.as_mut().unwrap();
            match invalid {
                "missing" => {
                    episode.source_turn_ids = vec!["turn-missing".into()];
                    episode.source_refs = vec!["turn:turn-missing".into()];
                }
                "foreign" => {
                    fixture
                        .db
                        .turn_records()
                        .upsert(&TurnRecord::new("agent-b", "turn-foreign", 1))
                        .unwrap();
                    episode.source_turn_ids = vec!["turn-foreign".into()];
                    episode.source_refs = vec!["turn:turn-foreign".into()];
                }
                "range" => {
                    episode.start_turn_index = 2;
                    episode.end_turn_index = 2;
                }
                "refs" => {
                    episode.source_refs = vec!["episode:episode-1".into()];
                }
                _ => unreachable!(),
            }
            assert!(
                fixture
                    .db
                    .commit_episode_memory(&fixture.command_from_plan(plan))
                    .is_err(),
                "{invalid}"
            );
            assert_eq!(fixture.durable_state(), fixture.state);
            assert_eq!(fixture.count("context_episode_anchors"), 0);
            assert_eq!(fixture.count("episode_memory_results"), 0);
        }
    }

    #[test]
    fn legacy_episode_without_receipt_is_not_adopted_or_overwritten() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        let episode = command.plan.finalized_episode.as_ref().unwrap();
        fixture.db.context_episodes().upsert(episode).unwrap();
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<EpisodeMemoryError>(),
            Some(EpisodeMemoryError::LegacyEpisodeCollision { .. })
        ));
        assert_eq!(fixture.durable_state(), fixture.state);
        assert_eq!(fixture.count("episode_memory_results"), 0);
        assert_eq!(fixture.count("audit_events"), 0);
    }

    #[test]
    fn immutable_episode_id_collision_preserves_existing_content() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        let mut prior = command.plan.finalized_episode.clone().unwrap();
        prior.objective = Some("different historical content".into());
        fixture.db.context_episodes().upsert(&prior).unwrap();
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<EpisodeMemoryError>(),
            Some(EpisodeMemoryError::ImmutableEpisodeConflict { .. })
        ));
        assert_eq!(
            fixture
                .db
                .context_episodes()
                .recent_for_agent(&fixture.state.id, 10)
                .unwrap(),
            vec![prior]
        );
        assert_eq!(fixture.durable_state(), fixture.state);
        assert_eq!(fixture.count("episode_memory_results"), 0);
    }

    #[test]
    fn active_builder_already_archived_without_receipt_fences_even_a_nonfinalizing_dispatch() {
        let mut fixture = Fixture::new();
        let finalize = fixture.command(true);
        let episode = finalize.plan.finalized_episode.as_ref().unwrap();
        fixture.db.context_episodes().upsert(episode).unwrap();
        let mut builder = finalize
            .plan
            .candidate
            .working_memory
            .active_episode_builder
            .clone()
            .unwrap();
        builder.id = episode.id.clone();
        builder.start_turn_index = 1;
        builder.latest_turn_index = 1;
        fixture.state.working_memory.active_episode_id = Some(builder.id.clone());
        fixture.state.working_memory.active_episode_builder = Some(builder);
        fixture
            .db
            .agent_states()
            .upsert_for_fixture(&fixture.state)
            .unwrap();
        let command = fixture.command(false);
        assert!(command.plan.finalized_episode.is_none());
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<EpisodeMemoryError>(),
            Some(EpisodeMemoryError::LegacyEpisodeCollision { .. })
        ));
        assert_eq!(fixture.durable_state(), fixture.state);
        assert_eq!(fixture.count("episode_memory_results"), 0);
    }

    #[test]
    fn candidate_state_without_receipt_is_not_a_success_witness() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        fixture
            .db
            .agent_states()
            .upsert_for_fixture(command.candidate())
            .unwrap();
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert!(error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert_eq!(fixture.count("episode_memory_results"), 0);
        assert_eq!(fixture.count("context_episode_anchors"), 0);
    }

    #[test]
    fn receipt_episode_projection_must_match_the_original_payload() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        fixture.db.commit_episode_memory(&command).unwrap();
        fixture
            .db
            .connection()
            .unwrap()
            .execute(
                "UPDATE episode_memory_results SET finalized_episode_id = 'episode-other'",
                [],
            )
            .unwrap();
        let error = fixture
            .db
            .episode_memory_receipt(
                command.key(),
                Some(command.command_digest()),
                command.invocation(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("columns and payload disagree"));
        assert!(fixture.db.commit_episode_memory(&command).is_err());
        assert_eq!(fixture.count("context_episode_anchors"), 1);
    }

    #[test]
    fn first_attempt_for_an_old_dispatch_does_not_adopt_a_later_state() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        let mut later = fixture.state.clone();
        later.turn_index = 2;
        later.current_turn_id = Some("turn-2".into());
        fixture
            .db
            .agent_states()
            .upsert_for_fixture(&later)
            .unwrap();
        let revision = fixture.revision();
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert!(error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some());
        assert_eq!(fixture.durable_state(), later);
        assert_eq!(fixture.revision(), revision);
        assert_eq!(fixture.count("episode_memory_results"), 0);
        assert_eq!(fixture.count("context_episode_anchors"), 0);
    }

    #[test]
    fn rebasing_an_old_dispatch_expected_state_does_not_ack_an_unwritten_candidate() {
        let fixture = Fixture::new();
        let original = fixture.command(true);
        let mut later = fixture.state.clone();
        later.turn_index = 2;
        later.current_turn_id = Some("turn-2".into());
        fixture
            .db
            .agent_states()
            .upsert_for_fixture(&later)
            .unwrap();
        let revision = fixture.revision();
        let command = EpisodeMemoryTransitionCommand::new(
            original.key.clone(),
            original.invocation.clone(),
            later.clone(),
            original.plan,
            fixture.timestamp,
        )
        .unwrap();
        assert_eq!(command.candidate().turn_index, 1);
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert!(error.to_string().contains("dispatch turn index mismatch"));
        assert_eq!(fixture.durable_state(), later);
        assert_eq!(fixture.revision(), revision);
        for table in [
            "context_episode_anchors",
            "audit_events",
            "runtime_index_outbox",
            "episode_memory_results",
        ] {
            assert_eq!(fixture.count(table), 0, "{table}");
        }
    }

    #[test]
    fn a_silently_ignored_state_write_rolls_back_instead_of_returning_applied() {
        let fixture = Fixture::new();
        fixture
            .db
            .connection()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER test_ignore_episode_state BEFORE UPDATE ON agent_states
             BEGIN SELECT RAISE(IGNORE); END;",
            )
            .unwrap();
        let command = fixture.command(true);
        let revision = fixture.revision();
        let error = fixture.db.commit_episode_memory(&command).unwrap_err();
        assert!(error
            .to_string()
            .contains("candidate was not persisted exactly"));
        assert_eq!(fixture.durable_state(), fixture.state);
        assert_eq!(fixture.revision(), revision);
        for table in [
            "context_episode_anchors",
            "audit_events",
            "runtime_index_outbox",
            "episode_memory_results",
        ] {
            assert_eq!(fixture.count(table), 0, "{table}");
        }
    }

    #[test]
    fn dispatch_binding_remains_valid_after_current_turn_id_is_cleared() {
        let mut fixture = Fixture::new();
        fixture.state.current_turn_id = None;
        fixture
            .db
            .agent_states()
            .upsert_for_fixture(&fixture.state)
            .unwrap();
        let command = fixture.command(true);
        let episode = command.plan.finalized_episode.as_ref().unwrap();
        assert_eq!(episode.source_turn_ids, ["turn-1"]);
        assert_eq!(episode.source_refs, ["turn:turn-1"]);
        assert!(command.candidate().current_turn_id.is_none());
        assert!(matches!(
            fixture.db.commit_episode_memory(&command).unwrap(),
            EpisodeMemoryCommit::Applied { .. }
        ));
        assert_eq!(fixture.durable_state(), *command.candidate());
    }

    #[test]
    fn receipt_commit_time_projection_must_match_the_original_payload() {
        let fixture = Fixture::new();
        let command = fixture.command(true);
        fixture.db.commit_episode_memory(&command).unwrap();
        fixture
            .db
            .connection()
            .unwrap()
            .execute(
                "UPDATE episode_memory_results SET committed_at = '2026-10-13T00:00:00+00:00'",
                [],
            )
            .unwrap();
        let error = fixture
            .db
            .episode_memory_receipt(
                command.key(),
                Some(command.command_digest()),
                command.invocation(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("columns and payload disagree"));
        assert!(fixture.db.commit_episode_memory(&command).is_err());
        assert_eq!(fixture.count("context_episode_anchors"), 1);
    }
}
