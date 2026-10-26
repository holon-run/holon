//! Synchronous reads owned by the caller's registered blocking worker.

use super::*;
use crate::diagnostics::attribution;
use crate::runtime_db::RuntimeDb;
use crate::types::{ChildAgentObservabilitySnapshot, ExternalTriggerRecord, TimerRecord};
use tokio::runtime::Handle;

impl RuntimeHandle {
    pub(crate) fn agent_state_scoped(&self, db: &RuntimeDb, handle: &Handle) -> Result<AgentState> {
        let lock_wait = attribution::AGENT_LOCK.start();
        let guard = handle.block_on(db.wait_for_memory_lock(self.inner.agent.lock()))?;
        drop(lock_wait);
        let clone_timer = attribution::AGENT_CLONE.start();
        let agent = guard.state.clone();
        drop(guard);
        drop(clone_timer);
        db.check_read_scope()?;
        Ok(agent)
    }

    pub(crate) fn lightweight_agent_state_projection_scoped(
        &self,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<LightweightAgentStateProjection> {
        let storage = self.inner.storage.read_only_with_db(db.clone())?;
        let agent = self.agent_state_scoped(db, handle)?;
        let work_queue = storage.work_queue_read_model()?;
        let posture_timer = attribution::POSTURE.start();
        let scheduling_posture =
            storage.agent_posture_projection_with_work_queue(&agent, &work_queue)?;
        drop(posture_timer);
        let closure_timer = attribution::CLOSURE.start();
        let closure = Self::closure_decision_with_storage_at(
            &storage,
            &agent,
            None,
            work_queue.clone(),
            self.now(),
        )?;
        drop(closure_timer);
        let children_timer = attribution::CHILDREN.start();
        let active_children = match &self.inner.host_bridge {
            Some(bridge) => bridge.child_summaries_scoped(&agent.id, db, handle)?,
            None => Vec::new(),
        };
        drop(children_timer);
        let identity_timer = attribution::IDENTITY.start();
        let identity = match &self.inner.host_bridge {
            Some(bridge) => bridge
                .identity_for_agent_scoped(&agent.id, db)?
                .map(|identity| {
                    AgentIdentityView::from_record(&identity, &self.inner.default_agent_id)
                })
                .unwrap_or_else(|| self.fallback_identity_view(&agent.id)),
            None => self.fallback_identity_view(&agent.id),
        };
        drop(identity_timer);
        let tasks_timer = attribution::ACTIVE_TASKS.start();
        let active_task_count = storage.active_task_count_for_agent(&agent.id)?;
        drop(tasks_timer);
        db.check_read_scope()?;
        Ok(LightweightAgentStateProjection {
            identity,
            active_task_count,
            lifecycle: crate::types::AgentLifecycleHint::from_status(
                &agent.id,
                agent.status.clone(),
            ),
            model: self.model_state_for(&agent),
            agent,
            scheduling_posture,
            closure,
            active_children,
            work_queue,
        })
    }

    pub(crate) fn active_tasks_scoped(
        &self,
        limit: usize,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<Vec<TaskRecord>> {
        crate::diagnostics::record_runtime_projection_cache_read();
        let cache = handle.block_on(db.wait_for_memory_lock(self.inner.projection_cache.lock()))?;
        let tasks = cache.active_tasks(limit);
        drop(cache);
        db.check_read_scope()?;
        Ok(tasks)
    }

    pub(crate) fn recent_timers_scoped(
        &self,
        limit: usize,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<Vec<TimerRecord>> {
        crate::diagnostics::record_runtime_projection_cache_read();
        let cache = handle.block_on(db.wait_for_memory_lock(self.inner.projection_cache.lock()))?;
        let timers = cache.recent_timers(limit);
        drop(cache);
        db.check_read_scope()?;
        Ok(timers)
    }

    pub(crate) fn latest_external_triggers_scoped(
        &self,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<Vec<ExternalTriggerRecord>> {
        crate::diagnostics::record_runtime_projection_cache_read();
        let cached = {
            let cache =
                handle.block_on(db.wait_for_memory_lock(self.inner.projection_cache.lock()))?;
            cache.latest_external_triggers()
        };
        db.check_read_scope()?;
        if !cached.is_empty() {
            return Ok(cached);
        }
        let agent_id = self.agent_state_scoped(db, handle)?.id;
        let mut records = db.external_triggers().latest_for_agent(&agent_id)?;
        if !records.is_empty() {
            let mut cache =
                handle.block_on(db.wait_for_memory_lock(self.inner.projection_cache.lock()))?;
            for record in &records {
                cache.upsert_external_trigger(record.clone());
            }
        }
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        db.check_read_scope()?;
        Ok(records)
    }

    pub(crate) fn child_agent_observability_scoped(
        &self,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<ChildAgentObservabilitySnapshot> {
        let storage = self.inner.storage.read_only_with_db(db.clone())?;
        let agent = self.agent_state_scoped(db, handle)?;
        let closure_timer = attribution::CHILD_CLOSURE.start();
        let work_queue = storage.work_queue_prompt_projection()?;
        let closure =
            Self::closure_decision_with_storage_at(&storage, &agent, None, work_queue, self.now())?;
        drop(closure_timer);
        let tasks_timer = attribution::CHILD_TASK_READ.start();
        let tasks = db.tasks().active_for_agent(&agent.id, usize::MAX)?;
        drop(tasks_timer);
        let briefs_timer = attribution::CHILD_BRIEFS.start();
        let briefs = storage.read_recent_briefs(16)?;
        drop(briefs_timer);
        let assemble_timer = attribution::CHILD_ASSEMBLE.start();
        let result = lifecycle::build_child_agent_observability(
            &agent,
            closure.waiting_reason,
            &tasks,
            &briefs,
        );
        drop(assemble_timer);
        db.check_read_scope()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::AgentStateReadProjection;
    use crate::runtime::tests::support::host_backed_test_runtime;
    use crate::runtime_db::{RuntimeDbReadScope, RuntimeDbRetryableError};
    use std::time::{Duration, Instant};

    fn projection_value(projection: &AgentStateReadProjection) -> serde_json::Value {
        let agent = &projection.agent;
        serde_json::json!({
            "source": projection.source.as_str(),
            "agent": agent.agent,
            "identity": agent.identity,
            "posture": agent.scheduling_posture,
            "count": agent.active_task_count,
            "lifecycle": agent.lifecycle,
            "model": agent.model,
            "closure": agent.closure,
            "children": agent.active_children,
            "queue": {
                "current": agent.work_queue.current,
                "queued_blocked": agent.work_queue.queued_blocked,
                "items": agent.work_queue.items,
                "current_runnable": agent.work_queue.current_runnable,
                "triggered_blocked": agent.work_queue.triggered_blocked,
                "queued_runnable": agent.work_queue.queued_runnable,
                "yielded": agent.work_queue.yielded,
                "waiting_for_operator": agent.work_queue.waiting_for_operator,
                "blocked": agent.work_queue.blocked,
                "completing": agent.work_queue.completing,
                "completed_recent": agent.work_queue.completed_recent,
            },
            "tasks": projection.tasks,
            "timers": projection.timers,
            "triggers": projection.external_triggers,
            "waits": projection.waits,
        })
    }

    async fn parent_projection(
        host: &crate::host::RuntimeHost,
        agent_id: &str,
    ) -> Result<AgentStateReadProjection> {
        let db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
            Instant::now() + Duration::from_secs(30),
        ));
        let host = host.clone();
        let agent_id = agent_id.to_owned();
        let handle = Handle::current();
        tokio::task::spawn_blocking(move || {
            host.operator_agent_state_projection_scoped(&agent_id, 10, 10, &db, &handle)
        })
        .await?
        .map_err(Into::into)
    }

    fn child_identity(id: &str, parent: &str) -> crate::types::AgentIdentityRecord {
        crate::types::AgentIdentityRecord::new(
            id,
            crate::types::AgentKind::Child,
            crate::types::AgentVisibility::Private,
            crate::types::AgentOwnership::ParentSupervised,
            crate::types::AgentProfilePreset::PrivateChild,
            Some(parent.to_owned()),
            None,
        )
    }

    fn child_task(id: &str, owner: &str, status: &str, background: bool) -> TaskRecord {
        serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": owner, "kind": "command_task", "status": status,
            "created_at": Utc::now(), "updated_at": Utc::now(),
            "detail": if background {
                serde_json::json!({"wait_policy": "background"})
            } else {
                serde_json::json!({"wait_policy": "blocking"})
            },
        }))
        .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_parent_child_summary_does_not_decode_out_of_scope_task_payloads() {
        let (_home, host, parent) = host_backed_test_runtime().await;
        let parent_id = parent.agent_id().await.unwrap();
        let identity = child_identity("payload-isolation-child", &parent_id);
        host.append_agent_identity(&identity).unwrap();
        let _child = host.get_or_create_agent(&identity.agent_id).await.unwrap();

        // Corrupt only synthetic rows that the child's active-owner query must
        // never decode. This fails through the parent projection on latest_all.
        for (owner, status) in [
            (identity.agent_id.as_str(), "completed"),
            (identity.agent_id.as_str(), "failed"),
            (identity.agent_id.as_str(), "cancelled"),
            (identity.agent_id.as_str(), "interrupted"),
            ("unrelated-owner", "running"),
        ] {
            let id = format!("{owner}-{status}");
            host.runtime_db()
                .tasks()
                .upsert(&child_task(&id, owner, status, false))
                .unwrap();
            host.runtime_db()
                .connection()
                .unwrap()
                .execute(
                    "UPDATE tasks SET payload_json = '{}' WHERE task_id = ?1",
                    [&id],
                )
                .unwrap();
        }
        let projection = parent_projection(&host, &parent_id).await;
        assert!(
            projection.is_ok(),
            "child summary decoded unrelated or terminal history: {:?}",
            projection.err()
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_parent_child_summary_preserves_active_owner_and_nonblocking_task_semantics() {
        let (_home, host, parent) = host_backed_test_runtime().await;
        let parent_id = parent.agent_id().await.unwrap();
        let mut children = Vec::new();
        for (id, loaded) in [
            ("active-loaded-child", true),
            ("active-unloaded-child", false),
        ] {
            let identity = child_identity(id, &parent_id);
            host.append_agent_identity(&identity).unwrap();
            let mut state = AgentState::new(id);
            state.status = AgentStatus::AwakeIdle;
            state.turn_index = 1;
            state.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
                turn_id: format!("{id}-completed-turn"),
                turn_index: 1,
                kind: crate::types::TurnTerminalKind::Completed,
                reason: None,
                last_assistant_message: None,
                no_brief_reason: None,
                checkpoint: None,
                completed_at: Utc::now(),
                duration_ms: 1,
            });
            host.runtime_db().agent_states().upsert(&state).unwrap();
            if loaded {
                let runtime = host.get_or_create_agent(id).await.unwrap();
                runtime.inner.agent.lock().await.state = state;
                runtime
                    .inner
                    .projection_cache
                    .lock()
                    .await
                    .upsert_task(child_task("cache-only-child-task", id, "cancelling", false));
            }
            for index in 0..17 {
                let brief: crate::types::BriefRecord = serde_json::from_value(serde_json::json!({
                    "id": format!("{id}-brief-{index}"), "agent_id": id,
                    "kind": if index % 2 == 0 { "ack" } else { "failure" },
                    "text": format!("brief-{index}"),
                    "created_at": Utc::now() + chrono::Duration::milliseconds(index),
                }))
                .unwrap();
                host.runtime_db().evidence().append_brief(&brief).unwrap();
            }
            // Legacy blocking flags must not override current background-only
            // semantics. This oldest task also guards against history limits.
            host.runtime_db()
                .tasks()
                .upsert(&child_task(&format!("{id}-blocker"), id, "queued", false))
                .unwrap();
            for index in 0..128 {
                host.runtime_db()
                    .tasks()
                    .upsert(&child_task(
                        &format!("{id}-background-{index}"),
                        id,
                        ["queued", "running", "cancelling"][index % 3],
                        true,
                    ))
                    .unwrap();
            }
            let mut foreign = child_task(
                &format!("{id}-foreign-link"),
                &parent_id,
                "cancelling",
                false,
            );
            foreign.detail = Some(serde_json::json!({"child_agent_id": id}));
            host.runtime_db().tasks().upsert(&foreign).unwrap();
            children.push(id);
        }
        for (status, active) in [
            ("queued", true),
            ("running", true),
            ("cancelling", true),
            ("completed", false),
            ("failed", false),
            ("cancelled", false),
            ("interrupted", false),
        ] {
            for id in &children {
                host.runtime_db()
                    .tasks()
                    .upsert(&child_task(
                        &format!("{id}-status-{status}"),
                        id,
                        status,
                        false,
                    ))
                    .unwrap();
            }
            let projection = parent_projection(&host, &parent_id).await.unwrap();
            assert_eq!(projection.agent.active_children.len(), 2);
            for child in &projection.agent.active_children {
                assert_eq!(child.observability.blocked_reason, None, "{status}");
                assert_eq!(
                    child.observability.phase,
                    crate::types::ChildAgentPhase::Waiting
                );
                assert_eq!(child.active_task_count, 129 + usize::from(active));
                assert_eq!(
                    child.observability.last_progress_brief.as_deref(),
                    Some("brief-16")
                );
                assert_eq!(
                    child.observability.last_result_brief.as_deref(),
                    Some("brief-15")
                );
            }
            let loaded = host.get_or_create_agent(children[0]).await.unwrap();
            let ordinary = loaded.child_agent_observability().await.unwrap();
            assert_eq!(projection.agent.active_children[0].observability, ordinary);
            assert!(host.try_get_loaded_runtime(children[1]).await.is_none());
            if active {
                for id in &children {
                    let mut task = host
                        .runtime_db()
                        .tasks()
                        .latest(&format!("{id}-status-{status}"))
                        .unwrap()
                        .unwrap();
                    task.status = child_task("status", id, "completed", false).status;
                    host.runtime_db().tasks().upsert(&task).unwrap();
                }
            }
        }
        // One old active task behind 128 newer terminal records must still
        // keep a completed child nonterminal; do not read latest N then filter.
        for id in &children {
            for index in 0..128 {
                host.runtime_db()
                    .tasks()
                    .upsert(&child_task(
                        &format!("{id}-background-{index}"),
                        id,
                        "completed",
                        true,
                    ))
                    .unwrap();
            }
        }
        for status in ["queued", "completed"] {
            for id in &children {
                let mut task = host
                    .runtime_db()
                    .tasks()
                    .latest(&format!("{id}-blocker"))
                    .unwrap()
                    .unwrap();
                task.status = child_task("status", id, status, false).status;
                host.runtime_db().tasks().upsert(&task).unwrap();
            }
            let projection = parent_projection(&host, &parent_id).await.unwrap();
            for child in projection.agent.active_children {
                assert_eq!(child.active_task_count, usize::from(status == "queued"));
                assert_eq!(
                    child.observability.phase,
                    if status == "queued" {
                        crate::types::ChildAgentPhase::Waiting
                    } else {
                        crate::types::ChildAgentPhase::Terminal
                    }
                );
            }
        }
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_loaded_projection_preserves_memory_cache_children_and_trigger_miss() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        let agent_id = runtime.agent_id().await.unwrap();
        runtime.inner.agent.lock().await.state.pending = 123;
        let task: TaskRecord = serde_json::from_value(serde_json::json!({
            "id": "cache-only-task", "agent_id": agent_id,
            "kind": "command_task", "status": "running",
            "created_at": Utc::now(), "updated_at": Utc::now(),
        }))
        .unwrap();
        let timer: TimerRecord = serde_json::from_value(serde_json::json!({
            "id": "cache-only-timer", "agent_id": agent_id,
            "created_at": Utc::now(), "duration_ms": 1000, "repeat": false,
        }))
        .unwrap();
        {
            let mut cache = runtime.inner.projection_cache.lock().await;
            cache.upsert_task(task);
            cache.upsert_timer(timer);
            cache.external_triggers.clear();
        }
        let trigger: ExternalTriggerRecord = serde_json::from_value(serde_json::json!({
            "external_trigger_id": "scoped-trigger", "target_agent_id": agent_id,
            "scope": "agent", "delivery_mode": "wake_hint", "token_hash": "test-only",
            "status": "active", "created_at": Utc::now(), "delivery_count": 0,
        }))
        .unwrap();
        runtime
            .inner
            .storage
            .append_external_trigger(&trigger)
            .unwrap();
        let child = crate::types::AgentIdentityRecord::new(
            "scoped-loaded-child",
            crate::types::AgentKind::Child,
            crate::types::AgentVisibility::Private,
            crate::types::AgentOwnership::ParentSupervised,
            crate::types::AgentProfilePreset::PrivateChild,
            Some(agent_id.clone()),
            Some("scoped-child-task".into()),
        );
        host.append_agent_identity(&child).unwrap();
        let child = host.get_or_create_agent(&child.agent_id).await.unwrap();
        child.inner.agent.lock().await.state.current_work_item_id =
            Some("memory-only-child-work".into());
        child
            .inner
            .agent
            .lock()
            .await
            .state
            .working_memory
            .current_working_memory
            .current_work_item_id = Some("memory-only-child-work".into());

        let scoped_db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
            Instant::now() + Duration::from_secs(30),
        ));
        let read_host = host.clone();
        let read_id = agent_id.clone();
        let handle = Handle::current();
        let scoped = tokio::task::spawn_blocking(move || {
            read_host.operator_agent_state_projection_scoped(&read_id, 10, 10, &scoped_db, &handle)
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(scoped.agent.agent.pending, 123);
        assert_eq!(scoped.tasks[0].id, "cache-only-task");
        assert_eq!(scoped.timers[0].id, "cache-only-timer");
        assert_eq!(scoped.external_triggers, vec![trigger]);
        assert_eq!(
            scoped.agent.active_children[0]
                .observability
                .current_work_item_id
                .as_deref(),
            Some("memory-only-child-work"),
        );
        let ordinary = host
            .operator_agent_state_projection(&agent_id, 10, 10)
            .await
            .unwrap();
        assert_eq!(projection_value(&scoped), projection_value(&ordinary));
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_loaded_memory_locks_cancel_or_expire_and_allow_healthy_retry() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        for operation in 0..4 {
            for expired in [false, true] {
                let agent_guard = if operation == 0 {
                    Some(runtime.inner.agent.lock().await)
                } else {
                    None
                };
                let cache_guard = if operation != 0 {
                    Some(runtime.inner.projection_cache.lock().await)
                } else {
                    None
                };
                let scope = RuntimeDbReadScope::new(
                    Instant::now()
                        + if expired {
                            Duration::from_millis(50)
                        } else {
                            Duration::from_secs(30)
                        },
                );
                let db = host.runtime_db().with_read_scope(scope.clone());
                let reader = runtime.clone();
                let handle = Handle::current();
                let (started, running) = tokio::sync::oneshot::channel();
                let worker = tokio::task::spawn_blocking(move || {
                    let _ = started.send(());
                    match operation {
                        0 => reader.agent_state_scoped(&db, &handle).map(|_| ()),
                        1 => reader.active_tasks_scoped(10, &db, &handle).map(|_| ()),
                        2 => reader.recent_timers_scoped(10, &db, &handle).map(|_| ()),
                        _ => reader
                            .latest_external_triggers_scoped(&db, &handle)
                            .map(|_| ()),
                    }
                });
                running.await.unwrap();
                if !expired {
                    scope.cancel();
                }
                let error = tokio::time::timeout(Duration::from_secs(5), worker)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err();
                assert!(error.is::<RuntimeDbRetryableError>(), "{error:#}");
                drop(cache_guard);
                drop(agent_guard);
            }
        }
        let db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
            Instant::now() + Duration::from_secs(30),
        ));
        let reader = runtime.clone();
        let handle = Handle::current();
        tokio::task::spawn_blocking(move || {
            reader.agent_state_scoped(&db, &handle)?;
            reader.active_tasks_scoped(10, &db, &handle)?;
            reader.recent_timers_scoped(10, &db, &handle)?;
            reader.latest_external_triggers_scoped(&db, &handle)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .unwrap()
        .unwrap();
        host.shutdown().await.unwrap();
    }
}
