//! Explicit owner-held preparation, never a context reader fallback.

use anyhow::{ensure, Result};
use rusqlite::Transaction;

mod scheduling_capture;

#[path = "input_recovery_material.rs"]
mod recovery_material;
pub(crate) use recovery_material::OriginalInputRecoveryMaterial;

#[cfg(test)]
#[path = "input_external_task_phase_tests.rs"]
mod external_task_phase_tests;

use super::{
    bootstrap_inputs::PreparedPendingQueue,
    context_inputs::ContextScopeFacts,
    input_evidence_snapshot::{
        prepare_input_evidence_snapshot_with_material_tx, InputEvidenceBootstrapSnapshot,
        InputEvidenceLoadRequest, InputEvidenceSnapshotError,
    },
};
use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceScope,
            ResidentInputEvidence,
        },
        resident_state::ResidentEpisodes,
    },
    types::{AgentRegistryStatus, AgentState},
};

pub(crate) struct PreparedInputRead<
    Current = std::sync::Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>,
> {
    pub facts: ContextScopeFacts,
    pub episodes: ResidentEpisodes,
    pub inputs: ResidentInputEvidence,
    pub pending: PreparedPendingQueue,
    pub work: Option<std::sync::Arc<crate::runtime::resident_work::ResidentWorkDomain>>,
    pub tasks: Option<std::sync::Arc<crate::runtime::resident_task::ResidentTaskDomain>>,
    pub scheduling: std::sync::Arc<crate::runtime::resident_scheduling::ResidentSchedulingDomain>,
    pub current_reads: Current,
    _work_material_views:
        Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    _input_material_views:
        Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

/// The caller holds the original Ready owner reservation throughout this
/// transaction and installation. Only qualification sidecars may be written.
#[cfg(test)]
pub(crate) fn prepare_input_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        false,
        None,
        false,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Explicit preparation for an already admitted cleanup, after owner identity
/// validation. Deleting reads only this scope's fixed/current qualification.
#[cfg(test)]
pub(crate) fn prepare_existing_admission_cleanup_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        true,
        None,
        false,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Original positive receipt recovery installs current Work and Input from
/// one view. The receipt supplies finite selectors, never old body authority.
#[cfg(test)]
pub(crate) fn prepare_original_input_recovery_with_work_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    work_request: &crate::runtime::resident_work::WorkCaptureRequest,
    task_request: &crate::runtime::resident_task::TaskCaptureRequest,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        true,
        Some(work_request),
        false,
        Some(task_request),
        None,
        None,
        None,
        None,
    )
}

#[cfg(test)]
pub(crate) fn prepare_original_input_recovery_with_queries_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    work: &crate::runtime::resident_work::WorkCaptureRequest,
    tasks: &crate::runtime::resident_task::TaskCaptureRequest,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
) -> Result<PreparedInputRead> {
    prepare_original_input_recovery_with_queries_and_event_roles_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        work,
        tasks,
        query,
        None,
    )
}

pub(crate) fn prepare_original_input_recovery_with_queries_and_event_roles_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    work: &crate::runtime::resident_work::WorkCaptureRequest,
    tasks: &crate::runtime::resident_task::TaskCaptureRequest,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        true,
        Some(work),
        false,
        Some(tasks),
        query,
        None,
        None,
        event_roles,
    )
}

