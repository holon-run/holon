//! Explicit owner-held preparation, never a context reader fallback.

use anyhow::{ensure, Result};
use rusqlite::Transaction;

mod scheduling_capture;

use super::{
    bootstrap_inputs::{pending_metadata_tx, pending_queue_inputs_tx, PreparedPendingQueue},
    context_inputs::{ContextDurableRead, ContextScopeFacts},
    episode_snapshot::{load_episode_memory_snapshot_in, EpisodeMemoryLoadScope},
    input_evidence_snapshot::{
        prepare_input_evidence_snapshot_tx, InputEvidenceBootstrapSnapshot,
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

pub(crate) struct PreparedInputRead {
    pub facts: ContextScopeFacts,
    pub episodes: ResidentEpisodes,
    pub inputs: ResidentInputEvidence,
    pub pending: PreparedPendingQueue,
    pub work: Option<std::sync::Arc<crate::runtime::resident_work::ResidentWorkDomain>>,
    pub tasks: Option<std::sync::Arc<crate::runtime::resident_task::ResidentTaskDomain>>,
    pub scheduling: std::sync::Arc<crate::runtime::resident_scheduling::ResidentSchedulingDomain>,
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
    )
}

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
    )
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
        true,
        Some(tasks),
        query,
        None,
        None,
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
) -> Result<PreparedInputRead> {
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
    prepare_input_domains_inner_tx(
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
    )
}

/// Explicit finite Scheduling preparation under the same existing owner and State.
pub(crate) fn prepare_input_with_scheduling_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    scheduling: &crate::runtime::resident_scheduling::SchedulingCaptureRequest,
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
    )
}
pub(crate) fn prepare_original_input_with_scheduling_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    state: &AgentState,
    request: &InputEvidenceLoadRequest,
    recent_episode_limit: usize,
    budget: &InputEvidenceBudgetLedger,
    scheduling: &crate::runtime::resident_scheduling::SchedulingCaptureRequest,
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
) -> Result<PreparedInputRead> {
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
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
    let scheduling_request = scheduling_capture::PreparedSchedulingRequest::new(
        scope,
        scheduling_capture,
        request.record_limit,
        request.byte_limit,
        budget,
    )?;
    // Count and conservative encoded bytes reject before Episode or input body
    // materialization. The later queue read is from this same SQL snapshot.
    let metadata = if original_recovery {
        super::bootstrap_inputs::pending_metadata_for_input_recovery_tx(tx, scope, budget)?
    } else {
        pending_metadata_tx(tx, scope, budget)?
    };
    ensure!(
        metadata.count == published_state.pending,
        "input preparation canonical pending queue differs from State"
    );
    let episode_scope = EpisodeMemoryLoadScope {
        agent_id: scope.agent_id.clone(),
        identity_incarnation: scope.identity_incarnation,
    };
    let current = if original_recovery {
        super::episode_snapshot::load_episode_memory_for_input_recovery_in(
            tx,
            &episode_scope,
            recent_episode_limit,
        )?
    } else {
        load_episode_memory_snapshot_in(tx, &episode_scope, recent_episode_limit, None)?
    }
    .require_current()?;
    let pending = if original_recovery {
        super::bootstrap_inputs::pending_queue_inputs_for_input_recovery_tx(tx, scope, budget)?
    } else {
        pending_queue_inputs_tx(tx, scope, budget)?
    };
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
            super::task_recovery::prepare_original_task_domains_with_queries_tx(
                tx,
                scope,
                published_state,
                capture,
                &pending.messages,
                request.record_limit,
                request.byte_limit,
                budget,
                query,
            )?
        };
        request
            .mandatory_roots
            .extend(task.input_roots.iter().cloned());
        Some(task)
    } else {
        None
    };
    let work = if let Some(capture) = work_request {
        let mut capture = capture.clone();
        if let Some(tasks) = &tasks {
            capture
                .mandatory_roots
                .extend(tasks.work_roots.iter().map(|id| {
                    crate::runtime::resident_work::WorkKey::new(
                        crate::runtime::resident_work::WorkFamily::Work,
                        id,
                    )
                }));
        }
        for message in &pending.messages {
            capture
                .required_trigger_message_ids
                .insert(message.id.clone());
            if message.kind == crate::types::MessageKind::TaskResult {
                if let crate::types::MessageOrigin::Task { task_id } = &message.origin {
                    ensure!(
                        message.task_id.as_deref() == Some(task_id.as_str()),
                        "pending TaskResult source differs"
                    );
                    capture.required_task_ids.insert(task_id.clone());
                }
            }
        }
        capture
            .latest_result_work_ids
            .extend(published_state.current_work_item_id.clone());
        let selected = capture.latest_result_work_ids.clone();
        let prepared = super::work_snapshot::prepare_original_work_recovery_snapshot_tx(
            tx,
            scope,
            &super::work_snapshot::WorkDomainLoadRequest {
                capture,
                record_limit: request.record_limit,
                byte_limit: request.byte_limit,
            },
        )?
        .require_current()?;
        ensure!(
            prepared.facts.identity == facts.identity
                && prepared.facts.state == facts.state
                && prepared.facts.control_revision == facts.control_revision,
            "Work recovery canonical read view differs"
        );
        if current_work_roots_only {
            request.mandatory_roots.extend(
                super::work_snapshot::current_recovery_owning_input_roots(
                    &prepared.resident,
                    published_state,
                )?,
            );
        } else {
            request.mandatory_roots.extend(prepared.owning_input_roots);
        }
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
        Some(std::sync::Arc::new(
            prepared.resident.with_budget(budget.clone())?,
        ))
    } else {
        None
    };
    let scheduling = if original_recovery {
        super::scheduling_snapshot::prepare_original_scheduling_recovery_snapshot_tx(
            tx,
            scope,
            &scheduling_request.request,
            budget,
        )?
    } else {
        super::scheduling_snapshot::prepare_scheduling_domain_snapshot_tx(
            tx,
            scope,
            &scheduling_request.request,
            budget,
        )?
    }
    .require_current()?;
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
    let prepared = if original_recovery {
        super::input_evidence_snapshot::prepare_original_input_recovery_snapshot_tx(
            tx, scope, &request,
        )?
    } else {
        prepare_input_evidence_snapshot_tx(tx, scope, &request)?
    };
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
    })
}
