//! One original Task phase lends its actual remaining allowance to Input readers.
use super::*;
use crate::runtime_db::input_evidence_snapshot::{
    read_material::InputReadMaterial, InputEvidenceLoadRequest,
};

impl TaskReadMaterial<'_> {
    /// No new scope, ledger registration for prior, or independent Memory cap.
    pub(in crate::runtime_db) fn with_input_material<T>(
        &mut self,
        action: impl FnOnce(&mut InputReadMaterial<'_>) -> Result<T>,
    ) -> Result<T> {
        let ledger = self
            .budget
            .context("Task/Input requires the original owner ledger")?;
        let request = InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: BTreeSet::new(),
            interrupted_sources: BTreeSet::new(),
            selected_completion_brief_ids: BTreeSet::new(),
            byte_limit: self.request.byte_limit,
            record_limit: self.request.record_limit,
        };
        self.with_input_material_for_request(self.scope, ledger, &request, None, action)
    }

    /// The entire original Input observation borrows this Task's admitted
    /// receipt/material, actual Memory and stricter caller limits.
    pub(in crate::runtime_db) fn with_input_material_for_request<T>(
        &mut self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        request: &InputEvidenceLoadRequest,
        event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
        action: impl FnOnce(&mut InputReadMaterial<'_>) -> Result<T>,
    ) -> Result<T> {
        ensure!(
            self.scope == scope && self.budget.is_some_and(|actual| actual.same_ledger(ledger)),
            "Task/Input original scope or ledger differs"
        );
        if let Some(roles) = event_roles {
            roles.visit_material(scope, ledger, |guard, bytes, records| {
                if self.guards.iter().any(|held| Arc::ptr_eq(held, guard)) {
                    return Ok(());
                }
                let next_bytes = self
                    .bytes
                    .checked_add(bytes)
                    .context("Task/Event byte prior overflow")?;
                let next_records = self
                    .records
                    .checked_add(records)
                    .context("Task/Event node prior overflow")?;
                bound(next_records, next_bytes, Some(self.request))?;
                let observed = ObservedTaskReadMaterial {
                    scope: self.scope,
                    ledger,
                    guard: guard.clone(),
                    bytes,
                    records,
                };
                if let Some(memory) = self.memory_budget.as_deref_mut() {
                    memory.additional_observed_task_material(&observed)?;
                }
                self.guards.push(observed.guard);
                self.bytes = next_bytes;
                self.records = next_records;
                Ok(())
            })?;
        }
        let limits = InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: BTreeSet::new(),
            interrupted_sources: BTreeSet::new(),
            selected_completion_brief_ids: BTreeSet::new(),
            byte_limit: request.byte_limit.min(self.request.byte_limit),
            record_limit: request.record_limit.min(self.request.record_limit),
        };
        let mut material = InputReadMaterial::new_with_memory(
            self.scope,
            &limits,
            ledger,
            (self.bytes, self.records),
            self.memory_budget.as_deref_mut(),
        )?;
        let result = action(&mut material);
        let usage = material.usage();
        let guards = material.into_guards();
        // This also retains a failed read's remaining actual guards until the
        // original actor unwinds; transient Tool scratch already retired itself.
        self.bytes = usage.0;
        self.records = usage.1;
        self.guards.extend(guards);
        bound(self.records, self.bytes, Some(self.request))?;
        result
    }
}
