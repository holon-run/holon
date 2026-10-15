//! Explicit inactive-child qualification under the caller's original owner.
//! This finite source never creates a Runtime, a Root or Ready authority.
use super::{
    context_inputs::ContextScopeFacts,
    current_read_snapshot::prepare_current_read_using_input_material_tx,
    input_evidence_snapshot::{
        prepare_input_evidence_snapshot_with_material_tx, read_material::InputReadMaterial,
        InputEvidenceBootstrapSnapshot, InputEvidenceLoadRequest, InputEvidenceSnapshotError,
        InputWindowRequest,
    },
    scheduling_snapshot::{read_cold_resource_scope_tx, SchedulingDomainLoadRequest},
    task_snapshot::TaskDomainLoadRequest,
    work_snapshot::{prepare_work_domain_snapshot_with_budget_tx, WorkDomainLoadRequest},
};
use crate::{
    runtime::{
        current_read_facts::{CurrentReadRequest, ResidentCurrentReadFacts},
        input_evidence_budget::{
            borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger,
        },
        resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceScope,
            InputWindowSelector, ResidentInputEvidence,
        },
        resident_scheduling::{ResidentSchedulingDomain, SchedulingCaptureRequest},
        resident_task::{ResidentTaskDomain, TaskCaptureRequest},
        resident_work::{ResidentWorkDomain, WorkCaptureRequest, WorkFamily, WorkKey},
    },
    types::AgentRegistryStatus,
};
use anyhow::{ensure, Context, Result};
use rusqlite::Transaction;
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ColdChildReadError {
    #[error("ResourceExhausted: cold child domains material")]
    ResourceExhausted,
    #[error("cold child domains belong to another original scope or ledger")]
    ForeignMaterial,
    #[error("cold child observation has no qualified State")]
    StateUnavailable,
}

/// Private accounting and immutable selected facts share one lifetime. No
/// database/Host/Runtime/Root handle, owner reservation or business grant escapes.
pub(crate) struct ColdChildDomains {
    facts: ContextScopeFacts,
    work: ResidentWorkDomain,
    tasks: ResidentTaskDomain,
    scheduling: ResidentSchedulingDomain,
    inputs: ResidentInputEvidence,
    current: Arc<ResidentCurrentReadFacts>,
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    own: (u64, usize),
    original_work_host_material: Vec<crate::runtime::resident_work::OriginalWorkHostAddressLoan>,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl ColdChildDomains {
    pub(crate) fn facts(&self) -> &ContextScopeFacts {
        &self.facts
    }
    pub(crate) fn work(&self) -> &ResidentWorkDomain {
        &self.work
    }
    pub(crate) fn tasks(&self) -> &ResidentTaskDomain {
        &self.tasks
    }
    pub(crate) fn scheduling(&self) -> &ResidentSchedulingDomain {
        &self.scheduling
    }
    pub(crate) fn inputs(&self) -> &ResidentInputEvidence {
        &self.inputs
    }
    pub(crate) fn current(&self) -> &ResidentCurrentReadFacts {
        &self.current
    }
    pub(crate) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope
                && self.ledger.same_ledger(ledger)
                && self.work.scope() == scope
                && self.tasks.scope() == scope
                && self.scheduling.scope() == scope
                && self.inputs.belongs_to(scope)
                && self.current.scope() == scope
                && !self.guards.is_empty(),
            ColdChildReadError::ForeignMaterial
        );
        ensure!(
            self.facts
                .identity
                .as_ref()
                .is_some_and(|identity| identity.agent_id == scope.agent_id
                    && identity.incarnation == scope.identity_incarnation
                    && identity.status == AgentRegistryStatus::Active)
                && self
                    .facts
                    .state
                    .as_ref()
                    .is_some_and(|state| state.id == scope.agent_id),
            ColdChildReadError::ForeignMaterial
        );
        for origin in &self.original_work_host_material {
            origin
                .validate_binding(scope, ledger)
                .context(ColdChildReadError::ForeignMaterial)?;
            ensure!(
                self.guards.iter().any(|guard| origin.matches_guard(guard)),
                ColdChildReadError::ForeignMaterial
            );
        }
        for guard in &self.guards {
            if self
                .original_work_host_material
                .iter()
                .any(|origin| origin.matches_guard(guard))
            {
                // Only the exact actual factory marker grants this accounting
                // classification. A failed scoped check never falls back here.
                guard
                    .validate_host_owned_material_binding(ledger, 0)
                    .context(ColdChildReadError::ForeignMaterial)?;
            } else {
                guard
                    .validate_owned_material_binding(scope, ledger, 0)
                    .context(ColdChildReadError::ForeignMaterial)?;
            }
        }
        Ok(())
    }
    /// Caller prior is already held by that caller and is never returned twice.
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        self.validate_accounting(&self.scope, &self.ledger)?;
        Ok(self.own)
    }
    #[cfg(test)]
    pub(crate) fn original_work_host_guards_for_test(
        &self,
    ) -> impl Iterator<Item = &Arc<BudgetViewGuard>> {
        self.original_work_host_material
            .iter()
            .map(|origin| origin.guard_for_test())
    }
    #[cfg(test)]
    pub(crate) fn first_guard_for_test(&self) -> &Arc<BudgetViewGuard> {
        &self.guards[0]
    }
}

