//! Original Stop continuations keep their exact Input witness through publication.
use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
            PendingAdmission,
        },
        mutation_witness::MutationWitness,
    },
    runtime_db::{
        input_mutation_receipts::{
            InputMutationFamily, InputMutationResult, OriginalInputMutationAttempt,
        },
        task_snapshot::TaskDomainLoadRequest,
        task_stop::TaskStopPhaseAck,
        transitions::{InputMutationExecution, PreparedInputTransition},
        work_snapshot::WorkDomainLoadRequest,
    },
};

/// The internal continuation keeps its owned ACK charged across worker awaits.
pub(crate) struct TaskStopMetadataPhaseRead {
    pub(crate) ack: Box<TaskStopPhaseAck>,
    _material: Arc<BudgetViewGuard>,
}
impl RuntimeHostBridge {
    /// Called only by the already owned original Stop continuation. This does
    /// not register fresh Host admission or signal any physical target.
    pub(crate) async fn commit_task_stop_phase_metadata(
        &self,
        prepared: PreparedInputTransition,
        request: TaskDomainLoadRequest,
    ) -> Result<TaskStopMetadataPhaseRead> {
        self.host()?
            .commit_task_stop_phase_metadata_owned(prepared, request)
            .await
    }
}
impl RuntimeHost {
    async fn commit_task_stop_phase_metadata_owned(
        &self,
        prepared: PreparedInputTransition,
        request: TaskDomainLoadRequest,
    ) -> Result<TaskStopMetadataPhaseRead> {
        let scope = prepared
            .task_stop_phase_scope()
            .context("Host Stop actor requires an original focused phase")?;
        anyhow::ensure!(
            prepared.descriptor().family() == InputMutationFamily::Task
                && prepared.descriptor().scopes() == std::slice::from_ref(scope),
            "Host Stop actor requires only its original parent scope"
        );
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let selector_bytes = serialized_bytes(&request.capture.required_keys)?
            .checked_mul(8)
            .context("Stop request material overflow")?;
        anyhow::ensure!(
            selector_bytes <= u64::try_from(request.byte_limit)?
                && request.capture.required_keys.len() <= request.record_limit
                && selector_bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("Stop actor limits overflow")?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "Stop actor request metadata"
            )
        );
        let _selectors = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: selector_bytes,
            },
        )?;
        let _prepared = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: prepared.owned_payload_bytes()?,
                metadata_estimate_bytes: serialized_bytes(prepared.descriptor())?,
            },
        )?;
        let scope = scope.clone();
        let mut reservations = self
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        anyhow::ensure!(reservations.len() == 1, "Host Stop original group changed");
        let reservation = &reservations[0];
        let event_roles = reservation.current_event_roles(
            &scope,
            ledger,
            (
                usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("Stop Event role allowance overflow")?,
                )?,
                usize::try_from(limits.record_limit)?,
            ),
        )?;
        let prepared = match event_roles {
            Some(roles) => {
                prepared.with_current_event_roles(roles, reservation.owner_epoch(), ledger)?
            }
            None => prepared,
        };
        let original_runtime = self.bridge().runtime_for_mutation(reservation).await?;
        // This is the first ACK under a Ready reservation, before any recovery
        // witness exists. Freeze the actual instance's roles directly.
        let task_queries = original_runtime
            .as_ref()
            .map(|runtime| {
                runtime.task_recovery_queries(
                    reservation,
                    request
                        .record_limit
                        .min(usize::try_from(limits.record_limit)?),
                    request.byte_limit.min(usize::try_from(
                        limits
                            .history_per_agent_bytes
                            .checked_add(limits.pending_per_agent_bytes)
                            .context("Stop query limit overflow")?,
                    )?),
                )
            })
            .transpose()?
            .flatten();
        let mut request = request;
        request.query = task_queries
            .as_ref()
            .and_then(|queries| queries.selection_for(&scope, reservation.owner_epoch()))
            .cloned()
            .unwrap_or_default();
        let mut runtime_facts = Vec::new();
        let mut current_read_source = None;
        if let Some(runtime) = &original_runtime {
            if let Some(root) = reservation.capture_ready_snapshot()? {
                current_read_source = root.current_reads.clone();
                runtime_facts.push(Arc::new(
                    runtime
                        .prepare_external_input_facts(
                            reservation,
                            root.state.clone(),
                            root.episodes.clone(),
                            root.input_evidence.clone(),
                        )
                        .await?,
                ));
            }
        }
        let prepared = match current_read_source {
            Some(source) => prepared.with_current_read_source(source)?,
            None => prepared,
        };
        let prepared = crate::runtime::attach_task_stop_phase_memory_to_original_owner(
            self.runtime_db(),
            ledger,
            reservation,
            prepared,
            Utc::now(),
        )?;
        let _decorated = ledger.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: prepared.owned_payload_bytes()?,
                metadata_estimate_bytes: 0,
            },
        )?;
        let work_request = WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Stop Work limits overflow")?,
            )?,
        };
        let input_source = reservations[0].input_query_source(
            &scope, ledger, (work_request.byte_limit, work_request.record_limit),
        )?;
        let mut pending = Vec::new();
        for message in prepared.pending_messages()? {
            match ledger.reserve_pending(&scope, &message.id, serialized_bytes(&message.body)?) {
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
        let operation_id = prepared.descriptor().command_nonce().to_owned();
        let pending_cleanup = pending.clone();
        let witness = match MutationWitness::input_mutation(
            operation_id.clone(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            pending,
        )
        .and_then(|witness| witness.with_input_runtime(runtime_facts))
        .and_then(|witness| witness.with_task_stop_parent_observation(&reservations[0], &prepared))
        {
            Ok(witness) => witness,
            Err(error) => {
                for (ledger, ticket) in &pending_cleanup {
                    ledger.definitely_not_committed(ticket)?;
                }
                return Err(error);
            }
        };
        if let Err(error) = MutationReservation::begin_all_with_witness(
            &mut reservations,
            operation_id,
            witness.clone(),
        ) {
            witness.release_pending_not_committed(false)?;
            return Err(error.into());
        }
        if let Err(error) = witness.mark_pending_started() {
            witness.finish_original_attempt();
            witness.release_pending_not_committed(false)?;
            MutationReservation::definitely_not_committed_all(&mut reservations)?;
            return Err(error);
        }
        let result = self
            .runtime_db()
            .transitions_in_scope(&scope)
            .commit_prepared_transition_with_input_query_source(
                &prepared,
                &work_request,
                ledger,
                &request,
                &input_source,
            );
        // No await separates the synchronous attempt from its original witness.
        let replay = matches!(&result, Ok(InputMutationExecution::Replayed { .. }));
        if replay {
            witness.mark_input_storage_replayed()?;
        }
        witness.finish_original_attempt();
        let execution = match result {
            Ok(execution) => execution,
            Err(error) => {
                if crate::runtime_db::persistence_certainty(&error)
                    == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
                {
                    witness.release_pending_not_committed(false)?;
                    MutationReservation::definitely_not_committed_all(&mut reservations)?;
                } else {
                    witness.mark_pending_unknown()?;
                }
                return Err(error);
            }
        };
        let (receipt, _actual_material, fault) = match execution {
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
        let InputMutationResult::TaskStopPhase { ack } = &receipt.outcome.result else {
            bail!("Host Stop ACK has another result family")
        };
        let result_guard = ledger.register_owned_bytes(
            &scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: serialized_bytes(ack.as_ref())?,
            },
        )?;
        // Fresh receipt resolution seals all actual partial domains and records
        // Known synchronously, before cancellation can interrupt publication.
        crate::runtime::input_recovery::resolve_input_recovery_with_queries(
            self.runtime_db(),
            &mut reservations,
            ledger,
            &self.runtime_context_config(),
            &RuntimeModelCatalog::from_config(&self.config()),
            task_queries.as_slice(),
        )?;
        let result = TaskStopMetadataPhaseRead {
            ack: ack.clone(),
            _material: result_guard,
        };
        if fault == Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate) {
            bail!("original Stop phase committed but publication requires reconciliation");
        }
        #[cfg(test)]
        if self
            .inner
            .external_publication_fault
            .swap(false, Ordering::SeqCst)
        {
            bail!("original Stop phase committed but whole publication requires reconciliation");
        }
        self.reconcile_external_mutations(&mut reservations).await?;
        Ok(result)
    }
}
