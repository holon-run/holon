use super::*;
use std::path::{Path, PathBuf};

use crate::runtime::closure::{derive_closure_decision, runtime_error_active, ClosureFacts};
use crate::storage::AppStorage;
use crate::types::{
    AgentTokenUsageSummary, BriefKind, ChildAgentBlockedReason, ChildAgentObservabilitySnapshot,
    ChildAgentPhase, DeliverySummaryRecord, TaskRecord, TaskStatus, TokenUsage, WaitingReason,
    WorkItemStatus, WorktreeSession,
};

const DELIVERY_SUMMARY_EVIDENCE_LIMIT: usize = 64;
const DELIVERY_SUMMARY_TRANSCRIPT_LIMIT: usize = 16;
const DELIVERY_SUMMARY_TEXT_FIELD_LIMIT: usize = 4_000;

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

impl RuntimeHandle {
    pub(super) async fn maybe_commit_turn_end_work_item_transition(
        &self,
    ) -> Result<Option<crate::types::WorkItemRecord>> {
        let (turn_index, work_item_id) = {
            let mut guard = self.inner.agent.lock().await;
            let turn_index = guard.state.turn_index;
            let work_item_id = guard.state.current_turn_work_item_id.take();
            self.inner.storage.write_agent(&guard.state)?;
            (turn_index, work_item_id)
        };

        let Some(work_item_id) = work_item_id else {
            return Ok(None);
        };

        let closure = self.current_closure_decision().await?;
        let Some(latest) = self.inner.storage.latest_work_item(&work_item_id)? else {
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

        let decision = resolve_turn_end_work_item_commit(&latest, &closure);
        let wrote_new_snapshot =
            latest.status != decision.status || latest.progress_note != decision.progress_note;
        let committed = if wrote_new_snapshot {
            let record = crate::types::WorkItemRecord {
                id: latest.id.clone(),
                agent_id: latest.agent_id.clone(),
                workspace_id: latest.workspace_id.clone(),
                parent_id: latest.parent_id.clone(),
                delivery_target: latest.delivery_target.clone(),
                status: decision.status,
                summary: latest.summary.clone(),
                progress_note: decision.progress_note.clone(),
                created_at: latest.created_at,
                updated_at: chrono::Utc::now(),
            };
            self.inner.storage.append_work_item(&record)?;
            self.inner.storage.append_event(&AuditEvent::new(
                "work_item_written",
                serde_json::json!({
                    "action": "turn_end_committed",
                    "record": record,
                }),
            ))?;
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
                "claimed_status": decision.claimed_status,
                "committed_status": committed.status,
                "decision_reason": decision.reason,
                "wrote_new_snapshot": wrote_new_snapshot,
                "closure": closure,
            }),
        ))?;
        if committed.status == WorkItemStatus::Completed
            && closure.outcome == crate::types::ClosureOutcome::Completed
        {
            self.maybe_generate_delivery_summary(&committed, turn_index)
                .await?;
        }
        Ok(Some(committed))
    }

    async fn maybe_generate_delivery_summary(
        &self,
        work_item: &crate::types::WorkItemRecord,
        turn_index: u64,
    ) -> Result<Option<DeliverySummaryRecord>> {
        if self
            .inner
            .storage
            .latest_delivery_summary(&work_item.id)?
            .is_some()
        {
            return Ok(None);
        }

        let evidence = self
            .delivery_summary_evidence(work_item, turn_index)
            .await?;
        let compact_evidence = compact_delivery_summary_evidence(evidence);
        let generated = self.generate_delivery_summary_text(&compact_evidence).await;
        let text = match generated {
            Ok(text) if !text.trim().is_empty() => text.trim().to_string(),
            Ok(_) => fallback_delivery_summary_text(&compact_evidence),
            Err(error) => {
                self.inner.storage.append_event(&AuditEvent::new(
                    "delivery_summary_generation_failed",
                    serde_json::json!({
                        "agent_id": work_item.agent_id,
                        "work_item_id": work_item.id,
                        "error": error.to_string(),
                    }),
                ))?;
                fallback_delivery_summary_text(&compact_evidence)
            }
        };
        if text.trim().is_empty() {
            return Ok(None);
        }

        let record = DeliverySummaryRecord::new(
            work_item.agent_id.clone(),
            work_item.id.clone(),
            text,
            Some(turn_index),
            Some(compact_evidence),
        );
        self.inner.storage.append_delivery_summary(&record)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "delivery_summary_created",
            serde_json::json!({
                "agent_id": record.agent_id,
                "work_item_id": record.work_item_id,
                "delivery_summary_id": record.id,
                "source_turn_index": record.source_turn_index,
            }),
        ))?;
        Ok(Some(record))
    }

    async fn delivery_summary_evidence(
        &self,
        work_item: &crate::types::WorkItemRecord,
        turn_index: u64,
    ) -> Result<serde_json::Value> {
        let work_item_snapshots = read_recent_matching(
            |limit| self.inner.storage.read_recent_work_items(limit),
            |record| record.id == work_item.id,
            DELIVERY_SUMMARY_EVIDENCE_LIMIT,
        )?;
        let work_plans = read_recent_matching(
            |limit| self.inner.storage.read_recent_work_plans(limit),
            |record| record.work_item_id == work_item.id,
            DELIVERY_SUMMARY_EVIDENCE_LIMIT,
        )?;
        let briefs = read_recent_matching(
            |limit| self.inner.storage.read_recent_briefs(limit),
            |record| record.work_item_id.as_deref() == Some(work_item.id.as_str()),
            DELIVERY_SUMMARY_EVIDENCE_LIMIT,
        )?;
        let tools = read_recent_matching(
            |limit| self.inner.storage.read_recent_tool_executions(limit),
            |record| record.work_item_id.as_deref() == Some(work_item.id.as_str()),
            DELIVERY_SUMMARY_EVIDENCE_LIMIT,
        )?;
        let transcript = self
            .inner
            .storage
            .read_recent_transcript(DELIVERY_SUMMARY_TRANSCRIPT_LIMIT)?;
        let terminal = {
            let guard = self.inner.agent.lock().await;
            guard
                .state
                .last_turn_terminal
                .clone()
                .filter(|record| record.turn_index == turn_index)
        };

        Ok(serde_json::json!({
            "work_item": work_item,
            "source_turn_index": turn_index,
            "work_item_snapshots": work_item_snapshots,
            "work_plans": work_plans,
            "briefs": briefs,
            "tool_executions": tools,
            "recent_transcript": transcript,
            "terminal": terminal,
        }))
    }

    async fn generate_delivery_summary_text(&self, evidence: &serde_json::Value) -> Result<String> {
        let prompt = format!(
            "{}\n\nEvidence JSON:\n{}",
            delivery_summary_prompt(),
            serde_json::to_string_pretty(evidence)?
        );
        let provider = self.current_provider().await;
        let response = provider
            .complete_turn(crate::provider::ProviderTurnRequest::plain(
                "Generate a final delivery summary from bounded runtime evidence.",
                vec![crate::provider::ConversationMessage::UserText(prompt)],
                Vec::new(),
            ))
            .await?;
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.total_input_tokens += response.input_tokens;
            guard.state.total_output_tokens += response.output_tokens;
            guard.state.total_model_rounds += 1;
            self.inner.storage.write_agent(&guard.state)?;
        }
        Ok(response
            .blocks
            .into_iter()
            .filter_map(|block| match block {
                crate::provider::ModelBlock::Text { text } => Some(text),
                crate::provider::ModelBlock::ToolUse { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string())
    }

    pub(crate) async fn closure_decision_for_state(
        &self,
        state: &AgentState,
        runtime_error_override: Option<bool>,
    ) -> Result<ClosureDecision> {
        let active_waiting_intents = self.active_waiting_intent_summaries().await?.len();
        let active_timers = self
            .inner
            .storage
            .latest_timer_records()?
            .into_iter()
            .filter(|timer| timer.status == TimerStatus::Active)
            .count();
        let runtime_error = match runtime_error_override {
            Some(value) => value,
            None => runtime_error_active(
                &self.inner.storage.read_recent_events(64)?,
                &self.inner.storage.read_recent_briefs(64)?,
            ),
        };
        Ok(derive_closure_decision(&ClosureFacts {
            runtime_error,
            awaiting_operator_input: false,
            active_blocking_tasks: self.blocking_active_task_count().await?,
            active_waiting_intents,
            active_timers,
            work_signal: self.current_work_reactivation_signal()?,
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
        let state_payload = to_json_value(state);
        self.inner.storage.append_event(&AuditEvent::new(
            "agent_state_changed",
            state_payload.clone(),
        ))?;
        self.inner
            .storage
            .append_event(&AuditEvent::new("session_state_changed", state_payload))?;
        Ok(())
    }

    pub async fn control(&self, action: ControlAction) -> Result<()> {
        {
            let mut guard = self.inner.agent.lock().await;
            match action {
                ControlAction::Pause => guard.state.status = AgentStatus::Paused,
                ControlAction::Resume => guard.state.status = AgentStatus::AwakeIdle,
                ControlAction::Stop => guard.state.status = AgentStatus::Stopped,
            }
            guard.state.current_run_id = None;
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.inner.storage.append_event(&AuditEvent::new(
            "control_applied",
            serde_json::json!({ "action": action }),
        ))?;
        self.inner.notify.notify_one();
        Ok(())
    }

    pub(crate) async fn request_service_shutdown(&self) -> Result<()> {
        self.inner
            .shutdown_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.storage.append_event(&AuditEvent::new(
            "runtime_service_shutdown_requested",
            serde_json::json!({}),
        ))?;
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

    pub(crate) async fn blocking_active_task_count(&self) -> Result<usize> {
        let state = self.agent_state().await?;
        let active_task_ids = state
            .active_task_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        Ok(self
            .latest_task_records()
            .await?
            .into_iter()
            .filter(|task| active_task_ids.contains(task.id.as_str()) && task.is_blocking())
            .count())
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
        let agent = self.agent_state().await?;
        let model = self.model_state_for(&agent);
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
        Ok(AgentSummary {
            identity,
            lifecycle: crate::types::AgentLifecycleHint::from_status(
                &agent.id,
                agent.status.clone(),
            ),
            agent,
            model,
            token_usage,
            closure,
            execution,
            active_workspace_occupancy,
            loaded_agents_md: (&loaded_agents_md).into(),
            skills,
            active_children,
            active_waiting_intents: self.active_waiting_intent_summaries().await?,
            active_external_triggers,
            recent_operator_notifications: self.recent_operator_notifications(10).await?,
            recent_brief_count: self.inner.storage.read_recent_briefs(50)?.len(),
            recent_event_count: self.inner.storage.read_recent_events(100)?.len(),
        })
    }

    pub async fn recent_briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        self.inner.storage.read_recent_briefs(limit)
    }

    pub async fn recent_events(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        self.inner.storage.read_recent_events(limit)
    }

    pub async fn recent_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        self.inner.storage.read_recent_tasks(limit)
    }

    pub async fn recent_transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        self.inner.storage.read_recent_transcript(limit)
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
            self.active_waiting_intent_summaries().await?.len(),
            &latest_tasks,
            &briefs,
        ))
    }

    pub(crate) fn child_agent_observability_from_storage(
        storage: &AppStorage,
        state: &AgentState,
    ) -> Result<ChildAgentObservabilitySnapshot> {
        let latest_tasks = storage.latest_task_records()?;
        let active_tasks = active_child_tasks(&state.active_task_ids, &latest_tasks);
        let active_waiting_intent_count = storage
            .latest_waiting_intents()?
            .into_iter()
            .filter(|record| record.status == WaitingIntentStatus::Active)
            .count();
        let active_timers = storage
            .latest_timer_records()?
            .into_iter()
            .filter(|timer| timer.status == TimerStatus::Active)
            .count();
        let closure = derive_closure_decision(&ClosureFacts {
            runtime_error: runtime_error_active(
                &storage.read_recent_events(64)?,
                &storage.read_recent_briefs(64)?,
            ),
            awaiting_operator_input: false,
            active_blocking_tasks: blocking_task_count(&active_tasks),
            active_waiting_intents: active_waiting_intent_count,
            active_timers,
            work_signal: super::memory_refresh::work_queue_reactivation_signal(
                &storage.work_queue_prompt_projection()?,
            ),
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
            active_waiting_intent_count,
            &active_tasks,
            &briefs,
        ))
    }

    pub async fn latest_work_items(&self) -> Result<Vec<crate::types::WorkItemRecord>> {
        self.inner.storage.latest_work_items()
    }

    pub async fn latest_work_item(
        &self,
        work_item_id: &str,
    ) -> Result<Option<crate::types::WorkItemRecord>> {
        self.inner.storage.latest_work_item(work_item_id)
    }

    pub async fn search_memory(
        &self,
        query: &str,
        limit: usize,
        include_all_workspaces: bool,
    ) -> Result<Vec<crate::memory::MemorySearchResult>> {
        let active_workspace_id = self
            .agent_state()
            .await?
            .active_workspace_entry
            .map(|entry| entry.workspace_id);
        let storage = self.inner.storage.clone();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            crate::memory::search_memory(
                &storage,
                &query,
                limit,
                active_workspace_id.as_deref(),
                include_all_workspaces,
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

    pub async fn latest_work_plan(
        &self,
        work_item_id: &str,
    ) -> Result<Option<crate::types::WorkPlanSnapshot>> {
        self.inner.storage.latest_work_plan(work_item_id)
    }

    pub async fn recent_timers(&self, limit: usize) -> Result<Vec<TimerRecord>> {
        self.inner.storage.read_recent_timers(limit)
    }

    pub async fn set_model_override(
        &self,
        model_override: crate::config::ModelRef,
    ) -> Result<crate::types::AgentModelState> {
        let mut next_state = self.agent_state().await?;
        next_state.model_override = Some(model_override.clone());
        self.reconfigure_provider_for_state(&next_state).await?;

        let model_state = self.model_state_for(&next_state);
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.model_override = Some(model_override);
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.append_audit_event(
            "agent_model_override_set",
            serde_json::json!({
                "agent_id": next_state.id,
                "model": model_state,
            }),
        )?;
        Ok(model_state)
    }

    pub async fn clear_model_override(&self) -> Result<crate::types::AgentModelState> {
        let mut next_state = self.agent_state().await?;
        next_state.model_override = None;
        self.reconfigure_provider_for_state(&next_state).await?;

        let model_state = self.model_state_for(&next_state);
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.model_override = None;
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.append_audit_event(
            "agent_model_override_cleared",
            serde_json::json!({
                "agent_id": next_state.id,
                "model": model_state,
            }),
        )?;
        Ok(model_state)
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
        self.inner.storage.write_agent(&guard.state)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "workspace_attached",
            serde_json::json!({
                "workspace_id": workspace.workspace_id,
                "workspace_anchor": workspace.workspace_anchor,
            }),
        ))?;
        Ok(())
    }

    pub async fn detach_workspace(&self, workspace_id: &str) -> Result<()> {
        let workspace_id = workspace_id.trim();
        if workspace_id.is_empty() {
            return Err(anyhow!("workspace_id is required"));
        }
        if workspace_id == AGENT_HOME_WORKSPACE_ID {
            return Err(anyhow!("AgentHome cannot be detached"));
        }

        let detached_agent_id = {
            let mut guard = self.inner.agent.lock().await;
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
            self.inner.storage.write_agent(&guard.state)?;
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
            format!("ws-{}", Uuid::new_v4().simple()),
            normalized_anchor.clone(),
            normalized_anchor
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToString::to_string),
        );
        Ok(workspace)
    }

    pub(crate) async fn workspace_entry_for_use(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspaceEntry>> {
        if workspace_id == AGENT_HOME_WORKSPACE_ID {
            return Ok(Some(Self::agent_home_workspace_entry(
                self.inner.storage.data_dir(),
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
        let workspace = Self::agent_home_workspace_entry(self.inner.storage.data_dir());
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
            if !guard
                .state
                .attached_workspaces
                .iter()
                .any(|id| id == AGENT_HOME_WORKSPACE_ID)
            {
                guard
                    .state
                    .attached_workspaces
                    .push(AGENT_HOME_WORKSPACE_ID.to_string());
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
            self.inner.storage.write_agent(&guard.state)?;
            self.inner.storage.mark_memory_index_dirty()?;
        }
        if let Some(occupancy_id) = previous_occupancy_id.as_deref() {
            if let Some(bridge) = self.inner.host_bridge.as_ref() {
                let _ = bridge.release_workspace_occupancy(occupancy_id).await?;
            }
        }
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
        if projection_kind == WorkspaceProjectionKind::GitWorktreeRoot
            && existing_state
                .active_workspace_entry
                .as_ref()
                .is_some_and(|entry| {
                    entry.projection_kind == WorkspaceProjectionKind::GitWorktreeRoot
                })
        {
            return Err(anyhow!(
                "agent {} is already using an isolated execution root; use UseWorkspace with a direct workspace before creating another isolated root",
                agent_id
            ));
        }
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
                let metadata = serde_json::json!({
                    "original_cwd": session.original_cwd,
                    "original_branch": session.original_branch,
                    "worktree_path": session.worktree_path,
                    "worktree_branch": session.worktree_branch,
                });
                (session.worktree_path.clone(), Some(session), Some(metadata))
            }
        };
        let selected_cwd = resolve_enter_cwd(&execution_root, cwd.as_deref())?;
        let execution_root_id = Self::build_execution_root_id(
            &workspace.workspace_id,
            projection_kind,
            &execution_root,
        )?;
        let occupancy = if let Some(bridge) = self.inner.host_bridge.as_ref() {
            bridge
                .acquire_workspace_occupancy(
                    &workspace.workspace_id,
                    &execution_root_id,
                    &agent_id,
                    access_mode,
                )
                .await?
        } else {
            None
        };
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
            self.inner.storage.write_agent(&guard.state)?;
            self.inner.storage.mark_memory_index_dirty()?;
            Ok(())
        }
        .await;
        if let Err(error) = write_result {
            if let Some(occupancy_id) = new_occupancy_id.as_deref() {
                if previous_occupancy_id.as_deref() != Some(occupancy_id) {
                    if let Some(bridge) = self.inner.host_bridge.as_ref() {
                        let _ = bridge.release_workspace_occupancy(occupancy_id).await;
                    }
                }
            }
            if let Some(worktree) = worktree_cleanup_session.as_ref() {
                let _ = self.discard_managed_worktree(worktree).await;
            }
            return Err(error);
        }
        if let Some(previous_occupancy_id) = previous_occupancy_id.as_deref() {
            if new_occupancy_id.as_deref() != Some(previous_occupancy_id) {
                if let Some(bridge) = self.inner.host_bridge.as_ref() {
                    let _ = bridge
                        .release_workspace_occupancy(previous_occupancy_id)
                        .await?;
                }
            }
        }
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
                    &self.agent_state().await?.execution_profile,
                    Some(projection_kind),
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
        let sleeping_until = duration_ms.map(|duration_ms| {
            chrono::Utc::now()
                + chrono::Duration::milliseconds(i64::try_from(duration_ms).unwrap_or(i64::MAX))
        });
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.status = AgentStatus::Asleep;
            guard.state.current_run_id = None;
            guard.state.sleeping_until = sleeping_until;
            self.inner.storage.write_agent(&guard.state)?;
            self.append_state_changed_events(&guard.state)?;
        }
        if let (Some(duration_ms), Some(sleeping_until)) = (duration_ms, sleeping_until) {
            self.spawn_session_sleep_wake(duration_ms, sleeping_until);
        }
        Ok(())
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
                TrustLevel::TrustedSystem,
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

    pub(super) async fn persist_brief(&self, brief: &BriefRecord) -> Result<()> {
        let mut bound_brief = brief.clone();
        {
            let guard = self.inner.agent.lock().await;
            bound_brief.workspace_id = guard
                .state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_id.clone())
                .unwrap_or_else(|| crate::types::AGENT_HOME_WORKSPACE_ID.to_string());
            if bound_brief.work_item_id.is_none() {
                bound_brief.work_item_id = guard.state.current_turn_work_item_id.clone();
            }
        }
        self.inner.storage.append_brief(&bound_brief)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "brief_created",
            to_json_value(&bound_brief),
        ))?;
        let mut guard = self.inner.agent.lock().await;
        guard.state.last_brief_at = Some(bound_brief.created_at);
        self.inner.storage.write_agent(&guard.state)?;
        Ok(())
    }

    pub(super) async fn agent_id(&self) -> Result<String> {
        Ok(self.inner.agent.lock().await.state.id.clone())
    }
}

