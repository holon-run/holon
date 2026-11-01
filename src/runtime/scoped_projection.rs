//! Synchronous reads owned by the caller's registered blocking worker.

use super::*;
use crate::diagnostics::attribution;
use crate::runtime_db::RuntimeDb;
use tokio::runtime::Handle;

impl RuntimeHandle {
    pub(crate) fn prepare_operator_current_scoped(
        &self,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<()> {
        let needs_preparation = self.with_scoped_ready_root(db, handle, |root| {
            Ok(root
                .current
                .as_ref()
                .and_then(|current| current.query.as_ref())
                .is_none())
        })?;
        if needs_preparation {
            // This is an explicit preparation boundary, not a cancellable
            // future inside with_scoped_ready_root's memory-lock waiter.
            let mut owner = handle.block_on(self.begin_scoped_projection_preparation(db))?;
            if owner
                .capture_published_root()?
                .current
                .as_ref()
                .and_then(|current| current.query.as_ref())
                .is_none()
            {
                handle.block_on(owner.prepare_current_query_scoped(db))?;
            }
        }
        db.check_read_scope()
    }

    fn with_scoped_ready_root<T>(
        &self,
        db: &RuntimeDb,
        handle: &Handle,
        read: impl FnOnce(&resident_state::AgentSnapshot) -> Result<T>,
    ) -> Result<T> {
        let _reservations = handle.block_on(db.wait_for_memory_lock(
            mutation_owner::MutationOwner::reserve_all(vec![self.inner.mutation_owner.clone()]),
        ))??;
        // Pending recovery and preparation debt remain explicit. Never run an
        // unscoped SQL/preparation future through the cancellable lock waiter.
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("scoped read needs a Ready publication")?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "scoped read publication scope differs"
        );
        let result = read(&root)?;
        db.check_read_scope()?;
        Ok(result)
    }

