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
        input_mutation_receipts::{with_fresh_input_mutation_resolution, InputMutationResolution},
        input_preparation::prepare_original_input_recovery_with_work_tx,
        RuntimeDb,
    },
    types::AgentRegistryStatus,
};

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
        let mut connection = db.connection()?;
        let current = with_fresh_input_mutation_resolution(
            &mut connection,
            attempt,
            |tx, result| {
                let InputMutationResolution::Present(receipt) = result else {
                    return Ok(None);
                };
                // Only this original positive receipt can promote these tickets.
                // Retrying preparation preserves this shared accepted observation.
                witness.mark_pending_result(receipt.outcome.applied, true)?;
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
                    let Some(state) = facts.state.as_ref() else {
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
                    let context =
                        models.resolved_context_config(config, state.model_override.as_ref());
                    let model = models.effective_model(state.model_override.as_ref());
                    let history = super::turn::context_history_preparation(&state.id, &model);
                    let limits = budget.limits();
                    let request = super::context_capture::context_input_evidence_request(
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
                    let current = prepare_original_input_recovery_with_work_tx(
                        tx,
                        scope,
                        state,
                        &request,
                        context.recent_episode_candidates,
                        budget,
                        &work_request,
                    )?;
                    let work = current
                        .work
                        .context("input recovery omitted current Work domain")?;
                    let charges = current
                        .pending
                        .messages
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
                        current.pending.messages,
                        budget,
                        witness.input_runtime(&scope.agent_id).map(Arc::as_ref),
                        outcome,
                        receipt.outcome.applied,
                        facts.identity,
                        cache,
                    )?;
                    if witness.input_storage_replayed() {
                        runtime.suppress_observed_input_replay_publication();
                    }
                    let runtime = Arc::new(runtime);
                    known.push(KnownCommit::State {
                        work: Some(work),
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
        MutationReservation::resolve_input_unknown(reservations, &recovery.key, witness, current)?;
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
