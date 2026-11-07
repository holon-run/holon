//! Same original metadata receipt, without preparing unrelated resident domains.
use super::*;
use crate::runtime_db::input_mutation_receipts::{
    with_fresh_input_mutation_resolution_and_preflight, InputMutationReceipt,
    InputMutationResolution, InputMutationResult, OriginalInputMutationAttempt,
};

#[derive(Debug)]
pub(crate) struct TaskStatusMetadataSparseRecovery {
    attempt: OriginalInputMutationAttempt,
    receipt: InputMutationReceipt,
    identity: Option<crate::types::AgentIdentityRecord>,
    mode: TaskStatusMetadataInstallMode,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl TaskStatusMetadataSparseRecovery {
    pub(crate) fn matches_original(&self, attempt: &OriginalInputMutationAttempt) -> bool {
        self.attempt.same_attempt(attempt) && self.receipt.prepared == *attempt.prepared()
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        &self.receipt
    }
    pub(crate) fn identity(&self) -> Option<&crate::types::AgentIdentityRecord> {
        self.identity.as_ref()
    }
    pub(crate) fn mode(&self) -> TaskStatusMetadataInstallMode {
        self.mode
    }
}
/// This borrowed read permit cannot outlive the original coherent fresh view.
pub(crate) struct TaskStatusMetadataReadView<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    evidence: &'a TaskStatusMetadataSparseRecovery,
}
impl TaskStatusMetadataReadView<'_, '_> {
    pub(crate) fn validate_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(self.tx, tx)
                && self.evidence.receipt.prepared.scopes() == std::slice::from_ref(scope),
            "Status metadata recovery crossed its original fresh view"
        );
        Ok(())
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        self.evidence.receipt()
    }
    pub(crate) fn mode(&self) -> TaskStatusMetadataInstallMode {
        self.evidence.mode()
    }
    pub(crate) fn identity(&self) -> Option<&crate::types::AgentIdentityRecord> {
        self.evidence.identity()
    }
}
impl crate::runtime_db::RuntimeDb {
    pub(crate) fn with_task_status_metadata_recovery<T>(
        &self,
        attempt: &OriginalInputMutationAttempt,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        read: impl FnOnce(
            &Transaction<'_>,
            &TaskStatusMetadataReadView<'_, '_>,
            &TaskDomainLoadRequest,
        ) -> Result<T>,
    ) -> Result<Option<(TaskStatusMetadataSparseRecovery, T)>> {
        ensure!(
            attempt.prepared().family() == InputMutationFamily::Task
                && attempt.prepared().scopes().len() == 1,
            "Status metadata recovery requires its single original scope"
        );
        let scope = &attempt.prepared().scopes()[0];
        let mut connection = self.connection()?;
        let material = std::cell::RefCell::new(PhaseMaterial::new(request));
        let retained = std::cell::RefCell::new(None);
        with_fresh_input_mutation_resolution_and_preflight(
            &mut connection,
            attempt,
            |tx| {
                let mut material = material.borrow_mut();
                let (guard, bytes, records) =
                    crate::runtime_db::transitions::task_status_receipt_budget_with_material_tx(
                        tx,
                        attempt.prepared(),
                        scope,
                        &material.remaining()?,
                        ledger,
                    )?;
                material.include(bytes, records)?;
                retained.replace(Some(guard.clone()));
                Ok(guard)
            },
            |tx, resolution| {
                let InputMutationResolution::Present(receipt) = resolution else {
                    return Ok(None);
                };
                let InputMutationResult::TaskStatusMetadata { ack } = &receipt.outcome.result
                else {
                    return Ok(None);
                };
                ack.validate(attempt.prepared())?;
                let mut material = material.borrow_mut();
                let owner = material.owner_tx(tx, scope, ledger)?;
                let mode = owner.mode.into();
                let identity = owner.identity.clone();
                let mut guards = owner.into_material();
                // The original whole ACK already proves the accepted source write.
                // Recovery observes current metadata, never requires historical source
                // bodies or runs the completion kernel again.
                guards.push(
                    retained
                        .borrow_mut()
                        .take()
                        .context("Status receipt material missing")?,
                );
                let evidence = TaskStatusMetadataSparseRecovery {
                    attempt: attempt.clone(),
                    receipt,
                    identity,
                    mode,
                    _guards: guards,
                };
                let value = read(
                    tx,
                    &TaskStatusMetadataReadView {
                        tx,
                        evidence: &evidence,
                    },
                    &material.remaining()?,
                )?;
                Ok(Some((evidence, value)))
            },
        )
    }
}
