//! Explicit preparation at an owner boundary. Ordinary readers never call SQL.

use std::sync::Arc;

use anyhow::{ensure, Context, Result};

use super::{
    input_evidence_budget::{
        BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, PendingBodyCharge,
    },
    mutation_owner::{MutationReservation, PublicationToken},
    resident_input_evidence::{InputEvidenceReadError, InputEvidenceScope, ResidentInputEvidence},
    resident_state::{AgentSnapshot, ResidentEpisodes},
};
use crate::{
    queue::RuntimeQueue,
    runtime_db::{
        context_inputs::ContextScopeFacts, input_evidence_snapshot::InputEvidenceLoadRequest,
        RuntimeDb,
    },
    types::AgentState,
};

/// These causes belong to the caller's explicit owner transition. A missing
/// key/window is a preparation need, never authority for an ordinary read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputPreparationCause {
    ValidatedRecovery,
    OwnerChange,
    ExistingAdmissionCleanup,
}

pub(crate) struct PreparedInputDomains {
    pub original_token: PublicationToken,
    pub facts: ContextScopeFacts,
    pub decoded_state: Option<crate::runtime_db::context_inputs::ContextStateDecode>,
    pub state: Arc<AgentState>,
    pub episodes: Arc<ResidentEpisodes>,
    pub inputs: Arc<ResidentInputEvidence>,
    pub current: Option<Arc<crate::runtime_db::current_read_writes::CommittedCurrentWrites>>,
    pub scheduling: Arc<super::resident_scheduling::ResidentSchedulingDomain>,
    pub queue: RuntimeQueue,
    /// Retain the observation with the prepared queue until core installs a
    /// Current view. This guard owns no body and never grants queue authority.
    pub queue_budget_view: Arc<BudgetViewGuard>,
}

pub(crate) enum InputPreparation {
    AlreadyPrepared,
    Prepared(PreparedInputDomains),
}

