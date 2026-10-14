//! Original TaskStop intent and independently acknowledged exit/result progress.
//! No durable target is a process capability. Local leases are held by runtime.

use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::input_mutation_receipts::{
    canonical_command_digest, lookup_input_mutation_receipt_tx, InputMutationFamily,
    InputMutationResult, PreparedInputMutation, TaskStopReceiptBinding,
};
use crate::{
    domain::execution_protocol::RejoinFence,
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
            OwnedBytesEstimate,
        },
        resident_input_evidence::InputEvidenceScope,
        resident_task::{TaskFactLocator, TaskFamily},
    },
    types::{
        AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin,
        TaskKind, TaskRecord, TaskStatus,
    },
};

#[path = "task_stop_child.rs"]
mod child;
pub(crate) use child::{
    prepare_task_stop_archive_material_tx, prepare_task_stop_child_identity_tx,
    tombstone_task_stop_child_tx, PreparedTaskStopChildIdentity,
};

mod worktree;
pub(crate) use worktree::{prepare_original_task_stop_worktree_tx, OriginalTaskStopWorktreeRead};
mod memory_binding;
mod owner_state;
pub(crate) use memory_binding::load_task_stop_phase_memory_binding_tx;
pub(crate) use owner_state::{
    observe_original_task_stop_owner_state_tx, OriginalTaskStopOwnerState,
};

mod continuation;
pub(crate) use continuation::{
    acknowledge_task_stop_result_tx, original_task_generation_matches_tx,
    prepare_task_stop_result_tx, OriginalTaskStopResultView, TaskStopResultAddress,
    TaskStopResultObservation, TaskStopResultOutcome, TaskStopResultUnavailable,
};
mod scope_inventory;
pub(crate) use scope_inventory::pending_task_stop_host_scope_inventory_tx;
#[cfg(test)]
pub(crate) use scope_inventory::pending_task_stop_scope_inventory_tx;
mod phase;
mod readonly_recovery;
mod sparse_recovery;
pub(crate) use phase::{
    apply_task_stop_progress_tx, prepare_task_stop_phase_tx, validate_task_stop_phase_ack_tx,
    ActualTaskStopProgress, TaskStopDomainInvalidation, TaskStopPhaseAck, TaskStopPhaseAddress,
    TaskStopPhaseError, TaskStopPhaseInstallMode, TaskStopProgressAction,
    TaskStopProgressAuthorization,
};
pub(in crate::runtime_db) use phase::{
    observe_task_stop_phase_mode_tx, task_stop_phase_identity_material_tx,
};
pub(crate) use readonly_recovery::{
    observe_initial_task_stop_readonly_recovery_tx, observe_task_stop_readonly_identity_tx,
    validate_task_stop_readonly_child_tx, InitialTaskStopReadonlyRecovery,
    TaskStopReadonlyIdentity,
};
pub(crate) use sparse_recovery::{
    TaskStopSparseReadView, TaskStopSparseRecoveryEvidence, TaskStopSparseStatePresence,
};

