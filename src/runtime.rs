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
        ExternalTriggerCapability, ExternalTriggerRecord, ExternalTriggerScope,
        ExternalTriggerStatus, ExternalTriggerSummary, LoadedAgentsMd, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, PendingWakeHint,
        Priority, QueueEntryRecord, QueueEntryStatus, ResolvedModelAvailability,
        RuntimeFailurePhase, RuntimeFailureSummary, RuntimePosture, SkillActivationSource,
        SkillActivationState, SkillsRuntimeView, TaskKind, TaskRecord, TaskRecoverySpec,
        TaskStatus, TimerRecord, TimerStatus, ToolExecutionRecord, TranscriptEntry,
        TranscriptEntryKind, TrustLevel, WaitingIntentRecord, WaitingIntentStatus,
        WaitingIntentSummary, WorkItemState, WorkspaceEntry, AGENT_HOME_WORKSPACE_ID,
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
    default_tool_output_tokens: u64,
    max_tool_output_tokens: u64,
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
        if guard.state.current_turn_work_item_id.is_none() {
            guard.state.current_turn_work_item_id = guard.state.current_work_item_id.clone();
        }
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
            description: hint.description.clone(),
            source: hint.source.clone(),
            scope: hint.scope.clone(),
            waiting_intent_id: hint.waiting_intent_id.clone(),
            external_trigger_id: hint.external_trigger_id.clone(),
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
                "description": hint.description,
                "source": hint.source,
                "scope": hint.scope,
                "waiting_intent_id": hint.waiting_intent_id,
                "external_trigger_id": hint.external_trigger_id,
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
mod tests;
