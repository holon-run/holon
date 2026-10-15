//! The loaded compact projection borrows one publication before its DTO copies.
use super::input_evidence_budget::{
    borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use super::selected_model_read::{capture_selected_model_state, SelectedModelReadMaterial};
use super::state_mutation::PreparedSchedulerRead;
use super::*;
use crate::diagnostics::attribution;
use anyhow::ensure;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CompactAgentReadError {
    #[error("ResourceExhausted: compact agent projection material")]
    ResourceExhausted,
    #[error("compact agent projection differs from its held publication")]
    ForeignMaterial,
}
/// Only selected material survives the parent owner. There is no Root, Runtime,
/// config or database backedge. The moved queue/posture are in the public DTO.
pub(crate) struct CompactAgentReadMaterial {
    read: PreparedSchedulerRead,
    identity: Arc<crate::types::AgentIdentityRecord>,
    identity_guard: Arc<BudgetViewGuard>,
    ledger: InputEvidenceBudgetLedger,
    output: Arc<BudgetViewGuard>,
    output_cost: (u64, usize),
    model: SelectedModelReadMaterial,
    total: (u64, usize),
    children: Option<Box<crate::host::ChildAgentSummariesReadMaterial>>,
}
impl CompactAgentReadMaterial {
    #[cfg(test)]
    pub(crate) fn children_material_for_test(
        &self,
    ) -> Option<&crate::host::ChildAgentSummariesReadMaterial> {
        self.children.as_deref()
    }
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        self.validate_accounting()?;
        match self.children.as_ref() {
            Some(children) => add(self.total, children.material_estimate()?),
            None => Ok(self.total),
        }
    }
    pub(crate) fn validate_accounting(&self) -> Result<()> {
        let scope = &self.read.work.scope;
        ensure!(
            self.identity.agent_id == scope.agent_id
                && self.identity.incarnation == scope.identity_incarnation
                && self.read.state.id == scope.agent_id,
            CompactAgentReadError::ForeignMaterial
        );
        self.identity_guard
            .validate_owned_material_binding(scope, &self.ledger, 0)
            .context(CompactAgentReadError::ForeignMaterial)?;
        self.read
            ._owned_budget
            .validate_owned_material_binding(scope, &self.ledger, 0)
            .context(CompactAgentReadError::ForeignMaterial)?;
        self.output
            .validate_owned_material_binding(scope, &self.ledger, self.output_cost.0)
            .context(CompactAgentReadError::ForeignMaterial)?;
        self.model.validate_accounting(scope, &self.ledger)?;
        if let Some(children) = self.children.as_ref() {
            children.validate_accounting(&self.ledger)?;
        }
        Ok(())
    }
}
fn add(a: (u64, usize), b: (u64, usize)) -> Result<(u64, usize)> {
    Ok((
        a.0.checked_add(b.0)
            .context(CompactAgentReadError::ResourceExhausted)?,
        a.1.checked_add(b.1)
            .context(CompactAgentReadError::ResourceExhausted)?,
    ))
}
fn copies(value: &impl serde::Serialize, count: usize) -> Result<(u64, usize)> {
    let (bytes, nodes) = borrowed_json_material(value)?;
    let nodes = nodes
        .checked_add(1)
        .and_then(|n| n.checked_mul(count))
        .context(CompactAgentReadError::ResourceExhausted)?;
    let bytes = bytes
        .checked_mul(u64::try_from(count)?)
        .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
        .context(CompactAgentReadError::ResourceExhausted)?;
    Ok((bytes, nodes))
}
fn material_limits(
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
) -> Result<(u64, usize)> {
    let actual = ledger.limits();
    Ok((
        actual
            .history_per_agent_bytes
            .checked_add(actual.pending_per_agent_bytes)
            .context(CompactAgentReadError::ResourceExhausted)?
            .min(64 * 1024 * 1024)
            .min(u64::try_from(limits.0)?),
        usize::try_from(actual.record_limit.min(65_536))?.min(limits.1),
    ))
}
fn admit(limits: (u64, usize), total: (u64, usize)) -> Result<()> {
    ensure!(
        total.0 <= limits.0 && total.1 <= limits.1,
        CompactAgentReadError::ResourceExhausted
    );
    Ok(())
}
impl RuntimeHandle {
    /// One held parent owner supplies the Scheduler read and identity header.
    /// No resources are required by the original compact schema.
    fn capture_compact_primary_from_owner(
        &self,
        owner: &mutation_owner::MutationReservation,
        limits: (usize, usize),
    ) -> Result<LightweightAgentStateProjection> {
        owner.validate_owner(&self.inner.mutation_owner)?;
        let root = owner
            .capture_ready_snapshot()?
            .ok_or(super::current_read_facts::CurrentReadError::RetiredScope)?;
        let identity = root.active_identity()?;
        let scope = root
            .work
            .as_ref()
            .ok_or(super::resident_work::WorkReadError::IncompleteLive)?
            .scope();
        let identity_guard = root
            ._identity_budget
            .as_ref()
            .context("compact identity has no original material")?;
        let ledger = &self.inner.input_evidence_budget;
        identity_guard
            .validate_owned_material_binding(scope, ledger, 0)
            .context(CompactAgentReadError::ForeignMaterial)?;
        let phase_limits = material_limits(ledger, limits)?;
        // Borrow the actual publication's identity allocation before Scheduler
        // owns a selector or selected body. This does not register it again.
        let identity_material = (identity_guard.owned_material_bytes()?, 24 * 8);
        admit(phase_limits, identity_material)?;
        let mut read = self.capture_scheduler_read_from_snapshot_with_material(
            &root,
            &super::resident_work::WorkCaptureRequest::current(),
            self.now(),
            limits,
            identity_material,
            None,
        )?;
        ensure!(
            Arc::ptr_eq(&root.state, &read.state),
            CompactAgentReadError::ForeignMaterial
        );
        // The selected read excludes caller prior; count this original identity
        // exactly once in the retained compact carrier and its output phase.
        let source = add(read.material_estimate()?, identity_material)?;
        let mut output = (
            u64::try_from(std::mem::size_of::<CompactAgentReadMaterial>())? + 8192,
            32,
        );
        // One complete State DTO and the existing Slim subset; the original
        // model path's unnecessary second complete State clone is avoided.
        output = add(output, copies(read.state.as_ref(), 2)?)?;
        output = add(output, copies(&read.scheduling_posture, 1)?)?;
        output = add(
            output,
            copies(
                &(
                    identity.agent_id.as_str(),
                    &identity.name,
                    &identity.parent_agent_id,
                    &identity.lineage_parent_agent_id,
                    &identity.delegated_from_task_id,
                ),
                2,
            )?,
        )?;
        let prior = add(source, output)?;
        admit(phase_limits, prior)?;
        let output_guard = ledger.register_owned_bytes(
            &read.work.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: output.0,
            },
        )?;
        let config = self.inner.config_snapshot.load();
        let (model, model_material) = capture_selected_model_state(
            &config.model_catalog,
            &config.base_context_config,
            &read.state,
            &read.work.scope,
            ledger,
            &read._owned_budget,
            limits,
            prior,
        )?;
        let total = add(prior, model_material.material_estimate())?;
        admit(phase_limits, total)?;
        // All selected strings above have their actual guards before cloning.
        let identity_timer = attribution::IDENTITY.start();
        let identity_view = AgentIdentityView::from_record(identity, &self.inner.default_agent_id);
        drop(identity_timer);
        let agent = read.state.as_ref().clone();
        let closure_timer = attribution::CLOSURE.start();
        let closure = Self::closure_decision_from_projection(&agent, &read.projection, None);
        drop(closure_timer);
        let tasks_timer = attribution::ACTIVE_TASKS.start();
        let active_task_count = read.projection.active_tasks.len();
        drop(tasks_timer);
        // Transfer these existing allocations; retaining the source carrier
        // does not require a second complete WorkQueue or Posture clone.
        let work_queue = std::mem::take(&mut read.work_queue);
        let posture_timer = attribution::POSTURE.start();
        let scheduling_posture = std::mem::take(&mut read.scheduling_posture);
        drop(posture_timer);
        let work_budget = read._owned_budget.clone();
        let material = Box::new(CompactAgentReadMaterial {
            read,
            identity: identity.clone(),
            identity_guard: identity_guard.clone(),
            ledger: ledger.clone(),
            output: output_guard,
            output_cost: output,
            model: model_material,
            total,
            children: None,
        });
        Ok(LightweightAgentStateProjection {
            identity: identity_view,
            active_task_count,
            lifecycle: crate::types::AgentLifecycleHint::from_status(
                &agent.id,
                agent.status.clone(),
            ),
            model,
            agent,
            scheduling_posture,
            closure,
            active_children: Vec::new(),
            work_queue,
            _work_budget: Some(work_budget),
            _scheduling_material: None,
            _ordinary_material: Some(material),
        })
    }
    #[inline(never)]
    pub(super) fn capture_compact_agent_projection(
        &self,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<LightweightAgentStateProjection>> + Send + '_>,
    > {
        Box::pin(async move {
            let owner = self.inner.mutation_owner.reserve().await?;
            let mut projection =
                self.capture_compact_primary_from_owner(&owner, (usize::MAX, usize::MAX))?;
            // Child observation takes another owner's facts. It must not wait
            // while retaining the parent mutation reservation.
            drop(owner);
            let children_timer = attribution::CHILDREN.start();
            projection.active_children = if let Some(bridge) = self.inner.host_bridge.as_ref() {
                let material = projection
                    ._ordinary_material
                    .as_mut()
                    .context("loaded parent material absent")?;
                let prior = material.material_estimate()?;
                if let Some(children) = bridge
                    .try_loaded_owned_child_summaries(
                        &projection.agent.id,
                        &self.inner.input_evidence_budget,
                        (usize::MAX, usize::MAX),
                        prior,
                    )
                    .await?
                {
                    let (summaries, children) = children.into_parts();
                    material.children = Some(Box::new(children));
                    summaries
                } else {
                    // Genuine cold inspection remains its original named path
                    // until its bounded actor joins this material consumer.
                    bridge.child_summaries(&projection.agent.id).await?
                }
            } else {
                Vec::new()
            };
            drop(children_timer);
            Ok(projection)
        })
    }
}
#[cfg(test)]
#[path = "compact_agent_read_tests.rs"]
mod tests;

#[path = "compact_agent_json.rs"]
mod json;