pub(super) const TASK_STOP_SCHEMA: &str = include_str!("task_stop/schema.sql");
const MAX_RECORDS: usize = 65_536;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStopSignal {
    Cancel,
    Force,
    Abort,
    NoHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopChildTarget {
    pub scope: InputEvidenceScope,
    pub parent_agent_id: String,
    pub creation_task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopWorktreeTarget {
    pub path: String,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopResultPlan {
    pub phase_nonce: String,
    pub message: MessageEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedTaskStop {
    pub task: TaskFactLocator,
    pub kind: TaskKind,
    pub created_at: DateTime<Utc>,
    pub rejoin: Option<RejoinFence>,
    pub owner_epoch: u64,
    pub signal: TaskStopSignal,
    pub lease_id: Option<String>,
    pub child: Option<TaskStopChildTarget>,
    /// Missing original identity is explicit; it never grants archive authority.
    pub child_unavailable: Option<String>,
    pub worktree: Option<TaskStopWorktreeTarget>,
    pub worktree_unavailable: Option<String>,
    pub result: Option<TaskStopResultPlan>,
}
impl PreparedTaskStop {
    pub(crate) fn generation_digest(&self) -> Result<String> {
        canonical_command_digest(&(
            &self.task.scope,
            &self.task.key,
            self.kind,
            self.created_at,
            &self.rejoin,
        ))
    }
    /// This only freezes data. The original Task proof and original child
    /// reservation must independently recheck it inside the business transaction.
    pub(crate) fn validate_original(&self, original: &TaskRecord) -> Result<()> {
        ensure!(
            self.task.key.family == TaskFamily::Task
                && self.task.key.id == original.id
                && self.task.scope.agent_id == original.agent_id
                && self.kind == original.kind
                && self.created_at == original.created_at
                && self.rejoin == original.rejoin_fence().ok(),
            "TaskStop original generation differs"
        );
        ensure!(
            self.task.scope.identity_incarnation > 0
                && self.owner_epoch > 0
                && self.task.revision > 0
                && valid_digest(&self.task.content_digest)
                && valid_digest(&self.task.projection_digest),
            "TaskStop original locator invalid"
        );
        ensure!(
            self.lease_id.as_deref().is_none_or(|id| !id.is_empty()),
            "TaskStop empty lease"
        );
        ensure!(
            (self.signal == TaskStopSignal::NoHandle) == self.lease_id.is_none(),
            "TaskStop signal and exact lease differ"
        );
        ensure!(
            match self.kind {
                TaskKind::CommandTask => matches!(
                    self.signal,
                    TaskStopSignal::Cancel | TaskStopSignal::Force | TaskStopSignal::NoHandle
                ),
                _ => matches!(
                    self.signal,
                    TaskStopSignal::Abort | TaskStopSignal::NoHandle
                ),
            },
            "TaskStop signal has another Task kind"
        );
        if let Some(child) = &self.child {
            ensure!(
                child.scope.identity_incarnation > 0
                    && child.scope.agent_id != original.agent_id
                    && child.parent_agent_id == original.agent_id
                    && child.creation_task_id == original.id
                    && original.is_child_agent_task()
                    && detail_string(original, "child_agent_id")
                        == Some(child.scope.agent_id.as_str()),
                "TaskStop child association differs"
            );
        }
        let requires_child =
            original.is_child_agent_task() && detail_string(original, "child_agent_id").is_some();
        ensure!(
            requires_child == (self.child.is_some() || self.child_unavailable.is_some())
                && !(self.child.is_some() && self.child_unavailable.is_some()),
            "TaskStop omits or duplicates original child responsibility"
        );
        if let Some(worktree) = &self.worktree {
            let artifact = original
                .detail
                .as_ref()
                .and_then(|detail| detail.get("worktree"));
            ensure!(
                original.is_worktree_child_agent_task()
                    && self.child.is_some()
                    && !worktree.path.is_empty()
                    && !worktree.branch.is_empty()
                    && artifact
                        .and_then(|a| a.get("worktree_path"))
                        .and_then(|v| v.as_str())
                        == Some(worktree.path.as_str())
                    && artifact
                        .and_then(|a| a.get("worktree_branch"))
                        .and_then(|v| v.as_str())
                        == Some(worktree.branch.as_str()),
                "TaskStop worktree differs from original artifact"
            );
        }
        ensure!(
            original.is_worktree_child_agent_task()
                == (self.worktree.is_some() || self.worktree_unavailable.is_some())
                && !(self.worktree.is_some() && self.worktree_unavailable.is_some()),
            "TaskStop omits or duplicates original worktree responsibility"
        );
        for reason in [&self.child_unavailable, &self.worktree_unavailable]
            .into_iter()
            .flatten()
        {
            ensure!(
                !reason.is_empty() && reason.len() <= 4096,
                "TaskStop unavailable reason invalid"
            );
        }
        match (&self.result, self.kind) {
            (None, TaskKind::CommandTask) => {}
            (Some(result), kind) if kind != TaskKind::CommandTask => {
                ensure!(
                    !result.phase_nonce.is_empty()
                        && !result.message.id.is_empty()
                        && result.message.agent_id == original.agent_id
                        && result.message.kind == MessageKind::TaskResult
                        && result.message.delivery_surface
                            == Some(MessageDeliverySurface::TaskRejoin)
                        && result.message.authority_class == AuthorityClass::RuntimeInstruction
                        && matches!(&result.message.origin, MessageOrigin::Task { task_id } if task_id == &original.id)
                        && result.message.task_id.as_deref() == Some(original.id.as_str()),
                    "TaskStop original Result address differs"
                );
            }
            _ => anyhow::bail!("TaskStop original Result responsibility missing or unexpected"),
        }
        Ok(())
    }
}
fn detail_string<'a>(task: &'a TaskRecord, key: &str) -> Option<&'a str> {
    task.detail.as_ref()?.get(key)?.as_str()
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStopPhysicalState {
    Pending,
    MonitorJoined,
    MissingProof,
    NeedsAttention,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStopChildState {
    NotRequired,
    Pending,
    RetiredJoined,
    NeedsAttention,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStopWorktreeState {
    NotRequired,
    Pending,
    Removed,
    AlreadyRemoved,
    Retained,
    Failed,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStopResultState {
    NotRequired,
    Pending,
    Completed,
    NeedsAttention,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopProgress {
    pub physical: TaskStopPhysicalState,
    pub child: TaskStopChildState,
    pub worktree: TaskStopWorktreeState,
    pub result: TaskStopResultState,
    pub reason: Option<String>,
}
impl TaskStopProgress {
    pub(crate) fn initial(intent: &PreparedTaskStop) -> Self {
        Self {
            physical: if intent.lease_id.is_some() {
                TaskStopPhysicalState::Pending
            } else {
                TaskStopPhysicalState::MissingProof
            },
            child: if intent.child.is_some() {
                TaskStopChildState::Pending
            } else if intent.child_unavailable.is_some() {
                TaskStopChildState::NeedsAttention
            } else {
                TaskStopChildState::NotRequired
            },
            worktree: if intent.worktree.is_some() {
                TaskStopWorktreeState::Pending
            } else if intent.worktree_unavailable.is_some() {
                TaskStopWorktreeState::Retained
            } else {
                TaskStopWorktreeState::NotRequired
            },
            result: if intent.result.is_some() {
                TaskStopResultState::Pending
            } else {
                TaskStopResultState::NotRequired
            },
            reason: None,
        }
    }
    pub(crate) fn is_pending(&self) -> bool {
        self.physical == TaskStopPhysicalState::Pending
            || self.child == TaskStopChildState::Pending
            || self.worktree == TaskStopWorktreeState::Pending
            || self.result == TaskStopResultState::Pending
    }
    fn validate(&self, intent: &PreparedTaskStop) -> Result<()> {
        ensure!(
            (self.child == TaskStopChildState::NotRequired)
                == (intent.child.is_none() && intent.child_unavailable.is_none())
                && (self.worktree == TaskStopWorktreeState::NotRequired)
                    == (intent.worktree.is_none() && intent.worktree_unavailable.is_none())
                && (self.result == TaskStopResultState::NotRequired) == intent.result.is_none(),
            "TaskStop progress omits an original responsibility"
        );
        ensure!(
            intent.lease_id.is_some()
                || matches!(
                    self.physical,
                    TaskStopPhysicalState::MissingProof | TaskStopPhysicalState::NeedsAttention
                ),
            "TaskStop no-handle plan cannot prove monitor joined"
        );
        ensure!(
            intent.child_unavailable.is_none() || self.child == TaskStopChildState::NeedsAttention,
            "TaskStop unavailable child cannot prove retirement"
        );
        ensure!(
            intent.worktree_unavailable.is_none()
                || self.worktree == TaskStopWorktreeState::Retained,
            "TaskStop unavailable artifact cannot prove removal"
        );
        ensure!(
            self.reason.as_ref().is_none_or(|s| s.len() <= 4096),
            "TaskStop reason exceeds finite projection"
        );
        Ok(())
    }
    fn validate_successor(&self, next: &Self, intent: &PreparedTaskStop) -> Result<()> {
        self.validate(intent)?;
        next.validate(intent)?;
        ensure!(
            self.physical == TaskStopPhysicalState::Pending || self.physical == next.physical,
            "TaskStop physical terminal changed"
        );
        ensure!(
            self.child == TaskStopChildState::Pending || self.child == next.child,
            "TaskStop child terminal changed"
        );
        ensure!(
            self.worktree == TaskStopWorktreeState::Pending || self.worktree == next.worktree,
            "TaskStop worktree terminal changed"
        );
        ensure!(
            self.result == TaskStopResultState::Pending || self.result == next.result,
            "TaskStop Result terminal changed"
        );
        Ok(())
    }
}

/// Independent from the canonical Task body; latest heads and pending coverage
/// are installed in the Task root with the same domain revision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopRecord {
    pub effect_seq: u64,
    pub prepared: PreparedInputMutation,
    pub intent: PreparedTaskStop,
    pub intent_digest: String,
    pub generation_digest: String,
    pub ack_task: TaskRecord,
    pub ack_task_digest: String,
    pub revision: u64,
    pub progress: TaskStopProgress,
    pub result_nonce: Option<String>,
    pub updated_at: DateTime<Utc>,
}
impl TaskStopRecord {
    pub(crate) fn resident_bytes(&self) -> Result<u64> {
        serialized_bytes(self)?
            .checked_add(u64::try_from(std::mem::size_of::<Self>())?)
            .context("TaskStop resident estimate overflow")
    }
    pub(crate) fn receipt_binding(&self) -> TaskStopReceiptBinding {
        TaskStopReceiptBinding {
            scope: self.intent.task.scope.clone(),
            effect_seq: self.effect_seq,
            intent_digest: self.intent_digest.clone(),
            generation_digest: self.generation_digest.clone(),
            task_ack_digest: self.ack_task_digest.clone(),
        }
    }
    pub(crate) fn validate_resident(&self) -> Result<()> {
        self.validate()
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.prepared.family() == InputMutationFamily::Task
                && self.prepared.scopes().contains(&self.intent.task.scope)
                && self.revision > 0
                && self.effect_seq > 0
                && canonical_command_digest(&self.intent)? == self.intent_digest,
            "TaskStop original descriptor/digest differs"
        );
        ensure!(
            self.intent.generation_digest()? == self.generation_digest,
            "TaskStop generation differs"
        );
        self.intent.validate_original(&self.ack_task)?;
        ensure!(
            canonical_command_digest(&self.ack_task)? == self.ack_task_digest,
            "TaskStop original Task ACK digest differs"
        );
        let expected_status = if self.intent.kind == TaskKind::CommandTask
            && self.intent.signal != TaskStopSignal::NoHandle
        {
            TaskStatus::Cancelling
        } else {
            TaskStatus::Cancelled
        };
        ensure!(
            self.ack_task.status == expected_status,
            "TaskStop original Task ACK status differs"
        );
        self.progress.validate(&self.intent)?;
        ensure!(
            match self.progress.result {
                TaskStopResultState::Completed =>
                    self.result_nonce.as_deref()
                        == self.intent.result.as_ref().map(|r| r.phase_nonce.as_str()),
                _ => self.result_nonce.is_none(),
            },
            "TaskStop Result receipt address differs"
        );
        Ok(())
    }
}

/// Not Clone or Deserialize. Its references borrow this original SQL attempt.
pub(crate) struct TaskStopWriteProof<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    prepared: &'tx PreparedInputMutation,
    intent: &'tx PreparedTaskStop,
    request: &'tx super::task_snapshot::TaskDomainLoadRequest,
    ledger: &'tx InputEvidenceBudgetLedger,
    original: TaskRecord,
    _budget: Arc<BudgetViewGuard>,
    _original_budget: Arc<BudgetViewGuard>,
}
pub(crate) fn prepare_task_stop_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    prepared: &'tx PreparedInputMutation,
    intent: &'tx PreparedTaskStop,
    original: &super::task_snapshot::TaskWriteProof,
    request: &'tx super::task_snapshot::TaskDomainLoadRequest,
    ledger: &'tx InputEvidenceBudgetLedger,
) -> Result<TaskStopWriteProof<'tx, 'connection>> {
    ensure!(
        prepared.family() == InputMutationFamily::Task
            && prepared.scopes().contains(&intent.task.scope)
            && original.scope() == &intent.task.scope
            && intent
                .child
                .as_ref()
                .is_none_or(|child| prepared.scopes().contains(&child.scope)),
        "TaskStop original whole scope differs"
    );
    ensure!(
        intent
            .result
            .as_ref()
            .is_none_or(|result| result.phase_nonce != prepared.command_nonce()),
        "TaskStop continuation reused original nonce"
    );
    let before = original.task_before(&intent.task.key.id)?;
    let scope_bytes:usize = tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(mode AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1),0)",
        params![intent.task.scope.agent_id,intent.task.scope.identity_incarnation],|row|row.get(0))?;
    let material = serialized_bytes(&(prepared, intent, before))?
        .checked_add(u64::try_from(scope_bytes)?)
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(2048))
        .context("TaskStop intent material overflow")?;
    ensure!(
        8 <= request.record_limit.min(MAX_RECORDS)
            && material <= u64::try_from(request.byte_limit.min(MAX_BYTES))?,
        "TaskStop intent ResourceExhausted before clone"
    );
    let budget = ledger.register_owned_bytes(
        &intent.task.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: material,
            metadata_estimate_bytes: 0,
        },
    )?;
    original.validate_task_stop_scope_tx(tx)?;
    intent.validate_original(before)?;
    ensure!(
        crate::runtime::task_generation::TaskRejoinGeneration::of(before).is_some(),
        "TaskStop original Task has an invalid rejoin generation"
    );
    let actual = super::task_snapshot::prepare_task_sql_readback_tx(
        tx,
        &intent.task.scope,
        &intent.task.key.id,
        request,
        ledger,
    )?;
    ensure!(
        &actual.task == before
            && actual.content_digest == intent.task.content_digest
            && actual.projection_digest == intent.task.projection_digest,
        "TaskStop original canonical Task differs"
    );
    let alias: Option<(u64, String, Option<String>, Option<String>)> = tx.query_row(
        "SELECT revision,availability,content_digest,projection_digest FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3",
        params![intent.task.scope.agent_id,intent.task.scope.identity_incarnation,intent.task.key.id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?;
    ensure!(
        alias
            == Some((
                intent.task.revision,
                "present".into(),
                Some(intent.task.content_digest.clone()),
                Some(intent.task.projection_digest.clone())
            )),
        "TaskStop original Task locator changed"
    );
    Ok(TaskStopWriteProof {
        tx,
        prepared,
        intent,
        request,
        ledger,
        original: actual.task,
        _budget: budget,
        _original_budget: actual.budget,
    })
}

impl TaskStopWriteProof<'_, '_> {
    /// A restarted supervised child can have an Interrupted Task but no local
    /// monitor. Only this original Stop may acknowledge its cancellation.
    pub(crate) fn interrupted_child_cancellation(
        &self,
        tx: &Transaction<'_>,
        incoming: &TaskRecord,
    ) -> Result<bool> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "TaskStop proof belongs to another transaction"
        );
        if self.original.status != TaskStatus::Interrupted
            || !self.original.is_child_agent_task()
            || self.intent.signal != TaskStopSignal::NoHandle
        {
            return Ok(false);
        }
        self.intent.validate_original(&self.original)?;
        ensure!(
            detail_string(&self.original, "child_agent_id").is_some(),
            "interrupted child Stop lacks its original association"
        );
        let mut expected = self.original.clone();
        expected.status = TaskStatus::Cancelled;
        expected.updated_at = self.prepared.prepared_at();
        expected.parent_message_id = None;
        expected
            .summary
            .get_or_insert_with(|| "task cancelled".into());
        expected
            .detail
            .get_or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .context("interrupted child Stop detail is not an object")?
            .insert("task_status".into(), serde_json::json!("cancelled"));
        ensure!(
            incoming == &expected && incoming.updated_at >= self.original.updated_at,
            "interrupted child Stop changed its original frozen Task fields"
        );
        Ok(true)
    }
}

