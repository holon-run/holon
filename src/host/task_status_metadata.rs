//! Already owned original Control completion may publish through the current owner.
use super::*;
use crate::runtime::{
    execute_original_control_metadata, select_original_control_status_head,
    OriginalControlMetadataCapsule, TaskStatusMetadataRead,
};
use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;
impl RuntimeHostBridge {
    pub(crate) async fn commit_original_control_worktree_metadata(
        &self,
        capsule: &mut OriginalControlMetadataCapsule,
        request: &TaskDomainLoadRequest,
    ) -> Result<TaskStatusMetadataRead> {
        let host = self.host()?;
        let scope = capsule.scope();
        let mut reservations = host
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        anyhow::ensure!(
            reservations.len() == 1,
            "Control metadata original group changed"
        );
        let mut facts = Vec::new();
        if let Some(runtime) = self.runtime_for_mutation(&reservations[0]).await? {
            if let Some(root) = reservations[0].capture_ready_snapshot()? {
                facts.push(Arc::new(
                    runtime
                        .prepare_external_input_facts(
                            &reservations[0],
                            root.state.clone(),
                            root.episodes.clone(),
                            root.input_evidence.clone(),
                        )
                        .await?,
                ));
            }
        }
        let ledger = &host.inner.input_evidence_budget;
        let selected = select_original_control_status_head(
            &reservations[0],
            capsule.task_id(),
            request,
            ledger,
        )?;
        let prepared = capsule.prepare(selected, None, request)?;
        let result = execute_original_control_metadata(
            host.runtime_db(),
            &mut reservations,
            prepared,
            request,
            ledger,
            &host.runtime_context_config(),
            &RuntimeModelCatalog::from_config(&host.config()),
            facts,
        )?;
        host.reconcile_external_mutations(&mut reservations).await?;
        Ok(result)
    }
}