/// No new owner, scheduler, canonical business commit or recovery ticket is
/// created. Call only after every original known/unknown group is reconciled.
/// The caller must keep this same reservation through its short install and
/// publish step, preserving original run handles and other resident domains.
#[cfg(test)]
pub(crate) fn prepare_reserved_input_domains(
    db: &RuntimeDb,
    reservation: &MutationReservation,
    published: &AgentSnapshot,
    cause: InputPreparationCause,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<InputPreparation> {
    prepare_reserved_input_domains_with_scheduling(
        db,
        reservation,
        published,
        cause,
        request,
        recent_episode_limit,
        budget,
        &super::resident_scheduling::SchedulingCaptureRequest::current(),
        None,
    )
}

pub(crate) fn prepare_reserved_input_domains_with_scheduling(
    db: &RuntimeDb,
    reservation: &MutationReservation,
    published: &AgentSnapshot,
    cause: InputPreparationCause,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    scheduling: &super::resident_scheduling::SchedulingCaptureRequest,
    current: Option<&super::current_queries::CurrentRoles>,
) -> Result<InputPreparation> {
    let scope = InputEvidenceScope {
        agent_id: reservation.agent_id().to_owned(),
        identity_incarnation: reservation.identity_incarnation(),
    };
    validate_ready_root(reservation, published, &scope)?;
    let current_ready = match current {
        None => true,
        Some(roles) => {
            ensure!(
                roles.ledger().same_ledger(budget),
                "Current preparation ledger differs"
            );
            roles.validate(&scope, published.token.owner_epoch, roles.selection())?;
            match published
                .current
                .as_ref()
                .and_then(|writes| writes.query.as_ref())
            {
                None => false,
                Some(query) => {
                    query.validate_binding(&scope, budget)?;
                    query.selection.covers(roles.selection())
                }
            }
        }
    };
    let scheduling_ready = published
        .scheduling
        .as_ref()
        .map(|domain| {
            ensure!(
                domain.scope() == &scope,
                "published Scheduling scope differs"
            );
            domain.capture(scheduling).map(|_| ())
        })
        .transpose();
    let scheduling_ready = match scheduling_ready {
        Ok(Some(())) => true,
        Ok(None) => false,
        Err(error)
            if error
                .downcast_ref::<super::resident_scheduling::SchedulingReadError>()
                .is_some() =>
        {
            false
        }
        Err(error) => return Err(error),
    };
    if scheduling_ready && current_ready {
        if let Some(inputs) = &published.input_evidence {
            ensure!(inputs.belongs_to(&scope), "published input scope differs");
            let material = crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
                budget, &scope,
            )?;
            let ready = inputs
                .preparation_request_is_covered(request, &material)
                .and_then(|covered| {
                    if covered {
                        Ok(())
                    } else {
                        inputs
                            .capture_for_request_with_material(request, &material)
                            .map(|_| ())
                    }
                });
            match ready {
                Ok(()) => {
                    if let Some(episodes) = &published.episodes {
                        ensure!(
                            episodes.belongs_to(&scope.agent_id, scope.identity_incarnation),
                            "published Episode scope differs"
                        );
                        if episodes.selected(recent_episode_limit).is_ok() {
                            return Ok(InputPreparation::AlreadyPrepared);
                        }
                    }
                }
                Err(error)
                    if error.downcast_ref::<InputEvidenceReadError>().is_some()
                        || error
                            .downcast_ref::<super::resident_input_evidence::InputQueryError>()
                            .is_some() => {}
                Err(error) => return Err(error),
            }
        }
    }
    let (prepared, current) = db.transaction(|tx| {
        let prepared = if cause == InputPreparationCause::ExistingAdmissionCleanup {
            crate::runtime_db::input_preparation::prepare_original_input_with_scheduling_tx(
                tx,
                &scope,
                &published.state,
                request,
                recent_episode_limit,
                budget,
                scheduling,
            )?
        } else {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                tx,
                &scope,
                &published.state,
                request,
                recent_episode_limit,
                budget,
                scheduling,
            )?
        };
        let current =
            current
                .map(|roles| {
                    crate::runtime_db::current_query::prepare(
                tx, &scope, Some(roles.selection()), budget,
                (request.byte_limit, request.record_limit),
            ).map(crate::runtime_db::current_read_writes::CommittedCurrentWrites::prepared_query)
                .map(Arc::new)
                })
                .transpose()?;
        Ok((prepared, current))
    })?;
    // Sidecar preparation is idempotent and cannot change canonical facts.
    // A failed/indeterminate transaction leaves the old published root intact;
    // it does not resolve a business family's unknown ticket.
    validate_ready_root(reservation, published, &scope)?;
    let inputs = Arc::new(prepared.inputs.with_budget(budget.clone())?);
    let charges = prepared
        .pending
        .iter()
        .map(|message| {
            Ok(PendingBodyCharge {
                message_id: message.id.clone(),
                body_bytes: u64::try_from(serde_json::to_vec(&message.body)?.len())?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    budget.install_actual_pending(&scope, &charges)?;
    let mut queue = RuntimeQueue::default();
    for message in prepared.pending {
        queue.push(message);
    }
    let queue_budget_view = budget.register_queue_view(
        &scope,
        InputMemoryCategory::Candidate,
        queue.shared_messages(),
    )?;
    tracing::debug!(agent_id=%scope.agent_id, incarnation=scope.identity_incarnation,
        owner_epoch=published.token.owner_epoch, generation=published.token.generation,
        ?cause, "prepared input domains at explicit owner boundary");
    Ok(InputPreparation::Prepared(PreparedInputDomains {
        original_token: published.token,
        facts: prepared.facts,
        decoded_state: prepared.decoded_state,
        state: published.state.clone(),
        episodes: Arc::new(prepared.episodes),
        inputs,
        current,
        scheduling: prepared.scheduling,
        queue,
        queue_budget_view,
    }))
}

fn validate_ready_root(
    reservation: &MutationReservation,
    published: &AgentSnapshot,
    scope: &InputEvidenceScope,
) -> Result<()> {
    let current = reservation
        .capture_ready_snapshot()?
        .context("input preparation requires an already published State root")?;
    ensure!(
        current.token == published.token
            && current.state == published.state
            && current.state.id == scope.agent_id
            && current.token.identity_incarnation == scope.identity_incarnation
            && current.token.owner_epoch == reservation.owner_epoch(),
        "input preparation reservation/publication generation differs"
    );
    Ok(())
}

#[cfg(test)]
#[path = "input_preparation_tests.rs"]
mod tests;
