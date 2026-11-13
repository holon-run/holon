//! Synchronous reads owned by the caller's registered blocking worker.

use super::*;
use crate::runtime_db::RuntimeDb;
use tokio::runtime::Handle;

impl RuntimeHandle {
    /// Availability-window errors produced by [`RuntimeHandle::observer_ready_root`].
    pub(super) const OBSERVER_PUBLICATION_OPERATION: &'static str = "observer publication";
    /// The window ends with the owner's next publication; interactive observer
    /// reads wait it out instead of surfacing retryable busy on first attempt.
    /// The bound must surface busy well inside existing caller deadlines:
    /// fleet activity snapshots assert a typed error within 1s, and the
    /// default summary exhaustion path asserts one within 2s.
    const OBSERVER_PUBLICATION_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
    const OBSERVER_PUBLICATION_RETRY_DELAY: std::time::Duration =
        std::time::Duration::from_millis(20);

    /// Observers pin one Ready cut; they never reserve an execution owner.
    #[cfg(test)]
    pub(super) fn capture_observer_root(
        &self,
        db: &RuntimeDb,
    ) -> Result<Arc<resident_state::AgentSnapshot>> {
        let root = self.observer_ready_root(db)?;
        self.prepare_observer_root(root, db)
    }

    pub(super) async fn capture_observer_root_async(
        &self,
        db: &RuntimeDb,
    ) -> Result<Arc<resident_state::AgentSnapshot>> {
        let root = self.observer_ready_root_bounded(db).await?;
        if !Self::observer_needs_preparation(&root) {
            return Ok(root);
        }
        let runtime = self.clone();
        let db = db.clone();
        tokio::task::spawn_blocking(move || runtime.prepare_observer_root(root, &db)).await?
    }