fn active_child_tasks<'a>(
    active_task_ids: &[String],
    tasks: &'a [TaskRecord],
) -> Vec<&'a TaskRecord> {
    tasks
        .iter()
        .filter(|task| active_task_ids.iter().any(|task_id| task_id == &task.id))
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
    let active_tasks = active_child_tasks(&agent.active_task_ids, latest_tasks);
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
        && agent.active_task_ids.is_empty()
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
        active_work_item_id: agent
            .working_memory
            .current_working_memory
            .active_work_item_id
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TurnEndWorkItemCommitDecision {
    claimed_status: crate::types::WorkItemStatus,
    status: crate::types::WorkItemStatus,
    progress_note: Option<String>,
    reason: &'static str,
}

fn resolve_turn_end_work_item_commit(
    latest: &crate::types::WorkItemRecord,
    closure: &ClosureDecision,
) -> TurnEndWorkItemCommitDecision {
    use crate::types::{ClosureOutcome, WorkItemStatus};

    let waiting_note = closure
        .waiting_reason
        .map(waiting_reason_progress_note)
        .map(str::to_string);
    let fallback_waiting = || TurnEndWorkItemCommitDecision {
        claimed_status: latest.status.clone(),
        status: WorkItemStatus::Waiting,
        progress_note: latest
            .progress_note
            .clone()
            .or_else(|| waiting_note.clone()),
        reason: "runtime_waiting_condition",
    };
    let failure_waiting = || TurnEndWorkItemCommitDecision {
        claimed_status: latest.status.clone(),
        status: WorkItemStatus::Waiting,
        progress_note: latest.progress_note.clone().or_else(|| {
            Some("Turn failed and requires operator intervention before continuing.".into())
        }),
        reason: "runtime_failed_turn",
    };

    match latest.status {
        WorkItemStatus::Active => {
            if closure.outcome == ClosureOutcome::Failed {
                failure_waiting()
            } else if closure.waiting_reason.is_some() {
                fallback_waiting()
            } else {
                TurnEndWorkItemCommitDecision {
                    claimed_status: WorkItemStatus::Active,
                    status: WorkItemStatus::Active,
                    progress_note: latest.progress_note.clone(),
                    reason: "default_remain_active",
                }
            }
        }
        WorkItemStatus::Waiting => {
            if latest
                .progress_note
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty())
                || latest
                    .summary
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|value| !value.is_empty())
                || closure.waiting_reason.is_some()
            {
                TurnEndWorkItemCommitDecision {
                    claimed_status: WorkItemStatus::Waiting,
                    status: WorkItemStatus::Waiting,
                    progress_note: latest.progress_note.clone().or_else(|| waiting_note),
                    reason: "accepted_waiting_claim",
                }
            } else {
                TurnEndWorkItemCommitDecision {
                    claimed_status: WorkItemStatus::Waiting,
                    status: WorkItemStatus::Active,
                    progress_note: latest.progress_note.clone(),
                    reason: "waiting_requires_reason",
                }
            }
        }
        WorkItemStatus::Queued => TurnEndWorkItemCommitDecision {
            claimed_status: WorkItemStatus::Queued,
            status: WorkItemStatus::Queued,
            progress_note: latest.progress_note.clone(),
            reason: "accepted_queued_claim",
        },
        WorkItemStatus::Completed => {
            if closure.outcome == ClosureOutcome::Completed {
                TurnEndWorkItemCommitDecision {
                    claimed_status: WorkItemStatus::Completed,
                    status: WorkItemStatus::Completed,
                    progress_note: latest.progress_note.clone(),
                    reason: "accepted_completed_claim",
                }
            } else if closure.outcome == ClosureOutcome::Failed {
                failure_waiting()
            } else if closure.waiting_reason.is_some() {
                fallback_waiting()
            } else {
                TurnEndWorkItemCommitDecision {
                    claimed_status: WorkItemStatus::Completed,
                    status: WorkItemStatus::Active,
                    progress_note: latest.progress_note.clone(),
                    reason: "completed_rejected_by_runtime_facts",
                }
            }
        }
    }
}

