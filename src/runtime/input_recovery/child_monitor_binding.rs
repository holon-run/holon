//! A binding whole ACK changed only this Task body; preserve original other domains.
use super::*;
use crate::runtime_db::{
    child_task_terminal::{
        OriginalChildMonitorBindingReadView, OriginalChildMonitorBindingSparseRecovery,
    },
    command_task_terminal::CommandTaskTerminalInstallMode,
};
#[derive(Debug)]
pub(in crate::runtime) struct ChildMonitorBindingKnown {
    evidence: OriginalChildMonitorBindingSparseRecovery,
    known: KnownCommit,
}
impl ChildMonitorBindingKnown {
    pub(super) fn new(
        evidence: OriginalChildMonitorBindingSparseRecovery,
        known: KnownCommit,
    ) -> Self {
        Self { evidence, known }
    }
    pub(in crate::runtime) fn evidence(&self) -> &OriginalChildMonitorBindingSparseRecovery {
        &self.evidence
    }
    pub(in crate::runtime) fn known(&self) -> KnownCommit {
        self.known.clone()
    }
}
pub(super) fn prepare_known_tx(
    tx: &rusqlite::Transaction<'_>,
    view: &mut OriginalChildMonitorBindingReadView<'_, '_, '_>,
    witness: &super::super::mutation_witness::MutationWitness,
) -> Result<KnownCommit> {
    if view.mode() != CommandTaskTerminalInstallMode::Current {
        return Ok(KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            identity: view.identity().cloned().map(Arc::new),
            state: None,
        })));
    }
    let scope = view.scope().clone();
    let original = witness
        .input_runtime(&scope.agent_id)
        .map(|facts| facts.task_status_metadata_known())
        .or_else(|| witness.input_child_binding_observation().cloned());
    let Some(KnownCommit::State {
        state,
        episodes,
        input_evidence,
        work,
        tasks,
        scheduling,
        runtime,
    }) = original
    else {
        // Actual Current identity/State presence is an observation, not a Ready
        // claim for a cold owner that had no published runtime domains.
        return Ok(KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            identity: view.identity().cloned().map(Arc::new),
            state: None,
        })));
    };
    view.validate_current_state(tx, &state)?;
    let tasks = if tasks.is_some() {
        Some(view.read_current_tasks(tx, &state)?)
    } else {
        None
    };
    if let Some(runtime) = runtime {
        return Ok(Arc::new(runtime.for_task_status_metadata_recovery(tasks)?)
            .task_status_metadata_known());
    }
    Ok(KnownCommit::State {
        state,
        episodes,
        input_evidence,
        work,
        tasks,
        scheduling,
        runtime: None,
    })
}
