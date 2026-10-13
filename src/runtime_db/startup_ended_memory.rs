//! Exclusive startup may finish only an original, canonically ended dispatch.
//! The borrowed session proves old Rust producers are gone, never OS cleanup.
use crate::{
    domain::execution_protocol::{
        CommandResult, ConversationOutcome, ExecutionAttempt, ExecutionAttemptState,
        ExecutionBinding, ExecutionOutcome, ExecutionOutcomeRecord, WorkItemOutcome,
    },
    runtime::resident_input_evidence::InputEvidenceScope,
    runtime_db::{
        control_transition::{
            prove_committed_control_stop_for_run_tx, stopped_run_proof_material_tx, ControlKey,
            ControlStoppedRun,
        },
        dispatch_memory::{load_dispatch_memory_binding_by_turn_tx, DispatchMemoryBinding},
        episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey},
        input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery,
        input_mutation_receipts::canonical_command_digest,
        RuntimeDb,
    },
    types::{AuditEvent, MessageEnvelope},
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};
const MAX_PROOF_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROOF_RECORDS: usize = 65_536;

#[derive(Debug)]
struct SessionActivity {
    database: PathBuf,
    active: AtomicBool,
}

/// The Host keeps this borrow across global recovery, canonical posture, and
/// the Finish ACK. An owned command cannot extend the startup capability.
pub(crate) struct StartupEndedMemorySession<'cap, 'guard> {
    _capability: &'cap ExclusiveStartupInputRecovery<'guard>,
    activity: Arc<SessionActivity>,
}
impl<'cap, 'guard> StartupEndedMemorySession<'cap, 'guard> {
    pub(crate) fn new(
        db: &RuntimeDb,
        capability: &'cap ExclusiveStartupInputRecovery<'guard>,
    ) -> Result<Self> {
        capability.validate_database(db)?;
        Ok(Self {
            _capability: capability,
            activity: Arc::new(SessionActivity {
                database: std::fs::canonicalize(db.path())?,
                active: AtomicBool::new(true),
            }),
        })
    }
    /// The Host charges these SQL-only sizes before constructing owned proof.
    pub(crate) fn interrupted_proof_material_tx(
        &self,
        tx: &Transaction<'_>,
        binding: &DispatchMemoryBinding,
    ) -> Result<(usize, usize)> {
        self.activity.validate_tx(tx)?;
        startup_interruption_proof_material_tx(tx, binding)
    }
    pub(crate) fn stopped_proof_material_tx(
        &self,
        tx: &Transaction<'_>,
        binding: &DispatchMemoryBinding,
        original: &ControlStoppedRun<'_>,
    ) -> Result<(usize, usize)> {
        self.activity.validate_tx(tx)?;
        stopped_binding_material_tx(tx, binding, original.run_id, original.activation_id)
    }
    pub(crate) fn authorize_interrupted_tx(
        &self,
        tx: &Transaction<'_>,
        binding: &DispatchMemoryBinding,
    ) -> Result<StartupEndedDispatchAuthorization> {
        self.interrupted_proof_material_tx(tx, binding)?;
        let common = read_original_address_tx(tx, binding)?;
        let interrupted = read_startup_interruption_tx(tx, binding, &common)?;
        Ok(self.authorization(
            binding,
            common,
            CanonicalEnd::StartupInterrupted(interrupted),
        ))
    }
    pub(crate) fn authorize_stopped_tx(
        &self,
        tx: &Transaction<'_>,
        binding: &DispatchMemoryBinding,
        key: &ControlKey,
        original: &ControlStoppedRun<'_>,
    ) -> Result<StartupEndedDispatchAuthorization> {
        self.stopped_proof_material_tx(tx, binding, original)?;
        let common = read_original_address_tx(tx, binding)?;
        ensure!(
            common.run_id == original.run_id,
            "startup Stop run differs from original dispatch"
        );
        let proof = prove_committed_control_stop_for_run_tx(tx, key, original)?
            .context("startup dispatch has no original canonical Stop")?;
        proof.require_same_transaction(tx)?;
        proof.matches_memory_dispatch(binding.key(), binding.invocation())?;
        let stopped = StoppedAddress {
            key: key.clone(),
            command_digest: proof.receipt().command_digest.clone(),
            owner_epoch: original.owner_epoch,
            runtime_lease_id: original.runtime_lease_id.into(),
            activation_id: original.activation_id.map(str::to_owned),
            source_digest: canonical_command_digest(original.source)?,
        };
        Ok(self.authorization(binding, common, CanonicalEnd::ControlStopped(stopped)))
    }
    fn authorization(
        &self,
        binding: &DispatchMemoryBinding,
        common: OriginalAddress,
        end: CanonicalEnd,
    ) -> StartupEndedDispatchAuthorization {
        StartupEndedDispatchAuthorization {
            binding: binding.clone(),
            original: common,
            end,
            activity: Arc::downgrade(&self.activity),
        }
    }
}
impl Drop for StartupEndedMemorySession<'_, '_> {
    fn drop(&mut self) {
        self.activity.active.store(false, Ordering::Release);
    }
}
impl SessionActivity {
    fn validate_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(
            self.active.load(Ordering::Acquire),
            "startup ended-memory session has ended"
        );
        let file: String = tx.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            !file.is_empty() && std::fs::canonicalize(file)? == self.database,
            "startup ended-memory transaction belongs to another database"
        );
        Ok(())
    }
}