fn waiting_reason_progress_note(reason: crate::types::WaitingReason) -> &'static str {
    match reason {
        crate::types::WaitingReason::AwaitingOperatorInput => "Waiting on operator input.",
        crate::types::WaitingReason::AwaitingExternalChange => "Waiting on an external change.",
        crate::types::WaitingReason::AwaitingTaskResult => "Waiting on a task result.",
        crate::types::WaitingReason::AwaitingTimer => "Waiting on a timer.",
    }
}

fn delivery_summary_prompt() -> &'static str {
    "Generate a final delivery summary for the whole work item, not just the latest turn.\n\
Use only the evidence below. Include what was changed, answered, or delivered; verification or evidence; and remaining risks or follow-up, if any.\n\
If later turns only fixed warnings or polish, mention them as follow-up fixes without replacing the main delivery.\n\
Return only the final summary text."
}

fn read_recent_matching<T, ReadFn, MatchFn>(
    mut read_recent: ReadFn,
    mut matches: MatchFn,
    per_match_limit: usize,
) -> Result<Vec<T>>
where
    ReadFn: FnMut(usize) -> Result<Vec<T>>,
    MatchFn: FnMut(&T) -> bool,
{
    let mut read_limit = per_match_limit.max(1);
    loop {
        let records = read_recent(read_limit)?;
        let total_records = records.len();
        let mut matching = records
            .into_iter()
            .filter(|record| matches(record))
            .collect::<Vec<_>>();
        if matching.len() >= per_match_limit || total_records < read_limit {
            if matching.len() > per_match_limit {
                matching.drain(0..(matching.len() - per_match_limit));
            }
            return Ok(matching);
        }
        let next_limit = read_limit.saturating_mul(2);
        if next_limit == read_limit {
            return Ok(matching);
        }
        read_limit = next_limit;
    }
}