/// Only a real INSERT under its original borrowed write proof can mint this.
/// It is valid only until the original whole receipt is written in this TX.
pub(crate) struct TaskStopInsertedObservation<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    prepared: &'tx PreparedInputMutation,
    intent: &'tx PreparedTaskStop,
    effect_seq: u64,
    record_digest: String,
    intent_digest: String,
    generation_digest: String,
    ack_task_digest: String,
    _budget: Arc<BudgetViewGuard>,
    _readback_budget: Arc<BudgetViewGuard>,
}
impl TaskStopInsertedObservation<'_, '_> {
    pub(in crate::runtime_db) fn validate_presence_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "Stop INSERT observation crossed transaction"
        );
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_stop_effects WHERE effect_seq=?1 AND command_nonce=?2 AND agent_id=?3 AND identity_incarnation=?4 AND intent_digest=?5 AND generation_digest=?6 AND ack_task_digest=?7)",
            params![self.effect_seq,self.prepared.command_nonce(),self.intent.task.scope.agent_id,
                self.intent.task.scope.identity_incarnation,self.intent_digest,self.generation_digest,self.ack_task_digest],|r|r.get(0))?;
        ensure!(exists, "Stop INSERT observation lost its actual row");
        Ok(())
    }
    pub(in crate::runtime_db) fn validate_scope_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        self.validate_presence_tx(tx)?;
        ensure!(
            self.intent.task.scope == *scope,
            "Stop INSERT observation has another Task scope"
        );
        Ok(())
    }
    fn validates(&self, tx: &Transaction<'_>, record: &TaskStopRecord) -> Result<bool> {
        self.validate_presence_tx(tx)?;
        if record.effect_seq != self.effect_seq {
            return Ok(false);
        }
        ensure!(
            record.prepared == *self.prepared
                && record.intent == *self.intent
                && record.intent_digest == self.intent_digest
                && record.generation_digest == self.generation_digest
                && record.ack_task_digest == self.ack_task_digest
                && canonical_command_digest(record)? == self.record_digest,
            "Stop INSERT observation actual row changed"
        );
        Ok(true)
    }
}

