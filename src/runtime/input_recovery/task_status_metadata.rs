//! Original metadata heads recover without hydrating unrelated domains.
use super::*;
use crate::runtime_db::{
    input_mutation_receipts::{InputMutationResult, PreparedInputMutation},
    task_snapshot::TaskDomainLoadRequest,
    task_status_metadata::{
        TaskStatusMetadataInstallMode, TaskStatusMetadataReadView, TaskStatusMetadataSparseRecovery,
    },
};

pub(super) struct PreparedTaskStatusKnown {
    original: PreparedInputMutation,
    known: KnownCommit,
    guards: Vec<Arc<super::super::input_evidence_budget::BudgetViewGuard>>,
}
#[derive(Debug)]
pub(in crate::runtime) struct TaskStatusMetadataKnown {
    proof: TaskStatusMetadataSparseRecovery,
    known: KnownCommit,
    _guards: Vec<Arc<super::super::input_evidence_budget::BudgetViewGuard>>,
}
impl PreparedTaskStatusKnown {
    pub(super) fn finish(
        self,
        proof: TaskStatusMetadataSparseRecovery,
    ) -> Result<TaskStatusMetadataKnown> {
        ensure!(
            self.original == proof.receipt().prepared,
            "Status recovery receipt changed"
        );
        Ok(TaskStatusMetadataKnown {
            proof,
            known: self.known,
            _guards: self.guards,
        })
    }
}
impl TaskStatusMetadataKnown {
    pub(in crate::runtime) fn proof(&self) -> &TaskStatusMetadataSparseRecovery {
        &self.proof
    }
    pub(in crate::runtime) fn known(&self) -> KnownCommit {
        self.known.clone()
    }
}
pub(super) fn prepare_known_tx(
    tx: &rusqlite::Transaction<'_>,
    view: &TaskStatusMetadataReadView<'_, '_>,
    witness: &super::super::mutation_witness::MutationWitness,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PreparedTaskStatusKnown> {
    let scope = &view.receipt().prepared.scopes()[0];
    view.validate_tx(tx, scope)?;
    let InputMutationResult::TaskStatusMetadata { ack } = &view.receipt().outcome.result else {
        anyhow::bail!("Status recovery lacks its original ACK")
    };
    let outcome = &view.receipt().outcome.scopes[0];
    let mut expected_keys =
        crate::runtime_db::input_recovery_install::InputMutationInstallKeys::default();
    expected_keys.tasks = outcome.install_keys.tasks.clone();
    ensure!(
        outcome.install_keys == expected_keys
            && outcome.audit_events.is_empty()
            && !outcome.notify_memory_index
            && !outcome.notify_scheduler,
        "Status metadata recovery has ordinary domain effects"
    );
    let mut guards = Vec::new();
    let known = if view.mode() != TaskStatusMetadataInstallMode::Current {
        KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            identity: view.identity().cloned().map(Arc::new),
            state: None,
        }))
    } else {
        let original = witness
            .input_runtime(&scope.agent_id)
            .map(|facts| facts.task_status_metadata_known())
            .or_else(|| witness.input_task_status_parent().cloned());
        match original {
            Some(KnownCommit::State {
                state,
                episodes,
                input_evidence,
                work,
                tasks,
                scheduling,
                runtime,
            }) => {
                let tasks = if let Some(tasks) = tasks {
                    let (delta, material) =
                        crate::runtime_db::task_snapshot::read_task_status_metadata_recovery_tx(
                            tx,
                            view,
                            scope,
                            tasks.coverage(),
                            request,
                            ledger,
                            witness.input_storage_replayed(),
                        )?;
                    guards.extend(material);
                    Some(Arc::new(tasks.with_committed(&delta)?))
                } else {
                    ensure!(
                        witness.input_storage_replayed() || ack.task_revision.is_none(),
                        "Status ACK changed Task without original prepared view"
                    );
                    None
                };
                if let Some(runtime) = runtime {
                    Arc::new(runtime.for_task_status_metadata_recovery(tasks)?)
                        .task_status_metadata_known()
                } else {
                    KnownCommit::State {
                        state,
                        episodes,
                        input_evidence,
                        work,
                        tasks,
                        scheduling,
                        runtime: None,
                    }
                }
            }
            None => {
                ensure!(
                    witness.input_storage_replayed() || ack.task_revision.is_none(),
                    "cold Status ACK changed unobserved Task"
                );
                KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                    identity: view.identity().cloned().map(Arc::new),
                    state: None,
                }))
            }
            Some(KnownCommit::Metadata(_)) => {
                anyhow::bail!("Status original state observation is invalid")
            }
        }
    };
    Ok(PreparedTaskStatusKnown {
        original: view.receipt().prepared.clone(),
        known,
        guards,
    })
}
