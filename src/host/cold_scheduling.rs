//! One explicit cold query under its exact incarnation owner; no activation.
use super::*;
use crate::runtime_db::scheduling_snapshot::{ColdSchedulingQuery, SchedulingDomainLoadRequest};

pub(super) struct HeldColdSchedulingRead {
    pub identity: AgentIdentityRecord,
    pub state: Option<AgentState>,
    pub timers: Vec<TimerRecord>,
    pub triggers: Vec<ExternalTriggerRecord>,
    pub material: crate::runtime::SchedulingReadMaterial,
    pub reservations: Vec<MutationReservation>,
}
impl RuntimeHost {
    pub(super) async fn acquire_cold_scheduling_query(
        &self,
        identity: &AgentIdentityRecord,
        timer_limit: usize,
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
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
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
        let ready = reservations[0].capture_ready_snapshot()?;
        let (identity, state, timers, triggers, material) = if let Some(root) = ready {
            anyhow::ensure!(
                root.state.id == scope.agent_id
                    && root.token.identity_incarnation == scope.identity_incarnation,
                "cold Scheduling published scope differs"
            );
            let selected = root
                .scheduling
                .as_ref()
                .ok_or(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive)?
                .capture(&request.capture)?;
            let (mut facts, usage, guards) = self.runtime_db().transaction(|tx| {
                crate::runtime_db::scheduling_snapshot::read_cold_scheduling_scope_tx(
                    tx, &scope, &request, ledger,
                )
            })?;
            anyhow::ensure!(
                facts.state.as_ref() == Some(root.state.as_ref()),
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
            let read = self.runtime_db().transaction(|tx| {
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
                    let selected = resident.capture(&request.capture)?;
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
            state,
            timers,
            triggers,
            material,
            reservations,
        })
    }
}
