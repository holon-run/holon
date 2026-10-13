//! Original results of explicitly prepared input/state transactions.
//! These helpers borrow the caller's original write transaction. Recovery opens
//! a fresh read view only after the original synchronous attempt has finished.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    runtime::resident_input_evidence::InputEvidenceScope,
    types::{AgentMessageDeliveryReceipt, AgentMessageDeliveryRecord, AuditEvent},
};

pub(crate) const INPUT_MUTATION_RECEIPT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS input_mutation_receipts (
  command_nonce TEXT PRIMARY KEY CHECK(length(command_nonce) > 0),
  family TEXT NOT NULL,
  phase_version INTEGER NOT NULL CHECK(phase_version = 1),
  command_digest TEXT NOT NULL CHECK(length(command_digest) = 64),
  scope_digest TEXT NOT NULL CHECK(length(scope_digest) = 64),
  scopes_json TEXT NOT NULL,
  prepared_at TEXT NOT NULL,
  prepared_json TEXT NOT NULL,
  outcome_json TEXT NOT NULL,
  committed_at TEXT NOT NULL,
  result_digest TEXT NOT NULL CHECK(length(result_digest) = 64)
);
"#;

const PHASE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputMutationFamily {
    AgentPosture,
    Evidence,
    Retention,
    Queue,
    QueueHeadNoProgress,
    Terminal,
    Task,
    WorkItem,
    Wait,
    Focus,
}
impl InputMutationFamily {
    fn as_str(self) -> &'static str {
        match self {
            Self::AgentPosture => "agent_posture",
            Self::Evidence => "evidence",
            Self::Retention => "retention",
            Self::Queue => "queue",
            Self::QueueHeadNoProgress => "queue_head_no_progress",
            Self::Terminal => "terminal",
            Self::Task => "task",
            Self::WorkItem => "work_item",
            Self::Wait => "wait",
            Self::Focus => "focus",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedInputMutation {
    family: InputMutationFamily,
    phase_version: u32,
    command_nonce: String,
    command_digest: String,
    scopes: Vec<InputEvidenceScope>,
    scope_digest: String,
    prepared_at: DateTime<Utc>,
}
impl PreparedInputMutation {
    /// `command_digest` hashes the frozen typed command, never Debug output or
    /// SQL-produced sequences/counts. The nonce is the original attempt address.
    pub(crate) fn new(
        family: InputMutationFamily,
        command_nonce: String,
        mut scopes: Vec<InputEvidenceScope>,
        command_digest: String,
        prepared_at: DateTime<Utc>,
    ) -> Result<Self> {
        ensure!(
            !command_nonce.trim().is_empty(),
            ReceiptError::InvalidDescriptor("empty nonce")
        );
        ensure!(
            valid_sha256(&command_digest),
            ReceiptError::InvalidDescriptor("invalid SHA256 command digest")
        );
        ensure!(
            !scopes.is_empty(),
            ReceiptError::InvalidDescriptor("empty scope group")
        );
        ensure!(
            scopes.iter().all(|scope| !scope.agent_id.trim().is_empty()
                && scope.identity_incarnation > 0
                && scope.identity_incarnation <= i64::MAX as u64),
            ReceiptError::InvalidDescriptor("invalid scope")
        );
        scopes.sort_by(|a, b| {
            (&a.agent_id, a.identity_incarnation).cmp(&(&b.agent_id, b.identity_incarnation))
        });
        ensure!(
            scopes.windows(2).all(|pair| pair[0] != pair[1]),
            ReceiptError::InvalidDescriptor("duplicate scope")
        );
        let scope_digest = canonical_command_digest(&serde_json::json!({
            "family": family,
            "phase_version": PHASE_VERSION,
            "command_digest": command_digest,
            "scopes": scopes,
        }))?;
        Ok(Self {
            family,
            phase_version: PHASE_VERSION,
            command_nonce,
            command_digest,
            scopes,
            scope_digest,
            prepared_at,
        })
    }
    pub(crate) fn family(&self) -> InputMutationFamily {
        self.family
    }
    pub(crate) fn command_nonce(&self) -> &str {
        &self.command_nonce
    }
    pub(crate) fn command_digest(&self) -> &str {
        &self.command_digest
    }
    pub(crate) fn scopes(&self) -> &[InputEvidenceScope] {
        &self.scopes
    }
    pub(crate) fn prepared_at(&self) -> DateTime<Utc> {
        self.prepared_at
    }
    pub(crate) fn scope_digest(&self) -> &str {
        &self.scope_digest
    }

    fn validate(&self) -> Result<()> {
        let expected = Self::new(
            self.family,
            self.command_nonce.clone(),
            self.scopes.clone(),
            self.command_digest.clone(),
            self.prepared_at,
        )?;
        ensure!(
            *self == expected,
            ReceiptError::CorruptReceipt("invalid prepared projection")
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputMutationScopeOutcome {
    #[serde(default)]
    pub install_keys: super::input_recovery_install::InputMutationInstallKeys,
    pub scope: InputEvidenceScope,
    /// Actual same-transaction events, including their allocated id/seq/epoch.
    /// Payloads retain only original notification responsibility after pruning;
    /// they do not become current AgentState authority or get appended again.
    pub audit_events: Vec<AuditEvent>,
    pub notify_memory_index: bool,
    pub notify_scheduler: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputMutationEvidenceFamily {
    Message,
    Turn,
    Transcript,
    ToolExecution,
    Brief,
}

/// Exact original child-deletion ACK. This is a return value, never current
/// identity/State installation authority; recovery reloads the current scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputTaskDeletionResult {
    pub identity: crate::types::AgentIdentityRecord,
    pub job: crate::types::AgentDeletionJob,
    pub created: bool,
}

/// Minimal original public result. Full mutable evidence belongs only to the
/// first ACK; current installation facts are separately loaded in a fresh view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum InputMutationResult {
    #[default]
    Transition,
    Task {
        task_id: String,
        deletion: Option<Box<InputTaskDeletionResult>>,
    },
    Evidence {
        family: InputMutationEvidenceFamily,
        id: String,
        actual_seq: Option<u64>,
        created_event_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_at: Option<chrono::DateTime<Utc>>,
    },
    /// `None` is the original successful stale/no-op disposition, not absence
    /// of this command's whole-transaction receipt.
    QueueHeadNoProgress {
        outcome: Option<crate::runtime_db::transitions::QueueHeadNoProgressOutcome>,
    },
    /// The canonical retention ACK precedes optional vacuum/diagnostic I/O.
    Retention {
        report: Box<crate::runtime_db::RuntimeDbRetentionReport>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputMutationOutcome {
    #[serde(default)]
    pub result: InputMutationResult,
    /// Business application, separate from the existence of a committed receipt.
    /// `false` is a successful no-op, never proof of transaction rollback.
    pub applied: bool,
    pub delivery_receipt: Option<AgentMessageDeliveryReceipt>,
    /// Exactly one ordered entry per original scope, including empty effects.
    pub scopes: Vec<InputMutationScopeOutcome>,
}
impl InputMutationOutcome {
    fn validate(&self, prepared: &PreparedInputMutation) -> Result<()> {
        match &self.result {
            InputMutationResult::Transition => ensure!(
                !matches!(
                    prepared.family(),
                    InputMutationFamily::Evidence
                        | InputMutationFamily::Retention
                        | InputMutationFamily::QueueHeadNoProgress
                        | InputMutationFamily::Task
                ),
                ReceiptError::InvalidOutcome("result family mismatch")
            ),
            InputMutationResult::Task { task_id, deletion } => {
                ensure!(
                    prepared.family() == InputMutationFamily::Task && !task_id.trim().is_empty(),
                    ReceiptError::InvalidOutcome("Task result family or ID mismatch")
                );
                if let Some(deletion) = deletion {
                    let identity = &deletion.identity;
                    let job = &deletion.job;
                    ensure!(
                        identity.agent_id == job.agent_id
                            && identity.incarnation > 0
                            && !job.deletion_id.trim().is_empty()
                            && prepared
                                .scopes()
                                .iter()
                                .any(|scope| scope.agent_id == identity.agent_id
                                    && scope.identity_incarnation == identity.incarnation)
                            && matches!(
                                identity.status,
                                crate::types::AgentRegistryStatus::Deleting
                                    | crate::types::AgentRegistryStatus::Deleted
                            ),
                        ReceiptError::InvalidOutcome(
                            "Task deletion result is outside original group"
                        )
                    );
                }
            }
            InputMutationResult::Evidence {
                id,
                actual_seq,
                created_event_seq,
                ..
            } => {
                ensure!(
                    prepared.family() == InputMutationFamily::Evidence && !id.trim().is_empty(),
                    ReceiptError::InvalidOutcome("evidence result family or id mismatch")
                );
                ensure!(
                    actual_seq.is_none_or(|seq| seq > 0 && seq <= i64::MAX as u64)
                        && created_event_seq.is_none_or(|seq| seq > 0 && seq <= i64::MAX as u64),
                    ReceiptError::InvalidOutcome("invalid evidence result sequence")
                );
            }
            InputMutationResult::QueueHeadNoProgress { outcome } => {
                ensure!(
                    prepared.family() == InputMutationFamily::QueueHeadNoProgress,
                    ReceiptError::InvalidOutcome("queue progress result family mismatch")
                );
                ensure!(
                    self.applied == outcome.is_some(),
                    ReceiptError::InvalidOutcome(
                        "queue progress application differs from original outcome"
                    )
                );
                if let Some(outcome) = outcome {
                    use crate::runtime_db::transitions::QueueHeadNoProgressOutcome;
                    let (attempt, max_attempts, quarantined) = match outcome {
                        QueueHeadNoProgressOutcome::BoundedDefer {
                            attempt,
                            max_attempts,
                        } => (*attempt, *max_attempts, false),
                        QueueHeadNoProgressOutcome::Quarantined {
                            attempt,
                            max_attempts,
                        } => (*attempt, *max_attempts, true),
                    };
                    ensure!(
                        attempt > 0
                            && max_attempts > 0
                            && ((attempt >= max_attempts) == quarantined),
                        ReceiptError::InvalidOutcome("invalid original queue progress attempt")
                    );
                }
            }
            InputMutationResult::Retention { .. } => ensure!(
                prepared.family() == InputMutationFamily::Retention,
                ReceiptError::InvalidOutcome("retention result family mismatch")
            ),
        }
        ensure!(
            self.scopes
                .iter()
                .map(|item| &item.scope)
                .eq(prepared.scopes.iter()),
            ReceiptError::GroupConflict
        );
        let mut ids = std::collections::BTreeSet::new();
        for item in &self.scopes {
            item.install_keys.validate()?;
            for event in &item.audit_events {
                ensure!(
                    !event.id.trim().is_empty()
                        && event.event_seq > 0
                        && event.event_seq <= i64::MAX as u64
                        && !event.event_log_epoch.is_empty(),
                    ReceiptError::InvalidOutcome("audit event is not an acknowledged event")
                );
                ensure!(
                    ids.insert(event.id.as_str()),
                    ReceiptError::InvalidOutcome("duplicate original audit event")
                );
            }
        }
        if let Some(receipt) = &self.delivery_receipt {
            ensure!(
                prepared
                    .scopes
                    .iter()
                    .any(|scope| scope.agent_id == receipt.target_agent_id),
                ReceiptError::InvalidOutcome("delivery target is outside original group")
            );
            ensure!(
                !receipt.delivery_id.trim().is_empty(),
                ReceiptError::InvalidOutcome("empty delivery identity")
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputMutationReceipt {
    pub prepared: PreparedInputMutation,
    pub outcome: InputMutationOutcome,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReceiptError {
    #[error("invalid prepared input mutation: {0}")]
    InvalidDescriptor(&'static str),
    #[error("original input command nonce has a different family or payload")]
    CommandConflict,
    #[error("original input command has a different complete scope group")]
    GroupConflict,
    #[error("input mutation receipt is inconsistent: {0}")]
    CorruptReceipt(&'static str),
    #[error("invalid input mutation outcome: {0}")]
    InvalidOutcome(&'static str),
    #[error("original synchronous input mutation attempt has not finished")]
    OriginalAttemptNotFinished,
}

/// Retained with the owner recovery group before SQL. A second attempt with the
/// same descriptor has a different Arc identity and cannot resolve this one.
#[derive(Debug, Clone)]
pub(crate) struct OriginalInputMutationAttempt {
    prepared: PreparedInputMutation,
    finished: Arc<AtomicBool>,
}
impl OriginalInputMutationAttempt {
    pub(crate) fn new(prepared: PreparedInputMutation) -> Self {
        Self {
            prepared,
            finished: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(crate) fn prepared(&self) -> &PreparedInputMutation {
        &self.prepared
    }
    pub(crate) fn finish_original_attempt(&self) {
        self.finished.store(true, Ordering::Release);
    }
    pub(crate) fn original_attempt_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub(crate) fn same_attempt(&self, other: &Self) -> bool {
        self.prepared == other.prepared && Arc::ptr_eq(&self.finished, &other.finished)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InputMutationResolution {
    Present(InputMutationReceipt),
    /// Only meaningful after matching the original owner/group/epoch/attempt.
    /// Never authorizes a different command or latest-state success inference.
    Absent,
}

/// Use inside the original transaction before business OCC/reducer execution.
/// Identity/authorization remains the entrypoint's responsibility.
pub(crate) fn lookup_input_mutation_receipt_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputMutation,
) -> Result<Option<InputMutationReceipt>> {
    lookup_input_mutation_receipt_in(tx, prepared)
}

pub(super) fn lookup_input_mutation_receipt_in(
    tx: &Connection,
    prepared: &PreparedInputMutation,
) -> Result<Option<InputMutationReceipt>> {
    prepared.validate()?;
    type Row = (
        String,
        u32,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let row: Option<Row> = tx.query_row(
        "SELECT family,phase_version,command_digest,scope_digest,scopes_json,prepared_at,prepared_json,outcome_json,committed_at,result_digest
         FROM input_mutation_receipts WHERE command_nonce=?1", [prepared.command_nonce()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)),
    ).optional()?;
    let Some((
        family,
        version,
        digest,
        scope_digest,
        scopes_json,
        prepared_at,
        prepared_json,
        outcome_json,
        committed_at,
        result_digest,
    )) = row
    else {
        return Ok(None);
    };
    let stored: PreparedInputMutation = serde_json::from_str(&prepared_json)
        .context("decoding original input mutation descriptor")?;
    stored.validate()?;
    let scopes: Vec<InputEvidenceScope> = serde_json::from_str(&scopes_json)?;
    ensure!(
        stored.family.as_str() == family
            && stored.phase_version == version
            && stored.command_nonce == prepared.command_nonce
            && stored.command_digest == digest
            && stored.scope_digest == scope_digest
            && stored.scopes == scopes
            && timestamp(stored.prepared_at) == prepared_at,
        ReceiptError::CorruptReceipt("normalized descriptor differs from payload")
    );
    ensure!(
        stored.scopes == prepared.scopes,
        ReceiptError::GroupConflict
    );
    ensure!(stored == *prepared, ReceiptError::CommandConflict);
    let outcome: InputMutationOutcome = serde_json::from_str(&outcome_json)?;
    outcome.validate(&stored)?;
    let committed_at = DateTime::parse_from_rfc3339(&committed_at)?.with_timezone(&Utc);
    ensure!(
        result_digest
            == canonical_command_digest(&serde_json::json!({
                "prepared":stored,"outcome":outcome,"committed_at":committed_at,
            }))?,
        ReceiptError::CorruptReceipt("original result checksum differs")
    );
    Ok(Some(InputMutationReceipt {
        prepared: stored,
        outcome,
        committed_at,
    }))
}

/// Call only after all canonical writes, audit/index intents, qualification and
/// exact readbacks (including State count epilogues) in the original transaction.
pub(crate) fn insert_input_mutation_receipt_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputMutation,
    outcome: &InputMutationOutcome,
    committed_at: DateTime<Utc>,
) -> Result<InputMutationReceipt> {
    prepared.validate()?;
    outcome.validate(prepared)?;
    if let Some(stored) = lookup_input_mutation_receipt_tx(tx, prepared)? {
        ensure!(
            stored.outcome == *outcome && stored.committed_at == committed_at,
            ReceiptError::CorruptReceipt("attempt to replace immutable original outcome")
        );
        return Ok(stored);
    }
    // Initial acknowledgement binds actual SQL events, not command copies with
    // unallocated sequence/epoch. Later recovery uses the preserved payloads.
    for item in &outcome.scopes {
        super::input_recovery_install::validate_original_install_keys_tx(
            tx,
            &item.scope,
            &item.install_keys,
        )?;
        for event in &item.audit_events {
            let (agent, seq, payload): (Option<String>, u64, String) = tx.query_row(
                "SELECT agent_id,event_seq,data_json FROM audit_events WHERE audit_event_id=?1",
                [&event.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let actual: AuditEvent = serde_json::from_str(&payload)?;
            ensure!(
                agent.as_deref() == Some(item.scope.agent_id.as_str())
                    && seq == event.event_seq
                    && actual == *event,
                ReceiptError::InvalidOutcome("audit event differs from actual SQL ACK")
            );
        }
    }
    if let Some(receipt) = &outcome.delivery_receipt {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM agent_message_deliveries WHERE delivery_id=?1",
            [&receipt.delivery_id],
            |r| r.get(0),
        )?;
        let actual: AgentMessageDeliveryRecord = serde_json::from_str(&payload)?;
        ensure!(
            actual.receipt(receipt.idempotent_replay) == *receipt,
            ReceiptError::InvalidOutcome("delivery outcome differs from actual SQL ACK")
        );
    }
    tx.execute(
        "INSERT INTO input_mutation_receipts(command_nonce,family,phase_version,command_digest,scope_digest,scopes_json,prepared_at,prepared_json,outcome_json,committed_at,result_digest)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![prepared.command_nonce,prepared.family.as_str(),prepared.phase_version,
            prepared.command_digest,prepared.scope_digest,serde_json::to_string(&prepared.scopes)?,
            timestamp(prepared.prepared_at),serde_json::to_string(prepared)?,
            serde_json::to_string(outcome)?,timestamp(committed_at),
            canonical_command_digest(&serde_json::json!({"prepared":prepared,"outcome":outcome,"committed_at":committed_at}))?],
    )?;
    let stored = lookup_input_mutation_receipt_tx(tx, prepared)?
        .context("input mutation receipt missing after write")?;
    ensure!(
        stored.outcome == *outcome && stored.committed_at == committed_at,
        ReceiptError::CorruptReceipt("original outcome exact readback differs")
    );
    Ok(stored)
}

/// Opens the read transaction after finished is observed; the callback can read
/// current identity/State/queue/qualified roots from exactly this same view.
/// It must not perform business writes. Owner/group/epoch/same-attempt checks
/// remain mandatory before and after this narrow storage resolution.
pub(crate) fn with_fresh_input_mutation_resolution<T>(
    connection: &mut Connection,
    attempt: &OriginalInputMutationAttempt,
    read_current: impl FnOnce(&Transaction<'_>, InputMutationResolution) -> Result<T>,
) -> Result<T> {
    ensure!(
        attempt.original_attempt_finished(),
        ReceiptError::OriginalAttemptNotFinished
    );
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let resolution = match lookup_input_mutation_receipt_tx(&tx, attempt.prepared())? {
        Some(receipt) => InputMutationResolution::Present(receipt),
        None => InputMutationResolution::Absent,
    };
    let result = read_current(&tx, resolution)?;
    tx.commit()?;
    Ok(result)
}

/// Stable serde encoding with recursively ordered object keys. Callers freeze
/// typed command clocks/IDs first; results allocated by SQL are not inputs.
pub(crate) fn canonical_command_digest(command: &impl Serialize) -> Result<String> {
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted = map
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>();
                serde_json::Value::Object(
                    sorted.into_iter().map(|(k, v)| (k, canonical(v))).collect(),
                )
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            }
            value => value,
        }
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical(serde_json::to_value(
            command
        )?))?)
    ))
}
fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
#[path = "input_mutation_receipts_tests.rs"]
mod tests;
