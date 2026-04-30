use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use serde_json::json;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{
    config::{AppConfig, RuntimeModelCatalog},
    context::ContextConfig,
    host::RuntimeHostBridge,
    provider::{build_provider_from_model_chain, AgentProvider},
    queue::RuntimeQueue,
    storage::AppStorage,
    system::{WorkspaceAccessMode, WorkspaceProjectionKind, LocalSystem},
    tool::ToolRegistry,
    types::{
        ActiveWorkspaceEntry, AuditEvent, ResolvedModelAvailability, SkillActivationSource,
        SkillActivationState, AgentStatus, AGENT_HOME_WORKSPACE_ID,
    },
};
use uuid::Uuid;

use super::{RuntimeAgent, RuntimeHandle, RuntimeInner, workspace};

#[derive(Debug, Clone)]
pub(super) struct ProviderReconfigurator {
    pub config: AppConfig,
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

    pub(crate) async fn current_provider(&self) -> Arc<dyn AgentProvider> {
        self.inner.provider.read().await.clone()
    }

    pub(crate) fn model_state_for(&self, state: &crate::types::AgentState) -> crate::types::AgentModelState {
        let effective_model = self
            .inner
            .model_catalog
            .effective_model(state.model_override.as_ref());
        let active_model = state
            .last_requested_model
            .as_ref()
            .filter(|requested| *requested == &effective_model)
            .and_then(|_| state.last_active_model.clone())
            .unwrap_or_else(|| effective_model.clone());
        let fallback_active = active_model != effective_model;
        let effective_chain = self
            .inner
            .model_catalog
            .provider_chain(state.model_override.as_ref());
        let resolved_policy = self.inner.model_catalog.resolved_model_policy(
            &self.inner.base_context_config,
            state.model_override.as_ref(),
        );
        crate::types::AgentModelState {
            source: if state.model_override.is_some() {
                crate::types::AgentModelSource::AgentOverride
            } else {
                crate::types::AgentModelSource::RuntimeDefault
            },
            runtime_default_model: self.inner.model_catalog.default_model.clone(),
            effective_model: effective_model.clone(),
            requested_model: Some(effective_model),
            active_model: Some(active_model),
            fallback_active,
            effective_fallback_models: effective_chain.into_iter().skip(1).collect(),
            override_model: state.model_override.clone(),
            resolved_policy,
            available_models: self.inner.model_catalog.available_models(),
            model_availability: self.inner.model_availability.clone(),
        }
    }

    pub(crate) async fn reconfigure_provider_for_state(
        &self,
        state: &crate::types::AgentState,
    ) -> Result<()> {
        let Some(reconfig) = self.inner.provider_reconfig.as_ref() else {
            return Err(anyhow!(
                "agent model override is unavailable for runtimes without host-managed provider configuration"
            ));
        };
        let chain = self
            .inner
            .model_catalog
            .provider_chain(state.model_override.as_ref());
        let resolved_context_config = self.inner.model_catalog.resolved_context_config(
            &self.inner.base_context_config,
            state.model_override.as_ref(),
        );
        let mut provider_config = reconfig.config.clone();
        provider_config.runtime_max_output_tokens = self
            .inner
            .model_catalog
            .resolved_model_policy(
                &self.inner.base_context_config,
                state.model_override.as_ref(),
            )
            .runtime_max_output_tokens;
        let provider = build_provider_from_model_chain(&provider_config, &chain)?;
        *self.inner.provider.write().await = provider;
        *self.inner.context_config.write().await = resolved_context_config;
        Ok(())
    }

    pub(crate) async fn current_context_config(&self) -> ContextConfig {
        self.inner.context_config.read().await.clone()
    }

