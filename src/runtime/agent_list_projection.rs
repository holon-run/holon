//! Small, committed Agent list entries. Preparation belongs to the producer.

use std::{cmp::Reverse, fmt, sync::Arc};

use anyhow::{ensure, Result};
use rusqlite::{Connection, Transaction};

use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    lightweight_agent_list_waiting_reason,
    resident_input_evidence::InputEvidenceScope,
    resident_scheduling::ResidentSchedulingDomain,
    resident_work::{derive_ranked_work_posture, ResidentWorkDomain, WorkCaptureRequest},
    try_agent_model_list_summary_for_fields, AgentModelSummaryFields,
};
use crate::{
    config::{ModelListCatalog, ModelRouteRef, ServiceTier},
    diagnostics::resource_io::{self, ResourceReadMaterial},
    host_registry::RuntimeRegistry,
    queue::QueuePostureFacts,
    runtime_db::{context_inputs::ContextDurableRead, current_query::queue_posture_tx, RuntimeDb},
    storage::derive_agent_posture_from_state,
    types::{
        ActiveWorkspaceEntry, AgentIdentityRecord, AgentIdentityView, AgentLifecycleHint,
        AgentListEntry, AgentPostureProjection, AgentRegistryStatus, AgentState, AgentStatus,
        AgentVisibility, WaitingReason,
    },
    work_item_scheduling::{compare_queue_work_rank, queue_work_rank, QueueWorkRank},
};

pub(super) const PUBLICATION_OPERATION: &str = "observer publication";

pub(crate) fn is_listed(identity: &AgentIdentityRecord) -> bool {
    identity.status == AgentRegistryStatus::Active && identity.visibility == AgentVisibility::Public
}

#[derive(Debug)]
pub(crate) struct AgentListProjection {
    pub(super) identity_revision: u64,
    status: AgentStatus,
    pending: usize,
    current_run_id: Option<String>,
    waiting_reason: Option<WaitingReason>,
    workspace: Option<ActiveWorkspaceEntry>,
    model_override: Option<ModelRouteRef>,
    model_override_reasoning_effort: Option<String>,
    model_override_service_tier: Option<ServiceTier>,
    last_requested_model: Option<ModelRouteRef>,
    last_active_model: Option<ModelRouteRef>,
    loaded_posture: AgentPostureProjection,
    // When both modes agree, share the loaded value.
    cold_posture: Option<AgentPostureProjection>,
    _budget: Arc<BudgetViewGuard>,
}

impl AgentListProjection {
    fn new(
        identity: &AgentIdentityRecord,
        state: &AgentState,
        loaded_posture: AgentPostureProjection,
        cold_posture: AgentPostureProjection,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let cold_posture = (cold_posture != loaded_posture).then_some(cold_posture);
        let budget = ledger.register_owned_bytes(
            &InputEvidenceScope {
                agent_id: identity.agent_id.clone(),
                identity_incarnation: identity.incarnation,
            },
            InputMemoryCategory::Current,
            OwnedBytesEstimate {
                payload_bytes: serialized_bytes(&(
                    &state.status,
                    &state.current_run_id,
                    &state.active_workspace_entry,
                    AgentModelSummaryFields::from(state),
                    &loaded_posture,
                    &cold_posture,
                ))?,
                metadata_estimate_bytes: std::mem::size_of::<Self>() as u64,
            },
        )?;
        Ok(Self {
            identity_revision: identity.revision,
            status: state.status.clone(),
            pending: state.pending,
            current_run_id: state.current_run_id.clone(),
            waiting_reason: lightweight_agent_list_waiting_reason(state),
            workspace: state
                .active_workspace_entry
                .clone()
                .map(ActiveWorkspaceEntry::without_projection_metadata),
            model_override: state.model_override.clone(),
            model_override_reasoning_effort: state.model_override_reasoning_effort.clone(),
            model_override_service_tier: state.model_override_service_tier,
            last_requested_model: state.last_requested_model.clone(),
            last_active_model: state.last_active_model.clone(),
            loaded_posture,
            cold_posture,
            _budget: budget,
        })
    }

    pub(crate) fn to_entry<'a>(
        &self,
        identity: &AgentIdentityRecord,
        loaded: bool,
        catalog: impl Into<ModelListCatalog<'a>>,
        default_agent_id: &str,
    ) -> Result<AgentListEntry> {
        let posture = if loaded {
            &self.loaded_posture
        } else {
            self.cold_posture.as_ref().unwrap_or(&self.loaded_posture)
        };
        resource_io::admit_cloned_value(&(
            identity,
            &self.status,
            &self.current_run_id,
            &self.workspace,
            posture,
        ))?;
        let model = try_agent_model_list_summary_for_fields(
            catalog.into(),
            AgentModelSummaryFields {
                model_override: self.model_override.as_ref(),
                model_override_reasoning_effort: self.model_override_reasoning_effort.as_deref(),
                model_override_service_tier: self.model_override_service_tier,
                last_requested_model: self.last_requested_model.as_ref(),
                last_active_model: self.last_active_model.as_ref(),
            },
        )?;
        Ok(AgentListEntry {
            identity: AgentIdentityView::from_record(identity, default_agent_id),
            lifecycle: AgentLifecycleHint::try_from_status(
                &identity.agent_id,
                self.status.clone(),
            )?,
            status: self.status.clone(),
            scheduling_posture: posture.clone(),
            pending: self.pending,
            current_run_id: self.current_run_id.clone(),
            waiting_reason: self.waiting_reason,
            model,
            active_workspace_entry: self.workspace.clone(),
        })
    }
}

