//! Resolve original input transactions while retaining their complete owner group.

use std::sync::Arc;

use anyhow::{ensure, Context, Result};

use super::{
    input_evidence_budget::{serialized_bytes, InputEvidenceBudgetLedger, PendingBodyCharge},
    mutation_owner::{IdentityMutationFacts, KnownCommit, MutationReservation},
    mutation_witness::MutationWitnessFamily,
    state_mutation::RuntimeMutationFacts,
};
use crate::{
    config::RuntimeModelCatalog,
    context::ContextConfig,
    runtime_db::{
        context_inputs::ContextDurableRead,
        input_mutation_receipts::{
            with_fresh_input_mutation_preparation_and_preflight, InputMutationResolution,
        },
        input_preparation::prepare_original_input_recovery_with_queries_tx,
        RuntimeDb,
    },
    types::AgentRegistryStatus,
};

mod child_monitor_binding;
mod child_task_terminal;
mod task_status_metadata;
mod task_stop_partial;
pub(in crate::runtime) use child_monitor_binding::ChildMonitorBindingKnown;
pub(in crate::runtime) use child_task_terminal::ChildTerminalKnown;
pub(in crate::runtime) use task_status_metadata::TaskStatusMetadataKnown;
pub(in crate::runtime) use task_stop_partial::TaskStopPartialKnown;

/// This is an explicit recovery actor, not an ordinary input read. Positive
/// receipts permit bounded qualification preparation in that same fresh view;
/// they never reinstall the original command candidate or execute it again.
pub(crate) fn resolve_input_recovery(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    budget: &InputEvidenceBudgetLedger,
    config: &ContextConfig,
    models: &RuntimeModelCatalog,
) -> Result<bool> {
    resolve_input_recovery_with_queries(db, reservations, budget, config, models, &[])
}