    pub(super) fn observer_ready_root(
        &self,
        db: &RuntimeDb,
    ) -> Result<Arc<resident_state::AgentSnapshot>> {
        db.check_read_scope()?;
        let root = self
            .inner
            .mutation_owner
            .capture()
            .map_err(|error| match error {
                mutation_owner::MutationOwnerError::NeedsReconcile(_) => {
                    error.into_observer_busy(Self::OBSERVER_PUBLICATION_OPERATION, db)
                }
                error => error.into(),
            })?
            .ok_or_else(|| {
                crate::runtime_db::RuntimeDbRetryableError::new(
                    Self::OBSERVER_PUBLICATION_OPERATION,
                    db.path(),
                    "Ready publication is not available",
                )
            })?;
        anyhow::ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "observer publication scope differs"
        );
        Ok(root)
    }

    fn observer_publication_window(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
            .is_some_and(|error| error.operation() == Self::OBSERVER_PUBLICATION_OPERATION)
    }

    /// Bounded convergence for async observer reads. A claim or an input
    /// recovery unpublishes the Ready cut only until the next publication, so
    /// waiting briefly beats surfacing retryable busy on the first attempt.
    /// Other errors, and windows that outlive the bound, keep their original
    /// error contract. The check itself stays in memory; no SQL reader is used.
    pub(super) async fn observer_ready_root_bounded(
        &self,
        db: &RuntimeDb,
    ) -> Result<Arc<resident_state::AgentSnapshot>> {
        let deadline = tokio::time::Instant::now() + Self::OBSERVER_PUBLICATION_WAIT;
        loop {
            let error = match self.observer_ready_root(db) {
                Ok(root) => return Ok(root),
                Err(error) => error,
            };
            if !Self::observer_publication_window(&error)
                || tokio::time::Instant::now() + Self::OBSERVER_PUBLICATION_RETRY_DELAY > deadline
            {
                return Err(error);
            }
            tokio::time::sleep(Self::OBSERVER_PUBLICATION_RETRY_DELAY).await;
        }
    }

    fn observer_needs_preparation(root: &resident_state::AgentSnapshot) -> bool {
        root.scheduling.is_none()
            || root
                .current
                .as_ref()
                .and_then(|current| current.query.as_ref())
                .is_none()
    }

    fn prepare_observer_root(
        &self,
        root: Arc<resident_state::AgentSnapshot>,
        db: &RuntimeDb,
    ) -> Result<Arc<resident_state::AgentSnapshot>> {
        if !Self::observer_needs_preparation(&root) {
            return Ok(root);
        }
        let scope = resident_input_evidence::InputEvidenceScope {
            agent_id: root.state.id.clone(),
            identity_incarnation: root.token.identity_incarnation,
        };
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let records = usize::try_from(limits.record_limit)?;
        let bytes = usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("observer material overflow")?,
        )?;
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(ledger, &scope)?;
        let selected = material.read(|| {
            db.with_read(|connection| {
                let tx = connection.transaction()?;
                let facts = crate::runtime_db::context_inputs::ContextDurableRead::new(
                    &tx,
                    &scope.agent_id,
                )
                .scope_facts()?;
                anyhow::ensure!(
                    facts.state.as_deref() == Some(root.state.as_ref()),
                    "observer canonical State differs from its publication"
                );
                let query = crate::runtime_db::current_query::prepare(
                    &tx,
                    &scope,
                    None,
                    ledger,
                    (bytes, records),
                )?;
                let mut selected = root.as_ref().clone();
                if selected.scheduling.is_none() {
                    let loaded =
                        crate::runtime_db::scheduling_snapshot::load_scheduling_domain_snapshot_tx(
                            &tx,
                            &scope,
                            &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
                                capture: resident_scheduling::SchedulingCaptureRequest::current(),
                                record_limit: records,
                                byte_limit: bytes,
                            },
                            ledger,
                        )?
                        .require_current()?;
                    anyhow::ensure!(
                        loaded.facts.state.as_deref() == Some(root.state.as_ref())
                            && loaded.facts.identity.as_ref() == Some(query.identity.as_ref())
                            && loaded.facts.control_revision.is_some(),
                        "observer Scheduling and State observations differ"
                    );
                    selected.scheduling = Some(Arc::new(loaded.resident));
                }
                selected.current = Some(Arc::new(
                    crate::runtime_db::current_read_writes::CommittedCurrentWrites::prepared_query(
                        query,
                    ),
                ));
                tx.commit()?;
                Ok(selected)
            })
        });
        if selected.as_ref().err().is_some_and(|error| {
            error
                .downcast_ref::<crate::runtime_db::RuntimeDbProtectionError>()
                .is_some()
        }) {
            return selected.map(Arc::new);
        }
        // SQL-derived facts belong only to this response. A concurrent ACK or
        // pending recovery must not splice them into an older published cut.
        let current = self.observer_ready_root(db)?;
        if !Arc::ptr_eq(&current, &root) {
            return Err(crate::runtime_db::RuntimeDbRetryableError::new(
                "observer publication",
                db.path(),
                "publication changed during read",
            )
            .into());
        }
        Ok(Arc::new(selected?))
    }

    #[cfg(test)]
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

    #[cfg(test)]
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
        let projection = self.with_scoped_ready_root(db, handle, |root| {
            self.operator_state_from_root(root, task_limit, timer_limit)
        })?;
        db.check_read_scope()?;
        Ok(projection)
    }

    pub(crate) fn acquire_operator_state_read_projection_scoped(
        &self,
        task_limit: usize,
        timer_limit: usize,
        db: &RuntimeDb,
        _handle: &Handle,
    ) -> Result<crate::host::AgentStateReadProjection> {
        db.check_read_scope()?;
        anyhow::ensure!(
            timer_limit <= resident_scheduling::SCHEDULING_HISTORY_WINDOW,
            resident_scheduling::SchedulingReadError::NeedsHistory
        );
        let root = self.observer_ready_root(db)?;
        let projection = self.operator_state_from_root(&root, task_limit, timer_limit)?;
        db.check_read_scope()?;
        Ok(projection)
    }

    #[cfg(test)]
    pub(crate) fn child_agent_summary_observation_scoped(
        &self,
        db: &RuntimeDb,
        handle: &Handle,
    ) -> Result<lifecycle::ChildAgentSummaryObservation> {
        self.with_scoped_ready_root(db, handle, |root| self.child_summary_from_root(root, db))
    }

    #[cfg(test)]
    pub(crate) fn acquire_child_agent_summary_observation_scoped(
        &self,
        db: &RuntimeDb,
        _handle: &Handle,
    ) -> Result<lifecycle::ChildAgentSummaryObservation> {
        let root = self.observer_ready_root(db)?;
        let root = if root.scheduling.is_none() {
            self.prepare_observer_root(root, db)?
        } else {
            root
        };
        let summary = self.child_summary_from_root(&root, db)?;
        db.check_read_scope()?;
        Ok(summary)
    }

    pub(super) fn child_summary_from_root(
        &self,
        root: &resident_state::AgentSnapshot,
        db: &RuntimeDb,
    ) -> Result<lifecycle::ChildAgentSummaryObservation> {
        let read = self.capture_child_observer_scheduler_from_root(root, self.now(), db)?;
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
                        resident_input_evidence::PreparedInputEvidence::Brief(brief) => Some(brief),
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
    }
}

