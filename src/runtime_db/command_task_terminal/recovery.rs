//! Same-attempt positive sparse recovery; this never reads a successor State.
use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        input_mutation_receipts::{
            with_fresh_input_mutation_resolution_and_preflight, InputMutationReceipt,
            InputMutationResolution, InputMutationResult, OriginalInputMutationAttempt,
        },
        task_snapshot::TaskDomainLoadRequest,
    },
};
use anyhow::Context;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct OriginalCommandTaskSparseRecovery {
    attempt: OriginalInputMutationAttempt,
    receipt: InputMutationReceipt,
    identity: Option<crate::types::AgentIdentityRecord>,
    mode: CommandTaskTerminalInstallMode,
    _guards: Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}
impl OriginalCommandTaskSparseRecovery {
    pub(crate) fn matches_original(&self, attempt: &OriginalInputMutationAttempt) -> bool {
        self.attempt.same_attempt(attempt) && self.receipt.prepared == *attempt.prepared()
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        &self.receipt
    }
    pub(crate) fn identity(&self) -> Option<&crate::types::AgentIdentityRecord> {
        self.identity.as_ref()
    }
    pub(crate) fn mode(&self) -> CommandTaskTerminalInstallMode {
        self.mode
    }
}
impl crate::runtime_db::RuntimeDb {
    pub(crate) fn observe_original_command_task_sparse_recovery(
        &self,
        attempt: &OriginalInputMutationAttempt,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Option<OriginalCommandTaskSparseRecovery>> {
        use crate::runtime_db::input_mutation_receipts::InputMutationFamily;
        ensure!(
            attempt.prepared().family() == InputMutationFamily::Task
                && attempt.prepared().scopes().len() == 1,
            "Command recovery requires its original Task receipt group"
        );
        let scope = &attempt.prepared().scopes()[0];
        let mut connection = self.connection()?;
        let material = std::cell::RefCell::new(None);
        with_fresh_input_mutation_resolution_and_preflight(
            &mut connection,
            attempt,
            |tx| {
                let view = crate::runtime_db::transitions::task_result_receipt_budget_tx(
                    tx,
                    attempt.prepared(),
                    scope,
                    request,
                    ledger,
                )?;
                material.replace(Some(view.clone()));
                Ok(view)
            },
            |tx, resolution| {
                let InputMutationResolution::Present(receipt) = resolution else {
                    return Ok(None);
                };
                let InputMutationResult::CommandTaskTerminal { ack } = &receipt.outcome.result
                else {
                    return Ok(None);
                };
                ack.validate(attempt.prepared())?;
                ensure!(
                    &ack.original.scope == scope,
                    "Command recovery original namespace differs"
                );
                let owner = observe_original_owner_tx(tx, scope, request, ledger)?;
                if owner.mode == CommandTaskTerminalInstallMode::Current {
                    return Ok(None);
                };
                // The old positive Task receipt authorizes current availability
                // observation only. No Task/raw aliases, State or queue is read.
                let identity = owner.identity.clone();
                let mode = owner.mode;
                let mut guards = owner.into_material();
                guards.push(
                    material
                        .borrow_mut()
                        .take()
                        .context("Command recovery receipt guard missing")?,
                );
                Ok(Some(OriginalCommandTaskSparseRecovery {
                    attempt: attempt.clone(),
                    receipt,
                    identity,
                    mode,
                    _guards: guards,
                }))
            },
        )
    }
}
