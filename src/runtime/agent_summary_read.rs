//! Scheduling observations follow the original summary DTO until serialization.
use super::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use super::resident_scheduling::{
    CapturedScheduling, ResidentSchedulingDomain, SchedulingCaptureRequest, SchedulingReadError,
};
use super::resident_state::AgentSnapshot;
use super::resident_work::{WorkCaptureRequest, WorkReadError};
use super::scheduling_output::{project_selected_scheduling_records, SchedulingReadMaterial};
use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use crate::types::AgentPostureProjection;
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
        let root = self
            .capture_observer_root_async(&self.inner.runtime_db)
            .await?;
        self.summary_scheduler_from_root(&root)
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
            root,
            &self.inner.config_snapshot.load_full(),
            &read.acquisition,
        )?;
        Ok((read, identity, metadata, resources))
    }

    pub(super) fn roster_scheduler_from_root(
        &self,
        root: &super::resident_state::AgentSnapshot,
    ) -> Result<(
        super::state_mutation::PreparedSchedulerRead,
        AgentIdentityView,
        crate::runtime_db::current_query::CurrentSchedulerMetadata,
    )> {
        let query = self
            .roster_query_from_root(root)?
            .context("Summary Current identity needs preparation")?;
        let read = self.capture_operator_scheduler_from_root(root, self.now())?;
        let identity = read.acquisition.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&query.identity.as_ref())?;
            Ok(AgentIdentityView::from_record(
                &query.identity,
                &self.inner.default_agent_id,
            ))
        })?;
        Ok((read, identity, query.scheduler))
    }

    fn roster_query_from_root<'a>(
        &self,
        root: &'a super::resident_state::AgentSnapshot,
    ) -> Result<Option<&'a crate::runtime_db::current_query::CurrentQuery>> {
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "Summary root scope differs"
        );
        let Some(query) = root
            .current
            .as_ref()
            .and_then(|current| current.query.as_ref())
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            query.scope.agent_id == root.state.id
                && query.scope.identity_incarnation == root.token.identity_incarnation,
            "Summary Current scope differs"
        );
        query.validate_binding(&query.scope, &self.inner.input_evidence_budget)?;
        anyhow::ensure!(
            query.identity.agent_id == root.state.id
                && query.identity.incarnation == root.token.identity_incarnation
                && query.identity.status == crate::types::AgentRegistryStatus::Active,
            "Summary identity differs"
        );
        Ok(Some(query))
    }

    fn roster_work_posture(
        root: &AgentSnapshot,
        scheduling: &ResidentSchedulingDomain,
    ) -> Result<AgentPostureProjection> {
        let work = root.work.as_ref().ok_or(WorkReadError::IncompleteLive)?;
        anyhow::ensure!(
            work.scope().agent_id == root.state.id
                && work.scope().identity_incarnation == root.token.identity_incarnation
                && scheduling.scope() == work.scope(),
            "roster Work and Scheduling scope differs"
        );
        let work = work.capture(&WorkCaptureRequest::current())?;
        let scheduling = scheduling.capture(&SchedulingCaptureRequest {
            history_limit: 0,
            ..SchedulingCaptureRequest::current()
        })?;
        let triggers = scheduling
            .active_default_trigger()?
            .map_or(&[][..], std::slice::from_ref);
        let ranked = work.queue_work_ranks(&root.state, triggers)?;
        let waits = work.context_active_wait_conditions()?;
        work.scheduler_work_posture(&root.state, false, &ranked, waits.into_iter())
    }

    async fn roster_posture(
        &self,
        root: &Arc<AgentSnapshot>,
        material: &ResourceReadMaterial,
        db: &RuntimeDb,
    ) -> Result<AgentPostureProjection> {
        if let Some(posture) =
            material.read(|| crate::storage::derive_agent_posture_from_state(&root.state, false))?
        {
            return Ok(posture);
        }
        let queued = self
            .roster_query_from_root(root)?
            .map(|query| query.scheduler.has_queued_input);
        let (queued, scheduling) = match queued {
            Some(queued) if queued || root.scheduling.is_some() => (queued, None),
            _ => self.read_missing_roster_facts(root, material, db).await?,
        };
        material.read(|| {
            if let Some(posture) =
                crate::storage::derive_agent_posture_from_state(&root.state, queued)?
            {
                return Ok(posture);
            }
            let scheduling = scheduling
                .as_ref()
                .or(root.scheduling.as_deref())
                .ok_or(SchedulingReadError::IncompleteLive)?;
            Self::roster_work_posture(root, scheduling)
        })
    }

    async fn read_missing_roster_facts(
        &self,
        root: &Arc<AgentSnapshot>,
        material: &ResourceReadMaterial,
        db: &RuntimeDb,
    ) -> Result<(bool, Option<ResidentSchedulingDomain>)> {
        let observed = root.clone();
        let ledger = self.inner.input_evidence_budget.clone();
        let material = material.clone();
        let result = db
            .with_read_async(move |connection| {
                material.read(|| {
                    let tx = connection.transaction()?;
                    let id = &observed.state.id;
                    let queued = crate::runtime_db::current_query::has_queued_input(&tx, id)?;
                    let (facts, scheduling) = if !queued && observed.scheduling.is_none() {
                        resource_io::admit_cloned_value(id)?;
                        let scope = super::resident_input_evidence::InputEvidenceScope {
                            agent_id: id.clone(),
                            identity_incarnation: observed.token.identity_incarnation,
                        };
                        let limits = ledger.limits();
                        let loaded = crate::runtime_db::scheduling_snapshot::load_scheduling_domain_snapshot_tx(
                            &tx,
                            &scope,
                            &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
                                capture: SchedulingCaptureRequest::current(),
                                record_limit: usize::try_from(limits.record_limit)?,
                                byte_limit: usize::try_from(
                                    limits.history_per_agent_bytes
                                        .checked_add(limits.pending_per_agent_bytes)
                                        .context("roster Scheduling byte limit overflow")?,
                                )?,
                            },
                            &ledger,
                        )?
                        .require_current()?;
                        (loaded.facts, Some(loaded.resident))
                    } else {
                        let facts = crate::runtime_db::context_inputs::ContextDurableRead::new(&tx, id)
                            .scope_facts()?;
                        (facts, None)
                    };
                    anyhow::ensure!(
                        facts.state.as_deref() == Some(observed.state.as_ref())
                            && facts.control_revision.is_some()
                            && facts.identity.as_ref().is_some_and(|identity| {
                                identity.agent_id == *id
                                    && identity.incarnation == observed.token.identity_incarnation
                                    && identity.status == crate::types::AgentRegistryStatus::Active
                            }),
                        "roster durable scope differs from its publication"
                    );
                    tx.commit()?;
                    Ok((queued, scheduling))
                })
            })
            .await;
        if result
            .as_ref()
            .is_err_and(|error| error.is::<crate::runtime_db::RuntimeDbProtectionError>())
        {
            return result;
        }
        // Fallback facts belong to this response and this Ready cut.
        anyhow::ensure!(
            Arc::ptr_eq(root, &self.observer_ready_root(db)?),
            crate::runtime_db::RuntimeDbRetryableError::new(
                Self::OBSERVER_PUBLICATION_OPERATION,
                db.path(),
                "publication changed during read",
            )
        );
        result
    }

    pub(crate) async fn agent_list_entry_read(
        &self,
        expected_identity: &crate::types::AgentIdentityRecord,
        material: &ResourceReadMaterial,
        db: &RuntimeDb,
    ) -> Result<crate::types::AgentListEntry> {
        db.check_read_scope()?;
        self.check_roster_identity(expected_identity)?;
        let root = self.observer_ready_root_bounded(db).await?;
        let identity = root.identity.as_deref().unwrap_or(expected_identity);
        anyhow::ensure!(
            identity.agent_id == root.state.id
                && identity.incarnation == root.token.identity_incarnation
                && identity.status == crate::types::AgentRegistryStatus::Active,
            "roster published identity differs"
        );
        let scheduling_posture = self.roster_posture(&root, material, db).await?;
        db.check_read_scope()?;
        self.agent_list_entry_from_state(&root.state, identity, scheduling_posture, material)
    }

    pub(crate) async fn agent_tree_entry_read(
        &self,
        expected_identity: &crate::types::AgentIdentityRecord,
        material: &ResourceReadMaterial,
    ) -> Result<crate::types::AgentListEntry> {
        self.check_roster_identity(expected_identity)?;
        let root = self
            .capture_observer_root_async(&self.inner.runtime_db)
            .await?;
        let query = self
            .roster_query_from_root(&root)?
            .context("Summary Current identity needs preparation")?;
        let posture = material.read(|| {
            if let Some(posture) = crate::storage::derive_agent_posture_from_state(
                &root.state,
                query.scheduler.has_queued_input,
            )? {
                return Ok(posture);
            }
            Ok(self
                .capture_operator_scheduler_from_root(&root, self.now())?
                .scheduling_posture)
        })?;
        self.agent_list_entry_from_state(&root.state, &query.identity, posture, material)
    }

    fn check_roster_identity(&self, identity: &crate::types::AgentIdentityRecord) -> Result<()> {
        anyhow::ensure!(
            identity.agent_id == self.inner.mutation_owner.agent_id()
                && identity.incarnation == self.inner.identity_incarnation,
            "roster identity differs from loaded owner"
        );
        Ok(())
    }

    fn agent_list_entry_from_state(
        &self,
        agent: &AgentState,
        identity: &crate::types::AgentIdentityRecord,
        scheduling_posture: AgentPostureProjection,
        material: &ResourceReadMaterial,
    ) -> Result<crate::types::AgentListEntry> {
        material.read(|| {
            let model = super::try_agent_model_list_summary_for_catalog(
                &self.inner.config_snapshot.load_full().model_catalog,
                agent,
            )?;
            resource_io::admit_cloned_value(&(
                identity,
                &agent.current_run_id,
                &agent.active_workspace_entry,
            ))?;
            Ok(crate::types::AgentListEntry {
                identity: AgentIdentityView::from_record(identity, &self.inner.default_agent_id),
                lifecycle: crate::types::AgentLifecycleHint::try_from_status(
                    &agent.id,
                    agent.status.clone(),
                )?,
                status: agent.status.clone(),
                scheduling_posture,
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

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "agent_summary_read_tests.rs"]
mod tests;

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "agent_summary_cut_tests.rs"]
mod cut_tests;