#[cfg(holon_lib_test_shard = "a")]
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

    async fn clear_current_for_scoped_host_read(runtime: &RuntimeHandle) {
        let root = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let mut reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        reservation
            .begin("scoped-host-unprepared-current".into())
            .unwrap();
        reservation
            .committed_prepared_domains(
                root.state.clone(),
                root.episodes.clone(),
                root.input_evidence.clone(),
                root.work.clone(),
                root.tasks.clone(),
                root.scheduling.clone(),
                root.queue_posture,
                root.scheduler_events,
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
    async fn explicit_child_summary_does_not_decode_out_of_scope_task_payloads() {
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
        let projection = host.child_agent_summaries(&parent_id).await;
        assert!(
            projection.is_ok(),
            "child summary decoded unrelated or terminal history: {:?}",
            projection.err()
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_cold_child_summary_preserves_active_owner_and_nonblocking_task_semantics() {
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
            let summaries = host.child_agent_summaries(&parent_id).await.unwrap();
            assert_eq!(summaries.len(), 2);
            for child in &summaries {
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
            let summaries = host.child_agent_summaries(&parent_id).await.unwrap();
            for child in summaries {
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
        // Freeze scheduler writers as well as resource refresh across both reads.
        runtime.wait_for_bootstrap().await.unwrap();
        let run_loop = runtime.inner.run_loop_test_gate.write().await;
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
        child.wait_for_bootstrap().await.unwrap();
        let child_loop = child.inner.run_loop_test_gate.write().await;
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
        assert!(scoped.agent.active_children.is_empty());
        let ordinary = host
            .operator_agent_state_projection(&agent_id, 10, 10)
            .await
            .unwrap();
        assert_eq!(projection_value(&scoped), projection_value(&ordinary));
        drop((refresh, child_refresh, run_loop, child_loop));
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_host_first_loaded_state_does_not_observe_or_publish_children() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        // Hold execution and refresh while comparing the observer's original cut.
        runtime.wait_for_bootstrap().await.unwrap();
        let run_loop = runtime.inner.run_loop_test_gate.write().await;
        runtime.await_initial_resources().await;
        let refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
        let agent_id = runtime.agent_id().await.unwrap();
        let identity = child_identity("scoped-first-read-child", &agent_id);
        host.append_agent_identity(&identity).unwrap();
        let child = host.get_or_create_agent(&identity.agent_id).await.unwrap();
        child.wait_for_bootstrap().await.unwrap();
        let child_loop = child.inner.run_loop_test_gate.write().await;
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

        for _ in 0..2 {
            let db = host.runtime_db().with_read_scope(RuntimeDbReadScope::new(
                Instant::now() + Duration::from_secs(30),
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
            assert_eq!(
                projection.source,
                crate::host::AgentStateProjectionSource::Loaded
            );
            assert_eq!(projection.agent.agent.id, agent_id);
            assert!(projection.agent.active_children.is_empty());
            let after = runtime.inner.mutation_owner.capture().unwrap().unwrap();
            let child_after = child.inner.mutation_owner.capture().unwrap().unwrap();
            for (reader, original, current, event_cursor) in [
                (&runtime, &before, &after, event_cursors[0]),
                (&child, &child_before, &child_after, event_cursors[1]),
            ] {
                assert!(Arc::ptr_eq(original, current));
                assert!(current.current.is_none());
                assert_eq!(original.state, current.state);
                assert!(Arc::ptr_eq(
                    original.scheduling.as_ref().unwrap(),
                    current.scheduling.as_ref().unwrap()
                ));
                let observed = reader
                    .capture_observer_root_async(host.runtime_db())
                    .await
                    .unwrap();
                let query = observed
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
        }
        drop((refresh, child_refresh, run_loop, child_loop));
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_host_unprepared_current_ignores_owner_but_honors_cancellation_and_deadline() {
        let (_home, host, runtime) = host_backed_test_runtime().await;
        runtime.await_initial_resources().await;
        let refresh = runtime.hold_resource_refresh_for_test().await.unwrap();
        let agent_id = runtime.agent_id().await.unwrap();
        clear_current_for_scoped_host_read(&runtime).await;
        let unprepared = runtime.inner.mutation_owner.capture().unwrap().unwrap();
        let reservation = runtime.inner.mutation_owner.reserve().await.unwrap();
        for expired in [false, true] {
            let scope = RuntimeDbReadScope::new(if expired {
                Instant::now() - Duration::from_millis(1)
            } else {
                Instant::now() + Duration::from_secs(30)
            });
            if !expired {
                scope.cancel();
            }
            let db = host.runtime_db().with_read_scope(scope.clone());
            let reader = host.clone();
            let read_id = agent_id.clone();
            let handle = Handle::current();
            let worker = tokio::task::spawn_blocking(move || {
                reader
                    .operator_agent_state_projection_scoped(&read_id, 10, 10, &db, &handle)
                    .map(|_| ())
            });
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
        let healthy = tokio::task::spawn_blocking(move || {
            reader.operator_agent_state_projection_scoped(&agent_id, 10, 10, &db, &handle)
        });
        tokio::time::timeout(Duration::from_secs(5), healthy)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(
            &unprepared,
            &runtime.inner.mutation_owner.capture().unwrap().unwrap()
        ));
        drop(reservation);
        drop(refresh);
        host.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_loaded_captures_leave_unprepared_current_unpublished() {
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
            .committed_prepared_domains(
                root.state.clone(),
                root.episodes.clone(),
                root.input_evidence.clone(),
                root.work.clone(),
                root.tasks.clone(),
                root.scheduling.clone(),
                root.queue_posture,
                root.scheduler_events,
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
            let result = tokio::task::spawn_blocking(move || match operation {
                0 => reader
                    .operator_state_read_projection_scoped(10, 10, &db, &handle)
                    .map(|_| ()),
                _ => reader
                    .child_agent_summary_observation_scoped(&db, &handle)
                    .map(|_| ()),
            })
            .await
            .unwrap();
            // State needs only published facts; child observability may still
            // use its narrow response-local scalar reads.
            result.unwrap();
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
