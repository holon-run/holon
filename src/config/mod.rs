use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use axum::http::{HeaderName, HeaderValue, Method};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};

use crate::{
    auth::{codex_cli_auth_file_exists, load_codex_cli_credential},
    context::ContextConfig,
    model_catalog::{
        BuiltInModelCatalog, BuiltInModelMetadata, ModelRuntimeOverride, ResolvedRuntimeModelPolicy,
    },
    model_discovery::{discovery_cache_path, load_discovery_cache_at, ModelDiscoveryCacheFile},
    provider::ProviderNativeWebSearchKind,
    types::{ViewImageSelectedMode, ViewImageVisionCandidate, ViewImageVisionSelection},
    web::{WebProviderKind, WebSearchMode},
};

mod builtin_providers;
mod credentials;
mod file;
mod models;
mod providers;
mod schema;
mod web;
mod x_search;

pub use builtin_providers::*;
pub use credentials::*;
pub use file::*;
pub use models::*;
pub use providers::*;
pub use schema::*;
pub use web::*;
pub use x_search::*;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub default_agent_id: String,
    pub http_addr: String,
    pub callback_base_url: String,
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
    pub api_cors: ApiCorsConfigFile,
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
    pub disable_provider_fallback: bool,
    pub tui_alternate_screen: AltScreenMode,
    pub validated_model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    pub validated_unknown_model_fallback: Option<ModelRuntimeOverride>,
    pub model_discovery_cache: ModelDiscoveryCacheFile,
    pub providers: ProviderRegistry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AltScreenMode {
    #[default]
    Auto,
    Always,
    Never,
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
    /// Local providers with `CredentialSource::None` (e.g. vllm) are excluded
    /// because their availability cannot be verified from config alone — they
    /// exist in the builtin registry regardless of whether the service is running.
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

    fn load_with_home_and_mode(
        home_override: Option<PathBuf>,
        mode: ConfigLoadMode,
    ) -> Result<Self> {
        let settings_env = load_settings_env().unwrap_or_default();
        let home_dir = home_override.unwrap_or_else(|| {
            env::var("HOLON_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_holon_home())
        });
        let config_file_path = persisted_config_path(&home_dir);
        let stored_config = load_persisted_config_at(&config_file_path)?;
        validate_api_cors_config(&stored_config.api.cors)?;
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
        let prompt_budget_estimated_tokens = env::var("HOLON_PROMPT_BUDGET_ESTIMATED_TOKENS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4096);
        let compaction_trigger_estimated_tokens =
            env::var("HOLON_COMPACTION_TRIGGER_ESTIMATED_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2048);
        let compaction_keep_recent_estimated_tokens =
            env::var("HOLON_COMPACTION_KEEP_RECENT_ESTIMATED_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(768);
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

        let disable_provider_fallback = resolve_disable_provider_fallback(&stored_config)?;
        let validated_model_overrides = resolve_model_catalog(&stored_config)?;
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
            mode,
        ) {
            Ok(selection) => selection,
            Err(error) if mode.allow_unresolved_model() => {
                tracing::debug!(error = %error, "using unresolved diagnostic model for config inspection");
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

        Ok(Self {
            default_agent_id,
            http_addr,
            callback_base_url,
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
            api_cors: stored_config.api.cors.clone(),
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
            disable_provider_fallback,
            tui_alternate_screen,
            validated_model_overrides,
            validated_unknown_model_fallback,
            model_discovery_cache,
            providers,
        })
    }

    pub fn run_dir(&self) -> PathBuf {
        self.home_dir.join("run")
    }

    pub fn runtime_db_path(&self) -> PathBuf {
        self.data_dir.join("state").join("runtime.sqlite")
    }

    pub fn runtime_db_lock_path(&self) -> PathBuf {
        self.data_dir.join("state").join("runtime.lock")
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
                ControlTransportKind::Tcp => !self.tcp_listener_is_local(),
            },
        }
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
    fn allow_unresolved_model(self) -> bool {
        matches!(self, Self::ConfigInspection)
    }

    fn skip_authenticated_model_resolution(self) -> bool {
        matches!(self, Self::ConfigInspection)
    }
}

impl AltScreenMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(anyhow!(
                "invalid alternate screen mode {other}; expected auto|always|never"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

pub fn load_settings_env() -> Result<HashMap<String, String>> {
    let path = settings_path();
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let settings: ClaudeSettings =
        serde_json::from_str(&content).context("failed to parse ~/.claude/settings.json")?;
    Ok(settings.env)
}

pub fn default_holon_home() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".holon")
}

pub fn default_codex_home() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".codex")
}

fn settings_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".claude/settings.json")
}

