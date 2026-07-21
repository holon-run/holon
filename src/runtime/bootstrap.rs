#[cfg(test)]
use std::sync::Mutex as StdMutex;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

use anyhow::{anyhow, Result};
use arc_swap::ArcSwap;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{
    config::{AppConfig, ModelRef, ModelRouteCapability, ModelRouteRef, RuntimeModelCatalog},
    context::ContextConfig,
    host::RuntimeHostBridge,
    model_catalog::BuiltInModelMetadata,
    model_discovery::{
        discovery_cache_needs_refresh, discovery_cache_path, discovery_cache_status_for_provider,
        load_discovery_cache_at, refresh_provider_models, ModelDiscoveryCacheFile,
        ModelDiscoveryCacheStatus, DEFAULT_DISCOVERY_CACHE_TTL,
    },
    provider::{
        build_candidate_from_model_route, build_provider_from_model_chain,
        resolved_model_availability, AgentProvider, ConversationMessage, ModelBlock,
        ProviderGenerateImageRequest, ProviderGenerateImageResponse,
        ProviderJsonSchemaResponseFormat, ProviderResponseFormatRequest, ProviderTurnRequest,
    },
    queue::RuntimeQueue,
    runtime_db::RuntimeDb,
    storage::{AppStorage, EventBus},
    system::{LocalSystem, WorkspaceAccessMode, WorkspaceProjectionKind},
    tool::{apply_patch::ApplyPatchSurface, ToolRegistry},
    types::{
        ActiveWorkspaceEntry, AgentState, AuditEvent, ResolvedModelAvailability,
        SkillActivationSource, SkillActivationState,
    },
};

use super::{
    clock::{Clock, SystemClock},
    scheduler_executor, workspace, AgentRuntimeProjectionCache, InitialWorkspaceBinding,
    RuntimeAgent, RuntimeHandle, RuntimeInner,
};

/// Snapshot of config-derived runtime fields that can be hot-swapped at runtime.
/// Stored behind an `ArcSwap` so that config reloads take effect on the next turn
/// without disturbing an in-progress turn.
pub(super) struct ConfigSnapshot {
    pub model_catalog: RuntimeModelCatalog,
    pub model_availability: Vec<ResolvedModelAvailability>,
    pub base_context_config: ContextConfig,
    pub provider_reconfig: Option<ProviderReconfigurator>,
    pub default_tool_output_tokens: u64,
    pub max_tool_output_tokens: u64,
    pub web_config: crate::web::WebConfig,
    pub x_search_config: Option<crate::config::XSearchRuntimeConfig>,
}

impl ConfigSnapshot {
    /// Build a fresh snapshot from a config, preserving the agent's current model override.
    pub fn from_config(config: &AppConfig) -> Result<Self> {
        let model_catalog = RuntimeModelCatalog::from_config(config);
        let model_availability = resolved_model_availability(config);
        let provider_reconfig = Some(ProviderReconfigurator {
            config: config.clone(),
        });
        let base_context_config = ContextConfig {
            recent_messages: config.context_window_messages,
            recent_briefs: config.context_window_briefs,
            compaction_trigger_messages: config.compaction_trigger_messages,
            compaction_keep_recent_messages: config.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: config.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: config.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: config.compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: config.recent_episode_candidates,
            max_relevant_episodes: config.max_relevant_episodes,
            ..ContextConfig::default()
        };
        Ok(Self {
            model_catalog,
            model_availability,
            base_context_config,
            provider_reconfig,
            default_tool_output_tokens: config.default_tool_output_tokens as u64,
            max_tool_output_tokens: config.max_tool_output_tokens as u64,
            web_config: config.web_config.clone(),
            x_search_config: crate::config::XSearchRuntimeConfig::from_app_config(config)?,
        })
    }
}

#[derive(Debug, Clone)]
pub(super) struct ProviderReconfigurator {
    pub(super) config: AppConfig,
}

