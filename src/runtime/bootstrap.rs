#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use arc_swap::ArcSwap;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{
    config::{
        AppConfig, ModelRouteCapability, ModelRouteRef, ResolvedViewImageVisionSelection,
        RuntimeModelCatalog,
    },
    context::ContextConfig,
    host::RuntimeHostBridge,
    model_discovery::{
        discovery_cache_needs_refresh, discovery_cache_path, discovery_cache_status_for_provider,
        load_discovery_cache_at, provider_discovers_model_capabilities, refresh_provider_models,
        ModelDiscoveryCacheFile, ModelDiscoveryCacheStatus, DEFAULT_DISCOVERY_CACHE_TTL,
    },
    provider::{
        build_candidate_from_model_route, build_provider_from_model_chain,
        build_provider_from_model_chain_with_override, resolved_model_availability, AgentProvider,
        ConversationMessage, ModelBlock, ModelRouteParameterOverride, ProviderGenerateImageRequest,
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

pub(crate) const VIEW_IMAGE_PROTOCOL_FAILURE_THRESHOLD: u32 = 2;
const VIEW_IMAGE_CANDIDATE_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const VIEW_IMAGE_ALL_CANDIDATES_COOLING_REASON: &str =
    "all_configured_view_image_candidates_cooling_after_protocol_validation_failures";
const VIEW_IMAGE_CANDIDATE_COOLDOWN_FALLBACK_REASON: &str =
    "view_image_candidate_cooldown_fallback_after_protocol_validation_failures";
const VIEW_IMAGE_SUPPORTED_CANDIDATE_REASON: &str = "model_advertises_image_input";

#[derive(Debug, Default)]
pub(super) struct ViewImageCandidateHealth {
    candidate_refs: Vec<ModelRouteRef>,
    failures: HashMap<ModelRouteRef, ViewImageCandidateFailure>,
    explicit_failures: HashMap<ModelRouteRef, ViewImageCandidateFailure>,
}

#[derive(Clone)]
pub(crate) struct PreparedViewImageObservation {
    pub(crate) route_selection: ResolvedViewImageVisionSelection,
    pub(crate) endpoint_config_identity: String,
    provider: Arc<dyn AgentProvider>,
}

fn view_image_endpoint_config_identity(
    route: &crate::config::ResolvedModelRoute,
    home: &std::path::Path,
) -> String {
    // Hash the complete effective configuration, including credential identity.
    // Never expose the unhashed configuration in diagnostics or tool results.
    let mut hash = Sha256::new();
    hash.update(format!("{route:?}"));
    hash.update(home.as_os_str().as_encoded_bytes());
    format!("{:x}", hash.finalize())
}

fn view_image_route_error(
    kind: &str,
    reason: &str,
    selection: &crate::types::ViewImageVisionSelection,
) -> anyhow::Error {
    crate::tool::ToolError::new(kind, "ViewImage could not use the selected model route")
        .with_details(serde_json::json!({
            "reason": reason,
            "selection": selection,
            "selected_mode": selection.selected_mode,
            "selection_reason": selection.selection_reason,
            "primary_provider": selection.primary_provider,
            "primary_model": selection.primary_model,
            "candidates": selection.candidates,
        }))
        .with_recovery_hint(
            "select an available image-input route with a supported observation transport and valid credentials",
        )
        .into()
}

#[derive(Debug, Clone, Default)]
struct ViewImageCandidateFailure {
    consecutive_protocol_failures: u32,
    cooling_until: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ViewImageCandidateFailureStatus {
    pub(crate) consecutive_protocol_failures: u32,
    pub(crate) circuit_open: bool,
    pub(crate) cooldown_seconds: u64,
}

impl ViewImageCandidateHealth {
    fn sync_candidate_pool(&mut self, selection: &ResolvedViewImageVisionSelection) {
        let candidate_refs = selection
            .selection
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.image_input && candidate.reason == VIEW_IMAGE_SUPPORTED_CANDIDATE_REASON
            })
            .filter_map(|candidate| ModelRouteRef::parse_compatible(&candidate.model_ref).ok())
            .collect::<Vec<_>>();
        if self.candidate_refs == candidate_refs {
            return;
        }
        if !self.candidate_refs.is_empty() {
            tracing::debug!(
                previous_candidates = ?self.candidate_refs,
                current_candidates = ?candidate_refs,
                "reset ViewImage candidate health after the eligible candidate list changed"
            );
        }
        self.candidate_refs = candidate_refs;
        self.failures.clear();
    }

    fn select_candidate(
        &mut self,
        mut selection: ResolvedViewImageVisionSelection,
        now: Instant,
    ) -> ResolvedViewImageVisionSelection {
        self.sync_candidate_pool(&selection);
        let mut cooling_candidates = Vec::new();
        let selected_candidate = self.candidate_refs.iter().find(|model_ref| {
            let Some(failure) = self.failures.get(*model_ref) else {
                return true;
            };
            let Some(cooling_until) = failure.cooling_until else {
                return true;
            };
            if cooling_until <= now {
                return true;
            }
            cooling_candidates.push((
                model_ref.as_string(),
                failure.consecutive_protocol_failures,
                cooling_until
                    .duration_since(now)
                    .as_secs()
                    .saturating_add(1),
            ));
            false
        });

        let Some(selected_candidate) = selected_candidate else {
            if !cooling_candidates.is_empty() {
                selection.selection.selected_mode =
                    crate::types::ViewImageSelectedMode::Unavailable;
                selection.selection.vision_provider = None;
                selection.selection.vision_model = None;
                selection.selection.resolved_model = None;
                selection.selected_route = None;
                selection.selection.selection_reason =
                    VIEW_IMAGE_ALL_CANDIDATES_COOLING_REASON.to_string();
                tracing::debug!(
                    candidates = ?cooling_candidates,
                    "ViewImage has no eligible candidate outside its cooldown"
                );
            }
            return selection;
        };

        if selection.selected_route.as_ref() == Some(selected_candidate) {
            return selection;
        }

        let Some(candidate) = selection
            .selection
            .candidates
            .iter()
            .find(|candidate| candidate.model_ref == selected_candidate.as_string())
        else {
            return selection;
        };
        let selected_provider = candidate.provider.clone();
        let selected_model = candidate.model.clone();
        selection.selection.selected_mode = if *selected_candidate == selection.primary_route {
            crate::types::ViewImageSelectedMode::NativeImageWithObservation
        } else {
            crate::types::ViewImageSelectedMode::VisionAdapter
        };
        selection.selection.vision_provider = Some(selected_provider);
        selection.selection.vision_model = Some(selected_model);
        selection.selection.resolved_model = Some(selected_candidate.as_string());
        selection.selected_route = Some(selected_candidate.clone());
        selection.selection.selection_reason =
            VIEW_IMAGE_CANDIDATE_COOLDOWN_FALLBACK_REASON.to_string();
        tracing::info!(
            cooled_candidates = ?cooling_candidates,
            selected_candidate = %selected_candidate.as_string(),
            "selected a healthy ViewImage candidate after protocol validation failures"
        );
        selection
    }

    fn record_protocol_failure(
        &mut self,
        selection: &ResolvedViewImageVisionSelection,
        now: Instant,
    ) -> Option<ViewImageCandidateFailureStatus> {
        let explicit = selection.selection.selection_source.as_deref() == Some("explicit_call");
        if !explicit {
            self.sync_candidate_pool(selection);
        }
        let model_ref = selection.selected_route.as_ref()?;
        if !explicit && !self.candidate_refs.contains(model_ref) {
            return None;
        }
        let failures = if explicit {
            &mut self.explicit_failures
        } else {
            &mut self.failures
        };
        let failure = failures.entry(model_ref.clone()).or_default();
        failure.consecutive_protocol_failures =
            failure.consecutive_protocol_failures.saturating_add(1);
        let circuit_open =
            failure.consecutive_protocol_failures >= VIEW_IMAGE_PROTOCOL_FAILURE_THRESHOLD;
        if circuit_open {
            failure.cooling_until = Some(now + VIEW_IMAGE_CANDIDATE_COOLDOWN);
        }
        Some(ViewImageCandidateFailureStatus {
            consecutive_protocol_failures: failure.consecutive_protocol_failures,
            circuit_open,
            cooldown_seconds: if circuit_open {
                VIEW_IMAGE_CANDIDATE_COOLDOWN.as_secs()
            } else {
                0
            },
        })
    }

    fn record_success(&mut self, selection: &ResolvedViewImageVisionSelection) -> bool {
        if selection.selection.selection_source.as_deref() == Some("explicit_call") {
            return selection
                .selected_route
                .as_ref()
                .is_some_and(|route| self.explicit_failures.remove(route).is_some());
        }
        self.sync_candidate_pool(selection);
        let Some(model_ref) = selection.selected_route.as_ref() else {
            return false;
        };
        let mut recovered = self.failures.remove(model_ref).is_some();
        for (failed_candidate, failure) in &mut self.failures {
            if failed_candidate == model_ref || failure.consecutive_protocol_failures == 0 {
                continue;
            }
            failure.consecutive_protocol_failures -= 1;
            recovered = true;
        }
        if recovered {
            tracing::info!(
                candidate = %model_ref.as_string(),
                "decayed ViewImage candidate failure state after a valid response"
            );
        }
        recovered
    }

    fn reset(&mut self) {
        self.candidate_refs.clear();
        self.failures.clear();
        self.explicit_failures.clear();
    }
}

use super::{
    clock::{Clock, SystemClock},
    scheduler, scheduler_executor, workspace, AgentRuntimeProjectionCache, InitialWorkspaceBinding,
    RuntimeAgent, RuntimeHandle, RuntimeInner,
};

