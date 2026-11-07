//! The real registered child monitor and retirement drainer share one original capsule.
use super::*;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, PendingAdmission,
};
use crate::runtime::mutation_owner::MutationReservation;
use crate::runtime_db::{
    child_task_terminal::ChildTaskTerminalAck,
    input_mutation_receipts::{InputMutationResult, OriginalInputMutationAttempt},
    task_snapshot::TaskDomainLoadRequest,
    transitions::{InputMutationExecution, PostCommitEffects, PreparedInputTransition},
    work_snapshot::WorkDomainLoadRequest,
};
use std::sync::Arc;
pub(crate) struct OriginalChildTerminalRead {
    pub(crate) ack: Box<ChildTaskTerminalAck>,
    _material: Arc<BudgetViewGuard>,
}
/// Only the original source's finished synchronous attempt may create a Known
/// receipt. The caller installs the whole group before acknowledging the capsule.
pub(crate) fn execute_original_child_terminal(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    source: &Arc<OriginalChildTaskProducer>,
    prepared: &PreparedInputTransition,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    config: &ContextConfig,
    models: &RuntimeModelCatalog,
    runtime_facts: Vec<Arc<RuntimeMutationFacts>>,
    task_queries: &[super::task_queries::TaskRecoveryQueries],
) -> Result<OriginalChildTerminalRead> {
    let scope = &source.address().scope;
    anyhow::ensure!(
        reservations.len() == prepared.descriptor().scopes().len()
            && reservations.iter().any(|r| r.agent_id() == scope.agent_id
                && r.identity_incarnation() == scope.identity_incarnation)
            && prepared
                .original_child_terminal()
                .is_some_and(|intent| &intent.original == source.address()),
        "child terminal original group differs"
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
    let witness = match super::mutation_witness::MutationWitness::input_mutation(
        operation.clone(),
        OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
        pending,
    )
    .and_then(|witness| witness.with_input_runtime(runtime_facts))
    .and_then(|witness| witness.with_original_child_producer(source, prepared, reservations))
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
        capture: super::resident_work::WorkCaptureRequest::current(),
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
    let InputMutationResult::ChildTaskTerminal { ack } = &receipt.outcome.result else {
        anyhow::bail!("child terminal whole ACK returned another family")
    };
    anyhow::ensure!(
        &ack.original == source.address(),
        "child terminal ACK returned another original lease"
    );
    let guard = crate::runtime_db::command_task_terminal::charge_material(
        scope,
        usize::try_from(serialized_bytes(ack.as_ref())?)?
            .checked_mul(8)
            .context("child ACK material overflow")?,
        ack.status_scopes
            .len()
            .checked_add(8)
            .and_then(|n| n.checked_mul(8))
            .context("child ACK count overflow")?,
        request,
        ledger,
    )?;
    let read = OriginalChildTerminalRead {
        ack: ack.clone(),
        _material: guard,
    };
    super::input_recovery::resolve_input_recovery_with_queries(
        db,
        reservations,
        ledger,
        config,
        models,
        task_queries,
    )?;
    if fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
        anyhow::bail!("original child terminal committed but publication requires reconciliation")
    }
    Ok(read)
}

