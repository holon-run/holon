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
    pub current: Option<Arc<crate::runtime_db::current_read_writes::CommittedCurrentWrites>>,
    pub scheduling: Arc<super::resident_scheduling::ResidentSchedulingDomain>,
    pub queue: RuntimeQueue,
    pub queue_posture: crate::queue::QueuePostureFacts,
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
        || Ok(cause),
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
    resolve_cause: impl FnOnce() -> Result<InputPreparationCause>,
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
            domain.validate_capture_request(scheduling)
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
                        inputs.validate_prepared_request(request, &material)
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
                            .is_some() =>
                {
                    #[cfg(test)]
                    eprintln!(
                        "input preparation reload: agent={} readiness={error:#}",
                        scope.agent_id
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }
    // Covered requests need only the held publication. Resolve the durable
    // lifecycle policy once a miss requires an actual preparation read.
    let cause = resolve_cause()?;
    #[cfg(test)]
    eprintln!(
        "input preparation reload: agent={} cause={cause:?} scheduling_ready={scheduling_ready} current_ready={current_ready} inputs_present={}",
        scope.agent_id,
        published.input_evidence.is_some()
    );
    let current_query = |tx: &rusqlite::Transaction<'_>| {
        current
            .map(|roles| {
                let query = crate::runtime_db::current_query::prepare(
                    tx,
                    &scope,
                    Some(roles.selection()),
                    budget,
                    (request.byte_limit, request.record_limit),
                )?;
                Ok::<_, anyhow::Error>(Arc::new(
                    crate::runtime_db::current_read_writes::CommittedCurrentWrites::prepared_query(
                        query,
                    ),
                ))
            })
            .transpose()
    };
    let original_recovery = cause == InputPreparationCause::ExistingAdmissionCleanup;
    let read = || {
        let started = std::time::Instant::now();
        let result = db.read_for_preparation(|tx| {
            let prepared = crate::runtime_db::input_preparation::load_input_with_scheduling_tx(
                tx,
                &scope,
                &published.state,
                request,
                recent_episode_limit,
                budget,
                scheduling,
                original_recovery,
            )?;
            Ok((prepared, current_query(tx)?))
        });
        tracing::trace!(
            agent_id = %scope.agent_id,
            elapsed_us = started.elapsed().as_micros(),
            qualification_miss = ?result.as_ref().err().and_then(qualification_miss),
            success = result.is_ok(),
            "input preparation reader finished"
        );
        result
    };
    let mut loaded = read();
    if matches!(
        loaded.as_ref().err().and_then(qualification_miss),
        Some(QualificationMiss::SchedulingCoverage | QualificationMiss::SchedulingEvidence)
    ) {
        db.transaction_with_context(
            crate::runtime_db::write_queue::RuntimeDbWriteContext::sync(
                "runtime_db.input_preparation",
                "scheduling_qualification",
            ),
            |tx| {
                crate::runtime_db::input_preparation::prepare_scheduling_for_input_tx(
                    tx,
                    &scope,
                    &published.state,
                    request,
                    budget,
                    scheduling,
                    original_recovery,
                )
            },
        )?;
        loaded = read();
    }
    if loaded.as_ref().err().and_then(qualification_miss) == Some(QualificationMiss::BirthCursor) {
        use super::resident_input_evidence::InputQueryError;
        ensure!(
            request
                .query
                .as_ref()
                .and_then(|query| query.selection.birth_after)
                .unwrap_or(0)
                == 0,
            InputQueryError::FrontierNotPrepared
        );
        initialize_input_birth_cursor(db, &scope, &published.state, original_recovery)?;
        loaded = read();
    }
    let loaded = qualify_and_reload_input(
        db,
        &scope,
        &published.state,
        original_recovery,
        loaded,
        read,
    );
    let (prepared, current) = loaded?;
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
        queue_posture: prepared.queue_posture,
        original_token: published.token,
        facts: prepared.facts,
        state: published.state.clone(),
        episodes: Arc::new(prepared.episodes),
        inputs,
        current,
        scheduling: prepared.scheduling,
        queue,
        queue_budget_view,
    }))
}