/// Serialize only the durable address into the enclosing command digest. This
/// has no Deserialize/Clone constructor, and the Weak marker cannot keep a
/// session, maintenance lease, handoff, owner, or resident root alive.
#[derive(Debug, Serialize)]
pub(crate) struct StartupEndedDispatchAuthorization {
    binding: DispatchMemoryBinding,
    original: OriginalAddress,
    end: CanonicalEnd,
    #[serde(skip)]
    activity: Weak<SessionActivity>,
}
impl StartupEndedDispatchAuthorization {
    /// SQL-only material, before decoding any original proof. The caller adds
    /// this to the same M2 combined allowance and shared ledger observation.
    pub(crate) fn proof_material_tx(&self, tx: &Transaction<'_>) -> Result<(usize, usize)> {
        let activity = self
            .activity
            .upgrade()
            .context("startup ended-memory session is no longer alive")?;
        activity.validate_tx(tx)?;
        match &self.end {
            CanonicalEnd::StartupInterrupted(_) => {
                startup_interruption_proof_material_tx(tx, &self.binding)
            }
            CanonicalEnd::ControlStopped(address) => stopped_binding_material_tx(
                tx,
                &self.binding,
                &self.original.run_id,
                address.activation_id.as_deref(),
            ),
        }
    }
    /// Call after the original whole receipt lookup. The ordinary Finish budget
    /// must precede this check: its source/Turn allowance also covers the finite
    /// original Stop validator's sequential source decode.
    pub(crate) fn validate_tx(
        &self,
        tx: &Transaction<'_>,
        key: &EpisodeMemoryKey,
        invocation: &EpisodeMemoryInvocation,
    ) -> Result<()> {
        let activity = self
            .activity
            .upgrade()
            .context("startup ended-memory session is no longer alive")?;
        activity.validate_tx(tx)?;
        ensure!(
            self.binding.key() == key && self.binding.invocation() == invocation,
            "startup authorization differs from original memory address"
        );
        ensure!(
            read_original_address_tx(tx, &self.binding)? == self.original,
            "startup original dispatch address changed"
        );
        // Posture belongs to a separate acknowledged phase. Never apply an old
        // end to a new run or an advanced dispatch, even within the same inc.
        let (index, run, payload_id, payload_index, payload_run): (u64, Option<String>, String, u64, Option<String>) = tx.query_row(
            "SELECT turn_index,current_run_id,json_extract(payload_json,'$.id'),json_extract(payload_json,'$.turn_index'),json_extract(payload_json,'$.current_run_id') FROM agent_states WHERE agent_id=?1",
            [&key.agent_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        ensure!(
            index == invocation.turn_index
                && payload_index == index
                && payload_id == key.agent_id
                && run.is_none()
                && payload_run.is_none(),
            "startup memory Finish requires original canonical posture with no new run"
        );
        match &self.end {
            CanonicalEnd::StartupInterrupted(expected) => ensure!(
                read_startup_interruption_tx(tx, &self.binding, &self.original)? == *expected,
                "startup interruption proof changed"
            ),
            CanonicalEnd::ControlStopped(expected) => {
                // No latest run/lease inference: reconstruct only the frozen
                // original address, and revalidate its immutable Stop receipt.
                let bytes: usize = tx.query_row(
                    "SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE evidence_id=?1",
                    [&invocation.message_id],
                    |r| r.get(0),
                )?;
                ensure!(
                    bytes <= MAX_PROOF_BYTES,
                    "startup Stop source exceeds proof budget"
                );
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM messages WHERE evidence_id=?1",
                    [&invocation.message_id],
                    |r| r.get(0),
                )?;
                let source: MessageEnvelope = serde_json::from_str(&payload)?;
                ensure!(
                    canonical_command_digest(&source)? == expected.source_digest,
                    "startup Stop source differs from frozen original"
                );
                let original = ControlStoppedRun {
                    agent_id: &key.agent_id,
                    identity_incarnation: key.identity_incarnation,
                    owner_epoch: expected.owner_epoch,
                    run_id: &self.original.run_id,
                    runtime_lease_id: &expected.runtime_lease_id,
                    activation_id: expected.activation_id.as_deref(),
                    source: &source,
                };
                let proof = prove_committed_control_stop_for_run_tx(tx, &expected.key, &original)?
                    .context("startup original Stop proof disappeared")?;
                proof.matches_memory_dispatch(key, invocation)?;
                ensure!(
                    proof.receipt().command_digest == expected.command_digest,
                    "startup Stop receipt changed"
                );
            }
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, PartialEq, Eq)]
struct OriginalAddress {
    run_id: String,
    source_sequence: u64,
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CanonicalEnd {
    StartupInterrupted(InterruptedAddress),
    ControlStopped(StoppedAddress),
}
#[derive(Debug, Serialize)]
struct StoppedAddress {
    key: ControlKey,
    command_digest: String,
    owner_epoch: u64,
    runtime_lease_id: String,
    activation_id: Option<String>,
    source_digest: String,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
struct InterruptedAddress {
    audit_digest: String,
    reason: String,
    interrupted_at: DateTime<Utc>,
    queue_digest: Option<String>,
    attempts: Vec<InterruptedAttemptAddress>,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
struct InterruptedAttemptAddress {
    startup_audit_digest: String,
    attempt_id: String,
    outcome_id: String,
    attempt_digest: String,
    outcome_digest: String,
}

/// No full source/Turn/audit/Attempt payload is returned by this preflight.
fn startup_interruption_proof_material_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
) -> Result<(usize, usize)> {
    let key = binding.key();
    let invocation = binding.invocation();
    let attempts = "SELECT attempt_id,terminal_outcome_id FROM execution_protocol_attempts WHERE agent_id=?1 AND json_extract(payload_json,'$.source_message_id')=?2 AND (lifecycle_state='open' OR json_extract(payload_json,'$.turn_id')=?3 OR json_extract(payload_json,'$.terminal_at')=(SELECT replace(json_extract(payload_json,'$.terminal.completed_at'),'Z','+00:00') FROM turn_records WHERE turn_id=?3))";
    let mut material = (0usize, 0usize);
    for selection in [
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(evidence_id AS BLOB)) size FROM messages WHERE evidence_id=?2",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(message_id AS BLOB)) size FROM queue_entries WHERE message_id=?2",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(turn_id AS BLOB)) size FROM turn_records WHERE turn_id=?3",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) size FROM agent_identities WHERE agent_id=?1",
        "SELECT length(CAST(data_json AS BLOB))+length(CAST(audit_event_id AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(kind AS BLOB))+length(CAST(created_at AS BLOB))+16 size FROM audit_events WHERE audit_event_id='audit:startup-interrupted-turn:'||?3 OR audit_event_id IN (SELECT 'audit:startup-interrupted-attempt:'||?1||':'||attempt_id FROM attempts)",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(attempt_id AS BLOB))+length(CAST(source_identity_json AS BLOB))+COALESCE(length(CAST(terminal_outcome_id AS BLOB)),0)+length(CAST(lifecycle_state AS BLOB))+16 size FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id IN (SELECT attempt_id FROM attempts)",
        "SELECT length(CAST(payload_json AS BLOB))+length(CAST(outcome_id AS BLOB))+length(CAST(attempt_id AS BLOB)) size FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id IN (SELECT terminal_outcome_id FROM attempts)",
    ] {
        let (count, bytes): (usize,usize) = tx.query_row(
            &format!("WITH attempts AS ({attempts}) SELECT COUNT(*),COALESCE(SUM(size),0) FROM ({selection})"),
            params![key.agent_id, invocation.message_id, key.actual_dispatch_turn_id],
            |r|Ok((r.get(0)?,r.get(1)?)))?;
        add_proof_material(&mut material, bytes, count)?;
    }
    Ok(material)
}
fn stopped_binding_material_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
    run_id: &str,
    activation_id: Option<&str>,
) -> Result<(usize, usize)> {
    let scope = InputEvidenceScope {
        agent_id: binding.key().agent_id.clone(),
        identity_incarnation: binding.key().identity_incarnation,
    };
    stopped_run_proof_material_tx(tx, &scope, run_id, activation_id)
}

