//! Already accepted Command producers reserve their original scope, including closing.
use super::*;
use crate::runtime::{
    FrozenCommandTaskTerminal, OriginalCommandTaskProducer, OriginalCommandTerminalRead,
};
use crate::runtime_db::{
    task_snapshot::TaskDomainLoadRequest, transitions::PreparedInputTransition,
};
impl RuntimeHostBridge {
    pub(crate) async fn commit_original_command_task_terminal(
        &self,
        source: &Arc<OriginalCommandTaskProducer>,
        frozen: &FrozenCommandTaskTerminal,
        pending: &mut Option<Arc<PreparedInputTransition>>,
        request: &TaskDomainLoadRequest,
    ) -> Result<OriginalCommandTerminalRead> {
        let host = self.host()?;
        let scope = &source.address().scope;
        let ledger = &host.inner.input_evidence_budget;
        let mut reservations = host
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        anyhow::ensure!(
            reservations.len() == 1,
            "Command original owner group changed"
        );
        let mut facts = vec![];
        let mut current_request = None;
        if let Some(runtime) = self.runtime_for_mutation(&reservations[0]).await? {
            current_request =
                Some(runtime.original_command_task_request(&reservations[0], request)?);
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
        let request = current_request
            .as_ref()
            .map_or(request, |current| &current.request);
        if pending.is_none() {
            let prepared = host.runtime_db().transaction(|tx| {
                PreparedInputTransition::prepare_original_command_terminal_tx(
                    tx, source, frozen, request, ledger,
                )
            })?;
            let prepared = crate::runtime::attach_original_command_terminal_memory_to_owner(
                host.runtime_db(),
                ledger,
                &reservations[0],
                prepared,
                Utc::now(),
            )?;
            *pending = Some(Arc::new(prepared));
        }
        let result = crate::runtime::execute_original_command_terminal(
            host.runtime_db(),
            &mut reservations,
            source,
            pending
                .as_deref()
                .context("Command original attempt missing")?,
            request,
            ledger,
            &host.runtime_context_config(),
            &RuntimeModelCatalog::from_config(&host.config()),
            facts,
            current_request
                .as_ref()
                .map_or(&[][..], |request| request.recovery_queries()),
        )?;
        host.reconcile_external_mutations(&mut reservations).await?;
        Ok(result)
    }
}
