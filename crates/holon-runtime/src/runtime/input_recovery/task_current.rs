//! Task-only receipt recovery: writer authority, reader acquisition, writer
//! requalification. Nothing in this module executes the original business write.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{ensure, Context, Result};

use super::super::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    mutation_owner::MutationReservation,
    mutation_witness::MutationWitness,
    resident_input_evidence::InputEvidenceScope,
    resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
    task_queries::TaskRecoveryQueries,
};
use crate::{
    runtime_db::{
        context_inputs::ContextDurableRead,
        input_mutation_receipts::{
            lookup_input_mutation_receipt_tx, with_fresh_input_mutation_preparation_and_preflight,
            InputMutationFamily, InputMutationReceipt, InputMutationResolution,
            InputMutationScopeOutcome, OriginalInputMutationAttempt,
        },
        task_recovery::{prepare_task_recovery_candidate_tx, TaskRecoveryCandidate},
        task_snapshot::{TaskDomainLoadRequest, TaskPreparationCandidateStale},
        RuntimeDb,
    },
    types::AgentRegistryStatus,
};

pub(super) struct TaskCurrentPreparation {
    receipt: Option<InputMutationReceipt>,
    candidates: BTreeMap<String, TaskRecoveryCandidate>,
    // Receipt bytes remain owned through the reader and the final writer.
    _receipt_guard: Arc<BudgetViewGuard>,
}

/// Reacquire only qualification on stale reader observations. The callback is
/// the existing current-view recovery preparation, never the business command.
pub(super) fn with_preparation<T, G>(
    db: &RuntimeDb,
    attempt: &OriginalInputMutationAttempt,
    witness: &MutationWitness,
    reservations: &[MutationReservation],
    request: &TaskDomainLoadRequest,
    budget: &InputEvidenceBudgetLedger,
    live: &[TaskRecoveryQueries],
    mut preflight: impl FnMut(&rusqlite::Transaction<'_>) -> Result<G>,
    mut current: impl FnMut(
        &rusqlite::Transaction<'_>,
        InputMutationResolution,
        &mut Option<TaskCurrentPreparation>,
    ) -> Result<T>,
) -> Result<T> {
    let task = attempt.prepared().family() == InputMutationFamily::Task;
    for retry in 0..3 {
        let mut preparation = if task {
            Some(prepare(
                db,
                attempt,
                witness,
                reservations,
                request,
                budget,
                live,
            )?)
        } else {
            None
        };
        let result = with_fresh_input_mutation_preparation_and_preflight(
            db,
            attempt,
            |tx| preflight(tx),
            |tx, resolution| {
                if let Some(preparation) = &preparation {
                    preparation.validate_receipt(attempt, &resolution)?;
                }
                current(tx, resolution, &mut preparation)
            },
        );
        match result {
            Err(error) if task && retry < 2 && error.is::<TaskPreparationCandidateStale>() => {
                // The next iteration returns to the original authority probe
                // and reader pool; it cannot repeat any original business write.
            }
            result => return result,
        }
    }
    unreachable!("finite Task qualification retry always returns")
}

impl TaskCurrentPreparation {
    pub(super) fn validate_receipt(
        &self,
        attempt: &OriginalInputMutationAttempt,
        current: &InputMutationResolution,
    ) -> Result<()> {
        ensure!(
            attempt.original_attempt_finished(),
            "Task receipt recovery original attempt is unfinished"
        );
        ensure!(
            match (&self.receipt, current) {
                (None, InputMutationResolution::Absent) => true,
                (Some(expected), InputMutationResolution::Present(actual)) => expected == actual,
                _ => false,
            },
            "Task receipt recovery original exact receipt changed"
        );
        Ok(())
    }

    pub(super) fn take(&mut self, scope: &InputEvidenceScope) -> Result<TaskRecoveryCandidate> {
        self.candidates
            .remove(&scope.agent_id)
            .ok_or_else(|| TaskPreparationCandidateStale("recovery scope became current").into())
    }
}

pub(super) fn task_selectors(outcome: &InputMutationScopeOutcome) -> TaskCaptureRequest {
    let mut capture = TaskCaptureRequest::current();
    capture.required_keys.extend(
        outcome
            .install_keys
            .tasks
            .iter()
            .map(|id| TaskKey::new(TaskFamily::Task, id)),
    );
    capture.required_keys.extend(
        outcome
            .install_keys
            .task_result_settlements
            .iter()
            .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id)),
    );
    capture
}

