//! Same-owner finite cold Summary facts; no Runtime or publication is created.
use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use crate::runtime::{ColdSchedulerFacts, SchedulerAgentSnapshot, SchedulerObservations};

pub(super) struct HeldColdSummary {
    pub identity: AgentIdentityRecord,
    pub state: AgentState,
    pub read: Option<ColdSchedulerFacts>,
    pub empty_projection: Option<crate::runtime::SchedulerProjection>,
    pub scheduling: crate::runtime::SchedulingReadMaterial,
    pub acquisition: ResourceReadMaterial,
    pub metadata: crate::runtime_db::current_query::CurrentSchedulerMetadata,
    pub notifications: Vec<crate::types::OperatorNotificationRecord>,
    pub triggers: Vec<crate::types::ExternalTriggerSummary>,
}

impl HeldColdSummary {
    pub fn observability(&self) -> Result<crate::types::ChildAgentObservabilitySnapshot> {
        let projection = self.projection();
        let closure = RuntimeHandle::closure_decision_from_projection(
            &self.state,
            projection,
            None,
            &self.acquisition,
        )?;
        self.acquisition.read(|| {
            resource_io::charge_owned_metadata(
                u64::try_from(
                    projection
                        .active_tasks
                        .len()
                        .checked_mul(std::mem::size_of::<&TaskRecord>())
                        .context("cold child Task reference slots overflow")?,
                )?,
                projection.active_tasks.len(),
            )?;
            let tasks = projection.active_tasks.iter().collect::<Vec<_>>();
            let briefs = self
                .read
                .iter()
                .flat_map(|read| read.briefs.iter())
                .filter_map(|record| match &record.record {
                    crate::runtime::resident_input_evidence::PreparedInputEvidence::Brief(
                        brief,
                    ) => Some(brief),
                    _ => None,
                });
            crate::runtime::build_child_agent_observability_with_active_tasks(
                &self.state,
                closure.waiting_reason,
                &tasks,
                briefs,
            )
        })
    }

    pub fn projection(&self) -> &crate::runtime::SchedulerProjection {
        self.read
            .as_ref()
            .map(|read| &read.projection)
            .or(self.empty_projection.as_ref())
            .expect("cold Summary projection")
    }
}

impl RuntimeHost {
    #[cfg(test)]
    pub(super) async fn child_agent_observability_snapshot(
        &self,
        agent_id: &str,
        storage: &AppStorage,
        state: &AgentState,
        material: &ResourceReadMaterial,
    ) -> Result<crate::types::ChildAgentObservabilitySnapshot> {
        if let Some(runtime) = self.try_get_loaded_runtime(agent_id).await {
            let mut snapshot = runtime.child_agent_observability().await?;
            snapshot._source_material = Some(material.guard()?);
            return Ok(snapshot);
        }
        RuntimeHandle::child_agent_observability_from_storage(storage, state, material)
    }

    pub(super) async fn acquire_cold_summary(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Result<HeldColdSummary> {
        self.acquire_cold_summary_in(identity, self.runtime_db())
            .await
    }

    pub(super) async fn acquire_cold_summary_in(
        &self,
        identity: &AgentIdentityRecord,
        db: &RuntimeDb,
    ) -> Result<HeldColdSummary> {
        let reservations = self
            .reserve_mutation_scopes_in(&[(identity.agent_id.clone(), identity.incarnation)], db)
            .await?;
        let ledger = &self.inner.input_evidence_budget;
        let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let acquisition = ResourceReadMaterial::current_or_new(ledger, &scope)?;
        let result = acquisition.read(|| {
            let selected = db.transaction(|tx| {
                crate::runtime_db::cold_summary::prepare_tx(tx, &scope, ledger)
            })?;
            let limits = ledger.limits();
            let bytes = usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("cold Summary limit overflow")?,
            )?;
            let records = usize::try_from(limits.record_limit)?;
            let (state, read, empty_projection, scheduling, triggers) = if let Some(domains) =
                selected.domains
            {
                resource_io::admit_cloned_value(&domains.state.as_ref())?;
                let state = domains.state.as_ref().clone();
                let read = crate::runtime::derive_scheduler_read(
                    domains,
                    &crate::runtime::resident_work::WorkCaptureRequest::current(),
                    state.pending,
                    Utc::now(),
                    (
                        selected.metadata.has_interrupted_replay,
                        selected.metadata.has_queued_input,
                        selected.metadata.latest_runtime_error,
                    ),
                    ledger,
                    &acquisition,
                )?;
                // Capture the same selected Scheduling domain for the public
                // active Trigger projection; no second SQL body read.
                let (read, scheduling) = read.into_cold_facts()?;
                let (_, triggers, material) = crate::runtime::project_cold_scheduling_records(
                    scheduling,
                    0,
                    ledger,
                    bytes,
                    records,
                    selected.scheduling_usage,
                    selected.scheduling_guards,
                )?;
                let triggers =
                    crate::runtime::active_trigger_summaries(triggers.iter().map(|row| &**row))?;
                (state, Some(read), None, material, triggers)
            } else {
                let mut state = try_stopped_unloaded_agent(&scope.agent_id)?;
                // Preserve the original child display fallback independently
                // of the general unloaded Summary's Stopped display.
                if identity.kind == AgentKind::Child {
                    state.status = AgentStatus::Booting;
                }
                let queue = crate::work_item_scheduling::WorkQueueReadModel::default();
                let projection = crate::runtime::SchedulerProjection::from_observations(
                    SchedulerAgentSnapshot::from_state(&state),
                    0,
                    queue,
                    SchedulerObservations {
                        active_tasks: Vec::new(),
                        has_interrupted_replay: false,
                        active_wait_conditions: &[],
                        activation_waits: Vec::new(),
                        execution_snapshot: None,
                        active_timers: 0,
                        runtime_error: false,
                    },
                    Utc::now(),
                );
                let material = crate::runtime::SchedulingReadMaterial::proven_empty_cold(
                    &scope,
                    ledger,
                    bytes,
                    records,
                    selected.scheduling_usage,
                    selected.scheduling_guards,
                )?;
                (state, None, Some(projection), material, Vec::new())
            };
            Ok(HeldColdSummary {
                identity: selected.identity,
                state,
                read,
                empty_projection,
                scheduling: scheduling.with_acquisition_material(acquisition.guard()?)?,
                acquisition: acquisition.clone(),
                metadata: selected.metadata,
                notifications: selected.notifications,
                triggers,
            })
        })?;
        drop(reservations);
        Ok(result)
    }
}