#[derive(Debug, PartialEq, Eq)]
enum QualificationMiss {
    BirthCursor,
    InputEvidence,
    SchedulingEvidence,
    SchedulingCoverage,
    InputQuery,
}

/// Retry an owner-held read only when it identifies exact, eligible Input keys.
fn qualify_and_reload_input<T>(
    db: &RuntimeDb,
    scope: &InputEvidenceScope,
    state: &AgentState,
    original_recovery: bool,
    mut loaded: Result<T>,
    mut read: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut attempted_keys = std::collections::BTreeSet::new();
    while let Some(plan) = loaded.as_ref().err().and_then(|error| {
        error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputQualificationPlan>()
    }) {
        // Completion edges require the previous batch to be qualified before
        // the reader can discover another batch. Repeated keys cannot progress.
        if !plan
            .keys()
            .iter()
            .all(|key| attempted_keys.insert(key.clone()))
        {
            break;
        }
        tracing::debug!(agent_id = %scope.agent_id, keys = plan.keys().len(), "qualifying selected Input evidence");
        db.transaction_with_context(
            crate::runtime_db::write_queue::RuntimeDbWriteContext::sync(
                "runtime_db.input_preparation",
                "input_evidence_qualification",
            ),
            |tx| {
                crate::runtime_db::input_preparation::qualify_input_for_preparation_tx(
                    tx,
                    scope,
                    state,
                    plan,
                    original_recovery,
                )
            },
        )?;
        loaded = read();
    }
    loaded
}

/// The caller holds the same owner through the read and any sidecar repair.
/// A reader lease ends before any writer ticket is acquired.
pub(super) fn read_owner_input<T>(
    db: &RuntimeDb,
    scope: &InputEvidenceScope,
    state: &AgentState,
    original_recovery: bool,
    mut observe: impl FnMut(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let mut read = || db.read_for_preparation(&mut observe);
    let mut loaded = read();
    if loaded.as_ref().err().and_then(qualification_miss) == Some(QualificationMiss::BirthCursor) {
        // These finite selections never request a birth range.
        initialize_input_birth_cursor(db, scope, state, original_recovery)?;
        loaded = read();
    }
    qualify_and_reload_input(db, scope, state, original_recovery, loaded, read)
}

fn initialize_input_birth_cursor(
    db: &RuntimeDb,
    scope: &InputEvidenceScope,
    state: &AgentState,
    original_recovery: bool,
) -> Result<()> {
    db.transaction_with_context(
        crate::runtime_db::write_queue::RuntimeDbWriteContext::sync(
            "runtime_db.input_preparation",
            "input_query_birth_cursors",
        ),
        |tx| {
            crate::runtime_db::input_preparation::initialize_input_birth_cursor_tx(
                tx,
                scope,
                state,
                original_recovery,
            )
        },
    )
}

fn qualification_miss(error: &anyhow::Error) -> Option<QualificationMiss> {
    use crate::runtime::{
        resident_input_evidence::InputQueryError, resident_scheduling::SchedulingReadError,
    };
    use crate::runtime_db::{
        input_evidence_snapshot::{
            InputEvidenceSnapshotError, InputQualificationPlan, MissingInputBirthCursor,
        },
        scheduling_snapshot::SchedulingSnapshotError,
    };
    if error.is::<MissingInputBirthCursor>() {
        Some(QualificationMiss::BirthCursor)
    } else if error.is::<InputQualificationPlan>()
        || matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::NeedsPreparation(_))
        )
    {
        Some(QualificationMiss::InputEvidence)
    } else if matches!(
        error.downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::NeedsPreparation(_))
    ) {
        Some(QualificationMiss::SchedulingEvidence)
    } else if matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::IncompleteLive)
    ) {
        Some(QualificationMiss::SchedulingCoverage)
    } else if matches!(
        error.downcast_ref::<InputQueryError>(),
        Some(InputQueryError::NeedsPreparation)
    ) {
        Some(QualificationMiss::InputQuery)
    } else {
        None
    }
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

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "input_preparation_tests.rs"]
mod tests;
