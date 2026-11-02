//! One explicit cold query under its exact incarnation owner; no activation.
use super::*;
use crate::runtime_db::scheduling_snapshot::{ColdSchedulingQuery, SchedulingDomainLoadRequest};

pub(super) struct HeldColdSchedulingRead {
    pub identity: AgentIdentityRecord,
    pub state: Option<AgentState>,
    pub timers: Vec<crate::runtime::TimerRecordLoan>,
    pub triggers: Vec<crate::runtime::ExternalTriggerRecordLoan>,
    pub material: crate::runtime::SchedulingReadMaterial,
    pub acquisition: crate::diagnostics::resource_io::ResourceReadMaterial,
    pub reservations: Vec<MutationReservation>,
}
impl RuntimeHost {
    /// Sources and retained scheduler material share the caller's allowance.
    pub(super) fn cold_scheduler_with_observed_scheduling(
        &self,
        identity: &AgentIdentityRecord,
        storage: &AppStorage,
        state: &AgentState,
        queue: std::borrow::Cow<'_, crate::work_item_scheduling::WorkQueueReadModel>,
        material: crate::runtime::SchedulingReadMaterial,
    ) -> Result<(
        crate::runtime::SchedulerProjection,
        crate::runtime::SchedulingReadMaterial,
    )> {
        let active_timers = material.active_timer_count()?;
        let phase =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new_for_identity(
                &self.inner.input_evidence_budget,
                &identity.agent_id,
                identity.incarnation,
            )?;
        let projection = phase.read(|| match queue {
            std::borrow::Cow::Owned(queue) => {
                crate::runtime::SchedulerProjection::from_state_with_scheduling_observation(
                    storage, state, queue, active_timers, &phase,
                )
            }
            std::borrow::Cow::Borrowed(queue) => {
                crate::runtime::SchedulerProjection::from_state_with_borrowed_scheduling_observation(
                    storage, state, queue, active_timers, &phase,
                )
            }
        })?;
        Ok((
            projection,
            material.with_scheduler_work_material(phase.guard()?)?,
        ))
    }

    /// Only capture's owning selection metadata enters this synchronous phase.
    /// CapturedScheduling already retains the original guard through consumption.
    pub(super) fn capture_cold_scheduling(
        &self,
        identity: &AgentIdentityRecord,
        resident: &crate::runtime::resident_scheduling::ResidentSchedulingDomain,
        request: &crate::runtime::resident_scheduling::SchedulingCaptureRequest,
    ) -> Result<crate::runtime::resident_scheduling::CapturedScheduling> {
        let phase =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new_for_identity(
                &self.inner.input_evidence_budget,
                &identity.agent_id,
                identity.incarnation,
            )?;
        let selected = {
            let _entry = phase.enter();
            resident.capture(request)?
        };
        phase.finish()?;
        Ok(selected)
    }

    /// Only the synchronous Work queue adapter enters the resource phase.
    /// Its original guard follows the existing Scheduling response holder.
    pub(super) fn cold_work_queue_with_observed_scheduling(
        &self,
        identity: &AgentIdentityRecord,
        storage: &AppStorage,
        state: &AgentState,
        triggers: &[crate::runtime::ExternalTriggerRecordLoan],
        material: crate::runtime::SchedulingReadMaterial,
    ) -> Result<(
        crate::work_item_scheduling::WorkQueueReadModel,
        crate::runtime::SchedulingReadMaterial,
    )> {
        let phase =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new_for_identity(
                &self.inner.input_evidence_budget,
                &identity.agent_id,
                identity.incarnation,
            )?;
        let queue = {
            let _entry = phase.enter();
            storage.work_queue_with_observed_scheduling(state, triggers.iter().map(|row| &**row))?
        };
        Ok((queue, material.with_work_queue_material(phase.guard()?)?))
    }

    pub(super) async fn acquire_cold_scheduling_query(
        &self,
        identity: &AgentIdentityRecord,
        timer_limit: usize,
    ) -> Result<HeldColdSchedulingRead> {
        self.acquire_cold_scheduling_query_in(identity, timer_limit, self.runtime_db())
            .await
    }

