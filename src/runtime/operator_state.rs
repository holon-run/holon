//! One operator State cut; selected/owned material follows the wire's last holder.
use super::input_evidence_budget::BudgetViewGuard;
use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use crate::types::ChildAgentSummary;
use tokio_util::bytes::Bytes;

pub(crate) struct StateProjectionMaterial {
    pub(crate) acquisition: ResourceReadMaterial,
    pub(crate) _work: Option<Arc<BudgetViewGuard>>,
    pub(crate) _tasks: Option<Arc<BudgetViewGuard>>,
    pub(crate) _scheduling: Option<SchedulingReadMaterial>,
    pub(crate) _children: Vec<Arc<BudgetViewGuard>>,
}

struct StateWire {
    bytes: Vec<u8>,
    _material: StateProjectionMaterial,
}

#[cfg(test)]
#[path = "operator_state_tests.rs"]
mod tests;

impl AsRef<[u8]> for StateWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl StateProjectionMaterial {
    pub(crate) fn retain_children(&mut self, children: &[ChildAgentSummary]) -> Result<()> {
        self.acquisition.read(|| {
            for child in children {
                for guard in [
                    child.observability._read_material.as_ref(),
                    child.observability._source_material.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    crate::runtime_db::repositories::reserve_source_vec_slot(&mut self._children)?;
                    self._children.push(guard.clone());
                }
            }
            Ok(())
        })
    }

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
    pub(crate) async fn operator_state_read_projection(
        &self,
        task_limit: usize,
        timer_limit: usize,
    ) -> Result<crate::host::AgentStateReadProjection> {
        anyhow::ensure!(
            timer_limit <= super::resident_scheduling::SCHEDULING_HISTORY_WINDOW,
            super::resident_scheduling::SchedulingReadError::NeedsHistory
        );
        let mut projection = {
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
                .context("State root needs publication")?;
            if root
                .current
                .as_ref()
                .and_then(|current| current.query.as_ref())
                .is_none()
            {
                drop(root);
                drop(reservations);
                // Explicit accepted query preparation; a Ready read never creates a candidate.
                let mut owner = self.begin_mutation().await?;
                owner.prepare_current_query(None).await?;
                self.operator_state_from_root(
                    owner.capture_published_root()?.as_ref(),
                    task_limit,
                    timer_limit,
                )?
            } else {
                self.operator_state_from_root(&root, task_limit, timer_limit)?
            }
        };
        // The parent cut and its reservation are released before observing children.
        projection.agent.active_children = if let Some(bridge) = self.inner.host_bridge.as_ref() {
            bridge.child_summaries(&projection.agent.agent.id).await?
        } else {
            Vec::new()
        };
        projection
            .material
            .retain_children(&projection.agent.active_children)?;
        Ok(projection)
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
        let mut read = self.capture_operator_scheduler_from_root(root, self.now())?;
        let acquisition = read.acquisition.clone();
        acquisition.read(|| {
            let query = root
                .current
                .as_ref()
                .and_then(|current| current.query.as_ref())
                .context("Current identity needs preparation")?;
            let identity = query.identity.as_ref();
            anyhow::ensure!(
                identity.agent_id == read.state.id
                    && identity.incarnation == self.inner.identity_incarnation
                    && identity.status == crate::types::AgentRegistryStatus::Active,
                "State projection identity differs"
            );
            resource_io::admit_cloned_value(&(identity, read.state.as_ref()))?;
            let identity = AgentIdentityView::from_record(identity, &self.inner.default_agent_id);
            let state = read.state.as_ref().clone();
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
            let scheduling = self.scheduling_records_from_capture(
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
            let agent = LightweightAgentStateProjection {
                identity,
                agent: state,
                scheduling_posture: read.scheduling_posture,
                active_task_count,
                lifecycle,
                model,
                closure,
                active_children: Vec::new(),
                work_queue: read.work_queue,
                _work_budget: None,
                _scheduling_material: None,
            };
            let material = StateProjectionMaterial {
                acquisition: acquisition.clone(),
                _work: Some(read._owned_budget),
                _tasks: Some(read._task_owned_budget),
                _scheduling: Some(scheduling_material),
                _children: Vec::new(),
            };
            Ok(crate::host::AgentStateReadProjection {
                source: crate::host::AgentStateProjectionSource::Loaded,
                agent,
                tasks,
                waits,
                timers,
                external_triggers,
                material,
            })
        })
    }
}
