use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

use anyhow::{anyhow, Result};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{
    config::{AppConfig, ModelRef, RuntimeModelCatalog},
    context::ContextConfig,
    host::RuntimeHostBridge,
    model_catalog::BuiltInModelMetadata,
    model_discovery::{discovery_cache_path, load_discovery_cache_at},
    provider::{build_provider_from_model_chain, resolved_model_availability, AgentProvider},
    queue::RuntimeQueue,
    runtime_db::RuntimeDb,
    storage::AppStorage,
    system::{LocalSystem, WorkspaceAccessMode, WorkspaceProjectionKind},
    tool::{apply_patch::ApplyPatchSurface, ToolRegistry},
    types::{
        ActiveWorkspaceEntry, AgentState, AuditEvent, ResolvedModelAvailability,
        SkillActivationSource, SkillActivationState,
    },
};

use super::{
    scheduler_executor, workspace, InitialWorkspaceBinding, RuntimeAgent, RuntimeHandle,
    RuntimeInner,
};

#[derive(Debug, Clone)]
pub(super) struct ProviderReconfigurator {
    config: AppConfig,
}

impl RuntimeHandle {
    pub fn new(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<InitialWorkspaceBinding>,
        callback_base_url: String,
        provider: Arc<dyn AgentProvider>,
        default_agent_id: String,
        context_config: ContextConfig,
    ) -> Result<Self> {
        let base_context_config = context_config.clone();
        Self::new_internal(
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
            crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS,
            crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS,
            crate::web::WebConfig::default(),
            None,
            None,
            None,
        )
    }

    pub(crate) fn new_static_with_host_bridge(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<InitialWorkspaceBinding>,
        callback_base_url: String,
        provider: Arc<dyn AgentProvider>,
        default_agent_id: String,
        context_config: ContextConfig,
        runtime_db: RuntimeDb,
        host_bridge: RuntimeHostBridge,
        model_catalog: RuntimeModelCatalog,
    ) -> Result<Self> {
        let base_context_config = context_config.clone();
        Self::new_internal(
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
            crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS,
            crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS,
            crate::web::WebConfig::default(),
            None,
            Some(runtime_db),
            Some(host_bridge),
        )
    }

    pub(crate) fn new_reconfigurable_with_host_bridge(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<InitialWorkspaceBinding>,
        callback_base_url: String,
        config: AppConfig,
        default_agent_id: String,
        context_config: ContextConfig,
        runtime_db: RuntimeDb,
        host_bridge: RuntimeHostBridge,
    ) -> Result<Self> {
        let model_catalog = RuntimeModelCatalog::from_config(&config);
        let model_availability = resolved_model_availability(&config);
        let base_context_config = context_config.clone();
        let mut provider_config = config.clone();
        provider_config.runtime_max_output_tokens = model_catalog
            .resolved_model_policy(&base_context_config, None)
            .runtime_max_output_tokens;
        let provider =
            build_provider_from_model_chain(&provider_config, &model_catalog.provider_chain(None))?;
        let resolved_context_config =
            model_catalog.resolved_context_config(&base_context_config, None);
        Self::new_internal(
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
            config.default_tool_output_tokens as u64,
            config.max_tool_output_tokens as u64,
            config.web_config.clone(),
            Some(ProviderReconfigurator { config }),
            Some(runtime_db),
            Some(host_bridge),
        )
    }