impl RuntimeHandle {
    pub(in crate::runtime) async fn admit_original_child_monitor(
        &self,
        task: &TaskRecord,
    ) -> Result<(
        super::state_mutation::RuntimeMutation,
        Arc<super::control_cancellation::ControlJoinLease>,
        Arc<OriginalChildTaskProducer>,
    )> {
        let mut registration = self.admit_control_task_handle_registration(task).await?;
        let limits = self.inner.input_evidence_budget.limits();
        let scope = registration.input_evidence_scope();
        let limits_request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("child source limits overflow")?,
            )?,
        };
        let _selector = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            task.id
                .len()
                .checked_add(256)
                .and_then(|n| n.checked_mul(8))
                .context("child source selector overflow")?,
            8,
            &limits_request,
            &self.inner.input_evidence_budget,
        )?;
        let mut capture = super::resident_task::TaskCaptureRequest::current();
        capture
            .required_keys
            .insert(super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::Task,
                &task.id,
            ));
        registration.prepare_owner_task_request(&capture).await?;
        let selected = registration.capture_tasks(&capture)?;
        let fact = selected
            .lookup(&super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::Task,
                &task.id,
            ))?
            .context("child registration lacks its qualified original Task")?;
        let lease = self.inner.control_cancellations.task(task);
        let source = match OriginalChildTaskProducer::admitted(
            &registration,
            fact,
            task,
            &lease,
            self.inner.mutation_owner.owner_epoch(),
            &self.inner.input_evidence_budget,
        ) {
            Ok(source) => source,
            Err(error) => {
                self.inner
                    .control_cancellations
                    .abandon_unattached_child(&lease)?;
                return Err(error);
            }
        };
        lease.install_original_child(source.clone())?;
        #[cfg(test)]
        source.arm_original_terminal_conflicts(
            self.inner
                .terminal_task_transition_conflicts_remaining
                .swap(0, Ordering::SeqCst),
        );
        Ok((registration, lease, source))
    }
    pub(in crate::runtime) fn bind_original_child_monitor_target(
        &self,
        source: &OriginalChildTaskProducer,
        child: &str,
        delivery: Option<&str>,
        baseline: u64,
    ) -> Result<()> {
        let limits = self.inner.input_evidence_budget.limits();
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("child monitor limits overflow")?,
            )?,
        };
        let target = self.inner.runtime_db.transaction(|tx| {
            crate::runtime_db::child_task_terminal::bind_original_child_target_tx(
                tx,
                source,
                child,
                delivery,
                baseline,
                &request,
                &self.inner.input_evidence_budget,
            )
        })?;
        source.validate_monitor_target_incarnation(target.target_scope())?;
        source.install_target(target)
    }
    pub(in crate::runtime) async fn complete_observed_child_monitor(
        &self,
        source: Arc<OriginalChildTaskProducer>,
        task: TaskRecord,
        mut message: MessageEnvelope,
        child: Option<String>,
        deletion: Option<crate::runtime_db::repositories::AgentDeletionRequest>,
        worktree: bool,
    ) -> Result<()> {
        message.task_id = Some(task.id.clone());
        message.work_item_id = task.work_item_id.clone();
        source.observe_terminal_result(task, message, child, deletion, worktree)?;
        // The observation belongs to the already registered original lease.
        // If its mutation registry has closed, do not create a new worker: the
        // monitor continues inline and its drainer retains the same capsule.
        match self.register_original_child_completion_if_open(source.clone())? {
            Some(result) => result
                .await
                .context("original child owned completion channel closed")?,
            None => self.continue_original_child_terminal(source).await,
        }
    }
    /// Registration is synchronous and precedes cleanup/SQL's first await.
    /// Caller cancellation leaves the exact observed capsule and owned work.
    #[cfg(test)]
    pub(in crate::runtime) fn register_original_child_completion(
        &self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<()>>> {
        self.register_original_child_completion_if_open(source)?
            .ok_or_else(super::execution_admission::closed_error)
    }
    fn register_original_child_completion_if_open(
        &self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> Result<Option<tokio::sync::oneshot::Receiver<Result<()>>>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        let original = source.clone();
        let registered = self.try_register_owned_original_child_terminal(&source, async move {
            let result = runtime.continue_original_child_terminal(original).await;
            let _ = sender.send(result);
        })?;
        Ok(registered.then_some(receiver))
    }
    pub(in crate::runtime) fn continue_original_child_terminal<'a>(
        &'a self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        // Break the Host drain/child completion Send cycle at its owned continuation.
        Box::pin(self.continue_original_child_terminal_inner(source))
    }
    async fn continue_original_child_terminal_inner(
        &self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> Result<()> {
        if let Err(error) = self.continue_original_child_spawn(source.clone()).await {
            if source.original_child_spawn_cancellation_error(&error)
                && self
                    .try_handoff_original_child_spawn_without_start(source.clone())
                    .await?
            {
                return Ok(());
            }
            return Err(error);
        }
        if source.is_durable_finished() {
            return Ok(());
        }
        if source.observed_terminal().is_none() && source.monitor_stage().is_some() {
            return self.continue_original_child_monitor(source).await;
        }
        let mut pending = source.prepared.lock().await;
        if source.is_durable_finished() {
            return Ok(());
        }
        let limits = self.inner.input_evidence_budget.limits();
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("child terminal limits overflow")?,
            )?,
        };
        if source.frozen_terminal()?.is_none() {
            if let Some(failure) = source.failed_spawn() {
                let target_missing = {
                    let view = source.source_view()?;
                    view.target().is_none()
                };
                if target_missing {
                    let target=self.inner.runtime_db.transaction(|tx|crate::runtime_db::child_task_terminal::bind_failed_original_child_target_tx(tx,&source,&failure,&request,&self.inner.input_evidence_budget))?;
                    source.install_target(target)?;
                }
            }
            let completed = match source.completed_terminal_observation() {
                Some(completed) => completed,
                None => {
                    let observed = source
                        .observed_terminal()
                        .context("child producer has no actual terminal observation")?;
                    let completion = source.completion_material()?;
                    // Both clones and all derived cleanup output are covered by
                    // the reservation acquired at original lease registration.
                    completion.observe_material(
                        serialized_bytes(&(&observed.task, &observed.message))
                            .unwrap_or(u64::MAX)
                            .saturating_mul(8),
                        1024,
                    );
                    let mut task = observed.task.clone();
                    let mut message = observed.message.clone();
                    let mut audits = Vec::new();
                    if observed.worktree {
                        let owner = self.inner.runtime_db.transaction(|tx| {
                            crate::runtime_db::command_task_terminal::observe_original_owner_tx(
                                tx,
                                &source.address().scope,
                                &request,
                                &self.inner.input_evidence_budget,
                            )
                        })?;
                        if owner.mode==crate::runtime_db::command_task_terminal::CommandTaskTerminalInstallMode::Current {
                            if let Some(detail)=task.detail.as_mut() {
                                let path=detail.get("worktree").and_then(|v|v.get("worktree_path")).and_then(serde_json::Value::as_str).map(PathBuf::from);
                                if let Ok(Some(cleanup))=self.cleanup_original_control_worktree_in_detail(&task.id,detail,"terminal_task_result",completion.clone(),&request).await {
                                    if let (Some(path),MessageBody::Text{text})=(path,message.body.clone()) {
                                        let mut text=text;super::tasks::append_task_owned_worktree_cleanup_note(&mut text,&cleanup.cleanup,&path);
                                        message.body=MessageBody::Text{text};
                                    }
                                    audits=cleanup.audits;
                                }
                            }
                        }
                    }
                    completion.observe_material(
                        serialized_bytes(&message.body)
                            .unwrap_or(u64::MAX)
                            .saturating_mul(8),
                        1024,
                    );
                    if let (Some(detail), MessageBody::Text { text }) =
                        (task.detail.as_mut(), &message.body)
                    {
                        detail["output_summary"] = serde_json::json!(text);
                        if let Some(metadata) = message
                            .metadata
                            .as_mut()
                            .and_then(serde_json::Value::as_object_mut)
                        {
                            if let Some(worktree) = detail.get("worktree") {
                                metadata.insert("worktree".into(), worktree.clone());
                            }
                        }
                    }
                    // Move the actual completed cleanup outcome before any next
                    // fallible encode, source lookup, owner acquisition or SQL.
                    source.save_completed_terminal_observation(
                        task,
                        message,
                        observed.child_id.clone(),
                        observed.deletion.clone(),
                        audits,
                        completion,
                    )
                }
            };
            source.freeze_terminal(
                &completed.task,
                &completed.message,
                completed.child_id.as_deref(),
                completed.deletion.as_ref(),
                &completed.audits,
                self.take_transition_fault(),
            )?;
        }
        let frozen = source
            .frozen_terminal()?
            .context("child actual terminal body missing")?;
        let mut occ_attempts = 0;
        let result = loop {
            let result = if let Some(bridge) = &self.inner.host_bridge {
                bridge
                    .commit_original_child_task_terminal(&source, &frozen, &mut pending, &request)
                    .await
            } else {
                anyhow::ensure!(
                    frozen.child_id.is_none(),
                    "actual child target requires its Host owner group"
                );
                let mut reservations = vec![self.inner.mutation_owner.reserve().await?];
                self.reconcile_reserved_mutations(&mut reservations).await?;
                let live = self.original_command_task_request(&reservations[0], &request)?;
                let request = &live.request;
                if pending.is_none() {
                    let prepared=self.inner.runtime_db.transaction(|tx|{
                    let inventory=crate::runtime_db::child_task_terminal::discover_original_child_observers_tx(tx,&source,request,&self.inner.input_evidence_budget)?;
                    PreparedInputTransition::prepare_original_child_terminal_tx(tx,&source,&frozen,inventory,Vec::new(),None,request,&self.inner.input_evidence_budget)
                })?;
                    let prepared = super::state_mutation::attach_original_child_memory_to_owner(
                        &self.inner.runtime_db,
                        &self.inner.input_evidence_budget,
                        &reservations[0],
                        prepared,
                        self.now(),
                    )?;
                    *pending = Some(Arc::new(prepared));
                }
                let version = self.inner.config_snapshot.load_full();
                let read = execute_original_child_terminal(
                    &self.inner.runtime_db,
                    &mut reservations,
                    &source,
                    pending.as_deref().unwrap(),
                    request,
                    &self.inner.input_evidence_budget,
                    &version.base_context_config,
                    &version.model_catalog,
                    Vec::new(),
                    live.recovery_queries(),
                );
                match read {
                    Ok(read) => {
                        self.reconcile_reserved_mutations(&mut reservations).await?;
                        Ok(read)
                    }
                    Err(error) => Err(error),
                }
            };
            match result {
                Err(error) if crate::runtime_db::retryable_state_transition_conflict(&error).is_some()
                    && crate::runtime_db::persistence_certainty(&error)
                        == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted) => {
                    // Refresh only CAS/material after the rolled-back whole
                    // attempt dropped its owners and released pending tickets.
                    pending.take();
                    occ_attempts += 1;
                    if occ_attempts == super::task_state_reducer::TASK_TRANSITION_MAX_ATTEMPTS {
                        break Err(super::task_state_reducer::TaskTransitionRetryExhausted::new(error).into());
                    }
                }
                Err(error) if crate::runtime_db::repositories::is_terminal_child_deletion_admission_rejected(&error)
                    && source.original_deletion_requested()
                    && (pending.is_none() || crate::runtime_db::persistence_certainty(&error)==Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)) => {
                    #[cfg(test)]
                    {
                        source.record_terminal_deletion_rejection_for_test(&error);
                        self.inner.last_child_deletion_rejections_for_test.lock().unwrap()
                            .insert(source.address().admitted.key.id.clone(), format!("{error:#}"));
                    }
                    source.retain_deletion_rejection()?;pending.take();
                }
                result=>break result,
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
        if let Some(notifications) = source.take_sparse_notifications()? {
            for scope in &notifications.receipt.outcome.scopes {
                if scope.scope != source.address().scope {
                    continue;
                }
                let effects = PostCommitEffects {
                    audit_events: scope.audit_events.clone(),
                    notify_memory_index: scope.notify_memory_index,
                    ..Default::default()
                };
                for warning in self
                    .inner
                    .storage
                    .publish_transition_events(&effects)
                    .into_iter()
                    .chain(self.inner.storage.notify_transition_memory_index(&effects))
                {
                    tracing::warn!(task_id=%source.address().admitted.key.id,effect=warning.effect,error=%warning.message,"child terminal committed with post-commit warning");
                }
            }
        }
        if let Some(deletion) = read.ack.deletion.as_ref() {
            if let Some(bridge) = &self.inner.host_bridge {
                if let Err(error) = bridge
                    .activate_agent_deletion(&deletion.identity.agent_id)
                    .await
                {
                    tracing::warn!(agent_id=%deletion.identity.agent_id,error=%error,"original child deletion ACK awaits coordinator activation");
                }
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
}
