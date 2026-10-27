//! Original producer attempts retain one nonce/body until whole publication.
use super::super::*;
use super::producer_maintenance::OriginalCommandTaskProducer;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate, PendingAdmission,
};
use crate::runtime::mutation_owner::MutationReservation;
use crate::runtime_db::{
    command_task_terminal::{CommandTaskTerminalAck, CommandTaskTerminalInstallMode},
    input_mutation_receipts::{InputMutationResult, OriginalInputMutationAttempt},
    task_snapshot::TaskDomainLoadRequest,
    transitions::{InputMutationExecution, PostCommitEffects, PreparedInputTransition},
    work_snapshot::WorkDomainLoadRequest,
};
use std::sync::Arc;

pub(crate) struct OriginalCommandTerminalRead {
    pub(crate) ack: Box<CommandTaskTerminalAck>,
    _material: Arc<BudgetViewGuard>,
}
/// Only the original source's finished synchronous attempt may create a Known
/// receipt. The caller installs the whole group before acknowledging the capsule.
pub(crate) fn execute_original_command_terminal(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    source: &Arc<OriginalCommandTaskProducer>,
    prepared: &PreparedInputTransition,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    config: &ContextConfig,
    models: &RuntimeModelCatalog,
    runtime_facts: Vec<Arc<RuntimeMutationFacts>>,
    task_queries: &[super::super::task_queries::TaskRecoveryQueries],
) -> Result<OriginalCommandTerminalRead> {
    let scope = &source.address().scope;
    anyhow::ensure!(
        reservations.len() == 1
            && reservations[0].agent_id() == scope.agent_id
            && reservations[0].identity_incarnation() == scope.identity_incarnation
            && prepared
                .original_command_terminal()
                .is_some_and(|intent| &intent.original == source.address()),
        "Command terminal original group differs"
    );
    let mut pending = Vec::new();
    for message in prepared.pending_messages()? {
        match ledger.reserve_pending(scope, &message.id, serialized_bytes(&message.body)?) {
            Ok(PendingAdmission::Reserved(ticket)) => pending.push((ledger.clone(), ticket)),
            Ok(PendingAdmission::ExistingAccepted) => {}
            Err(error) => {
                for (ledger, ticket) in &pending {
                    ledger.definitely_not_committed(ticket)?;
                }
                return Err(error.into());
            }
        }
    }
    let operation = prepared.descriptor().command_nonce().to_owned();
    let cleanup = pending.clone();
    let witness = match super::super::mutation_witness::MutationWitness::input_mutation(
        operation.clone(),
        OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
        pending,
    )
    .and_then(|witness| witness.with_input_runtime(runtime_facts))
    .and_then(|witness| witness.with_original_command_producer(source, prepared))
    {
        Ok(witness) => witness,
        Err(error) => {
            for (ledger, ticket) in &cleanup {
                ledger.definitely_not_committed(ticket)?;
            }
            return Err(error);
        }
    };
    if let Err(error) =
        MutationReservation::begin_all_with_witness(reservations, operation, witness.clone())
    {
        witness.release_pending_not_committed(false)?;
        return Err(error.into());
    }
    if let Err(error) = witness.mark_pending_started() {
        witness.finish_original_attempt();
        witness.release_pending_not_committed(false)?;
        MutationReservation::definitely_not_committed_all(reservations)?;
        return Err(error);
    }
    let work = WorkDomainLoadRequest {
        capture: super::super::resident_work::WorkCaptureRequest::current(),
        record_limit: request.record_limit,
        byte_limit: request.byte_limit,
    };
    let execution = db
        .transitions_in_scope(scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            prepared, &work, ledger, request,
        );
    if matches!(&execution, Ok(InputMutationExecution::Replayed { .. })) {
        witness.mark_input_storage_replayed()?;
    }
    witness.finish_original_attempt();
    let execution = match execution {
        Ok(execution) => execution,
        Err(error) => {
            if crate::runtime_db::persistence_certainty(&error)
                == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
            {
                witness.release_pending_not_committed(false)?;
                MutationReservation::definitely_not_committed_all(reservations)?;
            } else {
                witness.mark_pending_unknown()?;
            }
            return Err(error);
        }
    };
    let (receipt, _guards, fault) = match execution {
        InputMutationExecution::Committed { value, receipt } => {
            witness.mark_original_input_receipt_pending(&receipt, false)?;
            (
                receipt,
                value.effects.task_material_budget_views,
                value.effects.fault,
            )
        }
        InputMutationExecution::Replayed {
            receipt,
            _material_budget_views,
        } => {
            witness.mark_original_input_receipt_pending(&receipt, true)?;
            (receipt, _material_budget_views, None)
        }
    };
    let InputMutationResult::CommandTaskTerminal { ack } = &receipt.outcome.result else {
        anyhow::bail!("Command terminal whole ACK returned another family")
    };
    anyhow::ensure!(
        &ack.original == source.address(),
        "Command terminal ACK returned another original lease"
    );
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: serialized_bytes(ack.as_ref())?,
        },
    )?;
    let read = OriginalCommandTerminalRead {
        ack: ack.clone(),
        _material: guard,
    };
    super::super::input_recovery::resolve_input_recovery_with_queries(
        db,
        reservations,
        ledger,
        config,
        models,
        task_queries,
    )?;
    if fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
        anyhow::bail!("original Command terminal committed but publication requires reconciliation")
    }
    Ok(read)
}