impl RuntimeHandle {
    pub(crate) fn prepare_runtime_storage(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<InitialWorkspaceBinding>,
        runtime_db: RuntimeDb,
    ) -> Result<()> {
        let _ = prepare_runtime_storage(agent_id, data_dir, initial_workspace, Some(runtime_db))?;
        Ok(())
    }

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
            None,
            Arc::new(SystemClock),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_clock(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<InitialWorkspaceBinding>,
        callback_base_url: String,
        provider: Arc<dyn AgentProvider>,
        default_agent_id: String,
        context_config: ContextConfig,
        clock: Arc<dyn Clock>,
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
            None,
            clock,
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
        event_bus: EventBus,
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
            Some(event_bus),
            Arc::new(SystemClock),
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
        event_bus: EventBus,
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
            Some(event_bus),
            Arc::new(SystemClock),
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
        event_bus: Option<EventBus>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let x_search_config = provider_reconfig
            .as_ref()
            .map(|reconfig| crate::config::XSearchRuntimeConfig::from_app_config(&reconfig.config))
            .transpose()?
            .flatten();
        let config_snapshot = Arc::new(ConfigSnapshot {
            model_catalog: model_catalog.clone(),
            model_availability: model_availability.clone(),
            base_context_config: base_context_config.clone(),
            provider_reconfig: provider_reconfig.clone(),
            default_tool_output_tokens,
            max_tool_output_tokens,
            web_config: web_config.clone(),
            x_search_config,
        });
        let mut provider = provider;
        let PreparedRuntimeStorage {
            storage,
            runtime_db,
            state,
            queue,
            active_tasks,
            active_timers,
            projection_cache,
        } = prepare_runtime_storage(agent_id, data_dir, initial_workspace, runtime_db)?;
        if let Some(event_bus) = event_bus {
            storage.enable_event_bus(event_bus)?;
        }

        if let Some(reconfig) = config_snapshot.provider_reconfig.as_ref() {
            let chain = config_snapshot.model_catalog.provider_chain_for_turn(
                state.model_override.as_ref(),
                state.pending_fallback_model.as_ref(),
            );
            let mut provider_config = reconfig.config.clone();
            provider_config.runtime_max_output_tokens = config_snapshot
                .model_catalog
                .resolved_model_policy(
                    &config_snapshot.base_context_config,
                    state.model_override.as_ref(),
                )
                .runtime_max_output_tokens;
            provider = build_provider_from_model_chain(&provider_config, &chain)?;
        }
        let resolved_context_config = if config_snapshot.provider_reconfig.is_some() {
            config_snapshot.model_catalog.resolved_context_config(
                &config_snapshot.base_context_config,
                state.model_override.as_ref(),
            )
        } else {
            context_config.clone()
        };

        let runtime = Self {
            inner: Arc::new(RuntimeInner {
                agent: Mutex::new(RuntimeAgent {
                    last_persisted_state: state.clone(),
                    state,
                    queue,
                    current_run_abort: None,
                }),
                projection_cache: Mutex::new(projection_cache),
                object_query_cache: Arc::new(crate::object_query_cache::ObjectQueryCache::new(256)),
                notify: Notify::new(),
                storage,
                runtime_db,
                clock,
                provider: RwLock::new(provider),
                config_snapshot: ArcSwap::from(config_snapshot),
                context_config: RwLock::new(resolved_context_config),
                builtin_web_search_probe_cache: Mutex::new(HashMap::new()),
                view_image_observation_cache: Mutex::new(HashMap::new()),
                model_discovery_refreshes: Mutex::new(HashSet::new()),
                callback_base_url,
                tools: ToolRegistry::new(PathBuf::new()),
                system: Arc::new(LocalSystem::new()),
                default_agent_id,
                host_bridge,
                task_handles: Mutex::new(HashMap::new()),
                recovered_tasks: Mutex::new(Some(active_tasks)),
                recovered_timers: Mutex::new(Some(active_timers)),
                suppress_next_continue_active_tick: Mutex::new(false),
                shutdown_requested: AtomicBool::new(false),
                #[cfg(test)]
                transition_faults: StdMutex::new(std::collections::VecDeque::new()),
                #[cfg(test)]
                omit_next_scheduler_claim_shadow_comparison: AtomicBool::new(false),
                #[cfg(test)]
                transition_warnings: StdMutex::new(Vec::new()),
            }),
        };
        Ok(runtime)
    }

