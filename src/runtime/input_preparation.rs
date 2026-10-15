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
    pub current_reads: Arc<super::current_read_facts::ResidentCurrentReadFacts>,
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
    let event_roles = reservation.current_event_roles(
        &scope,
        budget,
        (request.byte_limit, request.record_limit),
    )?;
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
    let current_ready = if let Some(current) = &published.current_reads {
        ensure!(
            current.scope() == &scope,
            "published CurrentRead scope differs"
        );
        current.validate_state(&published.state)?;
        current.validate_identity(
            published
                .identity
                .as_deref()
                .context("published actual identity requires preparation")?,
        )?;
        let coverage = current.coverage();
        let event_ready = event_roles
            .as_ref()
            .and_then(|roles| roles.birth_after())
            .is_none_or(|after| {
                coverage.event_query.birth_cursor.is_some_and(|cursor| {
                    after <= cursor
                        && (after == cursor
                            || coverage
                                .event_query
                                .birth_after
                                .is_some_and(|cut| cut <= after))
                })
            });
        event_ready
            && event_roles
                .as_ref()
                .and_then(|roles| roles.window_limits())
                .is_none_or(|(body, headers)| {
                    coverage.event_body_limit >= body
                        && coverage.event_header_limit >= headers
                        && (coverage.events_exhaustive || coverage.events.len() >= headers)
                })
            && coverage.queue_complete
            && coverage.event_body_limit >= super::current_read_facts::SCHEDULER_EVENT_BODY_WINDOW
            && (coverage.events_exhaustive
                || coverage.events.len() >= super::current_read_facts::SUMMARY_EVENT_COUNT_WINDOW)
    } else {
        false
    };
    if scheduling_ready && current_ready {
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
            crate::runtime_db::input_preparation::prepare_original_input_with_scheduling_and_event_roles_tx(
                tx,
                &scope,
                &published.state,
                request,
                recent_episode_limit,
                budget,
                scheduling,
                event_roles.as_ref(),
            )
        } else {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_and_event_roles_tx(
                tx,
                &scope,
                &published.state,
                request,
                recent_episode_limit,
                budget,
                scheduling,
                event_roles.as_ref(),
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
        current_reads: prepared.current_reads,
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

/// Test-only callback keeps private Runtime ownership and observation lifetime
/// inside Runtime; DB fixtures borrow only the original scope and ledger.
#[cfg(test)]
impl super::RuntimeHandle {
    pub(crate) async fn with_original_input_current_fixture_for_test<T>(
        agent: &str,
        read: impl FnOnce(
            &RuntimeDb,
            &InputEvidenceScope,
            &AgentState,
            &InputEvidenceBudgetLedger,
        ) -> Result<T>,
    ) -> Result<T> {
        let home = tempfile::tempdir()?;
        let runtime = Self::new_isolated_resource_fixture(
            agent,
            home.path().into(),
            super::tests::support::context_config(),
        )?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let reservation = runtime.inner.mutation_owner.reserve().await?;
        let root = reservation
            .capture_ready_snapshot()?
            .context("actual joint fixture root missing")?;
        let scope = InputEvidenceScope {
            agent_id: root.state.id.clone(),
            identity_incarnation: root.token.identity_incarnation,
        };
        read(
            runtime.runtime_db(),
            &scope,
            &root.state,
            &runtime.inner.input_evidence_budget,
        )
    }
}