/// Borrows the same transaction as the original pre-SQL proof. The caller's
/// candidate is checked against actual canonical SQL, never treated as its ACK.
pub(crate) fn insert_task_stop_with_observation_ack_tx<'tx, 'connection>(
    proof: &TaskStopWriteProof<'tx, 'connection>,
    expected_task: &TaskRecord,
) -> Result<(
    TaskStopRecord,
    TaskStopInsertedObservation<'tx, 'connection>,
)> {
    let tx = proof.tx;
    let actual = super::task_snapshot::prepare_task_sql_readback_tx(
        tx,
        &proof.intent.task.scope,
        &proof.intent.task.key.id,
        proof.request,
        proof.ledger,
    )?;
    proof.intent.validate_original(&actual.task)?;
    ensure!(
        crate::runtime::task_generation::same_task_rejoin_generation(&proof.original, &actual.task),
        "TaskStop actual ACK changed its original raw rejoin generation"
    );
    ensure!(
        actual.task == super::repositories::slim_task_record_for_payload(expected_task),
        "TaskStop actual Task ACK differs from original command"
    );
    let expected_status = if proof.intent.kind == TaskKind::CommandTask
        && proof.intent.signal != TaskStopSignal::NoHandle
    {
        TaskStatus::Cancelling
    } else {
        TaskStatus::Cancelled
    };
    ensure!(
        actual.task.status == expected_status,
        "TaskStop actual Task status differs"
    );
    if proof.intent.kind == TaskKind::CommandTask {
        ensure!(
            actual
                .task
                .detail
                .as_ref()
                .and_then(|v| v.get("cancel_requested"))
                .and_then(|v| v.as_bool())
                == Some(true),
            "TaskStop Command cancel request missing from actual Task"
        );
    }
    let mut record = TaskStopRecord {
        effect_seq: 0,
        prepared: proof.prepared.clone(),
        intent: proof.intent.clone(),
        intent_digest: canonical_command_digest(proof.intent)?,
        generation_digest: proof.intent.generation_digest()?,
        ack_task_digest: canonical_command_digest(&actual.task)?,
        ack_task: actual.task,
        revision: 1,
        progress: TaskStopProgress::initial(proof.intent),
        result_nonce: None,
        updated_at: DateTime::parse_from_rfc3339(&super::migrations::timestamp(
            proof.prepared.prepared_at(),
        ))?
        .with_timezone(&Utc),
    };
    tx.execute("INSERT INTO task_stop_effects(command_nonce,agent_id,identity_incarnation,task_id,task_created_at,generation_digest,intent_digest,prepared_json,intent_json,revision,physical_state,child_state,worktree_state,result_state,result_nonce,progress_json,created_at,updated_at,ack_task_json,ack_task_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?11,?12,?13,NULL,?14,?15,?15,?16,?17)",
        params![record.prepared.command_nonce(),record.intent.task.scope.agent_id,record.intent.task.scope.identity_incarnation,
            record.intent.task.key.id,super::migrations::timestamp(record.intent.created_at),record.generation_digest,record.intent_digest,
            serde_json::to_string(&record.prepared)?,serde_json::to_string(&record.intent)?,
            state_name(record.progress.physical)?,state_name(record.progress.child)?,state_name(record.progress.worktree)?,state_name(record.progress.result)?,
            serde_json::to_string(&record.progress)?,super::migrations::timestamp(record.updated_at),serde_json::to_string(&record.ack_task)?,record.ack_task_digest])?;
    record.effect_seq = u64::try_from(tx.last_insert_rowid())?;
    record.validate()?;
    let ack_material = record
        .resident_bytes()?
        .checked_mul(8)
        .context("Stop INSERT ACK budget overflow")?;
    ensure!(
        ack_material <= u64::try_from(proof.request.byte_limit.min(MAX_BYTES))?,
        "Stop INSERT ACK ResourceExhausted"
    );
    let budget = proof.ledger.register_owned_bytes(
        &proof.intent.task.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: ack_material,
            metadata_estimate_bytes: 0,
        },
    )?;
    let mut ack = TaskStopInsertedObservation {
        tx: proof.tx,
        prepared: proof.prepared,
        intent: proof.intent,
        effect_seq: record.effect_seq,
        record_digest: canonical_command_digest(&record)?,
        intent_digest: record.intent_digest.clone(),
        generation_digest: record.generation_digest.clone(),
        ack_task_digest: record.ack_task_digest.clone(),
        _budget: budget.clone(),
        _readback_budget: budget,
    };
    // This private local cap is not returned until the actual inserted row has
    // passed the same normalized decoder and its exact expected digest.
    let mut observed = observe_task_stops_tx(
        tx,
        &proof.intent.task.scope,
        Some(record.effect_seq),
        false,
        |bytes, count| {
            let bytes = bytes
                .checked_mul(8)
                .and_then(|n| n.checked_add(512))
                .context("Stop inserted row material overflow")?;
            ensure!(
                count == 1 && bytes <= proof.request.byte_limit.min(MAX_BYTES),
                "Stop inserted row ResourceExhausted before decode"
            );
            proof
                .ledger
                .register_owned_bytes(
                    &proof.intent.task.scope,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: u64::try_from(bytes)?,
                        metadata_estimate_bytes: 0,
                    },
                )
                .map_err(Into::into)
        },
        Some(&ack),
    )?;
    let actual = observed
        .records
        .pop()
        .context("actual inserted Stop missing")?;
    let actual = Arc::try_unwrap(actual)
        .map_err(|_| anyhow::anyhow!("inserted Stop ACK unexpectedly shared"))?;
    ack._readback_budget = observed._budget;
    Ok((actual, ack))
}
fn state_name(value: impl Serialize) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .context("TaskStop state is not a string")
}

