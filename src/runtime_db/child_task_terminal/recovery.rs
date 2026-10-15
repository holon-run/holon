//! One fresh view of the exact finished child terminal receipt and all its owners.
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    OriginalChildTaskProducer,
};
use crate::runtime_db::{
    input_mutation_receipts::{
        with_fresh_input_mutation_resolution_and_preflight, InputMutationReceipt,
        InputMutationResolution, InputMutationResult, OriginalInputMutationAttempt,
    },
    task_snapshot::TaskDomainLoadRequest,
};
use rusqlite::Transaction;
use std::sync::Arc;

pub(crate) struct OriginalChildTerminalRecovery {
    attempt: OriginalInputMutationAttempt,
    receipt: InputMutationReceipt,
    owners: Vec<(
        InputEvidenceScope,
        super::super::command_task_terminal::CommandTaskOwnerObservation,
    )>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl OriginalChildTerminalRecovery {
    pub(crate) fn matches_original(&self, attempt: &OriginalInputMutationAttempt) -> bool {
        self.attempt.same_attempt(attempt) && self.receipt.prepared == *attempt.prepared()
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        &self.receipt
    }
    pub(crate) fn identity(
        &self,
        scope: &InputEvidenceScope,
    ) -> Option<&crate::types::AgentIdentityRecord> {
        self.owners
            .iter()
            .find(|(s, _)| s == scope)
            .and_then(|(_, owner)| owner.identity.as_ref())
    }
    pub(crate) fn mode(&self, scope: &InputEvidenceScope) -> Result<ChildTaskTerminalInstallMode> {
        self.owners
            .iter()
            .find(|(s, _)| s == scope)
            .map(|(_, owner)| owner.mode.into())
            .context("child recovery owner absent")
    }
}
pub(crate) struct OriginalChildTerminalReadView<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    evidence: &'a OriginalChildTerminalRecovery,
}
impl OriginalChildTerminalReadView<'_, '_> {
    pub(crate) fn validate_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx) && self.evidence.receipt.prepared.scopes().contains(scope),
            "child terminal recovery crossed its original fresh view"
        );
        Ok(())
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        self.evidence.receipt()
    }
    pub(crate) fn mode(&self, scope: &InputEvidenceScope) -> Result<ChildTaskTerminalInstallMode> {
        self.evidence.mode(scope)
    }
    pub(crate) fn identity(
        &self,
        scope: &InputEvidenceScope,
    ) -> Option<&crate::types::AgentIdentityRecord> {
        self.evidence.identity(scope)
    }
}
impl super::super::RuntimeDb {
    pub(crate) fn with_original_child_terminal_recovery<T>(
        &self,
        attempt: &OriginalInputMutationAttempt,
        source: &OriginalChildTaskProducer,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        read: impl FnOnce(
            &Transaction<'_>,
            &OriginalChildTerminalReadView<'_, '_>,
            &mut OriginalChildTerminalRecoveryMaterial<'_, '_, '_>,
        ) -> Result<T>,
    ) -> Result<Option<(OriginalChildTerminalRecovery, T)>> {
        source.validate_material_ledger(ledger)?;
        ensure!(
            attempt.prepared().family() == InputMutationFamily::Task
                && attempt
                    .prepared()
                    .scopes()
                    .contains(&source.address().scope),
            "child terminal recovery lacks original Task scope"
        );
        let mut connection = self.connection()?;
        let material = std::cell::RefCell::new(
            super::super::task_status_metadata::PhaseMaterial::new(request),
        );
        let guards = std::cell::RefCell::new(Vec::new());
        with_fresh_input_mutation_resolution_and_preflight(
            &mut connection,
            attempt,
            |tx| {
                let mut phase = material.borrow_mut();
                let (receipt_guards, bytes, records) = super::child_receipt_material_tx(
                    tx,
                    attempt.prepared(),
                    &source.address().scope,
                    &phase.remaining()?,
                    ledger,
                )?;
                phase.include(bytes, records)?;
                let raw = receipt_guards
                    .first()
                    .context("Child raw receipt guard missing")?
                    .clone();
                guards.borrow_mut().extend(receipt_guards);
                Ok(raw)
            },
            |tx, resolution| {
                let InputMutationResolution::Present(receipt) = resolution else {
                    return Ok(None);
                };
                let InputMutationResult::ChildTaskTerminal { ack } = &receipt.outcome.result else {
                    return Ok(None);
                };
                ack.validate(attempt.prepared())?;
                ensure!(
                    &ack.original == source.address(),
                    "fresh child receipt returned another original lease"
                );
                let mut phase = material.borrow_mut();
                let group_bytes = attempt
                    .prepared()
                    .scopes()
                    .iter()
                    .try_fold(256usize, |n, s| {
                        n.checked_add(s.agent_id.len() + 256)
                            .context("child fresh owner group overflow")
                    })?
                    .checked_mul(8)
                    .context("child fresh owner copies overflow")?;
                let group_count = attempt
                    .prepared()
                    .scopes()
                    .len()
                    .checked_mul(8)
                    .context("child fresh owner count overflow")?;
                let guard = super::super::command_task_terminal::charge_material(
                    &source.address().scope,
                    group_bytes,
                    group_count,
                    &phase.remaining()?,
                    ledger,
                )?;
                phase.include(group_bytes, group_count)?;
                guards.borrow_mut().push(guard);
                let mut owners = Vec::with_capacity(attempt.prepared().scopes().len());
                for scope in attempt.prepared().scopes() {
                    let raw:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
                    let bytes = raw
                        .checked_add(scope.agent_id.len() + 256)
                        .and_then(|n| n.checked_mul(8))
                        .context("child fresh identity material overflow")?;
                    let owner = super::super::command_task_terminal::observe_original_owner_tx(
                        tx,
                        scope,
                        &phase.remaining()?,
                        ledger,
                    )?;
                    phase.include(bytes, 8)?;
                    owners.push((scope.clone(), owner));
                }
                let mut evidence = OriginalChildTerminalRecovery {
                    attempt: attempt.clone(),
                    receipt,
                    owners,
                    _guards: guards.borrow_mut().drain(..).collect(),
                };
                let request = phase.remaining()?;
                let view = OriginalChildTerminalReadView {
                    tx,
                    evidence: &evidence,
                };
                let mut reader = OriginalChildTerminalRecoveryMaterial {
                    view: &view,
                    request: &request,
                    ledger,
                    material: super::super::task_snapshot::read_preparation::TaskReadMaterial::new(
                        &source.address().scope,
                        &request,
                        Some(ledger),
                    ),
                };
                let value = read(tx, &view, &mut reader)?;
                evidence._guards.extend(reader.material.into_guards());
                Ok(Some((evidence, value)))
            },
        )
    }
}

