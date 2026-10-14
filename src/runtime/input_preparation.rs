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
    pub state: Arc<AgentState>,
    pub episodes: Arc<ResidentEpisodes>,
    pub inputs: Arc<ResidentInputEvidence>,
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
) -> Result<InputPreparation> {
    let scope = InputEvidenceScope {
        agent_id: reservation.agent_id().to_owned(),
        identity_incarnation: reservation.identity_incarnation(),
    };
    validate_ready_root(reservation, published, &scope)?;
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
    if scheduling_ready {
        if let Some(inputs) = &published.input_evidence {
            ensure!(inputs.belongs_to(&scope), "published input scope differs");
            match inputs.capture_for_request(request) {
                Ok(_) => {
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
                Err(error) if error.downcast_ref::<InputEvidenceReadError>().is_some() => {}
                Err(error) => return Err(error),
            }
        }
    }
    let prepared = db.transaction(|tx| {
        if cause == InputPreparationCause::ExistingAdmissionCleanup {
            crate::runtime_db::input_preparation::prepare_original_input_with_scheduling_tx(
                tx,
                &scope,
                &published.state,
                request,
                recent_episode_limit,
                budget,
                scheduling,
            )
        } else {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                tx,
                &scope,
                &published.state,
                request,
                recent_episode_limit,
                budget,
                scheduling,
            )
        }
    })?;
    // Sidecar preparation is idempotent and cannot change canonical facts.
    // A failed/indeterminate transaction leaves the old published root intact;
    // it does not resolve a business family's unknown ticket.
    validate_ready_root(reservation, published, &scope)?;
    let inputs = Arc::new(prepared.inputs.with_budget(budget.clone())?);
    let charges = prepared
        .pending
        .messages
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
    for message in prepared.pending.messages {
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
        state: published.state.clone(),
        episodes: Arc::new(prepared.episodes),
        inputs,
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
