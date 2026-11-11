//! Standalone evidence owns the same original-attempt recovery protocol as Queue.
use super::*;
use crate::runtime_db::{
    input_mutation_receipts::{
        with_fresh_input_mutation_resolution, InputMutationResolution, InputMutationResult,
        OriginalInputMutationAttempt,
    },
    prepared_input_evidence::{InputEvidenceWrite, PreparedEvidenceValue, PreparedInputEvidence},
    transitions::{InputMutationExecution, PostCommitEffects},
};
use crate::runtime_error::{RuntimeError, RuntimeErrorDomain, TOOL_RESULT_SETTLEMENT_PENDING};

#[derive(Debug, thiserror::Error)]
#[error("the original tool result transaction has no committed receipt")]
struct ToolResultReceiptAbsent;

fn tool_result_not_committed(error: &anyhow::Error) -> bool {
    error.is::<ToolResultReceiptAbsent>()
        || crate::runtime_db::original_input_attempt_certainty(error)
            == Some(PersistenceCertainty::DefinitelyNotCommitted)
}

impl RuntimeMutation {
    #[inline(never)]
    pub(crate) fn commit_prepared_evidence<'a>(
        &'a mut self,
        prepared: PreparedInputEvidence,
        fault: Option<TransitionFaultPoint>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<InputMutationResult>> + Send + 'a>,
    > {
        Box::pin(self.commit_prepared_evidence_inner(prepared, fault))
    }