fn model_route_parameter_override(state: &AgentState) -> Option<ModelRouteParameterOverride<'_>> {
    state
        .model_override
        .as_ref()
        .map(|route_ref| ModelRouteParameterOverride {
            route_ref,
            reasoning_effort: state.model_override_reasoning_effort.as_deref(),
            service_tier: state.model_override_service_tier,
        })
}

/// Snapshot of config-derived runtime fields that can be hot-swapped at runtime.
/// Stored behind an `ArcSwap` so that config reloads take effect on the next turn
/// without disturbing an in-progress turn.
#[derive(Clone)]
pub(super) struct ConfigSnapshot {
    pub default_timezone: Option<String>,
    pub user_home_dir: Option<PathBuf>,
    pub model_catalog: RuntimeModelCatalog,
    pub model_availability: Vec<ResolvedModelAvailability>,
    pub base_context_config: ContextConfig,
    pub provider_reconfig: Option<ProviderReconfigurator>,
    pub default_tool_output_tokens: u64,
    pub max_tool_output_tokens: u64,
    pub command_task_output_policy: crate::types::CommandTaskOutputPolicy,
    pub web_config: crate::web::WebConfig,
    pub x_search_config: Option<crate::config::XSearchRuntimeConfig>,
    pub decision_tool_enabled: bool,
    pub decision_tool_max_calls_per_turn: Option<usize>,
    pub decision_tool_timeout_ms: u64,
    pub decision_tool_min_confidence: f32,
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
            default_timezone: config.stored_config.runtime.timezone.clone(),
            user_home_dir: config.user_home_dir.clone(),
            model_catalog,
            model_availability,
            base_context_config,
            provider_reconfig,
            default_tool_output_tokens: config.default_tool_output_tokens as u64,
            max_tool_output_tokens: config.max_tool_output_tokens as u64,
            command_task_output_policy: crate::types::CommandTaskOutputPolicy {
                retention_bytes: config.command_task_output_retention_bytes,
                execution_quota_bytes: config.command_task_output_quota_bytes,
                min_free_disk_bytes: config.command_task_min_free_disk_bytes,
                min_free_disk_percent: config.command_task_min_free_disk_percent,
            },
            web_config: config.web_config.clone(),
            x_search_config: crate::config::XSearchRuntimeConfig::from_app_config(config)?,
            decision_tool_enabled: config.stored_config.decision.enabled.unwrap_or(false)
                && config
                    .stored_config
                    .decision
                    .tools
                    .as_ref()
                    .and_then(|tools| tools.enabled)
                    .unwrap_or(false),
            decision_tool_max_calls_per_turn: config
                .stored_config
                .decision
                .tools
                .as_ref()
                .and_then(|tools| tools.max_calls_per_turn),
            decision_tool_timeout_ms: config
                .stored_config
                .decision
                .tools
                .as_ref()
                .and_then(|tools| tools.timeout_ms)
                .unwrap_or(1500)
                .max(1),
            decision_tool_min_confidence: config
                .stored_config
                .decision
                .tools
                .as_ref()
                .and_then(|tools| tools.min_confidence)
                .unwrap_or(0.0),
        })
    }
}

pub(super) struct ModelDiscoveryRefreshGuard {
    runtime: RuntimeHandle,
    provider_id: crate::config::ProviderId,
    released: bool,
}

impl ModelDiscoveryRefreshGuard {
    pub(super) fn new(runtime: RuntimeHandle, provider_id: crate::config::ProviderId) -> Self {
        Self {
            runtime,
            provider_id,
            released: false,
        }
    }

    async fn release(mut self) {
        self.runtime
            .inner
            .model_discovery_refreshes
            .lock()
            .await
            .remove(&self.provider_id);
        self.runtime
            .inner
            .model_discovery_refresh_notify
            .notify_waiters();
        self.released = true;
    }
}

impl Drop for ModelDiscoveryRefreshGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let runtime = self.runtime.clone();
        let provider_id = self.provider_id.clone();
        let handle = tokio::spawn(async move {
            runtime
                .inner
                .model_discovery_refreshes
                .lock()
                .await
                .remove(&provider_id);
            runtime
                .inner
                .model_discovery_refresh_notify
                .notify_waiters();
        });
        self.runtime.track_owned_task(handle);
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
        let _ = prepare_runtime_storage(
            agent_id,
            data_dir,
            initial_workspace,
            Some(runtime_db),
            chrono::Utc::now(),
        )?;
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
        let user_home_dir = crate::config::default_user_home_dir();
        Self::new_internal(
            agent_id,
            data_dir,
            initial_workspace,
            callback_base_url,
            provider,
            default_agent_id,
            base_context_config,
            context_config,
            user_home_dir,
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

    pub(super) fn new_offline_with_runtime_db(
        agent_id: impl Into<String>,
        data_dir: PathBuf,
        initial_workspace: impl Into<InitialWorkspaceBinding>,
        runtime_db: RuntimeDb,
    ) -> Result<Self> {
        Self::new_internal(
            agent_id,
            data_dir,
            initial_workspace,
            String::new(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            String::new(),
            ContextConfig::default(),
            ContextConfig::default(),
            None,
            RuntimeModelCatalog::default(),
            Vec::new(),
            crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS,
            crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS,
            crate::web::WebConfig::default(),
            None,
            Some(runtime_db),
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
        let user_home_dir = crate::config::default_user_home_dir();
        Self::new_internal(
            agent_id,
            data_dir,
            initial_workspace,
            callback_base_url,
            provider,
            default_agent_id,
            base_context_config,
            context_config,
            user_home_dir,
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
        user_home_dir: Option<PathBuf>,
        runtime_db: RuntimeDb,
        host_bridge: RuntimeHostBridge,
        model_catalog: RuntimeModelCatalog,
        event_bus: EventBus,
        default_timezone: Option<String>,
    ) -> Result<Self> {
        let base_context_config = context_config.clone();
        let runtime = Self::new_internal(
            agent_id,
            data_dir,
            initial_workspace,
            callback_base_url,
            provider,
            default_agent_id,
            base_context_config,
            context_config,
            user_home_dir,
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
        )?;
        let mut snapshot = (**runtime.inner.config_snapshot.load()).clone();
        snapshot.default_timezone = default_timezone;
        runtime.inner.config_snapshot.store(Arc::new(snapshot));
        Ok(runtime)
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
            config.user_home_dir.clone(),
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
        user_home_dir: Option<PathBuf>,
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
        let autonomous_continuation_decision_hook = provider_reconfig
            .as_ref()
            .map(|reconfig| {
                super::decision_openai::OpenAiSemanticCandidateSelectionHook::from_app_config(
                    &reconfig.config,
                )
            })
            .transpose()?
            .flatten()
            .map(|hook| Arc::new(hook) as Arc<dyn scheduler::AsyncSemanticCandidateSelectionHook>);
        let x_search_config = provider_reconfig
            .as_ref()
            .map(|reconfig| crate::config::XSearchRuntimeConfig::from_app_config(&reconfig.config))
            .transpose()?
            .flatten();
        let command_task_output_policy = provider_reconfig
            .as_ref()
            .map(|reconfig| crate::types::CommandTaskOutputPolicy {
                retention_bytes: reconfig.config.command_task_output_retention_bytes,
                execution_quota_bytes: reconfig.config.command_task_output_quota_bytes,
                min_free_disk_bytes: reconfig.config.command_task_min_free_disk_bytes,
                min_free_disk_percent: reconfig.config.command_task_min_free_disk_percent,
            })
            .unwrap_or_default();
        let config_snapshot = Arc::new(ConfigSnapshot {
            default_timezone: provider_reconfig
                .as_ref()
                .and_then(|reconfig| reconfig.config.stored_config.runtime.timezone.clone()),
            user_home_dir,
            model_catalog: model_catalog.clone(),
            model_availability: model_availability.clone(),
            base_context_config: base_context_config.clone(),
            provider_reconfig: provider_reconfig.clone(),
            default_tool_output_tokens,
            max_tool_output_tokens,
            command_task_output_policy,
            web_config: web_config.clone(),
            x_search_config,
            decision_tool_enabled: provider_reconfig
                .as_ref()
                .and_then(|reconfig| reconfig.config.stored_config.decision.tools.as_ref())
                .and_then(|tools| tools.enabled)
                .unwrap_or(false)
                && provider_reconfig.as_ref().is_some_and(|reconfig| {
                    reconfig
                        .config
                        .stored_config
                        .decision
                        .enabled
                        .unwrap_or(false)
                }),
            decision_tool_max_calls_per_turn: provider_reconfig
                .as_ref()
                .and_then(|reconfig| reconfig.config.stored_config.decision.tools.as_ref())
                .and_then(|tools| tools.max_calls_per_turn),
            decision_tool_timeout_ms: provider_reconfig
                .as_ref()
                .and_then(|reconfig| reconfig.config.stored_config.decision.tools.as_ref())
                .and_then(|tools| tools.timeout_ms)
                .unwrap_or(1500)
                .max(1),
            decision_tool_min_confidence: provider_reconfig
                .as_ref()
                .and_then(|reconfig| reconfig.config.stored_config.decision.tools.as_ref())
                .and_then(|tools| tools.min_confidence)
                .unwrap_or(0.0),
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
        } = prepare_runtime_storage(
            agent_id,
            data_dir,
            initial_workspace,
            runtime_db,
            clock.now(),
        )?;
        if let Some(event_bus) = event_bus {
            storage.enable_event_bus(event_bus)?;
        }

        if let Some(reconfig) = config_snapshot.provider_reconfig.as_ref() {
            let chain = config_snapshot
                .model_catalog
                .provider_chain(state.model_override.as_ref());
            let mut provider_config = reconfig.config.clone();
            provider_config.runtime_max_output_tokens = config_snapshot
                .model_catalog
                .resolved_model_policy(
                    &config_snapshot.base_context_config,
                    state.model_override.as_ref(),
                )
                .runtime_max_output_tokens;
            provider = build_provider_from_model_chain_with_override(
                &provider_config,
                &chain,
                model_route_parameter_override(&state),
            )?;
        }
        let resolved_context_config = if config_snapshot.provider_reconfig.is_some() {
            config_snapshot.model_catalog.resolved_context_config(
                &config_snapshot.base_context_config,
                state.model_override.as_ref(),
            )
        } else {
            context_config.clone()
        };
        let base_provider = provider.clone();

        let identity_incarnation = runtime_db
            .agent_identities()
            .latest(&state.id)?
            .map_or(1, |identity| identity.incarnation);
        let runtime = Self {
            inner: Arc::new(RuntimeInner {
                identity_incarnation,
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
                base_provider,
                provider: RwLock::new(provider),
                turn_fallback_model: RwLock::new(None),
                config_snapshot: ArcSwap::from(config_snapshot),
                context_config: RwLock::new(resolved_context_config),
                view_image_observation_cache: Mutex::new(HashMap::new()),
                view_image_candidate_health: Mutex::new(ViewImageCandidateHealth::default()),
                model_discovery_refreshes: Mutex::new(HashSet::new()),
                model_discovery_refresh_notify: Notify::new(),
                callback_base_url,
                tools: ToolRegistry::new(PathBuf::new()),
                system: Arc::new(LocalSystem::new()),
                default_agent_id,
                host_bridge,
                task_handles: Mutex::new(HashMap::new()),
                owned_task_handles: StdMutex::new(Vec::new()),
                recovered_tasks: Mutex::new(Some(active_tasks)),
                recovered_timers: Mutex::new(Some(active_timers)),
                bootstrap_result: StdMutex::new(None),
                bootstrap_notify: Notify::new(),
                autonomous_continuation_hook: Arc::new(
                    scheduler::StaticSemanticCandidateSelectionHook,
                ),
                autonomous_continuation_decision_hook: RwLock::new(
                    autonomous_continuation_decision_hook,
                ),
                suppress_next_continue_active_tick: Mutex::new(false),
                shutdown_requested: AtomicBool::new(false),
                execution_admission: super::execution_admission::ExecutionAdmission::default(),
                transition_faults: StdMutex::new(std::collections::VecDeque::new()),
                #[cfg(test)]
                completion_binding_replacement: StdMutex::new(None),
                #[cfg(test)]
                task_transition_conflicts_remaining: AtomicUsize::new(0),
                #[cfg(test)]
                task_transition_snapshot_pause: StdMutex::new(None),
                #[cfg(test)]
                terminal_task_transition_conflicts_remaining: AtomicUsize::new(0),
                #[cfg(test)]
                fail_after_next_runtime_claim: AtomicBool::new(false),
                #[cfg(test)]
                fail_non_retryable_after_next_runtime_claim: AtomicBool::new(false),
                #[cfg(test)]
                claim_work_item_plan_status_before_commit: StdMutex::new(None),
                #[cfg(test)]
                terminal_tool_interjection_checkpoint:
                    super::TerminalToolInterjectionCheckpoint::default(),
                transition_warnings: StdMutex::new(Vec::new()),
            }),
        };
        Ok(runtime)
    }

    pub(crate) async fn current_provider(&self) -> Arc<dyn AgentProvider> {
        self.inner.provider.read().await.clone()
    }

    pub(crate) fn model_state_for(&self, state: &AgentState) -> crate::types::AgentModelState {
        self.model_state_for_turn(state, None)
    }

    pub(crate) fn model_state_for_turn(
        &self,
        state: &AgentState,
        fallback_model: Option<&ModelRouteRef>,
    ) -> crate::types::AgentModelState {
        let snap = self.inner.config_snapshot.load();
        let mut selected_state = state.clone();
        selected_state.pending_fallback_model = None;
        let mut model_state = super::agent_model_state_for_catalog(
            &snap.model_catalog,
            &snap.base_context_config,
            &selected_state,
        );
        if let Some(fallback_model) = fallback_model {
            model_state.effective_service_tier = if model_state.effective_model == *fallback_model {
                state
                    .model_override_service_tier
                    .or_else(|| snap.model_catalog.route_service_tier(fallback_model))
            } else {
                snap.model_catalog.route_service_tier(fallback_model)
            };
            model_state.active_model = Some(fallback_model.clone());
            model_state.fallback_active = model_state.effective_model != *fallback_model;
            model_state.resolved_policy = snap
                .model_catalog
                .resolved_model_policy(&snap.base_context_config, Some(fallback_model));
            model_state.effective_fallback_models = snap
                .model_catalog
                .provider_chain_for_turn(state.model_override.as_ref(), Some(fallback_model))
                .into_iter()
                .skip(1)
                .collect();
        }
        model_state
    }

    pub(crate) async fn current_apply_patch_surface(&self) -> ApplyPatchSurface {
        let state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        // The turn-local fallback binding keeps invocation-time patch parsing on
        // the surface of the model that is actually serving this turn.
        let fallback_model = self.inner.turn_fallback_model.read().await.clone();
        self.apply_patch_surface_for_turn(&state, fallback_model.as_ref())
    }

    /// Attach a shared memory-index notify so the daemon-level indexer is
    /// woken when this runtime writes new evidence for indexing.
    pub(crate) fn enable_memory_index_notify(&self, notify: Arc<tokio::sync::Notify>) {
        let _ = self.inner.storage.enable_memory_index_notify(notify);
    }

    pub(crate) fn apply_patch_surface_for_turn(
        &self,
        state: &AgentState,
        fallback_model: Option<&ModelRouteRef>,
    ) -> ApplyPatchSurface {
        let route_ref = self
            .selected_model_ref_for_state(state, fallback_model)
            .unwrap_or_else(|| self.model_state_for(state).effective_model);
        ApplyPatchSurface::for_model_route_ref(&route_ref.as_string())
    }

    fn selected_model_ref_for_state(
        &self,
        state: &AgentState,
        fallback_model: Option<&ModelRouteRef>,
    ) -> Option<ModelRouteRef> {
        let snap = self.inner.config_snapshot.load();
        snap.model_catalog
            .provider_chain_for_turn(state.model_override.as_ref(), fallback_model)
            .into_iter()
            .next()
    }

    pub(crate) async fn reconfigure_provider_for_state(&self, state: &AgentState) -> Result<()> {
        self.reconfigure_provider_for_state_and_fallback(state, None)
            .await
    }

    async fn reconfigure_provider_for_state_and_fallback(
        &self,
        state: &AgentState,
        fallback_model: Option<&ModelRouteRef>,
    ) -> Result<()> {
        let snap = self.inner.config_snapshot.load();
        let Some(reconfig) = snap.provider_reconfig.as_ref() else {
            return Err(anyhow!(
                "agent model override is unavailable for runtimes without host-managed provider configuration"
            ));
        };
        let chain = snap
            .model_catalog
            .provider_chain_for_turn(state.model_override.as_ref(), fallback_model);
        let resolved_context_config = snap.model_catalog.resolved_context_config(
            &snap.base_context_config,
            fallback_model.or(state.model_override.as_ref()),
        );
        let mut provider_config = reconfig.config.clone();
        provider_config.runtime_max_output_tokens = snap
            .model_catalog
            .resolved_model_policy(&snap.base_context_config, state.model_override.as_ref())
            .runtime_max_output_tokens;
        let provider = build_provider_from_model_chain_with_override(
            &provider_config,
            &chain,
            model_route_parameter_override(state),
        )?;
        *self.inner.provider.write().await = provider;
        *self.inner.context_config.write().await = resolved_context_config;
        Ok(())
    }

    pub(crate) async fn reconfigure_provider_for_turn(
        &self,
        fallback_model: Option<&ModelRouteRef>,
    ) -> Result<()> {
        let snap = self.inner.config_snapshot.load();
        if snap.provider_reconfig.is_none() {
            match fallback_model {
                Some(fallback_model) => {
                    let provider = self
                        .inner
                        .base_provider
                        .select_model_lineage(fallback_model)
                        .ok_or_else(|| {
                            anyhow!(
                                "static provider cannot select recovery model lineage {}",
                                fallback_model.as_string()
                            )
                        })?;
                    *self.inner.provider.write().await = provider;
                }
                None if self.inner.turn_fallback_model.read().await.is_some() => {
                    *self.inner.provider.write().await = self.inner.base_provider.clone();
                }
                None => {}
            }
            *self.inner.turn_fallback_model.write().await = fallback_model.cloned();
            return Ok(());
        }
        let state = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        self.reconfigure_provider_for_state_and_fallback(&state, fallback_model)
            .await?;
        *self.inner.turn_fallback_model.write().await = fallback_model.cloned();
        Ok(())
    }

    pub(crate) async fn current_time_timezone(&self) -> Result<chrono_tz::Tz> {
        let guard = self.inner.agent.lock().await;
        let snapshot = self.inner.config_snapshot.load();
        crate::prompt::time::resolve_timezone(
            guard.state.timezone_override.as_deref(),
            snapshot.default_timezone.as_deref(),
        )
    }

    pub async fn set_timezone_override(&self, timezone: Option<String>) -> Result<AgentState> {
        self.update_agent_state(|state| state.set_timezone(timezone.as_deref()))
            .await
    }

    /// Hot-reload config-derived runtime fields from a new `AppConfig`.
    ///
    /// Builds a fresh `ConfigSnapshot`, swaps it atomically, then rebuilds
    /// the provider for the agent's current model-override state. The swap
    /// is atomic via `ArcSwap`, so an in-progress turn that already loaded
    /// the old snapshot continues unaffected; the next turn picks up the
    /// new snapshot automatically.
    pub(crate) async fn reload_config(&self, config: &AppConfig) -> Result<()> {
        let mut config = config.clone();
        config.user_home_dir = self.inner.config_snapshot.load().user_home_dir.clone();
        let new_snapshot = Arc::new(ConfigSnapshot::from_config(&config)?);
        let should_reconfigure_provider = new_snapshot.provider_reconfig.is_some();
        let decision_hook = new_snapshot
            .provider_reconfig
            .as_ref()
            .map(|reconfig| {
                super::decision_openai::OpenAiSemanticCandidateSelectionHook::from_app_config(
                    &reconfig.config,
                )
            })
            .transpose()?
            .flatten()
            .map(|hook| Arc::new(hook) as Arc<dyn scheduler::AsyncSemanticCandidateSelectionHook>);
        // Atomically swap the snapshot.
        self.inner.config_snapshot.store(new_snapshot);
        self.inner.view_image_candidate_health.lock().await.reset();
        *self
            .inner
            .autonomous_continuation_decision_hook
            .write()
            .await = decision_hook;
        // Rebuild provider + context_config for current state. This also
        // upgrades a bootstrap runtime when the reloaded config is ready.
        if should_reconfigure_provider {
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

    #[cfg(test)]
    pub(crate) async fn current_view_image_vision_selection(
        &self,
    ) -> Result<crate::types::ViewImageVisionSelection> {
        Ok(self
            .current_view_image_vision_route_selection()
            .await?
            .selection)
    }

    pub(crate) async fn current_view_image_vision_route_selection(
        &self,
    ) -> Result<ResolvedViewImageVisionSelection> {
        let selection = self.view_image_vision_selection().await?;
        let selection = self.select_healthy_view_image_candidate(selection).await;
        if !should_refresh_view_image_discovery(&selection) {
            return Ok(selection);
        }
        let failures = self
            .refresh_view_image_discovery_candidates(&selection.selection)
            .await?;
        let mut selection = self.view_image_vision_selection().await?;
        for (provider, error) in failures {
            selection
                .selection
                .candidates
                .push(crate::types::ViewImageVisionCandidate {
                    provider: provider.as_str().to_string(),
                    model: "*".to_string(),
                    model_ref: format!("{}/*", provider.as_str()),
                    image_input: false,
                    reason: format!("provider_model_discovery_failed: {error}"),
                });
        }
        Ok(self.select_healthy_view_image_candidate(selection).await)
    }

    async fn select_healthy_view_image_candidate(
        &self,
        selection: ResolvedViewImageVisionSelection,
    ) -> ResolvedViewImageVisionSelection {
        self.inner
            .view_image_candidate_health
            .lock()
            .await
            .select_candidate(selection, Instant::now())
    }

    pub(crate) async fn record_view_image_candidate_protocol_failure(
        &self,
        selection: &ResolvedViewImageVisionSelection,
    ) -> Option<ViewImageCandidateFailureStatus> {
        let status = self
            .inner
            .view_image_candidate_health
            .lock()
            .await
            .record_protocol_failure(selection, Instant::now());
        if let Some(status) = status {
            tracing::warn!(
                candidate = selection
                    .selected_route
                    .as_ref()
                    .map(ModelRouteRef::as_string)
                    .unwrap_or_else(|| "unknown".to_string()),
                consecutive_protocol_failures = status.consecutive_protocol_failures,
                failure_threshold = VIEW_IMAGE_PROTOCOL_FAILURE_THRESHOLD,
                cooldown_seconds = status.cooldown_seconds,
                circuit_open = status.circuit_open,
                "ViewImage candidate returned an invalid visual observation"
            );
        }
        status
    }

    pub(crate) async fn record_view_image_candidate_success(
        &self,
        selection: &ResolvedViewImageVisionSelection,
    ) {
        self.inner
            .view_image_candidate_health
            .lock()
            .await
            .record_success(selection);
    }

    async fn view_image_vision_selection(&self) -> Result<ResolvedViewImageVisionSelection> {
        let state = self.agent_state().await?;
        let fallback_model = self.inner.turn_fallback_model.read().await.clone();
        let snap = self.inner.config_snapshot.load();
        Ok(snap
            .model_catalog
            .select_view_image_vision_model_with_route(
                &snap.base_context_config,
                state.model_override.as_ref(),
                fallback_model.as_ref(),
            ))
    }

    async fn refresh_view_image_discovery_candidates(
        &self,
        selection: &crate::types::ViewImageVisionSelection,
    ) -> Result<Vec<(crate::config::ProviderId, String)>> {
        let snap = self.inner.config_snapshot.load();
        let Some(reconfig) = snap.provider_reconfig.as_ref() else {
            return Ok(Vec::new());
        };
        let config = reconfig.config.clone();
        let cache_path = discovery_cache_path(&config.home_dir);
        let cache_path_for_load = cache_path.clone();
        let cache =
            tokio::task::spawn_blocking(move || load_discovery_cache_at(&cache_path_for_load))
                .await??;
        let candidate_providers = selection
            .candidates
            .iter()
            .filter_map(|candidate| crate::config::ProviderId::parse(&candidate.provider).ok())
            .collect::<HashSet<_>>();
        let providers = config
            .providers
            .values()
            .filter(|provider| {
                provider_discovers_model_capabilities(provider)
                    && (candidate_providers.contains(&provider.id)
                        || crate::provider::provider_definition(provider.id.as_str()).is_some_and(
                            |definition| {
                                definition.catalog_policy
                                    == crate::provider::ProviderCatalogPolicy::DiscoveryOnly
                            },
                        ))
                    && (provider.auth.kind == crate::config::CredentialKind::None
                        || provider.has_configured_credential())
                    && discovery_cache_needs_refresh(provider, &cache, DEFAULT_DISCOVERY_CACHE_TTL)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for provider in providers {
            if let Err(error) = self
                .refresh_model_discovery_provider(provider.clone(), &cache_path)
                .await
            {
                tracing::warn!(
                    provider = %provider.id.as_str(),
                    error = %error,
                    "ViewImage model discovery refresh failed"
                );
                failures.push((provider.id, error.to_string()));
            }
        }
        self.reload_model_discovery_cache_snapshot().await?;
        Ok(failures)
    }

    async fn refresh_model_discovery_provider(
        &self,
        provider: crate::config::ProviderRuntimeConfig,
        cache_path: &std::path::Path,
    ) -> Result<()> {
        loop {
            let notified = self.inner.model_discovery_refresh_notify.notified();
            let should_refresh = {
                let mut in_flight = self.inner.model_discovery_refreshes.lock().await;
                if in_flight.contains(&provider.id) {
                    false
                } else {
                    in_flight.insert(provider.id.clone());
                    true
                }
            };
            if should_refresh {
                let refresh_guard =
                    ModelDiscoveryRefreshGuard::new(self.clone(), provider.id.clone());
                let result = refresh_provider_models(&provider, cache_path).await;
                refresh_guard.release().await;
                result?;
                return Ok(());
            }
            notified.await;
            if self
                .inner
                .model_discovery_refreshes
                .lock()
                .await
                .contains(&provider.id)
            {
                continue;
            }
            let cache_path = cache_path.to_path_buf();
            let cache =
                tokio::task::spawn_blocking(move || load_discovery_cache_at(&cache_path)).await??;
            if !discovery_cache_needs_refresh(&provider, &cache, DEFAULT_DISCOVERY_CACHE_TTL) {
                return Ok(());
            }
            return Err(anyhow!(
                "concurrent provider model discovery did not produce a fresh cache entry"
            ));
        }
    }

    pub(crate) async fn prepare_view_image_observation(
        &self,
        model: Option<&str>,
    ) -> Result<PreparedViewImageObservation> {
        let requested = model
            .map(|model| {
                ModelRouteRef::parse_compatible(model.trim()).map_err(|_| {
                    crate::tool::ToolError::new(
                        "invalid_tool_input",
                        "ViewImage `model` must be provider@endpoint/model or provider/model",
                    )
                    .with_recovery_hint(
                        "omit model to use the default route, or supply a valid model route",
                    )
                })
            })
            .transpose()?;
        let automatic = if requested.is_none() {
            Some(self.current_view_image_vision_route_selection().await?)
        } else {
            None
        };
        let state = self.agent_state().await?;
        let fallback = self.inner.turn_fallback_model.read().await.clone();
        let snap = self.inner.config_snapshot.load_full();
        let selection = if let Some(requested) = requested.as_ref() {
            snap.model_catalog.select_view_image_vision_model_for_call(
                &snap.base_context_config,
                state.model_override.as_ref(),
                fallback.as_ref(),
                requested,
            )
        } else {
            // Discovery can publish a new snapshot. Resolve the default again
            // from the snapshot used for all subsequent preflight checks.
            let mut resolved = snap
                .model_catalog
                .select_view_image_vision_model_with_route(
                    &snap.base_context_config,
                    state.model_override.as_ref(),
                    fallback.as_ref(),
                );
            if let Some(automatic) = automatic {
                resolved.selection.candidates.extend(
                    automatic
                        .selection
                        .candidates
                        .into_iter()
                        .filter(|candidate| candidate.model == "*"),
                );
            }
            self.select_healthy_view_image_candidate(resolved).await
        };
        let explicit = requested.is_some();
        let unavailable_kind = if explicit {
            "model_route_unavailable"
        } else {
            "vision_adapter_unavailable"
        };
        let route_ref = requested
            .as_ref()
            .or(selection.selected_route.as_ref())
            .ok_or_else(|| {
                view_image_route_error(
                    unavailable_kind,
                    &selection.selection.selection_reason,
                    &selection.selection,
                )
            })?;
        let metadata = snap
            .model_catalog
            .resolve_explicit_model_metadata(&snap.base_context_config, route_ref)
            .ok_or_else(|| {
                view_image_route_error(
                    unavailable_kind,
                    "endpoint_unavailable",
                    &selection.selection,
                )
            })?;
        if !crate::config::provider_has_usable_auth(&metadata.endpoint.runtime_config) {
            return Err(view_image_route_error(
                unavailable_kind,
                "credentials_unavailable",
                &selection.selection,
            ));
        }
        let route = snap
            .model_catalog
            .resolve_explicit_model_route(
                &snap.base_context_config,
                route_ref,
                ModelRouteCapability::VisionObservation,
            )
            .ok_or_else(|| {
                view_image_route_error(
                    if explicit {
                        "media_capability_mismatch"
                    } else {
                        unavailable_kind
                    },
                    "model_or_transport_lacks_view_image_observation",
                    &selection.selection,
                )
            })?;
        if explicit {
            let health = self.inner.view_image_candidate_health.lock().await;
            let cooling = [
                health.failures.get(&route.route_ref),
                health.explicit_failures.get(&route.route_ref),
            ]
            .into_iter()
            .flatten()
            .any(|failure| {
                failure
                    .cooling_until
                    .is_some_and(|until| until > Instant::now())
            });
            if cooling {
                return Err(view_image_route_error(
                    "model_route_unavailable",
                    "selected_route_cooling",
                    &selection.selection,
                ));
            }
        }
        let reconfig = snap.provider_reconfig.as_ref().ok_or_else(|| {
            view_image_route_error(
                unavailable_kind,
                "host_provider_configuration_unavailable",
                &selection.selection,
            )
        })?;
        // Construct and validate the exact provider before looking at cached observations.
        let provider = build_candidate_from_model_route(&reconfig.config.home_dir, &route)
            .map_err(|_| {
                view_image_route_error(
                    unavailable_kind,
                    "provider_initialization_failed",
                    &selection.selection,
                )
            })?
            .provider;
        Ok(PreparedViewImageObservation {
            endpoint_config_identity: view_image_endpoint_config_identity(
                &route,
                &reconfig.config.home_dir,
            ),
            route_selection: selection,
            provider,
        })
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
        prepared: &PreparedViewImageObservation,
        prompt: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<String> {
        let selection = &prepared.route_selection;
        if selection.selection.selected_mode == crate::types::ViewImageSelectedMode::Unavailable {
            return Err(anyhow!("no configured model supports image_input"));
        }
        let mut request = ProviderTurnRequest::plain(
            "You are a vision adapter for a headless agent. Inspect only the provided image and task prompt. Return exactly one JSON object and no markdown, prose, or implementation advice. The JSON object must match this shape: {\"type\":\"visual_observation\",\"schema\":\"visual_observation.v1\",\"summary\":\"string\",\"ocr\":[],\"elements\":[],\"relations\":[],\"issues\":[],\"uncertainties\":[],\"external_sources\":[]}. Required fields: type=\"visual_observation\", schema=\"visual_observation.v1\", summary, uncertainties. The uncertainties field must be an array of strings; use [] when there are no caveats. The ocr, elements, relations, issues, and external_sources fields must be arrays of objects; omit them or use [] when empty. Include visible text in ocr or summary; include bounding boxes when location matters; describe only visible evidence; say when uncertain.",
            vec![ConversationMessage::UserImage {
                prompt: prompt.to_string(),
                media_type: media_type.to_string(),
                data_base64: BASE64_STANDARD.encode(bytes),
                optional_history: false,
            }],
            Vec::new(),
        );
        request.response_format = Some(visual_observation_response_format());
        let response = prepared.provider.complete_turn(request).await?;
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
        requested_model: Option<&str>,
        request: ProviderGenerateImageRequest,
    ) -> Result<crate::provider::image_generation::GeneratedImageExecution> {
        use crate::provider::image_generation::{
            generate_image_with_shared_quota, resolve_image_generation_contract,
            GeneratedImageExecution, ImageGenerationParameters,
        };
        use crate::tool::ToolError;
        use serde_json::json;

        let state = self.agent_state().await?;
        let fallback_model = self.inner.turn_fallback_model.read().await.clone();
        let snap = self.inner.config_snapshot.load();
        let requested_route = requested_model
            .map(ModelRouteRef::parse_compatible)
            .transpose()
            .map_err(|_| {
                ToolError::new(
                    "model_route_unavailable",
                    "invalid image generation model route",
                )
                .with_recovery_hint(
                    "use a canonical provider@endpoint/model route from ListProviderModels",
                )
            })?;
        let route = snap
            .model_catalog
            .select_generate_image_route_for_call(
                &snap.base_context_config,
                requested_route.as_ref(),
                state.model_override.as_ref(),
                fallback_model.as_ref(),
            )
            .ok_or_else(|| ToolError::new(
                "model_route_unavailable",
                "selected image generation route is unavailable or lacks image_generation capability",
            ).with_details(json!({"requested_model": requested_model}))
             .with_recovery_hint("select an enabled image_output route with usable credentials; explicit choices do not fall back"))?;
        let contract = resolve_image_generation_contract(
            &route.route_ref,
            route.provider_config(),
            route.capabilities.image_output,
        );
        let parameters = contract
            .resolve_parameters(&ImageGenerationParameters {
                size: request.size,
                aspect_ratio: request.aspect_ratio,
                resolution: request.resolution,
                background: request.background,
                output_format: request.output_format,
            })
            .map_err(|error| {
                ToolError::new(error.code, error.to_string())
                    .with_details(serde_json::to_value(&error).unwrap_or_default())
                    .with_recovery_hint(error.recovery_hint.clone())
            })?;
        let resolved = &parameters.provider_parameters;
        let request = ProviderGenerateImageRequest {
            prompt: request.prompt,
            size: resolved.size.clone(),
            aspect_ratio: resolved.aspect_ratio.clone(),
            resolution: resolved.resolution.clone(),
            background: resolved.background.clone(),
            output_format: resolved.output_format.clone(),
        };
        let reconfig = snap.provider_reconfig.as_ref().ok_or_else(|| {
            anyhow!("image generation requires host-managed provider configuration")
        })?;
        let provider =
            build_candidate_from_model_route(&reconfig.config.home_dir, &route)?.provider;
        let response = generate_image_with_shared_quota(provider.as_ref(), request).await.map_err(|error| {
            if let Some(tool_error) = error.downcast_ref::<ToolError>() {
                let mut details = tool_error.details.clone().unwrap_or_else(|| json!({}));
                details["route"] = json!(route.route_ref.as_string());
                return tool_error.clone().with_details(details);
            }
            if let Some(parameter) = error.downcast_ref::<crate::provider::image_generation::ImageGenerationParameterError>() {
                return ToolError::new(parameter.code, parameter.to_string())
                    .with_details(json!({"route": route.route_ref.as_string(), "field": parameter.field}))
                    .with_recovery_hint(parameter.recovery_hint.clone());
            }
            let transport = error.downcast_ref::<crate::provider::ProviderTransportError>();
            let status = transport.and_then(|error| error.status);
            let rejected = matches!(status, Some(400 | 422));
            ToolError::new(
                if rejected { "provider_image_parameter_rejected" } else { "image_generation_failed" },
                if rejected { "provider rejected the selected image request; its parameter contract may have changed" }
                    else { "selected image provider request failed" },
            )
                .with_details(json!({
                    "route": route.route_ref.as_string(),
                    "provider_failure": crate::provider::classify_provider_error(&error),
                    "http_status": status,
                    "provider_error_code": transport.and_then(|error| error.code.as_deref()),
                }))
                .with_recovery_hint("inspect the selected route's provider error; no alternate model was tried")
        })?;
        let selection = json!({
            "requested_model": requested_route.as_ref().map(ModelRouteRef::as_string),
            "resolved_model": route.route_ref.as_string(),
            "selection_source": if requested_route.is_some() { "explicit" }
                else if snap.model_catalog.image_generation_model.is_some() { "image_generation_default" }
                else { "auto" },
            "contract_version": contract.contract_version,
        });
        Ok(GeneratedImageExecution {
            response,
            selection,
            contract,
            parameters,
        })
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
        let Ok(_admission) = self.execution_admission_lease() else {
            return;
        };
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
            let refresh_guard =
                ModelDiscoveryRefreshGuard::new(runtime.clone(), provider_id.clone());
            let handle = tokio::spawn(async move {
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
                refresh_guard.release().await;
            });
            self.track_owned_task(handle);
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

    pub(crate) async fn model_availability(&self) -> Result<Vec<ResolvedModelAvailability>> {
        let mut models = if let Some(config) = self.model_config_with_fresh_discovery_cache().await
        {
            resolved_model_availability(&config)
        } else {
            self.inner.config_snapshot.load().model_availability.clone()
        };
        if self.agent_capability_policy().await?.is_some_and(|policy| {
            !policy.allows(crate::types::ToolCapabilityFamily::LocalEnvironment)
        }) {
            for model in &mut models {
                model.media.deny_tool_access();
            }
        }
        Ok(models)
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
    now: chrono::DateTime<chrono::Utc>,
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
    #[cfg(test)]
    if runtime_db.agent_identities().latest(&agent_id)?.is_none() {
        runtime_db
            .agent_identities()
            .upsert(&crate::types::AgentIdentityRecord::new(
                agent_id.clone(),
                crate::types::AgentKind::Default,
                crate::types::AgentVisibility::Public,
                crate::types::AgentOwnership::SelfOwned,
                crate::types::AgentProfilePreset::PublicNamed,
                None,
                None,
            ))?;
    }
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
    let timer_wake_recovery = runtime_db
        .timers()
        .normalize_wakes_for_recovery(&agent_id, now)?;
    if timer_wake_recovery.retained_wakes > 0
        || timer_wake_recovery.created_wakes > 0
        || timer_wake_recovery.reactivated_wakes > 0
        || timer_wake_recovery.invalidated_wakes > 0
        || !timer_wake_recovery.dropped_message_ids.is_empty()
        || !timer_wake_recovery.settled_wait_conditions.is_empty()
    {
        storage.append_event(&AuditEvent::legacy(
            "timer_wakes_recovered",
            serde_json::json!({
                "agent_id": agent_id,
                "retained_wakes": timer_wake_recovery.retained_wakes,
                "created_wakes": timer_wake_recovery.created_wakes,
                "reactivated_wakes": timer_wake_recovery.reactivated_wakes,
                "invalidated_wakes": timer_wake_recovery.invalidated_wakes,
                "dropped_message_ids": timer_wake_recovery.dropped_message_ids,
                "cancelled_wait_condition_ids": timer_wake_recovery.settled_wait_conditions
                    .iter().map(|condition| &condition.id).collect::<Vec<_>>(),
                "blocked_work_item_ids": timer_wake_recovery.work_items
                    .iter().map(|work| &work.id).collect::<Vec<_>>(),
                "audit_event_ids": timer_wake_recovery.audit_events
                    .iter().map(|event| &event.id).collect::<Vec<_>>(),
            }),
        ))?;
    }

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
    if let Some(legacy_fallback) = state.pending_fallback_model.take() {
        storage.append_event(&AuditEvent::legacy(
            "legacy_pending_fallback_discarded",
            serde_json::json!({
                "agent_id": agent_id,
                "fallback_model_ref": legacy_fallback,
                "reason": "fallback_selection_is_message_bound",
            }),
        ))?;
    }
    state.pending = queue.len();
    state.total_message_count = storage.count_messages().unwrap_or_default();
    scheduler_executor::apply_bootstrap_recovered_projection(
        &mut state,
        scheduler_executor::BootstrapRecoveryFacts {
            queued_messages: queue.len(),
        },
    );
    if let Some(active) = state.active_workspace_entry.as_ref() {
        let existing = runtime_db
            .execution_root_entries()
            .get(&active.execution_root_id)?;
        let entry = workspace::execution_root_entry_from_active(
            active,
            existing
                .as_ref()
                .map(|entry| entry.created_at)
                .unwrap_or_else(chrono::Utc::now),
            existing.and_then(|entry| entry.worktree),
        );
        if let Err(error) = runtime_db.execution_root_entries().upsert(&entry) {
            tracing::warn!(
                agent_id = %agent_id,
                execution_root_id = %entry.execution_root_id,
                workspace_id = %entry.workspace_id,
                error = %error,
                "failed to reconcile active execution root registry entry during bootstrap"
            );
        }
    }
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

fn should_refresh_view_image_discovery(selection: &ResolvedViewImageVisionSelection) -> bool {
    selection.selection.selected_mode
        != crate::types::ViewImageSelectedMode::NativeImageWithObservation
        && selection.selection.selection_reason != VIEW_IMAGE_ALL_CANDIDATES_COOLING_REASON
        && selection.selection.selection_reason != VIEW_IMAGE_CANDIDATE_COOLDOWN_FALLBACK_REASON
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

#[cfg(test)]
mod view_image_route_tests {
    use super::*;
    use crate::{
        config::{
            CredentialKind, CredentialSource, ModelRef, ProviderEndpointId, ProviderId,
            ProviderTransportKind,
        },
        host::RuntimeHost,
        model_catalog::{ModelCapabilityOverride, ModelRuntimeOverride},
        tool::{tools::view_image, ToolError},
        types::{AuthorityClass, ViewImageResult},
    };
    use axum::{http::StatusCode, routing::post, Json, Router};
    use serde_json::{json, Value};
    use std::sync::atomic::Ordering;
    use tempfile::{tempdir, TempDir};

    struct VisionServer {
        url: String,
        calls: Arc<AtomicUsize>,
        models: Arc<StdMutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for VisionServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl VisionServer {
        async fn new(label: &str, status: StatusCode) -> Self {
            let calls = Arc::new(AtomicUsize::new(0));
            let models = Arc::new(StdMutex::new(Vec::new()));
            let observed_calls = calls.clone();
            let observed_models = models.clone();
            let label = label.to_string();
            let app = Router::new().route(
                "/v1/chat/completions",
                post(move |Json(request): Json<Value>| {
                    let calls = observed_calls.clone();
                    let models = observed_models.clone();
                    let label = label.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        models.lock().unwrap().push(request["model"].as_str().unwrap().to_string());
                        let body = if status.is_success() {
                            json!({
                                "id": "vision-test", "object": "chat.completion",
                                "model": request["model"],
                                "choices": [{
                                    "index": 0, "finish_reason": "stop",
                                    "message": {"role": "assistant", "content": json!({
                                        "type": "visual_observation", "schema": "visual_observation.v1",
                                        "summary": label, "uncertainties": [],
                                    }).to_string()},
                                }],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                            })
                        } else {
                            json!({"error": {"message": "unsupported image request", "type": "invalid_request_error", "code": "invalid_request_error"}})
                        };
                        (status, Json(body))
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/v1", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            Self {
                url,
                calls,
                models,
                task,
            }
        }

        fn count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    async fn vision_runtime(server: &VisionServer) -> (TempDir, RuntimeHost, RuntimeHandle) {
        let home = tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"ollama/vision-a"}}"#,
        )
        .unwrap();
        let mut config = {
            let _guard = crate::test_env::lock_env();
            AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap()
        };
        config.default_model = ModelRouteRef::parse_compatible("ollama/vision-a").unwrap();
        config.vision_model = Some(config.default_model.clone());
        config.vision_candidate_models.clear();
        config.fallback_models.clear();
        config.default_agent_id = "default".to_string();
        config.workspace_dir = home.path().join("workspace");
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&1_u32.to_be_bytes());
        png.extend_from_slice(&1_u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(config.workspace_dir.join("sample.png"), png).unwrap();
        let provider_id = ProviderId::parse("ollama").unwrap();
        config.providers.retain(|id, _| id == &provider_id);
        let provider = config.providers.get_mut(&provider_id).unwrap();
        provider.transport = ProviderTransportKind::OpenAiChatCompletions;
        provider.base_url = server.url.clone();
        provider.auth.source = CredentialSource::None;
        provider.auth.kind = CredentialKind::None;
        provider.credential = None;
        for (model, image_input) in [("vision-a", true), ("vision-b", true), ("text-only", false)] {
            config.validated_model_overrides.insert(
                ModelRef::parse(&format!("ollama/{model}")).unwrap(),
                ModelRuntimeOverride {
                    capabilities: Some(ModelCapabilityOverride {
                        agent_turn: Some(true),
                        image_input: Some(image_input),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            );
        }
        let host = RuntimeHost::new(config).unwrap();
        let runtime = host.default_runtime().await.unwrap();
        (home, host, runtime)
    }

    fn update_snapshot(runtime: &RuntimeHandle, update: impl FnOnce(&mut ConfigSnapshot)) {
        let mut snapshot = (*runtime.inner.config_snapshot.load_full()).clone();
        update(&mut snapshot);
        runtime.inner.config_snapshot.store(Arc::new(snapshot));
    }

    async fn observe(
        runtime: &RuntimeHandle,
        home: &TempDir,
        model: Option<&str>,
    ) -> Result<ViewImageResult> {
        let mut input = json!({
            "path": home.path().join("workspace/sample.png"),
            "prompt": "Describe the image",
        });
        if let Some(model) = model {
            input["model"] = json!(model);
        }
        let result = view_image::execute(
            runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &input,
        )
        .await?;
        Ok(serde_json::from_value(result.envelope.result.unwrap())?)
    }

    #[tokio::test]
    async fn media_defaults_follow_current_fallback_without_changing_selection() {
        let server = VisionServer::new("unused", StatusCode::OK).await;
        let (_home, _host, runtime) = vision_runtime(&server).await;
        update_snapshot(&runtime, |snapshot| {
            let catalog = &mut snapshot.model_catalog;
            catalog.vision_model = None;
            for endpoint in &mut catalog.provider_endpoints {
                endpoint.runtime_config.transport = ProviderTransportKind::OpenAiResponses;
            }
            for model in ["vision-a", "vision-b"] {
                catalog
                    .model_overrides
                    .get_mut(&ModelRef::parse(&format!("ollama/{model}")).unwrap())
                    .unwrap()
                    .capabilities
                    .as_mut()
                    .unwrap()
                    .image_generation = Some(true);
            }
        });
        let primary = ModelRouteRef::parse_compatible("ollama/vision-a").unwrap();
        let fallback = ModelRouteRef::parse_compatible("ollama/vision-b").unwrap();
        let defaults = runtime.media_tool_defaults(None).await.unwrap();
        assert_eq!(
            defaults.view_image.as_deref(),
            Some(primary.as_string().as_str())
        );
        assert_eq!(
            defaults.generate_image.as_deref(),
            Some(primary.as_string().as_str())
        );
        *runtime.inner.turn_fallback_model.write().await = Some(fallback.clone());
        let defaults = runtime.media_tool_defaults(None).await.unwrap();
        assert_eq!(
            defaults.view_image.as_deref(),
            Some(fallback.as_string().as_str())
        );
        assert_eq!(
            defaults.generate_image.as_deref(),
            Some(fallback.as_string().as_str())
        );
        let pending = runtime.media_tool_defaults(Some(&primary)).await.unwrap();
        assert_eq!(
            pending.view_image.as_deref(),
            Some(primary.as_string().as_str())
        );
        assert_eq!(
            pending.generate_image.as_deref(),
            Some(primary.as_string().as_str())
        );
        assert_eq!(
            *runtime.inner.turn_fallback_model.read().await,
            Some(fallback)
        );
        assert_eq!(server.count(), 0);
    }

    #[tokio::test]
    async fn view_image_single_call_selection_is_concurrent_and_default_compatible() {
        let server = VisionServer::new("visible sample", StatusCode::OK).await;
        let (home, _host, runtime) = vision_runtime(&server).await;
        let default = observe(&runtime, &home, None).await.unwrap();
        assert!(!default.cache_hit);
        assert_eq!(
            default.vision_selection.selection_source.as_deref(),
            Some("vision_default")
        );
        let (legacy, other) = tokio::join!(
            observe(&runtime, &home, Some("ollama/vision-a")),
            observe(&runtime, &home, Some("ollama@default/vision-b")),
        );
        let legacy = legacy.unwrap();
        let other = other.unwrap();
        assert!(legacy.cache_hit);
        assert_eq!(
            legacy.observation.generated_by,
            default.observation.generated_by
        );
        assert_eq!(
            legacy.vision_selection.requested_model.as_deref(),
            Some("ollama@default/vision-a")
        );
        assert_eq!(
            other.observation.generated_by.resolved_model.as_deref(),
            Some("ollama@default/vision-b")
        );
        assert!(!other.cache_hit);
        assert_eq!(server.count(), 2);
        let selected = runtime
            .current_view_image_vision_route_selection()
            .await
            .unwrap();
        assert_eq!(
            selected.selected_route.unwrap().as_string(),
            "ollama@default/vision-a"
        );
        assert!(runtime
            .agent_state()
            .await
            .unwrap()
            .model_override
            .is_none());
        assert!(runtime.inner.turn_fallback_model.read().await.is_none());
        assert_eq!(*server.models.lock().unwrap(), vec!["vision-a", "vision-b"]);
    }

    #[tokio::test]
    async fn view_image_cache_isolates_same_model_endpoints_and_base_url_changes() {
        let first = VisionServer::new("first endpoint", StatusCode::OK).await;
        let second = VisionServer::new("second endpoint", StatusCode::OK).await;
        let (home, _host, runtime) = vision_runtime(&first).await;
        let original = observe(&runtime, &home, None).await.unwrap();
        update_snapshot(&runtime, |snapshot| {
            let mut endpoint = snapshot.model_catalog.provider_endpoints[0].clone();
            endpoint.endpoint = ProviderEndpointId::parse("secondary").unwrap();
            endpoint.runtime_config.route_endpoint = endpoint.endpoint.clone();
            snapshot.model_catalog.provider_endpoints.push(endpoint);
        });
        let secondary = observe(&runtime, &home, Some("ollama@secondary/vision-a"))
            .await
            .unwrap();
        assert!(!secondary.cache_hit);
        assert_eq!(
            secondary.observation.generated_by.resolved_model.as_deref(),
            Some("ollama@secondary/vision-a")
        );
        assert_eq!(
            first.count(),
            2,
            "same URL is not a substitute for endpoint identity"
        );
        let pinned = runtime
            .prepare_view_image_observation(Some("ollama/vision-a"))
            .await
            .unwrap();
        update_snapshot(&runtime, |snapshot| {
            snapshot.model_catalog.provider_endpoints[0]
                .runtime_config
                .base_url = second.url.clone();
        });
        let changed = observe(&runtime, &home, Some("ollama/vision-a"))
            .await
            .unwrap();
        assert!(!changed.cache_hit);
        assert_eq!(changed.observation.summary, "second endpoint");
        assert_eq!(
            changed.observation.generated_by.resolved_model,
            original.observation.generated_by.resolved_model
        );
        assert_eq!(second.count(), 1);
        let bytes = std::fs::read(home.path().join("workspace/sample.png")).unwrap();
        let text = runtime
            .generate_view_image_observation(&pinned, "Inspect", "image/png", &bytes)
            .await
            .unwrap();
        assert!(
            text.contains("first endpoint"),
            "in-flight requests retain their validated provider"
        );
        assert_eq!(first.count(), 3);
        assert_eq!(second.count(), 1);
        assert!(
            observe(&runtime, &home, Some("ollama/vision-a"))
                .await
                .unwrap()
                .cache_hit
        );
    }

    #[tokio::test]
    async fn view_image_explicit_preflight_rejects_before_inference_and_before_cache() {
        let server = VisionServer::new("cached sample", StatusCode::OK).await;
        let (home, _host, runtime) = vision_runtime(&server).await;
        observe(&runtime, &home, None).await.unwrap();
        for (model, kind) in [
            ("", "invalid_tool_input"),
            ("bad-ref", "invalid_tool_input"),
            ("ollama@missing/vision-a", "model_route_unavailable"),
            (
                "missing-provider@default/vision-a",
                "model_route_unavailable",
            ),
            ("ollama@default/text-only", "media_capability_mismatch"),
        ] {
            let error = observe(&runtime, &home, Some(model)).await.unwrap_err();
            assert_eq!(ToolError::from_anyhow(&error).kind, kind, "{model}");
            assert_eq!(server.count(), 1);
        }
        update_snapshot(&runtime, |snapshot| {
            snapshot.model_catalog.provider_endpoints[0]
                .runtime_config
                .transport = ProviderTransportKind::GeminiGenerateContent;
        });
        let error = observe(&runtime, &home, Some("ollama/vision-a"))
            .await
            .unwrap_err();
        assert_eq!(
            ToolError::from_anyhow(&error).kind,
            "media_capability_mismatch"
        );
        update_snapshot(&runtime, |snapshot| {
            let endpoint = &mut snapshot.model_catalog.provider_endpoints[0].runtime_config;
            endpoint.transport = ProviderTransportKind::OpenAiChatCompletions;
            endpoint.auth.source = CredentialSource::Env;
            endpoint.auth.kind = CredentialKind::ApiKey;
            endpoint.credential = None;
        });
        let error = observe(&runtime, &home, Some("ollama/vision-a"))
            .await
            .unwrap_err();
        let error = ToolError::from_anyhow(&error);
        assert_eq!(error.kind, "model_route_unavailable");
        assert_eq!(error.details.unwrap()["reason"], "credentials_unavailable");
        assert_eq!(
            server.count(),
            1,
            "a cached observation cannot bypass credential validation"
        );
    }

    #[tokio::test]
    async fn view_image_explicit_provider_rejection_never_uses_default_route() {
        let primary = VisionServer::new("default", StatusCode::OK).await;
        let rejected = VisionServer::new("rejected", StatusCode::BAD_REQUEST).await;
        let (home, _host, runtime) = vision_runtime(&primary).await;
        update_snapshot(&runtime, |snapshot| {
            let mut endpoint = snapshot.model_catalog.provider_endpoints[0].clone();
            endpoint.endpoint = ProviderEndpointId::parse("rejected").unwrap();
            endpoint.runtime_config.route_endpoint = endpoint.endpoint.clone();
            endpoint.runtime_config.base_url = rejected.url.clone();
            snapshot.model_catalog.provider_endpoints.push(endpoint);
        });
        let error = observe(&runtime, &home, Some("ollama@rejected/vision-b"))
            .await
            .unwrap_err();
        let error = ToolError::from_anyhow(&error);
        assert_eq!(error.kind, "vision_observation_failed");
        let details = error.details.unwrap();
        assert_eq!(
            details["selection"]["resolved_model"],
            "ollama@rejected/vision-b"
        );
        assert_eq!(details["provider_failure"]["kind"], "contract_error");
        assert_eq!(rejected.count(), 1);
        assert_eq!(primary.count(), 0);
    }

    #[tokio::test]
    async fn view_image_explicit_cooldown_cannot_be_bypassed_by_cached_observation() {
        let server = VisionServer::new("cached sample", StatusCode::OK).await;
        let (home, _host, runtime) = vision_runtime(&server).await;
        observe(&runtime, &home, None).await.unwrap();
        let prepared = runtime
            .prepare_view_image_observation(Some("ollama/vision-a"))
            .await
            .unwrap();
        runtime
            .record_view_image_candidate_protocol_failure(&prepared.route_selection)
            .await;
        runtime
            .record_view_image_candidate_protocol_failure(&prepared.route_selection)
            .await;
        let error = observe(&runtime, &home, Some("ollama/vision-a"))
            .await
            .unwrap_err();
        let error = ToolError::from_anyhow(&error);
        assert_eq!(error.details.unwrap()["reason"], "selected_route_cooling");
        assert_eq!(server.count(), 1);
        assert!(observe(&runtime, &home, None).await.unwrap().cache_hit);
        runtime
            .inner
            .view_image_candidate_health
            .lock()
            .await
            .explicit_failures
            .clear();
        let automatic = runtime
            .current_view_image_vision_route_selection()
            .await
            .unwrap();
        runtime
            .record_view_image_candidate_protocol_failure(&automatic)
            .await;
        runtime
            .record_view_image_candidate_protocol_failure(&automatic)
            .await;
        let error = observe(&runtime, &home, Some("ollama/vision-a"))
            .await
            .unwrap_err();
        assert_eq!(
            ToolError::from_anyhow(&error).details.unwrap()["reason"],
            "selected_route_cooling",
            "an explicit request cannot bypass an existing automatic-route cooldown",
        );
        assert_eq!(server.count(), 1);
    }
}

#[cfg(test)]
mod view_image_candidate_health_tests {
    use super::*;

    use crate::config::ResolvedViewImageVisionSelection;
    use crate::types::{ViewImageSelectedMode, ViewImageVisionCandidate, ViewImageVisionSelection};

    fn candidate(
        provider: &str,
        model: &str,
        model_ref: &str,
        reason: &str,
    ) -> ViewImageVisionCandidate {
        ViewImageVisionCandidate {
            provider: provider.to_string(),
            model: model.to_string(),
            model_ref: model_ref.to_string(),
            image_input: true,
            reason: reason.to_string(),
        }
    }

    fn configured_selection(
        candidates: Vec<ViewImageVisionCandidate>,
    ) -> ResolvedViewImageVisionSelection {
        let selection = ViewImageVisionSelection {
            selected_mode: ViewImageSelectedMode::NativeImageWithObservation,
            requested_model: None,
            resolved_model: Some("openai@default/vision-a".to_string()),
            selection_source: Some("automatic".to_string()),
            contract_version: Some(crate::config::VIEW_IMAGE_CONTRACT_VERSION.to_string()),
            vision_provider: Some("openai".to_string()),
            vision_model: Some("vision-a".to_string()),
            selection_reason: "current_primary_model_supports_image_input".to_string(),
            primary_provider: Some("openai".to_string()),
            primary_model: Some("vision-a".to_string()),
            candidates,
        };
        ResolvedViewImageVisionSelection {
            selected_route: Some(
                ModelRouteRef::parse_compatible("openai@default/vision-a").unwrap(),
            ),
            primary_route: ModelRouteRef::parse_compatible("openai@default/vision-a").unwrap(),
            selection,
        }
    }

    fn two_candidates() -> ResolvedViewImageVisionSelection {
        configured_selection(vec![
            candidate(
                "openai",
                "vision-a",
                "openai@default/vision-a",
                VIEW_IMAGE_SUPPORTED_CANDIDATE_REASON,
            ),
            candidate(
                "anthropic",
                "vision-b",
                "anthropic@default/vision-b",
                VIEW_IMAGE_SUPPORTED_CANDIDATE_REASON,
            ),
        ])
    }

    #[test]
    fn explicit_view_image_health_does_not_reset_or_advance_automatic_candidates() {
        let now = Instant::now();
        let mut health = ViewImageCandidateHealth::default();
        let automatic = two_candidates();
        health.record_protocol_failure(&automatic, now);
        let mut explicit = automatic.clone();
        explicit.selection.selection_source = Some("explicit_call".to_string());
        explicit.selected_route =
            Some(ModelRouteRef::parse_compatible("anthropic@default/vision-b").unwrap());
        explicit.selection.candidates.truncate(1);
        health.record_protocol_failure(&explicit, now);
        health.record_protocol_failure(&explicit, now);
        assert_eq!(health.candidate_refs.len(), 2);
        assert_eq!(
            health.failures[automatic.selected_route.as_ref().unwrap()]
                .consecutive_protocol_failures,
            1,
        );
        assert_eq!(
            health
                .select_candidate(automatic.clone(), now)
                .selected_route,
            automatic.selected_route,
        );
        health.record_success(&explicit);
        assert!(health.explicit_failures.is_empty());
        assert_eq!(
            health.failures[automatic.selected_route.as_ref().unwrap()]
                .consecutive_protocol_failures,
            1,
        );
    }

    #[test]
    fn repeated_validation_failures_cool_candidate_and_successful_fallback_decays_it() {
        let now = Instant::now();
        let mut health = ViewImageCandidateHealth::default();
        let primary = two_candidates();

        assert_eq!(
            health
                .record_protocol_failure(&primary, now)
                .unwrap()
                .consecutive_protocol_failures,
            1
        );
        assert_eq!(
            health
                .select_candidate(primary.clone(), now)
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string)
                .as_deref(),
            Some("openai@default/vision-a")
        );

        let opened = health.record_protocol_failure(&primary, now).unwrap();
        assert!(opened.circuit_open);
        assert_eq!(opened.cooldown_seconds, 5 * 60);
        let fallback = health.select_candidate(primary.clone(), now);
        assert_eq!(
            fallback
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string),
            Some("anthropic@default/vision-b".to_string())
        );
        assert_eq!(
            fallback.selection.selected_mode,
            ViewImageSelectedMode::VisionAdapter
        );
        assert_eq!(
            fallback.selection.selection_reason,
            VIEW_IMAGE_CANDIDATE_COOLDOWN_FALLBACK_REASON
        );
        assert!(!should_refresh_view_image_discovery(&fallback));

        health.record_success(&fallback);
        health.record_success(&fallback);
        assert_eq!(
            health
                .failures
                .get(&ModelRouteRef::parse_compatible("openai@default/vision-a").unwrap())
                .unwrap()
                .consecutive_protocol_failures,
            0
        );
        assert_eq!(
            health
                .select_candidate(primary.clone(), now + Duration::from_secs(299))
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string),
            Some("anthropic@default/vision-b".to_string())
        );
        let recovered_candidate =
            health.select_candidate(primary.clone(), now + Duration::from_secs(301));
        assert_eq!(
            recovered_candidate
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string),
            Some("openai@default/vision-a".to_string())
        );
        health.record_success(&recovered_candidate);
        let next_failure = health
            .record_protocol_failure(&recovered_candidate, now + Duration::from_secs(302))
            .unwrap();
        assert_eq!(next_failure.consecutive_protocol_failures, 1);
        assert!(!next_failure.circuit_open);
    }

    #[test]
    fn candidate_list_changes_reset_health_without_admitting_unsupported_routes() {
        let now = Instant::now();
        let mut health = ViewImageCandidateHealth::default();
        let initial = two_candidates();
        health.record_protocol_failure(&initial, now);
        health.record_protocol_failure(&initial, now);
        assert_eq!(
            health
                .select_candidate(initial.clone(), now)
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string),
            Some("anthropic@default/vision-b".to_string())
        );

        let changed = configured_selection(vec![
            candidate(
                "openai",
                "vision-a",
                "openai@default/vision-a",
                VIEW_IMAGE_SUPPORTED_CANDIDATE_REASON,
            ),
            candidate(
                "custom",
                "vision-c",
                "custom@default/vision-c",
                "provider_transport_unsupported_for_view_image_observation",
            ),
        ]);
        assert_eq!(
            health
                .select_candidate(changed.clone(), now)
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string)
                .as_deref(),
            Some("openai@default/vision-a")
        );
        health.record_protocol_failure(&changed, now);
        health.record_protocol_failure(&changed, now);
        let unavailable = health.select_candidate(changed, now);
        assert_eq!(
            unavailable.selection.selected_mode,
            ViewImageSelectedMode::Unavailable
        );
        assert_eq!(unavailable.selected_route, None);
        assert_eq!(
            unavailable.selection.selection_reason,
            VIEW_IMAGE_ALL_CANDIDATES_COOLING_REASON
        );
        assert!(!should_refresh_view_image_discovery(&unavailable));

        let mut no_candidates = configured_selection(Vec::new());
        no_candidates.selection.selected_mode = ViewImageSelectedMode::Unavailable;
        no_candidates.selection.vision_provider = None;
        no_candidates.selection.vision_model = None;
        no_candidates.selection.selection_reason =
            "no_configured_view_image_candidates".to_string();
        no_candidates.selected_route = None;
        assert!(should_refresh_view_image_discovery(&no_candidates));
    }

    #[tokio::test]
    async fn fresh_runtime_state_does_not_inherit_candidate_cooldowns() {
        let selection = two_candidates();
        let runtime_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();
        let previous_runtime = RuntimeHandle::new(
            "default",
            runtime_dir.path().to_path_buf(),
            workspace_dir.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        previous_runtime
            .record_view_image_candidate_protocol_failure(&selection)
            .await;
        previous_runtime
            .record_view_image_candidate_protocol_failure(&selection)
            .await;
        assert_eq!(
            previous_runtime
                .select_healthy_view_image_candidate(selection.clone())
                .await
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string)
                .as_deref(),
            Some("anthropic@default/vision-b")
        );

        drop(previous_runtime);
        let restarted_runtime = RuntimeHandle::new(
            "default",
            runtime_dir.path().to_path_buf(),
            workspace_dir.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        assert_eq!(
            restarted_runtime
                .select_healthy_view_image_candidate(selection)
                .await
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string)
                .as_deref(),
            Some("openai@default/vision-a")
        );
    }

    #[tokio::test]
    async fn concurrent_protocol_failures_update_candidate_health_atomically() {
        let runtime_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            runtime_dir.path().to_path_buf(),
            workspace_dir.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        let selection = two_candidates();

        let (first, second) = tokio::join!(
            runtime.record_view_image_candidate_protocol_failure(&selection),
            runtime.record_view_image_candidate_protocol_failure(&selection)
        );
        let mut counts = [
            first.unwrap().consecutive_protocol_failures,
            second.unwrap().consecutive_protocol_failures,
        ];
        counts.sort_unstable();
        assert_eq!(counts, [1, 2]);
        assert_eq!(
            runtime
                .select_healthy_view_image_candidate(selection)
                .await
                .selected_route
                .as_ref()
                .map(ModelRouteRef::as_string)
                .as_deref(),
            Some("anthropic@default/vision-b")
        );
    }
}