/// An original positive receipt resolver lends the allocation phase that was
/// already guarded before its receipt decoder. No quota or body source resets.
pub(in crate::runtime_db) fn prepare_original_input_recovery_using_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    work: &crate::runtime::resident_work::WorkCaptureRequest,
    tasks: &crate::runtime::resident_task::TaskCaptureRequest,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    input_material: &mut super::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
) -> Result<PreparedInputRead> {
    input_material.validate_accounting(scope, budget)?;
    let mut current_request = crate::runtime::current_read_facts::CurrentReadRequest::current(
        request.byte_limit,
        request.record_limit,
    );
    if let Some(roles) = event_roles {
        roles.prepare_request(scope, budget, &mut current_request)?;
    }
    let read = prepare_input_domains_using_material_inner_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        true,
        Some(work),
        false,
        Some(tasks),
        query,
        None,
        None,
        None,
        current_request,
        input_material,
    )?;
    Ok(PreparedInputRead {
        facts: read.facts,
        episodes: read.episodes,
        inputs: read.inputs,
        pending: read.pending,
        work: read.work,
        tasks: read.tasks,
        scheduling: read.scheduling,
        current_reads: read
            .current_reads
            .ok_or(crate::runtime::current_read_facts::CurrentReadError::NeedsPreparation)?,
        _work_material_views: read._work_material_views,
        _input_material_views: read._input_material_views,
    })
}

pub(crate) fn prepare_control_recovery_with_queries_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    work: &crate::runtime::resident_work::WorkCaptureRequest,
    tasks: &crate::runtime::resident_task::TaskCaptureRequest,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    current: &super::control_transition::OriginalControlCurrentReadRecovery<'_, '_>,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<
    PreparedInputRead<
        Option<std::sync::Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>>,
    >,
> {
    prepare_input_domains_observation_inner_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        true,
        Some(work),
        true,
        Some(tasks),
        query,
        None,
        None,
        Some(OriginalCurrentReadRecovery::Control(current)),
        event_roles,
    )
}

/// Only a real Delivered Stop continuation may install fresh partial domains.
/// Task remains explicitly unprepared; this is not the ordinary Task loader.
pub(crate) fn prepare_partial_task_stop_input_recovery_tx(
    tx: &Transaction<'_>,
    view: &super::task_stop::TaskStopSparseReadView<'_, '_>,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    work: &crate::runtime::resident_work::WorkCaptureRequest,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<
    PreparedInputRead<
        Option<std::sync::Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>>,
    >,
> {
    view.validate_read_view_tx(tx, view.scope())?;
    let super::input_mutation_receipts::InputMutationResult::TaskStopPhase { ack } =
        &view.receipt().outcome.result
    else {
        anyhow::bail!("partial Stop recovery lacks its original phase ACK");
    };
    ensure!(
        matches!(&ack.result,Some(result) if matches!(result.outcome,super::task_stop::TaskStopResultOutcome::Delivered{..})),
        "only actual Delivered Stop Result can install new partial State"
    );
    prepare_input_domains_observation_inner_tx(
        tx,
        view.scope(),
        state,
        request,
        recent_episode_limit,
        budget,
        true,
        Some(work),
        true,
        None,
        None,
        None,
        None,
        Some(OriginalCurrentReadRecovery::DeliveredStop(view)),
        event_roles,
    )
}

/// Explicit finite Scheduling preparation under the same existing owner and State.
#[cfg(test)]
pub(crate) fn prepare_input_with_scheduling_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    scheduling: &crate::runtime::resident_scheduling::SchedulingCaptureRequest,
) -> Result<PreparedInputRead> {
    prepare_input_with_scheduling_and_event_roles_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        scheduling,
        None,
    )
}

pub(crate) fn prepare_input_with_scheduling_and_event_roles_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    scheduling: &crate::runtime::resident_scheduling::SchedulingCaptureRequest,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        false,
        None,
        false,
        None,
        None,
        None,
        Some(scheduling),
        event_roles,
    )
}
#[cfg(test)]
pub(crate) fn prepare_original_input_with_scheduling_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    scheduling: &crate::runtime::resident_scheduling::SchedulingCaptureRequest,
) -> Result<PreparedInputRead> {
    prepare_original_input_with_scheduling_and_event_roles_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        scheduling,
        None,
    )
}

pub(crate) fn prepare_original_input_with_scheduling_and_event_roles_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    scheduling: &crate::runtime::resident_scheduling::SchedulingCaptureRequest,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<PreparedInputRead> {
    prepare_input_domains_inner_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        true,
        None,
        false,
        None,
        None,
        None,
        Some(scheduling),
        event_roles,
    )
}

