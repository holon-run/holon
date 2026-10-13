//! Standalone evidence owns the same original-attempt recovery protocol as Queue.
use super::*;
use crate::runtime_db::{
    input_mutation_receipts::{InputMutationResult, OriginalInputMutationAttempt},
    prepared_input_evidence::{InputEvidenceWrite, PreparedEvidenceValue, PreparedInputEvidence},
    transitions::{InputMutationExecution, PostCommitEffects},
};

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
        let _owned = self
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
            )?;
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
        let storage = self.runtime.inner.storage.clone();
        let mut original_result = None;
        let replay_observation = witness.clone();
        let committed = self
            .commit_transition_inner(operation, Some(witness), || {
                match storage
                    .commit_prepared_input_evidence_with_work_request(&prepared, &work_request)?
                {
                    InputMutationExecution::Replayed { receipt } => {
                        replay_observation.mark_input_storage_replayed()?;
                        Err(PreparedInputReplay(receipt).into())
                    }
                    InputMutationExecution::Committed {
                        value: ack,
                        receipt,
                    } => {
                        original_result = Some(receipt.outcome.result);
                        let mut effects = PostCommitEffects {
                            agent_state: ack.agent_state,
                            input_evidence_delta: ack.input_delta,
                            work_delta: ack.work_delta,
                            work_delta_requires_prepared_domain: true,
                            notify_memory_index: true,
                            notify_scheduler: true,
                            fault,
                            ..Default::default()
                        };
                        match ack.record {
                            PreparedEvidenceValue::BriefCreated(actual) => {
                                effects.briefs.push(actual.brief);
                                if actual.event_inserted {
                                    effects.audit_events.push(actual.event);
                                }
                            }
                            #[cfg(test)]
                            PreparedEvidenceValue::Message(actual) => drop(actual),
                            #[cfg(test)]
                            PreparedEvidenceValue::Turn(actual) => drop(actual),
                            PreparedEvidenceValue::Transcript(actual) => drop(actual),
                            PreparedEvidenceValue::ToolDone => {}
                        }
                        Ok(TransitionCommit {
                            applied: receipt.outcome.applied,
                            effects,
                            ..Default::default()
                        })
                    }
                }
            })
            .await;
        match committed {
            Err(error) if error.downcast_ref::<PreparedInputReplay>().is_some() => {
                let receipt = error
                    .downcast::<PreparedInputReplay>()
                    .expect("checked replay")
                    .0;
                self.runtime
                    .reconcile_reserved_mutations(&mut self.reservations)
                    .await?;
                self.candidate = Box::new(self.runtime.inner.agent.lock().await.clone());
                self.base = self.candidate.clone();
                let root = self.runtime.inner.mutation_owner.capture()?;
                self.episodes = root.as_ref().and_then(|root| root.episodes.clone());
                self.input_evidence = root.as_ref().and_then(|root| root.input_evidence.clone());
                self.work = root.and_then(|root| root.work.clone());
                self.deferred_abort = None;
                Ok(receipt.outcome.result)
            }
            Err(error) => {
                if let Some(result) = original_result {
                    if self.reconcile_known_commit().await? {
                        return Ok(result);
                    }
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
        let admission = self.execution_admission_lease()?;
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let _admission = admission;
            let result = async {
                let mut mutation = runtime.begin_mutation().await?;
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