/// Carries actual Candidate observation until current-root installation or Drop.
#[derive(Debug)]
pub(crate) struct PreparedTaskStops {
    pub records: Vec<Arc<TaskStopRecord>>,
    _budget: Arc<BudgetViewGuard>,
}

/// This inventory is by original scope and independent of active Task/Queue.
/// latest terminal heads are a separate exact per-selected-Task request.
const STOP_PROJECTION: &str = "json_array(effect_seq,command_nonce,agent_id,identity_incarnation,task_id,task_created_at,generation_digest,intent_digest,revision,physical_state,child_state,worktree_state,result_state,result_nonce,created_at,updated_at,ack_task_digest)";
const STOP_RECEIPT_BYTES: &str = "COALESCE((SELECT length(CAST(json_array(receipt.family,receipt.phase_version,receipt.command_digest,receipt.scope_digest,receipt.scopes_json,receipt.prepared_at,receipt.prepared_json,receipt.outcome_json,receipt.committed_at,receipt.result_digest) AS BLOB)) FROM input_mutation_receipts receipt WHERE receipt.command_nonce=task_stop_effects.command_nonce),0)";
const STOP_PENDING: &str = "(physical_state='pending' OR child_state='pending' OR worktree_state='pending' OR result_state='pending')";

// Called only after raw Stop/initial-receipt material has a real owner guard.
// In particular intent_json must never be parsed in the first lengths gate.
fn result_receipt_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected_seq: Option<u64>,
    pending_only: bool,
) -> Result<(usize, usize)> {
    tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(json_array(receipt.family,receipt.phase_version,receipt.command_digest,receipt.scope_digest,receipt.scopes_json,receipt.prepared_at,receipt.prepared_json,receipt.outcome_json,receipt.committed_at,receipt.result_digest) AS BLOB))),0) FROM task_stop_effects JOIN input_mutation_receipts receipt ON receipt.command_nonce=json_extract(task_stop_effects.intent_json,'$.result.phase_nonce') WHERE task_stop_effects.agent_id=?1 AND identity_incarnation=?2 AND (?3 IS NULL OR effect_seq=?3) AND (?4=0 OR {STOP_PENDING})"),
        params![scope.agent_id,scope.identity_incarnation,selected_seq,pending_only],|r|Ok((r.get(0)?,r.get(1)?))).map_err(Into::into)
}