pub(in crate::runtime_db) fn prepare_child_terminal_input_recovery_tx(
    tx: &Transaction<'_>,
    view: &super::child_task_terminal::OriginalChildTerminalReadView<'_, '_>,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    work: &crate::runtime::resident_work::WorkCaptureRequest,
    tasks: &crate::runtime::resident_task::TaskCaptureRequest,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    material: &mut super::task_snapshot::read_preparation::TaskReadMaterial<'_>,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<PreparedInputRead> {
    let super::input_mutation_receipts::InputMutationResult::ChildTaskTerminal { ack } =
        &view.receipt().outcome.result
    else {
        anyhow::bail!("child recovery lacks its original terminal ACK");
    };
    let scope = &ack.original.scope;
    view.validate_tx(tx, scope)?;
    ensure!(
        view.mode(scope)? == super::child_task_terminal::ChildTaskTerminalInstallMode::Current,
        "child primary recovery cannot manufacture State"
    );
    prepare_input_domains_inner_tx(
        tx,
        scope,
        state,
        request,
        recent_episode_limit,
        budget,
        true,
        Some(work),
        false,
        Some(tasks),
        query,
        Some(material),
        None,
        event_roles,
    )
}

fn prepare_input_domains_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    original_recovery: bool,
    work_request: Option<&crate::runtime::resident_work::WorkCaptureRequest>,
    current_work_roots_only: bool,
    task_request: Option<&crate::runtime::resident_task::TaskCaptureRequest>,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    task_material: Option<&mut super::task_snapshot::read_preparation::TaskReadMaterial<'_>>,
    scheduling_capture: Option<&crate::runtime::resident_scheduling::SchedulingCaptureRequest>,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<PreparedInputRead> {
    let read = prepare_input_domains_observation_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        original_recovery,
        work_request,
        current_work_roots_only,
        task_request,
        query,
        task_material,
        scheduling_capture,
        None,
        event_roles,
    )?;
    Ok(PreparedInputRead {
        facts: read.facts,
        episodes: read.episodes,
        inputs: read.inputs,
        pending: read.pending,
        work: read.work,
        tasks: read.tasks,
        scheduling: read.scheduling,
        current_reads: read
            .current_reads
            .ok_or(crate::runtime::current_read_facts::CurrentReadError::NeedsPreparation)?,
        _work_material_views: read._work_material_views,
        _input_material_views: read._input_material_views,
    })
}

enum OriginalCurrentReadRecovery<'view, 'tx, 'conn> {
    Control(&'view super::control_transition::OriginalControlCurrentReadRecovery<'tx, 'conn>),
    DeliveredStop(&'view super::task_stop::TaskStopSparseReadView<'tx, 'conn>),
}
impl OriginalCurrentReadRecovery<'_, '_, '_> {
    fn prepare_current_tx(
        self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        state: &AgentState,
        request: &crate::runtime::current_read_facts::CurrentReadRequest,
        budget: &InputEvidenceBudgetLedger,
        input: &mut super::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
    ) -> Result<(
        Option<super::current_read_snapshot::CurrentReadSnapshot>,
        Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    )> {
        match self {
            Self::Control(current) => {
                current.prepare_current_tx(tx, scope, state, request, budget, input)
            }
            Self::DeliveredStop(view) => {
                view.validate_read_view_tx(tx, scope)?;
                let super::input_mutation_receipts::InputMutationResult::TaskStopPhase { ack } =
                    &view.receipt().outcome.result
                else {
                    anyhow::bail!("Stop CurrentRead recovery has no original phase ACK")
                };
                ensure!(
                    matches!(&ack.result,Some(result) if matches!(&result.outcome,super::task_stop::TaskStopResultOutcome::Delivered{..})),
                    "Stop CurrentRead recovery requires actual Delivered Result ACK"
                );
                match super::current_read_snapshot::prepare_current_read_using_input_material_tx(tx,scope,state,request,budget,input,true) {
                    Ok(snapshot)=>Ok((Some(snapshot),Vec::new())),
                    Err(error) if matches!(error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),
                        Some(crate::runtime::current_read_facts::CurrentReadError::UnknownNamespace(crate::runtime::current_read_facts::CurrentReadFamily::Queue)
                            |crate::runtime::current_read_facts::CurrentReadError::HistoricalUnavailable))=> {
                        let guards=super::current_read_snapshot::observe_original_incomplete_recovery_using_input_tx(tx,scope,state,request,budget,input)?;
                        Ok((None,guards))
                    }
                    Err(error)=>Err(error),
                }
            }
        }
    }
}

