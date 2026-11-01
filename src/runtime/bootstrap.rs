#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
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
        SkillActivationSource, SkillActivationState, TaskRecord, TaskStatus,
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
    scheduler, scheduler_executor, workspace, InitialWorkspaceBinding, RuntimeAgent, RuntimeHandle,
    RuntimeInner,
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
    pub resource_generation: u64,
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
            resource_generation: 0,
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
        // Direct runtime construction is test-only today; it must stay hermetic
        // instead of binding the ambient user home. Host construction owns the
        // explicit user-home binding from config.
        let user_home_dir = None;
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
        // Same hermetic rule as `new`: no ambient user-home binding here.
        let user_home_dir = None;
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
        let agent_id = agent_id.into();
        // Validate configuration before claiming an owner or closing a SQL gate.
        // Pure preparation failures must leave a retryable namespace untouched.
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
            resource_generation: 0,
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
        let runtime_db = match runtime_db {
            Some(runtime_db) => runtime_db,
            None => RuntimeDb::open_and_migrate(
                data_dir.join(".holon/state/runtime.sqlite"),
                data_dir.join(".holon/state/runtime.lock"),
            )?,
        };
        let storage_setup = prepare_runtime_storage_setup(
            agent_id.clone(),
            data_dir,
            initial_workspace,
            Some(runtime_db.clone()),
        )?;
        let input_evidence_budget = match host_bridge.as_ref() {
            Some(bridge) => bridge.input_evidence_budget()?,
            None => super::input_evidence_budget::InputEvidenceBudgetLedger::new(
                provider_reconfig
                    .as_ref()
                    .map(|reconfig| reconfig.config.stored_config.runtime.state_memory.resolve())
                    .transpose()?
                    .unwrap_or_default(),
            )?,
        };
        let identity_incarnation = runtime_db
            .agent_identities()
            .latest_for_mutation(&agent_id)?
            .map_or(1, |identity| identity.incarnation);
        let (mutation_owner, mut bootstrap_reservation) = match host_bridge.as_ref() {
            Some(bridge) => {
                bridge.claim_reserved_mutation_owner(&agent_id, identity_incarnation)?
            }
            None => {
                let owner = super::mutation_owner::MutationOwner::standalone(
                    &agent_id,
                    identity_incarnation,
                );
                let reservation = owner.try_reserve()?;
                (owner, reservation)
            }
        };
        super::control_recovery::resolve_unloaded_control_recovery(
            &runtime_db,
            std::slice::from_mut(&mut bootstrap_reservation),
            context_config.recent_episode_candidates,
            &input_evidence_budget,
        )?;
        super::episode_recovery::resolve_episode_recovery(
            &runtime_db,
            std::slice::from_mut(&mut bootstrap_reservation),
            context_config.recent_episode_candidates,
        )?;
        mutation_owner.validate_bootstrap()?;
        if bootstrap_reservation.recovery()?.is_some() {
            let known = super::mutation_owner::MutationReservation::validate_group_known(
                std::slice::from_ref(&bootstrap_reservation),
            )?;
            super::mutation_owner::MutationReservation::reconcile_all(
                std::slice::from_mut(&mut bootstrap_reservation),
                known,
            )?;
        }
        let mut provider = provider;
        // Private construction owns its namespace before normalization writes.
        // The helper records the exact posture ACK before derived preparation.
        let recovered_at = clock.now();
        let PreparedRuntimeStorage {
            storage,
            runtime_db,
            state,
            queue,
            active_tasks,
            active_timers,
        } = prepare_runtime_storage_owned(
            storage_setup,
            Some(&mut bootstrap_reservation),
            &context_config,
            Some(&config_snapshot),
            &input_evidence_budget,
            recovered_at,
        )?;
        let queue_budget_view = input_evidence_budget.register_queue_view(
            &super::resident_input_evidence::InputEvidenceScope {
                agent_id: state.id.clone(),
                identity_incarnation,
            },
            super::input_evidence_budget::InputMemoryCategory::Current,
            queue.shared_messages(),
        )?;
        bootstrap_reservation.publish(Arc::new(state.clone()))?;
        drop(bootstrap_reservation);
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

        let runtime = Self {
            inner: Arc::new(RuntimeInner {
                identity_incarnation,
                mutation_owner: mutation_owner.clone(),
                resources: super::resource_refresh::RuntimeResources::new(
                    &input_evidence_budget,
                    super::resident_input_evidence::InputEvidenceScope {
                        agent_id: agent_id.clone(),
                        identity_incarnation,
                    },
                    host_bridge.as_ref(),
                )?,
                input_evidence_budget,
                standalone_related_mutation_owners:
                    super::mutation_owner::MutationOwnerRegistry::default(),
                agent: Mutex::new(RuntimeAgent {
                    #[cfg(test)]
                    mutation_owner,
                    last_persisted_state: state.clone(),
                    state,
                    queue,
                    current_run_abort: None,
                    _queue_budget_view: Some(queue_budget_view),
                }),
                #[cfg(test)]
                installation_barrier: Mutex::new(()),
                #[cfg(test)]
                run_loop_test_gate: RwLock::new(()),
                object_query_cache: Arc::new(crate::object_query_cache::ObjectQueryCache::new(256)),
                notify: Notify::new(),
                operator_interjection_notify: Notify::new(),
                operator_interjection_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                provider_cooldown_wait: StdMutex::new(None),
                storage,
                runtime_db,
                clock,
                base_provider,
                provider: RwLock::new(provider),
                turn_fallback_model: RwLock::new(None),
                turn_provider_admission: StdMutex::new(None),
                config_snapshot: ArcSwap::from(config_snapshot),
                context_config: RwLock::new(resolved_context_config),
                #[cfg(test)]
                context_capture_checkpoint:
                    super::context_capture::ContextCaptureCheckpoint::default(),
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
                #[cfg(test)]
                last_child_deletion_rejections_for_test: StdMutex::new(HashMap::new()),
                task_stop_effects: super::task_stop_effects::TaskStopEffectRegistry::default(),
                owned_task_handles: StdMutex::new(Vec::new()),
                owned_task_drain: Mutex::new(Vec::new()),
                control_cancellations:
                    super::control_cancellation::ControlCancellationRegistry::default(),
                mutation_tasks: StdMutex::new(super::state_mutation::OwnedMutationTasks::default()),
                task_queries: super::task_queries::TaskQueryRegistry::default(),
                input_queries: super::input_query_roles::InputQueryRegistry::default(),
                current_queries: super::current_queries::CurrentQueryRegistry::default(),
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
                terminal_transition_faults: StdMutex::new(std::collections::VecDeque::new()),
                #[cfg(test)]
                claim_transition_faults: StdMutex::new(std::collections::VecDeque::new()),
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
                claim_snapshot_before_commit: StdMutex::new(None),
                #[cfg(test)]
                terminal_tool_interjection_checkpoint:
                    super::TerminalToolInterjectionCheckpoint::default(),
                #[cfg(test)]
                queued_message_projection_checkpoint: StdMutex::new(None),
                #[cfg(test)]
                provider_pause_commit_checkpoint: StdMutex::new(None),
                #[cfg(test)]
                transition_notifications_checkpoint: StdMutex::new(None),
                transition_warnings: StdMutex::new(Vec::new()),
            }),
        };
        runtime.start_resource_worker()?;
        Ok(runtime)
    }

    pub(crate) async fn current_provider(&self) -> Arc<dyn AgentProvider> {
        self.inner.provider.read().await.clone()
    }

    pub(crate) fn model_state_for(&self, state: &AgentState) -> crate::types::AgentModelState {
        self.model_state_for_turn(state, None)
    }

    pub(crate) fn try_model_state_for(
        &self,
        state: &AgentState,
    ) -> Result<crate::types::AgentModelState> {
        let snap = self.inner.config_snapshot.load();
        super::try_agent_model_state_for_catalog(
            &snap.model_catalog,
            &snap.base_context_config,
            state,
        )
    }

    pub(crate) fn model_state_for_turn(
        &self,
        state: &AgentState,
        fallback_model: Option<&ModelRouteRef>,
    ) -> crate::types::AgentModelState {
        let snap = self.inner.config_snapshot.load();
        let mut model_state = super::agent_model_state_for_catalog(
            &snap.model_catalog,
            &snap.base_context_config,
            state,
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
        let mut replacement = ConfigSnapshot::from_config(&config)?;
        let should_reconfigure_provider = replacement.provider_reconfig.is_some();
        let decision_hook = replacement
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
        // Config assignment and installation share the short Host fence.
        self.inner.resources.gate.publish_config(|| {
            replacement.resource_generation = self
                .inner
                .config_snapshot
                .load()
                .resource_generation
                .checked_add(1)
                .context("context resource config generation exhausted")?;
            self.inner.config_snapshot.store(Arc::new(replacement));
            self.inner.resources.registry.invalidate(
                super::resource_observation::ResourceInvalidation::ConfigurationChange,
            )?;
            Ok(())
        })?;
        self.inner.resources.notify();
        // Recompute eligibility without clearing shared provider incidents.
        self.inner
            .runtime_db
            .clear_agent_provider_queue_parking(&self.inner.default_agent_id)?;
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
                data_base64: BASE64_STANDARD.encode(bytes).into(),
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

        let state = self.capture_committed_state().await?;
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
        let mut request = ProviderGenerateImageRequest {
            material: request.material,
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
        if !request.material.is_scoped() {
            request.material =
                crate::provider::ProviderRequestMaterial::scoped(self.context_resource_material()?);
        }
        anyhow::ensure!(
            request.material.matches_runtime(
                &self.inner.input_evidence_budget,
                self.inner.mutation_owner.agent_id(),
                self.inner.identity_incarnation,
            ),
            "image generation material differs from Runtime identity"
        );
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

/// The original same-Tx boot observation retains selected Arcs, never a root.
pub(super) struct BootstrapTaskRecovery {
    selected: super::resident_task::CapturedTasks,
    record_limit: usize,
    byte_limit: usize,
}
pub(super) struct BootstrapTaskRecoveryRetention {
    _selected: super::resident_task::CapturedTasks,
    _owned: Arc<super::input_evidence_budget::BudgetViewGuard>,
}
impl BootstrapTaskRecovery {
    /// Old transient recovery APIs still consume owned records. Charge before
    /// cloning, and retain both that allocation and its frozen source until ACK.
    pub(super) fn into_records(
        self,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<(Vec<TaskRecord>, BootstrapTaskRecoveryRetention)> {
        use super::input_evidence_budget::{
            serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
        };
        use super::resident_task::{CanonicalTaskFact, TaskReadError};
        let count = super::task_reads::active_task_count(&self.selected)?;
        let mut bytes = 256u64;
        let mut present = 0usize;
        for row in &self.selected.records {
            let CanonicalTaskFact::Task(task) = &row.fact else {
                continue;
            };
            if !self.selected.coverage.active_task_ids.contains(&task.id) {
                continue;
            }
            row.validate(&self.selected.scope)?;
            anyhow::ensure!(
                matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                ),
                "bootstrap active Task body differs"
            );
            present = present
                .checked_add(1)
                .context("bootstrap Task count overflow")?;
            bytes = bytes
                .checked_add(serialized_bytes(task)?)
                .and_then(|n| n.checked_add(std::mem::size_of::<TaskRecord>() as u64))
                .context("bootstrap Task material overflow")?;
        }
        anyhow::ensure!(
            present == count,
            "bootstrap active Task snapshot missing body"
        );
        let bytes = bytes
            .checked_mul(8)
            .context("bootstrap Task copies overflow")?;
        let records = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(8))
            .context("bootstrap Task copies overflow")?;
        anyhow::ensure!(
            bytes <= u64::try_from(self.byte_limit)? && records <= self.record_limit,
            TaskReadError::ResourceExhausted("bootstrap transient Task recovery")
        );
        let guard = ledger.register_owned_bytes(
            &self.selected.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let mut tasks = self
            .selected
            .records
            .iter()
            .filter_map(|row| {
                let CanonicalTaskFact::Task(task) = &row.fact else {
                    return None;
                };
                self.selected
                    .coverage
                    .active_task_ids
                    .contains(&task.id)
                    .then(|| task.clone())
            })
            .collect::<Vec<_>>();
        // Preserve ContextDurableRead::active_tasks normalized SQL order.
        tasks.sort_by(|a, b| {
            b.updated_at
                .timestamp_millis()
                .cmp(&a.updated_at.timestamp_millis())
                .then_with(|| {
                    b.created_at
                        .timestamp_millis()
                        .cmp(&a.created_at.timestamp_millis())
                })
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok((
            tasks,
            BootstrapTaskRecoveryRetention {
                _selected: self.selected,
                _owned: guard,
            },
        ))
    }
}

struct PreparedRuntimeStorage {
    storage: AppStorage,
    runtime_db: RuntimeDb,
    state: AgentState,
    queue: RuntimeQueue,
    active_tasks: BootstrapTaskRecovery,
    active_timers: super::resident_scheduling::CapturedScheduling,
}

struct RuntimeStorageSetup {
    storage: AppStorage,
    runtime_db: RuntimeDb,
    agent_id: String,
    initial_workspace_entry: Option<crate::types::WorkspaceEntry>,
}

fn prepare_runtime_storage(
    agent_id: impl Into<String>,
    data_dir: PathBuf,
    initial_workspace: impl Into<InitialWorkspaceBinding>,
    runtime_db: Option<RuntimeDb>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<PreparedRuntimeStorage> {
    let setup = prepare_runtime_storage_setup(agent_id, data_dir, initial_workspace, runtime_db)?;
    let budget = super::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    prepare_runtime_storage_owned(setup, None, &ContextConfig::default(), None, &budget, now)
}

fn prepare_runtime_storage_setup(
    agent_id: impl Into<String>,
    data_dir: PathBuf,
    initial_workspace: impl Into<InitialWorkspaceBinding>,
    runtime_db: Option<RuntimeDb>,
) -> Result<RuntimeStorageSetup> {
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

    Ok(RuntimeStorageSetup {
        storage,
        runtime_db,
        agent_id,
        initial_workspace_entry,
    })
}

fn finish_bootstrap_metadata_phase(
    reservation: &mut super::mutation_owner::MutationReservation,
    facts: crate::runtime_db::context_inputs::ContextScopeFacts,
) -> Result<()> {
    finish_bootstrap_metadata_ack(
        reservation,
        Arc::new(super::mutation_owner::IdentityMutationFacts {
            identity: facts.identity.map(Arc::new),
            state: facts.state,
        }),
    )
}

fn finish_bootstrap_metadata_ack(
    reservation: &mut super::mutation_owner::MutationReservation,
    facts: Arc<super::mutation_owner::IdentityMutationFacts>,
) -> Result<()> {
    super::mutation_owner::MutationReservation::committed_all(
        std::slice::from_mut(reservation),
        vec![super::mutation_owner::KnownCommit::Metadata(facts)],
    )?;
    let known = super::mutation_owner::MutationReservation::validate_group_known(
        std::slice::from_ref(reservation),
    )?;
    super::mutation_owner::MutationReservation::reconcile_all(
        std::slice::from_mut(reservation),
        known,
    )?;
    Ok(())
}

/// Bootstrap has no loaded Runtime yet. Each Audit keeps its own receipt
/// under the existing reservation; it does not also acknowledge a State write.
pub(crate) fn append_unloaded_audit_event(
    storage: &AppStorage,
    db: &RuntimeDb,
    scope: &super::resident_input_evidence::InputEvidenceScope,
    prior: &Arc<super::mutation_owner::IdentityMutationFacts>,
    budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    reservation: &mut super::mutation_owner::MutationReservation,
    event: AuditEvent,
) -> Result<()> {
    validate_bootstrap_audit_scope(scope, prior, Some(reservation))?;
    reservation.begin(crate::ids::audit_event_id())?;
    append_bootstrap_audit_event(storage, db, scope, prior, budget, Some(reservation), event)?;
    finish_bootstrap_metadata_ack(reservation, prior.clone())
}

fn validate_bootstrap_audit_scope(
    scope: &super::resident_input_evidence::InputEvidenceScope,
    prior: &Arc<super::mutation_owner::IdentityMutationFacts>,
    reservation: Option<&super::mutation_owner::MutationReservation>,
) -> Result<()> {
    let identity = prior
        .identity
        .as_ref()
        .context("bootstrap Audit prior identity missing")?;
    anyhow::ensure!(
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && identity.status == crate::types::AgentRegistryStatus::Active,
        "bootstrap Audit prior identity differs"
    );
    anyhow::ensure!(
        prior
            .state
            .as_ref()
            .is_none_or(|state| state.id == scope.agent_id),
        "bootstrap Audit prior State differs"
    );
    if let Some(reservation) = reservation.as_ref() {
        anyhow::ensure!(
            reservation.agent_id() == scope.agent_id
                && reservation.identity_incarnation() == scope.identity_incarnation,
            "bootstrap Audit owner differs"
        );
    }
    Ok(())
}

fn append_bootstrap_audit_event(
    storage: &AppStorage,
    db: &RuntimeDb,
    scope: &super::resident_input_evidence::InputEvidenceScope,
    prior: &Arc<super::mutation_owner::IdentityMutationFacts>,
    budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    mut reservation: Option<&mut super::mutation_owner::MutationReservation>,
    event: AuditEvent,
) -> Result<()> {
    use crate::runtime_db::{
        input_mutation_receipts::OriginalInputMutationAttempt,
        transitions::{InputMutationExecution, PreparedInputTransition},
    };
    validate_bootstrap_audit_scope(scope, prior, reservation.as_deref())?;
    storage.event_log().ensure_writable()?;
    let prepared = PreparedInputTransition::audit(
        scope.clone(),
        event.id.clone(),
        chrono::Utc::now(),
        event,
        None,
    )?;
    let limits = budget.limits();
    let record_limit = usize::try_from(limits.record_limit)?;
    let byte_limit = usize::try_from(
        limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("bootstrap Audit byte limit overflow")?,
    )?;
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: super::resident_work::WorkCaptureRequest::current(),
        record_limit,
        byte_limit,
    };
    let tasks = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: super::resident_task::TaskCaptureRequest::current(),
        record_limit,
        byte_limit,
    };
    let witness = super::mutation_witness::MutationWitness::input_mutation(
        prepared.descriptor().command_nonce().to_owned(),
        OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
        Vec::new(),
    )?;
    if let Some(reservation) = reservation.as_deref_mut() {
        finish_bootstrap_metadata_ack(reservation, prior.clone())?;
        super::mutation_owner::MutationReservation::begin_all_with_witness(
            std::slice::from_mut(reservation),
            prepared.descriptor().command_nonce().to_owned(),
            witness.clone(),
        )?;
    }
    let result = db
        .transitions_in_scope(scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &prepared, &work, budget, &tasks,
        );
    witness.finish_original_attempt();
    let execution = match result {
        Ok(execution) => execution,
        Err(error) => {
            if crate::runtime_db::persistence_certainty(&error)
                == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
            {
                if let Some(reservation) = reservation.as_deref_mut() {
                    super::mutation_owner::MutationReservation::definitely_not_committed_all(
                        std::slice::from_mut(reservation),
                    )?;
                }
            }
            return Err(error);
        }
    };
    let receipt = match &execution {
        InputMutationExecution::Committed { receipt, .. }
        | InputMutationExecution::Replayed { receipt, .. } => receipt,
    };
    witness.mark_original_input_receipt_pending(receipt, false)?;
    if let Some(reservation) = reservation.as_deref_mut() {
        // Audit cannot change the prior durable identity/State. Its original
        // body proof is durable; complete bootstrap domains are prepared later.
        finish_bootstrap_metadata_ack(reservation, prior.clone())?;
        reservation.begin(crate::ids::audit_event_id())?;
    }
    if let InputMutationExecution::Committed { value, .. } = execution {
        storage
            .event_log()
            .publish_transition_events(&value.effects);
    }
    Ok(())
}

fn prepare_runtime_storage_owned(
    setup: RuntimeStorageSetup,
    mut reservation: Option<&mut super::mutation_owner::MutationReservation>,
    context_config: &ContextConfig,
    config_snapshot: Option<&ConfigSnapshot>,
    input_budget: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    recovered_at: chrono::DateTime<chrono::Utc>,
) -> Result<PreparedRuntimeStorage> {
    let RuntimeStorageSetup {
        storage,
        runtime_db,
        agent_id,
        initial_workspace_entry,
    } = setup;
    AppStorage::validate_sqlite_layout(storage.data_dir(), Some(&agent_id), &runtime_db)?;
    // Initialization and recovery normalization retain their existing transactions.
    // An error before the posture ACK cannot prove their combined outcome.
    if let Some(reservation) = reservation.as_deref_mut() {
        reservation.begin(crate::ids::audit_event_id())?;
    }
    runtime_db.initialize_sqlite_storage_domains()?;
    if let Some(workspace) = initial_workspace_entry.as_ref() {
        let known = runtime_db
            .workspace_entries()
            .latest_all_for_preparation()?;
        if !known
            .iter()
            .any(|entry| entry.workspace_id == workspace.workspace_id)
        {
            runtime_db.workspace_entries().upsert(workspace)?;
        }
    }

    // Standalone construction materializes an explicit durable namespace while
    // its reservation is held; missing identity is never loader incarnation proof.
    if runtime_db
        .agent_identities()
        .latest_for_mutation(&agent_id)?
        .is_none()
    {
        runtime_db
            .agent_identities()
            .create_legacy_materialized_with_relations(
                &crate::types::AgentIdentityRecord::new(
                    agent_id.clone(),
                    crate::types::AgentKind::Default,
                    crate::types::AgentVisibility::Public,
                    crate::types::AgentOwnership::SelfOwned,
                    crate::types::AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
                &crate::types::AgentCanonicalRecordSet::default(),
            )?;
    }
    let durable_identity = runtime_db
        .agent_identities()
        .latest_for_mutation(&agent_id)?
        .ok_or_else(|| anyhow!("bootstrap has no durable identity"))?;
    anyhow::ensure!(
        durable_identity.status == crate::types::AgentRegistryStatus::Active,
        "bootstrap identity is no longer active"
    );
    if let Some(reservation) = reservation.as_deref() {
        anyhow::ensure!(
            durable_identity.incarnation == reservation.identity_incarnation(),
            "bootstrap imports changed the reserved durable incarnation"
        );
    }

    let input_scope = super::resident_input_evidence::InputEvidenceScope {
        agent_id: agent_id.clone(),
        identity_incarnation: durable_identity.incarnation,
    };
    // Every import/materialization operation above returned its own ACK. This
    // read records their cold preparation handoff, not a legacy atomic receipt.
    let imported = runtime_db.bootstrap_scope_facts(&input_scope)?;
    anyhow::ensure!(
        imported.identity.as_ref() == Some(&durable_identity),
        "bootstrap import identity changed before its metadata handoff"
    );
    if let Some(reservation) = reservation.as_deref_mut() {
        finish_bootstrap_metadata_phase(reservation, imported)?;
        reservation.begin(crate::ids::audit_event_id())?;
    }
    let normalized = match runtime_db.timers().normalize_wakes_for_recovery_in_scope(
        &input_scope,
        input_budget,
        recovered_at,
    ) {
        Ok(normalized) => normalized,
        Err(error) => {
            if crate::runtime_db::persistence_certainty(&error)
                == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
            {
                if let Some(reservation) = reservation.as_deref_mut() {
                    super::mutation_owner::MutationReservation::definitely_not_committed_all(
                        std::slice::from_mut(reservation),
                    )?;
                }
            }
            return Err(error);
        }
    };
    let audit_facts = Arc::new(super::mutation_owner::IdentityMutationFacts {
        identity: normalized.scope_facts.identity.map(Arc::new),
        state: normalized.scope_facts.state,
    });
    if let Some(reservation) = reservation.as_deref_mut() {
        // Queue/wake normalization is its original independent transaction.
        // Save its exact ACK before audit or further bootstrap work can fail.
        finish_bootstrap_metadata_ack(reservation, audit_facts.clone())?;
        reservation.begin(crate::ids::audit_event_id())?;
    }
    // No Message/Episode evidence was rewritten by normalization. Its exact
    // queue removals are reflected by the fresh pending read below.
    anyhow::ensure!(
        normalized.queue_entries.len() == normalized.result.dropped_message_ids.len(),
        "timer normalization omitted an exact queue change"
    );
    let timer_wake_recovery = normalized.result;
    if timer_wake_recovery.retained_wakes > 0
        || timer_wake_recovery.created_wakes > 0
        || timer_wake_recovery.reactivated_wakes > 0
        || timer_wake_recovery.invalidated_wakes > 0
        || !timer_wake_recovery.dropped_message_ids.is_empty()
        || !timer_wake_recovery.settled_wait_conditions.is_empty()
    {
        append_bootstrap_audit_event(
            &storage,
            &runtime_db,
            &input_scope,
            &audit_facts,
            input_budget,
            reservation.as_deref_mut(),
            AuditEvent::legacy(
                "timer_wakes_recovered",
                serde_json::json!({
                    "agent_id": agent_id,
                    "retained_wakes": timer_wake_recovery.retained_wakes,
                    "created_wakes": timer_wake_recovery.created_wakes,
                    "reactivated_wakes": timer_wake_recovery.reactivated_wakes,
                    "invalidated_wakes": timer_wake_recovery.invalidated_wakes,
                    "dropped_message_ids": timer_wake_recovery.dropped_message_ids,
                    "cancelled_wait_condition_ids": timer_wake_recovery.settled_wait_conditions.iter().map(|condition| &condition.id).collect::<Vec<_>>(),
                    "blocked_work_item_ids": timer_wake_recovery.work_items.iter().map(|work| &work.id).collect::<Vec<_>>(),
                    "audit_event_ids": timer_wake_recovery.audit_events.iter().map(|event| &event.id).collect::<Vec<_>>(),
                }),
            ),
        )?;
    }

    // The normalization ACK/material has crossed its independent Known phase.
    drop((normalized.queue_entries, normalized.scheduling_ack));
    let pending = runtime_db.bootstrap_pending_metadata(&input_scope, input_budget)?;
    let queued_messages = pending.count;
    let recovered_agent = pending.facts.into_owned_state()?;
    let expected_agent_state = recovered_agent.clone();
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
        let known = runtime_db
            .workspace_entries()
            .latest_all_for_preparation()?;
        if !known
            .iter()
            .any(|entry| entry.workspace_id == workspace.workspace_id)
        {
            storage.append_workspace_entry(&workspace)?;
        }
        append_bootstrap_audit_event(
            &storage,
            &runtime_db,
            &input_scope,
            &audit_facts,
            input_budget,
            reservation.as_deref_mut(),
            AuditEvent::legacy(
                "agent_home_workspace_bindings_migrated",
                serde_json::json!({
                    "agent_id": agent_id,
                    "workspace_id": workspace.workspace_id,
                    "legacy_workspace_id": crate::types::AGENT_HOME_WORKSPACE_ID,
                }),
            ),
        )?;
    }

    if state
        .worktree_session
        .as_ref()
        .is_some_and(|worktree| !worktree.worktree_path.exists())
    {
        append_bootstrap_audit_event(
            &storage,
            &runtime_db,
            &input_scope,
            &audit_facts,
            input_budget,
            reservation.as_deref_mut(),
            AuditEvent::legacy(
                "recovery_cleared_missing_worktree_session",
                serde_json::json!({
                    "agent_id": agent_id,
                    "worktree_path": state
                        .worktree_session
                        .as_ref()
                        .map(|w| w.worktree_path.display().to_string()),
                    "reason": "worktree_path_does_not_exist"
                }),
            ),
        )?;
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
        append_bootstrap_audit_event(
            &storage,
            &runtime_db,
            &input_scope,
            &audit_facts,
            input_budget,
            reservation.as_deref_mut(),
            AuditEvent::legacy(
                "legacy_pending_fallback_discarded",
                serde_json::json!({
                    "agent_id": agent_id,
                    "fallback_model_ref": legacy_fallback,
                    "reason": "fallback_selection_is_message_bound",
                }),
            ),
        )?;
    }
    state.pending = queued_messages;
    state.total_message_count = runtime_db
        .messages()
        .count_for_preparation(Some(&agent_id))
        .unwrap_or_default();
    scheduler_executor::apply_bootstrap_recovered_projection(
        &mut state,
        scheduler_executor::BootstrapRecoveryFacts { queued_messages },
    );
    if let Some(active) = state.active_workspace_entry.as_ref() {
        let existing = runtime_db
            .execution_root_entries()
            .get_for_preparation(&active.execution_root_id)?;
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
    runtime_db
        .transitions_in_scope(&input_scope)
        .commit_agent_posture(
            &crate::runtime_db::transitions::AgentPostureTransitionCommand {
                agent_id: state.id.clone(),
                expected_incarnation: runtime_db
                    .agent_identities()
                    .latest_for_mutation(&state.id)?
                    .map(|identity| identity.incarnation),
                agent_state: crate::runtime_db::transitions::AgentStateMutation {
                    expected: expected_agent_state.map(Box::new),
                    record: Box::new(state.clone()),
                },
                audit_events: Vec::new(),
                fault: None,
            },
        )?;
    // This receipt proves the canonical posture only. Derived cache
    // failure remains retryable without treating the whole bootstrap as atomic.
    if let Some(reservation) = reservation.as_deref_mut() {
        reservation.committed(Arc::new(state.clone()), None)?;
    }
    runtime_db.validate_expected_storage_domains(RuntimeDb::expected_storage_domains())?;
    storage.enable_audit_event_index(runtime_db.clone(), Some(state.id.clone()))?;

    let recent_episode_candidates = context_config.recent_episode_candidates;
    let (episodes, inputs, work, tasks, scheduling, pending) = runtime_db.transaction(|tx| {
        let current = crate::runtime_db::episode_snapshot::load_episode_memory_snapshot_in(
            tx,
            &crate::runtime_db::episode_snapshot::EpisodeMemoryLoadScope {
                agent_id: agent_id.clone(),
                identity_incarnation: durable_identity.incarnation,
            },
            recent_episode_candidates,
            None,
        )?
        .require_current()?;
        let pending = crate::runtime_db::bootstrap_inputs::pending_queue_inputs_and_charges_tx(
            tx,
            &input_scope,
            input_budget,
        )?;
        anyhow::ensure!(
            current.identity == durable_identity
                && current.state.as_ref() == Some(&state)
                && current.control_revision.is_some()
                && pending.metadata.facts.identity.as_ref() == Some(&current.identity)
                && pending.metadata.facts.state.as_deref() == current.state.as_ref()
                && pending.metadata.facts.control_revision == current.control_revision
                && pending.metadata.count == state.pending,
            "bootstrap domains do not match their canonical State ACK"
        );
        let input_config = config_snapshot
            .filter(|snap| snap.provider_reconfig.is_some())
            .map(|snap| {
                snap.model_catalog.resolved_context_config(
                    &snap.base_context_config,
                    state.model_override.as_ref(),
                )
            })
            .unwrap_or_else(|| context_config.clone());
        let history = config_snapshot
            .map(|snap| {
                let model = snap
                    .model_catalog
                    .effective_model(state.model_override.as_ref());
                super::turn::context_history_preparation(&state.id, &model)
            })
            .unwrap_or_default();
        let limits = input_budget.limits();
        let mut request = super::context_capture::context_input_evidence_request(
            &state,
            None,
            &input_config,
            history,
            Default::default(),
            usize::try_from(limits.record_limit)?,
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .ok_or_else(|| anyhow!("bootstrap input byte limit overflow"))?,
            )?,
        )?;
        // Ordinary loaded closure readers do not expand windows on a miss.
        super::scheduler::include_scheduler_brief_window(&mut request.windows);
        for message in &pending.messages {
            use super::resident_input_evidence::{
                InputEvidenceFamily as Family, InputEvidenceKey as Key,
            };
            request
                .mandatory_roots
                .insert(Key::new(Family::Message, &message.id));
            // Snapshot preparation validates asserted recovery sources on its
            // same read view, including focused canonical absence observations.
        }
        let mut work_request = super::resident_work::WorkCaptureRequest::current();
        for message in &pending.messages {
            work_request
                .required_trigger_message_ids
                .insert(message.id.clone());
            if message.kind == crate::types::MessageKind::TaskResult {
                if let crate::types::MessageOrigin::Task { task_id } = &message.origin {
                    anyhow::ensure!(
                        message.task_id.as_deref() == Some(task_id.as_str()),
                        "bootstrap TaskResult source differs"
                    );
                    work_request.required_task_ids.insert(task_id.clone());
                }
            }
        }
        let tasks = crate::runtime_db::task_recovery::prepare_bootstrap_task_domains_tx(
            tx,
            &input_scope,
            &state,
            &super::resident_task::TaskCaptureRequest::current(),
            &pending.messages,
            request.record_limit,
            request.byte_limit,
            input_budget,
        )?;
        request.mandatory_roots.extend(tasks.input_roots);
        work_request
            .mandatory_roots
            .extend(tasks.work_roots.iter().map(|id| {
                super::resident_work::WorkKey::new(super::resident_work::WorkFamily::Work, id)
            }));
        let tasks = tasks.resident;
        let recovered_tasks = BootstrapTaskRecovery {
            selected: tasks.capture(&super::task_reads::active_task_request())?,
            record_limit: request.record_limit,
            byte_limit: request.byte_limit,
        };
        let work = crate::runtime_db::work_snapshot::prepare_work_domain_snapshot_tx(
            tx,
            &input_scope,
            &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: work_request,
                record_limit: request.record_limit,
                byte_limit: request.byte_limit,
            },
        )?
        .require_current()?;
        anyhow::ensure!(
            work.facts.identity.as_ref() == Some(&current.identity)
                && work.facts.state.as_deref() == current.state.as_ref()
                && work.facts.control_revision == current.control_revision,
            "bootstrap Work domain differs from its canonical State ACK"
        );
        request.mandatory_roots.extend(work.owning_input_roots);
        let work = Arc::new(work.resident.with_budget(input_budget.clone())?);
        let scheduling =
            crate::runtime_db::scheduling_snapshot::prepare_scheduling_domain_snapshot_tx(
                tx,
                &input_scope,
                &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
                    capture: super::resident_scheduling::SchedulingCaptureRequest::current(),
                    record_limit: request.record_limit,
                    byte_limit: request.byte_limit,
                },
                input_budget,
            )?
            .require_current()?;
        anyhow::ensure!(
            scheduling.facts.identity.as_ref() == Some(&current.identity)
                && scheduling.facts.state.as_deref() == current.state.as_ref()
                && scheduling.facts.control_revision == current.control_revision,
            "bootstrap Scheduling domain differs from its canonical State ACK"
        );
        request.mandatory_roots.extend(scheduling.input_roots);
        let scheduling = Arc::new(scheduling.resident);
        let crate::runtime_db::input_evidence_snapshot::InputEvidenceBootstrapSnapshot::Current(
            inputs,
        ) = crate::runtime_db::input_evidence_snapshot::prepare_input_evidence_snapshot_tx(
            tx,
            &input_scope,
            &request,
        )?
        else {
            return Err(anyhow!("bootstrap input evidence scope retired"));
        };
        anyhow::ensure!(
            inputs.facts.identity.as_ref() == Some(&current.identity)
                && inputs.facts.state.as_deref() == current.state.as_ref()
                && inputs.facts.control_revision == current.control_revision,
            "bootstrap Episode/input domains were read from different canonical facts"
        );
        tracing::debug!(agent_id=%agent_id,control_revision=current.control_revision,
            continuity=?current.continuity,"prepared coherent Episode/input bootstrap domains");
        let episodes = Arc::new(super::resident_state::ResidentEpisodes::bootstrap(
            &agent_id,
            durable_identity.incarnation,
            recent_episode_candidates,
            !current.has_more,
            current.episodes,
        )?);
        let inputs = Arc::new(inputs.resident.with_budget(input_budget.clone())?);
        input_budget.check_history_budget()?;
        Ok((
            episodes,
            inputs,
            work,
            (tasks, recovered_tasks),
            scheduling,
            pending,
        ))
    })?;
    let (tasks, recovered_tasks) = tasks;
    let recovered_timers =
        scheduling.capture(&super::resident_scheduling::SchedulingCaptureRequest::current())?;
    let mut queue = RuntimeQueue::default();
    for message in pending.messages {
        queue.push(message);
    }
    if let Some(reservation) = reservation.as_deref_mut() {
        // Preserve the posture ACK immediately after its SQL, even if derived
        // preparation fails. The ready readback is a separate installation fact,
        // not an in-place rewrite of that already acknowledged commit.
        reservation.publish(Arc::new(state.clone()))?;
        reservation.begin(crate::ids::audit_event_id())?;
        reservation.committed_prepared_with_scheduling_domains(
            Arc::new(state.clone()),
            Some(episodes),
            Some(inputs),
            Some(work),
            Some(tasks),
            Some(scheduling),
        )?;
    }

    Ok(PreparedRuntimeStorage {
        storage,
        runtime_db,
        state,
        queue,
        active_tasks: recovered_tasks,
        active_timers: recovered_timers,
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

        async fn new_gemini(label: &str) -> Self {
            let calls = Arc::new(AtomicUsize::new(0));
            let models = Arc::new(StdMutex::new(Vec::new()));
            let observed_calls = calls.clone();
            let observed_models = models.clone();
            let label = label.to_string();
            let app = Router::new().route(
                "/v1/models/{model_action}",
                post(move |axum::extract::Path(model_action): axum::extract::Path<String>,
                           Json(request): Json<Value>| {
                    let calls = observed_calls.clone();
                    let models = observed_models.clone();
                    let label = label.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        models.lock().unwrap().push(
                            model_action.strip_suffix(":generateContent").unwrap().to_string(),
                        );
                        let parts = request["contents"][0]["parts"].as_array().unwrap();
                        assert!(parts.iter().any(|part| {
                            part["inlineData"]["mimeType"] == "image/png"
                                && part["inlineData"]["data"].as_str().is_some_and(|data| !data.is_empty())
                        }));
                        Json(json!({
                            "candidates": [{"content": {"role": "model", "parts": [{
                                "text": json!({
                                    "type": "visual_observation", "schema": "visual_observation.v1",
                                    "summary": label, "uncertainties": [],
                                }).to_string()
                            }]}, "finishReason": "STOP"}],
                            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 10},
                        }))
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
    async fn view_image_native_gemini_observation_and_cache_follow_selected_route() {
        let server = VisionServer::new_gemini("native Gemini sample").await;
        let (home, _host, runtime) = vision_runtime(&server).await;
        update_snapshot(&runtime, |snapshot| {
            let endpoint = &mut snapshot.model_catalog.provider_endpoints[0].runtime_config;
            endpoint.transport = ProviderTransportKind::GeminiGenerateContent;
            endpoint.credential = Some("gemini-test-key".into());
        });
        let first = observe(&runtime, &home, Some("ollama/vision-a"))
            .await
            .unwrap();
        assert_eq!(first.observation.summary, "native Gemini sample");
        assert!(!first.cache_hit);
        let cached = observe(&runtime, &home, Some("ollama/vision-a"))
            .await
            .unwrap();
        assert!(cached.cache_hit);
        assert_eq!(server.count(), 1);
        let automatic = observe(&runtime, &home, None).await.unwrap();
        assert!(automatic.cache_hit);
        let other_route = observe(&runtime, &home, Some("ollama/vision-b"))
            .await
            .unwrap();
        assert!(!other_route.cache_hit);
        assert_eq!(server.count(), 2);
        assert_eq!(*server.models.lock().unwrap(), ["vision-a", "vision-b"]);
        let error = observe(&runtime, &home, Some("ollama/text-only"))
            .await
            .unwrap_err();
        assert_eq!(
            ToolError::from_anyhow(&error).kind,
            "media_capability_mismatch"
        );
        assert_eq!(server.count(), 2);
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
                .transport = ProviderTransportKind::AiEvaluationModel;
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

#[cfg(test)]
mod mutation_bootstrap_tests {
    use super::*;
    use crate::{host::RuntimeHost, provider::StubProvider};

    fn build_for_retry(
        host: &RuntimeHost,
        data_dir: PathBuf,
        workspace: PathBuf,
        agent_id: &str,
    ) -> Result<RuntimeHandle> {
        RuntimeHandle::new_internal(
            agent_id,
            data_dir,
            workspace,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            agent_id.into(),
            ContextConfig::default(),
            ContextConfig::default(),
            None,
            RuntimeModelCatalog::default(),
            Vec::new(),
            crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS,
            crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS,
            crate::web::WebConfig::default(),
            None,
            Some(host.runtime_db().clone()),
            Some(host.bridge()),
            None,
            Arc::new(SystemClock),
        )
    }

    fn retry_host(home: &std::path::Path) -> RuntimeHost {
        std::fs::write(
            home.join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config = AppConfig::load_with_home(Some(home.to_path_buf())).unwrap();
        RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused"))).unwrap()
    }

    #[tokio::test]
    async fn standalone_bootstrap_samples_injected_recovery_clock_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct SamplingClock {
            samples: AtomicUsize,
        }

        impl Clock for SamplingClock {
            fn now(&self) -> chrono::DateTime<chrono::Utc> {
                let sample = self.samples.fetch_add(1, Ordering::SeqCst);
                chrono::DateTime::from_timestamp(1_700_000_000 + sample as i64, 0).unwrap()
            }
        }

        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let clock = Arc::new(SamplingClock {
            samples: AtomicUsize::new(0),
        });
        for expected_samples in 1..=2 {
            let runtime = RuntimeHandle::new_with_clock(
                "sampled-recovery",
                home.path().to_path_buf(),
                workspace.path().to_path_buf(),
                String::new(),
                Arc::new(StubProvider::new("unused")),
                "sampled-recovery".into(),
                ContextConfig::default(),
                clock.clone(),
            )
            .unwrap();
            assert_eq!(clock.samples.load(Ordering::SeqCst), expected_samples);
            drop(runtime);
        }
    }

    #[tokio::test]
    async fn storage_setup_failure_leaves_same_host_owner_retryable() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let host = retry_host(home.path());
        let data_dir = home.path().join("setup-retry");
        std::fs::write(&data_dir, "blocks the agent directory").unwrap();
        let owner = host.bridge().mutation_owner_for("setup-retry", 1).unwrap();
        let error = build_for_retry(
            &host,
            data_dir.clone(),
            workspace.path().to_path_buf(),
            "setup-retry",
        )
        .err()
        .expect("a file cannot be used as the agent directory");
        assert!(error.to_string().contains("failed to create"));
        assert!(owner.capture().unwrap().is_none());
        assert!(host
            .runtime_db()
            .agent_states()
            .latest("setup-retry")
            .unwrap()
            .is_none());
        std::fs::remove_file(&data_dir).unwrap();
        let runtime = build_for_retry(
            &host,
            data_dir,
            workspace.path().to_path_buf(),
            "setup-retry",
        )
        .unwrap();
        assert!(runtime.inner.mutation_owner.capture().unwrap().is_some());
    }

    #[tokio::test]
    async fn cache_decode_failure_retains_exact_posture_ack_for_same_host_retry() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let host = retry_host(home.path());
        let data_dir = home.path().join("cache-retry");
        let build = || {
            build_for_retry(
                &host,
                data_dir.clone(),
                workspace.path().to_path_buf(),
                "cache-retry",
            )
        };
        let runtime = build().unwrap();
        let capability = runtime
            .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
            .await
            .unwrap();
        let descriptor = host
            .runtime_db()
            .external_triggers()
            .latest(&capability.external_trigger_id)
            .unwrap()
            .unwrap();
        let mut before = runtime.agent_state().await.unwrap();
        before.status = crate::types::AgentStatus::AwakeRunning;
        before.current_run_id = Some("unsettled-before-bootstrap".into());
        before.total_input_tokens = 413;
        runtime.storage().write_agent_for_fixture(&before).unwrap();
        drop(runtime);
        let connection = host.runtime_db().connection().unwrap();
        connection
            .execute(
                "UPDATE external_triggers SET payload_json = 'invalid-json' WHERE external_trigger_id = ?1",
                [&descriptor.external_trigger_id],
            )
            .unwrap();
        assert!(build().is_err(), "cache must decode the corrupted trigger");
        let committed = host
            .runtime_db()
            .agent_states()
            .latest("cache-retry")
            .unwrap()
            .unwrap();
        assert_ne!(
            committed, before,
            "canonical posture ACK precedes cache decode"
        );
        assert_eq!(committed.total_input_tokens, 413);
        let owner = host.bridge().mutation_owner_for("cache-retry", 1).unwrap();
        {
            let reservation = owner.reserve().await.unwrap();
            let recovery = reservation.recovery().unwrap().unwrap();
            assert_eq!(recovery.committed_state.as_deref(), Some(&committed));
            assert!(recovery.known.is_some());
        }
        connection
            .execute(
                "UPDATE external_triggers SET payload_json = ?1 WHERE external_trigger_id = ?2",
                rusqlite::params![
                    serde_json::to_string(&descriptor).unwrap(),
                    descriptor.external_trigger_id
                ],
            )
            .unwrap();
        let runtime = build().unwrap();
        assert_eq!(runtime.agent_state().await.unwrap(), committed);
        assert_eq!(
            host.runtime_db()
                .agent_states()
                .latest("cache-retry")
                .unwrap(),
            Some(committed)
        );
    }

    #[tokio::test]
    async fn pending_budget_preflight_keeps_same_host_bootstrap_retryable() {
        use crate::runtime::{
            input_evidence_budget::{
                BudgetResource, InputEvidenceBudgetError, InputEvidenceBudgetLedger,
            },
            resident_input_evidence::InputEvidenceScope,
        };
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let host = retry_host(home.path());
        let data_dir = home.path().join("budget-retry");
        let runtime = build_for_retry(
            &host,
            data_dir.clone(),
            workspace.path().to_path_buf(),
            "budget-retry",
        )
        .unwrap();
        let mut before = runtime.agent_state().await.unwrap();
        let scope = InputEvidenceScope {
            agent_id: before.id.clone(),
            identity_incarnation: runtime.inner.identity_incarnation,
        };
        let message = crate::types::MessageEnvelope::new(
            &scope.agent_id,
            crate::types::MessageKind::OperatorPrompt,
            crate::types::MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            crate::types::AuthorityClass::OperatorInstruction,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: "durably accepted before cold activation".into(),
            },
        );
        let message = host
            .runtime_db()
            .messages()
            .append_with_index_changes_in_scope(&scope, &message, &[])
            .unwrap()
            .record;
        host.runtime_db()
            .transaction(|tx| {
                crate::runtime_db::repositories::upsert_queue_entry_tx(
                    tx,
                    &crate::types::QueueEntryRecord {
                        message_id: message.id.clone(),
                        agent_id: scope.agent_id.clone(),
                        priority: message.priority.clone(),
                        status: crate::types::QueueEntryStatus::Queued,
                        created_at: message.created_at,
                        updated_at: message.created_at,
                    },
                )
            })
            .unwrap();
        before.pending = 1;
        runtime.storage().write_agent_for_fixture(&before).unwrap();
        drop(runtime);
        let (owner, mut reservation) = host
            .bridge()
            .claim_reserved_mutation_owner(&scope.agent_id, scope.identity_incarnation)
            .unwrap();
        let tiny_budget = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits {
            pending_per_agent_bytes: 1,
            ..Default::default()
        })
        .unwrap();
        let setup = prepare_runtime_storage_setup(
            &scope.agent_id,
            data_dir.clone(),
            workspace.path().to_path_buf(),
            Some(host.runtime_db().clone()),
        )
        .unwrap();
        let error = prepare_runtime_storage_owned(
            setup,
            Some(&mut reservation),
            &ContextConfig::default(),
            None,
            &tiny_budget,
            chrono::Utc::now(),
        )
        .err()
        .expect("metadata must reject before normalization decodes the accepted body");
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceBudgetError>(),
                Some(InputEvidenceBudgetError::ResourceExhausted {
                    resource: BudgetResource::PendingAgentBytes,
                    ..
                })
            ),
            "{error:#}"
        );
        assert_eq!(
            crate::runtime_db::persistence_certainty(&error),
            Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
        );
        assert!(reservation.recovery().unwrap().is_none());
        assert!(owner.capture().unwrap().is_some());
        assert_eq!(
            host.runtime_db()
                .agent_states()
                .latest(&scope.agent_id)
                .unwrap(),
            Some(before)
        );
        assert_eq!(tiny_budget.report().unwrap().pending.count, 0);
        drop(reservation);
        let runtime = build_for_retry(
            &host,
            data_dir,
            workspace.path().to_path_buf(),
            &scope.agent_id,
        )
        .unwrap();
        assert_eq!(runtime.agent_state().await.unwrap().pending, 1);
        assert!(runtime.inner.mutation_owner.capture().unwrap().is_some());
    }

    #[tokio::test]
    async fn invalid_pure_configuration_does_not_fence_the_owner_and_can_be_retried() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let mut invalid = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host =
            RuntimeHost::new_with_provider(invalid.clone(), Arc::new(StubProvider::new("unused")))
                .unwrap();
        let bridge = host.bridge();
        let owner = bridge.mutation_owner_for("config-retry", 1).unwrap();
        invalid.stored_config.decision.enabled = Some(true);
        invalid.stored_config.decision.model = None;
        let build = |reconfig| {
            RuntimeHandle::new_internal(
                "config-retry",
                home.path().join("agents/config-retry"),
                workspace.path().to_path_buf(),
                String::new(),
                Arc::new(StubProvider::new("unused")),
                "config-retry".into(),
                ContextConfig::default(),
                ContextConfig::default(),
                None,
                RuntimeModelCatalog::default(),
                Vec::new(),
                crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS,
                crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS,
                crate::web::WebConfig::default(),
                reconfig,
                Some(host.runtime_db().clone()),
                Some(bridge.clone()),
                None,
                Arc::new(SystemClock),
            )
        };
        let error = build(Some(ProviderReconfigurator { config: invalid }))
            .err()
            .expect("invalid decision config must fail");
        assert!(error
            .to_string()
            .contains("decision.enabled requires decision.model"));
        // The preexisting owner epoch remains valid; neither claim nor begin ran.
        assert!(owner.capture().unwrap().is_none());
        let runtime = build(None).unwrap();
        assert!(runtime.inner.mutation_owner.capture().unwrap().is_some());
    }
}

#[cfg(test)]
#[path = "bootstrap_task_recovery_tests.rs"]
mod task_recovery_tests;

#[cfg(test)]
#[path = "bootstrap_audit_tests.rs"]
mod audit_tests;