pub(crate) fn load_pending_task_stops_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    record_limit: usize,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskStops> {
    load_task_stops_selection_tx(tx, scope, None, true, record_limit, byte_limit, ledger)
}

/// Exact original address, including terminal progress; never a latest-by-id read.
pub(crate) fn load_task_stop_by_sequence_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    effect_seq: u64,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskStops> {
    load_task_stops_selection_tx(tx, scope, Some(effect_seq), false, 1, byte_limit, ledger)
}

fn load_task_stops_selection_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected_seq: Option<u64>,
    pending_only: bool,
    record_limit: usize,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskStops> {
    let (count, bytes): (usize, usize) = tx.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(prepared_json AS BLOB))+length(CAST(intent_json AS BLOB))+length(CAST(progress_json AS BLOB))+length(CAST(ack_task_json AS BLOB))+length(CAST({STOP_PROJECTION} AS BLOB))+{STOP_RECEIPT_BYTES}),0) FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND (?3 IS NULL OR effect_seq=?3) AND (?4=0 OR {STOP_PENDING})"),
        params![scope.agent_id,scope.identity_incarnation,selected_seq,pending_only],|r|Ok((r.get(0)?,r.get(1)?)),
    )?;
    let material = bytes
        .checked_mul(8)
        .and_then(|n| count.checked_mul(512).and_then(|m| n.checked_add(m)))
        .context("TaskStop material overflow")?;
    ensure!(
        count <= record_limit.min(MAX_RECORDS) && material <= byte_limit.min(MAX_BYTES),
        "TaskStop ResourceExhausted before decode"
    );
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(material)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let (receipt_count, receipt_bytes) =
        result_receipt_material_tx(tx, scope, selected_seq, pending_only)?;
    let receipt_material = receipt_bytes
        .checked_mul(8)
        .and_then(|n| {
            receipt_count
                .checked_mul(512)
                .and_then(|m| n.checked_add(m))
        })
        .context("TaskStop Result receipt material overflow")?;
    ensure!(
        receipt_count <= count
            && material
                .checked_add(receipt_material)
                .is_some_and(|n| n <= byte_limit.min(MAX_BYTES)),
        "TaskStop ResourceExhausted before Result receipt decode"
    );
    // These decoded receipt values do not escape row validation. The original
    // Stop body retains its separate guard in PreparedTaskStops after return.
    let _receipt_guard = if receipt_count > 0 {
        Some(ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(receipt_material)?,
                metadata_estimate_bytes: 0,
            },
        )?)
    } else {
        None
    };
    load_task_stop_rows_precharged_tx(
        tx,
        scope,
        selected_seq,
        pending_only,
        count,
        guard,
        None,
        None,
    )
}

