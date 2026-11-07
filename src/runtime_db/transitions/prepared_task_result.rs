//! Frozen standalone Result operations reuse the original owner/whole receipt.
use super::*;
use crate::runtime_db::task_result_settlement as results;

#[derive(Debug, serde::Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum TaskResultOperation {
    EnsurePending {
        task_id: String,
        message: Box<MessageEnvelope>,
        at: chrono::DateTime<Utc>,
    },
    #[cfg(test)]
    AdmitUnsettled {
        work_item_id: Option<String>,
        activation_id: String,
        at: chrono::DateTime<Utc>,
    },
    #[cfg(test)]
    AdmitReply {
        work_item_id: Option<String>,
        message_id: String,
        activation_id: String,
        at: chrono::DateTime<Utc>,
    },
    /// Fixed byte-budgeted identities; each source retains its original transaction.
    AdmitSnapshot {
        work_item_id: Option<String>,
        activation_id: String,
        anchor_message_id: Option<String>,
        reply_message_id: Option<String>,
        selected: std::collections::BTreeSet<String>,
        projections: Option<std::collections::BTreeMap<String, crate::types::IdleInputBatchItem>>,
        at: chrono::DateTime<Utc>,
    },
    AdmitMessage {
        message_id: String,
        activation_id: String,
        projection: Option<crate::types::IdleInputBatchItem>,
        at: chrono::DateTime<Utc>,
    },
    /// A recovery snapshot is advisory; the original transaction rechecks Invalid.
    SettleInvalidOnly {
        message_id: String,
        at: chrono::DateTime<Utc>,
    },
    SettleOwnerUnavailable {
        message_id: String,
        disposition: results::TaskResultSettlementDisposition,
        at: chrono::DateTime<Utc>,
    },
    Defer {
        message_id: String,
        reason: String,
        at: chrono::DateTime<Utc>,
        deadline: chrono::DateTime<Utc>,
    },
    MarkRecoveryDiagnostic {
        result_identity: String,
        epoch: u64,
        now: chrono::DateTime<Utc>,
    },
    Wake {
        work_item_id: String,
        at: chrono::DateTime<Utc>,
    },
}
#[derive(Debug, serde::Serialize)]
pub(crate) struct PreparedTaskResultTransition {
    agent_id: String,
    pub(super) operation: TaskResultOperation,
}
impl PreparedTaskResultTransition {
    pub(crate) fn new(agent_id: String, operation: TaskResultOperation) -> Result<Self> {
        let value = Self {
            agent_id,
            operation,
        };
        value.validate()?;
        Ok(value)
    }
    pub(super) fn agent_id(&self) -> &str {
        &self.agent_id
    }
    pub(super) fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.agent_id.is_empty(), "empty TaskResult namespace");
        let required = |id: &str| -> Result<()> {
            anyhow::ensure!(!id.is_empty(), "empty original TaskResult address");
            Ok(())
        };
        match &self.operation {
            TaskResultOperation::EnsurePending {
                task_id, message, ..
            } => {
                required(task_id)?;
                required(&message.id)?;
                anyhow::ensure!(
                    message.agent_id == self.agent_id,
                    "TaskResult frozen source namespace differs"
                );
            }
            #[cfg(test)]
            TaskResultOperation::AdmitUnsettled { activation_id, .. } => required(activation_id)?,
            TaskResultOperation::AdmitSnapshot {
                activation_id,
                selected,
                ..
            } => {
                required(activation_id)?;
                anyhow::ensure!(selected.len() <= 64, "Result snapshot exceeds fixed scan");
                for id in selected {
                    required(id)?;
                }
            }
            #[cfg(test)]
            TaskResultOperation::AdmitReply {
                message_id,
                activation_id,
                ..
            } => {
                required(message_id)?;
                required(activation_id)?;
            }
            TaskResultOperation::AdmitMessage {
                message_id,
                activation_id,
                ..
            } => {
                required(message_id)?;
                required(activation_id)?;
            }
            TaskResultOperation::SettleInvalidOnly { message_id, .. }
            | TaskResultOperation::SettleOwnerUnavailable { message_id, .. }
            | TaskResultOperation::Defer { message_id, .. } => required(message_id)?,
            TaskResultOperation::MarkRecoveryDiagnostic {
                result_identity, ..
            } => required(result_identity)?,
            TaskResultOperation::Wake { work_item_id, .. } => required(work_item_id)?,
        }
        Ok(())
    }
    pub(super) fn selection_tx<'tx, 'connection>(
        &'tx self,
        tx: &'tx Transaction<'connection>,
        limits: results::TaskResultWriteLimits,
    ) -> Result<results::TaskResultSelection<'tx, 'connection>> {
        let selection = match &self.operation {
            TaskResultOperation::AdmitSnapshot {
                work_item_id,
                activation_id,
                anchor_message_id,
                reply_message_id,
                selected,
                at,
                ..
            } => match reply_message_id {
                Some(message_id) => results::select_admit_reply_snapshot_tx(
                    tx,
                    &self.agent_id,
                    work_item_id.as_deref(),
                    message_id,
                    activation_id,
                    Some(selected),
                    *at,
                    limits,
                ),
                None => results::select_admit_unsettled_snapshot_tx(
                    tx,
                    &self.agent_id,
                    work_item_id.as_deref(),
                    activation_id,
                    anchor_message_id.as_deref(),
                    Some(selected),
                    *at,
                    limits,
                ),
            },
            #[cfg(test)]
            TaskResultOperation::AdmitUnsettled {
                work_item_id,
                activation_id,
                at,
            } => results::select_admit_unsettled_tx(
                tx,
                &self.agent_id,
                work_item_id.as_deref(),
                activation_id,
                *at,
                limits,
            ),
            #[cfg(test)]
            TaskResultOperation::AdmitReply {
                work_item_id,
                message_id,
                activation_id,
                at,
            } => results::select_admit_reply_message_tx(
                tx,
                &self.agent_id,
                work_item_id.as_deref(),
                message_id,
                activation_id,
                *at,
                limits,
            ),
            TaskResultOperation::AdmitMessage {
                message_id,
                activation_id,
                at,
                ..
            } => results::select_admit_message_tx(
                tx,
                &self.agent_id,
                message_id,
                activation_id,
                *at,
                limits,
            ),
            TaskResultOperation::SettleInvalidOnly { message_id, at } => {
                results::select_settle_invalid_only_tx(tx, &self.agent_id, message_id, *at, limits)
            }
            TaskResultOperation::SettleOwnerUnavailable {
                message_id,
                disposition,
                at,
            } => results::select_settle_owner_unavailable_tx(
                tx,
                message_id,
                *disposition,
                *at,
                limits,
            ),
            TaskResultOperation::Defer {
                message_id,
                reason,
                at,
                deadline,
            } => results::select_mark_deferred_tx(tx, message_id, reason, *at, *deadline, limits),
            TaskResultOperation::MarkRecoveryDiagnostic {
                result_identity,
                epoch,
                now,
            } => results::select_mark_recovery_diagnostic_tx(
                tx,
                &self.agent_id,
                result_identity,
                *epoch,
                *now,
                limits,
            ),
            TaskResultOperation::Wake { work_item_id, at } => {
                results::select_wake_deferred_for_owner_tx(
                    tx,
                    &self.agent_id,
                    work_item_id,
                    *at,
                    limits,
                )
            }
            TaskResultOperation::EnsurePending { .. } => {
                anyhow::bail!("ensurePending requires its original length preflight")
            }
        }?;
        let projections = match &self.operation {
            TaskResultOperation::AdmitSnapshot { projections, .. } => projections
                .as_ref()
                .map(results::ResultAdmissionProjections::Snapshot),
            TaskResultOperation::AdmitMessage {
                message_id,
                projection: Some(projection),
                ..
            } => Some(results::ResultAdmissionProjections::Message {
                message_id,
                projection,
            }),
            _ => None,
        };
        selection.with_admission_projections(projections, limits)
    }
}

