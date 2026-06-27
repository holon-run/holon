use super::*;
use std::path::{Path, PathBuf};

use crate::runtime::closure::{derive_closure_decision, ClosureFacts};
use crate::storage::AppStorage;
use crate::types::{
    AgentListEntry, AgentTokenUsageSummary, BriefKind, ChildAgentBlockedReason,
    ChildAgentObservabilitySnapshot, ChildAgentPhase, TaskRecord, TaskStatus, TokenUsage,
    WaitingReason, WorkItemState, WorkspaceOccupancyRecord, WorkspaceProjectionMetadata,
    WorktreeSession,
};

fn resolve_enter_cwd(execution_root: &Path, cwd: Option<&Path>) -> Result<PathBuf> {
    let selected_cwd = match cwd {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => execution_root.join(path),
        None => execution_root.to_path_buf(),
    };
    let normalized_root = crate::system::workspace::normalize_path(execution_root)?;
    let normalized_cwd = crate::system::workspace::normalize_path(&selected_cwd)?;
    if !normalized_cwd.starts_with(&normalized_root) {
        return Err(anyhow!(
            "cwd {} escapes execution root {}",
            selected_cwd.display(),
            execution_root.display()
        ));
    }
    Ok(selected_cwd)
}

pub(crate) struct ExistingGitWorktreeWorkspace {
    pub(crate) workspace: WorkspaceEntry,
    pub(crate) worktree_root: PathBuf,
    pub(crate) suggested_isolation_label: Option<String>,
}

