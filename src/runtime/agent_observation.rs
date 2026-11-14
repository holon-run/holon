//! Runtime adapters for observation material backed by the budget ledger.

use std::sync::Arc;

use holon_domain::{ObservationGuard, ObservationMaterial};

use super::input_evidence_budget::BudgetViewGuard;

impl ObservationGuard for BudgetViewGuard {
    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn fork_owned(&self) -> Arc<dyn ObservationGuard> {
        BudgetViewGuard::fork_owned(self)
    }
}

pub(crate) fn active_observation_material() -> anyhow::Result<Option<ObservationMaterial>> {
    Ok(
        crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?
            .map(ObservationMaterial::new),
    )
}

pub(crate) fn retain_observation_guard(material: &ObservationMaterial) -> Arc<BudgetViewGuard> {
    Arc::downcast(material.guard().clone().into_any())
        .expect("runtime observations carry a budget view guard")
}

#[cfg(test)]
pub(crate) fn same_observation_guard(
    material: &ObservationMaterial,
    guard: &Arc<BudgetViewGuard>,
) -> bool {
    let guard: Arc<dyn ObservationGuard> = guard.clone();
    Arc::ptr_eq(material.guard(), &guard)
}

pub(crate) fn read_lifecycle_hint(
    agent_id: &str,
    status: holon_domain::AgentStatus,
) -> std::io::Result<holon_domain::AgentLifecycleHint> {
    if status == holon_domain::AgentStatus::Stopped {
        crate::diagnostics::resource_io::charge_owned_metadata(
            (holon_domain::AgentLifecycleHint::STOPPED_OPERATOR_HINT.len()
                + std::mem::size_of::<String>()) as u64,
            1,
        )?;
    }
    Ok(holon_domain::AgentLifecycleHint::from_status(
        agent_id, status,
    ))
}
