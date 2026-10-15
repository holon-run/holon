//! Loaded child observations retain their original selected facts through use.
use super::input_evidence_budget::{
    borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use super::state_mutation::PreparedSchedulerRead;
use super::*;
use crate::types::{AgentIdentityRecord, ChildAgentSummary};
use anyhow::ensure;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChildAgentSummaryReadError {
    #[error("ResourceExhausted: child agent summary material")]
    ResourceExhausted,
    #[error("child agent summary differs from its original scope or ledger")]
    ForeignMaterial,
}
/// The public DTO and its actual selected material move together. Neither part
/// retains a Root, Runtime, owner reservation or database handle.
pub(crate) struct OwnedChildAgentSummaryRead {
    summary: ChildAgentSummary,
    material: ChildAgentSummaryReadMaterial,
}
pub(crate) struct ChildAgentSummaryReadMaterial {
    read: PreparedSchedulerRead,
    identity: Arc<AgentIdentityRecord>,
    identity_guard: Arc<BudgetViewGuard>,
    ledger: InputEvidenceBudgetLedger,
    output: Vec<Arc<BudgetViewGuard>>,
    output_cost: (u64, usize),
    own: (u64, usize),
}
impl OwnedChildAgentSummaryRead {
    pub(crate) fn into_parts(self) -> (ChildAgentSummary, ChildAgentSummaryReadMaterial) {
        (self.summary, self.material)
    }
}
impl ChildAgentSummaryReadMaterial {
    pub(crate) fn validate_accounting(&self, ledger: &InputEvidenceBudgetLedger) -> Result<()> {
        let scope = &self.read.work.scope;
        ensure!(
            self.ledger.same_ledger(ledger)
                && self.identity.agent_id == scope.agent_id
                && self.identity.incarnation == scope.identity_incarnation
                && self.read.state.id == scope.agent_id,
            ChildAgentSummaryReadError::ForeignMaterial
        );
        self.identity_guard
            .validate_owned_material_binding(scope, ledger, 0)
            .context(ChildAgentSummaryReadError::ForeignMaterial)?;
        self.read
            ._owned_budget
            .validate_owned_material_binding(scope, ledger, 0)
            .context(ChildAgentSummaryReadError::ForeignMaterial)?;
        let mut output = 0u64;
        for guard in &self.output {
            guard
                .validate_owned_material_binding(scope, ledger, 0)
                .context(ChildAgentSummaryReadError::ForeignMaterial)?;
            output = output
                .checked_add(guard.owned_material_bytes()?)
                .context(ChildAgentSummaryReadError::ResourceExhausted)?;
        }
        ensure!(
            output == self.output_cost.0,
            ChildAgentSummaryReadError::ForeignMaterial
        );
        Ok(())
    }
    /// Includes the actual child identity and selected source once; excludes
    /// the caller's still-held prior, which remains with that caller.
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        self.validate_accounting(&self.ledger)?;
        Ok(self.own)
    }
}
fn add(a: (u64, usize), b: (u64, usize)) -> Result<(u64, usize)> {
    Ok((
        a.0.checked_add(b.0)
            .context(ChildAgentSummaryReadError::ResourceExhausted)?,
        a.1.checked_add(b.1)
            .context(ChildAgentSummaryReadError::ResourceExhausted)?,
    ))
}
fn limits(ledger: &InputEvidenceBudgetLedger, requested: (usize, usize)) -> Result<(u64, usize)> {
    let actual = ledger.limits();
    Ok((
        actual
            .history_per_agent_bytes
            .checked_add(actual.pending_per_agent_bytes)
            .context(ChildAgentSummaryReadError::ResourceExhausted)?
            .min(64 * 1024 * 1024)
            .min(u64::try_from(requested.0)?),
        usize::try_from(actual.record_limit.min(65_536))?.min(requested.1),
    ))
}
fn admit(limits: (u64, usize), total: (u64, usize)) -> Result<()> {
    ensure!(
        total.0 <= limits.0 && total.1 <= limits.1,
        ChildAgentSummaryReadError::ResourceExhausted
    );
    Ok(())
}
// Only already-selected candidate addresses are measured. This is not another
// continuation selection algorithm and does not inspect any unselected body.
struct ClosureWorkAddresses<'a>(&'a scheduler::SchedulerProjection);
impl serde::Serialize for ClosureWorkAddresses<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(None)?;
        for item in self
            .0
            .current_work_item
            .iter()
            .chain(&self.0.queued_runnable_work_items)
        {
            // The actual semantic candidate owns these scalar/address fields.
            // Max generation digits cover both None and any actual generation.
            sequence.serialize_element(&(
                item.id.as_str(),
                item.revision,
                u64::MAX,
                "continue_active",
            ))?;
        }
        sequence.end()
    }
}
fn closure_material(
    state: &AgentState,
    projection: &scheduler::SchedulerProjection,
) -> Result<(u64, usize)> {
    let (bytes, nodes) = borrowed_json_material(&(
        &state.id,
        &projection.active_run_id,
        ClosureWorkAddresses(projection),
        state
            .last_turn_terminal
            .as_ref()
            .map(|terminal| terminal.kind),
    ))?;
    // Original semantic selection creates candidate/context/snapshot/proposal,
    // then resolves against a second context. Sixteen borrowed candidate copies
    // cover all these overlapping instances, signal copies and WorkID evidence.
    // At most 13 evidence strings/format results have <=64 fixed bytes each;
    // 8192/64 also covers Vec/DTO headers and native scalar formatting scratch.
    let nodes = nodes
        .checked_mul(16)
        .and_then(|value| value.checked_add(64))
        .context(ChildAgentSummaryReadError::ResourceExhausted)?;
    let bytes = bytes
        .checked_mul(16)
        .and_then(|value| value.checked_add(8192))
        .and_then(|value| value.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
        .context(ChildAgentSummaryReadError::ResourceExhausted)?;
    Ok((bytes, nodes))
}
struct ChildOutput<'a> {
    ledger: &'a InputEvidenceBudgetLedger,
    scope: &'a super::resident_input_evidence::InputEvidenceScope,
    limits: (u64, usize),
    prior: (u64, usize),
    cost: (u64, usize),
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl<'a> ChildOutput<'a> {
    fn new(
        ledger: &'a InputEvidenceBudgetLedger,
        scope: &'a super::resident_input_evidence::InputEvidenceScope,
        limits: (u64, usize),
        prior: (u64, usize),
    ) -> Result<Self> {
        // This pays for the private wrapper and its fixed-capacity guard slots
        // before constructing either. The original builder adds two charges.
        let cost = (
            u64::try_from(std::mem::size_of::<ChildAgentSummaryReadMaterial>())?
                .checked_add(8192)
                .context(ChildAgentSummaryReadError::ResourceExhausted)?,
            32,
        );
        admit(limits, add(prior, cost)?)?;
        let guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: cost.0,
            },
        )?;
        let mut guards = Vec::with_capacity(8);
        guards.push(guard);
        Ok(Self {
            ledger,
            scope,
            limits,
            prior,
            cost,
            guards,
        })
    }
    fn observe(&mut self, bytes: u64, nodes: usize) -> Result<()> {
        ensure!(
            self.guards.len() < self.guards.capacity(),
            ChildAgentSummaryReadError::ResourceExhausted
        );
        let cost = add(self.cost, (bytes, nodes))?;
        admit(self.limits, add(self.prior, cost)?)?;
        let guard = self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        self.guards.push(guard);
        self.cost = cost;
        Ok(())
    }
    fn json(&mut self, value: &impl serde::Serialize) -> Result<()> {
        let (bytes, nodes) = borrowed_json_material(value)?;
        let nodes = nodes
            .checked_add(1)
            .context(ChildAgentSummaryReadError::ResourceExhausted)?;
        self.observe(
            bytes
                .checked_add(
                    u64::try_from(nodes)?
                        .checked_mul(256)
                        .context(ChildAgentSummaryReadError::ResourceExhausted)?,
                )
                .context(ChildAgentSummaryReadError::ResourceExhausted)?,
            nodes,
        )
    }
    fn closure_waiting_reason(
        &mut self,
        state: &AgentState,
        projection: &scheduler::SchedulerProjection,
    ) -> Result<Option<WaitingReason>> {
        let cost = closure_material(state, projection)?;
        self.observe(cost.0, cost.1)?;
        Ok(RuntimeHandle::closure_decision_from_projection(state, projection, None).waiting_reason)
    }
    fn slots(&mut self, count: usize) -> Result<()> {
        let nodes = count
            .checked_add(1)
            .context(ChildAgentSummaryReadError::ResourceExhausted)?;
        self.observe(
            u64::try_from(nodes)?
                .checked_mul(256)
                .context(ChildAgentSummaryReadError::ResourceExhausted)?,
            nodes,
        )
    }
}
impl RuntimeHandle {
    /// Accounting-only validation before a Host lends its still-held prior.
    /// It neither reads a publication nor grants record/identity access.
    pub(crate) fn child_summary_uses_ledger(&self, ledger: &InputEvidenceBudgetLedger) -> bool {
        self.inner.input_evidence_budget.same_ledger(ledger)
    }
    fn child_summary_from_owner_with_material(
        &self,
        owner: &mutation_owner::MutationReservation,
        expected: &AgentIdentityRecord,
        requested: (usize, usize),
        caller_prior: (u64, usize),
    ) -> Result<OwnedChildAgentSummaryRead> {
        owner.validate_owner(&self.inner.mutation_owner)?;
        let root = owner
            .capture_ready_snapshot()?
            .ok_or(super::current_read_facts::CurrentReadError::RetiredScope)?;
        let identity = root.active_identity()?;
        // Host's original selected address cannot silently follow a successor
        // or move a child to a different parent. The view uses this same Root.
        ensure!(
            identity.agent_id == expected.agent_id
                && identity.incarnation == expected.incarnation
                && identity.kind == expected.kind
                && identity.parent_agent_id == expected.parent_agent_id,
            ChildAgentSummaryReadError::ForeignMaterial
        );
        let scope = root
            .work
            .as_ref()
            .ok_or(super::resident_work::WorkReadError::IncompleteLive)?
            .scope();
        let ledger = &self.inner.input_evidence_budget;
        let identity_guard = root
            ._identity_budget
            .as_ref()
            .context("child identity has no original material")?;
        identity_guard
            .validate_owned_material_binding(scope, ledger, 0)
            .context(ChildAgentSummaryReadError::ForeignMaterial)?;
        let phase_limits = limits(ledger, requested)?;
        let identity_cost = (identity_guard.owned_material_bytes()?, 24 * 8);
        let source_prior = add(caller_prior, identity_cost)?;
        admit(phase_limits, source_prior)?;
        let read = self.capture_scheduler_read_from_snapshot_with_material(
            &root,
            &super::resident_work::WorkCaptureRequest::current(),
            self.now(),
            requested,
            source_prior,
            None,
        )?;
        ensure!(
            Arc::ptr_eq(&read.state, &root.state),
            ChildAgentSummaryReadError::ForeignMaterial
        );
        let own_source = add(identity_cost, read.material_estimate()?)?;
        let active_task_count = super::task_reads::active_task_count(&read.tasks)?;
        ensure!(
            active_task_count == read.projection.active_tasks.len(),
            "child summary Task metadata differs from selected active facts"
        );
        let mut output = ChildOutput::new(
            ledger,
            &read.work.scope,
            phase_limits,
            add(caller_prior, own_source)?,
        )?;
        let waiting_reason = output.closure_waiting_reason(&read.state, &read.projection)?;
        output.slots(read.projection.active_tasks.len())?;
        let active_tasks = read.projection.active_tasks.iter().collect::<Vec<_>>();
        let briefs = read
            .observability_briefs
            .iter()
            .filter_map(|record| match &record.record {
                super::resident_input_evidence::PreparedInputEvidence::Brief(brief) => Some(brief),
                _ => None,
            });
        // One identity view/current-run copy and fixed public DTO metadata.
        output.json(&(
            &identity.agent_id,
            &identity.name,
            &identity.parent_agent_id,
            &identity.lineage_parent_agent_id,
            &identity.delegated_from_task_id,
            &read.state.current_run_id,
        ))?;
        let observability = {
            let mut observe = |bytes, nodes| output.observe(bytes, nodes);
            super::lifecycle::build_child_agent_observability_with_active_tasks_observed(
                &read.state,
                waiting_reason,
                &active_tasks,
                briefs,
                &mut Some(&mut observe),
            )?
        };
        let summary = ChildAgentSummary {
            identity: AgentIdentityView::from_record(identity, &self.inner.default_agent_id),
            status: read.state.status.clone(),
            current_run_id: read.state.current_run_id.clone(),
            pending: read.state.pending,
            active_task_count,
            observability,
        };
        let output_cost = output.cost;
        let guards = output.guards;
        let own = add(own_source, output_cost)?;
        Ok(OwnedChildAgentSummaryRead {
            summary,
            material: ChildAgentSummaryReadMaterial {
                read,
                identity: identity.clone(),
                identity_guard: identity_guard.clone(),
                ledger: ledger.clone(),
                output: guards,
                output_cost,
                own,
            },
        })
    }
    /// The parent owner must be released before awaiting this child's owner.
    #[inline(never)]
    pub(crate) fn capture_owned_child_summary_with_material<'a>(
        &'a self,
        expected: &'a AgentIdentityRecord,
        requested: (usize, usize),
        caller_prior: (u64, usize),
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<OwnedChildAgentSummaryRead>> + Send + 'a>,
    > {
        Box::pin(async move {
            let owner = self.inner.mutation_owner.reserve().await?;
            self.child_summary_from_owner_with_material(&owner, expected, requested, caller_prior)
        })
    }
}
#[cfg(test)]
#[path = "child_summary_read_tests.rs"]
mod tests;

#[cfg(test)]
impl RuntimeHandle {
    pub(crate) fn child_summary_foreign_ledger_fixture(
        agent_id: &str,
        home: std::path::PathBuf,
    ) -> Result<Self> {
        Self::new_isolated_resource_fixture(agent_id, home, super::tests::support::context_config())
    }
    pub(crate) async fn quiesce_child_summary_observer_for_test(&self) -> Result<()> {
        self.quiesce_context_resource_observer_task_for_test().await
    }
}
