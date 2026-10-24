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
        let identity = self.active_agent_identity(agent_id)?;
        let read = || -> Result<AgentStateReadProjection> {
            let runtime = handle.block_on(self.try_get_loaded_runtime_scoped(agent_id, db))?;
            let storage = self.agent_storage_read_only_scoped(agent_id, db)?;
            let waits = storage
                .raw_unresolved_wait_conditions_for_agent(agent_id)?
                .into_iter()
                .take(50)
                .collect();
            let agent_started = std::time::Instant::now();
            let (source, agent, tasks, timers, external_triggers) = if let Some(runtime) = runtime {
                crate::diagnostics::record_projection_state_source_loaded();
                let agent = runtime.lightweight_agent_state_projection_scoped(db, handle)?;
                crate::diagnostics::record_projection_state_agent(agent_started.elapsed());
                let started = std::time::Instant::now();
                let tasks = runtime.active_tasks_scoped(task_limit, db, handle)?;
                crate::diagnostics::record_projection_state_tasks(started.elapsed());
                let started = std::time::Instant::now();
                let timers = runtime.recent_timers_scoped(timer_limit, db, handle)?;
                crate::diagnostics::record_projection_state_timers(started.elapsed());
                let started = std::time::Instant::now();
                let triggers = runtime.latest_external_triggers_scoped(db, handle)?;
                crate::diagnostics::record_projection_state_external_triggers(started.elapsed());
                (
                    AgentStateProjectionSource::Loaded,
                    agent,
                    tasks,
                    timers,
                    triggers,
                )
            } else {
                crate::diagnostics::record_projection_state_source_storage();
                crate::diagnostics::record_projection_state_runtime_spawn_avoided();
                let state = storage
                    .read_agent()?
                    .unwrap_or_else(|| stopped_unloaded_agent(agent_id));
                let work_queue = storage.work_queue_read_model()?;
                let scheduling_posture =
                    storage.agent_posture_projection_with_work_queue(&state, &work_queue)?;
                let closure = RuntimeHandle::closure_decision_from_storage(&storage, &state)?;
                let active_children = self.child_agent_summaries_scoped(agent_id, db, handle)?;
                let agent = LightweightAgentStateProjection {
                    identity: AgentIdentityView::from_record(
                        &identity,
                        &self.config().default_agent_id,
                    ),
                    active_task_count: storage.active_task_count_for_agent(agent_id)?,
                    lifecycle: AgentLifecycleHint::from_status(agent_id, state.status.clone()),
                    model: crate::runtime::agent_model_state_for_catalog(
                        &RuntimeModelCatalog::from_config(&self.config()),
                        &self.runtime_context_config(),
                        &state,
                    ),
                    agent: state,
                    scheduling_posture,
                    closure,
                    active_children,
                    work_queue,
                };
                crate::diagnostics::record_projection_state_agent(agent_started.elapsed());
                let started = std::time::Instant::now();
                let tasks = storage.latest_active_task_records(task_limit)?;
                crate::diagnostics::record_projection_state_tasks(started.elapsed());
                let started = std::time::Instant::now();
                let timers = storage.read_recent_timers(timer_limit)?;
                crate::diagnostics::record_projection_state_timers(started.elapsed());
                let started = std::time::Instant::now();
                let triggers = storage.latest_external_triggers()?;
                crate::diagnostics::record_projection_state_external_triggers(started.elapsed());
                (
                    AgentStateProjectionSource::Storage,
                    agent,
                    tasks,
                    timers,
                    triggers,
                )
            };
            tracing::debug!(
                agent_id,
                state_projection_source = source.as_str(),
                "building public agent state projection"
            );
            db.check_read_scope()?;
            Ok(AgentStateReadProjection {
                waits,
                source,
                agent,
                tasks,
                timers,
                external_triggers,
            })
        };
        read().map_err(PublicAgentError::Runtime)
    }

    fn child_agent_summaries_scoped(
        &self,
        parent_agent_id: &str,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<Vec<ChildAgentSummary>> {
        db.check_read_scope()?;
        let mut children = Vec::new();
        for identity in self.agent_identity_records()?.into_iter().filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.kind == AgentKind::Child
                && record.parent_agent_id.as_deref() == Some(parent_agent_id)
        }) {
            db.check_read_cancellation()?;
            let storage = self.agent_storage_read_only_scoped(&identity.agent_id, db)?;
            let state = storage
                .read_agent()?
                .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
            let active_task_count = storage.active_task_count_for_agent(&identity.agent_id)?;
            let observability = match handle
                .block_on(self.try_get_loaded_runtime_scoped(&identity.agent_id, db))?
            {
                Some(runtime) => runtime.child_agent_observability_scoped(db, handle)?,
                None => RuntimeHandle::child_agent_observability_from_storage(&storage, &state)?,
            };
            children.push(ChildAgentSummary {
                identity: AgentIdentityView::from_record(
                    &identity,
                    &self.config().default_agent_id,
                ),
                status: state.status.clone(),
                current_run_id: state.current_run_id.clone(),
                pending: state.pending,
                active_task_count,
                observability,
            });
        }
        children.sort_by(|left, right| left.identity.agent_id.cmp(&right.identity.agent_id));
        db.check_read_scope()?;
        Ok(children)
    }
}

impl RuntimeHostBridge {
    pub(crate) fn identity_for_agent_scoped(
        &self,
        agent_id: &str,
        db: &RuntimeDb,
    ) -> Result<Option<AgentIdentityRecord>> {
        db.check_read_scope()?;
        let identity = self.host()?.agent_identity_record(agent_id)?;
        db.check_read_scope()?;
        Ok(identity)
    }

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
        host.runtime_db().agent_states().upsert(&agent).unwrap();
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
