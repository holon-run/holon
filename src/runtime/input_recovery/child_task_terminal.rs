//! Fresh publication of one exact child terminal group, including related heads.
use super::*;
use crate::runtime_db::{
    child_task_terminal::{
        ChildTaskTerminalInstallMode, OriginalChildTerminalReadView, OriginalChildTerminalRecovery,
        OriginalChildTerminalRecoveryMaterial,
    },
    input_mutation_receipts::InputMutationResult,
};

pub(in crate::runtime) struct PreparedChildTerminalKnown {
    original: crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
    known: Vec<KnownCommit>,
    _guards: Vec<Arc<super::super::input_evidence_budget::BudgetViewGuard>>,
}
pub(in crate::runtime) struct ChildTerminalKnown {
    evidence: OriginalChildTerminalRecovery,
    known: Vec<KnownCommit>,
    _guards: Vec<Arc<super::super::input_evidence_budget::BudgetViewGuard>>,
}
impl PreparedChildTerminalKnown {
    pub(super) fn finish(
        self,
        evidence: OriginalChildTerminalRecovery,
    ) -> Result<ChildTerminalKnown> {
        ensure!(
            self.original == evidence.receipt().prepared
                && self.known.len() == self.original.scopes().len(),
            "child recovery original group changed"
        );
        Ok(ChildTerminalKnown {
            evidence,
            known: self.known,
            _guards: self._guards,
        })
    }
}
impl ChildTerminalKnown {
    pub(in crate::runtime) fn evidence(&self) -> &OriginalChildTerminalRecovery {
        &self.evidence
    }
    pub(in crate::runtime) fn known(&self) -> Vec<KnownCommit> {
        self.known.clone()
    }
    pub(in crate::runtime) fn known_for_scope(
        &self,
        scope: &super::super::resident_input_evidence::InputEvidenceScope,
    ) -> Option<&KnownCommit> {
        self.evidence
            .receipt()
            .prepared
            .scopes()
            .iter()
            .position(|s| s == scope)
            .and_then(|i| self.known.get(i))
    }
}
impl std::fmt::Debug for ChildTerminalKnown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildTerminalKnown")
            .field("original", &self.evidence.receipt().prepared)
            .finish_non_exhaustive()
    }
}
pub(super) fn prepare_known_tx(
    tx: &rusqlite::Transaction<'_>,
    view: &OriginalChildTerminalReadView<'_, '_>,
    reservations: &[MutationReservation],
    witness: &super::super::mutation_witness::MutationWitness,
    reader: &mut OriginalChildTerminalRecoveryMaterial<'_, '_, '_>,
    budget: &InputEvidenceBudgetLedger,
    config: &ContextConfig,
    models: &RuntimeModelCatalog,
    live: &[super::super::task_queries::TaskRecoveryQueries],
) -> Result<PreparedChildTerminalKnown> {
    let receipt = view.receipt();
    let InputMutationResult::ChildTaskTerminal { ack } = &receipt.outcome.result else {
        anyhow::bail!("child recovery ACK missing")
    };
    let scope = &ack.original.scope;
    let record_limit = reader.request().record_limit;
    let byte_limit = reader.request().byte_limit;
    reader.observe_known_slots(tx)?;
    let mut known = Vec::with_capacity(receipt.prepared.scopes().len());
    for (member, outcome) in receipt
        .prepared
        .scopes()
        .iter()
        .zip(&receipt.outcome.scopes)
    {
        view.validate_tx(tx, member)?;
        let reservation = reservations
            .iter()
            .find(|r| {
                r.agent_id() == member.agent_id
                    && r.identity_incarnation() == member.identity_incarnation
            })
            .context("child recovery held owner missing")?;
        let mode = view.mode(member)?;
        if mode != ChildTaskTerminalInstallMode::Current {
            // This ACK never proves old Accepted work absent. Only own new
            // admission tickets are disposed by mark_original_input_receipt_pending.
            known.push(KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                identity: view.identity(member).cloned().map(Arc::new),
                state: None,
            })));
            continue;
        }
        if ack.deletion.as_ref().is_some_and(|deletion| {
            deletion.identity.agent_id == member.agent_id
                && deletion.identity.incarnation == member.identity_incarnation
        }) {
            let prior = witness
                .input_runtime(&member.agent_id)
                .map(|facts| facts.task_status_metadata_known())
                .or_else(|| witness.input_child_observation(member).cloned());
            let state = match prior {
                Some(KnownCommit::State { state, .. }) => Some(state),
                _ => None,
            };
            known.push(KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                identity: view.identity(member).cloned().map(Arc::new),
                state,
            })));
            continue;
        }
        if member != scope {
            let original = witness
                .input_runtime(&member.agent_id)
                .map(|facts| facts.task_status_metadata_known())
                .or_else(|| witness.input_child_observation(member).cloned());
            let scope_ack = ack
                .status_scopes
                .iter()
                .find(|s| &s.scope == member)
                .context("child related scope ACK absent")?;
            let facts = match original {
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
                        let delta = reader.read_related_status(
                            tx,
                            member,
                            tasks.coverage(),
                            witness.input_storage_replayed(),
                        )?;
                        if delta.revision == delta.base_revision {
                            // The sealed same-TX reader reobserved the original
                            // complete metadata without changing any head.
                            ensure!(
                                delta.scope == *tasks.scope()
                                    && delta.base_revision == tasks.coverage().revision
                                    && delta.coverage == *tasks.coverage()
                                    && delta.records.is_empty()
                                    && delta.stop_records.is_empty()
                                    && delta.removals.is_empty(),
                                "child related no-op changed its original Task domain"
                            );
                            Some(tasks)
                        } else {
                            Some(Arc::new(tasks.with_committed(&delta)?))
                        }
                    } else {
                        ensure!(
                            witness.input_storage_replayed() || scope_ack.revision.is_none(),
                            "child metadata ACK changed an unobserved Task domain"
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
                        witness.input_storage_replayed() || scope_ack.revision.is_none(),
                        "cold related child ACK changed an unobserved Task domain"
                    );
                    KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                        identity: view.identity(member).cloned().map(Arc::new),
                        state: None,
                    }))
                }
                Some(KnownCommit::Metadata(_)) => {
                    anyhow::bail!("child original related observation invalid")
                }
            };
            known.push(facts);
            continue;
        }
        // This primary member really committed Task/Result/Queue. Its fresh
        // complete domains use the established same-view original Input loader.
        let state = reader.read_primary_state(tx)?;
        let context = models.resolved_context_config(config, state.model_override.as_ref());
        let model = models.effective_model(state.model_override.as_ref());
        let history = super::super::turn::context_history_preparation(&state.id, &model);
        let acquisition =
            crate::diagnostics::resource_io::ResourceReadMaterial::new(budget, member)?;
        let _entry = acquisition.enter();
        let mut input = super::super::context_capture::context_input_evidence_request(
            &state,
            None,
            &context,
            history,
            Default::default(),
            record_limit,
            byte_limit,
        )?;
        super::super::scheduler::include_scheduler_brief_window(&mut input.windows);
        let live_roles = live.iter().find(|queries| {
            queries
                .selection_for(member, reservation.owner_epoch())
                .is_some()
        });
        if let Some(roles) =
            live_roles.and_then(|queries| queries.input_for(member, reservation.owner_epoch()))
        {
            roles.validate_binding(member, budget)?;
            crate::diagnostics::resource_io::admit_cloned_value(&roles.selection)?;
            input.query = Some(super::super::resident_input_evidence::InputQueryRead {
                selection: roles.selection.clone(),
                ledger: budget.clone(),
            });
        }
        reader.observe_primary_selectors(tx)?;
        let mut work = super::super::resident_work::WorkCaptureRequest::current();
        use super::super::resident_work::{WorkFamily, WorkKey};
        work.mandatory_roots.extend(
            outcome
                .install_keys
                .work_items
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Work, id)),
        );
        work.mandatory_roots.extend(
            outcome
                .install_keys
                .wait_conditions
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        work.mandatory_roots.extend(
            outcome
                .install_keys
                .continuations
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Frame, id)),
        );
        work.latest_result_work_ids
            .extend(outcome.install_keys.work_items.iter().cloned());
        work.required_task_ids
            .extend(outcome.install_keys.tasks.iter().cloned());
        let mut task = super::super::resident_task::TaskCaptureRequest::current();
        task.required_keys
            .extend(outcome.install_keys.tasks.iter().map(|id| {
                super::super::resident_task::TaskKey::new(
                    super::super::resident_task::TaskFamily::Task,
                    id,
                )
            }));
        task.required_keys.extend(
            outcome
                .install_keys
                .task_result_settlements
                .iter()
                .map(|id| {
                    super::super::resident_task::TaskKey::new(
                        super::super::resident_task::TaskFamily::ResultSettlement,
                        id,
                    )
                }),
        );
        let current = reader.read_primary_input_domains(
            tx,
            &state,
            &input,
            context.recent_episode_candidates,
            &work,
            &task,
            live.iter()
                .find_map(|queries| queries.selection_for(member, reservation.owner_epoch())),
        )?;
        let work = current
            .work
            .context("child recovery omitted current Work")?;
        let tasks = current
            .tasks
            .context("child recovery omitted current Tasks")?;
        // Containers, ID heap and the temporary PendingBodyCharge slots are
        // admitted before constructing either fresh queue or the charge vector.
        let slots = current.pending.len();
        let id_bytes = current.pending.iter().try_fold(0u64, |n, m| {
            n.checked_add(serialized_bytes(&m.id)?)
                .context("child pending ID overflow")
        })?;
        let queue_bytes = u64::try_from(std::mem::size_of::<super::super::RuntimeQueue>())?
            .checked_add(
                u64::try_from(slots)?
                    .checked_mul(u64::try_from(
                        std::mem::size_of::<Arc<crate::types::MessageEnvelope>>()
                            + std::mem::size_of::<PendingBodyCharge>(),
                    )?)
                    .context("child Queue slots overflow")?,
            )
            .and_then(|n| n.checked_add(id_bytes))
            .and_then(|n| n.checked_mul(4))
            .context("child Queue metadata overflow")?;
        ensure!(
            queue_bytes <= u64::try_from(byte_limit.min(64 * 1024 * 1024))?
                && slots
                    .checked_mul(4)
                    .is_some_and(|n| n <= record_limit.min(65_536)),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "child recovery Queue metadata"
            )
        );
        reader.observe_queue_metadata(tx, usize::try_from(queue_bytes)?, slots)?;
        let queue_guard = budget.register_owned_bytes(
            member,
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: queue_bytes,
            },
        )?;
        let charges = current
            .pending
            .iter()
            .map(|message| {
                Ok(PendingBodyCharge {
                    message_id: message.id.clone(),
                    body_bytes: serialized_bytes(&message.body)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        budget.install_actual_pending(member, &charges)?;
        let state = Arc::new(state);
        let episodes = Arc::new(current.episodes);
        let inputs = Arc::new(current.inputs.with_budget(budget.clone())?);
        let cache = crate::runtime_db::input_recovery_install::load_current_install_facts_tx(
            tx,
            member,
            &outcome.install_keys,
        )?;
        let mut runtime = RuntimeMutationFacts::for_input_recovery(
            reservation.owner_epoch(),
            state.clone(),
            episodes.clone(),
            inputs.clone(),
            work.clone(),
            Some(tasks.clone()),
            current.scheduling.clone(),
            current.pending,
            budget,
            witness.input_runtime(&member.agent_id).map(Arc::as_ref),
            outcome,
            receipt.outcome.applied,
            view.identity(member).cloned(),
            cache,
        )?
        .with_task_stop_queue_metadata(queue_guard);
        if let Some(roles) =
            live_roles.and_then(|queries| queries.current_for(member, reservation.owner_epoch()))
        {
            roles.validate(member, reservation.owner_epoch(), roles.selection())?;
            let query = acquisition.read(|| {
                crate::runtime_db::current_query::prepare(
                    tx,
                    member,
                    Some(roles.selection()),
                    budget,
                    (byte_limit, record_limit),
                )
            })?;
            runtime.current = Some(Arc::new(
                crate::runtime_db::current_read_writes::CommittedCurrentWrites::prepared_query(
                    query,
                ),
            ));
        }
        acquisition.finish()?;
        if witness.input_storage_replayed() {
            runtime.suppress_observed_input_replay_publication();
        }
        known.push(KnownCommit::State {
            state,
            episodes: Some(episodes),
            input_evidence: Some(inputs),
            work: Some(work),
            tasks: Some(tasks),
            scheduling: Some(current.scheduling),
            runtime: Some(Arc::new(runtime)),
        });
    }
    Ok(PreparedChildTerminalKnown {
        original: receipt.prepared.clone(),
        known,
        _guards: Vec::new(),
    })
}