fn workspace_paths_match(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

impl RuntimeHandle {
    pub(super) async fn maybe_commit_turn_end_work_item_transition(
        &self,
    ) -> Result<Option<crate::types::WorkItemRecord>> {
        let (turn_index, work_item_id) = {
            let mut guard = self.inner.agent.lock().await;
            let turn_index = guard.state.turn_index;
            let turn_completed = guard
                .state
                .last_turn_terminal
                .as_ref()
                .is_some_and(|record| record.turn_index == turn_index);
            if !turn_completed {
                return Ok(None);
            }
            let work_item_id = guard.state.current_turn_work_item_id.take();
            guard.persist_state(&self.inner.storage)?;
            (turn_index, work_item_id)
        };

        let current_turn_id = {
            let guard = self.inner.agent.lock().await;
            guard.state.current_turn_id.clone()
        };

        let Some(work_item_id) = work_item_id else {
            return Ok(None);
        };

        let closure = self.current_closure_decision().await?;
        let Some(latest) = self.inner.runtime_db.work_items().latest(&work_item_id)? else {
            self.inner.storage.append_event(&AuditEvent::new(
                "work_item_turn_end_commit_skipped",
                serde_json::json!({
                    "agent_id": self.agent_id().await?,
                    "turn_index": turn_index,
                    "work_item_id": work_item_id,
                    "reason": "missing_bound_work_item",
                    "closure": closure,
                }),
            ))?;
            return Ok(None);
        };

        let blocked_by = match closure.waiting_reason {
            Some(waiting_reason) => Some(waiting_reason_blocker(waiting_reason).to_string()),
            None if closure.outcome == crate::types::ClosureOutcome::Failed => {
                latest.blocked_by.clone()
            }
            None => None,
        };
        let mut refreshed = latest.clone();
        let plan_artifact_changed = crate::work_item_plan::refresh_plan_artifact_metadata(
            self.agent_home().as_path(),
            &mut refreshed,
        )?;
        let work_item_state_changed =
            latest.state == WorkItemState::Open && latest.blocked_by != blocked_by;
        let wrote_new_snapshot = work_item_state_changed || plan_artifact_changed;
        let committed = if wrote_new_snapshot {
            let record = crate::types::WorkItemRecord {
                revision: latest.revision + 1,
                blocked_by,
                updated_at: chrono::Utc::now(),
                turn_id: current_turn_id,
                ..refreshed
            };
            let current_focus = self.agent_state().await?.current_work_item_id.as_deref()
                == Some(record.id.as_str());
            self.inner
                .runtime_db
                .work_items()
                .upsert(&record, current_focus)?;
            self.record_work_item_projection(&record).await?;
            if plan_artifact_changed {
                self.append_work_item_plan_artifact_refreshed_event(&record)?;
            }
            self.append_work_item_written_event("turn_end_committed", &record, Value::Null)?;
            record
        } else {
            latest
        };

        self.inner.storage.append_event(&AuditEvent::new(
            "work_item_turn_end_committed",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "turn_index": turn_index,
                "work_item_id": committed.id,
                "committed_state": committed.state,
                "wrote_new_snapshot": wrote_new_snapshot,
                "plan_artifact_changed": plan_artifact_changed,
                "work_item_state_changed": work_item_state_changed,
                "closure": closure,
            }),
        ))?;
        Ok(Some(committed))
    }

    pub(crate) async fn closure_decision_for_state(
        &self,
        state: &AgentState,
        runtime_error_override: Option<bool>,
    ) -> Result<ClosureDecision> {
        let work_queue_projection = self.inner.storage.work_queue_prompt_projection()?;
        let projection = scheduler::SchedulerProjection::from_state_with_work_queue(
            &self.inner.storage,
            state,
            work_queue_projection,
        )?;
        let runtime_error = runtime_error_override.unwrap_or(projection.runtime_error);
        Ok(derive_closure_decision(&ClosureFacts {
            runtime_error,
            awaiting_operator_input: projection.current_work_item_waits_for_operator(),
            active_blocking_tasks: projection
                .active_tasks
                .iter()
                .filter(|task| task.is_blocking())
                .count(),
            active_waiting_intents: projection.active_waiting_intents,
            active_agent_waiting_intents: projection.active_agent_waiting_intents,
            active_timers: projection.active_timers,
            waiting_work_item_scheduling_state: projection.waiting_work_item_scheduling_state,
            work_signal: projection.work_reactivation_signal(),
            turn_started: state.turn_index > 0,
            turn_in_progress: state.current_run_id.is_some(),
            turn_terminal_kind: state
                .last_turn_terminal
                .as_ref()
                .filter(|record| record.turn_index == state.turn_index)
                .map(|record| record.kind),
            runtime_posture: Some(if state.status == AgentStatus::Asleep {
                RuntimePosture::Sleeping
            } else {
                RuntimePosture::Awake
            }),
        }))
    }

    pub(crate) fn closure_decision_from_storage(
        storage: &AppStorage,
        state: &AgentState,
    ) -> Result<ClosureDecision> {
        let projection = scheduler::SchedulerProjection::from_state(storage, state)?;
        Ok(derive_closure_decision(&ClosureFacts {
            runtime_error: projection.runtime_error,
            awaiting_operator_input: projection.current_work_item_waits_for_operator(),
            active_blocking_tasks: projection
                .active_tasks
                .iter()
                .filter(|task| task.is_blocking())
                .count(),
            active_waiting_intents: projection.active_waiting_intents,
            active_agent_waiting_intents: projection.active_agent_waiting_intents,
            active_timers: projection.active_timers,
            waiting_work_item_scheduling_state: projection.waiting_work_item_scheduling_state,
            work_signal: projection.work_reactivation_signal(),
            turn_started: state.turn_index > 0,
            turn_in_progress: state.current_run_id.is_some(),
            turn_terminal_kind: state
                .last_turn_terminal
                .as_ref()
                .filter(|record| record.turn_index == state.turn_index)
                .map(|record| record.kind),
            runtime_posture: Some(if state.status == AgentStatus::Asleep {
                RuntimePosture::Sleeping
            } else {
                RuntimePosture::Awake
            }),
        }))
    }

    pub(super) fn append_state_changed_events(&self, state: &AgentState) -> Result<()> {
        let state_payload = AgentStateChangedEvent::from_state(state);
        self.inner.storage.append_event(&AuditEvent::new(
            "agent_state_changed",
            to_json_value(&state_payload),
        ))?;
        Ok(())
    }

    pub async fn control(&self, action: ControlAction) -> Result<()> {
        let outcome = super::scheduler_executor::SchedulerDecisionExecutor::new(self)
            .apply_control(action)
            .await?;
        if let Some(occupancy_id) = outcome.occupancy_to_release.as_deref() {
            let bridge = self.inner.host_bridge.as_ref().ok_or_else(|| {
                anyhow!(
                    "cannot release workspace occupancy {} without host bridge",
                    occupancy_id
                )
            })?;
            let _ = bridge.release_workspace_occupancy(occupancy_id).await?;
            {
                let mut guard = self.inner.agent.lock().await;
                let should_clear = guard
                    .state
                    .active_workspace_entry
                    .as_ref()
                    .and_then(|entry| entry.occupancy_id.as_deref())
                    == Some(occupancy_id);
                if should_clear {
                    guard.state.active_workspace_entry = None;
                    guard.persist_state(&self.inner.storage)?;
                }
            }
        }
        if matches!(outcome.action, ControlAction::Stop) {
            let agent_id = self.agent_id().await?;
            let active_tasks = self
                .inner
                .storage
                .latest_active_task_records_for_agent(&agent_id, usize::MAX)?;
            self.interrupt_active_tasks_for_lifecycle_stop(active_tasks)
                .await?;
        }
        self.inner.storage.append_event(&AuditEvent::new(
            "control_applied",
            serde_json::json!({
                "requested_action": outcome.requested_action,
                "action": outcome.action,
                "status": outcome.status,
                "current_run_id": outcome.current_run_id,
                "aborted_run_id": outcome.aborted_run_id,
            }),
        ))?;
        if let Some(run_id) = outcome.aborted_run_id {
            self.inner.storage.append_event(&AuditEvent::new(
                "current_run_aborted",
                serde_json::json!({
                    "agent_id": self.agent_id().await?,
                    "run_id": run_id,
                    "mode": "stop",
                    "reason": "agent_stopped",
                }),
            ))?;
        }
        self.inner.notify.notify_one();
        Ok(())
    }

    pub(crate) async fn request_service_shutdown(&self) -> Result<()> {
        self.inner
            .shutdown_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome = super::scheduler_executor::SchedulerDecisionExecutor::new(self)
            .request_shutdown(super::scheduler_executor::ShutdownReason::DaemonShutdown)
            .await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "runtime_service_shutdown_requested",
            serde_json::json!({
                "aborted_run_id": &outcome.aborted_run_id,
                "status": outcome.status,
                "current_run_id": &outcome.current_run_id,
            }),
        ))?;
        if let Some(run_id) = outcome.aborted_run_id {
            self.inner.storage.append_event(&AuditEvent::new(
                "current_run_aborted",
                serde_json::json!({
                    "agent_id": self.agent_id().await?,
                    "run_id": run_id,
                    "mode": "shutdown",
                    "reason": "daemon_shutdown",
                }),
            ))?;
        }
        self.inner.notify.notify_one();
        Ok(())
    }

    pub async fn agent_state(&self) -> Result<AgentState> {
        let guard = self.inner.agent.lock().await;
        Ok(guard.state.clone())
    }

    pub(crate) async fn workspace_entry_by_id(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspaceEntry>> {
        let Some(bridge) = self.inner.host_bridge.as_ref() else {
            return Ok(None);
        };
        bridge.workspace_entry_by_id(workspace_id).await
    }

    pub(crate) async fn current_closure_decision(&self) -> Result<ClosureDecision> {
        self.current_closure_decision_with_runtime_error(None).await
    }

    pub async fn current_closure(&self) -> Result<Option<ClosureDecision>> {
        let state = self.agent_state().await?;
        if state.current_run_id.is_some() || state.pending > 0 || state.pending_wake_hint.is_some()
        {
            return Ok(None);
        }
        self.closure_decision_for_state(&state, None)
            .await
            .map(Some)
    }

    pub async fn wait_for_closure(&self) -> Result<ClosureDecision> {
        loop {
            if let Some(closure) = self.current_closure().await? {
                return Ok(closure);
            }
            tokio::select! {
                _ = self.inner.notify.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
    }

    pub(crate) async fn current_closure_decision_with_runtime_error(
        &self,
        runtime_error_override: Option<bool>,
    ) -> Result<ClosureDecision> {
        let state = self.agent_state().await?;
        self.closure_decision_for_state(&state, runtime_error_override)
            .await
    }

    pub(crate) async fn record_closure_decision_event(
        &self,
        runtime_error_override: Option<bool>,
    ) -> Result<ClosureDecision> {
        let closure = self
            .current_closure_decision_with_runtime_error(runtime_error_override)
            .await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "closure_decided",
            serde_json::json!({
                "agent_id": self.agent_id().await?,
                "closure": closure,
            }),
        ))?;
        Ok(closure)
    }

    pub async fn agent_summary(&self) -> Result<AgentSummary> {
        // Fast path: return current agent summary.
        self.build_agent_summary().await
    }

    /// Get a full AgentSummary for a different agent through the host bridge.
    /// This allows observing private child agents through the local trusted
    /// control boundary.
    pub async fn agent_summary_for(&self, agent_id: &str) -> Result<AgentSummary> {
        let bridge = self
            .inner
            .host_bridge
            .as_ref()
            .ok_or_else(|| anyhow!("agent_summary_for: no host bridge available"))?;
        bridge.agent_summary_for(agent_id).await
    }

    async fn build_agent_summary(&self) -> Result<AgentSummary> {
        let started_at = std::time::Instant::now();
        let agent = self.agent_state().await?;
        let active_task_count = self.inner.storage.active_task_count_for_agent(&agent.id)?;
        let model = self.model_state_for(&agent);
        let scheduling_posture = self.inner.storage.agent_posture_projection(&agent)?;
        let closure = self.current_closure_decision().await?;
        let execution = self.execution_snapshot().await?;
        let loaded_agents_md = self.loaded_agents_md().await?;
        let identity = self.agent_identity_view().await?;
        let skills = self.skills_runtime_view(&identity).await?;
        let active_workspace_occupancy = if let (Some(bridge), Some(entry)) = (
            self.inner.host_bridge.as_ref(),
            agent.active_workspace_entry.as_ref(),
        ) {
            if let Some(occupancy_id) = entry.occupancy_id.as_deref() {
                bridge.workspace_occupancy_by_id(occupancy_id).await?
            } else {
                None
            }
        } else {
            None
        };
        let active_children = if let Some(bridge) = self.inner.host_bridge.as_ref() {
            bridge.child_summaries(&agent.id).await?
        } else {
            Vec::new()
        };
        let token_usage = AgentTokenUsageSummary {
            total: TokenUsage::new(agent.total_input_tokens, agent.total_output_tokens),
            total_model_rounds: agent.total_model_rounds,
            last_turn: agent.last_turn_token_usage.clone(),
        };
        let active_external_triggers = self.active_external_trigger_summaries().await?;
        let summary = AgentSummary {
            identity,
            lifecycle: crate::types::AgentLifecycleHint::from_status(
                &agent.id,
                agent.status.clone(),
            ),
            agent,
            scheduling_posture,
            active_task_count,
            model,
            token_usage,
            closure,
            execution,
            active_workspace_occupancy,
            loaded_agents_md: (&loaded_agents_md).into(),
            skills,
            active_children,
            active_waiting_intents: self.active_waiting_intent_summaries().await?,
            active_wait_conditions: self.active_wait_condition_summaries().await?,
            active_external_triggers,
            recent_operator_notifications: self.recent_operator_notifications(10).await?,
            recent_brief_count: self.inner.storage.read_recent_briefs(50)?.len(),
            recent_event_count: self.inner.storage.read_recent_events(100)?.len(),
        };
        crate::diagnostics::record_agent_summary_projection(started_at.elapsed());
        Ok(summary)
    }

    pub async fn agent_list_entry(&self) -> Result<AgentListEntry> {
        let agent = self.agent_state().await?;
        let model = self.model_state_for(&agent);
        let scheduling_posture = match self.inner.storage.agent_posture_projection(&agent) {
            Ok(posture) => posture,
            Err(error) => {
                tracing::warn!(
                    agent_id = %agent.id,
                    error = %error,
                    "failed to read agent posture for /agents/list; using unknown placeholder"
                );
                crate::types::AgentPostureProjection::default()
            }
        };
        let identity = self.agent_identity_view().await?;
        let waiting_reason = super::lightweight_agent_list_waiting_reason(&agent);
        Ok(AgentListEntry {
            identity,
            lifecycle: crate::types::AgentLifecycleHint::from_status(
                &agent.id,
                agent.status.clone(),
            ),
            status: agent.status,
            scheduling_posture,
            pending: agent.pending,
            current_run_id: agent.current_run_id,
            waiting_reason,
            model: (&model).into(),
            active_workspace_entry: agent
                .active_workspace_entry
                .map(crate::types::ActiveWorkspaceEntry::without_projection_metadata),
        })
    }

    pub async fn recent_events(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        self.inner.storage.read_recent_events(limit)
    }

    pub async fn recent_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        self.inner.storage.read_recent_tasks(limit)
    }

    pub async fn active_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        crate::diagnostics::record_runtime_projection_cache_read();
        Ok(self.inner.projection_cache.lock().await.active_tasks(limit))
    }

    pub async fn recent_transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        self.inner.storage.read_recent_transcript(limit)
    }

    pub async fn transcript_entry_by_id(&self, entry_id: &str) -> Result<Option<TranscriptEntry>> {
        self.inner.storage.read_transcript_entry_by_id(entry_id)
    }

    pub(crate) async fn child_agent_observability(
        &self,
    ) -> Result<ChildAgentObservabilitySnapshot> {
        let agent = self.agent_state().await?;
        let closure = self.current_closure_decision().await?;
        let latest_tasks = self.latest_task_records().await?;
        let briefs = self.recent_briefs(16).await?;
        Ok(build_child_agent_observability(
            &agent,
            closure.waiting_reason,
            self.active_work_item_waiting_intent_count().await?,
            &latest_tasks,
            &briefs,
        ))
    }

    pub(crate) fn child_agent_observability_from_storage(
        storage: &AppStorage,
        state: &AgentState,
    ) -> Result<ChildAgentObservabilitySnapshot> {
        let projection = scheduler::SchedulerProjection::from_state(storage, state)?;
        let active_tasks = projection.active_tasks.iter().collect::<Vec<_>>();
        let closure = derive_closure_decision(&ClosureFacts {
            runtime_error: projection.runtime_error,
            awaiting_operator_input: projection.current_work_item_waits_for_operator(),
            active_blocking_tasks: blocking_task_count(&active_tasks),
            active_waiting_intents: projection.active_waiting_intents,
            active_agent_waiting_intents: projection.active_agent_waiting_intents,
            active_timers: projection.active_timers,
            waiting_work_item_scheduling_state: projection.waiting_work_item_scheduling_state,
            work_signal: projection.work_reactivation_signal(),
            turn_started: state.turn_index > 0,
            turn_in_progress: state.current_run_id.is_some(),
            turn_terminal_kind: state
                .last_turn_terminal
                .as_ref()
                .filter(|record| record.turn_index == state.turn_index)
                .map(|record| record.kind),
            runtime_posture: Some(if state.status == AgentStatus::Asleep {
                RuntimePosture::Sleeping
            } else {
                RuntimePosture::Awake
            }),
        });
        let briefs = storage.read_recent_briefs(16)?;
        Ok(build_child_agent_observability_with_active_tasks(
            state,
            closure.waiting_reason,
            projection.active_work_item_waiting_intents,
            &active_tasks,
            &briefs,
        ))
    }

    pub async fn latest_work_items(&self) -> Result<Vec<crate::types::WorkItemRecord>> {
        self.inner.runtime_db.work_items().latest_all()
    }

    pub async fn latest_work_items_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<crate::types::WorkItemRecord>> {
        let current_agent_id = self.agent_id().await?;
        if agent_id == current_agent_id {
            crate::diagnostics::record_runtime_projection_cache_read();
            return Ok(self
                .inner
                .projection_cache
                .lock()
                .await
                .latest_work_items(limit));
        }
        self.inner
            .runtime_db
            .work_items()
            .latest_for_agent(agent_id, limit)
    }

    pub async fn latest_work_item(
        &self,
        work_item_id: &str,
    ) -> Result<Option<crate::types::WorkItemRecord>> {
        self.inner.runtime_db.work_items().latest(work_item_id)
    }

    pub async fn search_memory(
        &self,
        query: &str,
        limit: usize,
        include_all_workspaces: bool,
    ) -> Result<crate::memory::MemorySearchQueryResult> {
        let active_workspace_id = self
            .agent_state()
            .await?
            .active_workspace_entry
            .map(|entry| entry.workspace_id);
        let storage = self.inner.storage.clone();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            crate::memory::search_memory_query(
                &storage,
                &query,
                limit,
                active_workspace_id.as_deref(),
                include_all_workspaces,
            )
        })
        .await?
    }

    pub async fn search_memory_for_agents(
        &self,
        query: &str,
        limit: usize,
        include_all_workspaces: bool,
        agent_ids: &[String],
    ) -> Result<crate::memory::MemorySearchQueryResult> {
        let active_workspace_id = self
            .agent_state()
            .await?
            .active_workspace_entry
            .map(|entry| entry.workspace_id);
        let storage = self.inner.storage.clone();
        let agent_storages = self
            .inner
            .host_bridge
            .as_ref()
            .map(|bridge| {
                agent_ids
                    .iter()
                    .map(|agent_id| bridge.agent_storage(agent_id))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let query = query.to_string();
        let agent_ids = agent_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            crate::memory::search_memory_query_for_agent_storages(
                &storage,
                &query,
                limit,
                active_workspace_id.as_deref(),
                include_all_workspaces,
                &agent_ids,
                &agent_storages,
            )
        })
        .await?
    }

    pub async fn get_memory(
        &self,
        source_ref: &str,
        max_chars: Option<usize>,
    ) -> Result<Option<crate::memory::MemoryGetResult>> {
        let active_workspace_id = self
            .agent_state()
            .await?
            .active_workspace_entry
            .map(|entry| entry.workspace_id);
        let storage = self.inner.storage.clone();
        let source_ref = source_ref.to_string();
        tokio::task::spawn_blocking(move || {
            crate::memory::get_memory(
                &storage,
                &source_ref,
                max_chars,
                active_workspace_id.as_deref(),
            )
        })
        .await?
    }

    pub async fn refresh_memory_index_for_changed_paths(
        &self,
        changed_paths: &[String],
    ) -> Result<()> {
        let storage = self.inner.storage.clone();
        let changed_paths = changed_paths.to_vec();
        tokio::task::spawn_blocking(move || {
            crate::memory::repair_memory_index_for_paths(&storage, &changed_paths)
        })
        .await?
    }

    pub async fn recent_timers(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        crate::diagnostics::record_runtime_projection_cache_read();
        Ok(self
            .inner
            .projection_cache
            .lock()
            .await
            .recent_timers(limit))
    }

    pub async fn latest_timer(&self, timer_id: &str) -> Result<Option<TimerRecord>> {
        self.inner.storage.latest_timer_record(timer_id)
    }

    pub async fn set_model_override(
        &self,
        model_override: crate::config::ModelRef,
        reasoning_effort: Option<String>,
    ) -> Result<crate::types::AgentModelState> {
        let mut next_state = self.agent_state().await?;
        next_state.model_override = Some(model_override.clone());
        next_state.model_override_reasoning_effort = reasoning_effort.clone();
        next_state.pending_fallback_model = None;
        let turn_in_progress = next_state.current_run_id.is_some();
        if !turn_in_progress {
            self.reconfigure_provider_for_state(&next_state).await?;
        }

        let model_state = self.model_state_for(&next_state);
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.model_override = Some(model_override);
            guard.state.model_override_reasoning_effort = reasoning_effort;
            guard.state.pending_fallback_model = None;
            guard.persist_state(&self.inner.storage)?;
        }
        self.append_audit_event(
            if turn_in_progress {
                "agent_model_override_requested"
            } else {
                "agent_model_override_set"
            },
            to_json_value(&AgentModelOverrideAuditEvent::from_model_state(
                next_state.id,
                &model_state,
                turn_in_progress,
            )),
        )?;
        Ok(model_state)
    }

    pub async fn clear_model_override(&self) -> Result<crate::types::AgentModelState> {
        let mut next_state = self.agent_state().await?;
        next_state.model_override = None;
        next_state.model_override_reasoning_effort = None;
        next_state.pending_fallback_model = None;
        let turn_in_progress = next_state.current_run_id.is_some();
        if !turn_in_progress {
            self.reconfigure_provider_for_state(&next_state).await?;
        }

        let model_state = self.model_state_for(&next_state);
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.model_override = None;
            guard.state.model_override_reasoning_effort = None;
            guard.state.pending_fallback_model = None;
            guard.persist_state(&self.inner.storage)?;
        }
        self.append_audit_event(
            if turn_in_progress {
                "agent_model_override_clear_requested"
            } else {
                "agent_model_override_cleared"
            },
            to_json_value(&AgentModelOverrideAuditEvent::from_model_state(
                next_state.id,
                &model_state,
                turn_in_progress,
            )),
        )?;
        Ok(model_state)
    }

    /// Acquire exclusive or shared occupancy for a workspace execution root.
    /// In bridge mode this delegates to the shared `RuntimeRegistry`; in
    /// standalone mode occupancy tracking is unavailable so it returns `None`.
    async fn acquire_workspace_occupancy(
        &self,
        workspace_id: &str,
        execution_root_id: &str,
        holder_agent_id: &str,
        access_mode: WorkspaceAccessMode,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        match self.inner.host_bridge.as_ref() {
            Some(bridge) => {
                bridge
                    .acquire_workspace_occupancy(
                        workspace_id,
                        execution_root_id,
                        holder_agent_id,
                        access_mode,
                    )
                    .await
            }
            None => Ok(None),
        }
    }

    /// Release a previously acquired workspace occupancy.
    /// In standalone mode this is a no-op returning `None`.
    async fn release_workspace_occupancy(
        &self,
        occupancy_id: &str,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        match self.inner.host_bridge.as_ref() {
            Some(bridge) => bridge.release_workspace_occupancy(occupancy_id).await,
            None => Ok(None),
        }
    }

    pub async fn attach_workspace(&self, workspace: &WorkspaceEntry) -> Result<()> {
        let mut guard = self.inner.agent.lock().await;
        if !guard
            .state
            .attached_workspaces
            .iter()
            .any(|id| id == &workspace.workspace_id)
        {
            guard
                .state
                .attached_workspaces
                .push(workspace.workspace_id.clone());
        }
        self.inner.storage.append_workspace_entry(workspace)?;
        guard.persist_state(&self.inner.storage)?;
        let state = guard.state.clone();
        drop(guard);
        self.inner.storage.append_event(&AuditEvent::new(
            "workspace_attached",
            serde_json::json!({
                "workspace_id": workspace.workspace_id,
                "workspace_anchor": workspace.workspace_anchor,
            }),
        ))?;
        self.sync_effective_skill_roots_for_state(&state).await?;
        Ok(())
    }

    pub async fn detach_workspace(&self, workspace_id: &str) -> Result<()> {
        let workspace_id = workspace_id.trim();
        if workspace_id.is_empty() {
            return Err(anyhow!("workspace_id is required"));
        }

        let detached_agent_id = {
            let mut guard = self.inner.agent.lock().await;
            let canonical_agent_home_id = crate::types::agent_home_workspace_id(&guard.state.id);
            if workspace_id == AGENT_HOME_WORKSPACE_ID || workspace_id == canonical_agent_home_id {
                let redundant_legacy_agent_home = guard
                    .state
                    .attached_workspaces
                    .iter()
                    .any(|id| id == AGENT_HOME_WORKSPACE_ID)
                    && guard
                        .state
                        .attached_workspaces
                        .iter()
                        .any(|id| id == &canonical_agent_home_id)
                    && !guard
                        .state
                        .active_workspace_entry
                        .as_ref()
                        .is_some_and(|entry| entry.workspace_id == AGENT_HOME_WORKSPACE_ID);
                if workspace_id != AGENT_HOME_WORKSPACE_ID || !redundant_legacy_agent_home {
                    return Err(anyhow!("AgentHome cannot be detached"));
                }
            }

            if guard
                .state
                .active_workspace_entry
                .as_ref()
                .is_some_and(|entry| entry.workspace_id == workspace_id)
            {
                return Err(anyhow!(
                    "workspace {workspace_id} is active; use UseWorkspace with another workspace_id first, then retry DetachWorkspace"
                ));
            }

            let before_len = guard.state.attached_workspaces.len();
            guard
                .state
                .attached_workspaces
                .retain(|id| id != workspace_id);
            if guard.state.attached_workspaces.len() == before_len {
                return Err(anyhow!(
                    "workspace {workspace_id} is not attached to agent {}",
                    guard.state.id
                ));
            }
            guard.persist_state(&self.inner.storage)?;
            guard.state.id.clone()
        };

        self.inner.storage.append_event(&AuditEvent::new(
            "workspace_detached",
            serde_json::json!({
                "agent_id": detached_agent_id,
                "workspace_id": workspace_id,
            }),
        ))?;
        Ok(())
    }

    /// Lazily remove stale entries from `attached_workspaces`.
    ///
    /// An entry is stale when its workspace ID is absent from the workspace
    /// registry, or when the registry entry's anchor path no longer exists on
    /// disk. The active workspace is always preserved, even if its anchor is
    /// currently invalid, to avoid silently clearing the execution context.
    ///
    /// Returns the list of workspace IDs that were pruned (empty if nothing
    /// changed). Each pruned ID emits a `workspace_detached` audit event with
    /// `reason: "stale_workspace_anchor"`.
    pub async fn prune_stale_attached_workspaces(&self) -> Result<Vec<String>> {
        let bridge = match self.inner.host_bridge.as_ref() {
            Some(b) => b,
            None => return Ok(Vec::new()), // no registry to check against
        };

        let agent_id_snapshot = {
            let guard = self.inner.agent.lock().await;
            guard.state.id.clone()
        };
        let canonical_agent_home_id = crate::types::agent_home_workspace_id(&agent_id_snapshot);

        let active_workspace_id = {
            let guard = self.inner.agent.lock().await;
            guard
                .state
                .active_workspace_entry
                .as_ref()
                .map(|e| e.workspace_id.clone())
        };

        // Collect IDs to prune under the agent lock so we have a consistent
        // snapshot of attached_workspaces.
        let attached_ids: Vec<String> = {
            let guard = self.inner.agent.lock().await;
            guard.state.attached_workspaces.clone()
        };

        let mut stale_ids = Vec::new();
        for ws_id in &attached_ids {
            // Never prune the active workspace or AgentHome (virtual, may not be in registry).
            if Some(ws_id) == active_workspace_id.as_ref()
                || ws_id == AGENT_HOME_WORKSPACE_ID
                || ws_id == &canonical_agent_home_id
            {
                continue;
            }

            // Check registry: entry must exist and its anchor must be a real directory.
            let entry = bridge.workspace_entry_by_id(ws_id).await?;
            let is_stale = match &entry {
                None => true, // ID not in registry
                Some(e) => !e.workspace_anchor.is_dir(),
            };
            if is_stale {
                stale_ids.push(ws_id.clone());
            }
        }

        if stale_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Remove stale IDs and persist.
        let agent_id = {
            let mut guard = self.inner.agent.lock().await;
            guard
                .state
                .attached_workspaces
                .retain(|id| !stale_ids.contains(id));
            guard.persist_state(&self.inner.storage)?;
            guard.state.id.clone()
        };

        for ws_id in &stale_ids {
            self.inner.storage.append_event(&AuditEvent::new(
                "workspace_detached",
                serde_json::json!({
                    "agent_id": &agent_id,
                    "workspace_id": ws_id,
                    "reason": "stale_workspace_anchor",
                }),
            ))?;
        }

        Ok(stale_ids)
    }

    pub(crate) async fn ensure_workspace_entry_for_path(
        &self,
        path: PathBuf,
    ) -> Result<WorkspaceEntry> {
        let normalized_anchor = crate::system::workspace::normalize_path(&path)?;
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            let workspace = bridge
                .ensure_workspace_entry(normalized_anchor.clone())
                .await?;
            return Ok(workspace);
        }
        if let Some(existing) = self
            .inner
            .storage
            .latest_workspace_entries()?
            .into_iter()
            .find(|entry| entry.workspace_anchor == normalized_anchor)
        {
            return Ok(existing);
        }
        let workspace = WorkspaceEntry::new(
            crate::ids::deterministic_workspace_id(&normalized_anchor),
            normalized_anchor.clone(),
            normalized_anchor
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToString::to_string),
        );
        Ok(workspace)
    }

    pub(crate) async fn attached_workspace_for_existing_git_worktree(
        &self,
        path: &Path,
    ) -> Result<Option<ExistingGitWorktreeWorkspace>> {
        let Some(detected) = crate::system::workspace::detect_existing_git_worktree(path)? else {
            return Ok(None);
        };
        let state = self.agent_state().await?;
        let known = self.inner.storage.latest_workspace_entries()?;
        let Some(workspace) = known.into_iter().find(|entry| {
            workspace_paths_match(&entry.workspace_anchor, &detected.parent_workspace_anchor)
                && state
                    .attached_workspaces
                    .iter()
                    .any(|id| id == &entry.workspace_id)
        }) else {
            return Ok(None);
        };
        let suggested_isolation_label = detected
            .worktree_root
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToString::to_string);
        Ok(Some(ExistingGitWorktreeWorkspace {
            workspace,
            worktree_root: detected.worktree_root,
            suggested_isolation_label,
        }))
    }

    pub(crate) async fn workspace_entry_for_use(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspaceEntry>> {
        if workspace_id == AGENT_HOME_WORKSPACE_ID {
            let agent_id = self.agent_id().await?;
            return Ok(Some(Self::agent_home_workspace_entry(
                self.inner.storage.data_dir(),
                &agent_id,
            )));
        }
        if let Some(existing) = self
            .inner
            .storage
            .latest_workspace_entries()?
            .into_iter()
            .find(|entry| entry.workspace_id == workspace_id)
        {
            return Ok(Some(existing));
        }
        self.workspace_entry_by_id(workspace_id).await
    }

    pub(crate) async fn activate_agent_home(
        &self,
        access_mode: WorkspaceAccessMode,
        cwd: Option<PathBuf>,
    ) -> Result<()> {
        let state = self.agent_state().await?;
        let workspace = Self::agent_home_workspace_entry(self.inner.storage.data_dir(), &state.id);
        let execution_root = crate::system::workspace::normalize_path(&workspace.workspace_anchor)?;
        let selected_cwd = resolve_enter_cwd(&execution_root, cwd.as_deref())?;
        let execution_root_id = Self::build_execution_root_id(
            &workspace.workspace_id,
            WorkspaceProjectionKind::CanonicalRoot,
            &execution_root,
        )?;
        let previous_occupancy_id = state
            .active_workspace_entry
            .as_ref()
            .and_then(|entry| entry.occupancy_id.clone());
        let entry = ActiveWorkspaceEntry {
            workspace_id: workspace.workspace_id.clone(),
            workspace_anchor: workspace.workspace_anchor.clone(),
            execution_root_id: execution_root_id.clone(),
            execution_root: execution_root.clone(),
            projection_kind: WorkspaceProjectionKind::CanonicalRoot,
            access_mode,
            cwd: selected_cwd.clone(),
            occupancy_id: None,
            projection_metadata: None,
        };
        {
            let mut guard = self.inner.agent.lock().await;
            workspace::canonicalize_agent_home_bindings(
                &mut guard.state,
                self.inner.storage.data_dir(),
                &state.id,
            )?;
            if !guard
                .state
                .attached_workspaces
                .iter()
                .any(|id| id == &workspace.workspace_id)
            {
                guard
                    .state
                    .attached_workspaces
                    .push(workspace.workspace_id.clone());
            }
            let known = self.inner.storage.latest_workspace_entries()?;
            if !known
                .iter()
                .any(|known| known.workspace_id == workspace.workspace_id)
            {
                self.inner.storage.append_workspace_entry(&workspace)?;
            }
            guard.state.active_workspace_entry = Some(entry);
            guard.state.worktree_session = None;
            guard.persist_state(&self.inner.storage)?;
            self.inner.storage.mark_memory_index_dirty()?;
        }
        if let Some(occupancy_id) = previous_occupancy_id.as_deref() {
            let _ = self.release_workspace_occupancy(occupancy_id).await?;
        }
        let state = self.agent_state().await?;
        self.sync_effective_skill_roots_for_state(&state).await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "workspace_used",
            serde_json::json!({
                "workspace_id": workspace.workspace_id,
                "workspace_anchor": workspace.workspace_anchor,
                "execution_root_id": execution_root_id,
                "execution_root": execution_root,
                "projection_kind": WorkspaceProjectionKind::CanonicalRoot,
                "access_mode": access_mode,
                "cwd": selected_cwd,
            }),
        ))?;
        Ok(())
    }

    pub async fn enter_workspace(
        &self,
        workspace: &WorkspaceEntry,
        projection_kind: WorkspaceProjectionKind,
        access_mode: WorkspaceAccessMode,
        cwd: Option<PathBuf>,
        branch_name: Option<String>,
    ) -> Result<()> {
        let agent_id = self.agent_id().await?;
        let existing_state = self.agent_state().await?;
        if !existing_state
            .attached_workspaces
            .iter()
            .any(|id| id == &workspace.workspace_id)
        {
            return Err(anyhow!(
                "workspace {} is not attached to agent {}",
                workspace.workspace_id,
                existing_state.id
            ));
        }
        crate::system::ensure_workspace_projection_allowed(
            &crate::system::HostLocalBoundary::from_parts(
                &existing_state.execution_profile,
                existing_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.projection_kind),
                existing_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode),
                existing_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.execution_root_id.clone()),
            ),
            projection_kind,
            "enter_workspace",
        )?;

        let normalized_anchor =
            crate::system::workspace::normalize_path(&workspace.workspace_anchor)?;
        let (execution_root, worktree_session, projection_metadata) = match projection_kind {
            WorkspaceProjectionKind::CanonicalRoot => (normalized_anchor.clone(), None, None),
            WorkspaceProjectionKind::GitWorktreeRoot => {
                let branch_name = branch_name
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| anyhow!("branch_name is required for git_worktree_root"))?;
                let seed = self.prepare_managed_worktree(&branch_name).await?;
                let session = WorktreeSession {
                    original_cwd: seed.original_cwd.clone(),
                    original_branch: seed.original_branch.clone(),
                    worktree_path: seed.worktree_path.clone(),
                    worktree_branch: seed.worktree_branch.clone(),
                };
                let metadata = WorkspaceProjectionMetadata::ManagedWorktree {
                    original_cwd: session.original_cwd.clone(),
                    original_branch: session.original_branch.clone(),
                    worktree_path: session.worktree_path.clone(),
                    worktree_branch: session.worktree_branch.clone(),
                };
                (session.worktree_path.clone(), Some(session), Some(metadata))
            }
        };
        let selected_cwd = resolve_enter_cwd(&execution_root, cwd.as_deref())?;
        let execution_root_id = Self::build_execution_root_id(
            &workspace.workspace_id,
            projection_kind,
            &execution_root,
        )?;
        let occupancy = self
            .acquire_workspace_occupancy(
                &workspace.workspace_id,
                &execution_root_id,
                &agent_id,
                access_mode,
            )
            .await?;
        let entry = ActiveWorkspaceEntry {
            workspace_id: workspace.workspace_id.clone(),
            workspace_anchor: workspace.workspace_anchor.clone(),
            execution_root_id: execution_root_id.clone(),
            execution_root: execution_root.clone(),
            projection_kind,
            access_mode,
            cwd: selected_cwd.clone(),
            occupancy_id: occupancy.as_ref().map(|record| record.occupancy_id.clone()),
            projection_metadata,
        };
        let previous_occupancy_id = existing_state
            .active_workspace_entry
            .as_ref()
            .and_then(|existing_entry| existing_entry.occupancy_id.clone());
        let new_occupancy_id = entry.occupancy_id.clone();
        let worktree_cleanup_session = worktree_session.clone();

        let write_result: Result<()> = async {
            let mut guard = self.inner.agent.lock().await;
            guard.state.active_workspace_entry = Some(entry.clone());
            guard.state.worktree_session = worktree_session;
            guard.persist_state(&self.inner.storage)?;
            self.inner.storage.mark_memory_index_dirty()?;
            Ok(())
        }
        .await;
        if let Err(error) = write_result {
            if let Some(occupancy_id) = new_occupancy_id.as_deref() {
                if previous_occupancy_id.as_deref() != Some(occupancy_id) {
                    let _ = self.release_workspace_occupancy(occupancy_id).await;
                }
            }
            if let Some(worktree) = worktree_cleanup_session.as_ref() {
                let _ = self.discard_managed_worktree(worktree).await;
            }
            return Err(error);
        }
        if let Some(previous_occupancy_id) = previous_occupancy_id.as_deref() {
            if new_occupancy_id.as_deref() != Some(previous_occupancy_id) {
                let _ = self
                    .release_workspace_occupancy(previous_occupancy_id)
                    .await?;
            }
        }
        let state = self.agent_state().await?;
        self.sync_effective_skill_roots_for_state(&state).await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "workspace_entered",
            serde_json::json!({
                "workspace_id": workspace.workspace_id,
                "workspace_anchor": workspace.workspace_anchor,
                "execution_root_id": execution_root_id,
                "execution_root": execution_root,
                "projection_kind": projection_kind,
                "access_mode": access_mode,
                "cwd": selected_cwd,
                "boundary": crate::system::HostLocalBoundary::from_parts(
                    &state.execution_profile,
                    Some(projection_kind),
                    Some(access_mode),
                    Some(execution_root_id),
                ).audit_metadata(),
            }),
        ))?;
        Ok(())
    }

    pub async fn enter_existing_git_worktree(
        &self,
        workspace: &WorkspaceEntry,
        worktree_root: PathBuf,
        access_mode: WorkspaceAccessMode,
        cwd: Option<PathBuf>,
    ) -> Result<()> {
        let agent_id = self.agent_id().await?;
        let existing_state = self.agent_state().await?;
        if !existing_state
            .attached_workspaces
            .iter()
            .any(|id| id == &workspace.workspace_id)
        {
            return Err(anyhow!(
                "workspace {} is not attached to agent {}",
                workspace.workspace_id,
                existing_state.id
            ));
        }
        crate::system::ensure_workspace_projection_allowed(
            &crate::system::HostLocalBoundary::from_parts(
                &existing_state.execution_profile,
                existing_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.projection_kind),
                existing_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode),
                existing_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.execution_root_id.clone()),
            ),
            WorkspaceProjectionKind::GitWorktreeRoot,
            "enter_existing_git_worktree",
        )?;

        let execution_root = crate::system::workspace::normalize_path(&worktree_root)?;
        let selected_cwd = resolve_enter_cwd(&execution_root, cwd.as_deref())?;
        let execution_root_id = Self::build_execution_root_id(
            &workspace.workspace_id,
            WorkspaceProjectionKind::GitWorktreeRoot,
            &execution_root,
        )?;
        let occupancy = self
            .acquire_workspace_occupancy(
                &workspace.workspace_id,
                &execution_root_id,
                &agent_id,
                access_mode,
            )
            .await?;
        let entry = ActiveWorkspaceEntry {
            workspace_id: workspace.workspace_id.clone(),
            workspace_anchor: workspace.workspace_anchor.clone(),
            execution_root_id: execution_root_id.clone(),
            execution_root: execution_root.clone(),
            projection_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            access_mode,
            cwd: selected_cwd.clone(),
            occupancy_id: occupancy.as_ref().map(|record| record.occupancy_id.clone()),
            projection_metadata: Some(WorkspaceProjectionMetadata::ExistingGitWorktree {
                worktree_root: execution_root.clone(),
            }),
        };
        let previous_occupancy_id = existing_state
            .active_workspace_entry
            .as_ref()
            .and_then(|existing_entry| existing_entry.occupancy_id.clone());
        let new_occupancy_id = entry.occupancy_id.clone();

        let write_result: Result<()> = async {
            let mut guard = self.inner.agent.lock().await;
            guard.state.active_workspace_entry = Some(entry.clone());
            guard.state.worktree_session = None;
            guard.persist_state(&self.inner.storage)?;
            self.inner.storage.mark_memory_index_dirty()?;
            Ok(())
        }
        .await;
        if let Err(error) = write_result {
            if let Some(occupancy_id) = new_occupancy_id.as_deref() {
                if previous_occupancy_id.as_deref() != Some(occupancy_id) {
                    let _ = self.release_workspace_occupancy(occupancy_id).await;
                }
            }
            return Err(error);
        }
        if let Some(previous_occupancy_id) = previous_occupancy_id.as_deref() {
            if new_occupancy_id.as_deref() != Some(previous_occupancy_id) {
                let _ = self
                    .release_workspace_occupancy(previous_occupancy_id)
                    .await?;
            }
        }
        let state = self.agent_state().await?;
        self.sync_effective_skill_roots_for_state(&state).await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "workspace_entered",
            serde_json::json!({
                "workspace_id": workspace.workspace_id,
                "workspace_anchor": workspace.workspace_anchor,
                "execution_root_id": execution_root_id,
                "execution_root": execution_root,
                "projection_kind": WorkspaceProjectionKind::GitWorktreeRoot,
                "access_mode": access_mode,
                "cwd": selected_cwd,
                "detected_kind": "existing_git_worktree",
                "ownership": "external",
                "boundary": crate::system::HostLocalBoundary::from_parts(
                    &state.execution_profile,
                    Some(WorkspaceProjectionKind::GitWorktreeRoot),
                    Some(access_mode),
                    Some(execution_root_id),
                ).audit_metadata(),
            }),
        ))?;
        Ok(())
    }

    pub async fn exit_workspace(&self) -> Result<()> {
        let state = self.agent_state().await?;
        let Some(active_entry) = state.active_workspace_entry.clone() else {
            return Err(anyhow!("agent has no active workspace entry"));
        };

        self.activate_agent_home(WorkspaceAccessMode::SharedRead, None)
            .await?;
        self.inner.storage.append_event(&AuditEvent::new(
            "workspace_exited",
            serde_json::json!({
                "workspace_id": active_entry.workspace_id,
                "execution_root_id": active_entry.execution_root_id,
                "projection_kind": active_entry.projection_kind,
                "access_mode": active_entry.access_mode,
                "boundary": crate::system::HostLocalBoundary::from_parts(
                    &state.execution_profile,
                    Some(active_entry.projection_kind),
                    Some(active_entry.access_mode),
                    Some(active_entry.execution_root_id.clone()),
                ).audit_metadata(),
            }),
        ))?;
        Ok(())
    }

    pub(super) async fn transition_to_sleep(&self, duration_ms: Option<u64>) -> Result<()> {
        self.transition_to_sleep_with_runnable_override(duration_ms, true)
            .await
    }

    pub(super) async fn transition_to_sleep_with_runnable_override(
        &self,
        duration_ms: Option<u64>,
        allow_runnable_work_override: bool,
    ) -> Result<()> {
        let sleeping_until = duration_ms.map(|duration_ms| {
            chrono::Utc::now()
                + chrono::Duration::milliseconds(i64::try_from(duration_ms).unwrap_or(i64::MAX))
        });
        if sleeping_until.is_none() && allow_runnable_work_override {
            if let Some((work_item, reason)) = self.indefinite_sleep_runnable_work()? {
                let state = {
                    let guard = self.inner.agent.lock().await;
                    self.inner.storage.append_event(&AuditEvent::new(
                        "scheduler_posture_decision",
                        serde_json::json!({
                            "boundary": "lifecycle_sleep",
                            "reason": "sleep_overridden_runnable_work",
                            "previous_status": guard.state.status,
                            "next_status": guard.state.status,
                            "evidence": [
                                format!("previous_run_id={:?}", guard.state.current_run_id),
                                "sleeping_until=None".to_string(),
                                format!("work_item_id={}", work_item.id),
                                format!("work_queue_reason={reason}"),
                                "work_item_scheduling_state=Runnable".to_string(),
                            ],
                        }),
                    ))?;
                    guard.state.clone()
                };
                self.emit_system_tick_from_work_queue(&work_item, reason)
                    .await?;
                self.append_state_changed_events(&state)?;
                return Ok(());
            }
        }
        let state = super::scheduler_executor::SchedulerDecisionExecutor::new(self)
            .transition_to_sleep(
                sleeping_until,
                super::scheduler_executor::SleepTransitionBoundary::LifecycleSleep,
            )
            .await?;
        self.append_state_changed_events(&state)?;
        if let (Some(duration_ms), Some(sleeping_until)) = (duration_ms, sleeping_until) {
            self.spawn_session_sleep_wake(duration_ms, sleeping_until);
        }
        Ok(())
    }

    fn indefinite_sleep_runnable_work(
        &self,
    ) -> Result<Option<(crate::types::WorkItemRecord, &'static str)>> {
        let projection = self.inner.storage.work_queue_prompt_projection()?;
        if let Some(current) = projection.current_runnable {
            return Ok(Some((current.work_item, "continue_active")));
        }
        Ok(projection
            .queued_runnable
            .into_iter()
            .next()
            .map(|queued| (queued.work_item, "queued_available")))
    }

    fn spawn_session_sleep_wake(
        &self,
        duration_ms: u64,
        sleeping_until: chrono::DateTime<chrono::Utc>,
    ) {
        let runtime = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(duration_ms)).await;
            let should_wake = {
                let guard = runtime.inner.agent.lock().await;
                guard.state.status == AgentStatus::Asleep
                    && guard.state.sleeping_until == Some(sleeping_until)
            };
            if !should_wake {
                return;
            }

            let mut message = MessageEnvelope::new(
                match runtime.agent_id().await {
                    Ok(agent_id) => agent_id,
                    Err(_) => return,
                },
                MessageKind::SystemTick,
                MessageOrigin::System {
                    subsystem: "sleep_duration".into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: format!("sleep duration elapsed after {duration_ms}ms"),
                },
            )
            .with_admission(
                MessageDeliverySurface::RuntimeSystem,
                AdmissionContext::RuntimeOwned,
            );
            message.metadata = Some(serde_json::json!({
                "sleep_wait": {
                    "duration_ms": duration_ms,
                    "sleeping_until": sleeping_until,
                }
            }));
            let _ = runtime.inner.storage.append_event(&AuditEvent::new(
                "system_tick_emitted",
                serde_json::json!({
                    "subsystem": "sleep_duration",
                    "sleep_wait": message
                        .metadata
                        .as_ref()
                        .and_then(|value| value.get("sleep_wait"))
                        .cloned(),
                }),
            ));
            let _ = runtime.enqueue(message).await;
        });
    }

    pub(super) async fn agent_id(&self) -> Result<String> {
        Ok(self.inner.agent.lock().await.state.id.clone())
    }
}