pub(crate) struct AgentListProjectionSource {
    pub(super) db: RuntimeDb,
    registry: RuntimeRegistry,
    ledger: InputEvidenceBudgetLedger,
}

impl fmt::Debug for AgentListProjectionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentListProjectionSource")
            .field("db", &self.db.path())
            .finish_non_exhaustive()
    }
}

impl AgentListProjectionSource {
    pub(crate) fn new(
        db: RuntimeDb,
        registry: RuntimeRegistry,
        ledger: InputEvidenceBudgetLedger,
    ) -> Self {
        Self {
            db,
            registry,
            ledger,
        }
    }

    pub(super) fn identity(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
        self.registry.agent_identity_record(agent_id)
    }

    pub(super) fn matches_identity(&self, identity: &AgentIdentityRecord) -> bool {
        self.registry.matches_agent_identity(identity)
    }

    pub(super) fn read(
        &self,
        tx: &Transaction<'_>,
        identity: &AgentIdentityRecord,
        expected_state: Option<&AgentState>,
    ) -> Result<AgentListProjection> {
        ResourceReadMaterial::without_current(|| {
            let material = ResourceReadMaterial::current_or_new_for_identity(
                &self.ledger,
                &identity.agent_id,
                identity.incarnation,
            )?;
            let (state, loaded, cold) = material.read(|| {
                let facts = ContextDurableRead::new(tx, &identity.agent_id).scope_facts()?;
                ensure!(
                    facts.identity.as_ref() == Some(identity),
                    "list identity changed"
                );
                ensure!(
                    expected_state.is_none_or(|expected| facts.state.as_deref() == Some(expected)),
                    "list State differs from the committed ACK"
                );
                let state = facts.state.unwrap_or_else(|| {
                    let mut absent = AgentState::new(identity.agent_id.clone());
                    absent.status = AgentStatus::Stopped;
                    Arc::new(absent)
                });
                if let Some(posture) = derive_agent_posture_from_state(&state, false)? {
                    return Ok((state, posture.clone(), posture));
                }
                let queue = queue_posture_tx(tx, &identity.agent_id)?;
                if let Some(posture) = derive_agent_posture_from_state(&state, queue.has_queued)? {
                    return Ok((state, posture.clone(), posture));
                }
                let (loaded, cold) = read_work_postures(tx, &state, queue)?;
                Ok((state, loaded, cold))
            })?;
            AgentListProjection::new(identity, &state, loaded, cold, &self.ledger)
        })
    }

    pub(super) fn from_state(
        &self,
        identity: &AgentIdentityRecord,
        state: &AgentState,
        queue: Option<QueuePostureFacts>,
        work: Option<&ResidentWorkDomain>,
        scheduling: Option<&ResidentSchedulingDomain>,
    ) -> Result<Option<AgentListProjection>> {
        ResourceReadMaterial::without_current(|| {
            ensure!(state.id == identity.agent_id, "list State scope differs");
            let posture = derive_agent_posture_from_state(
                state,
                queue.is_some_and(|queue| queue.has_queued),
            )?;
            if let Some(posture) = posture {
                return AgentListProjection::new(
                    identity,
                    state,
                    posture.clone(),
                    posture,
                    &self.ledger,
                )
                .map(Some);
            }
            let (Some(queue), Some(work), Some(scheduling)) = (queue, work, scheduling) else {
                return Ok(None);
            };
            if !work.coverage().live_complete
                || work.coverage().nonopen.limit < super::resident_work::WORK_NONOPEN_WINDOW
                || !scheduling.coverage().active_triggers_complete
            {
                return Ok(None);
            }
            ensure!(
                work.scope().agent_id == state.id
                    && work.scope().identity_incarnation == identity.incarnation
                    && scheduling.scope() == work.scope(),
                "list Work and Scheduling scope differs"
            );
            let material = ResourceReadMaterial::current_or_new(&self.ledger, work.scope())?;
            let (loaded, cold) = material.read(|| {
                let work = work.capture(&WorkCaptureRequest::current())?;
                let triggers = scheduling
                    .active_default_trigger()?
                    .map_or(&[][..], std::slice::from_ref);
                let ranked = work.queue_work_ranks(state, triggers)?;
                let mut waits = work.context_active_wait_conditions()?;
                let loaded =
                    derive_ranked_work_posture(state, false, &ranked, waits.iter().copied())?;
                let cold = match derive_agent_posture_from_state(state, queue.has_interrupted)? {
                    Some(posture) => posture,
                    None => {
                        waits.sort_unstable_by(|left, right| {
                            (
                                Reverse(left.updated_at.timestamp_millis()),
                                Reverse(left.created_at.timestamp_millis()),
                                &left.id,
                            )
                                .cmp(&(
                                    Reverse(right.updated_at.timestamp_millis()),
                                    Reverse(right.created_at.timestamp_millis()),
                                    &right.id,
                                ))
                        });
                        derive_ranked_work_posture(state, false, &ranked, waits.into_iter())?
                    }
                };
                Ok((loaded, cold))
            })?;
            AgentListProjection::new(identity, state, loaded, cold, &self.ledger).map(Some)
        })
    }
}