    pub(crate) async fn current_provider(&self) -> Arc<dyn AgentProvider> {
        self.inner.provider.read().await.clone()
    }

    pub(crate) fn model_state_for(&self, state: &AgentState) -> crate::types::AgentModelState {
        let snap = self.inner.config_snapshot.load();
        super::agent_model_state_for_catalog(&snap.model_catalog, &snap.base_context_config, state)
    }

    pub(crate) async fn current_apply_patch_surface(&self) -> ApplyPatchSurface {
        let state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        self.apply_patch_surface_for_state(&state)
    }

    /// Attach a shared memory-index notify so the daemon-level indexer is
    /// woken when this runtime writes new evidence for indexing.
    pub(crate) fn enable_memory_index_notify(&self, notify: Arc<tokio::sync::Notify>) {
        let _ = self.inner.storage.enable_memory_index_notify(notify);
    }

    pub(crate) fn apply_patch_surface_for_state(&self, state: &AgentState) -> ApplyPatchSurface {
        let route_ref = self
            .selected_model_ref_for_state(state)
            .unwrap_or_else(|| self.model_state_for(state).effective_model);
        ApplyPatchSurface::for_model_ref(&route_ref.model_ref().as_string())
    }

    fn selected_model_ref_for_state(&self, state: &AgentState) -> Option<ModelRouteRef> {
        let snap = self.inner.config_snapshot.load();
        snap.model_catalog
            .provider_chain_for_turn(
                state.model_override.as_ref(),
                state.pending_fallback_model.as_ref(),
            )
            .into_iter()
            .next()
    }

    pub(crate) async fn reconfigure_provider_for_state(&self, state: &AgentState) -> Result<()> {
        let snap = self.inner.config_snapshot.load();
        let Some(reconfig) = snap.provider_reconfig.as_ref() else {
            return Err(anyhow!(
                "agent model override is unavailable for runtimes without host-managed provider configuration"
            ));
        };
        let chain = snap.model_catalog.provider_chain_for_turn(
            state.model_override.as_ref(),
            state.pending_fallback_model.as_ref(),
        );
        let resolved_context_config = snap
            .model_catalog
            .resolved_context_config(&snap.base_context_config, state.model_override.as_ref());
        let mut provider_config = reconfig.config.clone();
        if let (Some(primary), Some(reasoning_effort)) = (
            chain.first(),
            state.model_override_reasoning_effort.as_ref(),
        ) {
            if let Some(provider) = provider_config.providers.get_mut(&primary.provider) {
                provider.reasoning_effort = Some(reasoning_effort.clone());
            }
        }
        provider_config.runtime_max_output_tokens = snap
            .model_catalog
            .resolved_model_policy(&snap.base_context_config, state.model_override.as_ref())
            .runtime_max_output_tokens;
        let provider = build_provider_from_model_chain(&provider_config, &chain)?;
        *self.inner.provider.write().await = provider;
        *self.inner.context_config.write().await = resolved_context_config;
        Ok(())
    }

    pub(crate) async fn reconfigure_provider_for_current_state(&self) -> Result<()> {
        let snap = self.inner.config_snapshot.load();
        if snap.provider_reconfig.is_none() {
            return Ok(());
        }
        let state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        self.reconfigure_provider_for_state(&state).await
    }