fn active_child_tasks<'a>(agent_id: &str, tasks: &'a [TaskRecord]) -> Vec<&'a TaskRecord> {
    tasks
        .iter()
        .filter(|task| {
            task.agent_id == agent_id && !task_state_reducer::is_terminal_task_status(&task.status)
        })
        .collect()
}

fn child_blocked_reason(
    agent_status: &AgentStatus,
    active_tasks: &[&TaskRecord],
) -> Option<ChildAgentBlockedReason> {
    let blocking_tasks = blocking_tasks(active_tasks);
    let reason = if blocking_tasks
        .iter()
        .any(|task| matches!(task.status, TaskStatus::Cancelling))
    {
        Some(ChildAgentBlockedReason::ManagedTaskCancelling)
    } else if blocking_tasks
        .iter()
        .any(|task| matches!(task.status, TaskStatus::Running))
    {
        Some(ChildAgentBlockedReason::ManagedTaskRunning)
    } else if blocking_tasks
        .iter()
        .any(|task| matches!(task.status, TaskStatus::Queued))
    {
        Some(ChildAgentBlockedReason::ManagedTaskQueued)
    } else {
        None
    };
    reason.or_else(|| {
        matches!(agent_status, AgentStatus::AwaitingTask)
            .then_some(ChildAgentBlockedReason::AwaitingManagedTask)
    })
}

