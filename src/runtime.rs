mod bootstrap;
mod callback;
mod closure;
mod command_task;
mod continuation;
mod failure;
mod lifecycle;
mod memory_refresh;
mod message_dispatch;
mod operator;
mod operator_dispatch;
mod provider_turn;
mod subagent;
mod task_state_reducer;
mod tasks;
#[cfg(test)]
mod test_util;
mod turn;
mod workspace;
mod worktree;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use bootstrap::ProviderReconfigurator;
use chrono::Utc;
use serde_json::Value;
use tokio::sync::{Mutex, Notify, RwLock};
use tracing::{error, info};
use uuid::Uuid;

#[cfg(test)]
use crate::provider::{ConversationMessage, ProviderTurnRequest};
use crate::{
    agents_md::load_agents_md,
    brief,
    config::RuntimeModelCatalog,
    context::{maybe_compact_agent, ContextConfig},
    host::RuntimeHostBridge,
    ingress::{WakeDisposition, WakeHint},
    memory::{mark_working_memory_prompted, refresh_episode_memory, refresh_working_memory},
    prompt::{build_effective_prompt, EffectivePrompt},
    provider::{provider_attempt_timeline, AgentProvider, ModelBlock},
    queue::RuntimeQueue,
    skills::{find_skill_by_entrypoint, load_skills_runtime_view, SkillVisibility},
    storage::{to_json_value, AppStorage, PollActivityMarker},
    system::{
        EffectiveExecution, ExecutionScopeKind, ExecutionSnapshot, LocalSystem,
        WorkspaceAccessMode, WorkspaceProjectionKind, WorkspaceView,
    },
    tool::ToolRegistry,
    types::{
        ActiveWorkspaceEntry, AdmissionContext, AgentIdentityView, AgentKind, AgentState,
        AgentStatus, AgentSummary, AuditEvent, BriefRecord, CallbackDeliveryMode,
        CallbackDeliveryPayload, CallbackDeliveryResult, CallbackIngressDisposition,
        CancelWaitingResult, ClosureDecision, ContinuationResolution, ControlAction,
        ExternalTriggerCapability, ExternalTriggerRecord, ExternalTriggerStatus,
        ExternalTriggerSummary, LoadedAgentsMd, MessageBody, MessageDeliverySurface,
        MessageEnvelope, MessageKind, MessageOrigin, PendingWakeHint, Priority, QueueEntryRecord,
        QueueEntryStatus, ResolvedModelAvailability, RuntimeFailurePhase, RuntimeFailureSummary,
        RuntimePosture, SkillActivationSource, SkillActivationState, SkillsRuntimeView, TaskKind,
        TaskRecord, TaskRecoverySpec, TaskStatus, TimerRecord, TimerStatus, ToolExecutionRecord,
        TranscriptEntry, TranscriptEntryKind, TrustLevel, WaitingIntentRecord, WaitingIntentStatus,
        WaitingIntentSummary, WorkspaceEntry, AGENT_HOME_WORKSPACE_ID,
    },
};
use command_task::ManagedTaskHandle;
use continuation::{resolve_continuation, ContinuationTrigger};
#[cfg(test)]
use subagent::sanitize_subagent_result;
use turn::LoopControlOptions;

#[derive(Debug, Clone)]
struct WorktreeSubagentResult {
    text: String,
    worktree_path: PathBuf,
    worktree_branch: String,
    changed_files: Vec<String>,
    failed: bool,
}

#[derive(Debug, Clone)]
pub struct ManagedWorktreeSeed {
    pub original_cwd: PathBuf,
    pub original_branch: String,
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
}

#[derive(Debug, Clone)]
pub enum InitialWorkspaceBinding {
    Detached,
    Anchor(PathBuf),
    Entry(WorkspaceEntry),
}

impl From<PathBuf> for InitialWorkspaceBinding {
    fn from(value: PathBuf) -> Self {
        Self::Anchor(value)
    }
}

impl From<WorkspaceEntry> for InitialWorkspaceBinding {
    fn from(value: WorkspaceEntry) -> Self {
        Self::Entry(value)
    }
}

impl From<Option<WorkspaceEntry>> for InitialWorkspaceBinding {
    fn from(value: Option<WorkspaceEntry>) -> Self {
        match value {
            Some(value) => Self::Entry(value),
            None => Self::Detached,
        }
    }
}

#[derive(Clone)]
pub struct RuntimeHandle {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    agent: Mutex<RuntimeAgent>,
    notify: Notify,
    storage: AppStorage,
    provider: RwLock<Arc<dyn AgentProvider>>,
    provider_reconfig: Option<ProviderReconfigurator>,
    model_catalog: RuntimeModelCatalog,
    model_availability: Vec<ResolvedModelAvailability>,
    base_context_config: ContextConfig,
    context_config: RwLock<ContextConfig>,
    callback_base_url: String,
    tools: ToolRegistry,
    system: Arc<LocalSystem>,
    default_agent_id: String,
    host_bridge: Option<RuntimeHostBridge>,
    task_handles: Mutex<HashMap<String, ManagedTaskHandle>>,
    recovered_tasks: Mutex<Option<Vec<TaskRecord>>>,
    recovered_timers: Mutex<Option<Vec<TimerRecord>>>,
    suppress_next_continue_active_tick: Mutex<bool>,
    shutdown_requested: AtomicBool,
}

#[derive(Debug)]
struct RuntimeAgent {
    state: AgentState,
    queue: RuntimeQueue,
}

impl RuntimeHandle {
    fn build_execution_root_id(
        workspace_id: &str,
        projection_kind: WorkspaceProjectionKind,
        execution_root: &Path,
    ) -> Result<String> {
        workspace::build_execution_root_id(workspace_id, projection_kind, execution_root)
    }

    fn agent_home_workspace_entry(data_dir: &Path) -> crate::types::WorkspaceEntry {
        workspace::agent_home_workspace_entry(data_dir)
    }

    pub fn storage(&self) -> &AppStorage {
        &self.inner.storage
    }

    pub fn poll_activity_marker(&self) -> Result<PollActivityMarker> {
        self.inner.storage.poll_activity_marker()
    }

    pub fn all_events(&self) -> Result<Vec<AuditEvent>> {
        self.inner.storage.read_recent_events(usize::MAX)
    }

    pub fn all_messages(&self) -> Result<Vec<MessageEnvelope>> {
        self.inner.storage.read_all_messages()
    }

    pub fn all_tool_executions(&self) -> Result<Vec<ToolExecutionRecord>> {
        self.inner.storage.read_recent_tool_executions(usize::MAX)
    }

    pub fn latest_task_records_snapshot(&self) -> Result<Vec<TaskRecord>> {
        let mut tasks = self.inner.storage.latest_task_records()?;
        tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(tasks)
    }

    pub(crate) fn agent_home(&self) -> PathBuf {
        self.inner.storage.data_dir().to_path_buf()
    }

    pub fn workspace_root(&self) -> PathBuf {
        self.execution_root_sync()
    }

    pub(crate) fn system(&self) -> Arc<LocalSystem> {
        self.inner.system.clone()
    }

    fn user_home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn fallback_identity_view(&self, agent_id: &str) -> AgentIdentityView {
        let kind = if agent_id == self.inner.default_agent_id {
            AgentKind::Default
        } else {
            AgentKind::Named
        };
        AgentIdentityView {
            agent_id: agent_id.to_string(),
            kind,
            visibility: crate::types::AgentVisibility::Public,
            ownership: crate::types::AgentOwnership::SelfOwned,
            profile_preset: crate::types::AgentProfilePreset::PublicNamed,
            status: crate::types::AgentRegistryStatus::Active,
            is_default_agent: agent_id == self.inner.default_agent_id,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        }
    }