fn add_proof_material(material: &mut (usize, usize), bytes: usize, records: usize) -> Result<()> {
    material.0 = material
        .0
        .checked_add(bytes)
        .context("startup proof byte overflow")?;
    material.1 = material
        .1
        .checked_add(records)
        .context("startup proof count overflow")?;
    ensure!(
        material.0 <= MAX_PROOF_BYTES && material.1 <= MAX_PROOF_RECORDS,
        "startup ended-memory proof exceeds preparation limit"
    );
    Ok(())
}

fn read_original_address_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
) -> Result<OriginalAddress> {
    let key = binding.key();
    let invocation = binding.invocation();
    let scope = InputEvidenceScope {
        agent_id: key.agent_id.clone(),
        identity_incarnation: key.identity_incarnation,
    };
    ensure!(
        load_dispatch_memory_binding_by_turn_tx(tx, &scope, &key.actual_dispatch_turn_id)?.as_ref()
            == Some(binding),
        "startup memory requires original unfinished Start receipt"
    );
    let (run, normalized_run, agent, id, index, source_owner, seq, source_id, source_agent, source_seq): (String,String,String,String,u64,String,u64,String,String,u64) = tx.query_row(
        "SELECT json_extract(t.payload_json,'$.run_id'),t.run_id,json_extract(t.payload_json,'$.agent_id'),json_extract(t.payload_json,'$.turn_id'),json_extract(t.payload_json,'$.turn_index'),m.agent_id,m.message_seq,json_extract(m.payload_json,'$.id'),json_extract(m.payload_json,'$.agent_id'),json_extract(m.payload_json,'$.message_seq') FROM turn_records t,messages m WHERE t.turn_id=?1 AND m.evidence_id=?2",
        params![key.actual_dispatch_turn_id, invocation.message_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
    ensure!(
        !run.is_empty()
            && run == normalized_run
            && agent == key.agent_id
            && id == key.actual_dispatch_turn_id
            && index == invocation.turn_index
            && source_owner == agent
            && source_agent == agent
            && source_id == invocation.message_id
            && source_seq == seq
            && seq > 0,
        "startup original source/Turn columns disagree"
    );
    let assigned: Option<(String, String)> = tx
        .query_row(
            "SELECT agent_id,turn_id FROM conversation_input_assignments WHERE message_id=?1",
            [&invocation.message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    ensure!(
        assigned == Some((key.agent_id.clone(), key.actual_dispatch_turn_id.clone())),
        "startup memory needs actual original source assignment"
    );
    Ok(OriginalAddress {
        run_id: run,
        source_sequence: seq,
    })
}
fn read_startup_interruption_tx(
    tx: &Transaction<'_>,
    binding: &DispatchMemoryBinding,
    original: &OriginalAddress,
) -> Result<InterruptedAddress> {
    let key = binding.key();
    let invocation = binding.invocation();
    let (kind, reason, at, normalized_kind, normalized_at): (String,String,String,String,String) = tx.query_row(
        "SELECT json_extract(payload_json,'$.terminal.kind'),json_extract(payload_json,'$.terminal.reason'),json_extract(payload_json,'$.terminal.completed_at'),terminal_kind,completed_at FROM turn_records WHERE turn_id=?1",
        [&key.actual_dispatch_turn_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let at: DateTime<Utc> = at.parse()?;
    ensure!(
        kind == "interrupted"
            && normalized_kind == kind
            && normalized_at == crate::runtime_db::repositories::timestamp(at)
            && matches!(
                reason.as_str(),
                "startup_recovery/superseded_by_replay"
                    | "startup_recovery/external_side_effect_indeterminate"
                    | "startup_recovery/orphaned_queue_claim"
                    | "startup_recovery/destructive_operation_verify_only"
                    | "startup_recovery/daemon_restart"
            ),
        "original Turn was not interrupted by canonical startup recovery"
    );
    let audit_id = format!(
        "audit:startup-interrupted-turn:{}",
        key.actual_dispatch_turn_id
    );
    let (audit_agent, audit_kind, audit_at, audit_seq, size): (String,String,String,u64,usize) = tx.query_row("SELECT agent_id,kind,created_at,event_seq,length(CAST(data_json AS BLOB)) FROM audit_events WHERE audit_event_id=?1", [&audit_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    ensure!(
        size <= 64 * 1024,
        "startup interruption audit exceeds proof budget"
    );
    let payload: String = tx.query_row(
        "SELECT data_json FROM audit_events WHERE audit_event_id=?1",
        [&audit_id],
        |r| r.get(0),
    )?;
    let audit: AuditEvent = serde_json::from_str(&payload)?;
    ensure!(
        audit_agent == key.agent_id
            && audit_kind == "startup_interrupted_turn_recovered"
            && audit.id == audit_id
            && audit.kind == audit_kind
            && audit.event_seq == audit_seq
            && audit.created_at == at
            && audit_at == crate::runtime_db::repositories::timestamp(at)
            && audit.data.get("agent_id").and_then(|x| x.as_str()) == Some(key.agent_id.as_str())
            && audit.data.get("turn_id").and_then(|x| x.as_str())
                == Some(key.actual_dispatch_turn_id.as_str())
            && audit.data.get("turn_index").and_then(|x| x.as_u64()) == Some(invocation.turn_index)
            && audit.data.get("reason").and_then(|x| x.as_str()) == Some(reason.as_str()),
        "original startup interruption audit disagrees"
    );
    let queue: Option<(String, String, String, usize)> = tx
        .query_row(
            "SELECT agent_id,status,updated_at,length(CAST(payload_json AS BLOB)) FROM queue_entries WHERE message_id=?1",
            [&invocation.message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let queue_digest = if let Some((agent, status, updated, size)) = queue {
        ensure!(size <= 64 * 1024, "startup queue proof exceeds budget");
        let payload: String = tx.query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id=?1",
            [&invocation.message_id],
            |r| r.get(0),
        )?;
        let record: crate::types::QueueEntryRecord = serde_json::from_str(&payload)?;
        ensure!(
            agent == key.agent_id
                && record.agent_id == agent
                && record.message_id == invocation.message_id
                && serde_json::to_value(&record.status)?.as_str() == Some(status.as_str())
                && updated == crate::runtime_db::repositories::timestamp(record.updated_at)
                && !matches!(status.as_str(), "dequeued" | "interjected"),
            "startup source has an unfinished/new queue claim"
        );
        Some(canonical_command_digest(&record)?)
    } else {
        None
    };
    let (attempt_count, attempt_bytes): (usize,usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(attempt_id AS BLOB))),0) FROM execution_protocol_attempts WHERE agent_id=?1 AND json_extract(payload_json,'$.source_message_id')=?2 AND (lifecycle_state='open' OR json_extract(payload_json,'$.turn_id')=?3 OR json_extract(payload_json,'$.terminal_at')=?4)",
        params![key.agent_id,invocation.message_id,key.actual_dispatch_turn_id,at.to_rfc3339()], |r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        attempt_count <= MAX_PROOF_RECORDS && attempt_bytes <= MAX_PROOF_BYTES,
        "startup attempt proof exceeds budget"
    );
    let sql = "SELECT attempt_id,length(CAST(payload_json AS BLOB)) FROM execution_protocol_attempts WHERE agent_id=?1 AND json_extract(payload_json,'$.source_message_id')=?2 AND (lifecycle_state='open' OR json_extract(payload_json,'$.turn_id')=?3 OR json_extract(payload_json,'$.terminal_at')=?4) ORDER BY attempt_id LIMIT 65537";
    let ids = {
        let mut statement = tx.prepare(sql)?;
        let rows = statement.query_map(
            params![
                key.agent_id,
                invocation.message_id,
                key.actual_dispatch_turn_id,
                at.to_rfc3339()
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, usize>(1)?)),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    ensure!(
        ids.len() <= MAX_PROOF_RECORDS
            && ids
                .iter()
                .try_fold(0usize, |sum, (_, bytes)| sum.checked_add(*bytes))
                .is_some_and(|bytes| bytes <= MAX_PROOF_BYTES),
        "startup attempt proof exceeds budget"
    );
    let mut attempts = Vec::new();
    for (id, _) in ids {
        let (normalized_state,identity,generation,outcome_id,payload): (String,String,u64,Option<String>,String) = tx.query_row("SELECT lifecycle_state,source_identity_json,source_generation,terminal_outcome_id,payload_json FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2", params![key.agent_id,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
        ensure!(
            attempt.agent_id == key.agent_id
                && attempt.attempt_id == id
                && normalized_state == "interrupted"
                && attempt.state == ExecutionAttemptState::Interrupted
                && attempt.source_message_id.as_deref() == Some(invocation.message_id.as_str())
                && serde_json::from_str::<
                    crate::domain::execution_protocol::ExecutionSourceIdentity,
                >(&identity)?
                    == attempt.source.identity
                && generation == attempt.source.generation
                && attempt
                    .run_id
                    .as_deref()
                    .is_none_or(|id| id == original.run_id)
                && attempt
                    .turn_id
                    .as_deref()
                    .is_none_or(|id| id == key.actual_dispatch_turn_id)
                && attempt
                    .terminal_at
                    .as_deref()
                    .context("startup attempt has no terminal time")?
                    .parse::<DateTime<Utc>>()?
                    == at
                && attempt.terminal_outcome_id == outcome_id,
            "startup attempt is not the original recovered dispatch"
        );
        let outcome_id = outcome_id.context("startup attempt has no outcome")?;
        let (outcome_attempt,size): (String,usize)=tx.query_row("SELECT attempt_id,length(CAST(payload_json AS BLOB)) FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id=?2",params![key.agent_id,outcome_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            size <= MAX_PROOF_BYTES,
            "startup outcome proof exceeds budget"
        );
        let payload:String=tx.query_row("SELECT payload_json FROM execution_protocol_outcomes WHERE agent_id=?1 AND outcome_id=?2",params![key.agent_id,outcome_id],|r|r.get(0))?;
        let outcome: ExecutionOutcomeRecord = serde_json::from_str(&payload)?;
        ensure!(
            outcome_attempt == id
                && outcome.attempt_id == id
                && outcome.outcome_id == outcome_id
                && outcome.created_at.parse::<DateTime<Utc>>()? == at
                && interrupted_outcome_matches(&attempt, &outcome),
            "startup recovered outcome disagrees"
        );
        let recovery_audit_id =
            format!("audit:startup-interrupted-attempt:{}:{}", key.agent_id, id);
        let (audit_agent, audit_kind, audit_at, audit_size): (String,String,String,usize) = tx.query_row(
            "SELECT agent_id,kind,created_at,length(CAST(data_json AS BLOB)) FROM audit_events WHERE audit_event_id=?1",
            [&recovery_audit_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        ensure!(
            audit_size <= 64 * 1024,
            "startup attempt audit exceeds proof budget"
        );
        let audit_payload: String = tx.query_row(
            "SELECT data_json FROM audit_events WHERE audit_event_id=?1",
            [&recovery_audit_id],
            |r| r.get(0),
        )?;
        let recovery_audit: AuditEvent = serde_json::from_str(&audit_payload)?;
        ensure!(
            audit_agent == key.agent_id
                && audit_kind == "startup_interrupted_execution_attempt_recovered"
                && recovery_audit.id == recovery_audit_id
                && recovery_audit.kind == audit_kind
                && recovery_audit.created_at == at
                && audit_at == crate::runtime_db::repositories::timestamp(at)
                && recovery_audit.data.get("agent_id").and_then(|x| x.as_str())
                    == Some(key.agent_id.as_str())
                && recovery_audit
                    .data
                    .get("attempt_id")
                    .and_then(|x| x.as_str())
                    == Some(id.as_str())
                && recovery_audit.data.get("reason").and_then(|x| x.as_str())
                    == Some("startup_recovery/daemon_restart"),
            "original attempt lacks canonical startup recovery audit"
        );
        attempts.push(InterruptedAttemptAddress {
            startup_audit_digest: canonical_command_digest(&recovery_audit)?,
            attempt_id: id,
            outcome_id,
            attempt_digest: canonical_command_digest(&attempt)?,
            outcome_digest: canonical_command_digest(&outcome)?,
        });
    }
    Ok(InterruptedAddress {
        audit_digest: canonical_command_digest(&audit)?,
        reason,
        interrupted_at: at,
        queue_digest,
        attempts,
    })
}
fn interrupted_outcome_matches(
    attempt: &ExecutionAttempt,
    outcome: &ExecutionOutcomeRecord,
) -> bool {
    let reason_ok = |reason: &str| {
        matches!(
            reason,
            "startup_recovery/daemon_restart" | "runtime_interrupted"
        )
    };
    match (&attempt.binding, &outcome.outcome) {
        (
            ExecutionBinding::WorkItem { .. },
            ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted { reason }),
        ) => reason_ok(reason),
        (
            ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. },
            ExecutionOutcome::Conversation(ConversationOutcome::Interrupted { reason }),
        ) => reason_ok(reason),
        (
            ExecutionBinding::Command,
            ExecutionOutcome::Command(CommandResult::Applied { references }),
        ) => references == &vec![format!("interrupted:{}", attempt.attempt_id)],
        _ => false,
    }
}

#[cfg(test)]
#[path = "startup_ended_memory_tests.rs"]
mod tests;