pub(crate) fn resolve_input_recovery_with_queries(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    budget: &InputEvidenceBudgetLedger,
    config: &ContextConfig,
    models: &RuntimeModelCatalog,
    live: &[super::task_queries::TaskRecoveryQueries],
) -> Result<bool> {
    let mut resolved = false;
    for index in 0..reservations.len() {
        let Some(recovery) = reservations[index].recovery()? else {
            continue;
        };
        if recovery.known.is_some() {
            continue;
        }
        let Some(witness) = recovery.witness.as_ref() else {
            continue;
        };
        let MutationWitnessFamily::InputMutation { prepared } = witness.family() else {
            continue;
        };
        if !witness.original_attempt_finished() {
            continue;
        }
        MutationReservation::validate_input_unknown(reservations, &recovery.key, witness)?;
        let attempt = witness
            .input_attempt()
            .context("input recovery lacks original attempt")?;
        let limits = budget.limits();
        let task_load = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Stop recovery limit overflow")?,
            )?,
        };
        if let Some((source, original)) = witness.input_child_binding() {
            let mut request = task_load.clone();
            if let Some(queries) = live.iter().find_map(|queries| {
                queries.selection_for(&source.address().scope, reservations[index].owner_epoch())
            }) {
                request.query = queries.clone();
                request
                    .capture
                    .required_keys
                    .extend(queries.required_keys.iter().cloned());
            }
            let retained_queue = witness
                .input_runtime(&source.address().scope.agent_id)
                .map(|facts| facts.original_child_binding_queue_material())
                .transpose()?
                .unwrap_or(crate::runtime_db::task_snapshot::TaskMaintenanceMaterial {
                    bytes: 0,
                    records: 0,
                });
            if let Some((proof, known)) = db.with_original_child_monitor_binding_recovery(
                attempt,
                &source,
                original,
                &request,
                retained_queue,
                budget,
                |tx, view| child_monitor_binding::prepare_known_tx(tx, view, witness),
            )? {
                witness.mark_original_input_receipt_pending(proof.receipt(), true)?;
                let known = Arc::new(ChildMonitorBindingKnown::new(proof, known));
                MutationReservation::resolve_child_monitor_binding_input_unknown(
                    reservations,
                    &recovery.key,
                    witness,
                    known,
                )?;
                resolved = true;
                continue;
            }
        }
        if let Some(source) = witness.input_child_producer() {
            if let Some((proof, partial)) = db.with_original_child_terminal_recovery(
                attempt,
                &source,
                &task_load,
                budget,
                |tx, view, reader| {
                    child_task_terminal::prepare_known_tx(
                        tx,
                        view,
                        reservations,
                        witness,
                        reader,
                        budget,
                        config,
                        models,
                        live,
                    )
                },
            )? {
                witness.mark_original_input_receipt_pending(proof.receipt(), true)?;
                if !witness.input_storage_replayed() {
                    source.retain_sparse_notifications(&proof, budget)?;
                }
                let known = Arc::new(partial.finish(proof)?);
                MutationReservation::resolve_child_terminal_input_unknown(
                    reservations,
                    &recovery.key,
                    witness,
                    known,
                )?;
                resolved = true;
                continue;
            }
        }
        if prepared.family()
            == crate::runtime_db::input_mutation_receipts::InputMutationFamily::Task
            && prepared.scopes().len() == 1
        {
            if let Some((proof, partial)) = db.with_task_status_metadata_recovery(
                attempt,
                &task_load,
                budget,
                |tx, view, remaining| {
                    task_status_metadata::prepare_known_tx(tx, view, witness, remaining, budget)
                },
            )? {
                witness.mark_original_input_receipt_pending(proof.receipt(), true)?;
                let partial = Arc::new(partial.finish(proof)?);
                let known = vec![partial.known()];
                MutationReservation::resolve_task_status_metadata_input_unknown(
                    reservations,
                    &recovery.key,
                    witness,
                    known,
                    partial,
                )?;
                resolved = true;
                continue;
            }
            if let Some(proof) =
                db.observe_original_command_task_sparse_recovery(attempt, &task_load, budget)?
            {
                witness.mark_original_input_receipt_pending(proof.receipt(), true)?;
                witness.retain_original_command_notifications(&proof, budget)?;
                let known = vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                    identity: proof.identity().cloned().map(Arc::new),
                    state: None,
                }))];
                MutationReservation::resolve_original_command_input_unknown(
                    reservations,
                    &recovery.key,
                    witness,
                    known,
                    Arc::new(proof),
                )?;
                resolved = true;
                continue;
            }
            if let Some((proof, partial)) =
                db.with_task_stop_sparse_recovery(attempt, &task_load, budget, |tx, view| {
                    let reservation = reservations
                        .iter()
                        .find(|r| {
                            r.agent_id() == view.scope().agent_id
                                && r.identity_incarnation() == view.scope().identity_incarnation
                        })
                        .context("Stop sparse parent reservation missing")?;
                    task_stop_partial::prepare_partial_stop_known_tx(
                        tx,
                        view,
                        reservation,
                        witness,
                        budget,
                        config,
                        models,
                    )
                })?
            {
                witness.mark_original_input_receipt_pending(proof.receipt(), true)?;
                if let Some(partial) = partial {
                    let partial = Arc::new(partial.finish(proof)?);
                    let known = vec![partial.known()];
                    MutationReservation::resolve_task_stop_input_unknown(
                        reservations,
                        &recovery.key,
                        witness,
                        known,
                        super::mutation_owner::OriginalInputRecoveryEvidence::Partial(partial),
                    )?;
                } else {
                    let known = vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                        identity: proof.identity().cloned().map(Arc::new),
                        state: None,
                    }))];
                    MutationReservation::resolve_task_stop_input_unknown(
                        reservations,
                        &recovery.key,
                        witness,
                        known,
                        super::mutation_owner::OriginalInputRecoveryEvidence::Sparse(Arc::new(
                            proof,
                        )),
                    )?;
                }
                resolved = true;
                continue;
            }
        }
        let mut readonly_proofs = Vec::new();
        let mut connection = db.connection()?;
        let current = with_fresh_input_mutation_preparation_and_preflight(
            &mut connection,
            attempt,
            |tx| {
                use crate::runtime_db::{
                    input_mutation_receipts::InputMutationFamily,
                    task_snapshot::TaskDomainLoadRequest,
                    transitions::task_result_receipt_budget_tx,
                };
                if !matches!(
                    prepared.family(),
                    InputMutationFamily::Task
                        | InputMutationFamily::TaskResult
                        | InputMutationFamily::Timer
                ) {
                    return Ok(None);
                }
                let limits = budget.limits();
                let request = TaskDomainLoadRequest {
                    query: Default::default(),
                    capture: super::resident_task::TaskCaptureRequest::current(),
                    record_limit: usize::try_from(limits.record_limit)?,
                    byte_limit: usize::try_from(
                        limits
                            .history_per_agent_bytes
                            .checked_add(limits.pending_per_agent_bytes)
                            .context("Result recovery receipt limit overflow")?,
                    )?,
                };
                let (owner, incarnation) = witness.owner();
                let scope = super::resident_input_evidence::InputEvidenceScope {
                    agent_id: owner.to_owned(),
                    identity_incarnation: incarnation,
                };
                task_result_receipt_budget_tx(tx, prepared, &scope, &request, budget).map(Some)
            },
            |tx, result| {
                let InputMutationResolution::Present(receipt) = result else {
                    return Ok(None);
                };
                // Only this original positive receipt can promote these tickets.
                // Retrying preparation preserves this shared accepted observation.
                witness.mark_original_input_receipt_pending(&receipt, true)?;
                let mut known = Vec::with_capacity(prepared.scopes().len());
                for (scope, outcome) in prepared.scopes().iter().zip(&receipt.outcome.scopes) {
                    ensure!(&outcome.scope == scope, "receipt scope group changed");
                    let reservation = reservations
                        .iter()
                        .find(|reservation| {
                            reservation.agent_id() == scope.agent_id
                                && reservation.identity_incarnation() == scope.identity_incarnation
                        })
                        .context("original input owner group is incomplete")?;
                    if let Some(original) = witness.initial_task_stop_readonly(&scope.agent_id) {
                        let proof=crate::runtime_db::task_stop::observe_initial_task_stop_readonly_recovery_tx(
                            tx,attempt,&receipt,original.target(),&task_load,budget)?;
                        let original_known = original.known();
                        let facts = if proof.child().state_present() == Some(true)
                            && matches!(original_known, KnownCommit::State { .. })
                        {
                            original_known
                        } else {
                            KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                                identity: Some(proof.child().identity().clone()),
                                state: None,
                            }))
                        };
                        known.push(facts);
                        readonly_proofs.push(Arc::new(proof));
                        continue;
                    }
                    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
                    if !facts.identity.as_ref().is_some_and(|identity| {
                        identity.agent_id == scope.agent_id
                            && identity.incarnation == scope.identity_incarnation
                            && matches!(
                                identity.status,
                                AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
                            )
                    }) {
                        // Withdraw this retired namespace's accepted accounting;
                        // its successor's State and pending bodies are never read.
                        budget.install_actual_pending(scope, &[])?;
                        known.push(KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                            identity: facts.identity.map(Arc::new),
                            state: None,
                        })));
                        continue;
                    }
                    let Some(state) = facts.state.as_deref() else {
                        let pending = crate::runtime_db::bootstrap_inputs::pending_metadata_for_input_recovery_tx(tx, scope, budget)?;
                        ensure!(
                            pending.count == 0,
                            "cold input recovery has pending work without State"
                        );
                        budget.install_actual_pending(scope, &[])?;
                        known.push(KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                            identity: facts.identity.map(Arc::new),
                            state: None,
                        })));
                        continue;
                    };
                    let acquisition =
                        crate::diagnostics::resource_io::ResourceReadMaterial::new(budget, scope)?;
                    let _entry = acquisition.enter();
                    let live_roles = live.iter().find(|queries| {
                        queries
                            .selection_for(scope, reservation.owner_epoch())
                            .is_some()
                    });
                    let context =
                        models.resolved_context_config(config, state.model_override.as_ref());
                    let model = models.effective_model(state.model_override.as_ref());
                    let history = super::turn::context_history_preparation(&state.id, &model);
                    let limits = budget.limits();
                    let mut request = super::context_capture::context_input_evidence_request(
                        state,
                        None,
                        &context,
                        history,
                        Default::default(),
                        usize::try_from(limits.record_limit)?,
                        usize::try_from(
                            limits
                                .history_per_agent_bytes
                                .checked_add(limits.pending_per_agent_bytes)
                                .context("input recovery byte limit overflow")?,
                        )?,
                    )?;
                    super::scheduler::include_scheduler_brief_window(&mut request.windows);
                    if let Some(roles) = live_roles
                        .and_then(|queries| queries.input_for(scope, reservation.owner_epoch()))
                    {
                        roles.validate_binding(scope, budget)?;
                        crate::diagnostics::resource_io::admit_cloned_value(&roles.selection)?;
                        request.query = Some(super::resident_input_evidence::InputQueryRead {
                            selection: roles.selection.clone(),
                            ledger: budget.clone(),
                        });
                    }
                    let mut work_request = super::resident_work::WorkCaptureRequest::current();
                    use super::resident_work::{WorkFamily, WorkKey};
                    work_request.mandatory_roots.extend(
                        outcome
                            .install_keys
                            .work_items
                            .iter()
                            .map(|id| WorkKey::new(WorkFamily::Work, id)),
                    );
                    work_request.mandatory_roots.extend(
                        outcome
                            .install_keys
                            .wait_conditions
                            .iter()
                            .map(|id| WorkKey::new(WorkFamily::Wait, id)),
                    );
                    work_request.mandatory_roots.extend(
                        outcome
                            .install_keys
                            .continuations
                            .iter()
                            .map(|id| WorkKey::new(WorkFamily::Frame, id)),
                    );
                    work_request
                        .latest_result_work_ids
                        .extend(outcome.install_keys.work_items.iter().cloned());
                    work_request
                        .required_task_ids
                        .extend(outcome.install_keys.tasks.iter().cloned());
                    let mut task_request = super::resident_task::TaskCaptureRequest::current();
                    task_request
                        .required_keys
                        .extend(outcome.install_keys.tasks.iter().map(|id| {
                            super::resident_task::TaskKey::new(
                                super::resident_task::TaskFamily::Task,
                                id,
                            )
                        }));
                    task_request.required_keys.extend(
                        outcome
                            .install_keys
                            .task_result_settlements
                            .iter()
                            .map(|id| {
                                super::resident_task::TaskKey::new(
                                    super::resident_task::TaskFamily::ResultSettlement,
                                    id,
                                )
                            }),
                    );
                    let current = prepare_original_input_recovery_with_queries_tx(
                        tx,
                        scope,
                        state,
                        &request,
                        context.recent_episode_candidates,
                        budget,
                        &work_request,
                        &task_request,
                        live_roles.and_then(|queries| {
                            queries.selection_for(scope, reservation.owner_epoch())
                        }),
                    )?;
                    let work = current
                        .work
                        .context("input recovery omitted current Work domain")?;
                    let tasks = current
                        .tasks
                        .context("input recovery omitted current Task domain")?;
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
                    budget.install_actual_pending(scope, &charges)?;
                    let state = Arc::new(state.clone());
                    let episodes = Arc::new(current.episodes);
                    let inputs = Arc::new(current.inputs.with_budget(budget.clone())?);
                    let cache =
                        crate::runtime_db::input_recovery_install::load_current_install_facts_tx(
                            tx,
                            scope,
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
                        witness.input_runtime(&scope.agent_id).map(Arc::as_ref),
                        outcome,
                        receipt.outcome.applied,
                        facts.identity,
                        cache,
                    )?;
                    if let Some(roles) = live_roles
                        .and_then(|queries| queries.current_for(scope, reservation.owner_epoch()))
                    {
                        roles.validate(scope, reservation.owner_epoch(), roles.selection())?;
                        let query = crate::runtime_db::current_query::prepare(
                            tx,
                            scope,
                            Some(roles.selection()),
                            budget,
                            (request.byte_limit, request.record_limit),
                        )?;
                        runtime.current = Some(Arc::new(
                            crate::runtime_db::current_read_writes::CommittedCurrentWrites::prepared_query(query),
                        ));
                    }
                    acquisition.finish()?;
                    if witness.input_storage_replayed() {
                        runtime.suppress_observed_input_replay_publication();
                    }
                    let runtime = Arc::new(runtime);
                    known.push(KnownCommit::State {
                        work: Some(work),

                        tasks: Some(tasks),
                        scheduling: Some(current.scheduling),
                        state,
                        episodes: Some(episodes),
                        input_evidence: Some(inputs),
                        runtime: Some(runtime),
                    });
                }
                Ok(Some(known))
            },
        )?;
        // Validate again before the fallible whole-ticket release. The owner
        // gates stay unknown if ledger validation fails for any original member.
        MutationReservation::validate_input_unknown(reservations, &recovery.key, witness)?;
        if current.is_none() {
            witness.release_pending_not_committed(true)?;
        }
        if readonly_proofs.is_empty() {
            MutationReservation::resolve_input_unknown(
                reservations,
                &recovery.key,
                witness,
                current,
            )?;
        } else {
            MutationReservation::resolve_task_stop_input_unknown(
                reservations,
                &recovery.key,
                witness,
                current.context("readonly positive proof omitted its owner group")?,
                super::mutation_owner::OriginalInputRecoveryEvidence::Readonly(readonly_proofs),
            )?;
        }
        resolved = true;
    }
    Ok(resolved)
}

#[cfg(test)]
#[path = "input_recovery_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "input_recovery_cache_tests.rs"]
mod cache_tests;
#[cfg(test)]
#[path = "input_recovery_replay_tests.rs"]
mod replay_tests;