fn build_child_agent_observability(
    agent: &AgentState,
    waiting_reason: Option<WaitingReason>,
    active_waiting_intent_count: usize,
    latest_tasks: &[TaskRecord],
    briefs: &[BriefRecord],
) -> ChildAgentObservabilitySnapshot {
    let active_tasks = active_child_tasks(&agent.id, latest_tasks);
    build_child_agent_observability_with_active_tasks(
        agent,
        waiting_reason,
        active_waiting_intent_count,
        &active_tasks,
        briefs,
    )
}

fn build_child_agent_observability_with_active_tasks(
    agent: &AgentState,
    waiting_reason: Option<WaitingReason>,
    active_waiting_intent_count: usize,
    active_tasks: &[&TaskRecord],
    briefs: &[BriefRecord],
) -> ChildAgentObservabilitySnapshot {
    let blocked_reason = child_blocked_reason(&agent.status, &active_tasks);
    let phase = if agent.last_turn_terminal.is_some()
        && agent.current_run_id.is_none()
        && agent.pending == 0
        && active_tasks.is_empty()
    {
        ChildAgentPhase::Terminal
    } else if blocked_reason.is_some() {
        ChildAgentPhase::Blocked
    } else if agent.current_run_id.is_some() || agent.pending > 0 {
        ChildAgentPhase::Running
    } else if waiting_reason.is_some()
        || active_waiting_intent_count > 0
        || matches!(
            agent.status,
            AgentStatus::Asleep | AgentStatus::Booting | AgentStatus::AwakeIdle
        )
    {
        ChildAgentPhase::Waiting
    } else {
        ChildAgentPhase::Waiting
    };

    ChildAgentObservabilitySnapshot {
        phase,
        blocked_reason,
        waiting_reason,
        current_work_item_id: agent
            .working_memory
            .current_working_memory
            .current_work_item_id
            .clone(),
        work_summary: agent
            .working_memory
            .current_working_memory
            .work_summary
            .clone(),
        last_progress_brief: briefs
            .iter()
            .rev()
            .find(|brief| brief.kind == BriefKind::Ack)
            .map(|brief| brief.text.clone()),
        last_result_brief: briefs
            .iter()
            .rev()
            .find(|brief| brief.kind.is_terminal())
            .map(|brief| brief.text.clone()),
    }
}