    /// Hot-reload config-derived runtime fields from a new `AppConfig`.
    ///
    /// Builds a fresh `ConfigSnapshot`, swaps it atomically, then rebuilds
    /// the provider for the agent's current model-override state. The swap
    /// is atomic via `ArcSwap`, so an in-progress turn that already loaded
    /// the old snapshot continues unaffected; the next turn picks up the
    /// new snapshot automatically.
    pub(crate) async fn reload_config(&self, config: &AppConfig) -> Result<()> {
        let new_snapshot = Arc::new(ConfigSnapshot::from_config(config)?);
        // Atomically swap the snapshot.
        self.inner.config_snapshot.store(new_snapshot);
        // Rebuild provider + context_config for current state.
        // If this runtime has no provider_reconfig (static provider), skip.
        let snap = self.inner.config_snapshot.load();
        if snap.provider_reconfig.is_some() {
            let state = {
                let guard = self.inner.agent.lock().await;
                guard.state.clone()
            };
            self.reconfigure_provider_for_state(&state).await?;
        }
        tracing::info!("hot-reloaded runtime config (provider/catalog/availability)");
        Ok(())
    }

    pub(crate) async fn current_context_config(&self) -> ContextConfig {
        self.inner.context_config.read().await.clone()
    }

    pub(crate) async fn current_view_image_vision_selection(
        &self,
    ) -> Result<crate::types::ViewImageVisionSelection> {
        let state = self.agent_state().await?;
        let snap = self.inner.config_snapshot.load();
        Ok(snap.model_catalog.select_view_image_vision_model(
            &snap.base_context_config,
            state.model_override.as_ref(),
            state.pending_fallback_model.as_ref(),
        ))
    }

    pub(crate) async fn cached_view_image_observation(
        &self,
        key: &crate::runtime::ViewImageObservationCacheKey,
    ) -> Option<crate::types::ViewImageObservation> {
        self.inner
            .view_image_observation_cache
            .lock()
            .await
            .get(key)
            .cloned()
    }

    pub(crate) async fn cache_view_image_observation(
        &self,
        key: crate::runtime::ViewImageObservationCacheKey,
        observation: crate::types::ViewImageObservation,
    ) {
        self.inner
            .view_image_observation_cache
            .lock()
            .await
            .insert(key, observation);
    }

