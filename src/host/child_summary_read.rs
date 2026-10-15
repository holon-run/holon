//! One Host phase for its original cache-selected, already loaded children.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    ChildAgentSummaryReadMaterial,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChildAgentSummariesReadError {
    #[error("ResourceExhausted: Host child summaries material")]
    ResourceExhausted,
    #[error("Host child summaries material belongs to another ledger")]
    ForeignMaterial,
}
pub(crate) struct OwnedChildAgentSummariesRead {
    summaries: Vec<ChildAgentSummary>,
    material: ChildAgentSummariesReadMaterial,
}
pub(crate) struct ChildAgentSummariesReadMaterial {
    ledger: InputEvidenceBudgetLedger,
    selector: Arc<BudgetViewGuard>,
    selector_cost: (u64, usize),
    children: Vec<ChildAgentSummaryReadMaterial>,
    own: (u64, usize),
}
impl OwnedChildAgentSummariesRead {
    pub(crate) fn into_parts(self) -> (Vec<ChildAgentSummary>, ChildAgentSummariesReadMaterial) {
        (self.summaries, self.material)
    }
}
impl ChildAgentSummariesReadMaterial {
    #[cfg(test)]
    pub(crate) fn selector_guard_for_test(&self) -> &Arc<BudgetViewGuard> {
        &self.selector
    }
    pub(crate) fn validate_accounting(&self, ledger: &InputEvidenceBudgetLedger) -> Result<()> {
        anyhow::ensure!(
            self.ledger.same_ledger(ledger),
            ChildAgentSummariesReadError::ForeignMaterial
        );
        self.selector
            .validate_host_owned_material_binding(ledger, self.selector_cost.0)
            .context(ChildAgentSummariesReadError::ForeignMaterial)?;
        let mut total = self.selector_cost;
        for child in &self.children {
            child.validate_accounting(ledger)?;
            total = add(total, child.material_estimate()?)?;
        }
        anyhow::ensure!(
            total == self.own,
            ChildAgentSummariesReadError::ForeignMaterial
        );
        Ok(())
    }
    /// This actual aggregate excludes its parent's caller prior.
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        self.validate_accounting(&self.ledger)?;
        Ok(self.own)
    }
}
fn add(a: (u64, usize), b: (u64, usize)) -> Result<(u64, usize)> {
    Ok((
        a.0.checked_add(b.0)
            .context(ChildAgentSummariesReadError::ResourceExhausted)?,
        a.1.checked_add(b.1)
            .context(ChildAgentSummariesReadError::ResourceExhausted)?,
    ))
}
fn limits(ledger: &InputEvidenceBudgetLedger, requested: (usize, usize)) -> Result<(u64, usize)> {
    let actual = ledger.limits();
    Ok((
        actual
            .history_per_agent_bytes
            .checked_add(actual.pending_per_agent_bytes)
            .context(ChildAgentSummariesReadError::ResourceExhausted)?
            .min(64 * 1024 * 1024)
            .min(u64::try_from(requested.0)?),
        usize::try_from(actual.record_limit.min(65536))?.min(requested.1),
    ))
}
fn admit(limits: (u64, usize), total: (u64, usize)) -> Result<()> {
    anyhow::ensure!(
        total.0 <= limits.0 && total.1 <= limits.1,
        ChildAgentSummariesReadError::ResourceExhausted
    );
    Ok(())
}
struct SelectedChildren {
    identities: Vec<AgentIdentityRecord>,
    guard: Arc<BudgetViewGuard>,
    cost: (u64, usize),
}
impl RuntimeHost {
    fn select_child_summary_addresses(
        &self,
        parent: &str,
        ledger: &InputEvidenceBudgetLedger,
        requested: (usize, usize),
        prior: (u64, usize),
    ) -> Result<SelectedChildren> {
        anyhow::ensure!(
            self.inner.input_evidence_budget.same_ledger(ledger),
            ChildAgentSummariesReadError::ForeignMaterial
        );
        let phase_limits = limits(ledger, requested)?;
        self.inner.registry.with_agent_identity_records(|records| {
            let selected = |record: &&AgentIdentityRecord| {
                record.status == AgentRegistryStatus::Active
                    && record.kind == AgentKind::Child
                    && record.parent_agent_id.as_deref() == Some(parent)
            };
            let count = records.values().filter(selected).count();
            let slots = count
                .checked_mul(
                    std::mem::size_of::<AgentIdentityRecord>()
                        + std::mem::size_of::<RuntimeHandle>()
                        + std::mem::size_of::<ChildAgentSummary>()
                        + std::mem::size_of::<ChildAgentSummaryReadMaterial>()
                        + std::mem::size_of::<AgentIdentityRecord>()
                        + std::mem::size_of::<ChildAgentSummary>(),
                )
                .context(ChildAgentSummariesReadError::ResourceExhausted)?;
            // Includes both stable-sort scratch vectors, all returned slots,
            // temporary Runtime handles and the private aggregate headers.
            let nodes = count
                .checked_mul(6)
                .and_then(|value| value.checked_add(32))
                .context(ChildAgentSummariesReadError::ResourceExhausted)?;
            let mut cost = (
                u64::try_from(slots)?
                    .checked_add(8192)
                    .and_then(|bytes| {
                        bytes.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?)
                    })
                    .context(ChildAgentSummariesReadError::ResourceExhausted)?,
                nodes,
            );
            for identity in records.values().filter(selected) {
                let (bytes, nodes) = borrowed_json_material(identity)?;
                cost = add(
                    cost,
                    (
                        bytes
                            .checked_add(
                                u64::try_from(nodes)?
                                    .checked_mul(256)
                                    .context(ChildAgentSummariesReadError::ResourceExhausted)?,
                            )
                            .context(ChildAgentSummariesReadError::ResourceExhausted)?,
                        nodes,
                    ),
                )?;
            }
            // Cache read-lock -> short ledger registration, no await/IO. Ledger
            // methods never call cache code while retaining their private lock.
            admit(phase_limits, add(prior, cost)?)?;
            let guard = ledger.register_host_owned_bytes(
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: cost.0,
                },
            )?;
            let mut identities = Vec::with_capacity(count);
            identities.extend(records.values().filter(selected).cloned());
            identities.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
            Ok(SelectedChildren {
                identities,
                guard,
                cost,
            })
        })
    }
    /// This path is pure for every loaded child. A genuine cold member leaves
    /// the original explicit cold adapter to its caller; it is never fake empty.
    pub(crate) async fn try_loaded_owned_child_summaries(
        &self,
        parent: &str,
        ledger: &InputEvidenceBudgetLedger,
        requested: (usize, usize),
        caller_prior: (u64, usize),
    ) -> Result<Option<OwnedChildAgentSummariesRead>> {
        let selected =
            self.select_child_summary_addresses(parent, ledger, requested, caller_prior)?;
        // Resolve the complete originally selected address cohort first. No
        // child source is captured then discarded merely because a later child
        // is genuinely cold; no loaded capture error falls back to storage.
        let mut runtimes = Vec::with_capacity(selected.identities.len());
        for identity in &selected.identities {
            let Some(runtime) = self.try_get_loaded_runtime(&identity.agent_id).await else {
                return Ok(None);
            };
            anyhow::ensure!(
                runtime.child_summary_uses_ledger(ledger),
                ChildAgentSummariesReadError::ForeignMaterial
            );
            runtimes.push(runtime);
        }
        let phase_limits = limits(ledger, requested)?;
        let mut summaries = Vec::with_capacity(selected.identities.len());
        let mut children = Vec::with_capacity(selected.identities.len());
        let mut own = selected.cost;
        for (identity, runtime) in selected.identities.into_iter().zip(runtimes) {
            let read = runtime
                .capture_owned_child_summary_with_material(
                    &identity,
                    requested,
                    add(caller_prior, own)?,
                )
                .await?;
            let (summary, material) = read.into_parts();
            material.validate_accounting(ledger)?;
            own = add(own, material.material_estimate()?)?;
            admit(phase_limits, add(caller_prior, own)?)?;
            summaries.push(summary);
            children.push(material);
        }
        summaries.sort_by(|left, right| left.identity.agent_id.cmp(&right.identity.agent_id));
        Ok(Some(OwnedChildAgentSummariesRead {
            summaries,
            material: ChildAgentSummariesReadMaterial {
                ledger: ledger.clone(),
                selector: selected.guard,
                selector_cost: selected.cost,
                children,
                own,
            },
        }))
    }
}
#[cfg(test)]
#[path = "child_summary_read_tests.rs"]
mod tests;