fn phase_limits(
    ledger: &InputEvidenceBudgetLedger,
    requested: (usize, usize),
) -> Result<(usize, usize)> {
    let actual = ledger.limits();
    Ok((
        usize::try_from(
            actual
                .history_per_agent_bytes
                .checked_add(actual.pending_per_agent_bytes)
                .context(ColdChildReadError::ResourceExhausted)?
                .min(64 * 1024 * 1024),
        )?
        .min(requested.0),
        usize::try_from(actual.record_limit.min(65536))?.min(requested.1),
    ))
}
fn same_facts(expected: &ContextScopeFacts, actual: &ContextScopeFacts) -> Result<()> {
    ensure!(
        expected.identity == actual.identity
            && expected.state == actual.state
            && expected.control_revision == actual.control_revision,
        "cold child domains do not share their original canonical read view"
    );
    Ok(())
}
/// New selector containers, headers and closure addresses are admitted from
/// borrowed fields before copying; the existing loaders separately own bodies.
fn selector(material: &mut InputReadMaterial<'_>, value: &impl serde::Serialize) -> Result<()> {
    let (bytes, nodes) = borrowed_json_material(value)?;
    let native = nodes
        .checked_add(8)
        .context(ColdChildReadError::ResourceExhausted)?;
    let nodes = native
        .checked_mul(8)
        .context(ColdChildReadError::ResourceExhausted)?;
    let bytes = bytes
        .checked_add(
            u64::try_from(native)?
                .checked_mul(256)
                .context(ColdChildReadError::ResourceExhausted)?,
        )
        .and_then(|n| n.checked_mul(8))
        .context(ColdChildReadError::ResourceExhausted)?;
    material.observe_projected(bytes, nodes)
}