fn prepare_input_domains_observation_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    original_recovery: bool,
    work_request: Option<&crate::runtime::resident_work::WorkCaptureRequest>,
    current_work_roots_only: bool,
    task_request: Option<&crate::runtime::resident_task::TaskCaptureRequest>,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    task_material: Option<&mut super::task_snapshot::read_preparation::TaskReadMaterial<'_>>,
    scheduling_capture: Option<&crate::runtime::resident_scheduling::SchedulingCaptureRequest>,
    current_recovery: Option<OriginalCurrentReadRecovery<'_, '_, '_>>,
    event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
) -> Result<
    PreparedInputRead<
        Option<std::sync::Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>>,
    >,
> {
    ensure!(
        original_recovery || current_recovery.is_none(),
        "Control receipt cannot grant ordinary preparation"
    );
    // The held owner's live leases are read selectors, never receipt or business
    // permission. Reject their actual footprint before the first body decoder.
    let mut current_request = crate::runtime::current_read_facts::CurrentReadRequest::current(
        request.byte_limit,
        request.record_limit,
    );
    let event_prior = event_roles
        .map(|roles| roles.prepare_request(scope, budget, &mut current_request))
        .transpose()?
        .unwrap_or((0, 0));
    current_request.byte_limit = current_request
        .byte_limit
        .min(crate::runtime::current_read_facts::CURRENT_READ_MAX_BYTES)
        .checked_sub(event_prior.0)
        .ok_or(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted)?;
    current_request.record_limit = current_request
        .record_limit
        .min(crate::runtime::current_read_facts::CURRENT_READ_MAX_RECORDS)
        .checked_sub(event_prior.1)
        .ok_or(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted)?;
    if let Some(material) = task_material {
        return material.with_input_material_for_request(
            scope,
            budget,
            request,
            event_roles,
            |input_material| {
                prepare_input_domains_using_material_inner_tx(
                    tx,
                    scope,
                    published_state,
                    request,
                    recent_episode_limit,
                    budget,
                    original_recovery,
                    work_request,
                    current_work_roots_only,
                    task_request,
                    query,
                    None,
                    scheduling_capture,
                    current_recovery,
                    current_request,
                    input_material,
                )
            },
        );
    }
    let mut input_material = super::input_evidence_snapshot::read_material::InputReadMaterial::new(
        scope,
        request,
        budget,
        event_prior,
    )?;
    let mut read = prepare_input_domains_using_material_inner_tx(
        tx,
        scope,
        published_state,
        request,
        recent_episode_limit,
        budget,
        original_recovery,
        work_request,
        current_work_roots_only,
        task_request,
        query,
        None,
        scheduling_capture,
        current_recovery,
        current_request,
        &mut input_material,
    )?;
    read._input_material_views = input_material.into_guards();
    Ok(read)
}

