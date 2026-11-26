//! Original results of explicitly prepared input/state transactions.
//! These helpers borrow the caller's original write transaction. Recovery opens
//! a fresh view only after the original synchronous attempt has finished.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
pub(crate) use holon_domain::input_mutation_receipt::{
    canonical_command_digest, InputMutationFamily, PreparedInputMutation, ReceiptError,
};
use holon_domain::input_mutation_receipt::{canonical_result_digest, valid_sha256};
use holon_storage::input_mutation_receipts as receipt_storage;
use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};

use crate::{
    runtime::resident_input_evidence::InputEvidenceScope,
    types::{AgentMessageDeliveryReceipt, AgentMessageDeliveryRecord, AuditEvent},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputMutationScopeOutcome {
    #[serde(default)]
    pub install_keys: super::input_recovery_install::InputMutationInstallKeys,
    pub scope: InputEvidenceScope,
    /// Actual same-transaction events, including their allocated id/seq/epoch.
    /// Payloads retain only original notification responsibility after pruning;
    /// they do not become current AgentState authority or get appended again.
    pub audit_events: Vec<super::transitions::AuditEventAck>,
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

/// Immutable address of the original TaskStop facet inside the whole receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopReceiptBinding {
    pub scope: InputEvidenceScope,
    pub effect_seq: u64,
    pub intent_digest: String,
    pub generation_digest: String,
    pub task_ack_digest: String,
}

/// Immutable original source binding, not a terminal Task or deletion ACK.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SupervisedInvocationAdmissionAck {
    pub parent_scope: InputEvidenceScope,
    pub parent_task_id: String,
    pub child_scope: InputEvidenceScope,
    pub source_message_id: String,
    pub source_turn_id: Option<String>,
}
impl SupervisedInvocationAdmissionAck {
    fn validate(&self, prepared: &PreparedInputMutation) -> Result<()> {
        ensure!(
            prepared.family() == InputMutationFamily::Queue
                && self.parent_scope.agent_id != self.child_scope.agent_id
                && prepared.scopes().len() == 2
                && prepared.scopes().contains(&self.parent_scope)
                && prepared.scopes().contains(&self.child_scope)
                && !self.parent_task_id.trim().is_empty()
                && !self.source_message_id.trim().is_empty()
                && self
                    .source_turn_id
                    .as_ref()
                    .is_none_or(|id| !id.trim().is_empty()),
            ReceiptError::InvalidOutcome("supervised admission binding differs")
        );
        Ok(())
    }
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop: Option<TaskStopReceiptBinding>,
    },
    /// Original focused Stop phase ACK; never current Task/identity authority.
    TaskStopPhase {
        ack: Box<super::task_stop::TaskStopPhaseAck>,
    },
    ChildTaskTerminal {
        ack: Box<super::child_task_terminal::ChildTaskTerminalAck>,
    },
    SupervisedInvocationAdmission {
        ack: Box<SupervisedInvocationAdmissionAck>,
    },
    CommandTaskTerminal {
        ack: Box<super::command_task_terminal::CommandTaskTerminalAck>,
    },
    TaskStatusMetadata {
        ack: Box<super::task_status_metadata::TaskStatusMetadataAck>,
    },
    /// Original successful Result operation, including None/false/empty/zero.
    /// Current installation facts are independently reloaded after lost ACK.
    TaskResult {
        outcome: Box<super::task_result_settlement::TaskResultWriteOutcome>,
    },
    ExternalTrigger {
        outcome: Box<super::transitions::ExternalTriggerMutationOutcome>,
    },
    ContextBudgetStopRecovery {
        outcome: super::transitions::execution_protocol_repository::ExecutionProtocolRecoveryCommitOutcome,
    },
    Timer {
        outcome: Box<super::transitions::TimerMutationOutcome>,
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
                        | InputMutationFamily::TaskResult
                        | InputMutationFamily::Timer
                        | InputMutationFamily::ContextBudgetStopRecovery
                ),
                ReceiptError::InvalidOutcome("result family mismatch")
            ),
            InputMutationResult::Task {
                task_id,
                deletion,
                stop,
            } => {
                ensure!(
                    prepared.family() == InputMutationFamily::Task && !task_id.trim().is_empty(),
                    ReceiptError::InvalidOutcome("Task result family or ID mismatch")
                );
                if let Some(stop) = stop {
                    ensure!(
                        stop.effect_seq > 0
                            && prepared.scopes().contains(&stop.scope)
                            && valid_sha256(&stop.intent_digest)
                            && valid_sha256(&stop.generation_digest)
                            && valid_sha256(&stop.task_ack_digest),
                        ReceiptError::InvalidOutcome("TaskStop receipt binding differs")
                    );
                }
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
            InputMutationResult::TaskStopPhase { ack } => ack.validate(prepared)?,
            InputMutationResult::CommandTaskTerminal { ack } => ack.validate(prepared)?,
            InputMutationResult::ChildTaskTerminal { ack } => ack.validate(prepared)?,
            InputMutationResult::SupervisedInvocationAdmission { ack } => ack.validate(prepared)?,
            InputMutationResult::TaskStatusMetadata { ack } => ack.validate(prepared)?,
            InputMutationResult::ExternalTrigger { outcome } => outcome.validate(prepared)?,
            InputMutationResult::ContextBudgetStopRecovery { outcome } => {
                ensure!(
                    prepared.family() == InputMutationFamily::ContextBudgetStopRecovery
                        && self.applied
                            == matches!(
                                outcome,
                                super::transitions::execution_protocol_repository::ExecutionProtocolRecoveryCommitOutcome::Applied(true)
                            ),
                    ReceiptError::InvalidOutcome("context budget recovery result mismatch")
                );
            }
            InputMutationResult::Timer { outcome } => {
                ensure!(
                    prepared.family() == InputMutationFamily::Timer,
                    ReceiptError::InvalidOutcome("Timer result family mismatch")
                );
                use super::transitions::TimerMutationOutcome;
                match outcome.as_ref() {
                    TimerMutationOutcome::Created { record } => ensure!(
                        prepared
                            .scopes()
                            .iter()
                            .any(|scope| scope.agent_id == record.agent_id)
                            && !record.id.is_empty(),
                        ReceiptError::InvalidOutcome("Timer creation scope mismatch")
                    ),
                    TimerMutationOutcome::Rescheduled { record }
                    | TimerMutationOutcome::ContentEdited { record } => ensure!(
                        prepared
                            .scopes()
                            .iter()
                            .any(|scope| scope.agent_id == record.agent_id)
                            && !record.id.is_empty(),
                        ReceiptError::InvalidOutcome("Timer update scope mismatch")
                    ),
                    TimerMutationOutcome::Fired { result } => {
                        ensure!(
                            result.message.is_some() == result.wake_created
                                && (!result.wake_created || result.advanced),
                            ReceiptError::InvalidOutcome("Timer firing disposition mismatch")
                        );
                        if let Some(message) = &result.message {
                            ensure!(
                                prepared
                                    .scopes()
                                    .iter()
                                    .any(|scope| scope.agent_id == message.agent_id)
                                    && !message.id.is_empty(),
                                ReceiptError::InvalidOutcome("Timer firing Message scope mismatch")
                            );
                        }
                    }
                    TimerMutationOutcome::Cancelled { result } => ensure!(
                        result.cancelled || result.dropped_message_id.is_none(),
                        ReceiptError::InvalidOutcome("Timer cancellation disposition mismatch")
                    ),
                }
            }
            InputMutationResult::TaskResult { outcome } => {
                ensure!(
                    prepared.family() == InputMutationFamily::TaskResult,
                    ReceiptError::InvalidOutcome("TaskResult result family mismatch")
                );
                use super::task_result_settlement::TaskResultWriteOutcome;
                let records: &[super::task_result_settlement::TaskResultSettlementRecord] =
                    match outcome.as_ref() {
                        TaskResultWriteOutcome::Record(record) => record.as_slice(),
                        TaskResultWriteOutcome::Records(records) => records,
                        TaskResultWriteOutcome::Changed(_)
                        | TaskResultWriteOutcome::Count(_)
                        | TaskResultWriteOutcome::Unit => &[],
                    };
                let mut ids = std::collections::BTreeSet::new();
                for record in records {
                    ensure!(
                        !record.result_identity.trim().is_empty()
                            && !record.task_id.trim().is_empty()
                            && !record.message_id.trim().is_empty()
                            && record.source.validates_identity(&record.task_id)
                            && record.rejoin_generation > 0
                            && prepared
                                .scopes()
                                .iter()
                                .any(|scope| scope.agent_id == record.agent_id)
                            && ids.insert(record.result_identity.as_str()),
                        ReceiptError::InvalidOutcome(
                            "TaskResult outcome is outside original group or malformed"
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
                .eq(prepared.scopes().iter()),
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
                    .scopes()
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
    let Some(stored) = receipt_storage::lookup_receipt_on(tx, prepared)? else {
        return Ok(None);
    };
    let outcome: InputMutationOutcome = serde_json::from_str(&stored.outcome_json)?;
    outcome.validate(&stored.prepared)?;
    let committed_at = DateTime::parse_from_rfc3339(&stored.committed_at)?.with_timezone(&Utc);
    ensure!(
        stored.result_digest == canonical_result_digest(&stored.prepared, &outcome, committed_at)?,
        ReceiptError::CorruptReceipt("original result checksum differs")
    );
    Ok(Some(InputMutationReceipt {
        prepared: stored.prepared,
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
    if let InputMutationResult::Task {
        task_id,
        stop: Some(stop),
        ..
    } = &outcome.result
    {
        super::task_stop::validate_task_stop_ack_binding_tx(tx, prepared, task_id, stop)?;
    }
    if let InputMutationResult::TaskStopPhase { ack } = &outcome.result {
        super::task_stop::validate_task_stop_phase_ack_tx(tx, ack)?;
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
                    && actual == **event,
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
    receipt_storage::insert_receipt_tx(
        tx,
        prepared,
        &serde_json::to_string(outcome)?,
        committed_at,
        &canonical_result_digest(prepared, outcome, committed_at)?,
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
    db: &super::RuntimeDb,
    attempt: &OriginalInputMutationAttempt,
    read_current: impl FnOnce(&Transaction<'_>, InputMutationResolution) -> Result<T>,
) -> Result<T> {
    with_fresh_input_mutation_resolution_and_preflight(db, attempt, |_| Ok(()), read_current)
}

/// The owned acquisition guard exists before receipt decoding and remains until
/// the same-view callback completes. It adds no business/recovery authorization.
pub(crate) fn with_fresh_input_mutation_resolution_and_preflight<T, G>(
    db: &super::RuntimeDb,
    attempt: &OriginalInputMutationAttempt,
    preflight: impl FnOnce(&Transaction<'_>) -> Result<G>,
    read_current: impl FnOnce(&Transaction<'_>, InputMutationResolution) -> Result<T>,
) -> Result<T> {
    ensure!(
        attempt.original_attempt_finished(),
        ReceiptError::OriginalAttemptNotFinished
    );
    db.read_for_preparation(|tx| {
        let _material = preflight(tx)?;
        let resolution = match lookup_input_mutation_receipt_tx(tx, attempt.prepared())? {
            Some(receipt) => InputMutationResolution::Present(receipt),
            None => InputMutationResolution::Absent,
        };
        read_current(tx, resolution)
    })
    .map_err(super::persistence::mark_original_input_recovery_failure)
}

/// Positive original receipts may authorize named qualification writes. Reserve
/// the writer before reading that fresh view; a Deferred snapshot cannot safely
/// upgrade after another connection commits. This never reruns the command.
pub(crate) fn with_fresh_input_mutation_preparation_and_preflight<T, G>(
    db: &super::RuntimeDb,
    attempt: &OriginalInputMutationAttempt,
    preflight: impl FnOnce(&Transaction<'_>) -> Result<G>,
    prepare_current: impl FnOnce(&Transaction<'_>, InputMutationResolution) -> Result<T>,
) -> Result<T> {
    ensure!(
        attempt.original_attempt_finished(),
        ReceiptError::OriginalAttemptNotFinished
    );
    db.recovery_transaction_once_with_phases(|tx, phases| {
        use holon_storage::write_callback_phases::WriteCallbackPhase;

        let phases = Some(phases);
        let _material = {
            let _phase = WriteCallbackPhase::RecoveryPreflight.start(phases);
            preflight(tx)?
        };
        let resolution = {
            let _phase = WriteCallbackPhase::RecoveryReceiptLookup.start(phases);
            match lookup_input_mutation_receipt_tx(tx, attempt.prepared())? {
                Some(receipt) => InputMutationResolution::Present(receipt),
                None => InputMutationResolution::Absent,
            }
        };
        let _phase = WriteCallbackPhase::RecoveryPrepare.start(phases);
        prepare_current(tx, resolution)
    })
    .map_err(super::persistence::mark_original_input_recovery_failure)
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
#[path = "input_mutation_receipts_tests.rs"]
mod tests;
