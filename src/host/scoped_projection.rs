//! Scoped projection assembly; invoke only from a registered blocking worker.

use super::*;
use tokio::runtime::Handle;

impl RuntimeHost {
    fn agent_storage_read_only_scoped(&self, agent_id: &str, db: &RuntimeDb) -> Result<AppStorage> {
        AppStorage::open_read_only_for_agent(
            self.agent_data_dir(agent_id),
            agent_id.to_owned(),
            db.clone(),
        )
    }

    pub(crate) fn operator_agent_state_projection_scoped(
        &self,
        agent_id: &str,
        task_limit: usize,
        timer_limit: usize,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> std::result::Result<AgentStateReadProjection, PublicAgentError> {
        db.check_read_scope().map_err(PublicAgentError::Runtime)?;
        let runtime = handle
            .block_on(self.try_get_loaded_runtime_scoped(agent_id, db))
            .map_err(PublicAgentError::Runtime)?;
        if let Some(runtime) = runtime {
            crate::diagnostics::record_projection_state_source_loaded();
            runtime
                .prepare_operator_current_scoped(db, handle)
                .map_err(PublicAgentError::Runtime)?;
            return runtime
                .operator_state_read_projection_scoped(task_limit, timer_limit, db, handle)
                .map_err(PublicAgentError::Runtime);
        }
        let identity = self.active_agent_identity(agent_id)?;
        crate::diagnostics::record_projection_state_source_storage();
        crate::diagnostics::record_projection_state_runtime_spawn_avoided();
        tracing::debug!(
            agent_id,
            state_projection_source = AgentStateProjectionSource::Storage.as_str(),
            "building public agent state projection"
        );
        let storage = self
            .agent_storage_read_only_scoped(agent_id, db)
            .map_err(PublicAgentError::Runtime)?;

        let cold_scheduling::HeldColdSchedulingRead {
            identity,
            state,
            timers,
            triggers: external_triggers,
            material: scheduling_material,
            acquisition,
            reservations,
        } = handle
            .block_on(self.acquire_cold_scheduling_query_in(&identity, timer_limit, db))
            .map_err(PublicAgentError::Runtime)?;
        let agent_started = std::time::Instant::now();
        let agent_state = state.unwrap_or_else(|| stopped_unloaded_agent(agent_id));
        let (work_queue, scheduling_material) = acquisition
            .read(|| {
                self.cold_work_queue_with_observed_scheduling(
                    &identity,
                    &storage,
                    &agent_state,
                    &external_triggers,
                    scheduling_material,
                )
            })
            .map_err(PublicAgentError::Runtime)?;
        let scheduling_posture = acquisition
            .read(|| storage.agent_posture_projection_with_work_queue(&agent_state, &work_queue))
            .map_err(PublicAgentError::Runtime)?;
        let (closure_projection, scheduling_material) = acquisition
            .read(|| {
                self.cold_scheduler_with_observed_scheduling(
                    &identity,
                    &storage,
                    &agent_state,
                    std::borrow::Cow::Borrowed(&work_queue),
                    scheduling_material,
                )
            })
            .map_err(PublicAgentError::Runtime)?;
        let closure = RuntimeHandle::closure_decision_from_projection(
            &agent_state,
            &closure_projection,
            None,
            &acquisition,
        )
        .map_err(PublicAgentError::Runtime)?;
        let tasks_started = std::time::Instant::now();
        let (waits, active_task_count, tasks) = acquisition
            .read(|| {
                let waits = storage
                    .raw_unresolved_wait_conditions_for_agent(agent_id)?
                    .into_iter()
                    .take(50)
                    .collect();
                let mut tasks = storage.latest_active_task_records(usize::MAX)?;
                let active_task_count = tasks.len();
                tasks.truncate(task_limit);
                Ok((waits, active_task_count, tasks))
            })
            .map_err(PublicAgentError::Runtime)?;
        crate::diagnostics::record_projection_state_tasks(tasks_started.elapsed());
        // Parent facts are frozen. Release before another agent's summary await.
        drop(reservations);
        let active_children = self
            .child_agent_summaries_scoped(agent_id, db, handle)
            .map_err(PublicAgentError::Runtime)?;
        let agent = LightweightAgentStateProjection {
            identity: AgentIdentityView::from_owned_record(
                identity,
                &self.config().default_agent_id,
            ),
            active_task_count,
            lifecycle: AgentLifecycleHint::from_status(agent_id, agent_state.status.clone()),
            model: crate::runtime::agent_model_state_for_catalog(
                &RuntimeModelCatalog::from_config(&self.config()),
                &self.runtime_context_config(),
                &agent_state,
            ),
            agent: agent_state,
            scheduling_posture,
            closure,
            active_children,
            work_queue,
            _work_budget: None,
            _scheduling_material: None,
        };
        crate::diagnostics::record_projection_state_agent(agent_started.elapsed());
        let mut material = crate::runtime::StateProjectionMaterial {
            acquisition,
            _work: None,
            _tasks: None,
            _scheduling: Some(scheduling_material),
            _children: Vec::new(),
        };
        material
            .retain_children(&agent.active_children)
            .map_err(PublicAgentError::Runtime)?;
        db.check_read_scope().map_err(PublicAgentError::Runtime)?;
        Ok(AgentStateReadProjection {
            waits,
            source: AgentStateProjectionSource::Storage,
            agent,
            tasks,
            timers,
            external_triggers,
            material,
        })
    }

    fn child_agent_summaries_scoped(
        &self,
        parent_agent_id: &str,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<Vec<ChildAgentSummary>> {
        db.check_read_scope()?;
        let mut children = Vec::new();
        for (identity, material) in self
            .inner
            .registry
            .child_identity_reads(parent_agent_id, &self.inner.input_evidence_budget)?
        {
            db.check_read_cancellation()?;
            if let Some(runtime) =
                handle.block_on(self.try_get_loaded_runtime_scoped(&identity.agent_id, db))?
            {
                runtime.prepare_operator_current_scoped(db, handle)?;
                let mut read = runtime.child_agent_summary_observation_scoped(db, handle)?;
                read.observability._source_material = Some(material.guard()?);
                read.acquisition.read(|| {
                    crate::diagnostics::resource_io::admit_cloned_value(
                        &read.state.current_run_id,
                    )?;
                    crate::runtime_db::repositories::reserve_source_vec_slot(&mut children)?;
                    children.push(ChildAgentSummary {
                        identity: AgentIdentityView::from_owned_record(
                            identity,
                            &self.config().default_agent_id,
                        ),
                        status: read.state.status.clone(),
                        current_run_id: read.state.current_run_id.clone(),
                        pending: read.state.pending,
                        active_task_count: read.active_task_count,
                        observability: read.observability,
                    });
                    Ok(())
                })?;
                continue;
            }
            let cold = handle.block_on(self.acquire_cold_summary_in(&identity, db))?;
            let active_task_count = cold.projection().active_tasks.len();
            let mut observability = cold.observability()?;
            observability._source_material = Some(material.guard()?);
            cold.acquisition.read(|| {
                crate::runtime_db::repositories::reserve_source_vec_slot(&mut children)?;
                children.push(ChildAgentSummary {
                    identity: AgentIdentityView::from_owned_record(
                        cold.identity,
                        &self.config().default_agent_id,
                    ),
                    status: cold.state.status,
                    current_run_id: cold.state.current_run_id,
                    pending: cold.state.pending,
                    active_task_count,
                    observability,
                });
                Ok(())
            })?;
        }
        children.sort_by(|left, right| left.identity.agent_id.cmp(&right.identity.agent_id));
        db.check_read_scope()?;
        Ok(children)
    }
}

impl RuntimeHostBridge {
    pub(crate) fn child_summaries_scoped(
        &self,
        parent_agent_id: &str,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<Vec<ChildAgentSummary>> {
        self.host()?
            .child_agent_summaries_scoped(parent_agent_id, db, handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::host_backed_test_runtime;
    use crate::runtime_db::RuntimeDbReadScope;

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_unloaded_state_reads_shared_storage_without_starting_target() {
        let (_home, host, _runtime) = host_backed_test_runtime().await;
        let identity = AgentIdentityRecord::new(
            "scoped-unloaded-agent",
            AgentKind::Named,
            AgentVisibility::Public,
            crate::types::AgentOwnership::SelfOwned,
            crate::types::AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&identity).unwrap();
        let agent_id = identity.agent_id;
        let mut agent = AgentState::new(&agent_id);
        agent.status = AgentStatus::Asleep;
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&agent)
            .unwrap();
        let db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
            std::time::Instant::now() + Duration::from_secs(30),
        ));
        let reader = host.clone();
        let read_id = agent_id.clone();
        let handle = Handle::current();
        let projection = tokio::task::spawn_blocking(move || {
            reader.operator_agent_state_projection_scoped(&read_id, 10, 10, &db, &handle)
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(projection.source, AgentStateProjectionSource::Storage);
        assert_eq!(projection.agent.agent.status, AgentStatus::Asleep);
        assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());
        assert!(!host.agent_data_dir(&agent_id).exists());
        let ordinary = host
            .operator_agent_state_projection(&agent_id, 10, 10)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&projection.agent.agent).unwrap(),
            serde_json::to_value(&ordinary.agent.agent).unwrap()
        );
        host.shutdown().await.unwrap();
    }
}