/// The charge callback must reserve the scalar estimate through the actual owner
/// ledger and combined allowance before the shared decoder allocates any body.
pub(in crate::runtime_db) fn observe_task_stops_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected_seq: Option<u64>,
    pending_only: bool,
    charge: impl FnMut(usize, usize) -> Result<Arc<BudgetViewGuard>>,
    inserted: Option<&TaskStopInsertedObservation<'_, '_>>,
) -> Result<PreparedTaskStops> {
    observe_task_stops_with_result_tx(
        tx,
        scope,
        selected_seq,
        pending_only,
        charge,
        inserted,
        None,
    )
}
pub(in crate::runtime_db) fn observe_task_stops_with_result_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected_seq: Option<u64>,
    pending_only: bool,
    mut charge: impl FnMut(usize, usize) -> Result<Arc<BudgetViewGuard>>,
    inserted: Option<&TaskStopInsertedObservation<'_, '_>>,
    result: Option<&TaskStopResultObservation<'_, '_>>,
) -> Result<PreparedTaskStops> {
    if let Some(ack) = inserted {
        ack.validate_presence_tx(tx)?;
    }
    let (count,bytes):(usize,usize)=tx.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(prepared_json AS BLOB))+length(CAST(intent_json AS BLOB))+length(CAST(progress_json AS BLOB))+length(CAST(ack_task_json AS BLOB))+length(CAST({STOP_PROJECTION} AS BLOB))+{STOP_RECEIPT_BYTES}),0) FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND (?3 IS NULL OR effect_seq=?3) AND (?4=0 OR {STOP_PENDING})"),
        params![scope.agent_id,scope.identity_incarnation,selected_seq,pending_only],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let guard = charge(bytes, count)?;
    let (receipt_count, receipt_bytes) =
        result_receipt_material_tx(tx, scope, selected_seq, pending_only)?;
    let _receipt_guard = if receipt_count > 0 {
        Some(charge(receipt_bytes, receipt_count)?)
    } else {
        None
    };
    load_task_stop_rows_precharged_tx(
        tx,
        scope,
        selected_seq,
        pending_only,
        count,
        guard,
        inserted,
        result,
    )
}
fn load_task_stop_rows_precharged_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selected_seq: Option<u64>,
    pending_only: bool,
    count: usize,
    guard: Arc<BudgetViewGuard>,
    inserted: Option<&TaskStopInsertedObservation<'_, '_>>,
    result: Option<&TaskStopResultObservation<'_, '_>>,
) -> Result<PreparedTaskStops> {
    let mut statement = tx.prepare(&format!("SELECT effect_seq,prepared_json,intent_json,intent_digest,generation_digest,revision,progress_json,result_nonce,updated_at,{STOP_PROJECTION},ack_task_json,ack_task_digest FROM task_stop_effects WHERE agent_id=?1 AND identity_incarnation=?2 AND (?3 IS NULL OR effect_seq=?3) AND (?4=0 OR {STOP_PENDING}) ORDER BY effect_seq LIMIT ?5"))?;
    let raw = statement.query_map(
        params![
            scope.agent_id,
            scope.identity_incarnation,
            selected_seq,
            pending_only,
            i64::try_from(count)?
        ],
        |r| {
            Ok((
                r.get::<_, u64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, u64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
            ))
        },
    )?;
    let mut records = Vec::with_capacity(count);
    for row in raw {
        let (
            effect_seq,
            prepared,
            intent,
            intent_digest,
            generation_digest,
            revision,
            progress,
            result_nonce,
            updated_at,
            projection,
            ack_task_json,
            ack_task_digest,
        ) = row?;
        let record = TaskStopRecord {
            effect_seq,
            prepared: serde_json::from_str(&prepared)?,
            intent: serde_json::from_str(&intent)?,
            ack_task: serde_json::from_str(&ack_task_json)?,
            ack_task_digest,
            intent_digest,
            generation_digest,
            revision,
            progress: serde_json::from_str(&progress)?,
            result_nonce,
            updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
        };
        record.validate()?;
        let expected = serde_json::json!([
            record.effect_seq,
            record.prepared.command_nonce(),
            record.intent.task.scope.agent_id,
            record.intent.task.scope.identity_incarnation,
            record.intent.task.key.id,
            super::migrations::timestamp(record.intent.created_at),
            record.generation_digest,
            record.intent_digest,
            record.revision,
            record.progress.physical,
            record.progress.child,
            record.progress.worktree,
            record.progress.result,
            record.result_nonce,
            super::migrations::timestamp(record.prepared.prepared_at()),
            super::migrations::timestamp(record.updated_at),
            record.ack_task_digest
        ]);
        ensure!(
            serde_json::from_str::<serde_json::Value>(&projection)? == expected,
            "TaskStop normalized/payload projection differs"
        );
        ensure!(
            record.intent.task.scope == *scope
                && selected_seq.is_none_or(|seq| seq == record.effect_seq)
                && (!pending_only || record.progress.is_pending()),
            "TaskStop inventory projection differs"
        );
        if !inserted
            .map(|ack| ack.validates(tx, &record))
            .transpose()?
            .unwrap_or(false)
        {
            validate_task_stop_receipt_precharged_tx(tx, &record)?;
        }
        if !result
            .map(|cap| cap.permits(tx, &record))
            .transpose()?
            .unwrap_or(false)
        {
            continuation::validate_stored_result_receipt_tx(tx, &record)?;
        }
        records.push(Arc::new(record));
    }
    ensure!(
        records.len() == count,
        "TaskStop pending inventory changed in read view"
    );
    Ok(PreparedTaskStops {
        records,
        _budget: guard,
    })
}