    pub(crate) async fn agent_identity_view(&self) -> Result<AgentIdentityView> {
        let agent_id = self.agent_id().await?;
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            if let Some(identity) = bridge.identity_for_agent(&agent_id).await? {
                return Ok(AgentIdentityView::from_record(
                    &identity,
                    &self.inner.default_agent_id,
                ));
            }
        }
        Ok(self.fallback_identity_view(&agent_id))
    }

    fn skill_visibility(&self, identity: &AgentIdentityView) -> SkillVisibility {
        if identity.kind == AgentKind::Default {
            SkillVisibility::DefaultAgent
        } else {
            SkillVisibility::NonDefaultAgent
        }
    }

    pub(crate) async fn inherit_from_parent_state(&self, parent_state: &AgentState) -> Result<()> {
        let next_state = {
            let guard = self.inner.agent.lock().await;
            let mut next_state = guard.state.clone();
            next_state.attached_workspaces = parent_state.attached_workspaces.clone();
            next_state.active_workspace_entry = parent_state.active_workspace_entry.clone();
            next_state.worktree_session = parent_state.worktree_session.clone();
            next_state.execution_profile = parent_state.execution_profile.clone();
            next_state.model_override = parent_state.model_override.clone();
            next_state
        };
        if self.inner.provider_reconfig.is_some() {
            self.reconfigure_provider_for_state(&next_state).await?;
        }
        let mut guard = self.inner.agent.lock().await;
        guard.state = next_state;
        self.inner.storage.write_agent(&guard.state)?;
        Ok(())
    }

    pub(crate) async fn workspace_view(&self) -> Result<WorkspaceView> {
        let guard = self.inner.agent.lock().await;
        self.workspace_view_from_state(&guard.state)
    }

    pub(crate) fn workspace_view_for_root(
        &self,
        execution_root: PathBuf,
        cwd: PathBuf,
        worktree_root: Option<PathBuf>,
    ) -> Result<WorkspaceView> {
        workspace::workspace_view_for_root(&self.inner.storage, execution_root, cwd, worktree_root)
    }

    fn workspace_view_from_state(&self, state: &AgentState) -> Result<WorkspaceView> {
        workspace::workspace_view_from_state(state, self.inner.storage.data_dir().to_path_buf())
    }

    fn execution_snapshot_for_view(
        &self,
        profile: crate::system::ExecutionProfile,
        workspace: &WorkspaceView,
        attached_workspace_ids: &[String],
    ) -> ExecutionSnapshot {
        workspace::execution_snapshot_for_view(
            profile,
            workspace,
            attached_workspace_ids,
            &self.inner.storage,
        )
    }

    fn workspace_anchor_for_state_ref<'a>(&self, state: &'a AgentState) -> Option<&'a Path> {
        workspace::workspace_anchor_for_state_ref(state)
    }

    fn execution_root_sync(&self) -> PathBuf {
        workspace::execution_root_sync(&self.inner.storage)
    }

    pub(crate) async fn effective_execution(
        &self,
        scope: ExecutionScopeKind,
    ) -> Result<EffectiveExecution> {
        let guard = self.inner.agent.lock().await;
        let profile = guard.state.execution_profile.clone();
        let attached_workspace_ids = guard.state.attached_workspaces.clone();
        drop(guard);
        let workspace = self.workspace_view().await?;
        Ok(workspace::build_effective_execution(
            &self.inner.storage,
            scope,
            profile,
            workspace,
            &attached_workspace_ids,
        ))
    }

    pub(crate) async fn effective_execution_for_workspace(
        &self,
        scope: ExecutionScopeKind,
        workspace: WorkspaceView,
    ) -> Result<EffectiveExecution> {
        let guard = self.inner.agent.lock().await;
        let profile = guard.state.execution_profile.clone();
        let attached_workspace_ids = guard.state.attached_workspaces.clone();
        drop(guard);
        Ok(workspace::build_effective_execution(
            &self.inner.storage,
            scope,
            profile,
            workspace,
            &attached_workspace_ids,
        ))
    }

    pub async fn execution_snapshot(&self) -> Result<ExecutionSnapshot> {
        Ok(self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?
            .snapshot())
    }

    pub(crate) async fn loaded_agents_md(&self) -> Result<LoadedAgentsMd> {
        let guard = self.inner.agent.lock().await;
        self.loaded_agents_md_for_state(&guard.state)
    }

    fn loaded_agents_md_for_state(&self, state: &AgentState) -> Result<LoadedAgentsMd> {
        load_agents_md(
            self.agent_home().as_path(),
            self.workspace_anchor_for_state_ref(state),
        )
    }

    pub(crate) async fn skills_runtime_view(
        &self,
        identity: &AgentIdentityView,
    ) -> Result<SkillsRuntimeView> {
        let guard = self.inner.agent.lock().await;
        self.skills_runtime_view_for_state(&guard.state, identity)
    }

    fn skills_runtime_view_for_state(
        &self,
        state: &AgentState,
        identity: &AgentIdentityView,
    ) -> Result<SkillsRuntimeView> {
        load_skills_runtime_view(
            self.skill_visibility(identity),
            self.user_home().as_deref(),
            self.agent_home().as_path(),
            state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_anchor.as_path()),
            &state.active_skills,
        )
    }

    async fn begin_interactive_turn(
        &self,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
    ) -> Result<()> {
        let mut guard = self.inner.agent.lock().await;
        guard.state.turn_index += 1;
        guard.state.last_turn_terminal = None;
        guard.state.current_turn_work_item_id = self
            .inner
            .storage
            .work_queue_prompt_projection()?
            .active
            .map(|item| item.id);
        guard.state.current_turn_operator_binding_id = operator_binding_id.and_then(|binding_id| {
            let binding_id = binding_id.trim();
            if binding_id.is_empty() {
                None
            } else {
                Some(binding_id.to_string())
            }
        });
        guard.state.current_turn_operator_reply_route_id =
            operator_reply_route_id.and_then(|route| {
                let route = route.trim();
                if route.is_empty() {
                    None
                } else {
                    Some(route.to_string())
                }
            });
        guard
            .state
            .active_skills
            .retain(|skill| matches!(skill.activation_state, SkillActivationState::SessionActive));
        self.inner.storage.write_agent(&guard.state)?;
        Ok(())
    }

    fn operator_transport_from_message(
        message: &MessageEnvelope,
    ) -> (Option<String>, Option<String>) {
        let transport = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("operator_transport"))
            .cloned();
        let binding_id = transport
            .as_ref()
            .and_then(|metadata| metadata.get("binding_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|binding_id| !binding_id.is_empty())
            .map(ToString::to_string);
        let reply_route_id = transport
            .as_ref()
            .and_then(|metadata| metadata.get("reply_route_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|route| !route.is_empty())
            .map(ToString::to_string);
        (binding_id, reply_route_id)
    }

    pub(crate) async fn promote_turn_active_skills(&self) -> Result<()> {
        let mut guard = self.inner.agent.lock().await;
        for skill in &mut guard.state.active_skills {
            if matches!(skill.activation_state, SkillActivationState::TurnActive) {
                skill.activation_state = SkillActivationState::SessionActive;
            }
        }
        self.inner.storage.write_agent(&guard.state)?;
        Ok(())
    }

    pub(crate) async fn record_skill_tool_activation(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<()> {
        match tool_name {
            "Read" => {
                if let Some(file_path) = input.get("file_path").and_then(|value| value.as_str()) {
                    self.record_skill_read_activation(file_path).await?;
                }
            }
            "ExecCommand" => {
                if let Some(command) = input.get("cmd").and_then(|value| value.as_str()) {
                    self.record_skill_command_activation(command).await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) async fn record_skill_read_activation(&self, file_path: &str) -> Result<()> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let resolved_path = execution.workspace.resolve_read_path(file_path)?;
        let state_snapshot = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let identity = self.agent_identity_view().await?;
        let skills = self.skills_runtime_view_for_state(&state_snapshot, &identity)?;
        let Some(skill) = find_skill_by_entrypoint(&skills.discoverable_skills, &resolved_path)
        else {
            return Ok(());
        };
        let mut guard = self.inner.agent.lock().await;
        let turn_index = guard.state.turn_index;
        let agent_id = guard.state.id.clone();

        if let Some(existing) = guard
            .state
            .active_skills
            .iter_mut()
            .find(|record| record.skill_id == skill.skill_id)
        {
            existing.activation_state = SkillActivationState::TurnActive;
            existing.activation_source = SkillActivationSource::ImplicitFromCatalog;
            existing.activated_at_turn = turn_index;
        } else {
            guard
                .state
                .active_skills
                .push(crate::types::ActiveSkillRecord {
                    skill_id: skill.skill_id.clone(),
                    name: skill.name.clone(),
                    path: skill.path.clone(),
                    scope: skill.scope.clone(),
                    agent_id: agent_id.clone(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::TurnActive,
                    activated_at_turn: turn_index,
                });
        }
        self.inner.storage.write_agent(&guard.state)?;
        self.inner.storage.append_event(&AuditEvent::new(
            "skill_activated",
            serde_json::json!({
                "agent_id": agent_id,
                "skill_id": skill.skill_id,
                "path": skill.path,
                "scope": skill.scope,
                "activation_source": SkillActivationSource::ImplicitFromCatalog,
                "activation_state": SkillActivationState::TurnActive,
                "turn_index": turn_index,
            }),
        ))?;
        Ok(())
    }

    async fn record_skill_command_activation(&self, command: &str) -> Result<()> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let state_snapshot = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let identity = self.agent_identity_view().await?;
        let skills = self.skills_runtime_view_for_state(&state_snapshot, &identity)?;

        for skill in skills.discoverable_skills {
            if command_mentions_path(command, &skill.path) {
                let skill_path = skill.path.to_string_lossy().into_owned();
                self.record_skill_read_activation(&skill_path).await?;
                continue;
            }

            if let Ok(relative_to_workspace) = skill
                .path
                .strip_prefix(execution.workspace.workspace_anchor())
            {
                if command_mentions_path(command, relative_to_workspace) {
                    let skill_path = skill.path.to_string_lossy().into_owned();
                    self.record_skill_read_activation(&skill_path).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn enqueue(&self, message: MessageEnvelope) -> Result<MessageEnvelope> {
        self.inner.storage.append_message(&message)?;
        self.inner.storage.append_queue_entry(&QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: message.agent_id.clone(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: Utc::now(),
        })?;
        {
            let mut guard = self.inner.agent.lock().await;
            guard.queue.push(message.clone());
            guard.state.pending = guard.queue.len();
            guard.state.last_wake_reason = Some(format!("{:?}", message.kind));
            guard.state.total_message_count = self.inner.storage.count_messages()?;
            if matches!(
                guard.state.status,
                AgentStatus::Asleep | AgentStatus::Booting
            ) {
                guard.state.status = AgentStatus::AwakeIdle;
                guard.state.sleeping_until = None;
            }
            self.inner.storage.write_agent(&guard.state)?;
        }

        self.inner.storage.append_event(&AuditEvent::new(
            "message_admitted",
            serde_json::json!({
                "message_id": message.id.clone(),
                "agent_id": message.agent_id.clone(),
                "kind": message.kind.clone(),
                "origin": message.origin.clone(),
                "trust": message.trust.clone(),
                "authority_class": message.authority_class,
                "delivery_surface": message.delivery_surface,
                "admission_context": message.admission_context,
                "correlation_id": message.correlation_id.clone(),
                "causation_id": message.causation_id.clone(),
            }),
        ))?;
        self.inner.storage.append_event(&AuditEvent::new(
            "message_enqueued",
            to_json_value(&message),
        ))?;
        self.inner.notify.notify_one();
        Ok(message)
    }

    pub(crate) fn append_audit_event(&self, kind: &str, data: serde_json::Value) -> Result<()> {
        self.inner
            .storage
            .append_event(&AuditEvent::new(kind, data))
    }

    pub async fn submit_wake_hint(&self, hint: WakeHint) -> Result<WakeDisposition> {
        let runtime_agent_id = self.agent_id().await?;
        let pending = PendingWakeHint {
            reason: hint.reason.clone(),
            source: hint.source.clone(),
            resource: hint.resource.clone(),
            body: hint.body.clone(),
            content_type: hint.content_type.clone(),
            correlation_id: hint.correlation_id.clone(),
            causation_id: hint.causation_id.clone(),
            created_at: Utc::now(),
        };

        let mut trigger_now = false;
        let disposition = {
            let mut guard = self.inner.agent.lock().await;
            match guard.state.status {
                AgentStatus::Paused | AgentStatus::Stopped => WakeDisposition::Ignored,
                AgentStatus::AwakeRunning | AgentStatus::AwaitingTask => {
                    guard.state.pending_wake_hint = Some(pending.clone());
                    self.inner.storage.write_agent(&guard.state)?;
                    WakeDisposition::Coalesced
                }
                AgentStatus::Booting | AgentStatus::AwakeIdle | AgentStatus::Asleep => {
                    if guard.queue.is_empty() {
                        if guard.state.pending_wake_hint.take().is_some() {
                            self.inner.storage.write_agent(&guard.state)?;
                        }
                        trigger_now = true;
                        WakeDisposition::Triggered
                    } else {
                        guard.state.pending_wake_hint = Some(pending.clone());
                        self.inner.storage.write_agent(&guard.state)?;
                        WakeDisposition::Coalesced
                    }
                }
            }
        };

        let event_kind = match disposition {
            WakeDisposition::Triggered => "wake_hint_triggered",
            WakeDisposition::Coalesced => "wake_hint_coalesced",
            WakeDisposition::Ignored => "wake_hint_ignored",
        };
        self.inner.storage.append_event(&AuditEvent::new(
            event_kind,
            serde_json::json!({
                "agent_id": runtime_agent_id,
                "reason": hint.reason,
                "source": hint.source,
                "resource": hint.resource,
                "body": hint.body,
                "content_type": hint.content_type,
                "correlation_id": hint.correlation_id,
                "causation_id": hint.causation_id,
            }),
        ))?;

        if trigger_now {
            if let Err(err) = self.emit_system_tick_from_wake_hint(&pending).await {
                let mut guard = self.inner.agent.lock().await;
                if guard.state.pending_wake_hint.is_none() {
                    guard.state.pending_wake_hint = Some(pending);
                    self.inner.storage.write_agent(&guard.state)?;
                }
                return Err(err);
            }
        }

        Ok(disposition)
    }

    pub async fn run(self) -> Result<()> {
        self.bootstrap_recovery().await?;
        {
            let mut guard = self.inner.agent.lock().await;
            if guard.state.status == AgentStatus::Booting {
                guard.state.status = AgentStatus::AwakeIdle;
                self.inner.storage.write_agent(&guard.state)?;
            }
        }

        loop {
            let next_message = {
                let mut guard = self.inner.agent.lock().await;
                if self.inner.shutdown_requested.load(Ordering::SeqCst) {
                    guard.state.current_run_id = None;
                    self.inner.storage.write_agent(&guard.state)?;
                    return Ok(());
                }
                if guard.state.status == AgentStatus::Stopped {
                    return Ok(());
                }
                if guard.state.status == AgentStatus::Paused {
                    None
                } else if let Some(message) = guard.queue.pop() {
                    let prior_state = guard.state.clone();
                    guard.state.pending = guard.queue.len();
                    guard.state.status = AgentStatus::AwakeRunning;
                    guard.state.current_run_id = Some(Uuid::new_v4().to_string());
                    guard.state.last_wake_reason = Some(format!("{:?}", message.kind));
                    self.inner.storage.write_agent(&guard.state)?;
                    self.inner.storage.append_queue_entry(&QueueEntryRecord {
                        message_id: message.id.clone(),
                        agent_id: message.agent_id.clone(),
                        priority: message.priority.clone(),
                        status: QueueEntryStatus::Dequeued,
                        created_at: message.created_at,
                        updated_at: Utc::now(),
                    })?;
                    Some((message, prior_state))
                } else {
                    None
                }
            };

            let Some((message, prior_state)) = next_message else {
                if self.maybe_emit_pending_system_tick(None).await? {
                    continue;
                }
                {
                    let mut guard = self.inner.agent.lock().await;
                    if !matches!(
                        guard.state.status,
                        AgentStatus::Asleep | AgentStatus::Paused
                    ) && guard.queue.is_empty()
                    {
                        guard.state.status = AgentStatus::Asleep;
                        guard.state.current_run_id = None;
                        guard.state.sleeping_until = None;
                        self.inner.storage.write_agent(&guard.state)?;
                        self.append_state_changed_events(&guard.state)?;
                    }
                }
                self.inner.notify.notified().await;
                continue;
            };

            let prior_closure = self.closure_decision_for_state(&prior_state, None).await?;
            if let Err(err) = self.process_message(message.clone(), prior_closure).await {
                error!("failed to process message {}: {err:#}", message.id);
                self.inner.storage.append_event(&AuditEvent::new(
                    "runtime_error",
                    serde_json::json!({
                        "message_id": message.id,
                        "message_kind": message.kind,
                        "error": err.to_string(),
                        "token_usage": provider_attempt_timeline(&err)
                            .and_then(|timeline| timeline.aggregated_token_usage.clone()),
                        "provider_attempt_timeline": provider_attempt_timeline(&err),
                    }),
                ))?;
                self.persist_runtime_failure_artifacts(&message, &err)
                    .await?;
                let mut guard = self.inner.agent.lock().await;
                if !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                ) {
                    guard.state.status = if task_state_reducer::has_blocking_active_tasks(
                        &self.inner.storage,
                        &guard.state.active_task_ids,
                    )? {
                        AgentStatus::AwaitingTask
                    } else {
                        AgentStatus::AwakeIdle
                    };
                }
                guard.state.current_run_id = None;
                self.inner.storage.write_agent(&guard.state)?;
                drop(guard);
                self.maybe_commit_turn_end_work_item_transition().await?;
                self.record_closure_decision_event(Some(true)).await?;
                self.maybe_emit_pending_system_tick(None).await?;
            } else {
                self.inner.storage.append_queue_entry(&QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: message.agent_id.clone(),
                    priority: message.priority.clone(),
                    status: QueueEntryStatus::Processed,
                    created_at: message.created_at,
                    updated_at: Utc::now(),
                })?;
            }
        }
    }

    async fn bootstrap_recovery(&self) -> Result<()> {
        if let Some(tasks) = self.inner.recovered_tasks.lock().await.take() {
            let (reattached, interrupted_tasks) =
                self.recover_supervised_child_tasks(tasks).await?;
            let interrupted = self.interrupt_active_tasks(interrupted_tasks).await?;
            if !reattached.is_empty() {
                self.inner.storage.append_event(&AuditEvent::new(
                    "supervised_child_task_monitor_reattached",
                    serde_json::json!({
                        "agent_id": self.agent_id().await?,
                        "task_ids": reattached.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
                    }),
                ))?;
            }
            if !interrupted.is_empty() {
                self.emit_system_tick_from_interrupted_tasks(&interrupted)
                    .await?;
            }
        }
        if let Some(timers) = self.inner.recovered_timers.lock().await.take() {
            self.recover_active_timers(timers).await?;
        }
        let pending_wake = {
            let guard = self.inner.agent.lock().await;
            guard.state.pending_wake_hint.clone()
        };
        if let Some(pending) = pending_wake {
            self.emit_system_tick_from_wake_hint(&pending).await?;
            let mut guard = self.inner.agent.lock().await;
            if guard.state.pending_wake_hint.as_ref() == Some(&pending) {
                guard.state.pending_wake_hint = None;
                self.inner.storage.write_agent(&guard.state)?;
            }
        }
        Ok(())
    }
}

fn command_mentions_path(command: &str, path: &Path) -> bool {
    let display = path.to_string_lossy();
    command.contains(display.as_ref())
}

#[cfg(test)]
fn current_input_summary(effective_prompt: &EffectivePrompt) -> String {
    let current_input = effective_prompt
        .context_sections
        .iter()
        .find(|section| section.name == "current_input")
        .map(|section| section.content.as_str())
        .unwrap_or_default();

    current_input
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_start_matches("- ")
        .rsplit_once("] ")
        .map(|(_, body)| body.to_string())
        .unwrap_or_else(|| current_input.to_string())
}

fn combine_text_history(history: &[String], text_blocks: &[String]) -> Vec<String> {
    history
        .iter()
        .cloned()
        .chain(text_blocks.iter().cloned())
        .collect()
}

fn is_max_output_stop_reason(stop_reason: Option<&str>) -> bool {
    matches!(
        stop_reason,
        Some("max_tokens") | Some("max_output_tokens") | Some("model_context_window_exceeded")
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use crate::{
        config::AppConfig,
        context::ContextConfig,
        host::RuntimeHost,
        prompt::{render_section, PromptSection, PromptStability},
        provider::{
            provider_turn_error, ProviderAttemptOutcome, ProviderAttemptRecord,
            ProviderAttemptTimeline, ProviderTransportDiagnostics, ProviderTurnResponse,
            ReqwestTransportDiagnostics, StubProvider,
        },
        storage::AppStorage,
        system::{ExecutionProfile, ExecutionSnapshot},
        types::{
            AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
            AgentState, AgentStatus, AgentVisibility, AuthorityClass, BriefKind, BriefRecord,
            CallbackDeliveryMode, ClosureOutcome, ContinuationClass, ContinuationTriggerKind,
            LoadedAgentsMd, MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin,
            PendingWakeHint, Priority, TaskOutputRetrievalStatus, TaskRecord, TaskRecoverySpec,
            TaskStatus, TimerRecord, TimerStatus, TokenUsage, TrustLevel, TurnTerminalKind,
            TurnTerminalRecord, WaitingIntentStatus, WaitingReason, WorkItemRecord, WorkItemStatus,
            WorkReactivationMode, WorkspaceEntry,
        },
    };
    use async_trait::async_trait;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use super::*;

    #[test]
    fn openai_max_output_tokens_stop_reason_triggers_recovery() {
        assert!(is_max_output_stop_reason(Some("max_output_tokens")));
    }

    fn context_config() -> ContextConfig {
        ContextConfig {
            recent_messages: 8,
            recent_briefs: 8,
            compaction_trigger_messages: 10,
            compaction_keep_recent_messages: 4,
            ..ContextConfig::default()
        }
    }

    async fn host_backed_test_runtime() -> (TempDir, RuntimeHost, RuntimeHandle) {
        let home = tempdir().unwrap();
        let config =
            crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let runtime = host.default_runtime().await.unwrap();
        (home, host, runtime)
    }

    #[tokio::test]
    async fn detached_host_runtime_starts_in_agent_home_workspace() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let snapshot = runtime.execution_snapshot().await.unwrap();

        assert_eq!(
            snapshot.workspace_id.as_deref(),
            Some(AGENT_HOME_WORKSPACE_ID)
        );
        assert_eq!(snapshot.workspace_anchor, runtime.agent_home());
        assert_eq!(snapshot.execution_root, runtime.agent_home());
    }

    #[tokio::test]
    async fn use_workspace_path_activates_project_workspace() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let workspace = tempdir().unwrap();

        crate::tool::tools::execute_builtin_tool(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "use-workspace".into(),
                name: "UseWorkspace".into(),
                input: serde_json::json!({
                    "path": workspace.path().display().to_string(),
                    "access_mode": "exclusive_write",
                }),
            },
        )
        .await
        .unwrap();
        let snapshot = runtime.execution_snapshot().await.unwrap();

        assert_ne!(
            snapshot.workspace_id.as_deref(),
            Some(AGENT_HOME_WORKSPACE_ID)
        );
        assert_eq!(snapshot.workspace_anchor, workspace.path());
        assert_eq!(snapshot.execution_root, workspace.path());
        assert_eq!(snapshot.cwd, workspace.path());
    }

    #[tokio::test]
    async fn use_workspace_agent_home_returns_to_fallback_without_deleting_project() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let workspace = tempdir().unwrap();
        let retained_file = workspace.path().join("retained.txt");
        std::fs::write(&retained_file, "keep").unwrap();

        crate::tool::tools::execute_builtin_tool(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "use-project".into(),
                name: "UseWorkspace".into(),
                input: serde_json::json!({ "path": workspace.path().display().to_string() }),
            },
        )
        .await
        .unwrap();
        crate::tool::tools::execute_builtin_tool(
            &runtime,
            "default",
            &TrustLevel::TrustedOperator,
            &crate::tool::ToolCall {
                id: "use-home".into(),
                name: "UseWorkspace".into(),
                input: serde_json::json!({ "workspace_id": AGENT_HOME_WORKSPACE_ID }),
            },
        )
        .await
        .unwrap();
        let snapshot = runtime.execution_snapshot().await.unwrap();

        assert_eq!(
            snapshot.workspace_id.as_deref(),
            Some(AGENT_HOME_WORKSPACE_ID)
        );
        assert_eq!(snapshot.execution_root, runtime.agent_home());
        assert!(retained_file.is_file());
    }

    fn private_child_identity(agent_id: &str) -> AgentIdentityView {
        AgentIdentityView {
            agent_id: agent_id.into(),
            kind: AgentKind::Child,
            visibility: AgentVisibility::Private,
            ownership: AgentOwnership::ParentSupervised,
            profile_preset: AgentProfilePreset::PrivateChild,
            status: AgentRegistryStatus::Active,
            is_default_agent: false,
            parent_agent_id: Some("default".into()),
            lineage_parent_agent_id: Some("default".into()),
            delegated_from_task_id: Some("task-1".into()),
        }
    }

    #[test]
    fn execution_snapshot_includes_attached_workspaces() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();

        // Add workspace entries to storage
        let entry1 = crate::types::WorkspaceEntry::new(
            String::from("ws-boot"),
            workspace.path().to_path_buf(),
            None,
        );
        runtime
            .inner
            .storage
            .append_workspace_entry(&entry1)
            .unwrap();

        let workspace2 = tempdir().unwrap();
        let entry2 = crate::types::WorkspaceEntry::new(
            String::from("ws-second"),
            workspace2.path().to_path_buf(),
            None,
        );
        runtime
            .inner
            .storage
            .append_workspace_entry(&entry2)
            .unwrap();

        // Create a state with multiple attached workspaces
        let mut state = crate::types::AgentState::new("default");
        state.attached_workspaces = vec!["ws-boot".into(), "ws-second".into()];
        state.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
            workspace_id: "ws-second".into(),
            workspace_anchor: workspace2.path().to_path_buf(),
            execution_root_id: "canonical_root:ws-second".into(),
            execution_root: workspace2.path().to_path_buf(),
            projection_kind: WorkspaceProjectionKind::CanonicalRoot,
            access_mode: WorkspaceAccessMode::ExclusiveWrite,
            cwd: workspace2.path().to_path_buf(),
            occupancy_id: None,
            projection_metadata: None,
        });
        state.execution_profile = ExecutionProfile::default();

        // Build the execution snapshot
        let workspace_view = runtime.workspace_view_from_state(&state).unwrap();
        let snapshot = runtime.execution_snapshot_for_view(
            state.execution_profile.clone(),
            &workspace_view,
            &state.attached_workspaces,
        );

        // Verify that attached_workspaces includes both workspaces
        assert_eq!(snapshot.attached_workspaces.len(), 2);
        assert_eq!(snapshot.attached_workspaces[0].0, "ws-second");
        assert_eq!(snapshot.attached_workspaces[0].1, workspace2.path());
        assert_eq!(snapshot.attached_workspaces[1].0, "ws-boot");
        assert_eq!(snapshot.attached_workspaces[1].1, workspace.path());
    }

    fn test_effective_prompt() -> EffectivePrompt {
        EffectivePrompt {
            identity: AgentIdentityView {
                agent_id: "default".into(),
                kind: AgentKind::Default,
                visibility: AgentVisibility::Public,
                ownership: AgentOwnership::SelfOwned,
                profile_preset: AgentProfilePreset::PublicNamed,
                status: AgentRegistryStatus::Active,
                is_default_agent: true,
                parent_agent_id: None,
                lineage_parent_agent_id: None,
                delegated_from_task_id: None,
            },
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: vec![],
                workspace_id: None,
                workspace_anchor: PathBuf::from("/tmp/agent-home"),
                execution_root: PathBuf::from("/tmp/agent-home"),
                cwd: PathBuf::from("/tmp/agent-home"),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
            },
            loaded_agents_md: LoadedAgentsMd::default(),
            cache_identity: crate::prompt::PromptCacheIdentity {
                agent_id: "default".into(),
                prompt_cache_key: "default".into(),
                working_memory_revision: 1,
                compression_epoch: 0,
            },
            system_sections: vec![],
            context_sections: vec![],
            rendered_system_prompt: "system".into(),
            rendered_context_attachment: "context".into(),
        }
    }

    fn closure_decision(
        outcome: ClosureOutcome,
        waiting_reason: Option<WaitingReason>,
    ) -> ClosureDecision {
        ClosureDecision {
            outcome,
            waiting_reason,
            work_signal: None,
            runtime_posture: RuntimePosture::Awake,
            evidence: Vec::new(),
        }
    }

    #[tokio::test]
    async fn current_closure_returns_none_while_foreground_work_remains() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.pending = 1;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        assert!(runtime.current_closure().await.unwrap().is_none());

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.pending = 0;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let closure = runtime.current_closure().await.unwrap().unwrap();
        assert_eq!(closure.outcome, ClosureOutcome::Completed);
    }

    #[tokio::test]
    async fn current_closure_returns_none_while_pending_wake_hint_remains() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.pending_wake_hint = Some(PendingWakeHint {
                reason: "wake".into(),
                source: None,
                resource: None,
                body: None,
                content_type: None,
                correlation_id: None,
                causation_id: None,
                created_at: Utc::now(),
            });
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        assert!(runtime.current_closure().await.unwrap().is_none());

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.pending_wake_hint = None;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let closure = runtime.current_closure().await.unwrap().unwrap();
        assert_eq!(closure.outcome, ClosureOutcome::Completed);
    }

    #[tokio::test]
    async fn wait_for_closure_blocks_until_foreground_work_clears() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.pending = 1;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let wait_runtime = runtime.clone();
        let waiter = tokio::spawn(async move { wait_runtime.wait_for_closure().await });

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(!waiter.is_finished());

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.pending = 0;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let closure = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(closure.outcome, ClosureOutcome::Completed);
    }

    async fn bind_turn_to_work_item(runtime: &RuntimeHandle, work_item_id: &str) {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.turn_index = 1;
        guard.state.current_turn_work_item_id = Some(work_item_id.to_string());
        guard.state.last_turn_terminal = Some(TurnTerminalRecord {
            turn_index: 1,
            kind: TurnTerminalKind::Completed,
            last_assistant_message: Some("done".into()),
            completed_at: Utc::now(),
            duration_ms: 10,
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    async fn seed_bound_work_item(
        runtime: &RuntimeHandle,
        status: WorkItemStatus,
        summary: Option<&str>,
        progress_note: Option<&str>,
    ) -> String {
        let created = runtime
            .update_work_item(
                None,
                "finish the bound delivery target".into(),
                WorkItemStatus::Active,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let updated = runtime
            .update_work_item(
                Some(created.id.clone()),
                created.delivery_target.clone(),
                status,
                summary.map(str::to_string),
                progress_note.map(str::to_string),
                created.parent_id.clone(),
            )
            .await
            .unwrap();
        bind_turn_to_work_item(runtime, &updated.id).await;
        updated.id
    }

    #[tokio::test]
    async fn work_item_query_tools_return_current_open_done_views() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let active = runtime
            .update_work_item(
                None,
                "finish active delivery".into(),
                WorkItemStatus::Active,
                Some("active summary".into()),
                None,
                None,
            )
            .await
            .unwrap();
        runtime
            .update_work_plan(
                active.id.clone(),
                vec![crate::types::WorkPlanItem {
                    step: "inspect query surface".into(),
                    status: crate::types::WorkPlanStepStatus::InProgress,
                }],
            )
            .await
            .unwrap();
        let queued = runtime
            .update_work_item(
                None,
                "queued delivery".into(),
                WorkItemStatus::Queued,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let completed = runtime
            .update_work_item(
                None,
                "completed delivery".into(),
                WorkItemStatus::Completed,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        bind_turn_to_work_item(&runtime, &active.id).await;

        let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
        let (active_result, _) = registry
            .execute(
                &runtime,
                "default",
                &TrustLevel::TrustedOperator,
                &crate::tool::ToolCall {
                    id: "active".into(),
                    name: "GetActiveWorkItem".into(),
                    input: serde_json::json!({"include_plan": true}),
                },
            )
            .await
            .unwrap();
        let active_payload = active_result.envelope.result.unwrap();
        assert_eq!(
            active_payload["context"]["current_work_item_id"].as_str(),
            Some(active.id.as_str())
        );
        assert_eq!(active_payload["work_item"]["state"].as_str(), Some("open"));
        assert_eq!(
            active_payload["work_item"]["focus"].as_str(),
            Some("current")
        );
        assert_eq!(
            active_payload["work_item"]["is_current"].as_bool(),
            Some(true)
        );
        assert_eq!(
            active_payload["work_item"]["plan"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let (list_result, _) = registry
            .execute(
                &runtime,
                "default",
                &TrustLevel::TrustedOperator,
                &crate::tool::ToolCall {
                    id: "list".into(),
                    name: "ListWorkItems".into(),
                    input: serde_json::json!({"filter": "open", "limit": 10}),
                },
            )
            .await
            .unwrap();
        let list_payload = list_result.envelope.result.unwrap();
        let items = list_payload["work_items"].as_array().unwrap();
        assert_eq!(list_payload["total_matching"].as_u64(), Some(2));
        assert!(items
            .iter()
            .any(|item| item["id"].as_str() == Some(active.id.as_str())));
        assert!(items
            .iter()
            .any(|item| item["id"].as_str() == Some(queued.id.as_str())));
        assert!(!items
            .iter()
            .any(|item| item["id"].as_str() == Some(completed.id.as_str())));

        let (done_result, _) = registry
            .execute(
                &runtime,
                "default",
                &TrustLevel::TrustedOperator,
                &crate::tool::ToolCall {
                    id: "done".into(),
                    name: "GetWorkItem".into(),
                    input: serde_json::json!({"work_item_id": completed.id}),
                },
            )
            .await
            .unwrap();
        let done_payload = done_result.envelope.result.unwrap();
        assert_eq!(done_payload["work_item"]["state"].as_str(), Some("done"));
        assert_eq!(done_payload["work_item"]["focus"].as_str(), Some("done"));

        bind_turn_to_work_item(&runtime, completed.id.as_str()).await;
        let (fallback_result, _) = registry
            .execute(
                &runtime,
                "default",
                &TrustLevel::TrustedOperator,
                &crate::tool::ToolCall {
                    id: "fallback-active".into(),
                    name: "GetActiveWorkItem".into(),
                    input: serde_json::json!({}),
                },
            )
            .await
            .unwrap();
        let fallback_payload = fallback_result.envelope.result.unwrap();
        assert_eq!(
            fallback_payload["context"]["current_work_item_id"].as_str(),
            Some(active.id.as_str())
        );
        assert_eq!(
            fallback_payload["work_item"]["id"].as_str(),
            Some(active.id.as_str())
        );
    }

    async fn mark_blocking_task(runtime: &RuntimeHandle, task_id: &str) {
        runtime
            .inner
            .storage
            .append_task(&TaskRecord {
                id: task_id.into(),
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                summary: Some("blocking command".into()),
                detail: Some(serde_json::json!({
                    "wait_policy": "blocking"
                })),
                recovery: None,
            })
            .unwrap();
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.active_task_ids = vec![task_id.to_string()];
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    #[tokio::test]
    async fn persist_brief_binds_current_turn_work_item() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(&runtime, WorkItemStatus::Active, None, None).await;

        runtime
            .persist_brief(&BriefRecord::new(
                "default",
                BriefKind::Result,
                "bound brief",
                None,
                None,
            ))
            .await
            .unwrap();

        let briefs = runtime.recent_briefs(10).await.unwrap();
        assert_eq!(briefs.len(), 1);
        assert_eq!(
            briefs[0].work_item_id.as_deref(),
            Some(work_item_id.as_str())
        );
    }

    #[tokio::test]
    async fn create_callback_binds_current_turn_work_item() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(&runtime, WorkItemStatus::Active, None, None).await;

        runtime
            .create_callback(
                "wait for review".into(),
                "github".into(),
                "review_submitted".into(),
                Some("pull_request:302".into()),
                CallbackDeliveryMode::WakeOnly,
            )
            .await
            .unwrap();

        let waiting = runtime.latest_waiting_intents().await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(
            waiting[0].work_item_id.as_deref(),
            Some(work_item_id.as_str())
        );
    }

    #[tokio::test]
    async fn interactive_tool_execution_binds_current_turn_work_item() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(OneToolThenTextProvider {
                calls: Mutex::new(0),
            }),
            "default".into(),
            ContextConfig {
                prompt_budget_estimated_tokens: 16384,
                compaction_keep_recent_estimated_tokens: 2048,
                ..context_config()
            },
        )
        .unwrap();

        let work_item = runtime
            .update_work_item(
                None,
                "verify binding".into(),
                WorkItemStatus::Active,
                Some("check tool work item binding".into()),
                None,
                None,
            )
            .await
            .unwrap();
        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "run one verification command".into(),
            },
        );

        runtime
            .process_interactive_message(
                &message,
                None,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        let tools = runtime.storage().read_recent_tool_executions(10).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name, "ExecCommand");
        assert_eq!(
            tools[0].work_item_id.as_deref(),
            Some(work_item.id.as_str())
        );
    }

    #[tokio::test]
    async fn runtime_sleeps_after_processing() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());

        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "hello".into(),
            },
        );
        runtime.enqueue(message).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.status, AgentStatus::Asleep);
        runtime_task.abort();
    }

    #[tokio::test]
    async fn turn_end_work_item_commit_defaults_completed_turn_to_active() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(&runtime, WorkItemStatus::Active, None, None).await;
        let committed = runtime
            .maybe_commit_turn_end_work_item_transition()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(committed.id, work_item_id);
        assert_eq!(committed.status, WorkItemStatus::Active);
        assert!(committed.progress_note.is_none());
        assert!(runtime
            .agent_state()
            .await
            .unwrap()
            .current_turn_work_item_id
            .is_none());
    }

    #[tokio::test]
    async fn turn_end_work_item_commit_moves_failed_turn_to_waiting() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(&runtime, WorkItemStatus::Active, None, None).await;
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.last_turn_terminal = Some(TurnTerminalRecord {
                turn_index: guard.state.turn_index,
                kind: TurnTerminalKind::Aborted,
                last_assistant_message: Some("provider context_length_exceeded".into()),
                completed_at: Utc::now(),
                duration_ms: 42,
            });
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let committed = runtime
            .maybe_commit_turn_end_work_item_transition()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(committed.id, work_item_id);
        assert_eq!(committed.status, WorkItemStatus::Waiting);
        assert_eq!(
            committed.progress_note.as_deref(),
            Some("Turn failed and requires operator intervention before continuing.")
        );
    }

    #[tokio::test]
    async fn turn_end_work_item_commit_moves_bound_item_to_waiting_when_runtime_is_waiting() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(&runtime, WorkItemStatus::Active, None, None).await;
        mark_blocking_task(&runtime, "blocking-wait").await;

        let committed = runtime
            .maybe_commit_turn_end_work_item_transition()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(committed.id, work_item_id);
        assert_eq!(committed.status, WorkItemStatus::Waiting);
        assert_eq!(
            committed.progress_note.as_deref(),
            Some("Waiting on a task result.")
        );
    }

    #[tokio::test]
    async fn turn_end_work_item_commit_preserves_explicit_completed_claim_without_waiting_facts() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(
            &runtime,
            WorkItemStatus::Completed,
            Some("finished"),
            Some("all requested changes are done"),
        )
        .await;
        let committed = runtime
            .maybe_commit_turn_end_work_item_transition()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(committed.id, work_item_id);
        assert_eq!(committed.status, WorkItemStatus::Completed);
        assert_eq!(
            committed.progress_note.as_deref(),
            Some("all requested changes are done")
        );
    }

    #[tokio::test]
    async fn turn_end_work_item_commit_rejects_completed_claim_when_runtime_is_still_waiting() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(
            &runtime,
            WorkItemStatus::Completed,
            Some("finished"),
            Some("marked complete too early"),
        )
        .await;
        mark_blocking_task(&runtime, "blocking-after-complete").await;

        let committed = runtime
            .maybe_commit_turn_end_work_item_transition()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(committed.id, work_item_id);
        assert_eq!(committed.status, WorkItemStatus::Waiting);
        assert_eq!(
            committed.progress_note.as_deref(),
            Some("marked complete too early")
        );
    }

    #[tokio::test]
    async fn turn_end_work_item_commit_preserves_explicit_queued_claim() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work_item_id = seed_bound_work_item(
            &runtime,
            WorkItemStatus::Queued,
            Some("yield the active slot"),
            Some("requeue after this turn"),
        )
        .await;
        let committed = runtime
            .maybe_commit_turn_end_work_item_transition()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(committed.id, work_item_id);
        assert_eq!(committed.status, WorkItemStatus::Queued);
        assert_eq!(
            committed.progress_note.as_deref(),
            Some("requeue after this turn")
        );
    }

    #[tokio::test]
    async fn reconcile_waiting_contract_cancels_wait_without_anchor_and_emits_audit() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let capability = runtime
            .create_callback(
                "wait for external review".into(),
                "github".into(),
                "review_submitted".into(),
                Some("pull_request:305".into()),
                CallbackDeliveryMode::WakeOnly,
            )
            .await
            .unwrap();
        let message = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "test".into(),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "tick".into(),
            },
        );
        let closure = runtime.current_closure_decision().await.unwrap();

        runtime
            .reconcile_waiting_contract(&message, &closure)
            .await
            .unwrap();

        let waiting = runtime.latest_waiting_intents().await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, capability.waiting_intent_id);
        assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
        let events = runtime.storage().read_recent_events(16).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "missing_active_work_item_before_wait"));
    }

    #[tokio::test]
    async fn reconcile_waiting_contract_cancels_old_waits_after_active_work_switch() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let old_work = runtime
            .update_work_item(
                None,
                "old delivery target".into(),
                WorkItemStatus::Active,
                Some("old work".into()),
                None,
                None,
            )
            .await
            .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard
                .state
                .working_memory
                .current_working_memory
                .active_work_item_id = Some(old_work.id.clone());
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }
        let capability = runtime
            .create_callback(
                "wait for old review".into(),
                "github".into(),
                "review_submitted".into(),
                Some("pull_request:123".into()),
                CallbackDeliveryMode::WakeOnly,
            )
            .await
            .unwrap();
        let old_waiting_created_at = runtime
            .latest_waiting_intents()
            .await
            .unwrap()
            .first()
            .expect("waiting intent should exist")
            .created_at;

        runtime
            .update_work_item(
                Some(old_work.id.clone()),
                old_work.delivery_target.clone(),
                WorkItemStatus::Completed,
                Some("old work done".into()),
                None,
                old_work.parent_id.clone(),
            )
            .await
            .unwrap();
        let new_work = runtime
            .update_work_item(
                None,
                "new delivery target".into(),
                WorkItemStatus::Active,
                Some("new work".into()),
                None,
                None,
            )
            .await
            .unwrap();

        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "switch to the new target".into(),
            },
        );
        message.created_at = old_waiting_created_at + chrono::Duration::seconds(1);
        let closure = runtime.current_closure_decision().await.unwrap();

        runtime
            .reconcile_waiting_contract(&message, &closure)
            .await
            .unwrap();

        let waiting = runtime.latest_waiting_intents().await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, capability.waiting_intent_id);
        assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
        assert_eq!(
            runtime
                .storage()
                .work_queue_prompt_projection()
                .unwrap()
                .active
                .as_ref()
                .map(|item| item.id.as_str()),
            Some(new_work.id.as_str())
        );
        let events = runtime.storage().read_recent_events(20).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "stale_waiting_intents_cancelled"));
    }

    #[tokio::test]
    async fn reconcile_waiting_contract_cancels_waits_when_only_waiting_anchor_exists() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let waiting_work = runtime
            .update_work_item(
                None,
                "waiting-only delivery target".into(),
                WorkItemStatus::Waiting,
                Some("waiting work".into()),
                None,
                None,
            )
            .await
            .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard
                .state
                .working_memory
                .current_working_memory
                .active_work_item_id = Some(waiting_work.id.clone());
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }
        let capability = runtime
            .create_callback(
                "wait for external response".into(),
                "github".into(),
                "review_submitted".into(),
                Some("pull_request:456".into()),
                CallbackDeliveryMode::WakeOnly,
            )
            .await
            .unwrap();
        let message = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "test".into(),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "tick".into(),
            },
        );
        let closure = runtime.current_closure_decision().await.unwrap();

        runtime
            .reconcile_waiting_contract(&message, &closure)
            .await
            .unwrap();

        let waiting = runtime.latest_waiting_intents().await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, capability.waiting_intent_id);
        assert_eq!(waiting[0].status, WaitingIntentStatus::Cancelled);
        assert!(runtime
            .storage()
            .work_queue_prompt_projection()
            .unwrap()
            .active
            .is_none());
        let events = runtime.storage().read_recent_events(20).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "missing_active_work_item_before_wait"));
    }

    #[tokio::test]
    async fn reconcile_waiting_contract_keeps_waits_when_anchor_is_newly_established() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let work = runtime
            .update_work_item(
                None,
                "newly anchored delivery target".into(),
                WorkItemStatus::Active,
                Some("new work".into()),
                None,
                None,
            )
            .await
            .unwrap();
        let capability = runtime
            .create_callback(
                "wait for fresh review".into(),
                "github".into(),
                "review_submitted".into(),
                Some("pull_request:789".into()),
                CallbackDeliveryMode::WakeOnly,
            )
            .await
            .unwrap();
        let waiting_created_at = runtime
            .latest_waiting_intents()
            .await
            .unwrap()
            .first()
            .expect("waiting intent should exist")
            .created_at;

        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "test".into(),
            },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "tick".into(),
            },
        );
        message.created_at = waiting_created_at + chrono::Duration::seconds(1);
        let closure = runtime.current_closure_decision().await.unwrap();

        runtime
            .reconcile_waiting_contract(&message, &closure)
            .await
            .unwrap();

        let waiting = runtime.latest_waiting_intents().await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, capability.waiting_intent_id);
        assert_eq!(waiting[0].status, WaitingIntentStatus::Active);
        assert_eq!(
            runtime
                .storage()
                .waiting_contract_anchor()
                .unwrap()
                .as_ref()
                .map(|item| item.id.as_str()),
            Some(work.id.as_str())
        );
        let events = runtime.storage().read_recent_events(20).unwrap();
        assert!(!events
            .iter()
            .any(|event| event.kind == "stale_waiting_intents_cancelled"));
    }

    #[tokio::test]
    async fn runtime_tracks_background_task() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        let task = runtime
            .schedule_command_task(
                "demo task".into(),
                crate::types::CommandTaskSpec {
                    cmd: "sleep 1".into(),
                    workdir: None,
                    shell: None,
                    login: true,
                    tty: false,
                    yield_time_ms: 10,
                    max_output_tokens: None,
                    accepts_input: false,
                    continue_on_result: false,
                },
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let state = runtime.agent_state().await.unwrap();
            if !state.active_task_ids.contains(&task.id) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "background task remained active past test deadline"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        runtime_task.abort();
    }

    #[tokio::test]
    async fn runtime_replays_unprocessed_queue_messages_after_restart() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("replayed")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        runtime
            .enqueue(MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                TrustLevel::TrustedOperator,
                Priority::Normal,
                MessageBody::Text {
                    text: "recover me".into(),
                },
            ))
            .await
            .unwrap();
        drop(runtime);

        let recovered = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("replayed")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(recovered.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let briefs = recovered.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| brief.text.contains("replayed")));
        runtime_task.abort();
    }

    #[tokio::test]
    async fn runtime_interrupts_inflight_task_after_restart() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_task(&TaskRecord {
                id: "sleep-recover".into(),
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                summary: Some("recoverable command".into()),
                detail: None,
                recovery: Some(TaskRecoverySpec::CommandTask {
                    summary: "recoverable command".into(),
                    spec: crate::types::CommandTaskSpec {
                        cmd: "sleep 5".into(),
                        workdir: None,
                        shell: None,
                        login: true,
                        tty: false,
                        yield_time_ms: 10,
                        max_output_tokens: None,
                        accepts_input: false,
                        continue_on_result: false,
                    },
                    trust: TrustLevel::TrustedOperator,
                    promoted_from_exec_command: false,
                }),
            })
            .unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let task = runtime
            .latest_task_records()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.id == "sleep-recover")
            .unwrap();
        assert_eq!(task.status, TaskStatus::Interrupted);
        assert_eq!(
            task.detail
                .as_ref()
                .and_then(|detail| detail.get("status_before_restart"))
                .and_then(serde_json::Value::as_str),
            Some("running")
        );
        let output = runtime
            .task_output("sleep-recover", false, 0)
            .await
            .unwrap();
        assert_eq!(output.retrieval_status, TaskOutputRetrievalStatus::NotReady);
        assert_eq!(output.task.status, TaskStatus::Interrupted);
        let events = runtime.storage().read_recent_events(100).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "task_interrupted_on_restart"));
        let messages = runtime.storage().read_recent_messages(20).unwrap();
        assert!(messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && matches!(
                    message.origin,
                    MessageOrigin::System { ref subsystem } if subsystem == "task_restart"
                )
        }));
        assert!(messages.iter().any(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|value| value.get("interrupted_tasks"))
                .and_then(|value| value.get("count"))
                .and_then(serde_json::Value::as_u64)
                == Some(1)
        }));
        assert!(messages.iter().any(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|value| value.get("interrupted_tasks"))
                .and_then(|value| value.get("items"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("status_before_restart")
                            .and_then(serde_json::Value::as_str)
                            == Some("running")
                    })
                })
        }));
        runtime_task.abort();
    }

    #[tokio::test]
    async fn runtime_fires_overdue_timer_after_restart() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_timer(&TimerRecord {
                id: "timer-recover".into(),
                agent_id: "default".into(),
                created_at: Utc::now(),
                duration_ms: 10,
                interval_ms: None,
                repeat: false,
                status: TimerStatus::Active,
                summary: Some("timer recovered".into()),
                next_fire_at: Some(Utc::now() - chrono::Duration::milliseconds(5)),
                last_fired_at: None,
                fire_count: 0,
            })
            .unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let timer = runtime
            .recent_timers(10)
            .await
            .unwrap()
            .into_iter()
            .find(|timer| timer.id == "timer-recover" && timer.fire_count == 1)
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Completed);
        runtime_task.abort();
    }

    #[tokio::test]
    async fn runtime_recovers_active_timer_without_next_fire_at() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_timer(&TimerRecord {
                id: "timer-missing-next-fire".into(),
                agent_id: "default".into(),
                created_at: Utc::now() - chrono::Duration::milliseconds(20),
                duration_ms: 10,
                interval_ms: None,
                repeat: false,
                status: TimerStatus::Active,
                summary: Some("timer fallback".into()),
                next_fire_at: None,
                last_fired_at: None,
                fire_count: 0,
            })
            .unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer fallback done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let timer = runtime
            .recent_timers(10)
            .await
            .unwrap()
            .into_iter()
            .find(|timer| timer.id == "timer-missing-next-fire" && timer.fire_count == 1)
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Completed);
        runtime_task.abort();
    }

    #[tokio::test]
    async fn schedule_timer_rejects_unrepresentable_duration() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let result = runtime.schedule_timer(u64::MAX, None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn runtime_emits_pending_wake_hint_as_system_tick_on_restart() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        agent.pending_wake_hint = Some(PendingWakeHint {
            reason: "restart wake".into(),
            source: Some("test".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: Some("corr".into()),
            causation_id: None,
            created_at: Utc::now(),
        });
        storage.write_agent(&agent).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("wake done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let state = runtime.agent_state().await.unwrap();
        assert!(state.pending_wake_hint.is_none());
        let messages = runtime.storage().read_recent_messages(10).unwrap();
        assert!(messages
            .iter()
            .any(|message| message.kind == MessageKind::SystemTick
                && message.authority_class == AuthorityClass::RuntimeInstruction));
        runtime_task.abort();
    }

    #[tokio::test]
    async fn runtime_emits_work_queue_system_tick_for_active_item_on_restart() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let active = WorkItemRecord::new(
            "default",
            "continue active runtime cleanup",
            WorkItemStatus::Active,
        );
        storage.append_work_item(&active).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let messages = runtime.storage().read_recent_messages(20).unwrap();
        assert!(messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_queue"))
                    .and_then(|value| value.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    == Some("continue_active")
        }));
        runtime_task.abort();
    }

    #[tokio::test]
    async fn recovered_agent_with_none_workspace_initializes_active_entry() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        // Create a recovered agent state without active_workspace_entry
        let mut agent = AgentState::new("default");
        agent.active_workspace_entry = None;
        agent.attached_workspaces = vec![];
        storage.write_agent(&agent).unwrap();

        // Recover the runtime - should initialize active_workspace_entry
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        // Verify that active_workspace_entry was initialized
        let state = runtime.inner.agent.lock().await.state.clone();
        assert!(
            state.active_workspace_entry.is_some(),
            "active_workspace_entry should be initialized for new agents"
        );
        let entry = state.active_workspace_entry.as_ref().unwrap();
        assert!(
            entry.workspace_id.starts_with("ws-"),
            "workspace_id should be generated for initial workspace"
        );
        assert_eq!(
            entry.execution_root,
            workspace.path(),
            "execution_root should match initial workspace path"
        );
    }

    #[tokio::test]
    async fn recovered_agent_with_missing_worktree_clears_workspace_fields() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        // Create a recovered agent with missing worktree session
        let mut agent = AgentState::new("default");
        let worktree_path = workspace.path().join("nonexistent");
        agent.worktree_session = Some(crate::types::WorktreeSession {
            original_cwd: worktree_path.clone(),
            original_branch: "main".into(),
            worktree_path: worktree_path.clone(),
            worktree_branch: "test-branch".into(),
        });
        agent.active_workspace_entry = Some(crate::types::ActiveWorkspaceEntry {
            workspace_id: "test-workspace".into(),
            workspace_anchor: worktree_path.clone(),
            execution_root_id: "test-root".into(),
            execution_root: worktree_path.clone(),
            projection_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
            access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
            cwd: worktree_path.clone(),
            occupancy_id: None,
            projection_metadata: None,
        });
        storage.write_agent(&agent).unwrap();

        // Recover the runtime - should clear missing worktree
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        // Verify that worktree_session was cleared and agent_home is activated
        let state = runtime.inner.agent.lock().await.state.clone();
        assert!(
            state.worktree_session.is_none(),
            "worktree_session should be cleared when worktree is missing"
        );
        // Verify agent_home is activated as fallback
        let entry = state.active_workspace_entry.as_ref();
        assert!(
            entry.is_some(),
            "active_workspace_entry should be set to agent_home when worktree is missing"
        );
        assert_eq!(
            entry.unwrap().workspace_id.starts_with("agent_home"),
            true,
            "workspace_id should be agent_home when worktree is missing"
        );
        assert_eq!(
            entry.unwrap().projection_kind,
            WorkspaceProjectionKind::CanonicalRoot,
            "projection_kind should be CanonicalRoot when worktree is missing"
        );
    }

    #[tokio::test]
    async fn current_closure_reports_continuable_for_active_work_item() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let active = WorkItemRecord::new(
            "default",
            "continue active runtime cleanup",
            WorkItemStatus::Active,
        );
        let active_id = active.id.clone();
        storage.append_work_item(&active).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let closure = runtime.current_closure_decision().await.unwrap();
        assert_eq!(closure.outcome, ClosureOutcome::Continuable);
        assert_eq!(closure.waiting_reason, None);
        let signal = closure.work_signal.expect("work signal should exist");
        assert_eq!(signal.work_item_id, active_id);
        assert_eq!(signal.status, WorkItemStatus::Active);
        assert_eq!(
            signal.reactivation_mode,
            WorkReactivationMode::ContinueActive
        );
    }

    #[tokio::test]
    async fn current_closure_reports_continuable_for_queued_work_item_without_active_item() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let queued = WorkItemRecord::new(
            "default",
            "activate queued runtime cleanup",
            WorkItemStatus::Queued,
        );
        let queued_id = queued.id.clone();
        storage.append_work_item(&queued).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let closure = runtime.current_closure_decision().await.unwrap();
        assert_eq!(closure.outcome, ClosureOutcome::Continuable);
        assert_eq!(closure.waiting_reason, None);
        let signal = closure.work_signal.expect("work signal should exist");
        assert_eq!(signal.work_item_id, queued_id);
        assert_eq!(signal.status, WorkItemStatus::Queued);
        assert_eq!(
            signal.reactivation_mode,
            WorkReactivationMode::ActivateQueued
        );
    }

    #[tokio::test]
    async fn idle_tick_prefers_active_work_item_over_queued_work_item() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let active = WorkItemRecord::new(
            "default",
            "continue active runtime cleanup",
            WorkItemStatus::Active,
        );
        let queued =
            WorkItemRecord::new("default", "queued runtime cleanup", WorkItemStatus::Queued);
        storage.append_work_item(&active).unwrap();
        storage.append_work_item(&queued).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        assert!(runtime.maybe_emit_pending_system_tick(None).await.unwrap());

        let queued_latest = runtime
            .latest_work_item(&queued.id)
            .await
            .unwrap()
            .expect("queued item should still exist");
        assert_eq!(queued_latest.status, WorkItemStatus::Queued);

        let messages = runtime.storage().read_recent_messages(10).unwrap();
        assert!(messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_queue"))
                    .and_then(|value| value.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    == Some("continue_active")
                && message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_queue"))
                    .and_then(|value| value.get("work_item_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(active.id.as_str())
        }));
    }

    #[tokio::test]
    async fn idle_tick_suppresses_continue_active_after_model_visible_task_result() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let active = WorkItemRecord::new(
            "default",
            "continue active runtime cleanup",
            WorkItemStatus::Active,
        );
        storage.append_work_item(&active).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let task_result_rejoin = ContinuationResolution {
            trigger_kind: ContinuationTriggerKind::TaskResult,
            class: ContinuationClass::ResumeExpectedWait,
            model_visible: true,
            prior_closure_outcome: ClosureOutcome::Waiting,
            prior_waiting_reason: Some(WaitingReason::AwaitingTaskResult),
            matched_waiting_reason: true,
            evidence: vec!["matches_waiting_reason".into()],
        };

        assert!(!runtime
            .maybe_emit_pending_system_tick(Some(&task_result_rejoin))
            .await
            .unwrap());

        let messages = runtime.storage().read_recent_messages(10).unwrap();
        assert!(!messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_queue"))
                    .and_then(|value| value.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    == Some("continue_active")
        }));
    }

    #[tokio::test]
    async fn queued_activation_updates_working_memory_before_follow_up_turn() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let queued =
            WorkItemRecord::new("default", "queued runtime cleanup", WorkItemStatus::Queued);
        storage.append_work_item(&queued).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());

        runtime
            .enqueue(MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                TrustLevel::TrustedOperator,
                Priority::Normal,
                MessageBody::Text {
                    text: "wrap up current work".into(),
                },
            ))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let state = runtime.agent_state().await.unwrap();
        assert_eq!(
            state
                .working_memory
                .current_working_memory
                .active_work_item_id
                .as_deref(),
            Some(queued.id.as_str())
        );
        assert!(
            state.working_memory.working_memory_revision > 0,
            "working memory should refresh after queued activation"
        );
        let deltas = runtime
            .storage()
            .read_recent_working_memory_deltas(10)
            .unwrap();
        assert!(deltas.iter().any(|delta| {
            delta
                .changed_fields
                .iter()
                .any(|field| field == "active_work_item_id")
                && delta.to_revision == state.working_memory.working_memory_revision
        }));
        assert_eq!(
            state
                .working_memory
                .active_episode_builder
                .as_ref()
                .and_then(|builder| builder.active_work_item_id.as_deref()),
            Some(queued.id.as_str())
        );

        runtime_task.abort();
    }

    #[tokio::test]
    async fn idle_tick_prefers_pending_wake_hint_over_work_queue_tick() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let active = WorkItemRecord::new(
            "default",
            "continue active runtime cleanup",
            WorkItemStatus::Active,
        );
        storage.append_work_item(&active).unwrap();

        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::Asleep;
        agent.pending_wake_hint = Some(PendingWakeHint {
            reason: "resume from callback".into(),
            source: Some("test".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: Some("wake-priority".into()),
            causation_id: None,
            created_at: Utc::now(),
        });
        storage.write_agent(&agent).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        assert!(runtime.maybe_emit_pending_system_tick(None).await.unwrap());
        let state = runtime.agent_state().await.unwrap();
        assert!(state.pending_wake_hint.is_none());

        let messages = runtime.storage().read_recent_messages(10).unwrap();
        assert!(messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && matches!(
                    message.origin,
                    MessageOrigin::System { ref subsystem } if subsystem == "wake_hint"
                )
        }));
        assert!(!messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_queue"))
                    .is_some()
        }));
    }

    #[tokio::test]
    async fn runtime_activates_queued_work_item_and_emits_work_queue_system_tick_on_restart() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let queued = WorkItemRecord::new(
            "default",
            "activate queued runtime cleanup",
            WorkItemStatus::Queued,
        );
        let queued_id = queued.id.clone();
        storage.append_work_item(&queued).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let active = runtime
            .latest_work_item(&queued_id)
            .await
            .unwrap()
            .expect("queued item should still exist");
        assert_eq!(active.status, WorkItemStatus::Active);

        let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "system_tick_emitted"
                && event.data["work_queue"]["work_item_id"].as_str() == Some(queued_id.as_str())
        }));
        runtime_task.abort();
    }

    #[tokio::test]
    async fn queued_work_item_update_wakes_sleeping_runtime_and_activates_it() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("tick done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let queued = runtime
            .update_work_item(
                None,
                "wake from direct queued work item update".into(),
                WorkItemStatus::Queued,
                Some("queued while idle".into()),
                None,
                None,
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let active = runtime
            .latest_work_item(&queued.id)
            .await
            .unwrap()
            .expect("queued item should still exist");
        assert_eq!(active.status, WorkItemStatus::Active);

        let messages = runtime.storage().read_recent_messages(20).unwrap();
        assert!(messages.iter().any(|message| {
            message.kind == MessageKind::SystemTick
                && message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("work_queue"))
                    .and_then(|value| value.get("work_item_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(queued.id.as_str())
        }));
        runtime_task.abort();
    }

    #[tokio::test]
    async fn agent_summary_reports_agents_md_sources_without_content() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let agent_agents_md = dir.path().join("AGENTS.md");
        let workspace_claude_md = workspace.path().join("CLAUDE.md");
        std::fs::write(&agent_agents_md, "agent-only secret").unwrap();
        std::fs::write(&workspace_claude_md, "workspace-only secret").unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let summary = runtime.agent_summary().await.unwrap();
        assert_eq!(
            summary
                .loaded_agents_md
                .agent_source
                .as_ref()
                .map(|source| source.path.clone()),
            Some(agent_agents_md)
        );
        assert_eq!(
            summary
                .loaded_agents_md
                .workspace_source
                .as_ref()
                .map(|source| source.path.clone()),
            Some(workspace_claude_md)
        );

        let json = serde_json::to_value(&summary).unwrap();
        assert!(json["loaded_agents_md"]["agent_source"]["content"].is_null());
        assert!(json["loaded_agents_md"]["workspace_source"]["content"].is_null());

        let mut legacy_json = json;
        legacy_json
            .as_object_mut()
            .expect("agent summary should serialize as object")
            .remove("lifecycle");
        let decoded: AgentSummary = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(
            decoded.lifecycle,
            crate::types::AgentLifecycleHint::default()
        );
    }

    #[tokio::test]
    async fn loaded_agents_md_uses_active_workspace_entry_anchor_without_legacy_anchor() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let worktree = tempdir().unwrap();
        let workspace_agents_md = workspace.path().join("AGENTS.md");
        std::fs::write(&workspace_agents_md, "workspace rules").unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                workspace_id: "workspace-1".into(),
                workspace_anchor: workspace.path().to_path_buf(),
                execution_root_id: RuntimeHandle::build_execution_root_id(
                    "workspace-1",
                    WorkspaceProjectionKind::GitWorktreeRoot,
                    worktree.path(),
                )
                .unwrap(),
                execution_root: worktree.path().to_path_buf(),
                projection_kind: WorkspaceProjectionKind::GitWorktreeRoot,
                access_mode: WorkspaceAccessMode::ExclusiveWrite,
                cwd: worktree.path().to_path_buf(),
                occupancy_id: None,
                projection_metadata: None,
            });
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let loaded = runtime.loaded_agents_md().await.unwrap();
        assert_eq!(
            loaded
                .workspace_source
                .as_ref()
                .map(|source| source.path.clone()),
            Some(workspace_agents_md)
        );
    }

    #[tokio::test]
    async fn detached_agent_does_not_load_workspace_agents_md() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        std::fs::write(workspace.path().join("AGENTS.md"), "workspace rules").unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            InitialWorkspaceBinding::Detached,
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let loaded = runtime.loaded_agents_md().await.unwrap();
        assert!(loaded.workspace_source.is_none());
    }

    #[tokio::test]
    async fn filtered_tool_specs_keep_exec_command_visible_when_process_execution_disabled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.execution_profile.process_execution_exposed = false;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }
        let identity = runtime.agent_identity_view().await.unwrap();
        let tools = runtime.filtered_tool_specs(&identity).unwrap();

        assert!(tools.iter().any(|tool| tool.name == "ExecCommand"));
    }

    #[tokio::test]
    async fn filtered_tool_specs_do_not_expose_worktree_discard() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.execution_profile.supports_managed_worktrees = false;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }
        let identity = runtime.agent_identity_view().await.unwrap();
        let tools = runtime.filtered_tool_specs(&identity).unwrap();

        assert!(!tools.iter().any(|tool| tool.name == "WorktreeTaskDiscard"));
    }

    #[tokio::test]
    async fn filtered_tool_specs_expose_no_public_task_creation_tool() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let identity = runtime.agent_identity_view().await.unwrap();
        let tools = runtime.filtered_tool_specs(&identity).unwrap();

        assert!(!tools.iter().any(|tool| tool.name == "CreateTask"));
    }

    #[tokio::test]
    async fn filtered_tool_specs_keep_spawn_agent_visible_without_host_bridge() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let identity = runtime.agent_identity_view().await.unwrap();

        let tools = runtime.filtered_tool_specs(&identity).unwrap();

        assert!(tools.iter().any(|tool| tool.name == "SpawnAgent"));
    }

    #[tokio::test]
    async fn filtered_tool_specs_hide_agent_creation_family_for_private_child() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let tools = runtime
            .filtered_tool_specs(&private_child_identity("tmp_child_demo"))
            .unwrap();

        assert!(!tools.iter().any(|tool| tool.name == "SpawnAgent"));
    }

    #[tokio::test]
    async fn filtered_tool_specs_keep_use_workspace_visible_for_private_child() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let tools = runtime
            .filtered_tool_specs(&private_child_identity("tmp_child_demo"))
            .unwrap();

        assert!(tools.iter().any(|tool| tool.name == "UseWorkspace"));
        assert!(!tools.iter().any(|tool| tool.name == "EnterWorkspace"));
        assert!(!tools.iter().any(|tool| tool.name == "ExitWorkspace"));
    }

    #[tokio::test]
    async fn filtered_tool_specs_keep_agent_creation_family_for_public_named_agent() {
        let (_home, _host, runtime) = host_backed_test_runtime().await;
        let identity = runtime.agent_identity_view().await.unwrap();
        let tools = runtime.filtered_tool_specs(&identity).unwrap();

        assert!(tools.iter().any(|tool| tool.name == "SpawnAgent"));
        assert!(tools.iter().any(|tool| tool.name == "UseWorkspace"));
        assert!(!tools.iter().any(|tool| tool.name == "EnterWorkspace"));
        assert!(!tools.iter().any(|tool| tool.name == "ExitWorkspace"));
    }

    #[tokio::test]
    async fn schedule_command_task_rejects_when_process_execution_disabled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.execution_profile.process_execution_exposed = false;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let err = runtime
            .schedule_command_task(
                "demo".into(),
                crate::types::CommandTaskSpec {
                    cmd: "printf test".into(),
                    workdir: None,
                    shell: None,
                    login: true,
                    tty: false,
                    yield_time_ms: 100,
                    max_output_tokens: None,
                    accepts_input: false,
                    continue_on_result: false,
                },
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("process execution is disabled"));
    }

    #[tokio::test]
    async fn schedule_inherited_child_agent_task_rejects_when_background_tasks_disabled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.execution_profile.allow_background_tasks = false;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let err = runtime
            .schedule_child_agent_task(
                "demo".into(),
                "prompt".into(),
                TrustLevel::TrustedOperator,
                crate::types::ChildAgentWorkspaceMode::Inherit,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("background tasks are disabled"));
    }

    #[tokio::test]
    async fn schedule_command_task_rejects_when_background_tasks_disabled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.execution_profile.allow_background_tasks = false;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let err = runtime
            .schedule_command_task(
                "demo".into(),
                crate::types::CommandTaskSpec {
                    cmd: "printf test".into(),
                    workdir: None,
                    shell: None,
                    login: true,
                    tty: false,
                    yield_time_ms: 100,
                    max_output_tokens: None,
                    accepts_input: false,
                    continue_on_result: false,
                },
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("background tasks are disabled"));
    }

    #[tokio::test]
    async fn stop_command_task_marks_cancelling_before_terminal_cancelled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let task = runtime
            .schedule_command_task(
                "long sleep".into(),
                crate::types::CommandTaskSpec {
                    cmd: "sleep 5".into(),
                    workdir: None,
                    shell: None,
                    login: true,
                    tty: false,
                    yield_time_ms: 10,
                    max_output_tokens: None,
                    accepts_input: false,
                    continue_on_result: false,
                },
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap();

        let stopped = runtime
            .stop_task(&task.id, &TrustLevel::TrustedOperator)
            .await
            .unwrap();
        assert_eq!(stopped.status, TaskStatus::Cancelling);

        let current = runtime.task_record(&task.id).await.unwrap().unwrap();
        assert_eq!(current.status, TaskStatus::Cancelling);

        let not_ready = runtime.task_output(&task.id, false, 0).await.unwrap();
        assert_eq!(
            not_ready.retrieval_status,
            TaskOutputRetrievalStatus::NotReady
        );
        assert_eq!(not_ready.task.status, TaskStatus::Cancelling);

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let current = runtime.task_record(&task.id).await.unwrap().unwrap();
            if current.status == TaskStatus::Cancelled {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn second_stop_requests_force_stop_and_runner_terminates_process_before_cancelled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let pid_file = dir.path().join("command-task.pid");
        let command = format!("echo $$ > {}; exec sleep 30", pid_file.display());
        let task = runtime
            .schedule_command_task(
                "force stop command".into(),
                crate::types::CommandTaskSpec {
                    cmd: command,
                    workdir: None,
                    shell: Some("sh".into()),
                    login: false,
                    tty: false,
                    yield_time_ms: 10,
                    max_output_tokens: None,
                    accepts_input: false,
                    continue_on_result: false,
                },
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pid_file.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .to_string();

        let stopped = runtime
            .stop_task(&task.id, &TrustLevel::TrustedOperator)
            .await
            .unwrap();
        assert_eq!(stopped.status, TaskStatus::Cancelling);
        assert_eq!(
            stopped
                .detail
                .as_ref()
                .and_then(|detail| detail.get("cancel_requested"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );

        let force_stopped = runtime
            .stop_task(&task.id, &TrustLevel::TrustedOperator)
            .await
            .unwrap();
        assert_eq!(force_stopped.status, TaskStatus::Cancelling);
        assert_eq!(
            force_stopped
                .detail
                .as_ref()
                .and_then(|detail| detail.get("force_stop_requested"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );

        let force_stop_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut saw_cancelling_while_pid_alive = false;
        while tokio::time::Instant::now() < force_stop_deadline {
            let current = runtime.task_record(&task.id).await.unwrap().unwrap();
            let pid_probe = std::process::Command::new("kill")
                .arg("-0")
                .arg(&pid)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            if current.status == TaskStatus::Cancelling && pid_probe.success() {
                saw_cancelling_while_pid_alive = true;
                break;
            }
            if current.status == TaskStatus::Cancelled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(saw_cancelling_while_pid_alive);

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let current = runtime.task_record(&task.id).await.unwrap().unwrap();
            let pid_probe = std::process::Command::new("kill")
                .arg("-0")
                .arg(&pid)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            if current.status == TaskStatus::Cancelled && !pid_probe.success() {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        assert!(!runtime
            .inner
            .task_handles
            .lock()
            .await
            .contains_key(&task.id));
        let pid_probe = std::process::Command::new("kill")
            .arg("-0")
            .arg(&pid)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!pid_probe.success());

        let current = runtime.task_record(&task.id).await.unwrap().unwrap();
        assert_eq!(current.status, TaskStatus::Cancelled);
        assert_eq!(
            current
                .detail
                .as_ref()
                .and_then(|detail| detail.get("cancelled_reason"))
                .and_then(serde_json::Value::as_str),
            Some("force_stop_requested")
        );
        assert_eq!(
            current
                .detail
                .as_ref()
                .and_then(|detail| detail.get("cancel_requested"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            current
                .detail
                .as_ref()
                .and_then(|detail| detail.get("force_stop_requested"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn cancelling_task_ignores_late_running_status_update() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let task = TaskRecord {
            id: "regression-task".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Cancelling,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some("regression task".into()),
            detail: Some(serde_json::json!({
                "task_status": "cancelling",
            })),
            recovery: None,
        };
        runtime.storage().append_task(&task).unwrap();

        let stale_running = TaskRecord {
            status: TaskStatus::Running,
            updated_at: Utc::now(),
            detail: Some(serde_json::json!({
                "task_status": "running",
            })),
            ..task.clone()
        };

        runtime
            .reduce_task_status_message(stale_running)
            .await
            .unwrap();

        let current = runtime.task_record(&task.id).await.unwrap().unwrap();
        assert_eq!(current.status, TaskStatus::Cancelling);
    }

    #[tokio::test]
    async fn latest_task_list_entries_return_compact_projection() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        runtime
            .storage()
            .append_task(&TaskRecord {
                id: "task-list-1".into(),
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                summary: Some("watch logs".into()),
                detail: Some(serde_json::json!({
                    "wait_policy": "blocking",
                    "cmd": "tail -f app.log",
                    "output_path": "/tmp/output.log",
                })),
                recovery: Some(TaskRecoverySpec::CommandTask {
                    summary: "watch logs".into(),
                    spec: crate::types::CommandTaskSpec {
                        cmd: "tail -f app.log".into(),
                        workdir: None,
                        shell: None,
                        login: true,
                        tty: false,
                        yield_time_ms: 100,
                        max_output_tokens: None,
                        accepts_input: false,
                        continue_on_result: true,
                    },
                    trust: TrustLevel::TrustedOperator,
                    promoted_from_exec_command: false,
                }),
            })
            .unwrap();

        let entries = runtime.latest_task_list_entries().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "task-list-1");
        assert_eq!(entries[0].status, TaskStatus::Running);
        assert_eq!(entries[0].summary.as_deref(), Some("watch logs"));
        assert_eq!(
            entries[0].wait_policy,
            crate::types::TaskWaitPolicy::Blocking
        );
    }

    #[tokio::test]
    async fn enter_git_worktree_root_rejects_when_managed_worktrees_disabled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.execution_profile.supports_managed_worktrees = false;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }
        let workspace_entry = WorkspaceEntry::new("ws-1", workspace.path().to_path_buf(), None);
        runtime.attach_workspace(&workspace_entry).await.unwrap();

        let err = runtime
            .enter_workspace(
                &workspace_entry,
                crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
                crate::system::WorkspaceAccessMode::ExclusiveWrite,
                None,
                Some("feature-1".into()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("git_worktree_root is disabled"));
    }

    #[tokio::test]
    async fn schedule_worktree_child_agent_task_rejects_when_background_tasks_disabled() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.execution_profile.allow_background_tasks = false;
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let err = runtime
            .schedule_child_agent_task(
                "demo".into(),
                "prompt".into(),
                TrustLevel::TrustedOperator,
                crate::types::ChildAgentWorkspaceMode::Worktree,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("background tasks are disabled"));
    }

    #[test]
    fn current_input_summary_extracts_body_from_context_section() {
        let prompt = EffectivePrompt {
            identity: AgentIdentityView {
                agent_id: "default".into(),
                kind: AgentKind::Default,
                visibility: AgentVisibility::Public,
                ownership: AgentOwnership::SelfOwned,
                profile_preset: AgentProfilePreset::PublicNamed,
                status: AgentRegistryStatus::Active,
                is_default_agent: true,
                parent_agent_id: None,
                lineage_parent_agent_id: None,
                delegated_from_task_id: None,
            },
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: vec![],
                workspace_id: None,
                workspace_anchor: PathBuf::from("/tmp/agent-home"),
                execution_root: PathBuf::from("/tmp/agent-home"),
                cwd: PathBuf::from("/tmp/agent-home"),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
            },
            loaded_agents_md: LoadedAgentsMd::default(),
            cache_identity: crate::prompt::PromptCacheIdentity {
                agent_id: "default".into(),
                prompt_cache_key: "default".into(),
                working_memory_revision: 1,
                compression_epoch: 0,
            },
            system_sections: vec![],
            context_sections: vec![PromptSection {
                name: "current_input".into(),
                id: "current_input".into(),
                content:
                    "Current input:\n- [operator][operator_instruction][OperatorPrompt] Fix the failing benchmark output."
                        .into(),
                stability: PromptStability::AgentScoped,
            }],
            rendered_system_prompt: String::new(),
            rendered_context_attachment: String::new(),
        };

        assert_eq!(
            current_input_summary(&prompt),
            "Fix the failing benchmark output."
        );
    }

    #[tokio::test]
    async fn interactive_turn_keeps_pending_working_memory_delta_when_prompt_omits_it() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                prompt_budget_estimated_tokens: 140,
                ..context_config()
            },
        )
        .unwrap();

        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.working_memory.current_working_memory =
                crate::types::WorkingMemorySnapshot {
                    delivery_target: Some("ship the prompt delta gating fix".into()),
                    current_plan: vec!["[InProgress] wire prompt render acknowledgement".into()],
                    ..crate::types::WorkingMemorySnapshot::default()
                };
            guard.state.working_memory.working_memory_revision = 5;
            guard.state.working_memory.pending_working_memory_delta =
                Some(crate::types::WorkingMemoryDelta {
                    from_revision: 4,
                    to_revision: 5,
                    created_at_turn: 7,
                    reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
                    changed_fields: vec!["current_plan".into()],
                    summary_lines: vec![
                        "updated the current plan with a long-form explanation of why prompt rendering acknowledgement must happen after budgeted assembly rather than before prompt construction".into(),
                        "recorded the continuity decision that pending deltas stay durable across turns until the model actually sees the delta section in a rendered prompt".into(),
                        "captured low-budget prompt coverage for the interactive runtime path that previously cleared the delta too early".into(),
                    ],
                });
            runtime.inner.storage.write_agent(&guard.state).unwrap();
        }

        let preview = runtime
            .preview_prompt(
                "Continue the runtime memory work and report the latest status.".into(),
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap();
        assert!(!preview
            .context_sections
            .iter()
            .any(|section| section.name == "working_memory_delta"));

        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the runtime memory work and report the latest status.".into(),
            },
        );
        runtime
            .process_interactive_message(
                &message,
                None,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        let state = runtime.agent_state().await.unwrap();
        let pending = state
            .working_memory
            .pending_working_memory_delta
            .as_ref()
            .expect("pending delta should remain until rendered");
        assert_eq!(pending.to_revision, 5);
        assert_eq!(
            state.working_memory.last_prompted_working_memory_revision,
            None
        );
    }

    struct TruncatingProvider {
        calls: Mutex<usize>,
    }

    struct TimelineProvider;

    struct OneToolThenTextProvider {
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl AgentProvider for OneToolThenTextProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            let blocks = if *calls == 1 {
                vec![ModelBlock::ToolUse {
                    id: "verify".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'ok'",
                        "shell": "sh",
                    }),
                }]
            } else {
                vec![ModelBlock::Text {
                    text: "done".into(),
                }]
            };
            Ok(ProviderTurnResponse {
                blocks,
                stop_reason: None,
                input_tokens: 10,
                output_tokens: 10,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }

    struct FailingTimelineProvider;

    struct ToolCaptureProvider {
        requests: Mutex<Vec<Vec<String>>>,
    }

    struct TurnLocalCompactionProbeProvider {
        calls: Mutex<usize>,
        requests: Mutex<Vec<ProviderTurnRequest>>,
    }

    struct BaselineOverBudgetProbeProvider {
        calls: Mutex<usize>,
    }

    struct ContextLengthExceededProvider;

    struct SleepOnlyToolProvider {
        calls: Mutex<usize>,
    }

    struct DisallowedToolThenTextProvider {
        calls: Mutex<usize>,
    }

    struct MaxOutputMutationToolProvider {
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl AgentProvider for TruncatingProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            if *calls == 1 {
                return Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::Text {
                        text: "Partial report heading:".into(),
                    }],
                    stop_reason: Some("max_tokens".into()),
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_usage: None,
                    request_diagnostics: None,
                });
            }

            assert!(request.conversation.iter().any(|message| match message {
                ConversationMessage::UserText(text) => {
                    text.contains("Output token limit hit")
                        || text.contains("Continue exactly where you left off")
                }
                _ => false,
            }));

            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "\n\n- final grounded recommendation".into(),
                }],
                stop_reason: None,
                input_tokens: 50,
                output_tokens: 25,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }

    #[async_trait]
    impl AgentProvider for TimelineProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "done with fallback history".into(),
                }],
                stop_reason: None,
                input_tokens: 12,
                output_tokens: 6,
                cache_usage: None,
                request_diagnostics: None,
            })
        }

        async fn complete_turn_with_diagnostics(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)> {
            let response = self.complete_turn(request).await?;
            Ok((
                response,
                Some(ProviderAttemptTimeline {
                    attempts: vec![
                        ProviderAttemptRecord {
                            provider: "openai".into(),
                            model_ref: "openai/gpt-5.4".into(),
                            attempt: 1,
                            max_attempts: 3,
                            failure_kind: Some("server_error".into()),
                            disposition: Some("retryable".into()),
                            outcome: ProviderAttemptOutcome::Retrying,
                            advanced_to_fallback: false,
                            backoff_ms: Some(200),
                            token_usage: None,
                            transport_diagnostics: None,
                        },
                        ProviderAttemptRecord {
                            provider: "anthropic".into(),
                            model_ref: "anthropic/claude-sonnet-4-6".into(),
                            attempt: 1,
                            max_attempts: 3,
                            failure_kind: None,
                            disposition: None,
                            outcome: ProviderAttemptOutcome::Succeeded,
                            advanced_to_fallback: false,
                            backoff_ms: None,
                            token_usage: Some(TokenUsage::new(12, 6)),
                            transport_diagnostics: None,
                        },
                    ],
                    aggregated_token_usage: Some(TokenUsage::new(12, 6)),
                    requested_model_ref: "openai/gpt-5.4".into(),
                    active_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                    winning_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                }),
            ))
        }
    }

    #[async_trait]
    impl AgentProvider for ToolCaptureProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            self.requests.lock().await.push(
                request
                    .tools
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>(),
            );
            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: "captured tool set".into(),
                }],
                stop_reason: None,
                input_tokens: 8,
                output_tokens: 4,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }

    #[async_trait]
    impl AgentProvider for TurnLocalCompactionProbeProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            self.requests.lock().await.push(request);
            let mut calls = self.calls.lock().await;
            *calls += 1;
            let response = match *calls {
                1 => ProviderTurnResponse {
                    blocks: vec![
                        ModelBlock::Text {
                            text: format!(
                                "Round 1 planning {}",
                                "very detailed preamble ".repeat(120)
                            ),
                        },
                        ModelBlock::ToolUse {
                            id: "exec-round-1".into(),
                            name: "ExecCommand".into(),
                            input: serde_json::json!({
                                "cmd": "printf 'first-round-output-should-not-stay-exact'",
                            }),
                        },
                    ],
                    stop_reason: Some("tool_use".into()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_usage: None,
                    request_diagnostics: None,
                },
                2 => ProviderTurnResponse {
                    blocks: vec![
                        ModelBlock::Text {
                            text: "Round 2 planning keep recent exact.".into(),
                        },
                        ModelBlock::ToolUse {
                            id: "exec-round-2".into(),
                            name: "ExecCommand".into(),
                            input: serde_json::json!({
                                "cmd": "printf 'second-round-output-should-remain-exact'",
                            }),
                        },
                    ],
                    stop_reason: Some("tool_use".into()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_usage: None,
                    request_diagnostics: None,
                },
                3 => ProviderTurnResponse {
                    blocks: vec![
                        ModelBlock::Text {
                            text: "Round 3 planning keep recent exact too.".into(),
                        },
                        ModelBlock::ToolUse {
                            id: "exec-round-3".into(),
                            name: "ExecCommand".into(),
                            input: serde_json::json!({
                                "cmd": "printf 'third-round-output-should-remain-exact'",
                            }),
                        },
                    ],
                    stop_reason: Some("tool_use".into()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_usage: None,
                    request_diagnostics: None,
                },
                _ => ProviderTurnResponse {
                    blocks: vec![ModelBlock::Text {
                        text: "Finished after compacted continuation.".into(),
                    }],
                    stop_reason: Some("end_turn".into()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_usage: None,
                    request_diagnostics: None,
                },
            };
            Ok(response)
        }
    }

    #[async_trait]
    impl AgentProvider for BaselineOverBudgetProbeProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            match *calls {
                1 => Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::ToolUse {
                        id: "exec-baseline-over-budget".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'baseline-over-budget'",
                        }),
                    }],
                    stop_reason: Some("tool_use".into()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_usage: None,
                    request_diagnostics: None,
                }),
                _ => panic!("continuation request should not be sent after baseline-over-budget"),
            }
        }
    }

    #[async_trait]
    impl AgentProvider for SleepOnlyToolProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            if *calls > 1 {
                anyhow::bail!("sleep-only round should not force another provider turn");
            }
            assert!(
                request.tools.iter().any(|tool| tool.name == "Sleep"),
                "Sleep must be visible in the provider tool surface"
            );

            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::ToolUse {
                    id: "sleep-1".into(),
                    name: "Sleep".into(),
                    input: serde_json::json!({
                        "reason": "waiting for review",
                        "duration_ms": 250,
                    }),
                }],
                stop_reason: None,
                input_tokens: 10,
                output_tokens: 5,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }

    #[async_trait]
    impl AgentProvider for DisallowedToolThenTextProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            match *calls {
                1 => Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::ToolUse {
                        id: "legacy-task".into(),
                        name: "CreateTask".into(),
                        input: serde_json::json!({
                            "prompt": "removed public task surface",
                        }),
                    }],
                    stop_reason: None,
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_usage: None,
                    request_diagnostics: None,
                }),
                2 => {
                    assert!(
                        request.conversation.iter().any(|message| matches!(
                            message,
                            ConversationMessage::UserToolResults(results)
                                if results.iter().any(|result|
                                    result.tool_use_id == "legacy-task"
                                        && result.is_error
                                        && result
                                            .error
                                            .as_ref()
                                            .is_some_and(|error| error.kind == "tool_not_exposed_for_round")
                                )
                        )),
                        "continuation should receive a structured error for the unavailable tool"
                    );
                    Ok(ProviderTurnResponse {
                        blocks: vec![ModelBlock::Text {
                            text: "Recovered after unavailable tool.".into(),
                        }],
                        stop_reason: None,
                        input_tokens: 10,
                        output_tokens: 5,
                        cache_usage: None,
                        request_diagnostics: None,
                    })
                }
                _ => anyhow::bail!("unexpected provider call after recovery text"),
            }
        }
    }

    #[async_trait]
    impl AgentProvider for MaxOutputMutationToolProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            match *calls {
                1 => Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::ToolUse {
                        id: "truncated-patch".into(),
                        name: "ApplyPatch".into(),
                        input: serde_json::json!({
                            "patch": "--- /dev/null\n+++ b/app.txt\n@@ -0,0 +1 @@\n+should-not-be-written\n",
                        }),
                    }],
                    stop_reason: Some("max_tokens".into()),
                    input_tokens: 20,
                    output_tokens: 10,
                    cache_usage: None,
                    request_diagnostics: None,
                }),
                2 => {
                    assert!(
                        request.conversation.iter().any(|message| matches!(
                            message,
                            ConversationMessage::UserToolResults(results)
                                if results.iter().any(|result|
                                    result.tool_use_id == "truncated-patch"
                                        && result.is_error
                                        && result
                                            .error
                                            .as_ref()
                                            .is_some_and(|error| error.kind == "truncated_mutation_tool_call")
                                )
                        )),
                        "continuation should receive a structured truncation error"
                    );
                    Ok(ProviderTurnResponse {
                        blocks: vec![ModelBlock::Text {
                            text: "Recovered after rejected truncated mutation.".into(),
                        }],
                        stop_reason: None,
                        input_tokens: 15,
                        output_tokens: 8,
                        cache_usage: None,
                        request_diagnostics: None,
                    })
                }
                _ => panic!("provider should stop after recovery"),
            }
        }
    }

    #[async_trait]
    impl AgentProvider for FailingTimelineProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            Err(provider_turn_error(
                "all configured providers failed for this turn: openai/gpt-5.4: fail_fast (contract_error): bad request",
                ProviderAttemptTimeline {
                    attempts: vec![ProviderAttemptRecord {
                        provider: "openai".into(),
                        model_ref: "openai/gpt-5.4".into(),
                        attempt: 1,
                        max_attempts: 3,
                        failure_kind: Some("contract_error".into()),
                        disposition: Some("fail_fast".into()),
                        outcome: ProviderAttemptOutcome::FailFastAborted,
                        advanced_to_fallback: false,
                        backoff_ms: None,
                        token_usage: None,
                        transport_diagnostics: Some(ProviderTransportDiagnostics {
                            stage: "request_send".into(),
                            provider: Some("openai".into()),
                            model_ref: Some("openai/gpt-5.4".into()),
                            url: Some(
                                "https://user:secret@example.com/v1/responses?api_key=token#frag"
                                    .into(),
                            ),
                            status: None,
                            reqwest: Some(ReqwestTransportDiagnostics {
                                is_timeout: false,
                                is_connect: false,
                                is_request: false,
                                is_body: true,
                                is_decode: false,
                                is_redirect: false,
                                status: None,
                            }),
                            source_chain: vec!["connection reset by peer".into()],
                        }),
                    }],
                    aggregated_token_usage: None,
                    requested_model_ref: "openai/gpt-5.4".into(),
                    active_model_ref: None,
                    winning_model_ref: None,
                },
                anyhow!("bad request"),
            ))
        }
    }

    #[async_trait]
    impl AgentProvider for ContextLengthExceededProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            Err(provider_turn_error(
                "all configured providers failed for this turn: openai-codex/gpt-5.3-codex-spark: fail_fast (contract_error): context_length_exceeded",
                ProviderAttemptTimeline {
                    attempts: vec![ProviderAttemptRecord {
                        provider: "openai-codex".into(),
                        model_ref: "openai-codex/gpt-5.3-codex-spark".into(),
                        attempt: 1,
                        max_attempts: 3,
                        failure_kind: Some("contract_error".into()),
                        disposition: Some("fail_fast".into()),
                        outcome: ProviderAttemptOutcome::FailFastAborted,
                        advanced_to_fallback: false,
                        backoff_ms: None,
                        token_usage: Some(TokenUsage::new(125_166, 0)),
                        transport_diagnostics: None,
                    }],
                    aggregated_token_usage: Some(TokenUsage::new(125_166, 0)),
                    requested_model_ref: "openai-codex/gpt-5.3-codex-spark".into(),
                    active_model_ref: None,
                    winning_model_ref: None,
                },
                anyhow!("context_length_exceeded: input too long"),
            ))
        }
    }

    #[tokio::test]
    async fn runtime_recovers_from_max_token_truncation() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(TruncatingProvider {
                calls: Mutex::new(0),
            }),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert!(outcome.final_text.contains("Partial report heading:"));
        assert!(outcome.final_text.contains("final grounded recommendation"));
    }

    #[tokio::test]
    async fn runtime_records_text_only_round_observations() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new(
                "I am still thinking through the runtime split before editing files.",
            )),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert!(outcome.final_text.contains("runtime split"));

        let events = runtime.storage().read_recent_events(10).unwrap();
        let provider_event = events
            .iter()
            .find(|event| event.kind == "provider_round_completed")
            .expect("missing provider_round_completed");
        assert_eq!(provider_event.data["round"], 1);
        assert_eq!(provider_event.data["tool_call_count"], 0);
        assert_eq!(provider_event.data["text_block_count"], 1);
        assert!(provider_event.data["text_preview"]
            .as_str()
            .unwrap()
            .contains("runtime split"));

        let text_only_event = events
            .iter()
            .find(|event| event.kind == "text_only_round_observed")
            .expect("missing text_only_round_observed");
        assert_eq!(text_only_event.data["has_text"], true);
        assert_eq!(text_only_event.data["triggered_recovery"], false);
        assert!(text_only_event.data["text_preview"]
            .as_str()
            .unwrap()
            .contains("runtime split"));
    }

    #[tokio::test]
    async fn first_provider_round_records_prompt_cache_identity_fields() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let mut prompt = test_effective_prompt();
        prompt.cache_identity.working_memory_revision = 7;
        prompt.cache_identity.compression_epoch = 3;
        prompt.cache_identity.prompt_cache_key = "default:wm7:ce3".into();

        runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                prompt,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        let events = runtime.storage().read_recent_events(10).unwrap();
        let provider_event = events
            .iter()
            .find(|event| event.kind == "provider_round_completed")
            .expect("missing provider_round_completed");
        assert_eq!(
            provider_event.data["prompt_cache_key"].as_str(),
            Some("default:wm7:ce3")
        );
        assert_eq!(
            provider_event.data["working_memory_revision"].as_u64(),
            Some(7)
        );
        assert_eq!(provider_event.data["compression_epoch"].as_u64(), Some(3));

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        let assistant_round = transcript
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
            .expect("missing assistant round transcript");
        assert_eq!(
            assistant_round.data["prompt_cache_key"].as_str(),
            Some("default:wm7:ce3")
        );
        assert_eq!(
            assistant_round.data["working_memory_revision"].as_u64(),
            Some(7)
        );
        assert_eq!(assistant_round.data["compression_epoch"].as_u64(), Some(3));
    }

    #[tokio::test]
    async fn sleep_only_tool_round_completes_without_extra_provider_turn() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(SleepOnlyToolProvider {
            calls: Mutex::new(0),
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(*provider.calls.lock().await, 1);
        assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
        assert!(outcome.final_text.is_empty());
        assert!(outcome.should_sleep);
        assert_eq!(outcome.sleep_duration_ms, Some(250));

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        assert_eq!(
            transcript
                .iter()
                .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
                .count(),
            1
        );
        assert!(transcript
            .iter()
            .any(|entry| entry.kind == TranscriptEntryKind::ToolResults));
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(
            state
                .last_turn_terminal
                .as_ref()
                .map(|terminal| terminal.kind),
            Some(TurnTerminalKind::Completed)
        );
    }

    #[tokio::test]
    async fn disallowed_tool_call_is_auditable_and_continuation_stays_valid() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(DisallowedToolThenTextProvider {
            calls: Mutex::new(0),
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.final_text, "Recovered after unavailable tool.");
        assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
        assert_eq!(*provider.calls.lock().await, 2);
        assert_eq!(
            runtime
                .storage()
                .read_recent_tool_executions(10)
                .unwrap()
                .len(),
            0
        );

        let events = runtime.storage().read_recent_events(20).unwrap();
        let failure_event = events
            .iter()
            .find(|event| event.kind == "tool_execution_failed")
            .expect("missing tool_execution_failed event");
        assert_eq!(failure_event.data["tool_name"].as_str(), Some("CreateTask"));
        assert_eq!(
            failure_event.data["reason"].as_str(),
            Some("tool_not_exposed_for_round")
        );
        assert_eq!(
            failure_event.data["error_kind"].as_str(),
            Some("tool_not_exposed_for_round")
        );

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        assert_eq!(
            transcript
                .iter()
                .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
                .count(),
            2
        );
        let tool_results = transcript
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::ToolResults)
            .expect("missing tool results transcript");
        assert_eq!(
            tool_results.data["results"][0]["tool_use_id"].as_str(),
            Some("legacy-task")
        );
        assert_eq!(
            tool_results.data["results"][0]["is_error"].as_bool(),
            Some(true)
        );
    }

    #[tokio::test]
    async fn max_output_mutation_tool_call_is_rejected_without_side_effects() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(MaxOutputMutationToolProvider {
            calls: Mutex::new(0),
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
        assert_eq!(
            outcome.final_text,
            "Recovered after rejected truncated mutation."
        );
        assert_eq!(*provider.calls.lock().await, 2);
        assert!(
            !workspace.path().join("app.txt").exists(),
            "ApplyPatch must not execute when the provider stopped at max_output_tokens"
        );
        assert_eq!(
            runtime
                .storage()
                .read_recent_tool_executions(10)
                .unwrap()
                .len(),
            0
        );

        let events = runtime.storage().read_recent_events(20).unwrap();
        let rejection_event = events
            .iter()
            .find(|event| event.kind == "truncated_mutation_tool_call_rejected")
            .expect("missing truncated_mutation_tool_call_rejected event");
        assert_eq!(
            rejection_event.data["tool_call_id"].as_str(),
            Some("truncated-patch")
        );
        assert_eq!(
            rejection_event.data["tool_name"].as_str(),
            Some("ApplyPatch")
        );
        assert_eq!(
            rejection_event.data["error_kind"].as_str(),
            Some("truncated_mutation_tool_call")
        );

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        let tool_results = transcript
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::ToolResults)
            .expect("missing tool results transcript");
        let content = tool_results.data["results"][0]["content"]
            .as_str()
            .expect("tool result content");
        assert!(content.contains("ApplyPatch failed"));
        assert!(content.contains("truncated_mutation_tool_call"));
        assert!(content.contains("max_tokens"));
        assert!(content.contains("retryable: true"));
        assert!(content.len() < 800);
    }

    #[tokio::test]
    async fn detached_runtime_provider_request_still_exposes_spawn_agent() {
        let dir = tempdir().unwrap();
        let provider = Arc::new(ToolCaptureProvider {
            requests: Mutex::new(Vec::new()),
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            InitialWorkspaceBinding::Detached,
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert!(outcome.final_text.contains("captured tool set"));
        let requests = provider.requests.lock().await;
        let tool_names = requests.last().expect("provider request should exist");
        assert!(
            tool_names.iter().any(|name| name == "SpawnAgent"),
            "detached runtime should still expose SpawnAgent to provider requests: {tool_names:?}"
        );
    }

    #[tokio::test]
    async fn turn_local_compaction_rewrites_older_rounds_into_runtime_recap() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(TurnLocalCompactionProbeProvider {
            calls: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            ContextConfig {
                prompt_budget_estimated_tokens: 3600,
                compaction_keep_recent_estimated_tokens: 180,
                ..context_config()
            },
        )
        .unwrap();

        let mut prompt = test_effective_prompt();
        prompt.system_sections = vec![PromptSection {
            name: "stable_system".into(),
            id: "stable_system".into(),
            content: "Keep runtime boundaries explicit.".into(),
            stability: PromptStability::Stable,
        }];
        prompt.context_sections = vec![PromptSection {
            name: "active_context".into(),
            id: "active_context".into(),
            content: "Preserve Anthropic prompt cache anchors across continuations.".into(),
            stability: PromptStability::AgentScoped,
        }];
        prompt.rendered_system_prompt = prompt
            .system_sections
            .iter()
            .map(render_section)
            .collect::<Vec<_>>()
            .join("\n\n");
        prompt.rendered_context_attachment = prompt
            .context_sections
            .iter()
            .map(render_section)
            .collect::<Vec<_>>()
            .join("\n\n");

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                prompt,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
        assert_eq!(*provider.calls.lock().await, 4);

        let requests = provider.requests.lock().await;
        let continuation_request = requests.get(3).expect("missing round 4 request");
        let cache = continuation_request
            .prompt_frame
            .cache
            .as_ref()
            .expect("continuation request should retain prompt cache identity");
        assert_eq!(cache.prompt_cache_key, "default");
        assert!(
            continuation_request
                .prompt_frame
                .system_blocks
                .iter()
                .any(|block| block.cache_breakpoint),
            "continuation request should retain cacheable system anchors"
        );
        let context_blocks = continuation_request
            .conversation
            .first()
            .and_then(|message| match message {
                ConversationMessage::UserBlocks(blocks) => Some(blocks),
                _ => None,
            })
            .expect("continuation request should retain structured context blocks");
        assert!(
            context_blocks.iter().any(|block| block.cache_breakpoint),
            "continuation request should retain cacheable context anchors"
        );
        let serialized_conversation = format!("{:?}", continuation_request.conversation);
        let events = runtime.storage().read_recent_events(50).unwrap();
        let round_four_event = events
            .iter()
            .find(|event| {
                event.kind == "provider_round_completed" && event.data["round"].as_u64() == Some(4)
            })
            .expect("missing round 4 provider completion event");
        assert_eq!(
            round_four_event.data["prompt_cache_key"].as_str(),
            Some("default")
        );
        assert_eq!(
            round_four_event.data["working_memory_revision"].as_u64(),
            Some(1)
        );
        assert_eq!(round_four_event.data["compression_epoch"].as_u64(), Some(0));
        let transcript = runtime.storage().read_recent_transcript(20).unwrap();
        let round_four_assistant = transcript
            .iter()
            .find(|entry| {
                entry.kind == TranscriptEntryKind::AssistantRound && entry.round == Some(4)
            })
            .expect("missing round 4 assistant transcript");
        assert_eq!(
            round_four_assistant.data["prompt_cache_key"].as_str(),
            Some("default")
        );
        let compaction_event = events
            .iter()
            .rev()
            .find(|event| event.kind == "turn_local_compaction_applied");
        if let Some(compaction_event) = compaction_event {
            assert!(
                !serialized_conversation.contains("first-round-output-should-not-stay-exact"),
                "older exact tool output should not survive after compaction: {serialized_conversation}"
            );
            let recap = continuation_request
                .conversation
                .iter()
                .find_map(|message| match message {
                    ConversationMessage::UserText(text)
                        if text.contains("Turn-local recap for older completed rounds") =>
                    {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .expect("missing deterministic recap after compaction");
            assert!(recap.contains("Round 1"), "unexpected recap: {recap}");
            assert!(
                recap.contains("ExecCommand completed exit_status=0"),
                "unexpected recap: {recap}"
            );
            assert!(!recap.contains("first-round-output-should-not-stay-exact"));
            assert!(serialized_conversation.contains("second-round-output-should-remain-exact"));
            assert!(serialized_conversation.contains("third-round-output-should-remain-exact"));
            assert!(
                compaction_event.data["compacted_rounds"]
                    .as_u64()
                    .unwrap_or_default()
                    >= 1
            );
            let checkpoint_request_id = compaction_event.data["checkpoint_request_id"]
                .as_str()
                .expect("compaction event missing checkpoint_request_id");
            let checkpoint_requested = events
                .iter()
                .find(|event| {
                    event.kind == "turn_local_checkpoint_requested"
                        && event.data["checkpoint_request_id"].as_str()
                            == Some(checkpoint_request_id)
                })
                .expect("missing structured checkpoint request event");
            let checkpoint_recorded = events
                .iter()
                .find(|event| {
                    event.kind == "turn_local_checkpoint_recorded"
                        && event.data["checkpoint_request_id"].as_str()
                            == Some(checkpoint_request_id)
                })
                .expect("missing structured checkpoint recorded event");
            assert_eq!(
                Some(checkpoint_request_id),
                checkpoint_requested.data["checkpoint_request_id"].as_str()
            );
            assert_eq!(
                Some(checkpoint_request_id),
                checkpoint_recorded.data["checkpoint_request_id"].as_str()
            );
            assert_eq!(
                checkpoint_recorded.data["checkpoint_recorded"].as_bool(),
                Some(true)
            );
            assert!(checkpoint_recorded.data["text_preview"]
                .as_str()
                .is_some_and(|preview| preview.contains("Finished after compacted continuation")));
        } else {
            assert!(serialized_conversation.contains("first-round-output-should-not-stay-exact"));
            assert!(serialized_conversation.contains("second-round-output-should-remain-exact"));
            assert!(serialized_conversation.contains("third-round-output-should-remain-exact"));
        }
        if let Some(checkpoint) = continuation_request
            .conversation
            .iter()
            .find_map(|message| match message {
                ConversationMessage::UserText(text)
                    if text.contains("progress checkpoint request") =>
                {
                    Some(text.clone())
                }
                _ => None,
            })
        {
            assert!(checkpoint.contains("current user goal"));
            assert!(checkpoint.contains("what remains unknown"));
            assert!(checkpoint.contains("next goal-aligned action"));
            assert!(checkpoint.contains("Do not assume the task requires code changes"));
            assert!(!checkpoint.contains("start editing"));
            assert!(!checkpoint.contains("begin implementation"));
        }
    }

    #[tokio::test]
    async fn turn_local_compaction_fails_fast_when_baseline_exceeds_budget() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(BaselineOverBudgetProbeProvider {
            calls: Mutex::new(0),
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            ContextConfig {
                prompt_budget_estimated_tokens: 320,
                compaction_keep_recent_estimated_tokens: 120,
                ..context_config()
            },
        )
        .unwrap();
        let mut prompt = test_effective_prompt();
        prompt.rendered_system_prompt = "system ".repeat(700);

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                prompt,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(*provider.calls.lock().await, 1);
        assert_eq!(outcome.terminal_kind, TurnTerminalKind::BaselineOverBudget);
        assert!(outcome
            .final_text
            .contains("continuation baseline exceeded the prompt budget"));

        let state = runtime.agent_state().await.unwrap();
        assert_eq!(
            state
                .last_turn_terminal
                .as_ref()
                .map(|terminal| terminal.kind),
            Some(TurnTerminalKind::BaselineOverBudget)
        );

        let events = runtime.storage().read_recent_events(20).unwrap();
        let baseline_event = events
            .iter()
            .find(|event| event.kind == "turn_local_baseline_over_budget")
            .expect("missing turn_local_baseline_over_budget event");
        assert_eq!(
            baseline_event.data["reason"].as_str(),
            Some("baseline_unfit")
        );
        assert!(
            baseline_event.data["estimated_baseline_tokens"]
                .as_u64()
                .unwrap_or_default()
                > baseline_event.data["effective_budget_estimated_tokens"]
                    .as_u64()
                    .unwrap_or_default()
        );
        assert!(
            events
                .iter()
                .all(|event| event.kind != "turn_local_compaction_applied"),
            "baseline-over-budget should fail fast, not masquerade as compaction"
        );
    }

    #[tokio::test]
    async fn context_length_exceeded_turn_fails_fast_without_runtime_error() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(ContextLengthExceededProvider),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "trigger provider context length fail-fast".into(),
            },
        );

        runtime
            .process_interactive_message(
                &message,
                None,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        let state = runtime.agent_state().await.unwrap();
        assert_eq!(
            state
                .last_turn_terminal
                .as_ref()
                .map(|terminal| terminal.kind),
            Some(TurnTerminalKind::Aborted)
        );

        let briefs = runtime.recent_briefs(10).await.unwrap();
        let failure = briefs
            .iter()
            .rev()
            .find(|brief| brief.kind == BriefKind::Failure)
            .expect("failure brief should exist");
        assert!(failure.text.contains("context_length_exceeded"));

        let events = runtime.storage().read_recent_events(20).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "turn_context_length_exceeded"));
        assert!(!events.iter().any(|event| event.kind == "runtime_error"));
    }

    #[tokio::test]
    async fn runtime_persists_provider_attempt_timeline_on_successful_round() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(TimelineProvider),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let _outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        let assistant_round = transcript
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
            .expect("missing assistant round transcript");
        let timeline = assistant_round.data["provider_attempt_timeline"]
            .as_object()
            .expect("missing provider attempt timeline");
        assert_eq!(
            timeline["winning_model_ref"].as_str(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(
            timeline["requested_model_ref"].as_str(),
            Some("openai/gpt-5.4")
        );
        assert_eq!(
            timeline["active_model_ref"].as_str(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(
            assistant_round.data["requested_model"].as_str(),
            Some("openai/gpt-5.4")
        );
        assert_eq!(
            assistant_round.data["active_model"].as_str(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(
            assistant_round.data["fallback_active"].as_bool(),
            Some(true)
        );
        assert_eq!(
            assistant_round.data["token_usage"]["total_tokens"].as_u64(),
            Some(18)
        );
        assert_eq!(timeline["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(
            timeline["aggregated_token_usage"]["total_tokens"].as_u64(),
            Some(18)
        );

        let events = runtime.storage().read_recent_events(10).unwrap();
        let provider_event = events
            .iter()
            .find(|event| event.kind == "provider_round_completed")
            .expect("missing provider_round_completed");
        assert_eq!(
            provider_event.data["token_usage"]["total_tokens"].as_u64(),
            Some(18)
        );
        assert_eq!(
            provider_event.data["provider_attempt_timeline"]["attempts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            provider_event.data["requested_model"].as_str(),
            Some("openai/gpt-5.4")
        );
        assert_eq!(
            provider_event.data["active_model"].as_str(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(provider_event.data["fallback_active"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn runtime_failure_artifacts_preserve_provider_attempt_timeline() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(FailingTimelineProvider),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Next,
            MessageBody::Text {
                text: "trigger provider failure".into(),
            },
        );
        let error = runtime
            .current_provider()
            .await
            .complete_turn(ProviderTurnRequest::plain(
                "system",
                vec![ConversationMessage::UserText("prompt".into())],
                Vec::new(),
            ))
            .await
            .unwrap_err();
        runtime
            .persist_runtime_failure_artifacts(&message, &error)
            .await
            .unwrap();

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        let failure = transcript
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::RuntimeFailure)
            .expect("missing runtime failure transcript");
        assert_eq!(
            failure.data["provider_attempt_timeline"]["attempts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            failure.data["provider_attempt_timeline"]["attempts"][0]["transport_diagnostics"]
                ["provider"],
            "openai"
        );
        assert_eq!(
            failure.data["provider_attempt_timeline"]["attempts"][0]["transport_diagnostics"]
                ["stage"],
            "request_send"
        );
        assert_eq!(
            failure.data["failure_artifact"]["metadata"]["url"],
            "https://example.com/v1/responses"
        );
        assert!(failure.data["token_usage"].is_null());
        assert!(failure.data["provider_attempt_timeline"]["winning_model_ref"].is_null());
        assert!(!failure.data["error_chain"].as_array().unwrap().is_empty());
    }

    struct StagnatingAfterVerificationProvider {
        calls: Mutex<usize>,
    }

    struct SkillReadProvider {
        calls: Mutex<usize>,
    }

    struct CountingProvider {
        calls: Mutex<usize>,
        reply: &'static str,
    }

    impl CountingProvider {
        async fn call_count(&self) -> usize {
            *self.calls.lock().await
        }
    }

    #[async_trait]
    impl AgentProvider for StagnatingAfterVerificationProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            if request.tools.is_empty() {
                return Ok(ProviderTurnResponse {
                    blocks: vec![ModelBlock::Text {
                        text: "What changed: app.txt\nWhy: to address the requested task.\nVerification: successful verification command completed.".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 25,
                    output_tokens: 25,
                    cache_usage: None,
            request_diagnostics: None,
                });
            }

            let mut calls = self.calls.lock().await;
            *calls += 1;

            let blocks = match *calls {
                1 => vec![
                    ModelBlock::ToolUse {
                        id: "patch".into(),
                        name: "ApplyPatch".into(),
                        input: serde_json::json!({
                            "patch": "--- a/app.txt\n+++ b/app.txt\n@@ -1,1 +1,1 @@\n-before\n+after\n",
                        }),
                    },
                    ModelBlock::ToolUse {
                        id: "verify".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'tests passed'",
                            "shell": "sh",
                        }),
                    },
                ],
                2 => vec![ModelBlock::ToolUse {
                    id: "read".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "cat app.txt",
                        "workdir": ".",
                    }),
                }],
                _ => vec![ModelBlock::ToolUse {
                    id: "agent".into(),
                    name: "AgentGet".into(),
                    input: serde_json::json!({}),
                }],
            };

            Ok(ProviderTurnResponse {
                blocks,
                stop_reason: None,
                input_tokens: 25,
                output_tokens: 25,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }

    #[async_trait]
    impl AgentProvider for SkillReadProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;

            let blocks = match *calls {
                1 => vec![ModelBlock::ToolUse {
                    id: "read-skill".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "cat .agents/skills/demo/SKILL.md",
                        "workdir": ".",
                    }),
                }],
                _ => vec![ModelBlock::Text {
                    text: "Skill loaded and applied.".into(),
                }],
            };

            Ok(ProviderTurnResponse {
                blocks,
                stop_reason: None,
                input_tokens: 20,
                output_tokens: 20,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }

    #[async_trait]
    impl AgentProvider for CountingProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            let mut calls = self.calls.lock().await;
            *calls += 1;
            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text {
                    text: self.reply.into(),
                }],
                stop_reason: None,
                input_tokens: 10,
                output_tokens: 5,
                cache_usage: None,
                request_diagnostics: None,
            })
        }
    }

    #[tokio::test]
    async fn non_model_visible_external_events_do_not_run_interactive_turn() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "should not run",
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let message = MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "test".into(),
                event_type: Some("ping".into()),
            },
            TrustLevel::UntrustedExternal,
            Priority::Normal,
            MessageBody::Text { text: "".into() },
        );

        runtime
            .process_message(message, closure_decision(ClosureOutcome::Completed, None))
            .await
            .unwrap();

        assert_eq!(provider.call_count().await, 0);
        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        assert!(transcript
            .iter()
            .all(|entry| entry.kind != TranscriptEntryKind::AssistantRound));
    }

    #[tokio::test]
    async fn model_visible_operator_and_timer_events_run_interactive_turn() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "ran interactive turn",
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let operator = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "plan the next step".into(),
            },
        );
        runtime
            .process_message(operator, closure_decision(ClosureOutcome::Completed, None))
            .await
            .unwrap();

        let timer = MessageEnvelope::new(
            "default",
            MessageKind::TimerTick,
            MessageOrigin::Timer {
                timer_id: "timer-1".into(),
            },
            TrustLevel::TrustedSystem,
            Priority::Normal,
            MessageBody::Text {
                text: "timer fired".into(),
            },
        );
        runtime
            .process_message(
                timer,
                closure_decision(ClosureOutcome::Waiting, Some(WaitingReason::AwaitingTimer)),
            )
            .await
            .unwrap();

        assert_eq!(provider.call_count().await, 2);
        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        assert!(
            transcript
                .iter()
                .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
                .count()
                >= 2
        );
    }

    #[tokio::test]
    async fn task_status_routes_only_through_task_state_reduction() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "should not run",
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let message = MessageEnvelope::new(
            "default",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: "task-1".into(),
            },
            TrustLevel::TrustedSystem,
            Priority::Normal,
            MessageBody::Text {
                text: "task running".into(),
            },
        );
        let mut message = message;
        message.metadata = Some(serde_json::json!({
            "task_id": "task-1",
            "task_kind": "child_agent_task",
            "task_status": "running",
            "task_summary": "task running",
            "task_detail": { "wait_policy": "blocking" },
        }));

        runtime
            .process_message(message, closure_decision(ClosureOutcome::Completed, None))
            .await
            .unwrap();

        assert_eq!(provider.call_count().await, 0);
        let tasks = runtime.latest_task_records().await.unwrap();
        assert!(tasks.iter().any(|task| task.id == "task-1"));
        let events = runtime.storage().read_recent_events(10).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "task_status_updated"));
    }

    #[tokio::test]
    async fn task_result_routes_through_reduction_and_follow_up_behavior() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "task follow-up",
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();
        runtime
            .storage()
            .append_task(&TaskRecord {
                id: "task-1".into(),
                agent_id: "default".into(),
                kind: TaskKind::ChildAgentTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                summary: Some("task running".into()),
                detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
                recovery: None,
            })
            .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.active_task_ids.push("task-1".into());
            runtime.storage().write_agent(&guard.state).unwrap();
        }

        let message = MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "task-1".into(),
            },
            TrustLevel::TrustedSystem,
            Priority::Normal,
            MessageBody::Text {
                text: "task completed".into(),
            },
        );
        let mut message = message;
        message.metadata = Some(serde_json::json!({
            "task_id": "task-1",
            "task_kind": "child_agent_task",
            "task_status": "completed",
            "task_summary": "task completed",
            "task_detail": { "wait_policy": "blocking" },
        }));

        runtime
            .process_message(
                message,
                closure_decision(
                    ClosureOutcome::Waiting,
                    Some(WaitingReason::AwaitingTaskResult),
                ),
            )
            .await
            .unwrap();

        assert_eq!(provider.call_count().await, 1);
        let state = runtime.agent_state().await.unwrap();
        assert!(!state.active_task_ids.contains(&"task-1".to_string()));
        let events = runtime.storage().read_recent_events(100).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "task_result_received"));
    }

    #[tokio::test]
    async fn task_result_persists_reduced_state_when_agent_status_is_not_mutable() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: Mutex::new(0),
            reply: "should not run",
        });
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "default".into(),
            context_config(),
        )
        .unwrap();
        {
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.status = AgentStatus::Paused;
            guard.state.active_task_ids.push("task-1".into());
            runtime.storage().write_agent(&guard.state).unwrap();
        }

        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "task-1".into(),
            },
            TrustLevel::TrustedSystem,
            Priority::Normal,
            MessageBody::Text {
                text: "task completed".into(),
            },
        );
        message.metadata = Some(serde_json::json!({
            "task_id": "task-1",
            "task_kind": "child_agent_task",
            "task_status": "completed",
            "task_summary": "task completed",
            "task_detail": { "wait_policy": "blocking" },
        }));

        runtime
            .process_message(message, closure_decision(ClosureOutcome::Completed, None))
            .await
            .unwrap();

        assert_eq!(provider.call_count().await, 0);
        let persisted = runtime
            .storage()
            .read_agent()
            .unwrap()
            .expect("agent state should be persisted");
        assert_eq!(persisted.status, AgentStatus::Paused);
        assert!(!persisted.active_task_ids.contains(&"task-1".to_string()));
    }

    #[tokio::test]
    async fn unknown_control_action_fails_without_mutating_runtime_state() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();
        let before = runtime.agent_state().await.unwrap();

        let message = MessageEnvelope::new(
            "default",
            MessageKind::Control,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Next,
            MessageBody::Text {
                text: "bogus".into(),
            },
        );
        let error = runtime
            .process_message(message, closure_decision(ClosureOutcome::Completed, None))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("unknown control action"));
        let after = runtime.agent_state().await.unwrap();
        assert_eq!(after.status, before.status);
        assert_eq!(after.current_run_id, before.current_run_id);
    }

    #[tokio::test]
    async fn final_status_rewrite_preserves_paused_stopped_and_asleep_states() {
        for status in [
            AgentStatus::Paused,
            AgentStatus::Stopped,
            AgentStatus::Asleep,
        ] {
            let dir = tempdir().unwrap();
            let workspace = tempdir().unwrap();
            let runtime = RuntimeHandle::new(
                "default",
                dir.path().to_path_buf(),
                workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),
                Arc::new(StubProvider::new("unused")),
                "default".into(),
                context_config(),
            )
            .unwrap();
            {
                let mut guard = runtime.inner.agent.lock().await;
                guard.state.status = status.clone();
                runtime.storage().write_agent(&guard.state).unwrap();
            }

            let message = MessageEnvelope::new(
                "default",
                MessageKind::WebhookEvent,
                MessageOrigin::Webhook {
                    source: "test".into(),
                    event_type: Some("ping".into()),
                },
                TrustLevel::UntrustedExternal,
                Priority::Normal,
                MessageBody::Text { text: "".into() },
            );

            runtime
                .process_message(message, closure_decision(ClosureOutcome::Completed, None))
                .await
                .unwrap();
            let state = runtime.agent_state().await.unwrap();
            assert_eq!(state.status, status);
        }
    }

    #[test]
    fn incoming_transcript_entries_preserve_delivery_surface_and_correlation_metadata() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("unused")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::WebhookEvent,
            MessageOrigin::Webhook {
                source: "github".into(),
                event_type: Some("issue_comment".into()),
            },
            TrustLevel::TrustedIntegration,
            Priority::Normal,
            MessageBody::Text {
                text: "payload".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::HttpWebhook,
            AdmissionContext::PublicUnauthenticated,
        );
        message.correlation_id = Some("corr-1".into());
        message.causation_id = Some("cause-1".into());

        runtime.record_incoming_transcript_entry(&message).unwrap();

        let transcript = runtime.storage().read_recent_transcript(10).unwrap();
        let entry = transcript.last().expect("incoming transcript entry");
        assert_eq!(
            entry.data["delivery_surface"].as_str(),
            Some("http_webhook")
        );
        assert_eq!(
            entry.data["admission_context"].as_str(),
            Some("public_unauthenticated")
        );
        assert_eq!(
            entry.data["authority_class"].as_str(),
            Some("integration_signal")
        );
        assert_eq!(entry.data["correlation_id"].as_str(), Some("corr-1"));
        assert_eq!(entry.data["causation_id"].as_str(), Some("cause-1"));
    }

    #[tokio::test]
    async fn runtime_does_not_force_completion_after_post_verification_stagnation() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        std::fs::write(workspace.path().join("app.txt"), "before").unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StagnatingAfterVerificationProvider {
                calls: Mutex::new(0),
            }),
            "default".into(),
            context_config(),
        )
        .unwrap();

        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: Some(3),
                },
            )
            .await
            .unwrap();

        assert!(
            !outcome.should_sleep,
            "runtime should not force terminal delivery after exploratory rounds"
        );
        assert!(
            outcome
                .final_text
                .contains("Stopped after reaching the maximum tool loop depth (3)."),
            "unexpected final_text: {}",
            outcome.final_text
        );
    }

    #[tokio::test]
    async fn reading_discovered_skill_marks_it_active_and_promotes_on_success() {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let skill_dir = workspace.path().join(".agents/skills/demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: demo skill\n---\nFollow the demo workflow.",
        )
        .unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(SkillReadProvider {
                calls: Mutex::new(0),
            }),
            "default".into(),
            ContextConfig {
                prompt_budget_estimated_tokens: 65536,
                compaction_keep_recent_estimated_tokens: 4096,
                ..context_config()
            },
        )
        .unwrap();

        runtime.begin_interactive_turn(None, None).await.unwrap();
        let prompt = runtime
            .preview_prompt(
                "use the demo skill".to_string(),
                TrustLevel::TrustedOperator,
            )
            .await
            .unwrap();
        let outcome = runtime
            .run_agent_loop(
                "default",
                TrustLevel::TrustedOperator,
                prompt,
                LoopControlOptions {
                    max_tool_rounds: None,
                },
            )
            .await
            .unwrap();
        runtime.promote_turn_active_skills().await.unwrap();

        assert_eq!(outcome.terminal_kind, TurnTerminalKind::Completed);
        let state = runtime.agent_state().await.unwrap();
        assert_eq!(state.active_skills.len(), 1);
        let skill = &state.active_skills[0];
        assert_eq!(skill.skill_id, "workspace:demo");
        assert_eq!(
            skill.activation_source,
            SkillActivationSource::ImplicitFromCatalog
        );
        assert_eq!(skill.activation_state, SkillActivationState::SessionActive);
        assert_eq!(skill.activated_at_turn, state.turn_index);

        let events = runtime.storage().read_recent_events(20).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "skill_activated" && event.data["skill_id"] == "workspace:demo"
        }));
    }

    #[test]
    fn sanitize_subagent_result_removes_think_and_tool_markup() {
        let input = r#"I'll inspect the workspace first.
<think>
hidden planning
</think>
**[SYSTEM] Updating plan...**
<list_files>
<path>.</path>
</list_files>
Final concise answer."#;

        let cleaned = sanitize_subagent_result(input);
        assert!(!cleaned.contains("<think>"));
        assert!(!cleaned.contains("<list_files>"));
        assert!(!cleaned.contains("[SYSTEM]"));
        assert!(cleaned.contains("I'll inspect the workspace first."));
        assert!(cleaned.contains("Final concise answer."));
    }

    #[test]
    fn sanitize_subagent_result_removes_single_line_tool_markup_and_system_lines() {
        let input = r#"[SYSTEM] Updating plan
Let me start by checking the workspace.
<read_file path="src/runtime.rs"></read_file>
Final answer with grounded content."#;

        let cleaned = sanitize_subagent_result(input);
        assert!(!cleaned.contains("[SYSTEM]"));
        assert!(!cleaned.contains("<read_file"));
        assert!(cleaned.contains("Let me start by checking the workspace."));
        assert!(cleaned.contains("Final answer with grounded content."));
    }

    #[test]
    fn sanitize_subagent_result_drops_unclosed_think_block() {
        let input = "I'll inspect this first.\n<think>\nhidden\nstill hidden";
        let cleaned = sanitize_subagent_result(input);
        assert_eq!(cleaned, "I'll inspect this first.");
    }

    #[test]
    fn sanitize_subagent_result_preserves_english_result_prefixes() {
        let cleaned = sanitize_subagent_result(
            "I will update src/runtime/subagent.rs and verify with cargo test.",
        );
        assert_eq!(
            cleaned,
            "I will update src/runtime/subagent.rs and verify with cargo test."
        );
    }

    #[test]
    fn sanitize_subagent_result_preserves_chinese_final_report() {
        let input =
            "结论：已经定位到问题。\n相关文件：src/runtime/subagent.rs\n验证：cargo test -q";
        let cleaned = sanitize_subagent_result(input);
        assert_eq!(cleaned, input);
    }

    #[test]
    fn runtime_failure_summary_preserves_exact_limit_without_ellipsis() {
        let message = "x".repeat(200);
        let error = anyhow!(message.clone());

        let summary = RuntimeHandle::summarize_runtime_failure_error(&error);

        assert_eq!(summary, message);
        assert_eq!(summary.chars().count(), 200);
        assert!(!summary.ends_with('…'));
    }

    #[test]
    fn runtime_failure_summary_keeps_prefix_for_long_single_segment() {
        let message = "x".repeat(260);
        let error = anyhow!(message);

        let summary = RuntimeHandle::summarize_runtime_failure_error(&error);

        assert_eq!(summary.chars().count(), 200);
        assert!(summary.ends_with('…'));
        assert!(summary.starts_with(&"x".repeat(16)));
        assert_ne!(summary, "…");
    }

    #[test]
    fn runtime_failure_summary_truncates_exact_budget_before_ellipsis() {
        let message = format!("{} {}", "x".repeat(200), "tail");
        let error = anyhow!(message);

        let summary = RuntimeHandle::summarize_runtime_failure_error(&error);
        let expected = format!("{}…", "x".repeat(199));

        assert_eq!(summary.chars().count(), 200);
        assert!(summary.ends_with('…'));
        assert_eq!(summary, expected);
    }

    #[test]
    fn wake_hint_preserved_when_replaced_during_critical_window() {
        use tokio::runtime::Runtime;

        // Enable checkpoint mechanism for this test
        crate::runtime::test_util::enable_checkpoint();

        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let rt = Runtime::new().unwrap();

        // Create agent with idle status and an initial wake hint
        let mut agent = AgentState::new("default");
        agent.status = AgentStatus::AwakeIdle;
        agent.pending_wake_hint = Some(PendingWakeHint {
            reason: "original-hint".into(),
            source: Some("test".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: Some("corr-original".into()),
            causation_id: None,
            created_at: Utc::now(),
        });
        storage.write_agent(&agent).unwrap();

        let runtime = RuntimeHandle::new(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            context_config(),
        )
        .unwrap();

        // Verify the hint is set
        rt.block_on(async {
            let state = runtime.agent_state().await.unwrap();
            assert!(state.pending_wake_hint.is_some());
            assert_eq!(
                state.pending_wake_hint.as_ref().unwrap().reason,
                "original-hint"
            );
        });

        // Spawn emit task in background - it will:
        // 1. Read "original-hint"
        // 2. Complete emit
        // 3. Block at checkpoint waiting for our signal
        let runtime_clone = runtime.clone();
        let emit_handle = std::thread::spawn(move || {
            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                // This will block at the checkpoint after emit completes
                runtime_clone
                    .maybe_emit_pending_system_tick(None)
                    .await
                    .unwrap()
            })
        });

        // Wait for the emit thread to reach the checkpoint
        // At this point:
        // - "original-hint" has been emitted as SystemTick
        // - The checkpoint notify is waiting
        // - The lock has NOT been reacquired yet
        rt.block_on(async {
            crate::runtime::test_util::wait_for_emit_at_checkpoint().await;
        });

        // NOW we're in the critical window: emit done, lock not held yet
        // Replace the hint while emit thread is blocked at checkpoint
        rt.block_on(async {
            // Acquire the lock and update the hint
            let mut guard = runtime.inner.agent.lock().await;
            guard.state.pending_wake_hint = Some(PendingWakeHint {
                reason: "new-hint".into(),
                source: Some("test".into()),
                resource: None,
                body: None,
                content_type: None,
                correlation_id: Some("corr-new".into()),
                causation_id: None,
                created_at: Utc::now(),
            });
            runtime.inner.storage.write_agent(&guard.state).unwrap();
            drop(guard);
        });

        // Release the checkpoint - let emit thread continue
        crate::runtime::test_util::release_checkpoint();

        // Wait for emit thread to finish
        emit_handle.join().unwrap();

        // Verify the NEW hint is preserved (not cleared by the old hint's comparison)
        rt.block_on(async {
            let state = runtime.agent_state().await.unwrap();
            assert!(state.pending_wake_hint.is_some());
            assert_eq!(state.pending_wake_hint.as_ref().unwrap().reason, "new-hint");
        });

        // Verify the SystemTick event was emitted
        let events = runtime.storage().read_recent_events(10).unwrap();
        assert!(events.iter().any(|e| e.kind == "system_tick_emitted"));

        // Disable checkpoint mechanism
        crate::runtime::test_util::disable_checkpoint();
    }
}