fn resolve_disable_provider_fallback(stored_config: &HolonConfigFile) -> Result<bool> {
    resolve_disable_provider_fallback_override(
        env::var("HOLON_DISABLE_PROVIDER_FALLBACK").ok().as_deref(),
        stored_config,
    )
}

fn resolve_disable_provider_fallback_override(
    env_override: Option<&str>,
    stored_config: &HolonConfigFile,
) -> Result<bool> {
    match env_override {
        Some(value) => parse_bool_value(value)
            .map_err(|_| anyhow!("HOLON_DISABLE_PROVIDER_FALLBACK expects a boolean"))?
            .ok_or_else(|| anyhow!("HOLON_DISABLE_PROVIDER_FALLBACK expects a boolean")),
        None => Ok(stored_config
            .runtime
            .disable_provider_fallback
            .unwrap_or(false)),
    }
}

fn resolve_anthropic_context_management_config() -> Result<AnthropicContextManagementConfig> {
    resolve_anthropic_context_management_config_with_defaults(
        default_anthropic_runtime_cache_strategy(),
        true,
    )
}

fn resolve_anthropic_compatible_context_management_config(
) -> Result<AnthropicContextManagementConfig> {
    resolve_anthropic_context_management_config_with_defaults(
        default_anthropic_runtime_cache_strategy(),
        false,
    )
}

fn resolve_anthropic_context_management_config_with_defaults(
    default_cache_strategy: AnthropicCacheStrategy,
    auto_prompt_cache_betas: bool,
) -> Result<AnthropicContextManagementConfig> {
    let enabled = match env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT").ok() {
        Some(value) => parse_bool_value(&value)
            .map_err(|_| anyhow!("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT expects a boolean"))?
            .ok_or_else(|| anyhow!("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT expects a boolean"))?,
        None => false,
    };
    let trigger_input_tokens = env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS")
        .ok()
        .map(|value| {
            parse_positive_u32_key(
                "HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS",
                &value,
            )
        })
        .transpose()?
        .unwrap_or(100_000);
    let keep_recent_tool_uses =
        env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_KEEP_RECENT_TOOL_USES")
            .ok()
            .map(|value| {
                parse_positive_u32_key(
                    "HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_KEEP_RECENT_TOOL_USES",
                    &value,
                )
            })
            .transpose()?
            .unwrap_or(3);
    let clear_at_least_input_tokens =
        env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_CLEAR_AT_LEAST_INPUT_TOKENS")
            .ok()
            .map(|value| {
                parse_positive_u32_key(
                    "HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_CLEAR_AT_LEAST_INPUT_TOKENS",
                    &value,
                )
            })
            .transpose()?;
    let cache_strategy = env::var("HOLON_ANTHROPIC_CACHE_STRATEGY")
        .ok()
        .map(|value| parse_anthropic_cache_strategy_env(&value))
        .transpose()?
        .unwrap_or(default_cache_strategy);
    let betas_env = env::var("HOLON_ANTHROPIC_BETAS").ok();
    let betas = match betas_env {
        Some(value) => parse_comma_separated_values(&value),
        None if auto_prompt_cache_betas
            && cache_strategy == AnthropicCacheStrategy::ClaudeCodePromptCache =>
        {
            vec![
                "claude-code-20250219".to_string(),
                "prompt-caching-scope-2026-01-05".to_string(),
            ]
        }
        None => Vec::new(),
    };

    Ok(AnthropicContextManagementConfig {
        enabled,
        trigger_input_tokens,
        keep_recent_tool_uses,
        clear_at_least_input_tokens,
        cache_strategy,
        betas,
    })
}

fn default_anthropic_runtime_cache_strategy() -> AnthropicCacheStrategy {
    AnthropicCacheStrategy::ClaudeCodePromptCache
}

fn parse_anthropic_cache_strategy_env(raw_value: &str) -> Result<AnthropicCacheStrategy> {
    if raw_value.trim().is_empty() {
        return Ok(default_anthropic_runtime_cache_strategy());
    }
    parse_anthropic_cache_strategy(raw_value)
}

fn parse_anthropic_cache_strategy(raw_value: &str) -> Result<AnthropicCacheStrategy> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "messages_native" | "messages-native" | "native" | "current" => {
            Ok(AnthropicCacheStrategy::MessagesNative)
        }
        "claude_code_prompt_cache"
        | "claude-code-prompt-cache"
        | "claude_cli_like"
        | "claude-cli-like"
        | "claude" => {
            Ok(AnthropicCacheStrategy::ClaudeCodePromptCache)
        }
        _ => Err(anyhow!(
            "HOLON_ANTHROPIC_CACHE_STRATEGY expects messages_native, claude_code_prompt_cache, or a legacy alias"
        )),
    }
}

#[cfg(test)]
mod tests;
