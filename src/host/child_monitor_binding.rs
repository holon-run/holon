//! The already accepted source binds through the current original-scope owner.
use super::*;
use crate::runtime::{mutation_witness::MutationWitness, OriginalChildTaskProducer};
use crate::runtime_db::{
    input_mutation_receipts::OriginalInputMutationAttempt,
    task_snapshot::TaskDomainLoadRequest,
    transitions::{InputMutationExecution, PreparedInputTransition},
    work_snapshot::WorkDomainLoadRequest,
};
impl RuntimeHostBridge {
    pub(crate) async fn commit_original_child_monitor_binding(
        &self,
        source: &Arc<OriginalChildTaskProducer>,
        pending: &mut Option<Arc<PreparedInputTransition>>,
        base: &TaskDomainLoadRequest,
    ) -> Result<bool> {
        let host = self.host()?;
        let scope = &source.address().scope;
        let ledger = &host.inner.input_evidence_budget;
        source.validate_material_ledger(ledger)?;
        tracing::debug!(
            task_id = scope_task_id(source),
            phase = "reserve",
            "original child monitor binding"
        );
        let mut reservations = host
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        let mut facts = Vec::new();
        let mut queries = Vec::new();
        let mut request = base.clone();
        if let Some(runtime) = self.runtime_for_mutation(&reservations[0]).await? {
            if let Some(query) = runtime.task_recovery_queries(
                &reservations[0],
                base.record_limit,
                base.byte_limit,
            )? {
                if let Some(selection) = query.selection_for(scope, reservations[0].owner_epoch()) {
                    request.query = selection.clone();
                    request
                        .capture
                        .required_keys
                        .extend(selection.required_keys.iter().cloned());
                }
                queries.push(query);
            }
            if let Some(root) = reservations[0].capture_ready_snapshot()? {
                facts.push(Arc::new(
                    runtime
                        .prepare_original_child_binding_facts(
                            &reservations[0],
                            root.state.clone(),
                            root.episodes.clone(),
                            root.input_evidence.clone(),
                            &mut request,
                        )
                        .await?,
                ));
            }
        }
        if pending.is_none() {
            let at = source.original_monitor_binding_clock(Utc::now())?;
            let prepared = host.runtime_db().transaction(|tx| {
                PreparedInputTransition::prepare_original_child_monitor_binding_tx(
                    tx, source, at, &request, ledger,
                )
            })?;
            let Some(prepared) = prepared else {
                return Ok(false);
            };
            *pending = Some(Arc::new(prepared));
        }
        let prepared = pending
            .as_ref()
            .context("original monitor binding capsule absent")?;
        anyhow::ensure!(
            prepared
                .original_child_monitor_binding()
                .is_some_and(|binding| &binding.original == source.address()),
            "binding retry changed original source"
        );
        let witness = MutationWitness::input_mutation(
            prepared.descriptor().command_nonce().into(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            Vec::new(),
        )?
        .with_input_runtime(facts)?
        .with_original_child_binding(source, prepared, &reservations[0])?;
        MutationReservation::begin_all_with_witness(
            &mut reservations,
            prepared.descriptor().command_nonce().into(),
            witness.clone(),
        )?;
        let work = WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit: request.record_limit,
            byte_limit: request.byte_limit,
        };
        let execution = host
            .runtime_db()
            .transitions_in_scope(scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                prepared, &work, ledger, &request,
            );
        if matches!(&execution, Ok(InputMutationExecution::Replayed { .. })) {
            witness.mark_input_storage_replayed()?;
        }
        witness.finish_original_attempt();
        let execution = match execution {
            Ok(value) => value,
            Err(error) => {
                if crate::runtime_db::persistence_certainty(&error)
                    == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
                {
                    MutationReservation::definitely_not_committed_all(&mut reservations)?;
                }
                return Err(error);
            }
        };
        let (receipt, _guards) = match execution {
            InputMutationExecution::Committed { value, receipt } => {
                witness.mark_original_input_receipt_pending(&receipt, false)?;
                (receipt, value.effects.task_material_budget_views)
            }
            InputMutationExecution::Replayed {
                receipt,
                _material_budget_views,
            } => {
                witness.mark_original_input_receipt_pending(&receipt, true)?;
                (receipt, _material_budget_views)
            }
        };
        anyhow::ensure!(
            matches!(&receipt.outcome.result,crate::runtime_db::input_mutation_receipts::InputMutationResult::Task{task_id,deletion:None,stop:None} if task_id.as_str()==scope_task_id(source)),
            "binding ACK changed its original Task result"
        );
        tracing::debug!(
            task_id = scope_task_id(source),
            phase = "recover",
            "original child monitor binding"
        );
        crate::runtime::input_recovery::resolve_input_recovery_with_queries(
            host.runtime_db(),
            &mut reservations,
            ledger,
            &host.runtime_context_config(),
            &RuntimeModelCatalog::from_config(&host.config()),
            &queries,
        )?;
        tracing::debug!(
            task_id = scope_task_id(source),
            phase = "publish",
            "original child monitor binding"
        );
        #[cfg(test)]
        if source.take_binding_lost_ack_for_test() {
            anyhow::bail!("original binding committed before publication test fault");
        }
        host.reconcile_external_mutations(&mut reservations).await?;
        tracing::debug!(
            task_id = scope_task_id(source),
            phase = "published",
            "original child monitor binding"
        );
        Ok(true)
    }
}
fn scope_task_id(source: &OriginalChildTaskProducer) -> &str {
    &source.address().admitted.key.id
}
