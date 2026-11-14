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
            return runtime
                .acquire_operator_state_read_projection_scoped(task_limit, timer_limit, db, handle)
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
            observation,
        } = handle
            .block_on(self.acquire_cold_scheduling_query_in(&identity, timer_limit, db, true))
            .map_err(PublicAgentError::Runtime)?;
        let agent_started = std::time::Instant::now();
        let agent_state = state.unwrap_or_else(|| stopped_unloaded_agent(agent_id));
        let (work_queue, scheduling_material) = acquisition
            .read(|| {
                self.cold_state_work_queue_with_observed_scheduling(
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
                self.cold_scheduler_with_shared_scheduling(
                    &identity,
                    &storage,
                    &agent_state,
                    &work_queue,
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
        if let Some(observation) = observation {
            observation.check().map_err(PublicAgentError::Runtime)?;
        }
        drop(reservations);
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
            work_queue,
            _scheduling_material: None,
        };
        let active_work_item_count = agent.work_queue.items.len();
        let active_timer_count = scheduling_material
            .active_timer_count()
            .map_err(PublicAgentError::Runtime)?;
        crate::diagnostics::record_projection_state_agent(agent_started.elapsed());
        let material = crate::runtime::StateProjectionMaterial {
            acquisition,
            _work: None,
            _tasks: None,
            _scheduling: Some(scheduling_material),
        };
        db.check_read_scope().map_err(PublicAgentError::Runtime)?;
        Ok(AgentStateReadProjection {
            waits,
            source: AgentStateProjectionSource::Storage,
            agent,
            tasks,
            timers,
            active_work_item_count,
            active_timer_count,
            external_triggers,
            material,
        })
    }
}

#[cfg(holon_lib_test_shard = "d")]
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
        let mut work = crate::types::WorkItemRecord::new(
            &agent_id,
            "scoped cold State loan 汉🦀",
            crate::types::WorkItemState::Open,
        );
        work.plan_status = crate::types::WorkItemPlanStatus::Ready;
        assert!(host
            .runtime_db()
            .work_items()
            .insert_new_qualified_for_fixture(&work, identity.incarnation)
            .unwrap());
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
        assert_eq!(projection.agent.work_queue.items.len(), 1);
        assert_eq!(projection.agent.work_queue.items[0].id, work.id);
        assert_eq!(
            serde_json::to_value(&projection.agent.work_queue.items[0].work_item).unwrap(),
            serde_json::to_value(&work).unwrap()
        );
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
        assert_eq!(
            serde_json::to_value((
                &projection.agent.work_queue.items,
                &projection.agent.work_queue.queued_blocked,
                &projection.agent.work_queue.queued_runnable,
            ))
            .unwrap(),
            serde_json::to_value((
                &ordinary.agent.work_queue.items,
                &ordinary.agent.work_queue.queued_blocked,
                &ordinary.agent.work_queue.queued_runnable,
            ))
            .unwrap()
        );
        host.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn cold_observer_rejects_a_completed_metadata_ack_without_a_root() -> Result<()> {
        use crate::runtime::mutation_owner::{
            IdentityMutationFacts, KnownCommit, MutationReservation,
        };
        let (_home, host, _runtime) = host_backed_test_runtime().await;
        let identity = AgentIdentityRecord::new(
            "cold-observer-generation",
            AgentKind::Named,
            AgentVisibility::Public,
            crate::types::AgentOwnership::SelfOwned,
            crate::types::AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&identity)?;
        let observation = crate::host::cold_summary::ColdObserverCut::capture(
            &host,
            &identity,
            host.runtime_db(),
        )?;
        observation.check()?;
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&identity.agent_id, identity.incarnation)?;
        assert!(owner.capture()?.is_none());
        let mut reservations = vec![owner.reserve().await?];
        MutationReservation::begin_all(&mut reservations, "cold-metadata-ack".into())?;
        MutationReservation::committed_all(
            &mut reservations,
            vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                identity: Some(Arc::new(identity)),
                state: None,
            }))],
        )?;
        MutationReservation::publish_all_known(&mut reservations)?;
        assert!(owner.capture()?.is_none());
        let error = observation.check().unwrap_err();
        assert!(error
            .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
            .is_some());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepared_cold_summary_uses_readers_while_execution_owner_and_writer_are_held(
    ) -> Result<()> {
        let (_home, host, _runtime) = host_backed_test_runtime().await;
        let identity = AgentIdentityRecord::new(
            "cold-observer-reader",
            AgentKind::Named,
            AgentVisibility::Public,
            crate::types::AgentOwnership::SelfOwned,
            crate::types::AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&identity)?;
        let state = AgentState::new(&identity.agent_id);
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&state)?;
        // Only the first accepted query may initialize missing qualification.
        drop(host.acquire_cold_summary(&identity).await?);
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&identity.agent_id, identity.incarnation)?;
        let reservation = owner.reserve().await?;
        let (start, ready) = tokio::sync::oneshot::channel();
        let mut start = Some(start);
        let (release, resume) = std::sync::mpsc::channel();
        let db = host.runtime_db().clone();
        let writer = tokio::task::spawn_blocking(move || {
            db.transaction(|_| {
                if let Some(start) = start.take() {
                    let _ = start.send(());
                }
                resume.recv()?;
                Ok(())
            })
        });
        ready.await?;
        let summary =
            tokio::time::timeout(Duration::from_secs(5), host.acquire_cold_summary(&identity))
                .await??;
        assert_eq!(summary.state.id, identity.agent_id);
        assert!(host
            .try_get_loaded_runtime(&identity.agent_id)
            .await
            .is_none());
        release.send(())?;
        writer.await??;
        drop(reservation);
        Ok(())
    }
}