/// The Host must hold this exact Active incarnation's original reservation.
/// Ordinary fixed/current namespace loaders share one actual Input allowance;
/// no receipt/recovery/Deleting permission is borrowed for this cold cause.
/// StateNone is deliberately not converted into an empty source here. The
/// eventual consumer must separately prove its original all-domain emptiness.
pub(crate) fn prepare_cold_child_domains_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    requested: (usize, usize),
    caller_prior: (u64, usize),
) -> Result<ColdChildDomains> {
    let limits = phase_limits(ledger, requested)?;
    let prior = (usize::try_from(caller_prior.0)?, caller_prior.1);
    let remaining = (
        limits
            .0
            .checked_sub(prior.0)
            .context(ColdChildReadError::ResourceExhausted)?,
        limits
            .1
            .checked_sub(prior.1)
            .context(ColdChildReadError::ResourceExhausted)?,
    );
    // The existing cold header gate bounds identity first and rejects a retired
    // incarnation before even touching the successor State's payload/native tree.
    let header_request = SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        byte_limit: remaining.0,
        record_limit: remaining.1,
    };
    let (facts, header, mut guards) =
        read_cold_resource_scope_tx(tx, scope, &header_request, ledger)?;
    let state = facts
        .state
        .as_ref()
        .ok_or(ColdChildReadError::StateUnavailable)?;
    ensure!(
        facts.control_revision.is_some(),
        ColdChildReadError::StateUnavailable
    );
    ensure!(
        facts
            .identity
            .as_ref()
            .is_some_and(|identity| identity.agent_id == scope.agent_id
                && identity.incarnation == scope.identity_incarnation
                && identity.status == AgentRegistryStatus::Active),
        ColdChildReadError::ForeignMaterial
    );
    let admitted = (
        prior
            .0
            .checked_add(header.0)
            .context(ColdChildReadError::ResourceExhausted)?,
        prior
            .1
            .checked_add(header.1)
            .context(ColdChildReadError::ResourceExhausted)?,
    );
    let mut input_request = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        byte_limit: limits.0,
        record_limit: limits.1,
    };
    let mut material = InputReadMaterial::new(scope, &input_request, ledger, admitted)?;
    material.validate_accounting(scope, ledger)?;
    // Covers the finite aggregate/move headers and its owned scope, before any
    // derived request Vec/set or final source allocation is constructed.
    selector(&mut material, scope)?;
    input_request.windows.push(InputWindowRequest {
        selector: InputWindowSelector::RecentBriefs,
        limit: crate::runtime::SCHEDULER_BRIEF_LIMIT,
    });
    let task_request = TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest {
            include_active: true,
            include_unsettled: true,
            terminal_limit: 0,
            required_keys: BTreeSet::new(),
        },
        byte_limit: limits.0,
        record_limit: limits.1,
    };
    let task = material.with_task_material(scope, ledger, |task_material| {
        super::task_snapshot::load_selected_using_material(
            tx,
            scope,
            &task_request,
            true,
            false,
            false,
            Some(ledger),
            None,
            None,
            task_material,
        )?
        .require_current()
    })?;
    same_facts(&facts, &task.facts)?;
    let mut work_capture = WorkCaptureRequest::current();
    selector(&mut material, &task.work_roots)?;
    work_capture.mandatory_roots.extend(
        task.work_roots
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    let work_remaining = material.remaining_material_limits()?;
    let work = prepare_work_domain_snapshot_with_budget_tx(
        tx,
        scope,
        &WorkDomainLoadRequest {
            capture: work_capture,
            byte_limit: work_remaining.0,
            record_limit: work_remaining.1,
        },
        false,
        ledger,
        None,
    )?
    .require_current()?;
    same_facts(&facts, &work.facts)?;
    material.observe_work_prior(&work)?;
    let mut scheduling_request = SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        byte_limit: limits.0,
        record_limit: limits.1,
    };
    let scheduling = material.prepare_original_scheduling_tx(
        tx,
        scope,
        &mut scheduling_request,
        ledger,
        false,
    )?;
    same_facts(&facts, &scheduling.facts)?;
    let mut current_request = CurrentReadRequest::current(limits.0, limits.1);
    material.narrow_current_request(&mut current_request)?;
    let current = prepare_current_read_using_input_material_tx(
        tx,
        scope,
        state,
        &current_request,
        ledger,
        &mut material,
        false,
    )?;
    ensure!(
        Some(current.control_revision) == facts.control_revision,
        "cold child Current control revision differs"
    );
    selector(
        &mut material,
        &(
            &task.input_roots,
            &work.owning_input_roots,
            &scheduling.input_roots,
            &work.resident.coverage().latest_results,
            &state.current_execution_binding,
        ),
    )?;
    input_request
        .mandatory_roots
        .extend(task.input_roots.iter().cloned());
    input_request
        .mandatory_roots
        .extend(work.owning_input_roots.iter().cloned());
    input_request
        .mandatory_roots
        .extend(scheduling.input_roots.iter().cloned());
    input_request.selected_completion_brief_ids.extend(
        work.resident
            .coverage()
            .latest_results
            .values()
            .flatten()
            .map(|head| head.brief_id.clone()),
    );
    for selected in work.resident.context_plan_sources() {
        selector(&mut material, &selected.id)?;
        input_request
            .mandatory_roots
            .insert(Key::new(Family::WorkExecution, &selected.id));
    }
    if let Some(binding) = &state.current_execution_binding {
        input_request
            .mandatory_roots
            .insert(Key::new(Family::Message, &binding.source_message_id));
        input_request
            .mandatory_roots
            .insert(Key::new(Family::Turn, &binding.turn_id));
        if let Some(attempt) = &binding.activation_id {
            input_request
                .mandatory_roots
                .insert(Key::new(Family::Attempt, attempt));
        }
    }
    let InputEvidenceBootstrapSnapshot::Current(input) =
        prepare_input_evidence_snapshot_with_material_tx(
            tx,
            scope,
            &input_request,
            false,
            &mut material,
        )?
    else {
        return Err(InputEvidenceSnapshotError::RetiredScope.into());
    };
    same_facts(&facts, &input.facts)?;
    ensure!(
        input
            .resident
            .covers_work_input_sources(work.resident.coverage(), &work.owning_input_roots),
        "cold child owning Input sources differ from their qualified Work heads"
    );
    let usage = material.usage();
    let own = (
        u64::try_from(
            usage
                .0
                .checked_sub(prior.0)
                .context(ColdChildReadError::ResourceExhausted)?,
        )?,
        usage
            .1
            .checked_sub(prior.1)
            .context(ColdChildReadError::ResourceExhausted)?,
    );
    guards.extend(work.material_views);
    guards.extend(current._material);
    guards.extend(material.into_guards());
    let source = ColdChildDomains {
        facts,
        work: work.resident,
        tasks: task.resident,
        scheduling: scheduling.resident,
        inputs: input.resident,
        current: current.resident,
        scope: scope.clone(),
        ledger: ledger.clone(),
        own,
        guards,
        original_work_host_material: work.original_host_material,
    };
    source.validate_accounting(scope, ledger)?;
    Ok(source)
}
