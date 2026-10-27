//! Only the existing accepted source can register this finite Host worker.
use super::*;
use crate::runtime::OriginalChildTaskProducer;
impl RuntimeHostBridge {
    pub(crate) async fn start_original_child_spawn(
        &self,
        runtime: RuntimeHandle,
        source: Arc<OriginalChildTaskProducer>,
    ) -> Result<()> {
        let host = self.host()?;
        source.validate_material_ledger(&host.inner.input_evidence_budget)?;
        anyhow::ensure!(
            runtime.instance_incarnation() == source.address().scope.identity_incarnation
                && runtime.instance_agent_id() == source.address().scope.agent_id
                && runtime.instance_owner_epoch() == source.address().owner_epoch,
            "original spawn runtime scope differs"
        );
        let spawn = source
            .original_child_spawn()
            .context("owned child spawn plan absent")?;
        let _registration = spawn.registration.lock().await;
        if source.original_child_spawn_registered() || source.original_child_spawn_completed() {
            #[cfg(test)]
            source
                .wait_original_spawn_registration_return_gate_for_test()
                .await?;
            return Ok(());
        }
        let worker = source.clone();
        let claim = source.clone();
        // Reuse the original Open fence and external owned-worker registry.
        // Claim is synchronous under that fence, before the first kernel await.
        let result = host
            .register_owned_external_mutation(
                async move {
                    let outcome = runtime
                        .perform_original_child_spawn_kernel(worker.clone())
                        .await;
                    worker.record_original_child_spawn_completion(outcome)
                },
                move || claim.claim_original_child_spawn_registration(),
            )
            .await;
        match result {
            Ok(result) => {
                drop(result);
                #[cfg(test)]
                source
                    .wait_original_spawn_registration_return_gate_for_test()
                    .await?;
                Ok(())
            }
            Err(error) if source.original_child_spawn_cancellation_error(&error) => Err(error),
            Err(error) => source.record_original_child_spawn_completion(Err(error)),
        }
    }
    pub(crate) async fn spawn_child_task_for_original_source(
        &self,
        parent_runtime: RuntimeHandle,
        source: &OriginalChildTaskProducer,
        task: &TaskRecord,
        prompt: String,
        authority: AuthorityClass,
        worktree: bool,
        model: AgentModelResolution,
    ) -> Result<ChildTaskSpawn> {
        let host = self.host()?;
        source.validate_material_ledger(&host.inner.input_evidence_budget)?;
        anyhow::ensure!(
            source.original_child_spawn_registered(),
            "original spawn has no registered Host worker"
        );
        source
            .validate_original_child_spawn_command(task, &prompt, &authority, worktree, &model)?;
        host.spawn_child_task_inner(
            parent_runtime,
            task,
            prompt,
            authority,
            worktree,
            None,
            model,
            Some(source),
        )
        .await
    }
}

impl RuntimeHostBridge {
    pub(crate) async fn handoff_original_child_spawn_without_start(
        &self,
        source: &Arc<OriginalChildTaskProducer>,
        token: &crate::runtime::OriginalChildSpawnNoStart,
        request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
    ) -> Result<bool> {
        use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;
        let host = self.host()?;
        let ledger = &host.inner.input_evidence_budget;
        source.validate_material_ledger(ledger)?;
        anyhow::ensure!(
            token.matches_original(source),
            "unstarted Host handoff crossed source"
        );
        let (record_limit, byte_limit) = TaskDomainLoadRequest::bounded_material_limits(
            request.record_limit,
            request.byte_limit,
        );
        let bounded = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit,
            byte_limit,
        };
        let scope = &source.address().scope;
        let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
            source.address(),
        )?)?
        .checked_add(512)
        .and_then(|n| n.checked_mul(8))
        .context("unstarted owner slots overflow")?;
        let _slots = crate::runtime_db::command_task_terminal::charge_material(
            scope, bytes, 8, &bounded, ledger,
        )?;
        let remaining = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit: record_limit
                .checked_sub(8)
                .context("unstarted owner count exceeds bound")?,
            byte_limit: byte_limit
                .checked_sub(bytes)
                .context("unstarted owner bytes exceed bound")?,
        };
        let held = host
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        let proof=host.runtime_db().transaction(|tx|crate::runtime_db::child_task_terminal::observe_original_child_spawn_no_start_handoff_tx(tx,source,token,&remaining,ledger))?;
        let Some(proof) = proof else { return Ok(false) };
        source.acknowledge_unstarted_child_handoff(token, &proof)?;
        drop(held);
        Ok(true)
    }
}
