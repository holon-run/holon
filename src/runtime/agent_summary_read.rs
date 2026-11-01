//! Scheduling observations follow the original summary DTO until serialization.
use super::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use super::resident_scheduling::CapturedScheduling;
use super::scheduling_output::{project_selected_scheduling_records, SchedulingReadMaterial};
use super::*;
use std::ops::Deref;

/// The JSON representation is the original `AgentSummary`.
pub struct OwnedAgentSummaryRead {
    summary: AgentSummary,
    material: AgentSummaryReadMaterial,
}
pub(crate) struct AgentSummaryReadMaterial {
    _scheduling: SchedulingReadMaterial,
    _owned: Vec<Arc<BudgetViewGuard>>,
    acquisition: Option<crate::diagnostics::resource_io::ResourceReadMaterial>,
    _resources: Option<super::resource_observation::InstructionResourceRead>,
}
struct SummaryWire {
    bytes: Vec<u8>,
    _material: AgentSummaryReadMaterial,
}
impl AsRef<[u8]> for SummaryWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl OwnedAgentSummaryRead {
    pub(crate) fn new(
        summary: AgentSummary,
        scheduling: SchedulingReadMaterial,
        owned: Vec<Arc<BudgetViewGuard>>,
    ) -> Self {
        Self {
            summary,
            material: AgentSummaryReadMaterial {
                _scheduling: scheduling,
                _owned: owned,
                acquisition: None,
                _resources: None,
            },
        }
    }

    pub(crate) fn with_acquisition(
        mut self,
        acquisition: crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<Self> {
        acquisition.read(|| {
            for child in &self.summary.active_children {
                for guard in [
                    child.observability._read_material.as_ref(),
                    child.observability._source_material.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    crate::runtime_db::repositories::reserve_source_vec_slot(
                        &mut self.material._owned,
                    )?;
                    self.material._owned.push(guard.clone());
                }
            }
            Ok(())
        })?;
        self.material.acquisition = Some(acquisition);
        Ok(self)
    }

    pub(super) fn with_resources(
        mut self,
        resources: super::resource_observation::InstructionResourceRead,
    ) -> Self {
        self.material._resources = Some(resources);
        self
    }

    pub(crate) fn with_fleet_inventory(
        mut self,
        inventory: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<Self> {
        anyhow::ensure!(
            inventory.matches_identity(
                &self.summary.identity.agent_id,
                self.summary.identity.incarnation,
            ),
            "fleet Summary inventory scope differs"
        );
        let guard = inventory.guard()?;
        inventory.read(|| {
            crate::runtime_db::repositories::reserve_source_vec_slot(&mut self.material._owned)?;
            self.material._owned.push(guard);
            Ok(())
        })?;
        Ok(self)
    }

    pub(crate) fn into_json_bytes(self) -> Result<tokio_util::bytes::Bytes> {
        let acquisition = self
            .material
            .acquisition
            .as_ref()
            .context("Summary wire requires its original acquisition")?;
        let bytes = acquisition.read(|| {
            use crate::diagnostics::resource_io;
            let size = usize::try_from(super::input_evidence_budget::serialized_bytes(
                &self.summary,
            )?)?;
            resource_io::charge_owned_payload(u64::try_from(size)?)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<SummaryWire>() + 128)?,
                1,
            )?;
            let mut bytes = vec![0; size];
            let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
            serde_json::to_writer(&mut writer, &self.summary)?;
            anyhow::ensure!(
                usize::try_from(writer.position())? == size,
                "Summary JSON size changed"
            );
            Ok(bytes)
        })?;
        Ok(tokio_util::bytes::Bytes::from_owner(SummaryWire {
            bytes,
            _material: self.material,
        }))
    }

    // The caller keeps `material` through the synchronous DTO serialization.
    pub(crate) fn into_parts(self) -> (AgentSummary, AgentSummaryReadMaterial) {
        (self.summary, self.material)
    }
}
impl Deref for OwnedAgentSummaryRead {
    type Target = AgentSummary;
    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}
impl std::fmt::Debug for OwnedAgentSummaryRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.summary, formatter)
    }
}
impl serde::Serialize for OwnedAgentSummaryRead {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.summary, serializer)
    }
}

pub(crate) fn active_trigger_summaries<'a>(
    records: impl IntoIterator<Item = &'a ExternalTriggerRecord>,
) -> Result<Vec<ExternalTriggerSummary>> {
    let mut summaries = Vec::new();
    for record in records
        .into_iter()
        .filter(|record| record.status == ExternalTriggerStatus::Active)
    {
        crate::runtime_db::repositories::reserve_source_vec_slot(&mut summaries)?;
        let record = super::scheduling_output::external_trigger_state_snapshot(record)?;
        summaries.push(ExternalTriggerSummary {
            external_trigger_id: record.external_trigger_id,
            target_agent_id: record.target_agent_id,
            scope: record.scope,
            delivery_mode: record.delivery_mode,
            status: record.status,
            delivery_count: record.delivery_count,
            created_at: record.created_at,
            revoked_at: record.revoked_at,
            last_delivered_at: record.last_delivered_at,
        });
    }
    Ok(summaries)
}
impl RuntimeHandle {
    pub(in crate::runtime) async fn capture_summary_scheduler(
        &self,
    ) -> Result<(
        super::state_mutation::PreparedSchedulerRead,
        AgentIdentityView,
        crate::runtime_db::current_query::CurrentSchedulerMetadata,
        super::resource_observation::InstructionResourceRead,
    )> {
        let mut reservations = super::mutation_owner::MutationOwner::reserve_all(vec![self
            .inner
            .mutation_owner
            .clone()])
        .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Summary root needs publication")?;
        if root
            .current
            .as_ref()
            .and_then(|current| current.query.as_ref())
            .is_none()
        {
            drop(root);
            drop(reservations);
            let mut owner = self.begin_mutation().await?;
            owner.prepare_current_query(None).await?;
            self.summary_scheduler_from_root(owner.capture_published_root()?.as_ref())
        } else {
            self.summary_scheduler_from_root(&root)
        }
    }