    async fn commit_prepared_evidence_inner(
        &mut self,
        prepared: PreparedInputEvidence,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<InputMutationResult> {
        self.validate_candidate_identity()?;
        let scopes = self
            .reservations
            .iter()
            .map(
                |reservation| super::super::resident_input_evidence::InputEvidenceScope {
                    agent_id: reservation.agent_id().to_owned(),
                    identity_incarnation: reservation.identity_incarnation(),
                },
            )
            .collect();
        let prepared = prepared.with_scope_group(scopes)?;
        let _owned = match prepared.original_material(&self.runtime.inner.input_evidence_budget)? {
            Some(material) => material,
            None => self
                .runtime
                .inner
                .input_evidence_budget
                .register_owned_bytes(
                    prepared.primary_scope(),
                    super::super::input_evidence_budget::InputMemoryCategory::Candidate,
                    super::super::input_evidence_budget::OwnedBytesEstimate {
                        payload_bytes: prepared.owned_payload_bytes()?,
                        metadata_estimate_bytes: 0,
                    },
                )?,
        };
        let result = self.commit_prepared_evidence_once(&prepared, fault).await;
        if prepared.is_fenced_tool_result() && result.as_ref().is_err_and(tool_result_not_committed)
        {
            // Keep the original result, owner and command identity through one
            // retry. The writer has released its transaction during the delay.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            return self.commit_prepared_evidence_once(&prepared, fault).await;
        }
        result
    }

    async fn commit_prepared_evidence_once(
        &mut self,
        prepared: &PreparedInputEvidence,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<InputMutationResult> {
        let mut saved = vec![Arc::new(self.facts(Default::default()))];
        for (index, reservation) in self.reservations.iter().enumerate() {
            if index == self.primary_index {
                continue;
            }
            if let Some(bridge) = &self.runtime.inner.host_bridge {
                if let Some(runtime) = bridge.runtime_for_mutation(reservation).await? {
                    let state = self.metadata[index]
                        .as_ref()
                        .and_then(|facts| facts.state.clone())
                        .context("related loaded evidence owner has no State")?;
                    saved.push(Arc::new(
                        runtime
                            .prepare_external_input_facts(
                                reservation,
                                state,
                                reservation.prepared_episodes_for_state_only_commit()?,
                                reservation.prepared_inputs_for_state_only_commit()?,
                            )
                            .await?,
                    ));
                }
            }
        }
        let operation = prepared.descriptor().command_nonce().to_owned();
        let witness = super::super::mutation_witness::MutationWitness::input_mutation(
            operation.clone(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            Vec::new(),
        )?
        .with_input_runtime(saved)?;
        let work_request = self.owner_work_load_request()?;
        let (task_request, _task_selectors) = self.owner_task_load_request()?;
        let ledger = self.runtime.inner.input_evidence_budget.clone();
        let input_query_source = self.owner_input_query_source()?;
        let current_query_source = self.owner_current_query_source()?;
        let storage = self.runtime.inner.storage.clone();
        let mut original_result = None;
        let replay_observation = witness.clone();
        let committed = self
            .commit_transition_inner(operation, Some(witness), || {
                match storage.commit_prepared_input_evidence_with_task_budget(
                    prepared,
                    &work_request,
                    &task_request,
                    &ledger,
                    input_query_source.as_ref(),
                    current_query_source.as_ref(),
                )? {
                    InputMutationExecution::Replayed {
                        receipt,
                        _material_budget_views,
                    } => {
                        replay_observation.mark_input_storage_replayed()?;
                        Err(PreparedInputReplay(receipt, _material_budget_views).into())
                    }
                    InputMutationExecution::Committed {
                        value: ack,
                        receipt,
                    } => {
                        #[cfg(test)]
                        if fault == Some(TransitionFaultPoint::ToolResultCommitAckLost) {
                            return Err(crate::runtime_db::persistence::mark_persistence_failure(
                                anyhow!("injected tool result commit ACK loss"),
                                PersistenceCertainty::Indeterminate,
                                crate::runtime_db::persistence::PersistenceFailurePhase::Commit,
                                None,
                            ));
                        }
                        original_result = Some(receipt.outcome.result);
                        let mut effects = PostCommitEffects {
                            current_read_writes: ack.current_read_writes,
                            agent_state: ack.agent_state,
                            input_evidence_delta: ack.input_delta,
                            work_delta: ack.work_delta,
                            work_delta_requires_prepared_domain: true,
                            task_delta: ack.task_delta,
                            task_delta_requires_prepared_domain: true,
                            task_material_budget_views: ack.task_material_budget_views,
                            notify_memory_index: true,
                            notify_scheduler: true,
                            fault,
                            ..Default::default()
                        };
                        match ack.record {
                            PreparedEvidenceValue::BriefCreated(actual) => {
                                effects.briefs.push(actual.brief);
                                if actual.event_inserted {
                                    effects.audit_events.push(
                                        crate::runtime_db::transitions::AuditEventAck::new(
                                            actual.event,
                                        )?,
                                    );
                                }
                            }
                            #[cfg(test)]
                            PreparedEvidenceValue::Message(actual) => drop(actual),
                            #[cfg(test)]
                            PreparedEvidenceValue::Turn(actual) => drop(actual),
                            PreparedEvidenceValue::TurnBarrierDone => {}
                            PreparedEvidenceValue::Transcript(actual) => drop(actual),
                            PreparedEvidenceValue::ToolDone => {}
                            PreparedEvidenceValue::ToolResult(events) => {
                                effects.audit_events.extend(events)
                            }
                        }
                        Ok(TransitionCommit {
                            applied: receipt.outcome.applied,
                            effects: Box::new(effects),
                            ..Default::default()
                        })
                    }
                }
            })
            .await;
        match committed {
            Err(error) if error.downcast_ref::<PreparedInputReplay>().is_some() => {
                let replay = error
                    .downcast::<PreparedInputReplay>()
                    .expect("checked replay");
                let PreparedInputReplay(receipt, _replay_material) = replay;
                self.runtime
                    .reconcile_reserved_mutations(&mut self.reservations)
                    .await?;
                self.candidate = Box::new(self.runtime.inner.agent.lock().await.clone());
                self.base = self.candidate.clone();
                let root = self.runtime.inner.mutation_owner.capture()?;
                self.episodes = root.as_ref().and_then(|root| root.episodes.clone());
                self.input_evidence = root.as_ref().and_then(|root| root.input_evidence.clone());
                self.work = root.as_ref().and_then(|root| root.work.clone());
                self.tasks = root.and_then(|root| root.tasks.clone());
                self.deferred_abort = None;
                Ok(receipt.outcome.result)
            }
            Err(error) => {
                if let Some(result) = original_result {
                    let published = self.reconcile_known_commit().await.map_err(|error| {
                        if prepared.is_fenced_tool_result() {
                            crate::runtime_db::persistence::mark_original_input_recovery_failure(
                                error,
                            )
                        } else {
                            error
                        }
                    })?;
                    if published {
                        return Ok(result);
                    }
                    if prepared.is_fenced_tool_result() {
                        return Err(
                            crate::runtime_db::persistence::mark_original_input_recovery_failure(
                                error,
                            ),
                        );
                    }
                } else if prepared.is_fenced_tool_result()
                    && persistence_certainty(&error) == Some(PersistenceCertainty::Indeterminate)
                {
                    let attempt = replay_observation
                        .input_attempt()
                        .context("tool settlement lost its original attempt")?;
                    let result = with_fresh_input_mutation_resolution(
                        &self.runtime.inner.runtime_db,
                        attempt,
                        |_, resolution| {
                            Ok(match resolution {
                                InputMutationResolution::Present(receipt) => {
                                    Some(receipt.outcome.result)
                                }
                                InputMutationResolution::Absent => None,
                            })
                        },
                    )?;
                    let published = self
                        .runtime
                        .reconcile_reserved_mutations(&mut self.reservations)
                        .await
                        .map_err(
                            crate::runtime_db::persistence::mark_original_input_recovery_failure,
                        )?;
                    return match result {
                        Some(result) if published => Ok(result),
                        Some(_) => {
                            Err(error.context("committed tool result publication is pending"))
                        }
                        None => Err(error.context(ToolResultReceiptAbsent)),
                    };
                }
                Err(error)
            }
            Ok(commit) => {
                self.runtime.apply_transition_commit(commit).await;
                original_result.context("evidence ACK omitted its original result")
            }
        }
    }
}

impl RuntimeHandle {
    pub(crate) async fn persist_prepared_evidence(&self, write: InputEvidenceWrite) -> Result<()> {
        let external_tool = match &write {
            InputEvidenceWrite::ToolResult { record, .. }
                if matches!(
                    record.tool_name.as_str(),
                    crate::tool::names::EXEC_COMMAND
                        | crate::tool::names::EXEC_COMMAND_BATCH
                        | crate::tool::names::SCHEDULE_DESTRUCTIVE_OPERATION
                ) =>
            {
                record
                    .turn_id
                    .as_ref()
                    .map(|turn_id| (record.id.clone(), turn_id.clone()))
            }
            _ => None,
        };
        let result = self.persist_prepared_evidence_owned(write).await;
        result.map_err(|error| {
            let Some((execution_id, turn_id)) = external_tool else {
                return error;
            };
            let (status, message) = if tool_result_not_committed(&error) {
                ("not_persisted", "Tool returned, but its result was not persisted")
            } else {
                ("outcome_unknown", "Tool returned, but its result settlement is unconfirmed")
            };
            error.context(
                RuntimeError::new(
                    RuntimeErrorDomain::Storage,
                    TOOL_RESULT_SETTLEMENT_PENDING,
                    message,
                )
                .with_safe_context("status", status)
                .with_safe_context("tool_execution_id", execution_id)
                .with_safe_context("turn_id", turn_id)
                .with_recovery_hint("Inspect the original tool execution and external state before submitting it again")
            )
        })
    }

    async fn persist_prepared_evidence_owned(&self, write: InputEvidenceWrite) -> Result<()> {
        if matches!(write, InputEvidenceWrite::ToolResult { .. }) {
            self.inner.storage.event_log().ensure_writable()?;
        }
        let admission = self.execution_admission_lease()?;
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = async {
                let mut mutation = runtime.begin_mutation().await?;
                let mut write = write;
                if let InputEvidenceWrite::ToolResult {
                    record, barrier, ..
                } = &mut write
                {
                    *barrier = mutation.tool_result_barrier(record).await?;
                }
                let prepared = runtime.inner.storage.prepare_input_evidence(
                    mutation.input_evidence_scope(),
                    crate::ids::audit_event_id(),
                    Utc::now(),
                    write,
                )?;
                mutation
                    .commit_prepared_evidence(prepared, None)
                    .await
                    .map(|_| ())
            }
            .await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("evidence worker ended before returning its result")?
    }
}
