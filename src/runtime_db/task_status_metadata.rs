//! Original Control completion metadata, separate from physical cleanup authority.
mod recovery;
pub(crate) use recovery::{TaskStatusMetadataReadView, TaskStatusMetadataSparseRecovery};

use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        },
        resident_input_evidence::InputEvidenceScope,
        resident_task::{QualifiedTaskFact, TaskFamily},
    },
    runtime_db::{
        control_transition::{ControlCancellationKind, ControlCancellationTarget, ControlKey},
        input_mutation_receipts::{InputMutationFamily, PreparedInputMutation},
        task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError},
    },
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStatusMetadataInstallMode {
    Current,
    NoState,
    Retired,
}
impl From<super::command_task_terminal::CommandTaskTerminalInstallMode>
    for TaskStatusMetadataInstallMode
{
    fn from(value: super::command_task_terminal::CommandTaskTerminalInstallMode) -> Self {
        use super::command_task_terminal::CommandTaskTerminalInstallMode as Mode;
        match value {
            Mode::Current => Self::Current,
            Mode::NoState => Self::NoState,
            Mode::Retired => Self::Retired,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStatusMetadataAck {
    pub key: ControlKey,
    pub obligation_id: String,
    pub target_digest: String,
    pub source_changed: bool,
    pub install_mode: TaskStatusMetadataInstallMode,
    pub task_revision: Option<u64>,
}
impl TaskStatusMetadataAck {
    pub(crate) fn validate(&self, prepared: &PreparedInputMutation) -> Result<()> {
        ensure!(
            prepared.family() == InputMutationFamily::Task
                && prepared.scopes().len() == 1
                && prepared.scopes()[0].agent_id == self.key.agent_id
                && prepared.scopes()[0].identity_incarnation == self.key.identity_incarnation
                && !self.key.command_id.is_empty()
                && !self.obligation_id.is_empty()
                && self.target_digest.len() == 64
                && self.target_digest.bytes().all(|b| b.is_ascii_hexdigit())
                && self.task_revision.is_none_or(|revision| revision > 0)
                && (self.install_mode == TaskStatusMetadataInstallMode::Current
                    || self.task_revision.is_none()),
            "TaskStatus metadata ACK differs from its original scope"
        );
        Ok(())
    }
}

/// This is a materialization selection, not source or business write authority.
#[derive(Debug)]
pub(crate) struct TaskStatusMetadataSelection {
    pub(in crate::runtime_db) revision: u64,
    pub(in crate::runtime_db) records: Vec<Arc<QualifiedTaskFact>>,
    _guards: Vec<Arc<BudgetViewGuard>>,
    pub(in crate::runtime_db) bytes: usize,
    pub(in crate::runtime_db) records_count: usize,
}
impl TaskStatusMetadataSelection {
    pub(crate) fn single(
        scope: &InputEvidenceScope,
        revision: u64,
        record: &Arc<QualifiedTaskFact>,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        ensure!(
            revision > 0
                && &record.locator.scope == scope
                && record.locator.key.family == TaskFamily::Task,
            "TaskStatus metadata selection crossed scope/family"
        );
        let bytes = std::mem::size_of::<Arc<QualifiedTaskFact>>()
            .checked_mul(8)
            .context("TaskStatus selected slots overflow")?;
        let guard =
            super::command_task_terminal::charge_material(scope, bytes, 8, request, ledger)?;
        let body = ledger.register_task_view(
            scope,
            InputMemoryCategory::Candidate,
            std::iter::once(record),
        )?;
        Ok(Self {
            revision,
            records: vec![record.clone()],
            _guards: vec![guard, body],
            bytes: bytes
                .checked_add(record.resident_bytes)
                .context("selected Task bytes overflow")?,
            records_count: 8,
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct FrozenControlWorktreeCompletion {
    pub(in crate::runtime_db) scope: InputEvidenceScope,
    pub(in crate::runtime_db) key: ControlKey,
    pub(in crate::runtime_db) target: ControlCancellationTarget,
    pub(in crate::runtime_db) evidence: Value,
    pub(in crate::runtime_db) at: DateTime<Utc>,
    pub(in crate::runtime_db) audits: Vec<crate::types::AuditEvent>,
    pub(in crate::runtime_db) completed: bool,
    #[serde(skip)]
    pub(in crate::runtime_db) guards: Vec<Arc<BudgetViewGuard>>,
    #[serde(skip)]
    pub(in crate::runtime_db) completion:
        Arc<crate::runtime::input_evidence_budget::OwnedCompletionBudget>,
    #[serde(skip)]
    pub(in crate::runtime_db) bytes: usize,
    #[serde(skip)]
    pub(in crate::runtime_db) records: usize,
}
impl FrozenControlWorktreeCompletion {
    #[cfg(test)]
    pub(crate) fn freeze(
        key: &ControlKey,
        target: &ControlCancellationTarget,
        evidence: &Value,
        at: DateTime<Utc>,
        request: &TaskDomainLoadRequest,
        completion: Arc<crate::runtime::input_evidence_budget::OwnedCompletionBudget>,
    ) -> Result<Self> {
        Self::freeze_with_audits(key, target, evidence, &[], true, at, request, completion)
    }
    pub(crate) fn freeze_with_audits(
        key: &ControlKey,
        target: &ControlCancellationTarget,
        evidence: &Value,
        audits: &[crate::types::AuditEvent],
        completed: bool,
        at: DateTime<Utc>,
        request: &TaskDomainLoadRequest,
        completion: Arc<crate::runtime::input_evidence_budget::OwnedCompletionBudget>,
    ) -> Result<Self> {
        ensure!(
            key.identity_incarnation > 0
                && !key.agent_id.is_empty()
                && !key.command_id.is_empty()
                && target.target_kind == ControlCancellationKind::Worktree
                && !target.obligation_id.is_empty()
                && !target.target_identity.is_empty()
                && !evidence.is_null(),
            "invalid original Control worktree completion"
        );
        let bytes = usize::try_from(serialized_bytes(&(
            key, target, evidence, audits, completed, at,
        ))?)?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .context("Control completion frozen material overflow")?;
        let _ = request;
        // Physical completion has already happened. Retain its actual evidence
        // even when a smaller SQL phase allowance must reject and retry it.
        // This same registered nonce was acquired before the physical kernel.
        // Its growth cannot drop a completed outcome due to a new ledger check.
        let records = audits
            .iter()
            .try_fold(8usize, |n, event| {
                n.checked_add(value_nodes(&event.data)?)
                    .context("Control audit node overflow")
            })?
            .checked_add(value_nodes(evidence)?)
            .and_then(|n| n.checked_mul(8))
            .context("Control completion node overflow")?;
        let node_bytes = records
            .checked_mul(128)
            .context("Control completion node bytes overflow")?;
        let bytes = bytes
            .checked_add(node_bytes)
            .context("Control completion combined bytes overflow")?;
        completion.observe_material(u64::try_from(bytes)?, 0);
        let scope = InputEvidenceScope {
            agent_id: key.agent_id.clone(),
            identity_incarnation: key.identity_incarnation,
        };
        ensure!(
            completion.matches_scope(&scope),
            "Control completion budget belongs to another original scope"
        );
        Ok(Self {
            scope,
            key: key.clone(),
            target: target.clone(),
            evidence: evidence.clone(),
            at,
            audits: audits.to_vec(),
            completed,
            guards: vec![completion.guard()],
            completion,
            bytes,
            records,
        })
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(crate) fn task_id(&self) -> &str {
        &self.target.target_identity
    }
}

/// The original decoder reads this finite key's complete obligation pool.
/// All raw/projection strings and cloned ControlKey addresses are admitted first.
pub(in crate::runtime_db) fn control_completion_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &ControlKey,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(Vec<Arc<BudgetViewGuard>>, usize, usize)> {
    ensure!(
        scope.agent_id == key.agent_id && scope.identity_incarnation == key.identity_incarnation,
        "Control metadata source scope differs"
    );
    let (bytes,records):(usize,usize)=tx.query_row("SELECT COALESCE(SUM(length(CAST(agent_id AS BLOB))+length(CAST(command_id AS BLOB))+length(CAST(obligation_id AS BLOB))+length(CAST(target_kind AS BLOB))+length(CAST(target_identity AS BLOB))+length(CAST(target_generation_json AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(state AS BLOB))+length(CAST(updated_at AS BLOB))+length(CAST(target_json AS BLOB))+COALESCE(length(CAST(completion_evidence_json AS BLOB)),0)+256),0),COUNT(*) FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3",params![key.agent_id,key.identity_incarnation,key.command_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    let bytes = bytes
        .checked_add(256)
        .and_then(|n| n.checked_mul(8))
        .context("Control original pool bytes overflow")?;
    let records = records
        .max(1)
        .checked_mul(8)
        .context("Control original pool count overflow")?;
    let raw =
        super::command_task_terminal::charge_material(scope, bytes, records, request, ledger)?;
    let nodes:usize=tx.query_row("SELECT COALESCE(SUM((SELECT COUNT(*) FROM json_tree(target_generation_json))+(SELECT COUNT(*) FROM json_tree(target_json))+CASE WHEN completion_evidence_json IS NULL THEN 0 ELSE (SELECT COUNT(*) FROM json_tree(completion_evidence_json)) END),0) FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3",params![key.agent_id,key.identity_incarnation,key.command_id],|r|r.get(0))?;
    let node_records = nodes
        .checked_mul(8)
        .context("Control original pool nodes overflow")?;
    let node_bytes = node_records
        .checked_mul(128)
        .context("Control original pool node bytes overflow")?;
    let mut phase = PhaseMaterial::new(request);
    phase.include(bytes, records)?;
    let nodes = super::command_task_terminal::charge_material(
        scope,
        node_bytes,
        node_records,
        &phase.remaining()?,
        ledger,
    )?;
    Ok((
        vec![raw, nodes],
        bytes
            .checked_add(node_bytes)
            .context("Control original pool total overflow")?,
        records
            .checked_add(node_records)
            .context("Control original pool total records overflow")?,
    ))
}

/// One phase allowance; guards remain with the actual ACK, never a second quota.
pub(in crate::runtime_db) struct PhaseMaterial<'a> {
    request: &'a TaskDomainLoadRequest,
    bytes: usize,
    records: usize,
}
impl<'a> PhaseMaterial<'a> {
    pub(in crate::runtime_db) fn new(request: &'a TaskDomainLoadRequest) -> Self {
        Self {
            request,
            bytes: 0,
            records: 0,
        }
    }
    pub(in crate::runtime_db) fn include(&mut self, bytes: usize, records: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("Status phase bytes overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("Status phase records overflow")?;
        ensure!(
            self.bytes <= self.request.byte_limit.min(64 * 1024 * 1024)
                && self.records <= self.request.record_limit.min(65536),
            TaskSnapshotError::ResourceExhausted("Status phase combined material")
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn remaining(&self) -> Result<TaskDomainLoadRequest> {
        let mut request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            byte_limit: 0,
            record_limit: 0,
        };
        request.byte_limit = self
            .request
            .byte_limit
            .min(64 * 1024 * 1024)
            .checked_sub(self.bytes)
            .context("Status phase bytes exhausted")?;
        request.record_limit = self
            .request
            .record_limit
            .min(65536)
            .checked_sub(self.records)
            .context("Status phase records exhausted")?;
        Ok(request)
    }
    pub(in crate::runtime_db) fn owner_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<super::command_task_terminal::CommandTaskOwnerObservation> {
        let n:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
        let bytes = n
            .checked_add(scope.agent_id.len() + 256)
            .and_then(|n| n.checked_mul(8))
            .context("Status owner bytes overflow")?;
        let owner = super::command_task_terminal::observe_original_owner_tx(
            tx,
            scope,
            &self.remaining()?,
            ledger,
        )?;
        self.include(bytes, 8)?;
        Ok(owner)
    }
}

/// Exact original target state, without decoding a current obligation pool.
pub(crate) fn original_target_completed_tx(
    tx: &Transaction<'_>,
    key: &ControlKey,
    target: &ControlCancellationTarget,
) -> Result<bool> {
    let encoded = serde_json::to_string(target)?;
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3 AND obligation_id=?4 AND state='completed' AND target_json=?5)",params![key.agent_id,key.identity_incarnation,key.command_id,target.obligation_id,encoded],|r|r.get(0))?)
}

fn value_nodes(value: &Value) -> Result<usize> {
    match value {
        Value::Array(values) => values.iter().try_fold(1usize, |n, v| {
            n.checked_add(value_nodes(v)?)
                .context("Control completion nodes overflow")
        }),
        Value::Object(values) => values.values().try_fold(1usize, |n, v| {
            n.checked_add(value_nodes(v)?)
                .context("Control completion nodes overflow")
        }),
        _ => Ok(1),
    }
}
/// Original append_event duplicate read and its native node bound share this phase.
pub(in crate::runtime_db) fn audit_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    audits: &[crate::types::AuditEvent],
    material: &mut PhaseMaterial<'_>,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Vec<Arc<BudgetViewGuard>>> {
    let mut guards = Vec::new();
    for event in audits {
        let raw:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(data_json AS BLOB))+COALESCE(length(CAST(agent_id AS BLOB)),0) FROM audit_events WHERE audit_event_id=?1),0)+COALESCE((SELECT length(CAST(value AS BLOB)) FROM runtime_metadata WHERE key='event_log_epoch'),0)+256",[&event.id],|r|r.get(0))?;
        let bytes = raw.checked_mul(8).context("Control audit bytes overflow")?;
        guards.push(super::command_task_terminal::charge_material(
            scope,
            bytes,
            8,
            &material.remaining()?,
            ledger,
        )?);
        material.include(bytes, 8)?;
        let nodes:usize=tx.query_row("SELECT COALESCE((SELECT COUNT(*) FROM audit_events a,json_tree(a.data_json) WHERE a.audit_event_id=?1),0)",[&event.id],|r|r.get(0))?;
        let records = nodes
            .checked_mul(8)
            .context("Control existing audit nodes overflow")?;
        let bytes = records
            .checked_mul(128)
            .context("Control existing audit nodes bytes overflow")?;
        guards.push(super::command_task_terminal::charge_material(
            scope,
            bytes,
            records,
            &material.remaining()?,
            ledger,
        )?);
        material.include(bytes, records)?;
    }
    Ok(guards)
}