    pub(crate) async fn generate_view_image_observation(
        &self,
        prompt: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<String> {
        let selection = self.current_view_image_vision_selection().await?;
        if selection.selected_mode == crate::types::ViewImageSelectedMode::Unavailable {
            return Err(anyhow!("no configured model supports image_input"));
        }
        let provider_name = selection
            .vision_provider
            .as_deref()
            .ok_or_else(|| anyhow!("vision selection did not include a provider"))?;
        let model_name = selection
            .vision_model
            .as_deref()
            .ok_or_else(|| anyhow!("vision selection did not include a model"))?;
        let snap = self.inner.config_snapshot.load();
        let vision_model_ref = ModelRef::parse(&format!("{provider_name}/{model_name}"))?;
        let vision_route = snap
            .model_catalog
            .resolve_model_route(
                &snap.base_context_config,
                &vision_model_ref,
                ModelRouteCapability::VisionObservation,
            )
            .ok_or_else(|| {
                anyhow!(
                    "vision model {provider_name}/{model_name} is not supported by ViewImage observation generation yet"
                )
            })?;
        let reconfig = snap.provider_reconfig.as_ref().ok_or_else(|| {
            anyhow!("ViewImage observation generation requires host-managed provider configuration")
        })?;
        let provider =
            build_candidate_from_model_route(&reconfig.config.home_dir, &vision_route)?.provider;
        let mut request = ProviderTurnRequest::plain(
            "You are a vision adapter for a headless agent. Inspect only the provided image and task prompt. Return exactly one JSON object and no markdown, prose, or implementation advice. The JSON object must match this shape: {\"type\":\"visual_observation\",\"schema\":\"visual_observation.v1\",\"summary\":\"string\",\"ocr\":[],\"elements\":[],\"relations\":[],\"issues\":[],\"uncertainties\":[],\"external_sources\":[]}. Required fields: type=\"visual_observation\", schema=\"visual_observation.v1\", summary, uncertainties. The uncertainties field must be an array of strings; use [] when there are no caveats. The ocr, elements, relations, issues, and external_sources fields must be arrays of objects; omit them or use [] when empty. Include visible text in ocr or summary; include bounding boxes when location matters; describe only visible evidence; say when uncertain.",
            vec![ConversationMessage::UserImage {
                prompt: prompt.to_string(),
                media_type: media_type.to_string(),
                data_base64: BASE64_STANDARD.encode(bytes),
            }],
            Vec::new(),
        );
        request.response_format = Some(visual_observation_response_format());
        let response = provider.complete_turn(request).await?;
        let text = response
            .blocks
            .iter()
            .filter_map(|block| match block {
                ModelBlock::Text { text } => Some(text.trim()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if text.is_empty() {
            return Err(anyhow!(
                "ViewImage observation provider returned no text content"
            ));
        }
        Ok(text)
    }

    pub(crate) async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        let state = self.agent_state().await?;
        let snap = self.inner.config_snapshot.load();
        let route = snap
            .model_catalog
            .select_generate_image_route(
                &snap.base_context_config,
                state.model_override.as_ref(),
                state.pending_fallback_model.as_ref(),
            )
            .ok_or_else(|| anyhow!("no configured model supports image_generation"))?;
        let reconfig = snap.provider_reconfig.as_ref().ok_or_else(|| {
            anyhow!("image generation requires host-managed provider configuration")
        })?;
        let provider =
            build_candidate_from_model_route(&reconfig.config.home_dir, &route)?.provider;
        provider.generate_image(request).await
    }

    async fn model_config_with_fresh_discovery_cache(&self) -> Option<AppConfig> {
        let snap = self.inner.config_snapshot.load();
        let Some(reconfig) = snap.provider_reconfig.as_ref() else {
            return None;
        };
        let mut config = reconfig.config.clone();
        let cache_path = discovery_cache_path(&config.home_dir);
        match tokio::task::spawn_blocking(move || load_discovery_cache_at(&cache_path)).await {
            Ok(Ok(cache)) => {
                self.spawn_model_discovery_refreshes(&config, &cache).await;
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

    async fn spawn_model_discovery_refreshes(
        &self,
        config: &AppConfig,
        cache: &ModelDiscoveryCacheFile,
    ) {
        let providers = config
            .providers
            .values()
            .filter(|provider| {
                discovery_cache_needs_refresh(provider, cache, DEFAULT_DISCOVERY_CACHE_TTL)
            })
            .cloned()
            .collect::<Vec<_>>();
        for provider in providers {
            let provider_id = provider.id.clone();
            {
                let mut in_flight = self.inner.model_discovery_refreshes.lock().await;
                if !in_flight.insert(provider_id.clone()) {
                    continue;
                }
            }

            let runtime = self.clone();
            let cache_path = discovery_cache_path(&config.home_dir);
            tokio::spawn(async move {
                let result = refresh_provider_models(&provider, &cache_path).await;
                match result {
                    Ok(report) => {
                        tracing::info!(
                            provider = %report.provider.as_str(),
                            model_count = report.model_count,
                            "refreshed model discovery cache"
                        );
                        if let Err(err) = runtime.reload_model_discovery_cache_snapshot().await {
                            tracing::warn!(
                                error = %err,
                                "failed to reload model discovery cache after refresh"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            provider = %provider_id.as_str(),
                            error = %err,
                            "model discovery cache refresh failed"
                        );
                    }
                }
                runtime
                    .inner
                    .model_discovery_refreshes
                    .lock()
                    .await
                    .remove(&provider_id);
            });
        }
    }

    async fn reload_model_discovery_cache_snapshot(&self) -> Result<()> {
        let snap = self.inner.config_snapshot.load();
        let Some(reconfig) = snap.provider_reconfig.as_ref() else {
            return Ok(());
        };
        let mut config = reconfig.config.clone();
        let cache_path = discovery_cache_path(&config.home_dir);
        let cache =
            tokio::task::spawn_blocking(move || load_discovery_cache_at(&cache_path)).await??;
        config.model_discovery_cache = cache;
        self.inner
            .config_snapshot
            .store(Arc::new(ConfigSnapshot::from_config(&config)?));
        Ok(())
    }

    pub(crate) async fn model_discovery_status(&self) -> Result<Vec<ModelDiscoveryCacheStatus>> {
        let snap = self.inner.config_snapshot.load();
        let Some(reconfig) = snap.provider_reconfig.as_ref() else {
            return Ok(Vec::new());
        };
        let config = reconfig.config.clone();
        let cache_path = discovery_cache_path(&config.home_dir);
        let cache =
            tokio::task::spawn_blocking(move || load_discovery_cache_at(&cache_path)).await??;
        self.spawn_model_discovery_refreshes(&config, &cache).await;
        let in_flight = self.inner.model_discovery_refreshes.lock().await.clone();
        Ok(config
            .providers
            .values()
            .map(|provider| {
                discovery_cache_status_for_provider(
                    provider,
                    &cache,
                    DEFAULT_DISCOVERY_CACHE_TTL,
                    in_flight.contains(&provider.id),
                )
            })
            .collect())
    }

    pub(crate) async fn available_models(&self) -> Result<Vec<BuiltInModelMetadata>> {
        if let Some(config) = self.model_config_with_fresh_discovery_cache().await {
            return Ok(RuntimeModelCatalog::from_config(&config).available_models());
        }
        let snap = self.inner.config_snapshot.load();
        Ok(snap.model_catalog.available_models())
    }

    pub(crate) async fn model_availability(&self) -> Result<Vec<ResolvedModelAvailability>> {
        if let Some(config) = self.model_config_with_fresh_discovery_cache().await {
            return Ok(resolved_model_availability(&config));
        }
        let snap = self.inner.config_snapshot.load();
        Ok(snap.model_availability.clone())
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
        let snap = self.inner.config_snapshot.load();
        snap.provider_reconfig
            .as_ref()
            .map(|reconfig| reconfig.config.clone())
    }
}

struct PreparedRuntimeStorage {
    storage: AppStorage,
    runtime_db: RuntimeDb,
    state: AgentState,
    queue: RuntimeQueue,
    active_tasks: Vec<crate::types::TaskRecord>,
    active_timers: Vec<crate::types::TimerRecord>,
    projection_cache: AgentRuntimeProjectionCache,
}

fn prepare_runtime_storage(
    agent_id: impl Into<String>,
    data_dir: PathBuf,
    initial_workspace: impl Into<InitialWorkspaceBinding>,
    runtime_db: Option<RuntimeDb>,
) -> Result<PreparedRuntimeStorage> {
    let agent_id = agent_id.into();
    let runtime_dir = data_dir.join(".holon");
    let runtime_db = match runtime_db {
        Some(runtime_db) => runtime_db,
        None => RuntimeDb::open_and_migrate(
            runtime_dir.join("state/runtime.sqlite"),
            runtime_dir.join("state/runtime.lock"),
        )?,
    };
    let storage = AppStorage::new_for_agent(data_dir, agent_id.clone(), runtime_db.clone())?;
    let initial_workspace = initial_workspace.into();
    let initial_workspace_entry = match &initial_workspace {
        InitialWorkspaceBinding::Entry(entry) => Some(entry.clone()),
        InitialWorkspaceBinding::Anchor(anchor) => Some(crate::types::WorkspaceEntry::new(
            {
                let normalized = crate::system::workspace::normalize_path(anchor)
                    .unwrap_or_else(|_| anchor.clone());
                crate::ids::deterministic_workspace_id(&normalized)
            },
            crate::system::workspace::normalize_path(anchor).unwrap_or_else(|_| anchor.clone()),
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

    let workspace_entries_complete = storage.legacy_importer().workspace_entries_complete()?;
    if let Some(workspace) = initial_workspace_entry.as_ref() {
        let known = storage.latest_workspace_entries()?;
        if !known
            .iter()
            .any(|entry| entry.workspace_id == workspace.workspace_id)
        {
            if workspace_entries_complete {
                runtime_db.workspace_entries().upsert(workspace)?;
            } else {
                storage.append_workspace_entry(workspace)?;
            }
        }
    }

    storage.legacy_importer().import_runtime_domains()?;

    let snapshot = storage.recovery_snapshot(&agent_id)?;
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

    if workspace::canonicalize_agent_home_bindings(&mut state, storage.data_dir(), &agent_id)? {
        let workspace = workspace::agent_home_workspace_entry(storage.data_dir(), &agent_id);
        let known = storage.latest_workspace_entries()?;
        if !known
            .iter()
            .any(|entry| entry.workspace_id == workspace.workspace_id)
        {
            storage.append_workspace_entry(&workspace)?;
        }
        storage.append_event(&AuditEvent::legacy(
            "agent_home_workspace_bindings_migrated",
            serde_json::json!({
                "agent_id": agent_id,
                "workspace_id": workspace.workspace_id,
                "legacy_workspace_id": crate::types::AGENT_HOME_WORKSPACE_ID,
            }),
        ))?;
    }

    if state
        .worktree_session
        .as_ref()
        .is_some_and(|worktree| !worktree.worktree_path.exists())
    {
        storage.append_event(&AuditEvent::legacy(
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
        if state
            .active_workspace_entry
            .as_ref()
            .is_some_and(|entry| entry.projection_kind == WorkspaceProjectionKind::GitWorktreeRoot)
        {
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
    storage.write_agent(&state)?;
    storage
        .legacy_importer()
        .import_derived_domains(&state.id)?;
    storage.enable_audit_event_index(runtime_db.clone(), Some(state.id.clone()))?;
    let projection_cache = AgentRuntimeProjectionCache::rebuild(
        state.id.clone(),
        runtime_db.tasks().latest_for_agent(&state.id, usize::MAX)?,
        runtime_db
            .work_items()
            .latest_for_agent(&state.id, usize::MAX)?,
        runtime_db
            .timers()
            .recent_for_agent(&state.id, usize::MAX)?,
        runtime_db.external_triggers().latest_for_agent(&state.id)?,
    );

    Ok(PreparedRuntimeStorage {
        storage,
        runtime_db,
        state,
        queue,
        active_tasks: snapshot.active_tasks,
        active_timers: snapshot.active_timers,
        projection_cache,
    })
}

fn visual_observation_response_format() -> ProviderResponseFormatRequest {
    ProviderResponseFormatRequest::JsonSchema(ProviderJsonSchemaResponseFormat {
        name: "visual_observation_v1".into(),
        strict: true,
        schema: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "type",
                "schema",
                "summary",
                "ocr",
                "elements",
                "relations",
                "issues",
                "uncertainties",
                "external_sources"
            ],
            "properties": {
                "type": { "type": "string", "const": "visual_observation" },
                "schema": { "type": "string", "const": "visual_observation.v1" },
                "summary": { "type": "string" },
                "ocr": { "type": "array", "items": { "type": "object" } },
                "elements": { "type": "array", "items": { "type": "object" } },
                "relations": { "type": "array", "items": { "type": "object" } },
                "issues": { "type": "array", "items": { "type": "object" } },
                "uncertainties": { "type": "array", "items": { "type": "string" } },
                "external_sources": { "type": "array", "items": { "type": "object" } }
            }
        }),
    })
}