    pub(super) fn new_internal(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<crate::runtime::InitialWorkspaceBinding>,
        callback_base_url: String,
        provider: Arc<dyn AgentProvider>,
        default_agent_id: String,
        base_context_config: ContextConfig,
        context_config: ContextConfig,
        model_catalog: RuntimeModelCatalog,
        model_availability: Vec<ResolvedModelAvailability>,
        provider_reconfig: Option<ProviderReconfigurator>,
        host_bridge: Option<RuntimeHostBridge>,
    ) -> Result<Self> {
        let mut provider = provider;
        let storage = AppStorage::new(data_dir)?;
        let agent_id = agent_id.into();
        let initial_workspace = initial_workspace.into();
        let initial_workspace_entry = match &initial_workspace {
            crate::runtime::InitialWorkspaceBinding::Entry(entry) => Some(entry.clone()),
            crate::runtime::InitialWorkspaceBinding::Anchor(anchor) => Some(crate::types::WorkspaceEntry::new(
                format!("ws-{}", Uuid::new_v4().simple()),
                anchor.clone(),
                anchor
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToString::to_string),
            )),
            crate::runtime::InitialWorkspaceBinding::Detached => {
                Some(Self::agent_home_workspace_entry(storage.data_dir()))
            }
        };
        if let Some(workspace) = initial_workspace_entry.as_ref() {
            let known = storage.latest_workspace_entries()?;
            if !known
                .iter()
                .any(|entry| entry.workspace_id == workspace.workspace_id)
            {
                storage.append_workspace_entry(workspace)?;
            }
        }
        let snapshot = storage.recovery_snapshot()?;
        let mut queue = RuntimeQueue::default();
        for message in &snapshot.replay_messages {
            queue.push(message.clone());
        }
        let recovered_agent = snapshot.agent;
        let recovered_from_storage = recovered_agent.is_some();
        let mut state = recovered_agent.unwrap_or_else(|| crate::types::AgentState::new(agent_id.clone()));
        if state.attached_workspaces.is_empty() {
            if let Some(workspace) = initial_workspace_entry.as_ref() {
                let should_seed_initial_binding = !recovered_from_storage
                    || state
                        .active_workspace_entry
                        .as_ref()
                        .is_some_and(|entry| entry.workspace_id == workspace.workspace_id);
                if should_seed_initial_binding {
                    state.attached_workspaces.push(workspace.workspace_id.clone());
                }
            }
        }
        // Initialize active_workspace_entry for new agents
        if state.active_workspace_entry.is_none() {
            if let Some(workspace) = initial_workspace_entry.as_ref() {
                state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                    workspace_id: workspace.workspace_id.clone(),
                    workspace_anchor: workspace.workspace_anchor.clone(),
                    execution_root_id: Self::build_execution_root_id(
                        &workspace.workspace_id,
                        WorkspaceProjectionKind::CanonicalRoot,
                        &workspace.workspace_anchor,
                    )?,
                    execution_root: workspace.workspace_anchor.clone(),
                    projection_kind: WorkspaceProjectionKind::CanonicalRoot,
                    access_mode: WorkspaceAccessMode::ExclusiveWrite,
                    cwd: workspace.workspace_anchor.clone(),
                    occupancy_id: None,
                    projection_metadata: None,
                });
            }
        }
        // Handle missing worktree recovery: clear session and reset cwd
        if state
            .worktree_session
            .as_ref()
            .is_some_and(|worktree| !worktree.worktree_path.exists())
        {
            storage.append_event(&crate::types::AuditEvent::new(
                "recovery_cleared_missing_worktree_session",
                json!({
                    "agent_id": agent_id,
                    "worktree_path": state
                        .worktree_session
                        .as_ref()
                        .map(|w| w.worktree_path.display().to_string()),
                    "reason": "worktree_path_does_not_exist"
                }),
            ))?;
            state.worktree_session = None;
            if state
                .active_workspace_entry
                .as_ref()
                .is_some_and(|entry| entry.projection_kind == WorkspaceProjectionKind::GitWorktreeRoot)
            {
                // Replace the missing worktree entry with a canonical AgentHome entry
                let agent_home = storage.data_dir();
                let agent_home_id = format!("agent_home-{}", agent_id);
                let kind = WorkspaceProjectionKind::CanonicalRoot;
                state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                    workspace_id: agent_home_id.clone(),
                    workspace_anchor: agent_home.to_path_buf(),
                    execution_root_id: Self::build_execution_root_id(
                        &agent_home_id,
                        kind,
                        &agent_home,
                    )?,
                    execution_root: agent_home.to_path_buf(),
                    projection_kind: kind,
                    access_mode: WorkspaceAccessMode::ExclusiveWrite,
                    cwd: agent_home.to_path_buf(),
                    occupancy_id: None,
                    projection_metadata: None,
                });
            }
        }
        let active_tasks = snapshot
            .active_tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let blocking_active_tasks = snapshot
            .active_tasks
            .iter()
            .filter(|task| task.is_blocking())
            .count();
        state
            .active_skills
            .retain(|skill| matches!(skill.activation_state, SkillActivationState::SessionActive));
        for skill in &mut state.active_skills {
            skill.activation_source = SkillActivationSource::Restored;
        }
        state.active_task_ids = active_tasks;
        state.pending = queue.len();
        state.total_message_count = storage.count_messages().unwrap_or_default();
        if !matches!(state.status, AgentStatus::Paused | AgentStatus::Stopped) {
            if state.pending > 0 || state.pending_wake_hint.is_some() {
                state.status = AgentStatus::AwakeIdle;
            } else if blocking_active_tasks > 0 {
                state.status = AgentStatus::AwaitingTask;
            }
        }
        if let Some(reconfig) = provider_reconfig.as_ref() {
            let chain = model_catalog.provider_chain(state.model_override.as_ref());
            let mut provider_config = reconfig.config.clone();
            provider_config.runtime_max_output_tokens = model_catalog
                .resolved_model_policy(&base_context_config, state.model_override.as_ref())
                .runtime_max_output_tokens;
            provider = build_provider_from_model_chain(&provider_config, &chain)?;
        }
        storage.write_agent(&state)?;

        let resolved_context_config = if provider_reconfig.is_some() {
            model_catalog
                .resolved_context_config(&base_context_config, state.model_override.as_ref())
        } else {
            context_config.clone()
        };

        Ok(Self {
            inner: Arc::new(RuntimeInner {
                agent: Mutex::new(RuntimeAgent { state, queue }),
                notify: Notify::new(),
                storage,
                provider: RwLock::new(provider),
                provider_reconfig,
                model_catalog,
                model_availability,
                base_context_config,
                context_config: RwLock::new(resolved_context_config),
                callback_base_url,
                tools: ToolRegistry::new(PathBuf::new()),
                system: Arc::new(LocalSystem::new()),
                default_agent_id,
                host_bridge,
                task_handles: Mutex::new(std::collections::HashMap::new()),
                recovered_tasks: Mutex::new(Some(snapshot.active_tasks)),
                recovered_timers: Mutex::new(Some(snapshot.active_timers)),
                suppress_next_continue_active_tick: Mutex::new(false),
                shutdown_requested: AtomicBool::new(false),
            }),
        })
    }
}

