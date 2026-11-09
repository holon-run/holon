//! Same-owner finite cold Summary facts; no Runtime or publication is created.
use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use crate::runtime::{ColdSchedulerFacts, SchedulerAgentSnapshot, SchedulerObservations};

pub(super) struct ColdObserverCut {
    owner: crate::runtime::mutation_owner::MutationOwner,
    token: crate::runtime::mutation_owner::PublicationToken,
    root: Option<Arc<crate::runtime::resident_state::AgentSnapshot>>,
    db: RuntimeDb,
}
impl ColdObserverCut {
    pub(super) fn capture(
        host: &RuntimeHost,
        identity: &AgentIdentityRecord,
        db: &RuntimeDb,
    ) -> Result<Self> {
        db.check_read_scope()?;
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&identity.agent_id, identity.incarnation)?;
        let (token, root) = owner.capture_with_token().map_err(|error| {
            crate::runtime_db::RuntimeDbRetryableError::new(
                "cold observer publication",
                db.path(),
                error,
            )
        })?;
        Ok(Self {
            owner,
            token,
            root,
            db: db.clone(),
        })
    }
    pub(super) fn validate_state(&self, state: Option<&AgentState>) -> Result<()> {
        if let Some(root) = &self.root {
            if state != Some(root.state.as_ref()) {
                self.check()?;
            }
            anyhow::ensure!(
                state == Some(root.state.as_ref()),
                "cold observer State differs from publication"
            );
        }
        Ok(())
    }
    pub(super) fn check(&self) -> Result<()> {
        self.db.check_read_scope()?;
        let (token, current) = self.owner.capture_with_token().map_err(|error| {
            crate::runtime_db::RuntimeDbRetryableError::new(
                "cold observer publication",
                self.db.path(),
                error,
            )
        })?;
        let same = match (&self.root, &current) {
            (None, None) => true,
            (Some(prior), Some(current)) => Arc::ptr_eq(prior, current),
            _ => false,
        };
        anyhow::ensure!(
            self.token == token && same,
            crate::runtime_db::RuntimeDbRetryableError::new(
                "cold observer publication",
                self.db.path(),
                "publication changed during read"
            )
        );
        Ok(())
    }
}

pub(super) fn needs_query_preparation(error: &anyhow::Error) -> bool {
    use crate::runtime::{
        resident_scheduling::SchedulingReadError, resident_task::TaskReadError,
        resident_work::WorkReadError,
    };
    matches!(error.downcast_ref::<SchedulingReadError>(), Some(SchedulingReadError::IncompleteLive | SchedulingReadError::NeedsPreparation(_)))
        || matches!(error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(), Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::NeedsPreparation(_)))
        || matches!(error.downcast_ref::<WorkReadError>(), Some(WorkReadError::IncompleteLive | WorkReadError::NeedsPreparation(_)))
        || matches!(error.downcast_ref::<crate::runtime_db::work_snapshot::WorkSnapshotError>(), Some(crate::runtime_db::work_snapshot::WorkSnapshotError::NeedsPreparation(_) | crate::runtime_db::work_snapshot::WorkSnapshotError::CoverageNeedsPreparation))
        || matches!(error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(), Some(crate::runtime_db::task_snapshot::TaskSnapshotError::NeedsPreparation(_) | crate::runtime_db::task_snapshot::TaskSnapshotError::CoverageNeedsPreparation))
        || matches!(error.downcast_ref::<TaskReadError>(), Some(TaskReadError::NeedsPreparation(_) | TaskReadError::IncompleteActive | TaskReadError::IncompleteResults | TaskReadError::IncompleteStops | TaskReadError::IncompleteQuery | TaskReadError::IncompleteStatusProjection))
        || matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(), Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(_)))
        || matches!(error.downcast_ref::<crate::runtime::resident_input_evidence::InputEvidenceReadError>(), Some(crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(_) | crate::runtime::resident_input_evidence::InputEvidenceReadError::WindowNotPrepared))
}

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
        let observation = ColdObserverCut::capture(self, identity, db)?;
        let ledger = &self.inner.input_evidence_budget;
        let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let acquisition = ResourceReadMaterial::current_or_new(ledger, &scope)?;
        let read = acquisition.read(|| {
            db.with_read(|connection| {
                let tx = connection.transaction()?;
                let selected = crate::runtime_db::cold_summary::load_tx(&tx, &scope, ledger)?;
                tx.commit()?;
                Ok(selected)
            })
        });
        let (selected, reservations, observation) = match read {
            Ok(selected) => {
                observation.validate_state(
                    selected
                        .domains
                        .as_ref()
                        .map(|domains| domains.state.as_ref()),
                )?;
                (selected, Vec::new(), Some(observation))
            }
            Err(error) if needs_query_preparation(&error) => {
                let reservations = self
                    .reserve_mutation_scopes_in(
                        &[(identity.agent_id.clone(), identity.incarnation)],
                        db,
                    )
                    .await?;
                let selected = acquisition.read(|| {
                    db.transaction(|tx| {
                        crate::runtime_db::cold_summary::prepare_tx(tx, &scope, ledger)
                    })
                })?;
                (selected, reservations, None)
            }
            Err(error) => return Err(error),
        };
        let result = acquisition.read(|| {
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
        if let Some(observation) = observation {
            observation.check()?;
        }
        drop(reservations);
        Ok(result)
    }
}