pub(super) fn prepare(
    db: &RuntimeDb,
    attempt: &OriginalInputMutationAttempt,
    witness: &MutationWitness,
    reservations: &[MutationReservation],
    request: &TaskDomainLoadRequest,
    budget: &InputEvidenceBudgetLedger,
    live: &[TaskRecoveryQueries],
) -> Result<TaskCurrentPreparation> {
    let result = (|| {
        ensure!(
            attempt.prepared().family() == InputMutationFamily::Task,
            "Task receipt recovery family differs"
        );
        // This is deliberately the original FIFO/Immediate writer, not an
        // advisory receipt lookup on the preparation pool.
        let (receipt, receipt_guard) = db.recovery_transaction_once(|tx| {
            ensure!(
                attempt.original_attempt_finished(),
                "Task receipt recovery original attempt is unfinished"
            );
            let scope = attempt
                .prepared()
                .scopes()
                .first()
                .context("Task receipt scope missing")?;
            let guard = crate::runtime_db::transitions::task_result_receipt_budget_tx(
                tx,
                attempt.prepared(),
                scope,
                request,
                budget,
            )?;
            Ok((
                lookup_input_mutation_receipt_tx(tx, attempt.prepared())?,
                guard,
            ))
        })?;
        let mut candidates = BTreeMap::new();
        if let Some(receipt) = &receipt {
            witness.mark_original_input_receipt_pending(receipt, true)?;
            db.read_for_preparation(|tx| {
                for outcome in &receipt.outcome.scopes {
                    let scope = &outcome.scope;
                    // The writer validates the original readonly proof without
                    // loading or replacing this owner's current domains.
                    if witness
                        .initial_task_stop_readonly(&scope.agent_id)
                        .is_some()
                    {
                        continue;
                    }
                    let read = ContextDurableRead::new(tx, &scope.agent_id);
                    // Retired namespace authority never reaches successor State.
                    let identity = read.identity_fact()?;
                    let Some(identity) = identity.as_ref().filter(|identity| {
                        identity.agent_id == scope.agent_id
                            && identity.incarnation == scope.identity_incarnation
                            && matches!(
                                identity.status,
                                AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
                            )
                    }) else {
                        continue;
                    };
                    let facts = read.scope_facts()?;
                    let Some(state) = facts.state.as_deref() else {
                        continue;
                    };
                    let reservation = reservations
                        .iter()
                        .find(|reservation| {
                            reservation.agent_id() == scope.agent_id
                                && reservation.identity_incarnation() == scope.identity_incarnation
                        })
                        .context("original Task recovery owner group is incomplete")?;
                    let acquisition =
                        crate::diagnostics::resource_io::ResourceReadMaterial::new(budget, scope)?;
                    let _entry = acquisition.enter();
                    let pending =
                        crate::runtime_db::bootstrap_inputs::prepare_pending_queue_read_tx(
                            tx, scope, budget,
                        )?;
                    ensure!(
                        pending.count == state.pending,
                        "Task recovery reader queue differs from current State"
                    );
                    let pending = pending.read_messages()?;
                    let query = (identity.status == AgentRegistryStatus::Active)
                        .then(|| {
                            live.iter().find_map(|queries| {
                                queries.selection_for(scope, reservation.owner_epoch())
                            })
                        })
                        .flatten();
                    let candidate = prepare_task_recovery_candidate_tx(
                        tx,
                        scope,
                        state,
                        &task_selectors(outcome),
                        &pending,
                        request.record_limit,
                        request.byte_limit,
                        budget,
                        query,
                    )?;
                    acquisition.finish()?;
                    ensure!(
                        candidates
                            .insert(scope.agent_id.clone(), candidate)
                            .is_none(),
                        "duplicate original Task recovery scope"
                    );
                }
                Ok(())
            })?;
        }
        Ok(TaskCurrentPreparation {
            receipt,
            candidates,
            _receipt_guard: receipt_guard,
        })
    })();
    result.map_err(crate::runtime_db::persistence::mark_original_input_recovery_failure)
}

#[cfg(all(test, holon_lib_test_shard = "a"))]
#[path = "task_current_tests.rs"]
mod tests;
