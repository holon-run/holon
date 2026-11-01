//! A finite terminal read does not replace other consumers' prepared domains.
use super::*;
use crate::runtime_db::{
    context_inputs::ContextDurableRead,
    input_evidence_snapshot::{
        prepare_input_evidence_with_scope_read_tx, InputEvidenceBootstrapSnapshot,
        InputEvidenceLoadRequest,
    },
    input_preparation::turn_inputs::{self, TurnInputMembers, TurnInputSelector},
};

impl RuntimeMutation {
    pub(in crate::runtime) fn prepare_terminal_inputs(
        &self,
        request: &InputEvidenceLoadRequest,
        turn: &TurnInputSelector<'_>,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<(
        TurnInputMembers,
        super::super::resident_input_evidence::CapturedInputEvidence,
    )> {
        let root = self.capture_published_root()?;
        anyhow::ensure!(
            root.state.as_ref() == &self.candidate.state,
            "terminal read cannot consume a private candidate"
        );
        let scope = self.input_evidence_scope();
        let budget = &self.runtime.inner.input_evidence_budget;
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        anyhow::ensure!(
            request.record_limit > 0
                && request.record_limit <= record_limit
                && request.byte_limit > 0
                && request.byte_limit <= byte_limit,
            "terminal read exceeds the held owner's budget"
        );
        material.read(|| {
            self.runtime.inner.runtime_db.transaction(|tx| {
                let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
                anyhow::ensure!(
                    read.facts().state.as_deref() == Some(root.state.as_ref()),
                    "terminal input preparation differs from the held State"
                );
                let members = turn_inputs::select_tx(tx, &scope, turn, request.record_limit)?;
                crate::diagnostics::resource_io::admit_cloned_value(request)?;
                let mut request = request.clone();
                members.extend_request(&mut request)?;
                let original_cleanup = read.facts().identity.as_ref().is_some_and(|identity| {
                    identity.status == crate::types::AgentRegistryStatus::Deleting
                });
                let snapshot = prepare_input_evidence_with_scope_read_tx(
                    tx,
                    &scope,
                    request.borrowed(),
                    original_cleanup,
                    read,
                )?;
                let InputEvidenceBootstrapSnapshot::Current(snapshot) = snapshot else {
                    anyhow::bail!("terminal input scope retired during preparation");
                };
                let inputs = snapshot
                    .resident
                    .with_budget(budget.clone())?
                    .capture_for_request(&request)?;
                // Pending queue Messages are not execution sources for this terminal.
                // Existing Input/Current roles and windows keep their published cut.
                Ok((members, inputs))
            })
        })
    }
}