/// Exact original receipt material is reserved before its first body decode.
pub(crate) fn receipt_budget_tx(
    tx: &Transaction<'_>,
    original: &crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
    scope: &InputEvidenceScope,
    request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
    receipt_budget_with_material_tx(tx, original, scope, request, ledger).map(|value| value.0)
}

pub(in crate::runtime_db) fn receipt_budget_with_material_tx(
    tx: &Transaction<'_>,
    original: &crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
    scope: &InputEvidenceScope,
    request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<(
    std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
    usize,
    usize,
)> {
    use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
    let receipt_material = if original.family()
        == crate::runtime_db::input_mutation_receipts::InputMutationFamily::Task
    {
        "original Task receipt"
    } else {
        "original Result receipt"
    };
    let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(family AS BLOB))+length(CAST(phase_version AS BLOB))+length(CAST(command_digest AS BLOB))+length(CAST(scope_digest AS BLOB))+length(CAST(scopes_json AS BLOB))+length(CAST(prepared_at AS BLOB))+length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB))+length(CAST(committed_at AS BLOB))+length(CAST(result_digest AS BLOB)) FROM input_mutation_receipts WHERE command_nonce=?1),0)",
        [original.command_nonce()],|r|r.get(0))?;
    let bytes = bytes
        .checked_mul(8)
        .context("TaskResult receipt bytes overflow")?;
    anyhow::ensure!(
        bytes <= request.byte_limit.min(64 * 1024 * 1024),
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(receipt_material)
    );
    let records:usize=tx.query_row("SELECT COALESCE((SELECT MAX(COALESCE(json_array_length(outcome_json,'$.result.outcome.value'),1),1)+COALESCE(json_array_length(outcome_json,'$.scopes'),0)+COALESCE((SELECT SUM(COALESCE(json_array_length(s.value,'$.install_keys.tasks'),0)+COALESCE(json_array_length(s.value,'$.install_keys.task_result_settlements'),0)+COALESCE(json_array_length(s.value,'$.install_keys.input_roots'),0)+COALESCE(json_array_length(s.value,'$.install_keys.work_items'),0)+COALESCE(json_array_length(s.value,'$.install_keys.wait_conditions'),0)+COALESCE(json_array_length(s.value,'$.install_keys.continuations'),0)+COALESCE(json_array_length(s.value,'$.install_keys.scheduling'),0)+COALESCE(json_array_length(s.value,'$.audit_events'),0)) FROM json_each(outcome_json,'$.scopes') s),0) FROM input_mutation_receipts WHERE command_nonce=?1),0)",
        [original.command_nonce()],|r|r.get(0))?;
    let records = records
        .checked_mul(8)
        .context("TaskResult receipt count overflow")?;
    anyhow::ensure!(
        records <= request.record_limit.min(65536),
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(receipt_material)
    );
    let metadata_bytes = records.checked_mul(128).context("receipt nodes overflow")?;
    anyhow::ensure!(
        bytes
            .checked_add(metadata_bytes)
            .context("receipt material overflow")?
            <= request.byte_limit.min(64 * 1024 * 1024),
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(receipt_material)
    );
    Ok((
        ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(bytes)?,
                metadata_estimate_bytes: u64::try_from(metadata_bytes)?,
            },
        )?,
        bytes
            .checked_add(metadata_bytes)
            .context("receipt total overflow")?,
        records,
    ))
}