    pub(crate) fn operator_state_read_projection_scoped(
        &self,
        task_limit: usize,
        timer_limit: usize,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<crate::host::AgentStateReadProjection> {
        db.check_read_scope()?;
        anyhow::ensure!(
            timer_limit <= resident_scheduling::SCHEDULING_HISTORY_WINDOW,
            resident_scheduling::SchedulingReadError::NeedsHistory
        );
        let mut projection = self.with_scoped_ready_root(db, handle, |root| {
            self.operator_state_from_root(root, task_limit, timer_limit)
        })?;
        // Release the parent reservation before taking any child owner.
        let children_timer = attribution::CHILDREN.start();
        projection.agent.active_children = match &self.inner.host_bridge {
            Some(bridge) => {
                bridge.child_summaries_scoped(&projection.agent.agent.id, db, handle)?
            }
            None => Vec::new(),
        };
        drop(children_timer);
        projection
            .material
            .retain_children(&projection.agent.active_children)?;
        db.check_read_scope()?;
        Ok(projection)
    }

    pub(crate) fn child_agent_summary_observation_scoped(
        &self,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<lifecycle::ChildAgentSummaryObservation> {
        self.with_scoped_ready_root(db, handle, |root| {
            let read = self.capture_operator_scheduler_from_root(root, self.now())?;
            let material = read.acquisition.clone();
            material.read(|| {
                let active_task_count = task_reads::active_task_count(&read.tasks)?;
                anyhow::ensure!(
                    active_task_count == read.projection.active_tasks.len(),
                    "scoped child Task metadata differs from selected active facts"
                );
                let closure = Self::closure_decision_from_projection(
                    &read.state,
                    &read.projection,
                    None,
                    &read.acquisition,
                )?;
                let metadata_bytes = std::mem::size_of::<Vec<&TaskRecord>>()
                    .checked_add(
                        active_task_count
                            .checked_mul(std::mem::size_of::<&TaskRecord>())
                            .context("scoped child Task reference slots overflow")?,
                    )
                    .context("scoped child Task metadata overflow")?;
                crate::diagnostics::resource_io::charge_owned_metadata(
                    u64::try_from(metadata_bytes)?,
                    active_task_count,
                )?;
                let _references = self.inner.input_evidence_budget.register_owned_bytes(
                    &read.tasks.scope,
                    input_evidence_budget::InputMemoryCategory::Pins,
                    input_evidence_budget::OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: u64::try_from(metadata_bytes)?,
                    },
                )?;
                let active_tasks = read.projection.active_tasks.iter().collect::<Vec<_>>();
                let briefs =
                    read.observability_briefs
                        .iter()
                        .filter_map(|record| match &record.record {
                            resident_input_evidence::PreparedInputEvidence::Brief(brief) => {
                                Some(brief)
                            }
                            _ => None,
                        });
                let observability = lifecycle::build_child_agent_observability_with_active_tasks(
                    &read.state,
                    closure.waiting_reason,
                    &active_tasks,
                    briefs,
                )?;
                Ok(lifecycle::ChildAgentSummaryObservation {
                    state: read.state.clone(),
                    active_task_count,
                    observability,
                    acquisition: material.clone(),
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::AgentStateReadProjection;
    use crate::runtime::tests::support::{host_backed_test_runtime, LifecycleHarness};
    use crate::runtime_db::{RuntimeDbReadScope, RuntimeDbRetryableError};
    use crate::types::{ExternalTriggerRecord, TimerRecord};
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
        if let Some(runtime) = host.try_get_loaded_runtime(agent_id).await {
            drop(runtime.operator_state_read_projection(10, 10).await?);
        }
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

    async fn clear_current_for_scoped_host_read(runtime: &RuntimeHandle) {
        let root = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        reservation
            .begin("scoped-host-unprepared-current".into())
            .unwrap();
        reservation
            .committed_prepared_with_scheduling_domains(
                root.state.clone(),
                root.episodes.clone(),
                root.input_evidence.clone(),
                root.work.clone(),
                root.tasks.clone(),
                root.scheduling.clone(),
            )
            .unwrap();
        let unprepared = reservation.publish(root.state.clone()).unwrap();
        assert!(unprepared.current.is_none());
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

    fn write_cold_task_fixture(host: &crate::host::RuntimeHost, task: &TaskRecord) {
        use crate::runtime::{
            resident_input_evidence::InputEvidenceScope,
            resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
        };
        use crate::runtime_db::task_snapshot::{
            prepare_task_write_tx, register_task_writes_tx, TaskDomainLoadRequest,
        };

        let db = host.runtime_db();
        let owner = db
            .agent_identities()
            .latest(&task.agent_id)
            .unwrap()
            .unwrap();
        let state = db.agent_states().latest(&task.agent_id).unwrap().unwrap();
        let scope = InputEvidenceScope {
            agent_id: task.agent_id.clone(),
            identity_incarnation: owner.incarnation,
        };
        let keys = std::collections::BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        // The writer finalizes complete active metadata, not the scoped reader's
        // one-key selection. Use its existing configured allowance.
        let limits = host.bridge().input_evidence_budget().unwrap().limits();
        let (record_limit, byte_limit) = TaskDomainLoadRequest::bounded_material_limits(
            usize::try_from(limits.record_limit).unwrap(),
            8 * 1024 * 1024,
        );
        let request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: TaskCaptureRequest {
                include_active: false,
                include_unsettled: false,
                terminal_limit: 0,
                recovery_source_ids: Default::default(),
                required_keys: keys.clone(),
            },
            recovery_at: None,
            record_limit,
            byte_limit,
        };
        db.transaction(|tx| {
            let proof = prepare_task_write_tx(tx, &scope, &keys)?;
            crate::runtime_db::repositories::upsert_task_tx(tx, task)?;
            register_task_writes_tx(tx, &proof, &keys, &state, &request)?;
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_parent_child_summary_does_not_decode_out_of_scope_task_payloads() {
        let (_home, host, parent) = host_backed_test_runtime().await;
        let parent_id = parent.agent_id().await.unwrap();
        let identity = child_identity("payload-isolation-child", &parent_id);
        host.append_agent_identity(&identity).unwrap();
        let child = host.get_or_create_agent(&identity.agent_id).await.unwrap();
        drop(child.operator_state_read_projection(10, 10).await.unwrap());

        // A loaded child uses its prepared publication, not synthetic SQL
        // history belonging to terminal tasks or a different owner.
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
    async fn scoped_cold_child_summary_preserves_active_owner_and_nonblocking_task_semantics() {
        let (_home, host, parent) = host_backed_test_runtime().await;
        let parent_id = parent.agent_id().await.unwrap();
        let mut children = Vec::new();
        for id in ["active-unloaded-child", "other-active-unloaded-child"] {
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
            host.runtime_db()
                .agent_states()
                .upsert_for_fixture(&state)
                .unwrap();
            for index in 0..17 {
                let brief: crate::types::BriefRecord = serde_json::from_value(serde_json::json!({
                    "id": format!("{id}-brief-{index}"), "agent_id": id,
                    "kind": if index % 2 == 0 { "ack" } else { "failure" },
                    "text": format!("brief-{index}"),
                    "created_at": Utc::now() + chrono::Duration::milliseconds(index),
                }))
                .unwrap();
                // Cold Summary selects qualified Input facts, not raw history.
                host.runtime_db()
                    .evidence()
                    .append_brief_in_scope(
                        &resident_input_evidence::InputEvidenceScope {
                            agent_id: id.to_owned(),
                            identity_incarnation: identity.incarnation,
                        },
                        &brief,
                        &[],
                    )
                    .unwrap();
            }
            // Legacy blocking flags must not override current background-only
            // semantics. This oldest task also guards against history limits.
            write_cold_task_fixture(
                &host,
                &child_task(&format!("{id}-blocker"), id, "queued", false),
            );
            for index in 0..128 {
                write_cold_task_fixture(
                    &host,
                    &child_task(
                        &format!("{id}-background-{index}"),
                        id,
                        ["queued", "running", "cancelling"][index % 3],
                        true,
                    ),
                );
            }
            let mut foreign = child_task(
                &format!("{id}-foreign-link"),
                &parent_id,
                "cancelling",
                false,
            );
            foreign.detail = Some(serde_json::json!({"child_agent_id": id}));
            write_cold_task_fixture(&host, &foreign);
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
                write_cold_task_fixture(
                    &host,
                    &child_task(&format!("{id}-status-{status}"), id, status, false),
                );
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
            for id in &children {
                assert!(host.try_get_loaded_runtime(id).await.is_none());
            }
            if active {
                for id in &children {
                    let mut task = host
                        .runtime_db()
                        .tasks()
                        .latest(&format!("{id}-status-{status}"))
                        .unwrap()
                        .unwrap();
                    task.status = child_task("status", id, "completed", false).status;
                    write_cold_task_fixture(&host, &task);
                }
            }
        }
        // One old active task behind 128 newer terminal records must still
        // keep a completed child nonterminal; do not read latest N then filter.
        for id in &children {
            for index in 0..128 {
                write_cold_task_fixture(
                    &host,
                    &child_task(&format!("{id}-background-{index}"), id, "completed", true),
                );
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
                write_cold_task_fixture(&host, &task);
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
    async fn scoped_loaded_projection_uses_resident_cut_not_unpublished_memory_or_sql() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        runtime.await_initial_resources().await;
        let refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
        let agent_id = runtime.agent_id().await.unwrap();
        drop(
            runtime
                .operator_state_read_projection(10, 10)
                .await
                .unwrap(),
        );
        runtime.inner.agent.lock().await.state.pending = 123;
        let task: TaskRecord = serde_json::from_value(serde_json::json!({
            "id": "sql-only-task", "agent_id": agent_id,
            "kind": "command_task", "status": "running",
            "created_at": Utc::now(), "updated_at": Utc::now(),
        }))
        .unwrap();
        let timer: TimerRecord = serde_json::from_value(serde_json::json!({
            "id": "sql-only-timer", "agent_id": agent_id,
            "created_at": Utc::now(), "duration_ms": 1000, "repeat": false,
        }))
        .unwrap();
        runtime.inner.storage.append_task(&task).unwrap();
        runtime.inner.storage.append_timer(&timer).unwrap();
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
        child.await_initial_resources().await;
        let child_refresh = child.hold_resource_refresh_for_test().await.unwrap();
        drop(child.operator_state_read_projection(10, 10).await.unwrap());
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

        let root = runtime.inner.mutation_owner.capture().unwrap().unwrap();
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
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture().unwrap().unwrap()
        ));
        assert_ne!(scoped.agent.agent.pending, 123);
        assert!(scoped.tasks.iter().all(|task| task.id != "sql-only-task"));
        assert!(scoped
            .timers
            .iter()
            .all(|timer| timer.id != "sql-only-timer"));
        assert!(scoped
            .external_triggers
            .iter()
            .all(|record| record.external_trigger_id != trigger.external_trigger_id));
        assert_eq!(
            scoped.agent.active_children[0]
                .observability
                .current_work_item_id
                .as_deref(),
            None,
        );
        let ordinary = host
            .operator_agent_state_projection(&agent_id, 10, 10)
            .await
            .unwrap();
        assert_eq!(projection_value(&scoped), projection_value(&ordinary));
        drop((refresh, child_refresh));
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_host_first_loaded_state_prepares_parent_and_child_current() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        runtime.await_initial_resources().await;
        let refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
        let agent_id = runtime.agent_id().await.unwrap();
        let identity = child_identity("scoped-first-read-child", &agent_id);
        host.append_agent_identity(&identity).unwrap();
        let child = host.get_or_create_agent(&identity.agent_id).await.unwrap();
        child.await_initial_resources().await;
        let child_refresh = child.hold_resource_refresh_for_test().await.unwrap();
        // Do not warm either runtime through the ordinary test-only State read.
        clear_current_for_scoped_host_read(&runtime).await;
        clear_current_for_scoped_host_read(&child).await;
        let before = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let child_before = child.inner.mutation_owner.capture().unwrap().unwrap();
        let event_cursors = [&before, &child_before].map(|root| {
            host.runtime_db()
                .transaction(|tx| {
                    crate::runtime_db::current_query::event_cursor(
                        tx,
                        &super::resident_input_evidence::InputEvidenceScope {
                            agent_id: root.state.id.clone(),
                            identity_incarnation: root.token.identity_incarnation,
                        },
                    )
                })
                .unwrap()
        });

        for first_read in [true, false] {
            let db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
                Instant::now() + Duration::from_secs(30),
            ));
            let reader = host.clone();
            let read_id = agent_id.clone();
            let handle = Handle::current();
            let prepared = runtime.inner.mutation_owner.capture().unwrap().unwrap();
            let projection = tokio::task::spawn_blocking(move || {
                reader.operator_agent_state_projection_scoped(&read_id, 10, 10, &db, &handle)
            })
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                projection.source,
                crate::host::AgentStateProjectionSource::Loaded
            );
            assert_eq!(projection.agent.agent.id, agent_id);
            assert!(projection
                .agent
                .active_children
                .iter()
                .any(|summary| summary.identity.agent_id == identity.agent_id));
            let after = runtime.inner.mutation_owner.capture().unwrap().unwrap();
            let child_after = child.inner.mutation_owner.capture().unwrap().unwrap();
            for ((original, current), event_cursor) in
                [(&before, &after), (&child_before, &child_after)]
                    .into_iter()
                    .zip(event_cursors)
            {
                assert_eq!(original.state, current.state);
                assert!(Arc::ptr_eq(
                    original.scheduling.as_ref().unwrap(),
                    current.scheduling.as_ref().unwrap()
                ));
                let query = current
                    .current
                    .as_ref()
                    .and_then(|current| current.query.as_ref())
                    .unwrap();
                assert_eq!(query.scope.agent_id, current.state.id);
                assert_eq!(query.identity.agent_id, current.state.id);
                assert_eq!(query.event_cursor, event_cursor);
                assert_eq!(
                    query.selection,
                    crate::runtime_db::current_query::CurrentSelection {
                        events_after: Some(event_cursor),
                        ..Default::default()
                    },
                );
            }
            if !first_read {
                assert!(Arc::ptr_eq(&prepared, &after));
            }
        }
        drop(child_refresh);
        drop(refresh);
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_host_unprepared_current_owner_wait_cancels_or_expires_before_retry() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        runtime.await_initial_resources().await;
        let refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
        let agent_id = runtime.agent_id().await.unwrap();
        clear_current_for_scoped_host_read(&runtime).await;
        let unprepared = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        for expired in [false, true] {
            let reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
            let scope = RuntimeDbReadScope::new(
                Instant::now()
                    + if expired {
                        Duration::from_millis(50)
                    } else {
                        Duration::from_secs(30)
                    },
            );
            let db = host.runtime_db().with_read_scope(scope.clone());
            let reader = host.clone();
            let read_id = agent_id.clone();
            let handle = Handle::current();
            let (started, running) = tokio::sync::oneshot::channel();
            let worker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                reader
                    .operator_agent_state_projection_scoped(&read_id, 10, 10, &db, &handle)
                    .map(|_| ())
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
            assert!(
                matches!(error, crate::host::PublicAgentError::Runtime(ref error)
                    if error.is::<RuntimeDbRetryableError>()),
                "{error:?}"
            );
            drop(reservation);
            assert!(Arc::ptr_eq(
                &unprepared,
                &runtime.inner.mutation_owner.capture().unwrap().unwrap()
            ));
        }
        let db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
            Instant::now() + Duration::from_secs(30),
        ));
        let reader = host.clone();
        let handle = Handle::current();
        tokio::task::spawn_blocking(move || {
            reader.operator_agent_state_projection_scoped(&agent_id, 10, 10, &db, &handle)
        })
        .await
        .unwrap()
        .unwrap();
        drop(refresh);
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_loaded_reads_reject_unprepared_current_without_preparation() {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
        drop(
            runtime
                .operator_state_read_projection(10, 10)
                .await
                .unwrap(),
        );
        let root = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        reservation
            .begin("scoped-unprepared-current".into())
            .unwrap();
        reservation
            .committed_prepared_with_scheduling_domains(
                root.state.clone(),
                root.episodes.clone(),
                root.input_evidence.clone(),
                root.work.clone(),
                root.tasks.clone(),
                root.scheduling.clone(),
            )
            .unwrap();
        let unprepared = reservation.publish(root.state.clone()).unwrap();
        drop(reservation);
        assert!(unprepared.current.is_none());

        for operation in 0..2 {
            let db = runtime
                .runtime_db()
                .with_read_scope(RuntimeDbReadScope::new(
                    Instant::now() + Duration::from_secs(30),
                ));
            let reader = runtime.clone();
            let handle = Handle::current();
            let error = tokio::task::spawn_blocking(move || match operation {
                0 => reader
                    .operator_state_read_projection_scoped(10, 10, &db, &handle)
                    .map(|_| ()),
                _ => reader
                    .child_agent_summary_observation_scoped(&db, &handle)
                    .map(|_| ()),
            })
            .await
            .unwrap()
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Current scheduler metadata needs preparation"),
                "{error:#}"
            );
            assert!(Arc::ptr_eq(
                &unprepared,
                &runtime.inner.mutation_owner.capture().unwrap().unwrap()
            ));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_loaded_reads_preserve_pending_reconcile_fence() {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
        drop(
            runtime
                .operator_state_read_projection(10, 10)
                .await
                .unwrap(),
        );
        let key = {
            let mut reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
            reservation
                .begin("scoped-pending-reconcile".into())
                .unwrap()
        };

        for operation in 0..2 {
            let db = runtime
                .runtime_db()
                .with_read_scope(RuntimeDbReadScope::new(
                    Instant::now() + Duration::from_secs(30),
                ));
            let reader = runtime.clone();
            let handle = Handle::current();
            let error = tokio::task::spawn_blocking(move || match operation {
                0 => reader
                    .operator_state_read_projection_scoped(10, 10, &db, &handle)
                    .map(|_| ()),
                _ => reader
                    .child_agent_summary_observation_scoped(&db, &handle)
                    .map(|_| ()),
            })
            .await
            .unwrap()
            .unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<mutation_owner::MutationOwnerError>(),
                    Some(mutation_owner::MutationOwnerError::NeedsReconcile(actual))
                        if actual == &key
                ),
                "{error:#}"
            );
            assert!(matches!(
                runtime.inner.mutation_owner.capture(),
                Err(mutation_owner::MutationOwnerError::NeedsReconcile(actual))
                    if actual == key
            ));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_loaded_owner_wait_cancels_or_expires_and_allows_healthy_retry() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        drop(
            runtime
                .operator_state_read_projection(10, 10)
                .await
                .unwrap(),
        );
        for operation in 0..2 {
            for expired in [false, true] {
                let reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
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
                        0 => reader
                            .operator_state_read_projection_scoped(10, 10, &db, &handle)
                            .map(|_| ()),
                        _ => reader
                            .child_agent_summary_observation_scoped(&db, &handle)
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
                drop(reservation);
            }
        }
        let db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
            Instant::now() + Duration::from_secs(30),
        ));
        let reader = runtime.clone();
        let handle = Handle::current();
        tokio::task::spawn_blocking(move || {
            reader.operator_state_read_projection_scoped(10, 10, &db, &handle)?;
            reader.child_agent_summary_observation_scoped(&db, &handle)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .unwrap()
        .unwrap();
        host.shutdown().await.unwrap();
    }
}