/// Original receipt validation never grants current Task/child write authority.
/// Caller holds the original effect plan and actual owner reservation; body and
/// receipt material must already have an explicit bounded Candidate guard.
#[cfg(test)]
pub(crate) fn validate_original_task_stop_receipt_tx(
    tx: &Transaction<'_>,
    record: &TaskStopRecord,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Arc<BudgetViewGuard>> {
    let bytes: usize = tx.query_row("SELECT length(CAST(json_array(family,phase_version,command_digest,scope_digest,scopes_json,prepared_at,prepared_json,outcome_json,committed_at,result_digest) AS BLOB)) FROM input_mutation_receipts WHERE command_nonce=?1",
        [record.prepared.command_nonce()],|row|row.get(0))?;
    let material = u64::try_from(bytes)?
        .checked_add(serialized_bytes(record)?)
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(512))
        .context("TaskStop receipt material overflow")?;
    ensure!(
        material <= u64::try_from(byte_limit.min(MAX_BYTES))?,
        "TaskStop receipt ResourceExhausted before decode"
    );
    let guard = ledger.register_owned_bytes(
        &record.intent.task.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(material)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    record.validate()?;
    validate_task_stop_receipt_precharged_tx(tx, record)?;
    Ok(guard)
}

fn validate_task_stop_receipt_precharged_tx(
    tx: &Transaction<'_>,
    record: &TaskStopRecord,
) -> Result<()> {
    let receipt = lookup_input_mutation_receipt_tx(tx, &record.prepared)?
        .context("TaskStop original whole receipt missing")?;
    ensure!(
        matches!(&receipt.outcome.result,InputMutationResult::Task { task_id, stop:Some(binding),.. } if task_id == &record.intent.task.key.id && binding == &record.receipt_binding()),
        "TaskStop original receipt has another result"
    );
    Ok(())
}

/// First ACK checks exact allocated facet columns without decoding mutable data.
pub(crate) fn validate_task_stop_ack_binding_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputMutation,
    task_id: &str,
    binding: &TaskStopReceiptBinding,
) -> Result<()> {
    ensure!(
        prepared.family() == InputMutationFamily::Task
            && prepared.scopes().contains(&binding.scope),
        "TaskStop receipt scope differs"
    );
    let count:usize=tx.query_row("SELECT COUNT(*) FROM task_stop_effects WHERE command_nonce=?1 AND agent_id=?2 AND identity_incarnation=?3 AND task_id=?4 AND effect_seq=?5 AND intent_digest=?6 AND generation_digest=?7 AND ack_task_digest=?8",
        params![prepared.command_nonce(),binding.scope.agent_id,binding.scope.identity_incarnation,task_id,binding.effect_seq,binding.intent_digest,binding.generation_digest,binding.task_ack_digest],|r|r.get(0))?;
    ensure!(
        count == 1,
        "TaskStop receipt has no exact original facet ACK"
    );
    Ok(())
}

#[cfg(test)]
pub(crate) struct PreparedTaskStopProgress {
    pub record: Arc<TaskStopRecord>,
    pub changed: bool,
    _budget: Arc<BudgetViewGuard>,
}

/// Only focused progress changes here. The enclosing original owner transaction
/// must finalize the Task domain once and publish its final revision after ACK.
/// This proof grants no canonical Task, child/home or Result admission write.
#[cfg(test)]
pub(crate) fn mark_task_stop_monitor_joined_tx(
    tx: &Transaction<'_>,
    expected: &TaskStopRecord,
    joined: &crate::runtime::task_stop_join::TaskStopMonitorJoin,
    at: DateTime<Utc>,
    byte_limit: usize,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskStopProgress> {
    ensure!(
        joined.matches(expected),
        "TaskStop join proof differs from original effect"
    );
    let observed = load_task_stop_by_sequence_tx(
        tx,
        &expected.intent.task.scope,
        expected.effect_seq,
        byte_limit,
        ledger,
    )?;
    let current = observed
        .records
        .first()
        .context("TaskStop original progress missing")?;
    ensure!(
        joined.matches(current)
            && current.prepared == expected.prepared
            && current.ack_task_digest == expected.ack_task_digest,
        "TaskStop original progress identity changed"
    );
    if current.progress.physical == TaskStopPhysicalState::MonitorJoined {
        return Ok(PreparedTaskStopProgress {
            record: current.clone(),
            changed: false,
            _budget: observed._budget.clone(),
        });
    }
    ensure!(
        current.revision == expected.revision && current.progress == expected.progress,
        "TaskStop progress revision conflict"
    );
    ensure!(
        current.progress.physical == TaskStopPhysicalState::Pending,
        "TaskStop physical progress has no pending original monitor"
    );
    ensure!(
        at >= current.updated_at,
        "TaskStop progress clock moved backwards"
    );
    let mut next = current.as_ref().clone();
    next.progress.physical = TaskStopPhysicalState::MonitorJoined;
    current
        .progress
        .validate_successor(&next.progress, &current.intent)?;
    next.revision = next
        .revision
        .checked_add(1)
        .context("TaskStop progress revision overflow")?;
    next.updated_at =
        DateTime::parse_from_rfc3339(&super::migrations::timestamp(at))?.with_timezone(&Utc);
    next.validate()?;
    let changed = tx.execute(
        "UPDATE task_stop_effects SET revision=?1,physical_state='monitor_joined',progress_json=?2,updated_at=?3 WHERE effect_seq=?4 AND agent_id=?5 AND identity_incarnation=?6 AND command_nonce=?7 AND intent_digest=?8 AND revision=?9 AND physical_state='pending'",
        params![next.revision,serde_json::to_string(&next.progress)?,super::migrations::timestamp(at),
            next.effect_seq,next.intent.task.scope.agent_id,next.intent.task.scope.identity_incarnation,
            next.prepared.command_nonce(),next.intent_digest,current.revision],
    )?;
    ensure!(changed == 1, "TaskStop progress revision conflict");
    Ok(PreparedTaskStopProgress {
        record: Arc::new(next),
        changed: true,
        _budget: observed._budget.clone(),
    })
}

#[cfg(test)]
mod tests;