pub(super) fn new(
    agent_id: impl Into<String>,
    data_dir: PathBuf,
    initial_workspace: impl Into<crate::runtime::InitialWorkspaceBinding>,
    callback_base_url: String,
    provider: Arc<dyn AgentProvider>,
    default_agent_id: String,
    context_config: ContextConfig,
) -> Result<RuntimeHandle> {
    let base_context_config = context_config.clone();
    RuntimeHandle::new_internal(
        agent_id,
        data_dir,
        initial_workspace,
        callback_base_url,
        provider,
        default_agent_id,
        base_context_config,
        context_config,
        RuntimeModelCatalog::default(),
        Vec::new(),
        None,
        None,
    )
}

pub(super) fn new_static_with_host_bridge(
    agent_id: impl Into<String>,
    data_dir: PathBuf,
    initial_workspace: impl Into<crate::runtime::InitialWorkspaceBinding>,
    callback_base_url: String,
    provider: Arc<dyn AgentProvider>,
    default_agent_id: String,
    context_config: ContextConfig,
    host_bridge: RuntimeHostBridge,
    model_catalog: RuntimeModelCatalog,
) -> Result<RuntimeHandle> {
    let base_context_config = context_config.clone();
    RuntimeHandle::new_internal(
        agent_id,
        data_dir,
        initial_workspace,
        callback_base_url,
        provider,
        default_agent_id,
        base_context_config,
        context_config,
        model_catalog,
        Vec::new(),
        None,
        Some(host_bridge),
    )
}

pub(super) fn new_reconfigurable_with_host_bridge(
    agent_id: impl Into<String>,
    data_dir: PathBuf,
    initial_workspace: impl Into<crate::runtime::InitialWorkspaceBinding>,
    callback_base_url: String,
    config: AppConfig,
    default_agent_id: String,
    context_config: ContextConfig,
    host_bridge: RuntimeHostBridge,
) -> Result<RuntimeHandle> {
    let model_catalog = RuntimeModelCatalog::from_config(&config);
    let model_availability = crate::provider::resolved_model_availability(&config);
    let base_context_config = context_config.clone();
    let mut provider_config = config.clone();
    provider_config.runtime_max_output_tokens = model_catalog
        .resolved_model_policy(&base_context_config, None)
        .runtime_max_output_tokens;
    let provider =
        build_provider_from_model_chain(&provider_config, &model_catalog.provider_chain(None))?;
    let resolved_context_config =
        model_catalog.resolved_context_config(&base_context_config, None);
    RuntimeHandle::new_internal(
        agent_id,
        data_dir,
        initial_workspace,
        callback_base_url,
        provider,
        default_agent_id,
        base_context_config,
        resolved_context_config,
        model_catalog,
        model_availability,
        Some(ProviderReconfigurator { config }),
        Some(host_bridge),
    )
}