    fn new_internal(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<InitialWorkspaceBinding>,
        callback_base_url: String,
        provider: Arc<dyn AgentProvider>,
        default_agent_id: String,
        base_context_config: ContextConfig,
        context_config: ContextConfig,
        model_catalog: RuntimeModelCatalog,
        model_availability: Vec<ResolvedModelAvailability>,
        default_tool_output_tokens: u64,
        max_tool_output_tokens: u64,
        web_config: crate::web::WebConfig,
        provider_reconfig: Option<ProviderReconfigurator>,
        runtime_db: Option<RuntimeDb>,
        host_bridge: Option<RuntimeHostBridge>,
    ) -> Result<Self> {
        let mut provider = provider;
        let storage = AppStorage::new(data_dir)?;
        let runtime_db = match runtime_db {
            Some(runtime_db) => runtime_db,
            None => RuntimeDb::open_and_migrate(
                storage.runtime_dir().join("state/runtime.sqlite"),
                storage.runtime_dir().join("state/runtime.lock"),
            )?,
        };
        let agent_id = agent_id.into();
        let initial_workspace = initial_workspace.into();
        let initial_workspace_entry = match &initial_workspace {
            InitialWorkspaceBinding::Entry(entry) => Some(entry.clone()),
            InitialWorkspaceBinding::Anchor(anchor) => Some(crate::types::WorkspaceEntry::new(
                crate::ids::workspace_id(),
                anchor.clone(),
                anchor
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToString::to_string),
            )),
            InitialWorkspaceBinding::Detached => Some(workspace::agent_home_workspace_entry(
                storage.data_dir(),
                &agent_id,
            )),
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
        let mut state = recovered_agent.unwrap_or_else(|| AgentState::new(agent_id.clone()));

        if state.attached_workspaces.is_empty() {
            if let Some(workspace) = initial_workspace_entry.as_ref() {
                let should_seed_initial_binding = !recovered_from_storage
                    || state
                        .active_workspace_entry
                        .as_ref()
                        .is_some_and(|entry| entry.workspace_id == workspace.workspace_id);
                if should_seed_initial_binding {
                    state
                        .attached_workspaces
                        .push(workspace.workspace_id.clone());
                }
            }
        }

        if state.active_workspace_entry.is_none() {
            if let Some(workspace) = initial_workspace_entry.as_ref() {
                state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                    workspace_id: workspace.workspace_id.clone(),
                    workspace_anchor: workspace.workspace_anchor.clone(),
                    execution_root_id: workspace::build_execution_root_id(
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

        if state
            .worktree_session
            .as_ref()
            .is_some_and(|worktree| !worktree.worktree_path.exists())
        {
            storage.append_event(&AuditEvent::new(
                "recovery_cleared_missing_worktree_session",
                serde_json::json!({
                    "agent_id": agent_id,
                    "worktree_path": state
                        .worktree_session
                        .as_ref()
                        .map(|w| w.worktree_path.display().to_string()),
                    "reason": "worktree_path_does_not_exist"
                }),
            ))?;
            state.worktree_session = None;
            if state.active_workspace_entry.as_ref().is_some_and(|entry| {
                entry.projection_kind == WorkspaceProjectionKind::GitWorktreeRoot
            }) {
                let data_dir = storage.data_dir();
                let workspace_entry = workspace::agent_home_workspace_entry(&data_dir, &agent_id);
                let kind = WorkspaceProjectionKind::CanonicalRoot;
                state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                    workspace_id: workspace_entry.workspace_id.clone(),
                    workspace_anchor: workspace_entry.workspace_anchor.clone(),
                    execution_root_id: workspace::build_execution_root_id(
                        &workspace_entry.workspace_id,
                        kind,
                        &workspace_entry.workspace_anchor,
                    )?,
                    execution_root: workspace_entry.workspace_anchor.clone(),
                    projection_kind: kind,
                    access_mode: WorkspaceAccessMode::ExclusiveWrite,
                    cwd: workspace_entry.workspace_anchor.clone(),
                    occupancy_id: None,
                    projection_metadata: None,
                });
            }
        }

        state
            .active_skills
            .retain(|skill| matches!(skill.activation_state, SkillActivationState::SessionActive));
        for skill in &mut state.active_skills {
            skill.activation_source = SkillActivationSource::Restored;
        }
        state.pending = queue.len();
        state.total_message_count = storage.count_messages().unwrap_or_default();
        scheduler_executor::apply_bootstrap_recovered_projection(
            &mut state,
            scheduler_executor::BootstrapRecoveryFacts {
                queued_messages: queue.len(),
            },
        );

        if let Some(reconfig) = provider_reconfig.as_ref() {
            let chain = model_catalog.provider_chain_for_turn(
                state.model_override.as_ref(),
                state.pending_fallback_model.as_ref(),
            );
            let mut provider_config = reconfig.config.clone();
            provider_config.runtime_max_output_tokens = model_catalog
                .resolved_model_policy(&base_context_config, state.model_override.as_ref())
                .runtime_max_output_tokens;
            provider = build_provider_from_model_chain(&provider_config, &chain)?;
        }
        storage.write_agent(&state)?;
        let mut legacy_work_items = storage.read_recent_work_items(usize::MAX)?;
        for record in &mut legacy_work_items {
            crate::work_item_plan::refresh_plan_artifact_metadata(storage.data_dir(), record)?;
        }
        runtime_db
            .work_items()
            .import_legacy(legacy_work_items, state.current_work_item_id.as_deref())?;

        let resolved_context_config = if provider_reconfig.is_some() {
            model_catalog
                .resolved_context_config(&base_context_config, state.model_override.as_ref())
        } else {
            context_config.clone()
        };

        Ok(Self {
            inner: Arc::new(RuntimeInner {
                agent: Mutex::new(RuntimeAgent {
                    state,
                    queue,
                    current_run_abort: None,
                }),
                notify: Notify::new(),
                storage,
                runtime_db,
                provider: RwLock::new(provider),
                provider_reconfig,
                model_catalog,
                model_availability,
                base_context_config,
                context_config: RwLock::new(resolved_context_config),
                default_tool_output_tokens,
                max_tool_output_tokens,
                web_config,
                builtin_web_search_probe_cache: Mutex::new(HashMap::new()),
                callback_base_url,
                tools: ToolRegistry::new(PathBuf::new()),
                system: Arc::new(LocalSystem::new()),
                default_agent_id,
                host_bridge,
                task_handles: Mutex::new(HashMap::new()),
                recovered_tasks: Mutex::new(Some(snapshot.active_tasks)),
                recovered_timers: Mutex::new(Some(snapshot.active_timers)),
                suppress_next_continue_active_tick: Mutex::new(false),
                shutdown_requested: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) async fn current_provider(&self) -> Arc<dyn AgentProvider> {
        self.inner.provider.read().await.clone()
    }

    pub(crate) fn model_state_for(&self, state: &AgentState) -> crate::types::AgentModelState {
        super::agent_model_state_for_catalog(
            &self.inner.model_catalog,
            &self.inner.base_context_config,
            state,
        )
    }

    pub(crate) async fn current_apply_patch_surface(&self) -> ApplyPatchSurface {
        let state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        self.apply_patch_surface_for_state(&state)
    }

    pub(crate) fn apply_patch_surface_for_state(&self, state: &AgentState) -> ApplyPatchSurface {
        let model_ref = self
            .selected_model_ref_for_state(state)
            .unwrap_or_else(|| self.model_state_for(state).effective_model);
        ApplyPatchSurface::for_model_ref(&model_ref.as_string())
    }

    fn selected_model_ref_for_state(&self, state: &AgentState) -> Option<ModelRef> {
        self.inner
            .model_catalog
            .provider_chain_for_turn(
                state.model_override.as_ref(),
                state.pending_fallback_model.as_ref(),
            )
            .into_iter()
            .next()
    }

    pub(crate) async fn reconfigure_provider_for_state(&self, state: &AgentState) -> Result<()> {
        let Some(reconfig) = self.inner.provider_reconfig.as_ref() else {
            return Err(anyhow!(
                "agent model override is unavailable for runtimes without host-managed provider configuration"
            ));
        };
        let chain = self.inner.model_catalog.provider_chain_for_turn(
            state.model_override.as_ref(),
            state.pending_fallback_model.as_ref(),
        );
        let resolved_context_config = self.inner.model_catalog.resolved_context_config(
            &self.inner.base_context_config,
            state.model_override.as_ref(),
        );
        let mut provider_config = reconfig.config.clone();
        if let (Some(primary), Some(reasoning_effort)) = (
            chain.first(),
            state.model_override_reasoning_effort.as_ref(),
        ) {
            if let Some(provider) = provider_config.providers.get_mut(&primary.provider) {
                provider.reasoning_effort = Some(reasoning_effort.clone());
            }
        }
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

    pub(crate) async fn reconfigure_provider_for_current_state(&self) -> Result<()> {
        if self.inner.provider_reconfig.is_none() {
            return Ok(());
        }
        let state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        self.reconfigure_provider_for_state(&state).await
    }

    pub(crate) async fn current_context_config(&self) -> ContextConfig {
        self.inner.context_config.read().await.clone()
    }

    async fn model_config_with_fresh_discovery_cache(&self) -> Option<AppConfig> {
        let Some(reconfig) = self.inner.provider_reconfig.as_ref() else {
            return None;
        };
        let mut config = reconfig.config.clone();
        let cache_path = discovery_cache_path(&config.home_dir);
        match tokio::task::spawn_blocking(move || load_discovery_cache_at(&cache_path)).await {
            Ok(Ok(cache)) => {
                config.model_discovery_cache = cache;
                Some(config)
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    "failed to refresh model discovery cache; using startup model catalog"
                );
                None
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "model discovery cache refresh task failed; using startup model catalog"
                );
                None
            }
        }
    }

    pub(crate) async fn available_models(&self) -> Result<Vec<BuiltInModelMetadata>> {
        if let Some(config) = self.model_config_with_fresh_discovery_cache().await {
            return Ok(RuntimeModelCatalog::from_config(&config).available_models());
        }
        Ok(self.inner.model_catalog.available_models())
    }

    pub(crate) async fn model_availability(&self) -> Result<Vec<ResolvedModelAvailability>> {
        if let Some(config) = self.model_config_with_fresh_discovery_cache().await {
            return Ok(resolved_model_availability(&config));
        }
        Ok(self.inner.model_availability.clone())
    }

    pub(crate) async fn model_providers(&self) -> Result<Vec<crate::types::ModelProviderEntry>> {
        let models = self.model_availability().await?;
        let config = self.provider_config_for_projection().await;
        Ok(
            crate::provider::resolved_model_providers_from_availability_for_runtime(
                config.as_ref(),
                &models,
            ),
        )
    }

    pub(crate) async fn provider_models(
        &self,
        provider: &str,
    ) -> Result<Vec<crate::types::ProviderModelEntry>> {
        let models = self.model_availability().await?;
        Ok(crate::provider::provider_models_from_availability_for_runtime(&models, provider))
    }

    async fn provider_config_for_projection(&self) -> Option<AppConfig> {
        self.inner
            .provider_reconfig
            .as_ref()
            .map(|reconfig| reconfig.config.clone())
    }
}