fn compact_delivery_summary_evidence(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            if text.chars().count() <= DELIVERY_SUMMARY_TEXT_FIELD_LIMIT {
                serde_json::Value::String(text)
            } else {
                let truncated = text
                    .chars()
                    .take(DELIVERY_SUMMARY_TEXT_FIELD_LIMIT)
                    .collect::<String>();
                serde_json::Value::String(format!(
                    "{truncated}\n...[truncated for delivery summary evidence]"
                ))
            }
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(compact_delivery_summary_evidence)
                .collect(),
        ),
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key, compact_delivery_summary_evidence(value)))
                .collect(),
        ),
        other => other,
    }
}

fn fallback_delivery_summary_text(evidence: &serde_json::Value) -> String {
    evidence
        .get("briefs")
        .and_then(|briefs| briefs.as_array())
        .and_then(|briefs| {
            briefs.iter().rev().find_map(|brief| {
                let kind = brief.get("kind").and_then(|value| value.as_str())?;
                if kind != "result" {
                    return None;
                }
                brief
                    .get("text")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string)
            })
        })
        .or_else(|| {
            evidence
                .get("work_item")
                .and_then(|item| item.get("progress_note").or_else(|| item.get("summary")))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            evidence
                .get("terminal")
                .and_then(|terminal| terminal.get("last_assistant_message"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_default()
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
    fn read_recent_matching_expands_before_filtering() -> Result<()> {
        let records = (0..100usize).collect::<Vec<_>>();
        let matching = read_recent_matching(
            |limit| {
                let start = records.len().saturating_sub(limit);
                Ok(records[start..].to_vec())
            },
            |record| record % 10 == 0,
            3,
        )?;

        assert_eq!(matching, vec![70, 80, 90]);
        Ok(())
    }

    #[test]
    fn compact_delivery_summary_evidence_truncates_large_strings() {
        let evidence = serde_json::json!({
            "tool_executions": [{
                "summary": "command output",
                "output": {
                    "stdout": "x".repeat(DELIVERY_SUMMARY_TEXT_FIELD_LIMIT + 100)
                }
            }]
        });

        let compact = compact_delivery_summary_evidence(evidence);
        let stdout = compact["tool_executions"][0]["output"]["stdout"]
            .as_str()
            .expect("stdout remains a string");
        assert!(stdout.len() < DELIVERY_SUMMARY_TEXT_FIELD_LIMIT + 100);
        assert!(stdout.contains("truncated for delivery summary evidence"));
    }

    #[test]
    fn child_blocked_reason_prioritizes_cancelling_then_running_then_queued() {
        let queued = task_with_wait_policy(
            "queued",
            TaskStatus::Queued,
            crate::types::TaskWaitPolicy::Blocking,
        );
        let running = task_with_wait_policy(
            "running",
            TaskStatus::Running,
            crate::types::TaskWaitPolicy::Blocking,
        );
        let cancelling = task_with_wait_policy(
            "cancelling",
            TaskStatus::Cancelling,
            crate::types::TaskWaitPolicy::Blocking,
        );
        let active = vec![&queued, &running, &cancelling];

        assert_eq!(
            child_blocked_reason(&AgentStatus::AwakeRunning, &active),
            Some(ChildAgentBlockedReason::ManagedTaskCancelling)
        );

        let active = vec![&queued, &running];
        assert_eq!(
            child_blocked_reason(&AgentStatus::AwakeRunning, &active),
            Some(ChildAgentBlockedReason::ManagedTaskRunning)
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
        agent.active_task_ids = vec!["background".into()];
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
        agent.active_task_ids = vec!["background".into()];
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
}
