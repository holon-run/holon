//! One operator State cut; selected/owned material follows the wire's last holder.
use super::input_evidence_budget::BudgetViewGuard;
use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use tokio_util::bytes::Bytes;

pub(crate) struct StateProjectionMaterial {
    pub(crate) acquisition: ResourceReadMaterial,
    pub(crate) _work: Option<Arc<BudgetViewGuard>>,
    pub(crate) _tasks: Option<Arc<BudgetViewGuard>>,
    pub(crate) _scheduling: Option<SchedulingReadMaterial>,
}

struct StateWire {
    bytes: Vec<u8>,
    _material: StateProjectionMaterial,
}

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "operator_state_tests.rs"]
mod tests;

impl AsRef<[u8]> for StateWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl StateProjectionMaterial {
    pub(crate) fn into_json_bytes(self, value: &impl serde::Serialize) -> Result<Bytes> {
        let bytes = self.acquisition.read(|| {
            let size = usize::try_from(super::input_evidence_budget::serialized_bytes(value)?)?;
            resource_io::charge_owned_payload(u64::try_from(size)?)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<StateWire>() + 128)?,
                1,
            )?;
            // Fixed-size writer cannot grow beyond the allocation admitted above.
            let mut bytes = vec![0; size];
            let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
            serde_json::to_writer(&mut writer, value)?;
            anyhow::ensure!(
                usize::try_from(writer.position())? == size,
                "State JSON size changed"
            );
            Ok(bytes)
        })?;
        Ok(Bytes::from_owner(StateWire {
            bytes,
            _material: self,
        }))
    }
}

impl RuntimeHandle {
    #[cfg(test)]
    pub(crate) async fn operator_state_read_projection(
        &self,
        task_limit: usize,
        timer_limit: usize,
    ) -> Result<crate::host::AgentStateReadProjection> {
        anyhow::ensure!(
            timer_limit <= super::resident_scheduling::SCHEDULING_HISTORY_WINDOW,
            super::resident_scheduling::SchedulingReadError::NeedsHistory
        );
        let root = self
            .observer_ready_root_bounded(&self.inner.runtime_db)
            .await?;
        self.operator_state_from_root(&root, task_limit, timer_limit)
    }

    pub(super) fn operator_state_from_root(
        &self,
        root: &super::resident_state::AgentSnapshot,
        task_limit: usize,
        timer_limit: usize,
    ) -> Result<crate::host::AgentStateReadProjection> {
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "State root scope differs"
        );
        let unavailable = || {
            crate::runtime_db::RuntimeDbRetryableError::new(
                Self::OBSERVER_PUBLICATION_OPERATION,
                self.inner.runtime_db.path(),
                "State dependencies need publication",
            )
        };
        let identity = root.identity.as_deref().ok_or_else(unavailable)?;
        anyhow::ensure!(
            identity.agent_id == root.state.id
                && identity.incarnation == root.token.identity_incarnation
                && identity.status == crate::types::AgentRegistryStatus::Active,
            "State projection identity differs"
        );
        anyhow::ensure!(
            root.queue_posture.is_some()
                && root.scheduler_events.is_some_and(|events| {
                    events.event_cursor
                        >= self.inner.scheduler_audit_frontier.load(Ordering::Acquire)
                })
                && root.work.is_some()
                && root.tasks.is_some()
                && root.scheduling.is_some()
                && root.input_evidence.is_some(),
            unavailable()
        );
        let mut read = self
            .capture_state_scheduler_from_root(root, self.now())
            .map_err(|error| {
                if state_domains_need_publication(&error) {
                    error.context(unavailable())
                } else {
                    error
                }
            })?;
        let acquisition = read.acquisition.clone();
        acquisition.read(|| {
            resource_io::admit_cloned_value(&(identity, read.state.as_ref()))?;
            let identity = AgentIdentityView::from_record(identity, &self.inner.default_agent_id);
            let state = read.state.as_ref().clone();
            let (work_queue, work_budget, active_work_item_count) =
                read.active_state_work_queue(&self.inner.input_evidence_budget)?;
            let closure = Self::closure_decision_from_projection(
                &state,
                &read.projection,
                None,
                &acquisition,
            )?;
            let active_task_count = read.projection.active_tasks.len();
            let mut tasks = std::mem::take(&mut read.projection.active_tasks);
            tasks.truncate(task_limit);
            let waits = read.work.unresolved_wait_conditions()?;
            let waits = &waits[..waits.len().min(50)];
            resource_io::admit_cloned_value(&waits)?;
            let waits = waits.iter().map(|wait| (*wait).clone()).collect();
            let scheduling = self.state_scheduling_records_from_capture(
                super::scheduling_reads::SchedulingReadSnapshot {
                    token: root.token,
                    state: root.state.clone(),
                    scheduling: read.scheduling,
                },
                timer_limit,
            )?;
            anyhow::ensure!(
                scheduling.token == root.token && scheduling.state.as_ref() == &state,
                "State/Scheduling cut differs"
            );
            let model = self.try_model_state_for(&state)?;
            let lifecycle =
                crate::types::AgentLifecycleHint::from_status(&state.id, state.status.clone());
            let (timers, external_triggers, scheduling_material) = scheduling.into_parts();
            let active_timer_count = scheduling_material.active_timer_count()?;
            let agent = LightweightAgentStateProjection {
                identity,
                agent: state,
                scheduling_posture: read.scheduling_posture,
                active_task_count,
                lifecycle,
                model,
                closure,
                work_queue,
                _scheduling_material: None,
            };
            let material = StateProjectionMaterial {
                acquisition: acquisition.clone(),
                _work: Some(work_budget),
                _tasks: Some(read._task_owned_budget),
                _scheduling: Some(scheduling_material),
            };
            Ok(crate::host::AgentStateReadProjection {
                source: crate::host::AgentStateProjectionSource::Loaded,
                agent,
                tasks,
                waits,
                timers,
                active_work_item_count,
                active_timer_count,
                external_triggers,
                material,
            })
        })
    }
}

fn state_domains_need_publication(error: &anyhow::Error) -> bool {
    use super::{
        resident_input_evidence::{InputEvidenceReadError, InputQueryError},
        resident_scheduling::SchedulingReadError,
        resident_task::TaskReadError,
        resident_work::WorkReadError,
    };
    use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;

    matches!(
        error.downcast_ref::<WorkReadError>(),
        Some(
            WorkReadError::NeedsPreparation(_)
                | WorkReadError::TurnWaitsNotPrepared(_)
                | WorkReadError::TaskWaitsNotPrepared(_)
                | WorkReadError::TriggerWaitsNotPrepared(_)
                | WorkReadError::IncompleteLive
                | WorkReadError::WindowExpansion { .. }
        )
    ) || matches!(
        error.downcast_ref::<TaskReadError>(),
        Some(
            TaskReadError::NeedsPreparation(_)
                | TaskReadError::IncompleteActive
                | TaskReadError::IncompleteResults
                | TaskReadError::WindowExpansion
        )
    ) || matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(
            SchedulingReadError::IncompleteLive
                | SchedulingReadError::NeedsHistory
                | SchedulingReadError::NeedsPreparation(_)
        )
    ) || error.is::<InputEvidenceReadError>()
        || matches!(
            error.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::NeedsPreparation)
        )
        || matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::NeedsPreparation(_))
        )
}