// Read the live inputs only. Non-open Work ranks as completing/completed and
// cannot be selected by the list posture policy.
fn read_work_postures(
    tx: &Transaction<'_>,
    state: &AgentState,
    queue: QueuePostureFacts,
) -> Result<(AgentPostureProjection, AgentPostureProjection)> {
    use crate::runtime_db::repositories::{
        context_active_waits_on, context_open_work_items_on, decode_external_trigger_payload,
    };

    let works = context_open_work_items_on(tx, &state.id)?;
    if works.is_empty() {
        let loaded = derive_ranked_work_posture(state, false, &[], std::iter::empty())?;
        let cold = derive_agent_posture_from_state(state, queue.has_interrupted)?
            .unwrap_or_else(|| loaded.clone());
        return Ok((loaded, cold));
    }
    let waits = context_active_waits_on(tx, &state.id)?;
    let triggers = read_records(
        tx,
        "SELECT payload_json FROM external_triggers WHERE target_agent_id=?1 AND status='active'
         ORDER BY created_at DESC, external_trigger_id ASC",
        &state.id,
        |payload| {
            resource_io::admit_typed_json_payload(payload)?;
            decode_external_trigger_payload(payload)
        },
    )?;
    let suspended = read_records(
        tx,
        "SELECT DISTINCT suspended_work_item_id FROM work_item_continuations
         WHERE agent_id=?1 AND state='active' ORDER BY suspended_work_item_id",
        &state.id,
        |id| {
            resource_io::charge_owned_metadata(id.len() as u64, 1)?;
            Ok(id.to_owned())
        },
    )?;
    ensure!(
        works.iter().all(|work| work.agent_id == state.id)
            && waits.iter().all(|wait| wait.agent_id == state.id)
            && triggers
                .iter()
                .all(|trigger| trigger.target_agent_id == state.id),
        "list posture inputs differ from State scope",
    );
    ensure!(triggers.len() <= 1, "multiple active default Triggers");
    let mut ranks: Vec<QueueWorkRank<'_>> = Vec::new();
    for work in &works {
        crate::runtime_db::repositories::reserve_source_vec_slot(&mut ranks)?;
        ranks.push(queue_work_rank(
            work,
            state.current_work_item_id.as_deref() == Some(&work.id),
            suspended.binary_search(&work.id).is_ok(),
            waits.iter(),
            &|id| {
                triggers
                    .first()
                    .filter(|trigger| trigger.external_trigger_id == id)
                    .and_then(|trigger| trigger.last_delivered_at)
            },
        ));
    }
    ranks.sort_unstable_by(compare_queue_work_rank);
    let mut loaded_waits = Vec::new();
    for wait in &waits {
        crate::runtime_db::repositories::reserve_source_vec_slot(&mut loaded_waits)?;
        loaded_waits.push(wait);
    }
    loaded_waits.sort_unstable_by(|left, right| {
        (Reverse(left.updated_at), Reverse(left.created_at), &left.id).cmp(&(
            Reverse(right.updated_at),
            Reverse(right.created_at),
            &right.id,
        ))
    });
    let loaded = derive_ranked_work_posture(state, false, &ranks, loaded_waits.into_iter())?;
    let cold = match derive_agent_posture_from_state(state, queue.has_interrupted)? {
        Some(posture) => posture,
        None => derive_ranked_work_posture(state, false, &ranks, waits.iter())?,
    };
    Ok((loaded, cold))
}

fn read_records<T>(
    connection: &Connection,
    sql: &str,
    id: &str,
    decode: impl Fn(&str) -> Result<T>,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([id])?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        crate::runtime_db::repositories::reserve_source_vec_slot(&mut records)?;
        let payload = row.get_ref(0)?.as_str()?;
        records.push(decode(payload)?);
    }
    Ok(records)
}