/// This callback-only reader exposes no general TaskReadMaterial or new body grant.
/// Every State/head acquisition is limited by the exact original fresh receipt.
pub(crate) struct OriginalChildTerminalRecoveryMaterial<'a, 'tx, 'conn> {
    view: &'a OriginalChildTerminalReadView<'tx, 'conn>,
    request: &'a TaskDomainLoadRequest,
    ledger: &'a InputEvidenceBudgetLedger,
    material: super::super::task_snapshot::read_preparation::TaskReadMaterial<'a>,
}
impl OriginalChildTerminalRecoveryMaterial<'_, '_, '_> {
    pub(crate) fn request(&self) -> &TaskDomainLoadRequest {
        self.request
    }
    #[cfg(test)]
    pub(crate) fn material_for_test(&self) -> super::super::task_snapshot::TaskMaintenanceMaterial {
        self.material.material_estimate()
    }
    pub(crate) fn observe_known_slots(&mut self, tx: &Transaction<'_>) -> Result<()> {
        let scope = match &self.view.receipt().outcome.result {
            InputMutationResult::ChildTaskTerminal { ack } => &ack.original.scope,
            _ => unreachable!(),
        };
        self.view.validate_tx(tx, scope)?;
        let count = self.view.receipt().prepared.scopes().len();
        self.material.charge_ids(
            count
                .checked_mul(512)
                .context("child Known slots overflow")?,
            count,
        )
    }
    pub(crate) fn read_primary_state(
        &mut self,
        tx: &Transaction<'_>,
    ) -> Result<crate::types::AgentState> {
        let InputMutationResult::ChildTaskTerminal { ack } = &self.view.receipt().outcome.result
        else {
            unreachable!()
        };
        let scope = &ack.original.scope;
        self.view.validate_tx(tx, scope)?;
        ensure!(
            self.view.mode(scope)? == ChildTaskTerminalInstallMode::Current,
            "child primary State is unavailable"
        );
        let bytes:usize=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(current_run_id AS BLOB)),0) FROM agent_states WHERE agent_id=?1",[&scope.agent_id],|r|r.get(0))?;
        self.material.charge_ids(
            bytes
                .checked_add(256)
                .and_then(|n| n.checked_mul(2))
                .context("child fresh State material overflow")?,
            2,
        )?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1",
            [&scope.agent_id],
            |row| row.get(0),
        )?;
        self.material.charge_ids(
            nodes
                .checked_mul(2 * 256)
                .context("child fresh State node bytes overflow")?,
            nodes
                .checked_mul(2)
                .context("child fresh State nodes overflow")?,
        )?;
        let (state, _guard) = super::super::command_task_terminal::read_original_state_tx(
            tx,
            scope,
            self.request,
            self.ledger,
        )?;
        // The shared eight-copy material remains until the entire group publishes.
        Ok(state)
    }
    pub(crate) fn observe_primary_selectors(&mut self, tx: &Transaction<'_>) -> Result<()> {
        let InputMutationResult::ChildTaskTerminal { ack } = &self.view.receipt().outcome.result
        else {
            unreachable!()
        };
        self.view.validate_tx(tx, &ack.original.scope)?;
        let keys = &self
            .view
            .receipt()
            .outcome
            .scopes
            .iter()
            .find(|outcome| outcome.scope == ack.original.scope)
            .context("child primary selector outcome missing")?
            .install_keys;
        let count = keys
            .work_items
            .len()
            .checked_add(keys.wait_conditions.len())
            .and_then(|n| n.checked_add(keys.continuations.len()))
            .and_then(|n| n.checked_add(keys.tasks.len()))
            .and_then(|n| n.checked_add(keys.task_result_settlements.len()))
            .context("child recovery selectors overflow")?;
        let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
            keys,
        )?)?
        .checked_add(
            count
                .checked_mul(256)
                .context("child selector slots overflow")?,
        )
        .and_then(|n| n.checked_mul(2))
        .context("child selector copies overflow")?;
        self.material.charge_ids(
            bytes,
            count
                .checked_mul(2)
                .context("child selector count overflow")?,
        )
    }
    pub(crate) fn observe_queue_metadata(
        &mut self,
        tx: &Transaction<'_>,
        bytes: usize,
        count: usize,
    ) -> Result<()> {
        let InputMutationResult::ChildTaskTerminal { ack } = &self.view.receipt().outcome.result
        else {
            unreachable!()
        };
        self.view.validate_tx(tx, &ack.original.scope)?;
        self.material.charge_ids(bytes, count)
    }
    pub(crate) fn read_primary_input_domains(
        &mut self,
        tx: &Transaction<'_>,
        state: &crate::types::AgentState,
        input: &super::super::input_evidence_snapshot::InputEvidenceLoadRequest,
        episodes: usize,
        work: &crate::runtime::resident_work::WorkCaptureRequest,
        tasks: &crate::runtime::resident_task::TaskCaptureRequest,
        query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
        event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    ) -> Result<super::super::input_preparation::PreparedInputRead> {
        super::super::input_preparation::prepare_child_terminal_input_recovery_tx(
            tx,
            self.view,
            state,
            input,
            episodes,
            self.ledger,
            work,
            tasks,
            query,
            &mut self.material,
            event_roles,
        )
    }
    pub(crate) fn read_related_status(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        before: &crate::runtime::resident_task::TaskCoverage,
        storage_replayed: bool,
    ) -> Result<crate::runtime::resident_task::CommittedTaskDelta> {
        self.view.validate_tx(tx, scope)?;
        let InputMutationResult::ChildTaskTerminal { ack } = &self.view.receipt().outcome.result
        else {
            unreachable!()
        };
        ensure!(
            scope != &ack.original.scope
                && self.view.mode(scope)? == ChildTaskTerminalInstallMode::Current,
            "child related metadata read has no original Current source"
        );
        let related = ack
            .status_scopes
            .iter()
            .find(|row| &row.scope == scope)
            .context("child related ACK absent")?;
        super::super::task_snapshot::read_child_status_metadata_using_material_tx(
            tx,
            scope,
            before,
            related.revision,
            storage_replayed,
            &mut self.material,
        )
    }
}
impl std::fmt::Debug for OriginalChildTerminalRecovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalChildTerminalRecovery")
            .field("original", &self.receipt.prepared)
            .finish_non_exhaustive()
    }
}