    fn summary_scheduler_from_root(
        &self,
        root: &super::resident_state::AgentSnapshot,
    ) -> Result<(
        super::state_mutation::PreparedSchedulerRead,
        AgentIdentityView,
        crate::runtime_db::current_query::CurrentSchedulerMetadata,
        super::resource_observation::InstructionResourceRead,
    )> {
        let (read, identity, metadata) = self.roster_scheduler_from_root(root)?;
        let resources = self.capture_instruction_resources(
            &read.state,
            &self.inner.config_snapshot.load_full(),
            &read.acquisition,
        )?;
        Ok((read, identity, metadata, resources))
    }

    fn roster_scheduler_from_root(
        &self,
        root: &super::resident_state::AgentSnapshot,
    ) -> Result<(
        super::state_mutation::PreparedSchedulerRead,
        AgentIdentityView,
        crate::runtime_db::current_query::CurrentSchedulerMetadata,
    )> {
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "Summary root scope differs"
        );
        let query = root
            .current
            .as_ref()
            .and_then(|current| current.query.as_ref())
            .context("Summary Current identity needs preparation")?;
        anyhow::ensure!(
            query.scope.agent_id == root.state.id
                && query.scope.identity_incarnation == root.token.identity_incarnation,
            "Summary Current scope differs"
        );
        query.validate_binding(&query.scope, &self.inner.input_evidence_budget)?;
        let read = self.capture_operator_scheduler_from_root(root, self.now())?;
        let identity = read.acquisition.read(|| {
            anyhow::ensure!(
                query.identity.agent_id == root.state.id
                    && query.identity.incarnation == root.token.identity_incarnation
                    && query.identity.status == crate::types::AgentRegistryStatus::Active,
                "Summary identity differs"
            );
            crate::diagnostics::resource_io::admit_cloned_value(&query.identity.as_ref())?;
            Ok(AgentIdentityView::from_record(
                &query.identity,
                &self.inner.default_agent_id,
            ))
        })?;
        Ok((read, identity, query.scheduler))
    }

    pub(crate) async fn agent_list_entry_read(
        &self,
        expected_identity: &crate::types::AgentIdentityRecord,
        material: &crate::diagnostics::resource_io::ResourceReadMaterial,
    ) -> Result<crate::types::AgentListEntry> {
        anyhow::ensure!(
            expected_identity.agent_id == self.inner.mutation_owner.agent_id()
                && expected_identity.incarnation == self.inner.identity_incarnation,
            "roster identity differs from loaded owner"
        );
        let mut reservations = super::mutation_owner::MutationOwner::reserve_all(vec![self
            .inner
            .mutation_owner
            .clone()])
        .await?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("roster root needs publication")?;
        let (read, identity, _) = if root
            .current
            .as_ref()
            .and_then(|current| current.query.as_ref())
            .is_none()
        {
            drop(root);
            drop(reservations);
            let mut owner = self.begin_mutation().await?;
            owner.prepare_current_query(None).await?;
            material.read(|| {
                self.roster_scheduler_from_root(owner.capture_published_root()?.as_ref())
            })?
        } else {
            material.read(|| self.roster_scheduler_from_root(&root))?
        };
        material.read(|| {
            use crate::diagnostics::resource_io;
            let agent = &read.state;
            let model = super::try_agent_model_list_summary_for_catalog(
                &self.inner.config_snapshot.load_full().model_catalog,
                agent,
            )?;
            resource_io::admit_cloned_value(&(
                &agent.current_run_id,
                &agent.active_workspace_entry,
            ))?;
            Ok(crate::types::AgentListEntry {
                identity,
                lifecycle: crate::types::AgentLifecycleHint::try_from_status(
                    &agent.id,
                    agent.status.clone(),
                )?,
                status: agent.status.clone(),
                scheduling_posture: read.scheduling_posture,
                pending: agent.pending,
                current_run_id: agent.current_run_id.clone(),
                waiting_reason: super::lightweight_agent_list_waiting_reason(agent),
                model,
                active_workspace_entry: agent
                    .active_workspace_entry
                    .clone()
                    .map(crate::types::ActiveWorkspaceEntry::without_projection_metadata),
            })
        })
    }

    pub(super) fn summary_triggers_from_capture(
        &self,
        selected: CapturedScheduling,
    ) -> Result<(Vec<ExternalTriggerSummary>, SchedulingReadMaterial)> {
        let ledger: &InputEvidenceBudgetLedger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let (timers, triggers, material) = project_selected_scheduling_records(
            selected,
            0,
            ledger,
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("summary Scheduling byte limit overflow")?,
            )?,
            usize::try_from(limits.record_limit)?,
            (0, 0),
            Vec::new(),
        )?;
        debug_assert!(timers.is_empty());
        // Only the final public scalar subset is copied; the body stays shared.
        Ok((
            active_trigger_summaries(triggers.iter().map(|row| &**row))?,
            material,
        ))
    }
}

#[cfg(test)]
#[path = "agent_summary_read_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent_summary_cut_tests.rs"]
mod cut_tests;
