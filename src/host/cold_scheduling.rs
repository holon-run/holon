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
        self.acquire_cold_scheduling_query_in_scope(scope, timer_limit, (0, 0), Vec::new())
            .await
    }

    pub(super) async fn acquire_cold_scheduling_query_by_id(
        &self,
        agent_id: &str,
        timer_limit: usize,
    ) -> std::result::Result<HeldColdSchedulingRead, PublicAgentError> {
        let limits = self.inner.input_evidence_budget.limits();
        let request = SchedulingDomainLoadRequest {
            capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .ok_or_else(|| {
                        PublicAgentError::Runtime(anyhow!("cold locator limit overflow"))
                    })?
                    .min(64 * 1024 * 1024),
            )
            .map_err(|error| PublicAgentError::Runtime(error.into()))?,
            record_limit: usize::try_from(limits.record_limit.min(65_536))
                .map_err(|error| PublicAgentError::Runtime(error.into()))?,
        };
        let locator = self
            .runtime_db()
            .transaction(|tx| {
                crate::runtime_db::scheduling_snapshot::read_cold_resource_locator_tx(
                    tx,
                    agent_id,
                    &request,
                    &self.inner.input_evidence_budget,
                )
            })
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_owned(),
            })?;
        match locator.status {
            AgentRegistryStatus::Active => {}
            AgentRegistryStatus::Deleting => {
                return Err(PublicAgentError::Deleting {
                    agent_id: agent_id.to_owned(),
                })
            }
            AgentRegistryStatus::Deleted => {
                return Err(PublicAgentError::Deleted {
                    agent_id: agent_id.to_owned(),
                })
            }
        }
        self.acquire_cold_scheduling_query_in_scope(
            locator.scope,
            timer_limit,
            locator.usage,
            locator.guards,
        )
        .await
        .map_err(PublicAgentError::Runtime)
    }

    async fn acquire_cold_scheduling_query_in_scope(
        &self,
        scope: crate::runtime::resident_input_evidence::InputEvidenceScope,
        timer_limit: usize,
        prior: (usize, usize),
        mut prior_guards: Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    ) -> Result<HeldColdSchedulingRead> {
        anyhow::ensure!(
            timer_limit <= crate::runtime::resident_scheduling::SCHEDULING_HISTORY_WINDOW,
            crate::runtime::resident_scheduling::SchedulingReadError::NeedsHistory
        );
        let reservations = self
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let full_request = SchedulingDomainLoadRequest {
            capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("cold Scheduling byte limit overflow")?,
            )?,
            record_limit: usize::try_from(limits.record_limit)?,
        };
        let request = SchedulingDomainLoadRequest {
            capture: full_request.capture.clone(),
            byte_limit: full_request
                .byte_limit
                .min(64 * 1024 * 1024)
                .checked_sub(prior.0)
                .context("ResourceExhausted: cold locator bytes")?,
            record_limit: full_request
                .record_limit
                .min(65_536)
                .checked_sub(prior.1)
                .context("ResourceExhausted: cold locator records")?,
        };
        let combined = |usage: (usize, usize)| -> Result<(usize, usize)> {
            Ok((
                prior
                    .0
                    .checked_add(usage.0)
                    .context("cold material bytes overflow")?,
                prior
                    .1
                    .checked_add(usage.1)
                    .context("cold material records overflow")?,
            ))
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
                full_request.byte_limit,
                full_request.record_limit,
                combined(usage)?,
                {
                    prior_guards.extend(guards);
                    prior_guards
                },
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
                            full_request.byte_limit,
                            full_request.record_limit,
                            combined(material_usage)?,
                            {
                                prior_guards.extend(_read_material);
                                prior_guards
                            },
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
                        full_request.byte_limit,
                        full_request.record_limit,
                        combined(material_usage)?,
                        {
                            prior_guards.extend(guards);
                            prior_guards
                        },
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
