//! Same finished binding attempt, exact whole Task receipt, one fresh owner view.
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_task::ResidentTaskDomain,
    OriginalChildTaskProducer,
};
use crate::runtime_db::{
    command_task_terminal as owner,
    input_mutation_receipts::{
        with_fresh_input_mutation_resolution_and_preflight, InputMutationReceipt,
        InputMutationResolution, InputMutationResult, OriginalInputMutationAttempt,
    },
    task_snapshot::{self as tasks, TaskDomainLoadRequest},
    transitions::PreparedInputTransition,
};
use rusqlite::Transaction;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct OriginalChildMonitorBindingSparseRecovery {
    attempt: OriginalInputMutationAttempt,
    receipt: InputMutationReceipt,
    _identity: Option<crate::types::AgentIdentityRecord>,
    _mode: owner::CommandTaskTerminalInstallMode,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl OriginalChildMonitorBindingSparseRecovery {
    pub(crate) fn matches_original(&self, attempt: &OriginalInputMutationAttempt) -> bool {
        self.attempt.same_attempt(attempt) && self.receipt.prepared == *attempt.prepared()
    }
    pub(crate) fn receipt(&self) -> &InputMutationReceipt {
        &self.receipt
    }
}
pub(crate) struct OriginalChildMonitorBindingReadView<'a, 'conn, 'material> {
    tx: &'a Transaction<'conn>,
    scope: &'a InputEvidenceScope,
    mode: owner::CommandTaskTerminalInstallMode,
    identity: Option<&'a crate::types::AgentIdentityRecord>,
    request: &'material TaskDomainLoadRequest,
    ledger: &'material InputEvidenceBudgetLedger,
    material: &'a mut tasks::read_preparation::TaskReadMaterial<'material>,
}
impl OriginalChildMonitorBindingReadView<'_, '_, '_> {
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        self.scope
    }
    pub(crate) fn mode(&self) -> owner::CommandTaskTerminalInstallMode {
        self.mode
    }
    pub(crate) fn identity(&self) -> Option<&crate::types::AgentIdentityRecord> {
        self.identity
    }
    pub(crate) fn validate_current_state(
        &mut self,
        tx: &Transaction<'_>,
        expected: &crate::types::AgentState,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx)
                && self.mode == owner::CommandTaskTerminalInstallMode::Current,
            "binding Task read has no actual Current owner view"
        );
        let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(current_run_id AS BLOB)),0) FROM agent_states WHERE agent_id=?1),0)",[&self.scope.agent_id],|row|row.get(0))?;
        self.material.charge_ids(
            bytes
                .checked_mul(2)
                .context("binding recovery State copies overflow")?,
            2,
        )?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM agent_states s,json_tree(s.payload_json) WHERE s.agent_id=?1",
            [&self.scope.agent_id],
            |row| row.get(0),
        )?;
        self.material.charge_ids(
            nodes
                .checked_mul(2 * 256)
                .context("binding recovery State nodes overflow")?,
            nodes
                .checked_mul(2)
                .context("binding recovery State count overflow")?,
        )?;
        let facts =
            crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &self.scope.agent_id)
                .scope_facts()?;
        ensure!(
            facts.identity.as_ref() == self.identity && facts.state.as_ref() == Some(expected),
            "binding-only receipt changed original State/identity facts"
        );
        Ok(())
    }
    pub(crate) fn read_current_tasks(
        &mut self,
        tx: &Transaction<'_>,
        expected: &crate::types::AgentState,
    ) -> Result<Arc<ResidentTaskDomain>> {
        ensure!(
            std::ptr::eq(tx, self.tx)
                && self.mode == owner::CommandTaskTerminalInstallMode::Current,
            "binding Task read has no actual Current owner view"
        );
        let current = tasks::load_selected_using_material(
            tx,
            self.scope,
            self.request,
            // A positive original receipt permits named preparation for the
            // current live selector view. It does not revive old query roles.
            true,
            true,
            false,
            Some(self.ledger),
            None,
            None,
            self.material,
        )?
        .require_current()?;
        ensure!(
            current.facts.identity.as_ref() == self.identity
                && current.facts.state.as_ref() == Some(expected),
            "binding-only receipt changed original State/identity facts"
        );
        Ok(Arc::new(current.resident))
    }
}
impl crate::runtime_db::RuntimeDb {
    pub(crate) fn with_original_child_monitor_binding_recovery<T>(
        &self,
        attempt: &OriginalInputMutationAttempt,
        source: &OriginalChildTaskProducer,
        prepared: &PreparedInputTransition,
        request: &TaskDomainLoadRequest,
        retained_queue: tasks::TaskMaintenanceMaterial,
        ledger: &InputEvidenceBudgetLedger,
        read: impl FnOnce(
            &Transaction<'_>,
            &mut OriginalChildMonitorBindingReadView<'_, '_, '_>,
        ) -> Result<T>,
    ) -> Result<Option<(OriginalChildMonitorBindingSparseRecovery, T)>> {
        source.validate_material_ledger(ledger)?;
        let intent = prepared
            .original_child_monitor_binding()
            .context("binding recovery lacks original facet")?;
        ensure!(
            &intent.original == source.address()
                && attempt.prepared() == prepared.descriptor()
                && attempt.prepared().scopes() == std::slice::from_ref(&source.address().scope),
            "binding recovery changed its exact original attempt"
        );
        let scope = &source.address().scope;
        let mut connection = self.connection()?;
        let phase = std::cell::RefCell::new(
            crate::runtime_db::task_status_metadata::PhaseMaterial::new(request),
        );
        // The finished witness owns this queue container guard. Include its
        // integers before any receipt/State/Task acquisition, without a new guard.
        phase
            .borrow_mut()
            .include(retained_queue.bytes, retained_queue.records)?;
        let usage = intent.material_estimate();
        phase.borrow_mut().include(usage.bytes, usage.records)?;
        let guards = std::cell::RefCell::new(Vec::new());
        with_fresh_input_mutation_resolution_and_preflight(
            &mut connection,
            attempt,
            |tx| {
                let (held, bytes, records) = super::child_receipt_material_tx(
                    tx,
                    attempt.prepared(),
                    scope,
                    &phase.borrow().remaining()?,
                    ledger,
                )?;
                phase.borrow_mut().include(bytes, records)?;
                let guard = held
                    .first()
                    .context("binding receipt raw guard absent")?
                    .clone();
                guards.borrow_mut().extend(held);
                Ok(guard)
            },
            |tx, resolution| {
                let InputMutationResolution::Present(receipt) = resolution else {
                    return Ok(None);
                };
                ensure!(
                    matches!(&receipt.outcome.result,InputMutationResult::Task{task_id,deletion:None,stop:None}
                if task_id==&source.address().admitted.key.id),
                    "binding exact receipt has another Task result"
                );
                let observation = phase.borrow_mut().owner_tx(tx, scope, ledger)?;
                let mode = observation.mode;
                let (record_limit, byte_limit) = TaskDomainLoadRequest::bounded_material_limits(
                    request.record_limit,
                    request.byte_limit,
                );
                let remaining = phase.borrow().remaining()?;
                let mut material =
                    tasks::read_preparation::TaskReadMaterial::new(scope, request, Some(ledger))
                        .with_admitted_material(tasks::TaskMaintenanceMaterial {
                            bytes: byte_limit
                                .checked_sub(remaining.byte_limit)
                                .context("binding fresh bytes underflow")?,
                            records: record_limit
                                .checked_sub(remaining.record_limit)
                                .context("binding fresh records underflow")?,
                        })?;
                let value = read(
                    tx,
                    &mut OriginalChildMonitorBindingReadView {
                        tx,
                        scope,
                        mode,
                        identity: observation.identity.as_ref(),
                        request,
                        ledger,
                        material: &mut material,
                    },
                )?;
                let identity = observation.identity.clone();
                let mut held = guards.borrow_mut().drain(..).collect::<Vec<_>>();
                held.extend(observation.into_material());
                held.extend(material.into_guards());
                Ok(Some((
                    OriginalChildMonitorBindingSparseRecovery {
                        attempt: attempt.clone(),
                        receipt,
                        _identity: identity,
                        _mode: mode,
                        _guards: held,
                    },
                    value,
                )))
            },
        )
    }
}