impl RuntimeHandle {
    /// The registered monitor and retirement drainer share this same capsule.
    pub(in crate::runtime) async fn continue_original_command_terminal(
        &self,
        source: Arc<OriginalCommandTaskProducer>,
    ) -> Result<()> {
        if source.is_durable_finished() {
            return Ok(());
        }
        let frozen = source
            .frozen_terminal()?
            .context("Command producer has no frozen terminal responsibility")?;
        let mut pending = source.prepared.lock().await;
        if source.is_durable_finished() {
            return Ok(());
        }
        let limits = self.inner.input_evidence_budget.limits();
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: super::super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Command request limits overflow")?,
            )?,
        };
        let result = if let Some(bridge) = &self.inner.host_bridge {
            bridge
                .commit_original_command_task_terminal(&source, &frozen, &mut pending, &request)
                .await
        } else {
            let mut reservations = vec![self.inner.mutation_owner.reserve().await?];
            self.reconcile_reserved_mutations(&mut reservations).await?;
            let live_request = self.original_command_task_request(&reservations[0], &request)?;
            let request = &live_request.request;
            if pending.is_none() {
                let prepared = self.inner.runtime_db.transaction(|tx| {
                    PreparedInputTransition::prepare_original_command_terminal_tx(
                        tx,
                        &source,
                        &frozen,
                        request,
                        &self.inner.input_evidence_budget,
                    )
                })?;
                let prepared =
                    super::super::state_mutation::attach_original_command_terminal_memory_to_owner(
                        &self.inner.runtime_db,
                        &self.inner.input_evidence_budget,
                        &reservations[0],
                        prepared,
                        self.now(),
                    )?;
                *pending = Some(Arc::new(prepared));
            }
            let version = self.inner.config_snapshot.load_full();
            let result = execute_original_command_terminal(
                &self.inner.runtime_db,
                &mut reservations,
                &source,
                pending.as_deref().unwrap(),
                request,
                &self.inner.input_evidence_budget,
                &version.base_context_config,
                &version.model_catalog,
                vec![],
                live_request.recovery_queries(),
            );
            match result {
                Ok(result) => {
                    self.reconcile_reserved_mutations(&mut reservations).await?;
                    Ok(result)
                }
                Err(error) => Err(error),
            }
        };
        let read = match result {
            Ok(read) => read,
            Err(error) => {
                if crate::runtime_db::persistence_certainty(&error)
                    == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
                {
                    pending.take();
                }
                return Err(error);
            }
        };
        // Sparse metadata has no runtime facts to reissue its original audit/index
        // responsibility. The capsule retains that exact receipt across lost ACK.
        if let Some(notifications) = source.take_sparse_notifications()? {
            let outcome = &notifications.receipt.outcome.scopes[0];
            let effects = PostCommitEffects {
                audit_events: outcome.audit_events.clone(),
                notify_memory_index: outcome.notify_memory_index,
                ..Default::default()
            };
            for warning in self
                .inner
                .storage
                .publish_transition_events(&effects)
                .into_iter()
                .chain(self.inner.storage.notify_transition_memory_index(&effects))
            {
                tracing::warn!(task_id=%source.address().admitted.key.id,effect=warning.effect,error=%warning.message,"Command terminal committed with post-commit warning");
            }
        }
        pending.take();
        source.acknowledge(&read.ack)?;
        self.release_completed_task_handle_exact(
            &source.address().admitted.key.id,
            &source.address().lease_id,
        )
        .await;
        Ok(())
    }
    pub(super) async fn commit_original_command_terminal(
        &self,
        source: &Arc<OriginalCommandTaskProducer>,
        task: &TaskRecord,
        message: Option<&MessageEnvelope>,
    ) -> Result<()> {
        if source.is_durable_finished() {
            return Ok(());
        }
        source.freeze_terminal(
            task,
            message,
            &self.inner.input_evidence_budget,
            self.take_transition_fault(),
        )?;
        self.continue_original_command_terminal(source.clone())
            .await
    }
    pub(super) async fn prepare_original_command_running(
        &self,
        source: &Arc<OriginalCommandTaskProducer>,
        running: &TaskRecord,
    ) -> Result<()> {
        if !self.accepts_execution_admission() {
            return Ok(());
        }
        let limits = self.inner.input_evidence_budget.limits();
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: super::super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Command running limits overflow")?,
            )?,
        };
        let observation = self.inner.runtime_db.transaction(|tx| {
            crate::runtime_db::command_task_terminal::observe_original_owner_tx(
                tx,
                &source.address().scope,
                &request,
                &self.inner.input_evidence_budget,
            )
        })?;
        if observation.mode != CommandTaskTerminalInstallMode::Current
            || observation.identity.as_ref().is_some_and(|identity| {
                identity.status == crate::types::AgentRegistryStatus::Deleting
            })
        {
            // Admission is already closed. Keep the accepted process/terminal
            // obligation without manufacturing a new active Task write.
            return Ok(());
        }
        let mut owner = match self.begin_mutation().await {
            Ok(owner) => owner,
            Err(error)
                if matches!(
                    error.downcast_ref::<super::super::mutation_owner::MutationOwnerError>(),
                    Some(super::super::mutation_owner::MutationOwnerError::Replaced)
                ) && !self.accepts_execution_admission() =>
            {
                return Ok(())
            }
            Err(error) => return Err(error),
        };
        let mut required = super::super::resident_task::TaskCaptureRequest::current();
        required
            .required_keys
            .insert(super::super::resident_task::TaskKey::new(
                super::super::resident_task::TaskFamily::Task,
                &running.id,
            ));
        owner.prepare_owner_task_request(&required).await?;
        let selected = owner.capture_tasks(&required)?;
        let actual = selected
            .lookup(&super::super::resident_task::TaskKey::new(
                super::super::resident_task::TaskFamily::Task,
                &running.id,
            ))?
            .context("original Command Task absent")?;
        let super::super::resident_task::CanonicalTaskFact::Task(actual) = &actual.fact else {
            unreachable!()
        };
        {
            let view = source.source_view()?;
            view.validate_original_generation(actual)?;
            view.validate_original_generation(running)?;
        }
        if !matches!(actual.status, TaskStatus::Queued | TaskStatus::Running) {
            return Ok(());
        }
        let command = crate::runtime_db::transitions::TaskTransitionCommand {
            agent_id: running.agent_id.clone(),
            expected_owner_incarnation: self.inner.identity_incarnation,
            task: running.clone(),
            agent_deletion: None,
            task_result_settlement: None,
            queue_entry: None,
            work_items: vec![],
            expected_wait_conditions: vec![],
            wait_conditions: vec![],
            agent_state: None,
            message_evidence: vec![],
            audit_events: vec![AuditEvent::legacy(
                "task_status_updated",
                serde_json::to_value(crate::types::TaskLifecycleAuditEvent::from_task(running))?,
            )],
            index_changes: self.inner.storage.index_changes_for_task(running)?,
            notify_scheduler: false,
            commit_on_idempotent: false,
            fault: self.take_transition_fault(),
        };
        let (commit, _) = owner
            .commit_task_transition(
                format!("command-running:{}", source.address().lease_id),
                command,
                Default::default(),
                true,
            )
            .await?;
        drop(owner);
        self.apply_transition_commit(commit).await;
        Ok(())
    }
}

pub(crate) struct OriginalCommandTaskRequest {
    pub(crate) request: TaskDomainLoadRequest,
    queries: Option<super::super::task_queries::TaskRecoveryQueries>,
}
impl OriginalCommandTaskRequest {
    pub(crate) fn recovery_queries(&self) -> &[super::super::task_queries::TaskRecoveryQueries] {
        self.queries.as_slice()
    }
}
impl RuntimeHandle {
    pub(crate) fn original_command_task_request(
        &self,
        reservation: &MutationReservation,
        base: &TaskDomainLoadRequest,
    ) -> Result<OriginalCommandTaskRequest> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        let queries =
            self.task_recovery_queries(reservation, base.record_limit, base.byte_limit)?;
        let mut request = base.clone();
        request.query = queries
            .as_ref()
            .and_then(|queries| {
                queries.selection_for_owner(
                    reservation.agent_id(),
                    reservation.identity_incarnation(),
                    reservation.owner_epoch(),
                )
            })
            .cloned()
            .unwrap_or_default();
        request
            .capture
            .required_keys
            .extend(request.query.required_keys.iter().cloned());
        Ok(OriginalCommandTaskRequest { request, queries })
    }
}