    pub(super) async fn acquire_cold_scheduling_query_in(
        &self,
        identity: &AgentIdentityRecord,
        timer_limit: usize,
        db: &RuntimeDb,
    ) -> Result<HeldColdSchedulingRead> {
        anyhow::ensure!(
            timer_limit <= crate::runtime::resident_scheduling::SCHEDULING_HISTORY_WINDOW,
            crate::runtime::resident_scheduling::SchedulingReadError::NeedsHistory
        );
        let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let reservations = self
            .reserve_mutation_scopes_in(&[(scope.agent_id.clone(), scope.identity_incarnation)], db)
            .await?;
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let request = SchedulingDomainLoadRequest {
            capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("cold Scheduling byte limit overflow")?,
            )?,
            record_limit: usize::try_from(limits.record_limit)?,
        };
        // All work below is synchronous. Never carry the TLS entry across the
        // reservation await above; the original response retains the guard.
        let acquisition =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(ledger, &scope)?;
        let _entry = acquisition.enter();
        let ready = reservations[0].capture_ready_snapshot()?;
        let (identity, state, timers, triggers, material) = if let Some(root) = ready {
            anyhow::ensure!(
                root.state.id == scope.agent_id
                    && root.token.identity_incarnation == scope.identity_incarnation,
                "cold Scheduling published scope differs"
            );
            let resident = root
                .scheduling
                .as_ref()
                .ok_or(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive)?;
            let selected = self.capture_cold_scheduling(identity, resident, &request.capture)?;
            let (mut facts, usage, guards) = db.transaction(|tx| {
                crate::runtime_db::scheduling_snapshot::read_cold_scheduling_scope_tx(
                    tx, &scope, &request, ledger,
                )
            })?;
            anyhow::ensure!(
                facts.state.as_deref() == Some(root.state.as_ref()),
                "cold State and selected Scheduling publication differs"
            );
            let (timers, triggers, material) = crate::runtime::project_cold_scheduling_records(
                selected,
                timer_limit,
                ledger,
                request.byte_limit,
                request.record_limit,
                usage,
                guards,
            )?;
            (
                facts
                    .identity
                    .take()
                    .context("cold Scheduling identity absent")?,
                facts.state.take(),
                timers,
                triggers,
                material,
            )
        } else {
            let read = db.transaction(|tx| {
                crate::runtime_db::scheduling_snapshot::prepare_cold_scheduling_query_tx(
                    tx, &scope, &request, ledger,
                )
            })?;
            match read {
                ColdSchedulingQuery::Current(current) => {
                    let crate::runtime_db::scheduling_snapshot::SchedulingCurrentSnapshot {
                        mut facts,
                        resident,
                        material_usage,
                        _read_material,
                        ..
                    } = *current;
                    let selected =
                        self.capture_cold_scheduling(identity, &resident, &request.capture)?;
                    let (timers, triggers, material) =
                        crate::runtime::project_cold_scheduling_records(
                            selected,
                            timer_limit,
                            ledger,
                            request.byte_limit,
                            request.record_limit,
                            material_usage,
                            _read_material,
                        )?;
                    (
                        facts
                            .identity
                            .take()
                            .context("cold Scheduling identity absent")?,
                        facts.state.take(),
                        timers,
                        triggers,
                        material,
                    )
                }
                ColdSchedulingQuery::ProvenEmptyNoState {
                    identity,
                    material_usage,
                    guards,
                } => {
                    let material = crate::runtime::SchedulingReadMaterial::proven_empty_cold(
                        &scope,
                        ledger,
                        request.byte_limit,
                        request.record_limit,
                        material_usage,
                        guards,
                    )?;
                    (identity, None, Vec::new(), Vec::new(), material)
                }
            }
        };
        Ok(HeldColdSchedulingRead {
            identity,
            state: state
                .map(crate::runtime_db::context_inputs::own_state)
                .transpose()?,
            timers,
            triggers,
            material: material.with_acquisition_material(acquisition.guard()?)?,
            acquisition,
            reservations,
        })
    }
}