/// A caller that already admitted receipt/source material keeps its original
/// phase alive through this same-view read; it grants no new recovery authority.
fn prepare_input_domains_using_material_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    published_state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    original_recovery: bool,
    work_request: Option<&crate::runtime::resident_work::WorkCaptureRequest>,
    current_work_roots_only: bool,
    task_request: Option<&crate::runtime::resident_task::TaskCaptureRequest>,
    query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
    task_material: Option<&mut super::task_snapshot::read_preparation::TaskReadMaterial<'_>>,
    scheduling_capture: Option<&crate::runtime::resident_scheduling::SchedulingCaptureRequest>,
    current_recovery: Option<OriginalCurrentReadRecovery<'_, '_, '_>>,
    mut current_request: crate::runtime::current_read_facts::CurrentReadRequest,
    mut input_material: &mut super::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
) -> Result<
    PreparedInputRead<
        Option<std::sync::Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>>,
    >,
> {
    input_material.validate_accounting(scope, budget)?;
    let facts = input_material.scope_facts_tx(tx)?;
    if !facts.identity.as_ref().is_some_and(|identity| {
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && (identity.status == AgentRegistryStatus::Active
                || (original_recovery && identity.status == AgentRegistryStatus::Deleting))
    }) {
        return Err(InputEvidenceSnapshotError::RetiredScope.into());
    }
    ensure!(
        facts.state.as_ref() == Some(published_state) && facts.control_revision.is_some(),
        "input preparation canonical State differs from its published root"
    );
    // The owned Scheduling selector must be admitted before its first clone.
    // Its guard lives beside the request through the nested Scheduling read.
    let mut scheduling_request =
        scheduling_capture::PreparedSchedulingRequest::using_input_material(
            scope,
            scheduling_capture,
            budget,
            &mut input_material,
        )?;
    // Count and conservative encoded bytes reject before Episode or input body
    // materialization. The later queue read is from this same SQL snapshot.
    let metadata = input_material.pending_metadata_tx(tx, scope, budget, original_recovery)?;
    ensure!(
        metadata.count == published_state.pending,
        "input preparation canonical pending queue differs from State"
    );
    let current = input_material.episode_snapshot_tx(
        tx,
        scope,
        budget,
        recent_episode_limit,
        original_recovery,
    )?;
    let pending = input_material.pending_queue_inputs_tx(tx, scope, budget, original_recovery)?;
    input_material.selectors(request)?;
    let mut request = request.clone();
    for message in &pending.messages {
        request
            .mandatory_roots
            .insert(Key::new(Family::Message, &message.id));
        // Snapshot preparation validates asserted recovery sources on its
        // same read view, including focused canonical absence observations.
    }
    let tasks = if let Some(capture) = task_request {
        let task = if let Some(material) = task_material {
            super::task_recovery::prepare_original_task_domains_using_material_tx(
                tx,
                scope,
                published_state,
                capture,
                &pending.messages,
                request.record_limit,
                request.byte_limit,
                budget,
                query,
                material,
            )?
        } else {
            input_material.with_task_material(scope, budget, |material| {
                super::task_recovery::prepare_original_task_domains_using_material_tx(
                    tx,
                    scope,
                    published_state,
                    capture,
                    &pending.messages,
                    request.record_limit,
                    request.byte_limit,
                    budget,
                    query,
                    material,
                )
            })?
        };
        request
            .mandatory_roots
            .extend(task.input_roots.iter().cloned());
        Some(task)
    } else {
        None
    };
    let mut work_material_views = Vec::new();
    let work = if let Some(capture) = work_request {
        let (prepared, selected) = input_material.prepare_original_work_tx(
            tx,
            scope,
            published_state,
            capture,
            tasks.as_ref().map(|task| &task.work_roots),
            &pending.messages,
            current_work_roots_only,
            budget,
        )?;
        ensure!(
            prepared.facts.identity == facts.identity
                && prepared.facts.state == facts.state
                && prepared.facts.control_revision == facts.control_revision,
            "Work recovery canonical read view differs"
        );
        request.mandatory_roots.extend(prepared.owning_input_roots);
        request.selected_completion_brief_ids.extend(
            prepared
                .resident
                .coverage()
                .latest_results
                .values()
                .flatten()
                .map(|head| head.brief_id.clone()),
        );
        request
            .selected_completion_brief_ids
            .extend(selected.iter().filter_map(|id| {
                prepared
                    .resident
                    .coverage()
                    .latest_results
                    .get(id)
                    .and_then(Option::as_ref)
                    .map(|head| head.brief_id.clone())
            }));
        work_material_views = prepared.material_views;
        Some(std::sync::Arc::new(
            prepared.resident.with_budget(budget.clone())?,
        ))
    } else {
        None
    };
    let scheduling = input_material.prepare_original_scheduling_tx(
        tx,
        scope,
        &mut scheduling_request.request,
        budget,
        original_recovery,
    )?;
    ensure!(
        scheduling.facts.identity == facts.identity
            && scheduling.facts.state == facts.state
            && scheduling.facts.control_revision == facts.control_revision,
        "Scheduling preparation canonical view differs"
    );
    request
        .mandatory_roots
        .extend(scheduling.input_roots.iter().cloned());
    let scheduling = std::sync::Arc::new(scheduling.resident);

    // Current's actual decoder and retained view coexist with this original
    // Input/Work/Event material; independent full-domain limits are insufficient.
    input_material.narrow_current_request(&mut current_request)?;
    let current_read = if let Some(recovery) = current_recovery {
        let (current, guards) = recovery.prepare_current_tx(
            tx,
            scope,
            published_state,
            &current_request,
            budget,
            &mut input_material,
        )?;
        work_material_views.extend(guards);
        current
    } else if original_recovery {
        Some(
            super::current_read_snapshot::prepare_current_read_using_input_material_tx(
                tx,
                scope,
                published_state,
                &current_request,
                budget,
                &mut input_material,
                true,
            )?,
        )
    } else {
        Some(
            super::current_read_snapshot::prepare_current_read_using_input_material_tx(
                tx,
                scope,
                published_state,
                &current_request,
                budget,
                &mut input_material,
                false,
            )?,
        )
    };
    let current_reads = if let Some(current_read) = current_read {
        ensure!(
            Some(current_read.control_revision) == facts.control_revision,
            "CurrentRead preparation canonical read view differs"
        );
        work_material_views.extend(current_read._material);
        Some(current_read.resident)
    } else {
        None
    };
    let prepared = prepare_input_evidence_snapshot_with_material_tx(
        tx,
        scope,
        &request,
        original_recovery,
        &mut input_material,
    )?;
    let input = match prepared {
        InputEvidenceBootstrapSnapshot::Current(input) => input,
        InputEvidenceBootstrapSnapshot::Retired {
            requested_scope,
            current_identity,
        } => {
            return Err(
                anyhow::Error::from(InputEvidenceSnapshotError::RetiredScope).context(format!(
                    "input preparation scope {:?} retired; current identity {:?}",
                    requested_scope, current_identity
                )),
            );
        }
    };
    ensure!(
        facts.identity.as_ref() == Some(&current.identity)
            && facts.state == current.state
            && facts.control_revision == current.control_revision
            && input.facts.identity == facts.identity
            && input.facts.state == facts.state
            && input.facts.control_revision == facts.control_revision
            && pending.metadata.facts.identity == facts.identity
            && pending.metadata.facts.state == facts.state
            && pending.metadata.facts.control_revision == facts.control_revision,
        "prepared domains do not share their canonical read view"
    );
    Ok(PreparedInputRead {
        facts,
        episodes: ResidentEpisodes::bootstrap(
            &scope.agent_id,
            scope.identity_incarnation,
            recent_episode_limit,
            !current.has_more,
            current.episodes,
        )?,
        inputs: input.resident,
        pending,
        work,
        tasks: tasks.map(|tasks| tasks.resident),
        scheduling,
        current_reads,
        _work_material_views: work_material_views,
        _input_material_views: Vec::new(),
    })
}