fn blocking_tasks<'a>(active_tasks: &[&'a TaskRecord]) -> Vec<&'a TaskRecord> {
    active_tasks
        .iter()
        .copied()
        .filter(|task| task.is_blocking())
        .collect()
}

fn blocking_task_count(active_tasks: &[&TaskRecord]) -> usize {
    blocking_tasks(active_tasks).len()
}

fn waiting_reason_blocker(reason: crate::types::WaitingReason) -> &'static str {
    match reason {
        crate::types::WaitingReason::AwaitingOperatorInput => "Waiting on operator input.",
        crate::types::WaitingReason::AwaitingExternalChange => "Waiting on an external change.",
        crate::types::WaitingReason::AwaitingTaskResult => "Waiting on a task result.",
        crate::types::WaitingReason::AwaitingTimer => "Waiting on a timer.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn task(id: &str, status: TaskStatus) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            agent_id: "child".into(),
            kind: TaskKind::CommandTask,
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        }
    }

    fn task_with_wait_policy(
        id: &str,
        status: TaskStatus,
        wait_policy: crate::types::TaskWaitPolicy,
    ) -> TaskRecord {
        let mut task = task(id, status);
        task.detail = Some(serde_json::json!({
            "wait_policy": wait_policy,
        }));
        task
    }

    #[test]
    fn child_blocked_reason_ignores_background_tasks() {
        let queued = task_with_wait_policy(
            "queued",
            TaskStatus::Queued,
            crate::types::TaskWaitPolicy::Background,
        );
        let running = task_with_wait_policy(
            "running",
            TaskStatus::Running,
            crate::types::TaskWaitPolicy::Background,
        );
        let cancelling = task_with_wait_policy(
            "cancelling",
            TaskStatus::Cancelling,
            crate::types::TaskWaitPolicy::Background,
        );
        let active = vec![&queued, &running, &cancelling];

        assert_eq!(
            child_blocked_reason(&AgentStatus::AwakeRunning, &active),
            None
        );

        assert_eq!(
            child_blocked_reason(&AgentStatus::AwaitingTask, &active),
            Some(ChildAgentBlockedReason::AwaitingManagedTask)
        );
    }

    #[test]
    fn idle_child_defaults_to_waiting_not_running() {
        let mut agent = AgentState::new("child");
        agent.status = AgentStatus::AwakeIdle;

        let snapshot = build_child_agent_observability(&agent, None, 0, &[], &[]);

        assert_eq!(snapshot.phase, ChildAgentPhase::Waiting);
    }

    #[test]
    fn background_only_tasks_do_not_mark_child_blocked() {
        let background = task_with_wait_policy(
            "background",
            TaskStatus::Running,
            crate::types::TaskWaitPolicy::Background,
        );
        let active = vec![&background];

        assert_eq!(
            child_blocked_reason(&AgentStatus::AwakeRunning, &active),
            None
        );

        let mut agent = AgentState::new("child");
        agent.status = AgentStatus::AwakeIdle;
        let snapshot = build_child_agent_observability(&agent, None, 0, &[background], &[]);

        assert_ne!(snapshot.phase, ChildAgentPhase::Blocked);
        assert_eq!(snapshot.blocked_reason, None);
    }

    #[test]
    fn storage_fallback_ignores_background_only_tasks_for_waiting_reason() {
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = AppStorage::new(temp.path().to_path_buf()).expect("storage");

        let mut agent = AgentState::new("child");
        agent.status = AgentStatus::AwakeIdle;
        storage.write_agent(&agent).expect("write agent");
        storage
            .append_task(&task_with_wait_policy(
                "background",
                TaskStatus::Running,
                crate::types::TaskWaitPolicy::Background,
            ))
            .expect("append task");

        let snapshot =
            RuntimeHandle::child_agent_observability_from_storage(&storage, &agent).expect("view");

        assert_eq!(snapshot.blocked_reason, None);
        assert_eq!(snapshot.waiting_reason, None);
        assert_ne!(snapshot.phase, ChildAgentPhase::Blocked);
    }

    // --- resolve_enter_cwd: path escape safety ---

    #[test]
    fn resolve_enter_cwd_none_returns_execution_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        let result = resolve_enter_cwd(&root, None).unwrap();
        assert_eq!(result, root);
    }

    #[test]
    fn resolve_enter_cwd_relative_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let nested = root.join("src").join("lib");
        std::fs::create_dir_all(&nested).unwrap();

        let result = resolve_enter_cwd(&root, Some(Path::new("src/lib"))).unwrap();
        assert_eq!(result, root.join("src/lib"));
    }

    #[test]
    fn resolve_enter_cwd_absolute_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let abs = root.join("src");
        let result = resolve_enter_cwd(&root, Some(&abs)).unwrap();
        assert_eq!(result, abs);
    }

    #[test]
    fn resolve_enter_cwd_relative_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        let result = resolve_enter_cwd(&root, Some(Path::new("../escape")));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes execution root"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_enter_cwd_absolute_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let result = resolve_enter_cwd(&root, Some(&outside));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes execution root"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_enter_cwd_dotdot_within_root_ok() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();

        // "a/b/../c" resolves to "a/c" which is still inside root
        let result = resolve_enter_cwd(&root, Some(Path::new("a/b/../c"))).unwrap();
        assert_eq!(result, root.join("a/b/../c"));
    }

    #[test]
    fn resolve_enter_cwd_multiple_dotdot_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        let result = resolve_enter_cwd(&root, Some(Path::new("../../escape")));
        assert!(result.is_err());
    }

    #[test]
    fn resolve_enter_cwd_exact_root_via_dotdot_ok() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        // "sub/.." resolves to root itself, which is valid
        let result = resolve_enter_cwd(&root, Some(Path::new("sub/.."))).unwrap();
        assert_eq!(result, root.join("sub/.."));
    }

    // --- workspace_paths_match ---

    #[test]
    fn workspace_paths_match_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace");
        std::fs::create_dir_all(&path).unwrap();
        assert!(workspace_paths_match(&path, &path));
    }

    #[test]
    fn workspace_paths_match_different() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert!(!workspace_paths_match(&a, &b));
    }

    #[test]
    fn workspace_paths_match_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(workspace_paths_match(&real, &link));
    }
}
