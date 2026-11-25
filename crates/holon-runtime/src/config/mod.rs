use std::{collections::HashMap, env, path::PathBuf, time::Duration};

use anyhow::{anyhow, Result};

use crate::{
    authentication::AuthConfig,
    model_catalog::ModelRuntimeOverride,
    model_discovery::{discovery_cache_path, load_discovery_cache_at, ModelDiscoveryCacheFile},
};

mod x_search;

pub use holon_config::*;
pub use x_search::XSearchRuntimeConfig;

pub fn provider_config_views(config: &AppConfig) -> Vec<ProviderConfigView> {
    config
        .providers
        .values()
        .map(|provider| provider_config_view(config, provider))
        .collect()
}

pub fn provider_config_view(
    config: &AppConfig,
    provider: &ProviderRuntimeConfig,
) -> ProviderConfigView {
    ProviderConfigView {
        id: provider.id.as_str().to_string(),
        transport: provider.transport.as_str().to_string(),
        base_url: provider.base_url.clone(),
        auth: ProviderAuthView {
            source: provider.auth.source.as_str().to_string(),
            kind: provider.auth.kind.as_str().to_string(),
            env: provider.auth.env.clone(),
            profile: provider.auth.profile.clone(),
            external: provider.auth.external.clone(),
        },
        credential_configured: provider.has_configured_credential()
            || matches!(provider.auth.source, CredentialSource::None),
        configured_in_config: config.stored_config.providers.contains_key(&provider.id),
        header_names: provider.headers.names(),
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub default_agent_id: String,
    pub http_addr: String,
    pub callback_base_url: String,
    pub user_home_dir: Option<PathBuf>,
    pub home_dir: PathBuf,
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    pub workspace_dir: PathBuf,
    pub context_window_messages: usize,
    pub context_window_briefs: usize,
    pub compaction_trigger_messages: usize,
    pub compaction_keep_recent_messages: usize,
    pub prompt_budget_estimated_tokens: usize,
    pub compaction_trigger_estimated_tokens: usize,
    pub compaction_keep_recent_estimated_tokens: usize,
    pub recent_episode_candidates: usize,
    pub max_relevant_episodes: usize,
    pub control_token: Option<String>,
    pub control_auth_mode: ControlAuthMode,
    pub auth: AuthConfig,
    pub api_cors: ApiCorsConfigFile,
    pub api_projection: ApiProjectionConfigFile,
    pub local_apps: LocalAppsConfigFile,
    pub config_file_path: PathBuf,
    pub stored_config: HolonConfigFile,
    pub web_config: crate::web::WebConfig,
    pub default_model: ModelRouteRef,
    pub fallback_models: Vec<ModelRouteRef>,
    pub vision_model: Option<ModelRouteRef>,
    pub image_generation_model: Option<ModelRouteRef>,
    pub vision_candidate_models: Vec<ModelRouteRef>,
    pub runtime_max_output_tokens: u32,
    pub default_tool_output_tokens: u32,
    pub max_tool_output_tokens: u32,
    pub command_task_output_retention_bytes: u64,
    pub command_task_output_quota_bytes: u64,
    pub command_task_min_free_disk_bytes: u64,
    pub command_task_min_free_disk_percent: u8,
    pub disable_provider_fallback: bool,
    pub tui_alternate_screen: AltScreenMode,
    pub validated_model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    pub validated_unknown_model_fallback: Option<ModelRuntimeOverride>,
    pub model_discovery_cache: ModelDiscoveryCacheFile,
    pub providers: ProviderRegistry,
}

impl ModelCatalogSource for AppConfig {
    fn model_catalog_config(&self) -> ModelCatalogConfig<'_> {
        let config = self;
        ModelCatalogConfig {
            stored_config: &config.stored_config,
            default_model: &config.default_model,
            fallback_models: &config.fallback_models,
            vision_model: config.vision_model.as_ref(),
            image_generation_model: config.image_generation_model.as_ref(),
            vision_candidate_models: &config.vision_candidate_models,
            disable_provider_fallback: config.provider_fallback_disabled(),
            providers: &config.providers,
            model_discovery_cache: &config.model_discovery_cache,
            validated_model_overrides: &config.validated_model_overrides,
            validated_unknown_model_fallback: config.validated_unknown_model_fallback.as_ref(),
            runtime_max_output_tokens: config.runtime_max_output_tokens,
        }
    }
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        Self::load_with_home(None)
    }

    pub fn load_with_home(home_override: Option<PathBuf>) -> Result<Self> {
        Self::load_with_home_and_mode(home_override, ConfigLoadMode::Runtime)
    }

    pub fn load_for_config_inspection() -> Result<Self> {
        Self::load_with_home_for_config_inspection(None)
    }

    pub fn load_with_home_for_config_inspection(home_override: Option<PathBuf>) -> Result<Self> {
        Self::load_with_home_and_mode(home_override, ConfigLoadMode::ConfigInspection)
    }

    pub fn reload_runtime_config(&self) -> Result<Self> {
        let mut reloaded =
            Self::load_with_home_and_mode(Some(self.home_dir.clone()), ConfigLoadMode::Runtime)?;
        reloaded.default_agent_id = self.default_agent_id.clone();
        reloaded.http_addr = self.http_addr.clone();
        reloaded.callback_base_url = self.callback_base_url.clone();
        reloaded.user_home_dir = self.user_home_dir.clone();
        reloaded.home_dir = self.home_dir.clone();
        reloaded.data_dir = self.data_dir.clone();
        reloaded.socket_path = self.socket_path.clone();
        reloaded.workspace_dir = self.workspace_dir.clone();
        reloaded.context_window_messages = self.context_window_messages;
        reloaded.context_window_briefs = self.context_window_briefs;
        reloaded.compaction_trigger_messages = self.compaction_trigger_messages;
        reloaded.compaction_keep_recent_messages = self.compaction_keep_recent_messages;
        reloaded.prompt_budget_estimated_tokens = self.prompt_budget_estimated_tokens;
        reloaded.compaction_trigger_estimated_tokens = self.compaction_trigger_estimated_tokens;
        reloaded.compaction_keep_recent_estimated_tokens =
            self.compaction_keep_recent_estimated_tokens;
        reloaded.stored_config.runtime.context_budget_overrides =
            self.stored_config.runtime.context_budget_overrides;
        // Pool limits stay with the Host and all its existing RuntimeDb clones.
        reloaded.stored_config.runtime.db_readers = self.stored_config.runtime.db_readers.clone();
        reloaded.recent_episode_candidates = self.recent_episode_candidates;
        reloaded.max_relevant_episodes = self.max_relevant_episodes;
        reloaded.control_token = self.control_token.clone();
        reloaded.control_auth_mode = self.control_auth_mode;
        reloaded.config_file_path = self.config_file_path.clone();
        Ok(reloaded)
    }

    /// Returns true when the default model provider has a usable credential,
    /// indicating the agent can actually make model calls.
    ///
    /// Providers with `CredentialSource::None` are ready when they explicitly
    /// declare `CredentialKind::None`; availability is then governed by the
    /// provider transport rather than a stored credential.
    pub fn default_provider_ready(&self) -> bool {
        self.providers
            .values()
            .find(|provider| {
                provider.route_provider == self.default_model.provider
                    && provider.route_endpoint == self.default_model.endpoint
            })
            .map(provider_has_usable_auth)
            .unwrap_or(false)
    }

    /// Returns true when browser-first bootstrap was explicitly requested and
    /// the configured default provider is not ready yet.
    pub fn bootstrap_mode_enabled(&self) -> bool {
        bootstrap_mode_requested() && !self.default_provider_ready()
    }

    fn load_with_home_and_mode(
        home_override: Option<PathBuf>,
        mode: ConfigLoadMode,
    ) -> Result<Self> {
        // `~/.claude/settings.json` is only an optional Claude-compatible env
        // source, so a damaged file degrades instead of failing the load.
        // Report its aggregated diagnostic once per load.
        let claude_settings = load_settings_env();
        if let Some(diagnostic) = claude_settings.diagnostic() {
            tracing::warn!(target: "holon::config", "{diagnostic}");
        }
        let settings_env = claude_settings.into_env();
        let user_home_dir = default_user_home_dir();
        let home_dir = home_override.unwrap_or_else(|| {
            env::var("HOLON_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_holon_home())
        });
        let config_file_path = persisted_config_path(&home_dir);
        let mut stored_config = load_persisted_config_at(&config_file_path)?;
        validate_api_cors_config(&stored_config.api.cors)?;
        validate_api_csrf_config(&stored_config.api.csrf)?;
        validate_api_projection_config(&stored_config.api.projection)?;
        validate_api_prompt_config(&stored_config.api.prompt)?;
        validate_local_apps_config(&stored_config.local_apps)?;
        let credential_store_path = credential_store_path(&home_dir);
        let credential_store =
            if config_uses_credential_profiles(&stored_config) || credential_store_path.exists() {
                load_credential_store_at(&credential_store_path)?
            } else {
                CredentialStoreFile::default()
            };
        let http_addr = env::var("HOLON_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
        let callback_base_url =
            env::var("HOLON_CALLBACK_BASE_URL").unwrap_or_else(|_| format!("http://{http_addr}"));
        let data_dir = home_dir.clone();
        let socket_path = env::var("HOLON_SOCKET_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir.join("run").join("holon.sock"));
        let workspace_dir = env::var("HOLON_WORKSPACE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let default_agent_id =
            env::var("HOLON_AGENT_ID").unwrap_or_else(|_| DEFAULT_LOCAL_AGENT_ID.into());
        let context_window_messages = env::var("HOLON_CONTEXT_WINDOW_MESSAGES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(12);
        let context_window_briefs = env::var("HOLON_CONTEXT_WINDOW_BRIEFS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8);
        let compaction_trigger_messages = env::var("HOLON_COMPACTION_TRIGGER_MESSAGES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(20);
        let compaction_keep_recent_messages = env::var("HOLON_COMPACTION_KEEP_RECENT_MESSAGES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8);
        let budget_overrides = crate::context::ContextBudgetOverrides {
            prompt_budget_estimated_tokens: env::var("HOLON_PROMPT_BUDGET_ESTIMATED_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok()),
            compaction_trigger_estimated_tokens: env::var(
                "HOLON_COMPACTION_TRIGGER_ESTIMATED_TOKENS",
            )
            .ok()
            .and_then(|value| value.parse().ok()),
            compaction_keep_recent_estimated_tokens: env::var(
                "HOLON_COMPACTION_KEEP_RECENT_ESTIMATED_TOKENS",
            )
            .ok()
            .and_then(|value| value.parse().ok()),
        };
        stored_config.runtime.context_budget_overrides = budget_overrides;
        let prompt_budget_estimated_tokens = budget_overrides
            .prompt_budget_estimated_tokens
            .unwrap_or(crate::context::DEFAULT_PROMPT_BUDGET_ESTIMATED_TOKENS);
        let compaction_trigger_estimated_tokens = budget_overrides
            .compaction_trigger_estimated_tokens
            .unwrap_or_else(|| {
                crate::context::default_compaction_trigger_estimated_tokens(
                    prompt_budget_estimated_tokens,
                )
            });
        let compaction_keep_recent_estimated_tokens = budget_overrides
            .compaction_keep_recent_estimated_tokens
            .unwrap_or_else(|| {
                crate::context::default_compaction_keep_recent_estimated_tokens(
                    compaction_trigger_estimated_tokens,
                )
            });
        let recent_episode_candidates = env::var("HOLON_RECENT_EPISODE_CANDIDATES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(12);
        let max_relevant_episodes = env::var("HOLON_MAX_RELEVANT_EPISODES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(3);
        let control_token = env::var("HOLON_CONTROL_TOKEN").ok();
        let control_auth_mode = env::var("HOLON_CONTROL_AUTH_MODE")
            .ok()
            .map(|value| ControlAuthMode::parse(&value))
            .transpose()?
            .unwrap_or(ControlAuthMode::Auto);
        let auth = holon_config::authentication::materialize_auth_config(&stored_config.auth)?;
        let runtime_max_output_tokens = env::var("HOLON_MAX_OUTPUT_TOKENS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .or_else(|| {
                env::var("ANTHROPIC_MAX_OUTPUT_TOKENS")
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok())
            })
            .or(stored_config.runtime.max_output_tokens)
            .filter(|value| *value > 0)
            .unwrap_or(8192);
        let default_tool_output_tokens = env::var("HOLON_DEFAULT_TOOL_OUTPUT_TOKENS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .or(stored_config.runtime.default_tool_output_tokens)
            .filter(|value| *value > 0)
            .unwrap_or(crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32)
            .min(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32);
        let max_tool_output_tokens = env::var("HOLON_MAX_TOOL_OUTPUT_TOKENS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .or(stored_config.runtime.max_tool_output_tokens)
            .filter(|value| *value > 0)
            .unwrap_or(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32)
            .min(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32)
            .max(default_tool_output_tokens);
        let default_command_task_output_policy = crate::types::CommandTaskOutputPolicy::default();
        let command_task_output_retention_bytes =
            env::var("HOLON_COMMAND_TASK_OUTPUT_RETENTION_BYTES")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .or(stored_config.runtime.command_task_output_retention_bytes)
                .filter(|value| *value > 0)
                .unwrap_or(default_command_task_output_policy.retention_bytes)
                .max(4 * 1024);
        let command_task_output_quota_bytes = env::var("HOLON_COMMAND_TASK_OUTPUT_QUOTA_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or(stored_config.runtime.command_task_output_quota_bytes)
            .filter(|value| *value > 0)
            .unwrap_or(default_command_task_output_policy.execution_quota_bytes)
            .max(command_task_output_retention_bytes);
        let command_task_min_free_disk_bytes = env::var("HOLON_COMMAND_TASK_MIN_FREE_DISK_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or(stored_config.runtime.command_task_min_free_disk_bytes)
            .unwrap_or(default_command_task_output_policy.min_free_disk_bytes);
        let command_task_min_free_disk_percent =
            env::var("HOLON_COMMAND_TASK_MIN_FREE_DISK_PERCENT")
                .ok()
                .and_then(|value| value.parse::<u8>().ok())
                .or(stored_config.runtime.command_task_min_free_disk_percent)
                .unwrap_or(default_command_task_output_policy.min_free_disk_percent)
                .min(100);

        let disable_provider_fallback = resolve_disable_provider_fallback(&stored_config)?;
        validate_retired_scheduler_selector(&stored_config)?;
        resolve_runtime_db_retention_policy(&stored_config)?;
        stored_config.runtime.state_memory.resolve()?;
        stored_config.runtime.db_readers.resolve()?;
        let validated_model_overrides = resolve_model_catalog(&stored_config)?;
        resolve_model_route_overrides(&stored_config)?;
        let validated_unknown_model_fallback =
            validate_optional_model_runtime_override(stored_config.model.unknown_fallback.clone())?;
        let model_discovery_cache = load_discovery_cache_at(&discovery_cache_path(&home_dir))
            .unwrap_or_else(|error| {
                tracing::warn!(error = %error, "ignoring invalid model discovery cache");
                ModelDiscoveryCacheFile::default()
            });
        let mut providers =
            resolve_provider_registry(&stored_config, &settings_env, &credential_store)?;
        for provider in providers.values_mut() {
            if provider.auth.source == CredentialSource::AuthProfile {
                provider.credential_store_path = Some(credential_store_path.clone());
            }
        }
        let explicit_default = resolve_default_model(&stored_config)?;
        let explicit_fallbacks = resolve_fallback_models(&stored_config)?;
        let explicit_vision_model = resolve_vision_model(&stored_config)?;
        let explicit_image_generation_model = resolve_image_generation_model(&stored_config)?;
        let (default_model, fallback_models) = match resolve_model_selection_for_load_mode(
            explicit_default,
            explicit_fallbacks,
            &providers,
            &validated_model_overrides,
            mode.skip_authenticated_model_resolution(),
        ) {
            Ok(selection) => selection,
            Err(error) if mode.allow_unresolved_model() => {
                tracing::debug!(
                    error = %error,
                    "using unresolved model for bootstrap or config inspection"
                );
                (
                    ModelRouteRef::new(
                        ProviderId::openai(),
                        ProviderEndpointId::default_endpoint(),
                        "unknown",
                    ),
                    Vec::new(),
                )
            }
            Err(error) => return Err(error),
        };
        let vision_candidate_models =
            authenticated_model_route_candidates(&providers, &validated_model_overrides);
        let tui_alternate_screen = env::var("HOLON_TUI_ALTERNATE_SCREEN")
            .ok()
            .map(|value| AltScreenMode::parse(&value))
            .transpose()?
            .or(stored_config.tui.alternate_screen)
            .unwrap_or(AltScreenMode::Auto);
        let web_config = crate::web::materialize_web_config(&stored_config.web, &credential_store)?;

        let config = Self {
            default_agent_id,
            http_addr,
            callback_base_url,
            user_home_dir,
            home_dir,
            data_dir,
            socket_path,
            workspace_dir,
            context_window_messages,
            context_window_briefs,
            compaction_trigger_messages,
            compaction_keep_recent_messages,
            prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens,
            recent_episode_candidates,
            max_relevant_episodes,
            control_token,
            control_auth_mode,
            auth,
            api_cors: stored_config.api.cors.clone(),
            api_projection: stored_config.api.projection.clone(),
            local_apps: stored_config.local_apps.clone(),
            config_file_path,
            stored_config,
            web_config,
            default_model,
            fallback_models,
            vision_model: explicit_vision_model,
            image_generation_model: explicit_image_generation_model,
            vision_candidate_models,
            runtime_max_output_tokens,
            default_tool_output_tokens,
            max_tool_output_tokens,
            command_task_output_retention_bytes,
            command_task_output_quota_bytes,
            command_task_min_free_disk_bytes,
            command_task_min_free_disk_percent,
            disable_provider_fallback,
            tui_alternate_screen,
            validated_model_overrides,
            validated_unknown_model_fallback,
            model_discovery_cache,
            providers,
        };
        config.validate_model_route_options()?;
        Ok(config)
    }
}

impl AppConfig {
    pub fn run_dir(&self) -> PathBuf {
        self.home_dir.join("run")
    }

    pub fn runtime_db_path(&self) -> PathBuf {
        self.data_dir.join("state").join("runtime.sqlite")
    }

    pub fn diagnostics_db_path(&self) -> PathBuf {
        self.home_dir.join("diagnostics.sqlite")
    }

    pub fn diagnostics_writer_config(
        &self,
    ) -> Result<crate::diagnostics_store::DiagnosticsWriterConfig> {
        resolve_diagnostics_writer_config(&self.stored_config)
    }

    pub fn otlp_exporter_config(&self) -> Result<Option<crate::otlp_exporter::OtlpExporterConfig>> {
        let credential_store = load_credential_store_at(&credential_store_path(&self.home_dir))?;
        resolve_otlp_exporter_config(&self.stored_config, &credential_store)
    }

    pub fn runtime_db_lock_path(&self) -> PathBuf {
        self.data_dir.join("state").join("runtime.lock")
    }

    pub fn runtime_db_maintenance_lock_path(&self) -> PathBuf {
        self.data_dir.join("state").join("runtime-maintenance.lock")
    }

    pub fn runtime_db_retention_policy(
        &self,
    ) -> Result<crate::runtime_db::RuntimeDbRetentionPolicy> {
        resolve_runtime_db_retention_policy(&self.stored_config)
    }

    pub fn agent_root_dir(&self) -> PathBuf {
        self.data_dir.join("agents")
    }

    pub fn log_root_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn control_token_required(&self, transport: ControlTransportKind) -> bool {
        match self.control_auth_mode {
            ControlAuthMode::Disabled => false,
            ControlAuthMode::Required => true,
            ControlAuthMode::Auto => match transport {
                ControlTransportKind::Unix => false,
                ControlTransportKind::Tcp => {
                    !self.tcp_listener_is_local()
                        || self
                            .control_token
                            .as_deref()
                            .is_some_and(|token| !token.trim().is_empty())
                }
            },
        }
    }

    pub fn validate_model_route_options(&self) -> Result<()> {
        validate_model_route_options(&RuntimeModelCatalog::from_config(self))
    }

    pub fn provider_chain(&self) -> Vec<ModelRouteRef> {
        RuntimeModelCatalog::from_config(self).provider_chain(None)
    }

    pub fn provider_chain_with_override(
        &self,
        model_override: Option<&ModelRouteRef>,
    ) -> Vec<ModelRouteRef> {
        RuntimeModelCatalog::from_config(self).provider_chain(model_override)
    }

    pub fn provider_fallback_disabled(&self) -> bool {
        self.disable_provider_fallback
    }

    /// Effective discovery configuration declared for this endpoint runtime.
    ///
    /// Endpoint-level entries win over the provider-level default; `None` keeps
    /// the transport/built-in default behavior.
    pub fn provider_discovery(
        &self,
        provider: &ProviderRuntimeConfig,
    ) -> Option<ProviderDiscoveryConfig> {
        resolved_provider_discovery(&self.stored_config, provider)
    }

    pub fn tcp_listener_is_local(&self) -> bool {
        let trimmed = self.http_addr.trim().to_ascii_lowercase();
        if trimmed == "localhost" || trimmed.starts_with("localhost:") {
            return true;
        }
        trimmed
            .parse::<std::net::SocketAddr>()
            .map(|addr| addr.ip().is_loopback())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigLoadMode {
    Runtime,
    ConfigInspection,
}

impl ConfigLoadMode {
    fn skip_authenticated_model_resolution(self) -> bool {
        matches!(self, Self::ConfigInspection)
    }

    fn allow_unresolved_model(self) -> bool {
        matches!(self, Self::ConfigInspection)
            || matches!(self, Self::Runtime) && bootstrap_mode_requested()
    }
}

fn bootstrap_mode_requested() -> bool {
    env::var("HOLON_BOOTSTRAP").ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn resolve_disable_provider_fallback(stored_config: &HolonConfigFile) -> Result<bool> {
    resolve_disable_provider_fallback_override(
        env::var("HOLON_DISABLE_PROVIDER_FALLBACK").ok().as_deref(),
        stored_config,
    )
}

fn validate_retired_scheduler_selector(stored_config: &HolonConfigFile) -> Result<()> {
    validate_retired_scheduler_selector_values(
        env::var("HOLON_SCHEDULER").ok().as_deref(),
        stored_config.runtime.scheduler.as_deref(),
    )
}

fn validate_retired_scheduler_selector_values(
    env_override: Option<&str>,
    stored_value: Option<&str>,
) -> Result<()> {
    let Some((source, value)) = env_override
        .map(|value| ("HOLON_SCHEDULER", value))
        .or_else(|| stored_value.map(|value| ("runtime.scheduler", value)))
    else {
        return Ok(());
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "canonical" => {
            tracing::warn!(
                source,
                "scheduler selection is obsolete; remove the canonical selector before the next minor release"
            );
            Ok(())
        }
        "legacy" => Err(anyhow!(
            "{source}=legacy is no longer supported; use Holon v0.31.1 with a pre-migration database backup if legacy rollback is required"
        )),
        _ => Err(anyhow!(
            "{source} is obsolete; remove it because canonical scheduling is always enabled"
        )),
    }
}

fn resolve_diagnostics_writer_config(
    stored_config: &HolonConfigFile,
) -> Result<crate::diagnostics_store::DiagnosticsWriterConfig> {
    let configured = &stored_config.runtime.diagnostics;
    let defaults = crate::diagnostics_store::DiagnosticsWriterConfig::default();
    crate::diagnostics_store::DiagnosticsWriterConfig {
        queue_capacity: configured.queue_capacity.unwrap_or(defaults.queue_capacity),
        slow_trace_threshold: Duration::from_millis(
            configured
                .slow_trace_threshold_ms
                .unwrap_or(defaults.slow_trace_threshold.as_millis() as u64),
        ),
        sample_rate_permyriad: configured
            .sample_rate_permyriad
            .unwrap_or(defaults.sample_rate_permyriad),
        retention_age: Duration::from_secs(
            configured
                .retention_age_days
                .unwrap_or(defaults.retention_age.as_secs() / (24 * 60 * 60))
                .saturating_mul(24 * 60 * 60),
        ),
        retention_max_traces: configured
            .retention_max_traces
            .unwrap_or(defaults.retention_max_traces),
        retention_min_traces: configured
            .retention_min_traces
            .unwrap_or(defaults.retention_min_traces),
        retention_delete_batch: configured
            .retention_delete_batch
            .unwrap_or(defaults.retention_delete_batch),
    }
    .validate()
}

fn resolve_otlp_exporter_config(
    stored_config: &HolonConfigFile,
    credential_store: &CredentialStoreFile,
) -> Result<Option<crate::otlp_exporter::OtlpExporterConfig>> {
    let configured = &stored_config.runtime.observability.otlp;
    if !configured.enabled.unwrap_or(false) {
        return Ok(None);
    }

    let defaults = crate::otlp_exporter::OtlpExporterConfig::default();
    let mut headers = configured.headers.clone();
    if let Some(profile) = configured.credential_profile.as_deref() {
        let profile = normalize_credential_profile_id(profile)?;
        let credential = credential_store
            .profiles
            .get(&profile)
            .ok_or_else(|| anyhow!("OTLP credential profile {profile} is not configured"))?;
        if headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
        {
            return Err(anyhow!(
                "OTLP headers must not configure authorization when credential_profile is set"
            ));
        }
        if credential.material.trim().is_empty() {
            return Err(anyhow!("OTLP credential profile {profile} is empty"));
        }
        match credential.kind {
            CredentialKind::ApiKey
            | CredentialKind::BearerToken
            | CredentialKind::OAuth
            | CredentialKind::SessionToken => {
                headers.insert(
                    "authorization".to_string(),
                    format!("Bearer {}", credential.material),
                );
            }
            CredentialKind::AwsSdk | CredentialKind::None => {
                return Err(anyhow!(
                    "OTLP credential profile {profile} must use api_key, bearer_token, oauth, or session_token"
                ));
            }
        }
    }

    let config = crate::otlp_exporter::OtlpExporterConfig {
        endpoint: configured.endpoint.clone().unwrap_or_default(),
        headers,
        queue_capacity: configured.queue_capacity.unwrap_or(defaults.queue_capacity),
        batch_size: configured.batch_size.unwrap_or(defaults.batch_size),
        batch_interval: Duration::from_millis(
            configured
                .batch_interval_ms
                .unwrap_or(defaults.batch_interval.as_millis() as u64),
        ),
        timeout: Duration::from_millis(
            configured
                .timeout_ms
                .unwrap_or(defaults.timeout.as_millis() as u64),
        ),
    };
    config.validate()?;
    Ok(Some(config))
}

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests;

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod models {
    use super::*;
    use crate::model_catalog::BuiltInModelCatalog;
    use anyhow::Result;
    mod list_catalog_tests;
    mod policy_material_tests;
    mod route_cache_tests;
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod provider_headers {
    use super::*;
    mod tests;
}
