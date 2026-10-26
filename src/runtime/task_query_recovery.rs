//! Exact current-instance live roles for explicit original receipt recovery.
use super::{super::mutation_owner::MutationReservation, TaskRecoveryQueries};
use anyhow::{Context, Result};

impl super::super::RuntimeHandle {
    pub(crate) async fn task_recovery_queries_for_group(
        &self,
        reservations: &[MutationReservation],
    ) -> Result<Vec<TaskRecoveryQueries>> {
        let limits = self.inner.input_evidence_budget.limits();
        let records = usize::try_from(limits.record_limit)?;
        let bytes = usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("query recovery limits overflow")?,
        )?;
        let mut roles = Vec::new();
        for reservation in reservations {
            if reservation
                .recovery()?
                .is_none_or(|recovery| recovery.known.is_some())
            {
                continue;
            }
            let runtime = if reservation
                .validate_owner(&self.inner.mutation_owner)
                .is_ok()
            {
                Some(self.clone())
            } else {
                match &self.inner.host_bridge {
                    Some(bridge) => bridge.runtime_for_mutation(reservation).await?,
                    None => None,
                }
            };
            if let Some(runtime) = runtime {
                if let Some(role) = runtime.task_recovery_queries(reservation, records, bytes)? {
                    roles.push(role);
                }
            }
        }
        Ok(roles)
    }
}
impl crate::host::RuntimeHost {
    pub(crate) async fn task_recovery_queries_for_group(
        &self,
        reservations: &[MutationReservation],
    ) -> Result<Vec<TaskRecoveryQueries>> {
        let limits = self.bridge().input_evidence_budget()?.limits();
        let records = usize::try_from(limits.record_limit)?;
        let bytes = usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("query recovery limits overflow")?,
        )?;
        let mut roles = Vec::new();
        for reservation in reservations {
            if reservation
                .recovery()?
                .is_none_or(|recovery| recovery.known.is_some())
            {
                continue;
            }
            if let Some(runtime) = self.bridge().runtime_for_mutation(reservation).await? {
                if let Some(role) = runtime.task_recovery_queries(reservation, records, bytes)? {
                    roles.push(role);
                }
            }
        }
        Ok(roles)
    }
}
