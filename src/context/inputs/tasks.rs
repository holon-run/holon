//! Task context reads use complete resident metadata and selected actual bodies.

use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_task::{CapturedTasks, TaskReadError},
    task_reads::{active_task_records, admitted_results},
};
use anyhow::Context;
use std::{collections::BTreeSet, sync::Arc};

/// Exact sources needed by the original pending-result selector, including its
/// current-message/reply de-duplication and bounded unique reply prefix.
pub(crate) struct PendingTaskInputRoots {
    pub roots: BTreeSet<crate::runtime::resident_input_evidence::InputEvidenceKey>,
    pub _owned_budget: Arc<BudgetViewGuard>,
}

fn is_current_task_input(
    metadata: &crate::runtime::resident_task::TaskResultMetadata,
    current_message_id: &str,
) -> bool {
    metadata.message_id == current_message_id
        || metadata.reply_message_id.as_deref() == Some(current_message_id)
}

pub(crate) fn pending_task_input_roots(
    tasks: &CapturedTasks,
    state: &AgentState,
    current_message_id: &str,
    ledger: &InputEvidenceBudgetLedger,
    record_limit: usize,
    byte_limit: usize,
) -> Result<PendingTaskInputRoots> {
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    let activation = state
        .current_execution_binding
        .as_ref()
        .and_then(|b| b.activation_id.as_deref());
    // Reserve before temporary sorting vectors, reply de-duplication, owning
    // key copies and their merged Input request coexist. Potential duplicate
    // IDs are charged conservatively; they never grant extra selected roots.
    let mut bytes = 0u64;
    let mut count = 0usize;
    for id in activation
        .and_then(|id| tasks.coverage.activation_result_ids.get(id))
        .into_iter()
        .flatten()
    {
        let metadata = tasks.coverage.result_metadata.get(id).ok_or_else(|| {
            TaskReadError::NeedsPreparation(crate::runtime::resident_task::TaskKey::new(
                crate::runtime::resident_task::TaskFamily::ResultSettlement,
                id,
            ))
        })?;
        for source in std::iter::once(metadata.message_id.as_str())
            .chain(metadata.reply_message_id.as_deref())
        {
            count = count
                .checked_add(1)
                .context("Task input selector count overflow")?;
            bytes = bytes
                .checked_add(serialized_bytes(&source)?)
                .and_then(|n| n.checked_add(128))
                .context("Task input selector bytes overflow")?;
        }
    }
    let bytes = bytes
        .checked_mul(2)
        .context("Task input selector copies overflow")?;
    anyhow::ensure!(
        count <= record_limit && bytes <= u64::try_from(byte_limit)?,
        TaskReadError::ResourceExhausted("context input selectors")
    );
    let owned_budget = ledger.register_owned_bytes(
        &tasks.scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?;
    let mut roots = BTreeSet::new();
    let Some(activation) = activation else {
        return Ok(PendingTaskInputRoots {
            roots,
            _owned_budget: owned_budget,
        });
    };
    let mut replies = BTreeSet::new();
    for record in admitted_results(tasks, activation)? {
        let metadata = &tasks.coverage.result_metadata[&record.result_identity];
        if is_current_task_input(metadata, current_message_id) {
            continue;
        }
        roots.insert(Key::new(Family::Message, &record.message_id));
        if let Some(reply) = &metadata.reply_message_id {
            if replies.insert(reply.clone()) {
                roots.insert(Key::new(Family::Message, reply));
            }
        }
    }
    Ok(PendingTaskInputRoots {
        roots,
        _owned_budget: owned_budget,
    })
}

impl ResidentContextInputs<'_, '_> {
    fn task_inputs(&self) -> Result<&CapturedTasks> {
        self.tasks
            .ok_or_else(|| TaskReadError::IncompleteActive.into())
    }
    fn retain_task_projection<T: serde::Serialize>(&self, value: &T, copies: u64) -> Result<()> {
        if let Some((ledger, scope)) = self.input_budget() {
            let bytes = serialized_bytes(value)?
                .checked_mul(copies)
                .context("Task context budget overflow")?;
            self.task_projection_guards
                .borrow_mut()
                .push(ledger.register_owned_bytes(
                    &scope,
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: bytes,
                        metadata_estimate_bytes: 0,
                    },
                )?);
        }
        Ok(())
    }
    pub(super) fn resident_active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
        self.check_scope(agent_id)?;
        let rows = active_task_records(self.task_inputs()?)?;
        self.retain_task_projection(&rows, 1)?;
        Ok(rows.into_iter().cloned().collect())
    }
    pub(super) fn resident_admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.check_scope(agent_id)?;
        let rows = admitted_results(self.task_inputs()?, activation_id)?;
        self.retain_task_projection(&rows, 1)?;
        Ok(rows.into_iter().cloned().collect())
    }
    pub(super) fn resident_admitted_settlement_inputs(
        &self,
        agent_id: &str,
        activation_id: &str,
        current_message_id: &str,
        reply_limit: usize,
    ) -> Result<Vec<AdmittedSettlementInput>> {
        self.check_scope(agent_id)?;
        let tasks = self.task_inputs()?;
        let mut rows = admitted_results(tasks, activation_id)?;
        // The selector excludes the current body and its result wrappers using
        // canonical Task metadata; excluded wrappers have no Input read grant.
        rows.retain(|record| {
            !is_current_task_input(
                &tasks.coverage.result_metadata[&record.result_identity],
                current_message_id,
            )
        });
        // Estimate only borrowed facts; no Task/result/Message body is cloned yet.
        let mut sources = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = 0usize;
        for record in &rows {
            let message = self
                .captured
                .lookup(&Key::new(Family::Message, &record.message_id))?;
            let message = message
                .map(|row| match &row.record {
                    PreparedInputEvidence::Message(message) => Ok(message),
                    _ => anyhow::bail!("resident Task result source family mismatch"),
                })
                .transpose()?;
            if message.and_then(crate::wake_contract::agent_message_reply_reference)
                == Some(current_message_id)
            {
                continue;
            }
            sources.extend(message);
            if pending < reply_limit {
                if let Some(reply) =
                    message.and_then(crate::wake_contract::agent_message_reply_reference)
                {
                    if seen.insert(reply) {
                        let reply = self.captured.lookup(&Key::new(Family::Message, reply))?;
                        if let Some(reply) = reply {
                            let PreparedInputEvidence::Message(reply) = &reply.record else {
                                anyhow::bail!("resident reply family mismatch")
                            };
                            sources.push(reply);
                        }
                    }
                }
            }
            pending += 1;
        }
        self.retain_task_projection(&(&rows, &sources), 2)?;
        prepare_admitted_settlement_inputs(
            rows.into_iter().cloned().collect(),
            current_message_id,
            reply_limit,
            |id| self.history_message_by_id(id),
        )
    }
}

pub(super) type TaskProjectionGuards = std::cell::RefCell<Vec<Arc<BudgetViewGuard>>>;
