//! Configuration module: file loading, provider registry, model catalog,
//! credentials, web config, and schema helpers.

mod builtin_providers;
mod credentials;
mod file;
mod models;
mod providers;
mod schema;
mod web;

pub use builtin_providers::*;
pub use credentials::*;
pub use file::*;
pub use models::*;
pub use providers::*;
pub use schema::*;
pub use web::*;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::auth::{codex_cli_auth_file_exists, load_codex_cli_credential};
use crate::model_catalog::{BuiltInModelCatalog, ModelRuntimeOverride};
use crate::model_discovery::{
    discovery_cache_path, load_discovery_cache_at, ModelDiscoveryCacheFile,
};
pub use file::AltScreenMode;
pub use providers::ControlAuthMode;

pub const DEFAULT_LOCAL_AGENT_ID: &str = "main";

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
    pub default_model: ModelRef,
    pub fallback_models: Vec<ModelRef>,
    pub vision_model: Option<ModelRef>,
    pub vision_candidate_models: Vec<ModelRef>,
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
                (ModelRef::new(ProviderId::openai(), "unknown"), Vec::new())
            }
            Err(error) => return Err(error),
        };
        let vision_candidate_models =
            authenticated_model_candidates(&providers, &validated_model_overrides);
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

    pub fn provider_chain(&self) -> Vec<ModelRef> {
        RuntimeModelCatalog::from_config(self).provider_chain(None)
    }

    pub fn provider_chain_with_override(&self, model_override: Option<&ModelRef>) -> Vec<ModelRef> {
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
enum ConfigLoadMode {
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

pub fn validate_provider_config(
    provider_id: &ProviderId,
    provider_config: &ProviderConfigFile,
) -> Result<()> {
    parse_url_value("providers.<id>.base_url", &provider_config.base_url)?;
    validate_provider_auth(provider_id, &provider_config.auth)?;
    validate_provider_builtin_web_search(provider_id, provider_config)
}

pub fn persisted_provider_config_mut<'a>(
    config: &'a mut HolonConfigFile,
    key: &str,
    suffix: &str,
) -> Result<&'a mut ProviderConfigFile> {
    let rest = key
        .strip_prefix("providers.")
        .and_then(|value| value.strip_suffix(suffix))
        .ok_or_else(|| unknown_config_key(key))?;
    if rest.is_empty() {
        return Err(anyhow!(
            "providers.<id>{suffix} requires a non-empty provider id"
        ));
    }
    let id = ProviderId::parse(rest)?;
    if !config.providers.contains_key(&id) {
        let defaults = built_in_provider_default_config(&id)?
            .ok_or_else(|| anyhow!("provider {} not found", id.as_str()))?;
        config.providers.insert(id.clone(), defaults);
    }
    config
        .providers
        .get_mut(&id)
        .ok_or_else(|| anyhow!("provider {} not found", id.as_str()))
}

pub fn get_provider_config_key(config: &HolonConfigFile, key: &str) -> Result<Value> {
    let rest = key
        .strip_prefix("providers.")
        .ok_or_else(|| unknown_config_key(key))?;
    if rest.is_empty() {
        return Err(anyhow!("providers.<id> requires a non-empty provider id"));
    }
    if let Some(id) = rest.strip_suffix(".transport") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.transport.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".base_url") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.base_url.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.source") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.auth.source.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.kind") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.auth.kind.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.env") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.env.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.profile") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.profile.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.external") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.external.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if rest.contains('.') {
        return Err(unknown_config_key(key));
    }
    Ok(config
        .providers
        .get(&ProviderId::parse(rest)?)
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or(Value::Null))
}

pub fn set_provider_config_key(
    config: &mut HolonConfigFile,
    key: &str,
    raw_value: &str,
) -> Result<()> {
    if key.ends_with(".transport") {
        persisted_provider_config_mut(config, key, ".transport")?.transport =
            ProviderTransportKind::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".base_url") {
        let value = raw_value.trim();
        parse_url_value(key, value)?;
        persisted_provider_config_mut(config, key, ".base_url")?.base_url = value.to_string();
        return Ok(());
    }
    if key.ends_with(".auth.source") {
        persisted_provider_config_mut(config, key, ".auth.source")?
            .auth
            .source = CredentialSource::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".auth.kind") {
        persisted_provider_config_mut(config, key, ".auth.kind")?
            .auth
            .kind = CredentialKind::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".auth.env") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.env")?
            .auth
            .env = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".auth.profile") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.profile")?
            .auth
            .profile = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".auth.external") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.external")?
            .auth
            .external = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    Err(unknown_config_key(key))
}

pub fn unset_provider_config_key(config: &mut HolonConfigFile, key: &str) -> Result<()> {
    if key.ends_with(".auth.env") {
        persisted_provider_config_mut(config, key, ".auth.env")?
            .auth
            .env = None;
        return Ok(());
    }
    if key.ends_with(".auth.profile") {
        persisted_provider_config_mut(config, key, ".auth.profile")?
            .auth
            .profile = None;
        return Ok(());
    }
    if key.ends_with(".auth.external") {
        persisted_provider_config_mut(config, key, ".auth.external")?
            .auth
            .external = None;
        return Ok(());
    }
    let id = key
        .strip_prefix("providers.")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("providers.<id> requires a non-empty provider id"))?;
    if config.providers.remove(&ProviderId::parse(id)?).is_none() {
        return Err(anyhow!("provider {id} not found"));
    }
    Ok(())
}

fn resolve_model_selection_for_load_mode(
    explicit_default: Option<ModelRef>,
    explicit_fallbacks: Option<Vec<ModelRef>>,
    providers: &ProviderRegistry,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
    mode: ConfigLoadMode,
) -> Result<(ModelRef, Vec<ModelRef>)> {
    if mode.skip_authenticated_model_resolution() {
        let default_model =
            explicit_default.unwrap_or_else(|| ModelRef::new(ProviderId::openai(), "unknown"));
        let fallback_models = explicit_fallbacks.unwrap_or_default();
        return Ok((
            default_model.clone(),
            dedupe_fallback_models(fallback_models, &default_model),
        ));
    }

    resolve_model_selection_from_explicit(
        explicit_default,
        explicit_fallbacks,
        providers,
        model_overrides,
    )
}

pub fn resolve_model_selection_from_explicit(
    explicit_default: Option<ModelRef>,
    explicit_fallbacks: Option<Vec<ModelRef>>,
    providers: &ProviderRegistry,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
) -> Result<(ModelRef, Vec<ModelRef>)> {
    let auth_candidates = if explicit_default.is_none() || explicit_fallbacks.is_none() {
        authenticated_model_candidates(providers, model_overrides)
    } else {
        Vec::new()
    };

    let default_model = explicit_default
        .or_else(|| auth_candidates.first().cloned())
        .ok_or_else(|| {
            anyhow!(
                "no default model configured and no authenticated provider with a known model is available; set HOLON_MODEL or model.default, or configure provider credentials"
            )
        })?;
    let fallback_models = explicit_fallbacks.unwrap_or_else(|| {
        auth_candidates
            .into_iter()
            .filter(|model| model != &default_model)
            .collect()
    });

    Ok((
        default_model.clone(),
        dedupe_fallback_models(fallback_models, &default_model),
    ))
}

pub fn resolve_default_model(stored_config: &HolonConfigFile) -> Result<Option<ModelRef>> {
    if let Ok(value) = env::var("HOLON_MODEL") {
        return ModelRef::parse(&value).map(Some);
    }
    if let Some(value) = &stored_config.model.default {
        return ModelRef::parse(value).map(Some);
    }
    Ok(None)
}

pub fn resolve_fallback_models(stored_config: &HolonConfigFile) -> Result<Option<Vec<ModelRef>>> {
    if let Ok(value) = env::var("HOLON_MODEL_FALLBACKS") {
        Ok(Some(parse_model_ref_list(&value)?))
    } else if !stored_config.model.fallbacks.is_empty() {
        Ok(Some(
            stored_config
                .model
                .fallbacks
                .iter()
                .map(|value| ModelRef::parse(value))
                .collect::<Result<Vec<_>>>()?,
        ))
    } else {
        Ok(None)
    }
}

pub fn resolve_vision_model(stored_config: &HolonConfigFile) -> Result<Option<ModelRef>> {
    if let Ok(value) = env::var("HOLON_VISION_MODEL") {
        return ModelRef::parse(&value).map(Some);
    }
    if let Some(value) = &stored_config.vision.default {
        return ModelRef::parse(value).map(Some);
    }
    Ok(None)
}

pub fn authenticated_model_candidates(
    providers: &ProviderRegistry,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
) -> Vec<ModelRef> {
    let catalog = BuiltInModelCatalog::default();
    let mut provider_ids = providers
        .values()
        .filter(|provider| provider_has_usable_auth(provider))
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();
    provider_ids.sort_by(|left, right| {
        provider_auth_priority(left)
            .cmp(&provider_auth_priority(right))
            .then_with(|| left.as_str().cmp(right.as_str()))
    });

    let mut candidates = provider_ids
        .into_iter()
        .filter_map(|provider| {
            catalog
                .preferred_model_for_provider(&provider)
                .or_else(|| preferred_override_model_for_provider(&provider, model_overrides))
        })
        .collect::<Vec<_>>();
    candidates.dedup();
    candidates
}

pub fn provider_has_usable_auth(provider: &ProviderRuntimeConfig) -> bool {
    match provider.auth.source {
        CredentialSource::Env => provider.has_configured_credential(),
        CredentialSource::AuthProfile => {
            provider.has_configured_credential()
                || (provider.id.is_openai_codex()
                    && provider.auth.profile.as_deref() == Some(OPENAI_CODEX_CREDENTIAL_PROFILE)
                    && provider
                        .codex_home
                        .as_deref()
                        .map(|home| {
                            codex_cli_auth_file_exists(home)
                                && load_codex_cli_credential(home).is_ok()
                        })
                        .unwrap_or(false))
        }
        CredentialSource::ExternalCli => {
            provider.auth.external.as_deref() == Some("codex_cli")
                && provider
                    .codex_home
                    .as_deref()
                    .map(|home| {
                        codex_cli_auth_file_exists(home) && load_codex_cli_credential(home).is_ok()
                    })
                    .unwrap_or(false)
        }
        CredentialSource::None | CredentialSource::CredentialProcess => false,
    }
}

pub fn provider_auth_priority(provider: &ProviderId) -> usize {
    match provider.as_str() {
        ProviderId::OPENAI_CODEX => 0,
        ProviderId::OPENAI => 1,
        ProviderId::ANTHROPIC => 2,
        ProviderId::GEMINI => 3,
        _ => 100,
    }
}

pub fn preferred_override_model_for_provider(
    provider: &ProviderId,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
) -> Option<ModelRef> {
    let mut models = model_overrides
        .keys()
        .filter(|model| model.provider == *provider)
        .cloned()
        .collect::<Vec<_>>();
    models.sort_by_key(ModelRef::as_string);
    models.into_iter().next()
}

pub fn dedupe_fallback_models(
    configured: Vec<ModelRef>,
    default_model: &ModelRef,
) -> Vec<ModelRef> {
    configured
        .into_iter()
        .filter(|model| model != default_model)
        .fold(Vec::new(), |mut acc, model| {
            if !acc.iter().any(|existing| existing == &model) {
                acc.push(model);
            }
            acc
        })
}

pub fn resolve_disable_provider_fallback(stored_config: &HolonConfigFile) -> Result<bool> {
    resolve_disable_provider_fallback_override(
        env::var("HOLON_DISABLE_PROVIDER_FALLBACK").ok().as_deref(),
        stored_config,
    )
}

pub fn resolve_disable_provider_fallback_override(
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

pub fn parse_model_ref_list(raw_value: &str) -> Result<Vec<ModelRef>> {
    let values = raw_value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ModelRef::parse)
        .collect::<Result<Vec<_>>>()?;
    if values.is_empty() {
        return Err(anyhow!("model ref list must not be empty"));
    }
    Ok(values)
}

pub fn parse_string_list(raw_value: &str) -> Result<Vec<String>> {
    let trimmed = raw_value.trim();
    if trimmed.starts_with('[') {
        let values: Vec<String> =
            serde_json::from_str(trimmed).context("expected a JSON string array")?;
        return Ok(values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect());
    }
    Ok(trimmed
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect())
}

pub fn web_provider_config_mut<'a>(
    config: &'a mut HolonConfigFile,
    key: &str,
    suffix: &str,
) -> Result<&'a mut WebProviderConfigFile> {
    let rest = key.strip_prefix("web.providers.").unwrap();
    let name = rest.strip_suffix(suffix).unwrap();
    if name.is_empty() {
        return Err(anyhow!(
            "web.providers.<name>{suffix} requires a non-empty provider name"
        ));
    }
    config.web.providers.get_mut(name).ok_or_else(|| {
        anyhow!("web provider {name} not found; set web.providers.{name}.kind first")
    })
}

pub fn default_web_command_output() -> WebCommandOutputConfigFile {
    WebCommandOutputConfigFile {
        format: WebCommandOutputFormatFile::Json,
        mapping: WebCommandResultMappingFile {
            title: String::new(),
            url: String::new(),
            snippet: None,
            published_at: None,
        },
    }
}

pub fn web_provider_output_mapping_key(rest: &str) -> Option<(&str, &str)> {
    let (name, field) = rest.split_once(".output.mapping.")?;
    matches!(field, "title" | "url" | "snippet" | "published_at").then_some((name, field))
}

pub fn output_mapping_field<'a>(
    mapping: &'a WebCommandResultMappingFile,
    field: &str,
) -> Option<&'a str> {
    match field {
        "title" => (!mapping.title.is_empty()).then_some(mapping.title.as_str()),
        "url" => (!mapping.url.is_empty()).then_some(mapping.url.as_str()),
        "snippet" => mapping.snippet.as_deref(),
        "published_at" => mapping.published_at.as_deref(),
        _ => None,
    }
}

pub fn set_output_mapping_field(
    mapping: &mut WebCommandResultMappingFile,
    field: &str,
    value: &str,
) {
    match field {
        "title" => mapping.title = value.to_string(),
        "url" => mapping.url = value.to_string(),
        "snippet" => {
            mapping.snippet = (!value.is_empty()).then(|| value.to_string());
        }
        "published_at" => {
            mapping.published_at = (!value.is_empty()).then(|| value.to_string());
        }
        _ => {}
    }
}

pub fn unset_output_mapping_field(mapping: &mut WebCommandResultMappingFile, field: &str) {
    match field {
        "title" => mapping.title.clear(),
        "url" => mapping.url.clear(),
        "snippet" => mapping.snippet = None,
        "published_at" => mapping.published_at = None,
        _ => {}
    }
}

pub fn parse_model_catalog_value(
    raw_value: &str,
) -> Result<BTreeMap<String, ModelRuntimeOverride>> {
    let parsed: BTreeMap<String, ModelRuntimeOverride> =
        serde_json::from_str(raw_value).context("models.catalog expects a JSON object")?;
    let mut validated = BTreeMap::new();
    for (model_ref, override_config) in parsed {
        ModelRef::parse(&model_ref)?;
        validated.insert(model_ref, validate_model_runtime_override(override_config)?);
    }
    Ok(validated)
}

pub fn parse_optional_model_runtime_override(
    raw_value: &str,
) -> Result<Option<ModelRuntimeOverride>> {
    if raw_value.trim().eq_ignore_ascii_case("null") {
        return Ok(None);
    }
    let parsed: ModelRuntimeOverride =
        serde_json::from_str(raw_value).context("expected a JSON object or null")?;
    validate_optional_model_runtime_override(Some(parsed))
}

pub fn parse_bool_value(raw_value: &str) -> Result<Option<bool>> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "true" | "1" | "yes" | "on" => Ok(Some(true)),
        "false" | "0" | "no" | "off" => Ok(Some(false)),
        _ => Err(anyhow!("expected boolean true|false|1|0|yes|no|on|off")),
    }
}

pub fn resolve_anthropic_context_management_config() -> Result<AnthropicContextManagementConfig> {
    resolve_anthropic_context_management_config_with_defaults(
        default_anthropic_runtime_cache_strategy(),
        true,
    )
}

pub fn resolve_anthropic_compatible_context_management_config(
) -> Result<AnthropicContextManagementConfig> {
    resolve_anthropic_context_management_config_with_defaults(
        default_anthropic_runtime_cache_strategy(),
        false,
    )
}

pub fn resolve_anthropic_context_management_config_with_defaults(
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

pub fn default_anthropic_runtime_cache_strategy() -> AnthropicCacheStrategy {
    AnthropicCacheStrategy::ClaudeCodePromptCache
}

pub fn parse_anthropic_cache_strategy_env(raw_value: &str) -> Result<AnthropicCacheStrategy> {
    if raw_value.trim().is_empty() {
        return Ok(default_anthropic_runtime_cache_strategy());
    }
    parse_anthropic_cache_strategy(raw_value)
}

pub fn parse_anthropic_cache_strategy(raw_value: &str) -> Result<AnthropicCacheStrategy> {
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

pub fn parse_comma_separated_values(raw_value: &str) -> Vec<String> {
    raw_value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub fn parse_positive_u32_key(key: &str, raw_value: &str) -> Result<u32> {
    raw_value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

pub fn parse_positive_u64_key(key: &str, raw_value: &str) -> Result<u64> {
    raw_value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

pub fn parse_positive_usize_key(key: &str, raw_value: &str) -> Result<usize> {
    raw_value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

pub fn parse_percentage_u8_key(key: &str, raw_value: &str) -> Result<u8> {
    raw_value
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|value| *value > 0 && *value <= 100)
        .ok_or_else(|| anyhow!("{key} expects an integer from 1 to 100"))
}

pub fn validate_model_runtime_override(
    override_config: ModelRuntimeOverride,
) -> Result<ModelRuntimeOverride> {
    if let Some(percent) = override_config.effective_context_window_percent {
        if percent == 0 || percent > 100 {
            return Err(anyhow!(
                "effective_context_window_percent expects an integer from 1 to 100"
            ));
        }
    }
    if let (Some(window), Some(prompt_budget)) = (
        override_config.context_window_tokens,
        override_config.prompt_budget_estimated_tokens,
    ) {
        if prompt_budget > window {
            return Err(anyhow!(
                "prompt_budget_estimated_tokens must not exceed context_window_tokens"
            ));
        }
    }
    if let (Some(trigger), Some(prompt_budget)) = (
        override_config.compaction_trigger_estimated_tokens,
        override_config.prompt_budget_estimated_tokens,
    ) {
        if trigger > prompt_budget {
            return Err(anyhow!(
                "compaction_trigger_estimated_tokens must not exceed prompt_budget_estimated_tokens"
            ));
        }
    }
    if let (Some(keep_recent), Some(trigger)) = (
        override_config.compaction_keep_recent_estimated_tokens,
        override_config.compaction_trigger_estimated_tokens,
    ) {
        if keep_recent > trigger {
            return Err(anyhow!(
                "compaction_keep_recent_estimated_tokens must not exceed compaction_trigger_estimated_tokens"
            ));
        }
    }
    if override_config.is_empty() {
        return Ok(ModelRuntimeOverride::default());
    }
    Ok(override_config)
}

pub fn ensure_unknown_model_fallback(config: &mut HolonConfigFile) -> &mut ModelRuntimeOverride {
    config
        .model
        .unknown_fallback
        .get_or_insert_with(ModelRuntimeOverride::default)
}

pub fn clear_unknown_model_fallback_field(
    config: &mut HolonConfigFile,
    clear: impl FnOnce(&mut ModelRuntimeOverride),
) {
    if let Some(value) = config.model.unknown_fallback.as_mut() {
        clear(value);
        if value.is_empty() {
            config.model.unknown_fallback = None;
        }
    }
}

pub fn parse_url_value(key: &str, raw_value: &str) -> Result<()> {
    let trimmed = raw_value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{key} expects a non-empty URL"));
    }
    let parsed = reqwest::Url::parse(trimmed)
        .with_context(|| format!("{key} expects a valid absolute URL"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(anyhow!("{key} expects an http or https URL"));
    }
    Ok(())
}

pub fn is_startup_only_config_key(key: &str) -> bool {
    let _ = key;
    false
}

pub fn startup_only_config_key_error(key: &str) -> anyhow::Error {
    anyhow!(
        "config key {key} is startup-only; configure it via env vars or CLI startup flags instead of runtime config mutation"
    )
}

pub fn get_config_value(
    primary_env: &str,
    secondary_env: Option<&str>,
    settings_env: &HashMap<String, String>,
) -> Option<String> {
    env::var(primary_env)
        .ok()
        .or_else(|| secondary_env.and_then(|key| env::var(key).ok()))
        .or_else(|| settings_env.get(primary_env).cloned())
        .or_else(|| secondary_env.and_then(|key| settings_env.get(key).cloned()))
}

pub fn unknown_config_key(key: &str) -> anyhow::Error {
    if is_startup_only_config_key(key) {
        return startup_only_config_key_error(key);
    }
    if key.starts_with("web.providers.") {
        return anyhow!("unknown web providers config key {key}; mutable fields: .kind, .base_url, .credential_profile; .capabilities is derived read-only metadata; use web.providers.<name>.kind to create a provider first");
    }
    let supported = config_schema()
        .into_iter()
        .map(|entry| entry.key)
        .collect::<Vec<_>>();
    let suggestions = supported
        .iter()
        .filter(|candidate| candidate.contains(key) || key.contains(**candidate))
        .copied()
        .collect::<Vec<_>>();
    if suggestions.is_empty() {
        anyhow!(
            "unknown config key {key}; supported keys: {}",
            supported.join(", ")
        )
    } else {
        anyhow!(
            "unknown config key {key}; did you mean: {}",
            suggestions.join(", ")
        )
    }
}

pub fn read_only_web_provider_capabilities_key_error(key: &str) -> anyhow::Error {
    anyhow!(
        "{key} is derived read-only capability metadata; configure web.providers.<name>.kind instead"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::MutexGuard;

    use serde_json::{json, Value};
    use tempfile::tempdir;

    use crate::context::ContextConfig;
    use crate::model_catalog::{
        BuiltInModelMetadata, ModelCapabilityOverride, ModelMetadataSource, ModelRuntimeOverride,
    };
    use crate::model_discovery::{ModelDiscoveryCacheFile, ProviderModelDiscoveryCache};
    use crate::provider::ProviderNativeWebSearchKind;

    use super::{
        built_in_provider_doc_entries, built_in_provider_registry_with_settings, config_schema,
        credential_store_path, default_api_cors_allowed_headers, default_api_cors_allowed_methods,
        default_holon_home, get_config_key, get_config_value, list_credential_profiles_at,
        load_persisted_config_at, parse_anthropic_cache_strategy,
        parse_anthropic_cache_strategy_env, parse_comma_separated_values, parse_url_value,
        persisted_config_path, provider_registry_for_tests,
        resolve_anthropic_context_management_config, save_persisted_config_at, set_config_key,
        set_credential_profile_at, unset_config_key, validate_provider_config,
        AnthropicCacheStrategy, AnthropicContextManagementConfig, AppConfig, ControlAuthMode,
        CredentialKind, CredentialSource, CredentialStoreFile, HolonConfigFile, ModelConfigFile,
        ModelRef, ProviderAuthConfig, ProviderBuiltinWebSearchConfig, ProviderConfigFile,
        ProviderId, ProviderRegistry, ProviderRuntimeConfig, ProviderTransportKind,
        RuntimeModelCatalog, DEFAULT_LOCAL_AGENT_ID, OPENAI_CODEX_CREDENTIAL_PROFILE,
    };

    struct EnvVarSnapshot {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    struct EnvVarGuard {
        snapshots: Vec<EnvVarSnapshot>,
        _lock: MutexGuard<'static, ()>,
    }

    const VERCEL_AI_GATEWAY_ENV_KEYS: &[&str] = &[
        "VERCEL_OIDC_TOKEN",
        "AI_GATEWAY_API_KEY",
        "VERCEL_AI_GATEWAY_API_KEY",
    ];

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let mut guard = Self::new();
            guard.set_var(key, value);
            guard
        }

        fn unset(key: &'static str) -> Self {
            let mut guard = Self::new();
            guard.unset_var(key);
            guard
        }

        fn unset_many(keys: &[&'static str]) -> Self {
            let mut guard = Self::new();
            for key in keys {
                guard.unset_var(key);
            }
            guard
        }

        fn set_and_unset(
            set_vars: &[(&'static str, &std::ffi::OsStr)],
            unset_vars: &[&'static str],
        ) -> Self {
            let mut guard = Self::new();
            for (key, value) in set_vars {
                guard.set_var(key, value);
            }
            for key in unset_vars {
                guard.unset_var(key);
            }
            guard
        }

        fn new() -> Self {
            Self {
                snapshots: Vec::new(),
                _lock: crate::test_env::lock_env(),
            }
        }

        fn set_var(&mut self, key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
            self.snapshots.push(EnvVarSnapshot {
                key,
                original: std::env::var_os(key),
            });
            std::env::set_var(key, value);
        }

        fn unset_var(&mut self, key: &'static str) {
            self.snapshots.push(EnvVarSnapshot {
                key,
                original: std::env::var_os(key),
            });
            std::env::remove_var(key);
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for snapshot in self.snapshots.iter().rev() {
                if let Some(value) = &snapshot.original {
                    std::env::set_var(snapshot.key, value);
                } else {
                    std::env::remove_var(snapshot.key);
                }
            }
        }
    }

    struct TestAppConfigFixture {
        _home_dir: tempfile::TempDir,
        _workspace_dir: tempfile::TempDir,
        config: AppConfig,
    }

    fn test_app_config(default_model: &str, fallback_models: &[&str]) -> TestAppConfigFixture {
        let home_dir = tempdir().unwrap();
        let workspace_dir = tempdir().unwrap();
        let home_path = home_dir.path().to_path_buf();
        let workspace_path = workspace_dir.path().to_path_buf();
        let config = AppConfig {
            default_agent_id: "default".into(),
            http_addr: "127.0.0.1:0".into(),
            callback_base_url: "http://127.0.0.1:0".into(),
            home_dir: home_path.clone(),
            data_dir: home_path.clone(),
            socket_path: home_path.join("run").join("holon.sock"),
            workspace_dir: workspace_path,
            context_window_messages: 8,
            context_window_briefs: 8,
            compaction_trigger_messages: 10,
            compaction_keep_recent_messages: 4,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            control_token: Some("control-value".into()),
            control_auth_mode: ControlAuthMode::Auto,
            api_cors: Default::default(),
            config_file_path: home_path.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse(default_model).unwrap(),
            fallback_models: fallback_models
                .iter()
                .map(|value| ModelRef::parse(value).unwrap())
                .collect(),
            vision_model: None,
            vision_candidate_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: HashMap::new(),
            validated_unknown_model_fallback: None,
            model_discovery_cache: ModelDiscoveryCacheFile::default(),
            providers: provider_registry_for_tests(
                Some("openai-key"),
                Some("anthropic-token"),
                PathBuf::from("/tmp/codex-home"),
            ),
            web_config: crate::web::WebConfig::default(),
        };
        TestAppConfigFixture {
            _home_dir: home_dir,
            _workspace_dir: workspace_dir,
            config,
        }
    }

    #[test]
    fn app_config_defaults_unset_agent_env_to_main_agent() {
        let dir = tempdir().unwrap();
        let _agent_guard = EnvVarGuard::unset("HOLON_AGENT_ID");
        save_persisted_config_at(
            &persisted_config_path(dir.path()),
            &HolonConfigFile {
                model: ModelConfigFile {
                    default: Some("openai/gpt-5.4".into()),
                    ..ModelConfigFile::default()
                },
                ..HolonConfigFile::default()
            },
        )
        .unwrap();

        let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();

        assert_eq!(config.default_agent_id, DEFAULT_LOCAL_AGENT_ID);
    }

    #[test]
    fn app_config_honors_explicit_default_agent_env() {
        let dir = tempdir().unwrap();
        let _agent_guard = EnvVarGuard::set("HOLON_AGENT_ID", "release-bot");
        save_persisted_config_at(
            &persisted_config_path(dir.path()),
            &HolonConfigFile {
                model: ModelConfigFile {
                    default: Some("openai/gpt-5.4".into()),
                    ..ModelConfigFile::default()
                },
                ..HolonConfigFile::default()
            },
        )
        .unwrap();

        let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();

        assert_eq!(config.default_agent_id, "release-bot");
    }

    #[test]
    fn config_falls_back_to_settings_values() {
        let _base_url_guard = EnvVarGuard::unset("ANTHROPIC_BASE_URL");

        let mut settings = HashMap::new();
        settings.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "https://example.com".to_string(),
        );

        let value = get_config_value("ANTHROPIC_BASE_URL", None, &settings);
        assert_eq!(value.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn built_in_openai_codex_defaults_to_holon_oauth_profile() {
        let settings_env = HashMap::new();
        let registry = built_in_provider_registry_with_settings(&settings_env).unwrap();
        let openai_codex = registry.get(&ProviderId::openai_codex()).unwrap();

        assert_eq!(openai_codex.auth.source, CredentialSource::AuthProfile);
        assert_eq!(openai_codex.auth.kind, CredentialKind::OAuth);
        assert_eq!(
            openai_codex.auth.profile.as_deref(),
            Some(OPENAI_CODEX_CREDENTIAL_PROFILE)
        );
        assert_eq!(openai_codex.auth.external.as_deref(), Some("codex_cli"));
        assert!(openai_codex.credential.is_none());
    }

    #[test]
    fn app_config_load_materializes_openai_codex_profile_from_existing_store() {
        let dir = tempdir().unwrap();
        let _agent_guard = EnvVarGuard::unset("HOLON_AGENT_ID");
        save_persisted_config_at(
            &persisted_config_path(dir.path()),
            &HolonConfigFile {
                model: ModelConfigFile {
                    default: Some("openai-codex/gpt-5.4".into()),
                    ..ModelConfigFile::default()
                },
                ..HolonConfigFile::default()
            },
        )
        .unwrap();
        set_credential_profile_at(
            &credential_store_path(dir.path()),
            OPENAI_CODEX_CREDENTIAL_PROFILE,
            CredentialKind::OAuth,
            "{\"tokens\":{\"access_token\":\"token\",\"refresh_token\":\"refresh\",\"account_id\":\"acct\"}}"
                .into(),
        )
        .unwrap();

        let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();
        let openai_codex = config.providers.get(&ProviderId::openai_codex()).unwrap();

        assert_eq!(
            openai_codex.auth.profile.as_deref(),
            Some(OPENAI_CODEX_CREDENTIAL_PROFILE)
        );
        let credential: Value = serde_json::from_str(
            openai_codex
                .credential
                .as_deref()
                .expect("openai-codex credential material should be loaded"),
        )
        .unwrap();
        assert_eq!(
            credential["tokens"]["access_token"],
            Value::String("token".into())
        );
    }

    #[test]
    fn app_config_applies_provider_overrides_before_materializing_openai_codex_profile() {
        let dir = tempdir().unwrap();
        let _agent_guard = EnvVarGuard::unset("HOLON_AGENT_ID");
        save_persisted_config_at(
            &persisted_config_path(dir.path()),
            &HolonConfigFile {
                model: ModelConfigFile {
                    default: Some("openai-codex/gpt-5.4".into()),
                    ..ModelConfigFile::default()
                },
                providers: BTreeMap::from([(
                    ProviderId::openai_codex(),
                    ProviderConfigFile {
                        transport: ProviderTransportKind::OpenAiCodexResponses,
                        base_url: "https://chatgpt.com/backend-api/codex".into(),
                        auth: ProviderAuthConfig {
                            source: CredentialSource::ExternalCli,
                            kind: CredentialKind::SessionToken,
                            env: None,
                            profile: None,
                            external: Some("codex_cli".into()),
                        },
                        reasoning_effort: None,
                        builtin_web_search: None,
                    },
                )]),
                ..HolonConfigFile::default()
            },
        )
        .unwrap();
        set_credential_profile_at(
            &credential_store_path(dir.path()),
            OPENAI_CODEX_CREDENTIAL_PROFILE,
            CredentialKind::ApiKey,
            "wrong-kind-token".into(),
        )
        .unwrap();

        let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();
        let openai_codex = config.providers.get(&ProviderId::openai_codex()).unwrap();

        assert_eq!(openai_codex.auth.source, CredentialSource::ExternalCli);
        assert_eq!(openai_codex.auth.kind, CredentialKind::SessionToken);
        assert_eq!(openai_codex.credential, None);
    }

    #[test]
    fn control_auth_mode_parses_known_values() {
        assert_eq!(
            ControlAuthMode::parse("auto").unwrap(),
            ControlAuthMode::Auto
        );
        assert_eq!(
            ControlAuthMode::parse("required").unwrap(),
            ControlAuthMode::Required
        );
        assert_eq!(
            ControlAuthMode::parse("disabled").unwrap(),
            ControlAuthMode::Disabled
        );
    }

    #[test]
    fn anthropic_cache_strategy_parses_supported_values() {
        assert_eq!(
            parse_anthropic_cache_strategy("messages_native").unwrap(),
            AnthropicCacheStrategy::MessagesNative
        );
        assert_eq!(
            parse_anthropic_cache_strategy("claude-code-prompt-cache").unwrap(),
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );
        assert_eq!(
            parse_anthropic_cache_strategy("current").unwrap(),
            AnthropicCacheStrategy::MessagesNative
        );
        assert_eq!(
            parse_anthropic_cache_strategy("claude-cli-like").unwrap(),
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );
        let err = parse_anthropic_cache_strategy("unknown")
            .err()
            .expect("unknown strategy should fail");
        assert!(err.to_string().contains("messages_native"));
        assert!(err.to_string().contains("claude_code_prompt_cache"));
    }

    #[test]
    fn anthropic_runtime_cache_strategy_defaults_to_claude_code_prompt_cache() {
        let _env_guard =
            EnvVarGuard::unset_many(&["HOLON_ANTHROPIC_CACHE_STRATEGY", "HOLON_ANTHROPIC_BETAS"]);

        let config = resolve_anthropic_context_management_config().unwrap();

        assert_eq!(
            config.cache_strategy,
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );
        assert_eq!(
            config.betas,
            vec![
                "claude-code-20250219".to_string(),
                "prompt-caching-scope-2026-01-05".to_string()
            ]
        );
    }

    #[test]
    fn anthropic_runtime_cache_strategy_empty_env_uses_default() {
        assert_eq!(
            parse_anthropic_cache_strategy_env("").unwrap(),
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );
        assert_eq!(
            parse_anthropic_cache_strategy_env("  ").unwrap(),
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );
    }

    #[test]
    fn anthropic_context_management_struct_default_stays_neutral() {
        assert_eq!(
            AnthropicContextManagementConfig::default().cache_strategy,
            AnthropicCacheStrategy::MessagesNative
        );
        assert!(AnthropicContextManagementConfig::default().betas.is_empty());
    }

    #[test]
    fn comma_separated_values_drop_empty_items() {
        assert_eq!(
            parse_comma_separated_values(
                " claude-code-20250219, ,prompt-caching-scope-2026-01-05 "
            ),
            vec![
                "claude-code-20250219".to_string(),
                "prompt-caching-scope-2026-01-05".to_string()
            ]
        );
    }

    #[test]
    fn default_holon_home_uses_home_directory() {
        let _home_guard = EnvVarGuard::set("HOME", "/tmp/holon-home-test");
        assert_eq!(
            default_holon_home(),
            Path::new("/tmp/holon-home-test/.holon")
        );
    }

    #[test]
    fn set_get_and_unset_round_trip_model_default() {
        let mut config = HolonConfigFile::default();
        set_config_key(&mut config, "model.default", "openai-codex/gpt-5.4").unwrap();
        assert_eq!(
            get_config_key(&config, "model.default").unwrap(),
            json!("openai-codex/gpt-5.4")
        );

        unset_config_key(&mut config, "model.default").unwrap();
        assert_eq!(
            get_config_key(&config, "model.default").unwrap(),
            Value::Null
        );
    }

    #[test]
    fn set_get_and_unset_round_trip_runtime_disable_provider_fallback() {
        let mut config = HolonConfigFile::default();
        set_config_key(&mut config, "runtime.disable_provider_fallback", "true").unwrap();
        assert_eq!(
            get_config_key(&config, "runtime.disable_provider_fallback").unwrap(),
            Value::Bool(true)
        );

        unset_config_key(&mut config, "runtime.disable_provider_fallback").unwrap();
        assert_eq!(
            get_config_key(&config, "runtime.disable_provider_fallback").unwrap(),
            Value::Null
        );
    }

    #[test]
    fn set_get_and_unset_round_trip_api_cors_config() {
        let mut config = HolonConfigFile::default();
        set_config_key(&mut config, "api.cors.enabled", "true").unwrap();
        set_config_key(
            &mut config,
            "api.cors.allowed_origins",
            r#"["http://192.168.1.10:5173"]"#,
        )
        .unwrap();
        set_config_key(&mut config, "api.cors.allowed_methods", r#"["GET","POST"]"#).unwrap();
        set_config_key(
            &mut config,
            "api.cors.allowed_headers",
            r#"["content-type","authorization"]"#,
        )
        .unwrap();
        set_config_key(&mut config, "api.cors.allow_credentials", "false").unwrap();
        set_config_key(&mut config, "api.cors.max_age_seconds", "120").unwrap();

        assert_eq!(
            get_config_key(&config, "api.cors.enabled").unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allowed_origins").unwrap(),
            json!(["http://192.168.1.10:5173"])
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allowed_methods").unwrap(),
            json!(["GET", "POST"])
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allowed_headers").unwrap(),
            json!(["content-type", "authorization"])
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allow_credentials").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            get_config_key(&config, "api.cors.max_age_seconds").unwrap(),
            json!(120)
        );

        unset_config_key(&mut config, "api.cors.enabled").unwrap();
        unset_config_key(&mut config, "api.cors.allowed_origins").unwrap();
        unset_config_key(&mut config, "api.cors.allowed_methods").unwrap();
        unset_config_key(&mut config, "api.cors.allowed_headers").unwrap();
        unset_config_key(&mut config, "api.cors.allow_credentials").unwrap();
        unset_config_key(&mut config, "api.cors.max_age_seconds").unwrap();
        assert_eq!(
            get_config_key(&config, "api.cors.enabled").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allowed_origins").unwrap(),
            json!([])
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allowed_methods").unwrap(),
            json!(default_api_cors_allowed_methods())
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allowed_headers").unwrap(),
            json!(default_api_cors_allowed_headers())
        );
        assert_eq!(
            get_config_key(&config, "api.cors.allow_credentials").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "api.cors.max_age_seconds").unwrap(),
            json!(600)
        );
    }

    #[test]
    fn api_cors_rejects_credentials_with_wildcard_origin() {
        let mut config = HolonConfigFile::default();
        set_config_key(&mut config, "api.cors.allowed_origins", r#"["*"]"#).unwrap();

        let error = set_config_key(&mut config, "api.cors.allow_credentials", "true")
            .expect_err("credentials plus wildcard origin should be rejected");

        assert!(
            error.to_string().contains("allow_credentials"),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn set_get_and_unset_round_trip_tool_output_budgets() {
        let mut config = HolonConfigFile::default();
        set_config_key(&mut config, "runtime.default_tool_output_tokens", "1500").unwrap();
        set_config_key(&mut config, "runtime.max_tool_output_tokens", "6000").unwrap();

        assert_eq!(
            get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
            json!(1_500)
        );
        assert_eq!(
            get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
            json!(6_000)
        );

        set_config_key(&mut config, "runtime.default_tool_output_tokens", "100000").unwrap();
        set_config_key(&mut config, "runtime.max_tool_output_tokens", "100000").unwrap();
        assert_eq!(
            get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
            json!(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS)
        );
        assert_eq!(
            get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
            json!(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS)
        );

        unset_config_key(&mut config, "runtime.default_tool_output_tokens").unwrap();
        unset_config_key(&mut config, "runtime.max_tool_output_tokens").unwrap();
        assert_eq!(
            get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
            Value::Null
        );
    }

    #[test]
    fn set_get_and_unset_round_trip_web_config() {
        let mut config = HolonConfigFile::default();
        set_config_key(&mut config, "web.fetch.enabled", "true").unwrap();
        set_config_key(
            &mut config,
            "web.fetch.allowed_hosts",
            "localhost:3000,127.0.0.1:5173",
        )
        .unwrap();
        set_config_key(&mut config, "web.search.provider", "duckduckgo").unwrap();
        set_config_key(&mut config, "web.search.builtin_provider.enabled", "false").unwrap();
        set_config_key(&mut config, "web.search.max_results", "3").unwrap();
        set_config_key(&mut config, "web.search.mode", "aggregate").unwrap();
        set_config_key(&mut config, "web.search.providers", "searx,brave").unwrap();
        set_config_key(&mut config, "web.search.max_provider_attempts", "2").unwrap();
        set_config_key(&mut config, "web.providers.brave.kind", "brave").unwrap();
        set_config_key(&mut config, "web.fetch.max_response_bytes", "12345").unwrap();
        set_config_key(&mut config, "web.fetch.timeout_seconds", "7").unwrap();
        set_config_key(&mut config, "web.fetch.max_redirects", "2").unwrap();

        assert_eq!(
            get_config_key(&config, "web.fetch.enabled").unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            get_config_key(&config, "web.fetch.allowed_hosts").unwrap(),
            json!(["localhost:3000", "127.0.0.1:5173"])
        );
        assert_eq!(
            get_config_key(&config, "web.search.provider").unwrap(),
            json!("duckduckgo")
        );
        assert_eq!(
            get_config_key(&config, "web.search.builtin_provider.enabled").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            get_config_key(&config, "web.search.max_results").unwrap(),
            json!(3)
        );
        assert_eq!(
            get_config_key(&config, "web.search.mode").unwrap(),
            json!("aggregate")
        );
        assert_eq!(
            get_config_key(&config, "web.search.providers").unwrap(),
            json!(["searx", "brave"])
        );
        assert_eq!(
            get_config_key(&config, "web.search.max_provider_attempts").unwrap(),
            json!(2)
        );
        assert_eq!(
            get_config_key(&config, "web.providers.searx.capabilities").unwrap(),
            Value::Null
        );
        let capabilities = get_config_key(&config, "web.providers.brave.capabilities").unwrap();
        assert_eq!(capabilities["auth"], json!("api_key"));
        assert_eq!(capabilities["status"], json!("supported"));
        assert_eq!(capabilities["default_priority"], json!(80));
        let read_only_capabilities_error =
            "web.providers.brave.capabilities is derived read-only capability metadata; configure web.providers.<name>.kind instead";
        assert_eq!(
            set_config_key(
                &mut config,
                "web.providers.brave.capabilities",
                r#"{"status":"supported"}"#
            )
            .unwrap_err()
            .to_string(),
            read_only_capabilities_error
        );
        assert_eq!(
            unset_config_key(&mut config, "web.providers.brave.capabilities")
                .unwrap_err()
                .to_string(),
            read_only_capabilities_error
        );
        assert_eq!(
            get_config_key(&config, "web.providers.brave.kind").unwrap(),
            json!("brave")
        );
        assert_eq!(
            get_config_key(&config, "web.providers.brave.base_url").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "web.fetch.max_response_bytes").unwrap(),
            json!(12_345)
        );
        assert_eq!(
            get_config_key(&config, "web.fetch.timeout_seconds").unwrap(),
            json!(7)
        );
        assert_eq!(
            get_config_key(&config, "web.fetch.max_redirects").unwrap(),
            json!(2)
        );

        unset_config_key(&mut config, "web.fetch.allowed_hosts").unwrap();
        unset_config_key(&mut config, "web.search.provider").unwrap();
        unset_config_key(&mut config, "web.search.builtin_provider.enabled").unwrap();
        unset_config_key(&mut config, "web.search.mode").unwrap();
        unset_config_key(&mut config, "web.search.providers").unwrap();
        unset_config_key(&mut config, "web.search.max_provider_attempts").unwrap();
        unset_config_key(&mut config, "web.providers.brave").unwrap();
        unset_config_key(&mut config, "web.fetch.max_response_bytes").unwrap();
        unset_config_key(&mut config, "web.fetch.timeout_seconds").unwrap();
        unset_config_key(&mut config, "web.fetch.max_redirects").unwrap();
        assert_eq!(
            get_config_key(&config, "web.fetch.allowed_hosts").unwrap(),
            json!([])
        );
        assert_eq!(
            get_config_key(&config, "web.search.provider").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "web.search.builtin_provider.enabled").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "web.search.mode").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "web.search.providers").unwrap(),
            json!([])
        );
        assert_eq!(
            get_config_key(&config, "web.search.max_provider_attempts").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "web.fetch.max_response_bytes").unwrap(),
            Value::Null
        );
    }

    #[test]
    fn set_get_and_unset_round_trip_unknown_model_fallback_field() {
        let mut config = HolonConfigFile::default();
        set_config_key(
            &mut config,
            "model.unknown_fallback.prompt_budget_estimated_tokens",
            "64000",
        )
        .unwrap();
        set_config_key(
            &mut config,
            "model.unknown_fallback.runtime_max_output_tokens",
            "4096",
        )
        .unwrap();
        assert_eq!(
            get_config_key(
                &config,
                "model.unknown_fallback.prompt_budget_estimated_tokens"
            )
            .unwrap(),
            json!(64_000)
        );
        assert_eq!(
            get_config_key(&config, "model.unknown_fallback.runtime_max_output_tokens").unwrap(),
            json!(4_096)
        );

        unset_config_key(
            &mut config,
            "model.unknown_fallback.prompt_budget_estimated_tokens",
        )
        .unwrap();
        assert_eq!(
            get_config_key(
                &config,
                "model.unknown_fallback.prompt_budget_estimated_tokens"
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "model.unknown_fallback.runtime_max_output_tokens").unwrap(),
            json!(4_096)
        );

        unset_config_key(
            &mut config,
            "model.unknown_fallback.runtime_max_output_tokens",
        )
        .unwrap();
        assert_eq!(
            get_config_key(&config, "model.unknown_fallback").unwrap(),
            Value::Null
        );
    }

    #[test]
    fn built_in_provider_registry_declares_provider_specific_builtin_search() {
        let registry = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();

        let openai_codex = registry.get(&ProviderId::openai_codex()).unwrap();
        let openai_codex_search = openai_codex.builtin_web_search.as_ref().unwrap();
        assert_eq!(
            openai_codex_search.kind,
            ProviderNativeWebSearchKind::OpenAi
        );
        assert_eq!(openai_codex_search.advertised_tool_type, "web_search");
        assert_eq!(openai_codex_search.backend_kind, "openai_codex_web_search");

        let anthropic = registry.get(&ProviderId::anthropic()).unwrap();
        let anthropic_search = anthropic.builtin_web_search.as_ref().unwrap();
        assert_eq!(
            anthropic_search.kind,
            ProviderNativeWebSearchKind::Anthropic
        );
        assert_eq!(anthropic_search.advertised_tool_type, "web_search_20250305");
        assert_eq!(anthropic_search.backend_kind, "anthropic_web_search");

        let zai = registry.get(&ProviderId::parse("zai").unwrap()).unwrap();
        let zai_search = zai.builtin_web_search.as_ref().unwrap();
        assert_eq!(zai_search.kind, ProviderNativeWebSearchKind::Anthropic);
        assert_eq!(zai_search.advertised_tool_type, "web_search_20250305");
        assert_eq!(zai_search.backend_kind, "zai_web_search_prime");

        let deepseek = registry
            .get(&ProviderId::parse("deepseek").unwrap())
            .unwrap();
        let deepseek_search = deepseek.builtin_web_search.as_ref().unwrap();
        assert_eq!(deepseek_search.kind, ProviderNativeWebSearchKind::Anthropic);
        assert_eq!(deepseek_search.advertised_tool_type, "web_search_20250305");
        assert_eq!(deepseek_search.backend_kind, "deepseek_web_search");
    }

    #[test]
    fn set_get_and_unset_round_trip_models_catalog_object() {
        let mut config = HolonConfigFile::default();
        set_config_key(
            &mut config,
            "models.catalog",
            r#"{"anthropic/claude-sonnet-4-6":{"prompt_budget_estimated_tokens":32000,"capabilities":{"image_input":true}}}"#,
        )
        .unwrap();
        assert_eq!(
            get_config_key(&config, "models.catalog").unwrap(),
            json!({
                "anthropic/claude-sonnet-4-6": {
                    "capabilities": {
                        "image_input": true
                    },
                    "prompt_budget_estimated_tokens": 32_000
                }
            })
        );

        unset_config_key(&mut config, "models.catalog").unwrap();
        assert_eq!(
            get_config_key(&config, "models.catalog").unwrap(),
            json!({})
        );
    }

    #[test]
    fn models_catalog_rejects_invalid_model_refs() {
        let mut config = HolonConfigFile::default();
        let err = set_config_key(
            &mut config,
            "models.catalog",
            r#"{"gpt-5.4":{"prompt_budget_estimated_tokens":32000}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("expected provider/model"));
    }

    #[test]
    fn custom_provider_auth_requires_explicit_contract() {
        let id = ProviderId::parse("openrouter").unwrap();
        let auth = ProviderAuthConfig {
            source: CredentialSource::Env,
            kind: CredentialKind::ApiKey,
            env: None,
            profile: None,
            external: None,
        };
        let err = super::validate_provider_auth(&id, &auth).unwrap_err();
        assert!(err.to_string().contains("requires auth.env"));
    }

    #[test]
    fn provider_builtin_web_search_rejects_empty_tool_metadata() {
        let id = ProviderId::parse("custom-anthropic").unwrap();
        let config = ProviderConfigFile {
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: "https://api.example.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("CUSTOM_API_KEY".into()),
                profile: None,
                external: None,
            },
            reasoning_effort: None,
            builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                enabled: true,
                kind: ProviderNativeWebSearchKind::Anthropic,
                advertised_tool_type: String::new(),
                backend_kind: "custom_backend".into(),
            }),
        };

        let err = validate_provider_config(&id, &config).unwrap_err();
        assert!(err.to_string().contains("advertised_tool_type"));
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn provider_builtin_web_search_rejects_transport_kind_mismatch() {
        let id = ProviderId::parse("custom-openai").unwrap();
        let config = ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.example.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("CUSTOM_API_KEY".into()),
                profile: None,
                external: None,
            },
            reasoning_effort: None,
            builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                enabled: true,
                kind: ProviderNativeWebSearchKind::Anthropic,
                advertised_tool_type: "web_search_20250305".into(),
                backend_kind: "custom_backend".into(),
            }),
        };

        let err = validate_provider_config(&id, &config).unwrap_err();
        assert!(err.to_string().contains("incompatible with transport"));
    }

    #[test]
    fn provider_builtin_web_search_rejects_wrong_tool_type() {
        let id = ProviderId::parse("custom-openai").unwrap();
        let config = ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.example.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("CUSTOM_API_KEY".into()),
                profile: None,
                external: None,
            },
            reasoning_effort: None,
            builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                enabled: true,
                kind: ProviderNativeWebSearchKind::OpenAi,
                advertised_tool_type: "web_search_20250305".into(),
                backend_kind: "custom_backend".into(),
            }),
        };

        let err = validate_provider_config(&id, &config).unwrap_err();
        assert!(err.to_string().contains("web_search_preview"));
    }

    #[test]
    fn provider_builtin_web_search_accepts_codex_tool_type() {
        let id = ProviderId::openai_codex();
        let config = ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
            },
            reasoning_effort: None,
            builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                enabled: true,
                kind: ProviderNativeWebSearchKind::OpenAi,
                advertised_tool_type: "web_search".into(),
                backend_kind: "openai_codex_web_search".into(),
            }),
        };

        validate_provider_config(&id, &config).unwrap();
    }

    #[test]
    fn provider_builtin_web_search_rejects_wrong_codex_tool_type() {
        let id = ProviderId::openai_codex();
        let config = ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
            },
            reasoning_effort: None,
            builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                enabled: true,
                kind: ProviderNativeWebSearchKind::OpenAi,
                advertised_tool_type: "web_search_preview".into(),
                backend_kind: "openai_codex_web_search".into(),
            }),
        };

        let err = validate_provider_config(&id, &config).unwrap_err();
        assert!(err.to_string().contains("web_search for OpenAI Codex"));
    }

    #[test]
    fn materialize_provider_config_can_disable_builtin_web_search_for_builtin_provider() {
        let id = ProviderId::openai_codex();
        let built_in = built_in_provider_registry_with_settings(&HashMap::new())
            .unwrap()
            .remove(&id)
            .unwrap();
        assert!(built_in.builtin_web_search.is_some());

        let runtime = super::materialize_provider_config(
            id.clone(),
            ProviderConfigFile {
                transport: ProviderTransportKind::OpenAiCodexResponses,
                base_url: "https://chatgpt.com/backend-api/codex".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::ExternalCli,
                    kind: CredentialKind::SessionToken,
                    env: None,
                    profile: None,
                    external: Some("codex_cli".into()),
                },
                reasoning_effort: None,
                builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                    enabled: false,
                    kind: ProviderNativeWebSearchKind::OpenAi,
                    advertised_tool_type: "web_search".into(),
                    backend_kind: "openai_codex_web_search".into(),
                }),
            },
            &HashMap::new(),
            &CredentialStoreFile::default(),
            Some(built_in),
        )
        .unwrap();

        assert_eq!(runtime.id, id);
        assert!(runtime.builtin_web_search.is_none());
    }

    #[test]
    fn materialize_provider_config_resolves_env_credentials_from_settings() {
        let mut settings_env = HashMap::new();
        settings_env.insert("OPENROUTER_API_KEY".to_string(), "settings-key".to_string());
        let id = ProviderId::parse("openrouter").unwrap();
        let runtime = super::materialize_provider_config(
            id.clone(),
            ProviderConfigFile {
                transport: ProviderTransportKind::OpenAiResponses,
                base_url: "https://openrouter.example/v1".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::Env,
                    kind: CredentialKind::ApiKey,
                    env: Some("OPENROUTER_API_KEY".into()),
                    profile: None,
                    external: None,
                },
                reasoning_effort: None,
                builtin_web_search: None,
            },
            &settings_env,
            &CredentialStoreFile::default(),
            None,
        )
        .unwrap();

        assert_eq!(runtime.id, id);
        assert_eq!(runtime.credential.as_deref(), Some("settings-key"));
    }

    #[test]
    fn credential_source_accepts_credential_profile_alias() {
        assert_eq!(
            CredentialSource::parse("credential_profile").unwrap(),
            CredentialSource::AuthProfile
        );
        assert_eq!(
            CredentialSource::parse("auth_profile").unwrap(),
            CredentialSource::AuthProfile
        );
        assert_eq!(CredentialSource::AuthProfile.as_str(), "credential_profile");
    }

    #[test]
    fn credential_store_lists_profiles_without_raw_material() {
        let dir = tempdir().unwrap();
        let path = credential_store_path(dir.path());

        let status = set_credential_profile_at(
            &path,
            "openai:default",
            CredentialKind::ApiKey,
            "sk-test-value".into(),
        )
        .unwrap();

        assert_eq!(status.profile, "openai:default");
        let profiles = list_credential_profiles_at(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&profiles).unwrap(),
            json!([{
                "profile": "openai:default",
                "kind": "api_key",
                "configured": true
            }])
        );
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("sk-test-value"));
        assert!(!serde_json::to_string(&profiles)
            .unwrap()
            .contains("sk-test-value"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn materialize_provider_config_resolves_credential_profile() {
        let settings_env = HashMap::new();
        let id = ProviderId::parse("openrouter").unwrap();
        let mut credential_store = CredentialStoreFile::default();
        credential_store.profiles.insert(
            "openrouter:default".into(),
            super::CredentialProfileFile {
                kind: CredentialKind::ApiKey,
                material: "profile-value".into(),
            },
        );

        let runtime = super::materialize_provider_config(
            id.clone(),
            ProviderConfigFile {
                transport: ProviderTransportKind::OpenAiResponses,
                base_url: "https://openrouter.example/v1".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::AuthProfile,
                    kind: CredentialKind::ApiKey,
                    env: None,
                    profile: Some(" openrouter:default ".into()),
                    external: None,
                },
                reasoning_effort: None,
                builtin_web_search: None,
            },
            &settings_env,
            &credential_store,
            None,
        )
        .unwrap();

        assert_eq!(runtime.id, id);
        assert_eq!(runtime.credential.as_deref(), Some("profile-value"));
    }

    #[test]
    fn app_config_rejects_bad_credential_store_permissions_when_store_exists() {
        let dir = tempdir().unwrap();
        save_persisted_config_at(
            &persisted_config_path(dir.path()),
            &HolonConfigFile {
                model: ModelConfigFile {
                    default: Some("openai/gpt-5.4".into()),
                    ..ModelConfigFile::default()
                },
                ..HolonConfigFile::default()
            },
        )
        .unwrap();
        let store_path = credential_store_path(dir.path());
        fs::write(&store_path, r#"{"profiles":{}}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&store_path, fs::Permissions::from_mode(0o644)).unwrap();
        }

        #[cfg(unix)]
        {
            let err = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap_err();
            assert!(err.to_string().contains("chmod 600"));
        }
        #[cfg(not(unix))]
        AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();

        let mut config = load_persisted_config_at(&persisted_config_path(dir.path())).unwrap();
        config.providers.insert(
            ProviderId::parse("openrouter").unwrap(),
            ProviderConfigFile {
                transport: ProviderTransportKind::OpenAiResponses,
                base_url: "https://openrouter.example/v1".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::AuthProfile,
                    kind: CredentialKind::ApiKey,
                    env: None,
                    profile: Some("openrouter:default".into()),
                    external: None,
                },
                reasoning_effort: None,
                builtin_web_search: None,
            },
        );
        save_persisted_config_at(&persisted_config_path(dir.path()), &config).unwrap();

        #[cfg(unix)]
        {
            let err = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap_err();
            assert!(err.to_string().contains("chmod 600"));
        }
    }

    #[test]
    fn built_in_provider_registry_includes_compatible_provider_defaults() {
        let _env = EnvVarGuard::unset_many(VERCEL_AI_GATEWAY_ENV_KEYS);
        let mut settings_env = HashMap::new();
        settings_env.insert("OPENROUTER_API_KEY".to_string(), "settings-key".to_string());
        settings_env.insert(
            "HOLON_OPENROUTER_BASE_URL".to_string(),
            "https://openrouter.example/api/v3".to_string(),
        );
        settings_env.insert("DEEPSEEK_API_KEY".to_string(), "deepseek-key".to_string());
        settings_env.insert("XIAOMI_API_KEY".to_string(), "xiaomi-key".to_string());
        settings_env.insert(
            "XIAOMI_TOKEN_PLAN_API_KEY".to_string(),
            "xiaomi-token-plan-key".to_string(),
        );
        settings_env.insert("ZAI_API_KEY".to_string(), "zai-key".to_string());
        settings_env.insert("BIGMODEL_API_KEY".to_string(), "bigmodel-key".to_string());
        settings_env.insert("DASHSCOPE_API_KEY".to_string(), "dashscope-key".to_string());
        settings_env.insert("NEARAI_API_KEY".to_string(), "nearai-key".to_string());
        settings_env.insert("GEMINI_API_KEY".to_string(), "gemini-key".to_string());
        settings_env.insert(
            "HOLON_GEMINI_BASE_URL".to_string(),
            "https://gemini.example/v1beta".to_string(),
        );

        let providers = super::built_in_provider_registry_with_settings(&settings_env).unwrap();

        let openrouter = providers
            .get(&ProviderId::parse("openrouter").unwrap())
            .unwrap();
        assert_eq!(
            openrouter.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(openrouter.base_url, "https://openrouter.example/api/v3");
        assert_eq!(openrouter.auth.env.as_deref(), Some("OPENROUTER_API_KEY"));
        assert_eq!(openrouter.credential.as_deref(), Some("settings-key"));

        let gemini = providers.get(&ProviderId::gemini()).unwrap();
        assert_eq!(
            gemini.transport,
            ProviderTransportKind::GeminiGenerateContent
        );
        assert_eq!(gemini.base_url, "https://gemini.example/v1beta");
        assert_eq!(gemini.auth.env.as_deref(), Some("GEMINI_API_KEY"));
        assert_eq!(gemini.credential.as_deref(), Some("gemini-key"));

        assert!(providers.get(&ProviderId::parse("qwen").unwrap()).is_none());

        let dashscope = providers
            .get(&ProviderId::parse("dashscope").unwrap())
            .unwrap();
        assert_eq!(
            dashscope.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            dashscope.base_url,
            "https://dashscope.aliyuncs.com/apps/anthropic"
        );
        assert_eq!(dashscope.auth.env.as_deref(), Some("DASHSCOPE_API_KEY"));
        assert_eq!(dashscope.credential.as_deref(), Some("dashscope-key"));
        assert_eq!(
            dashscope.context_management.cache_strategy,
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );

        let nearai = providers
            .get(&ProviderId::parse("nearai").unwrap())
            .unwrap();
        assert_eq!(
            nearai.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(nearai.base_url, "https://cloud-api.near.ai/v1");
        assert_eq!(nearai.auth.env.as_deref(), Some("NEARAI_API_KEY"));
        assert_eq!(nearai.credential.as_deref(), Some("nearai-key"));

        let stepfun_plan = providers
            .get(&ProviderId::parse("stepfun-plan").unwrap())
            .unwrap();
        assert_eq!(
            stepfun_plan.auth.env.as_deref(),
            Some("STEPFUN_PLAN_API_KEY or STEPFUN_API_KEY")
        );

        let deepseek = providers
            .get(&ProviderId::parse("deepseek").unwrap())
            .unwrap();
        assert_eq!(deepseek.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(deepseek.base_url, "https://api.deepseek.com/anthropic");
        assert_eq!(deepseek.credential.as_deref(), Some("deepseek-key"));

        let xiaomi = providers
            .get(&ProviderId::parse("xiaomi").unwrap())
            .unwrap();
        assert_eq!(
            xiaomi.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(xiaomi.base_url, "https://api.xiaomimimo.com/v1");
        assert_eq!(xiaomi.credential.as_deref(), Some("xiaomi-key"));

        let xiaomi_token_plan = providers
            .get(&ProviderId::parse("xiaomi-token-plan").unwrap())
            .unwrap();
        assert_eq!(
            xiaomi_token_plan.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(
            xiaomi_token_plan.base_url,
            "https://token-plan-cn.xiaomimimo.com/v1"
        );
        assert_eq!(
            xiaomi_token_plan.credential.as_deref(),
            Some("xiaomi-token-plan-key")
        );

        let zai = providers.get(&ProviderId::parse("zai").unwrap()).unwrap();
        assert_eq!(zai.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(zai.base_url, "https://api.z.ai/api/anthropic");
        assert_eq!(zai.credential.as_deref(), Some("zai-key"));

        let bigmodel = providers
            .get(&ProviderId::parse("bigmodel").unwrap())
            .unwrap();
        assert_eq!(bigmodel.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(bigmodel.base_url, "https://open.bigmodel.cn/api/anthropic");
        assert_eq!(bigmodel.credential.as_deref(), Some("bigmodel-key"));

        let minimax = providers
            .get(&ProviderId::parse("minimax").unwrap())
            .unwrap();
        assert_eq!(minimax.transport, ProviderTransportKind::AnthropicMessages);

        let synthetic = providers
            .get(&ProviderId::parse("synthetic").unwrap())
            .unwrap();
        assert_eq!(
            synthetic.transport,
            ProviderTransportKind::AnthropicMessages
        );

        let tokenhub = providers
            .get(&ProviderId::parse("tencent-tokenhub").unwrap())
            .unwrap();
        assert_eq!(
            tokenhub.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );

        let vllm = providers.get(&ProviderId::parse("vllm").unwrap()).unwrap();
        assert_eq!(vllm.auth.source, CredentialSource::None);
        assert_eq!(vllm.credential, None);

        let vercel_ai_gateway = providers
            .get(&ProviderId::parse("vercel-ai-gateway").unwrap())
            .unwrap();
        assert_eq!(
            vercel_ai_gateway.auth.env.as_deref(),
            Some("VERCEL_OIDC_TOKEN or AI_GATEWAY_API_KEY or VERCEL_AI_GATEWAY_API_KEY")
        );
        assert_eq!(vercel_ai_gateway.auth.kind, CredentialKind::BearerToken);
        assert_eq!(vercel_ai_gateway.credential, None);

        for provider in [
            "deepseek",
            "zai",
            "bigmodel",
            "minimax",
            "synthetic",
            "vercel-ai-gateway",
        ] {
            let config = providers
                .get(&ProviderId::parse(provider).unwrap())
                .unwrap();
            assert_eq!(
                config.context_management.cache_strategy,
                AnthropicCacheStrategy::ClaudeCodePromptCache,
                "{provider} should use Claude Code-like prompt-cache lowering by default"
            );
            assert!(
                config.context_management.betas.is_empty(),
                "{provider} should not auto-inject Claude-specific betas"
            );
        }
    }

    #[test]
    fn vercel_ai_gateway_prefers_oidc_bearer_token_over_api_key() {
        let mut settings_env = HashMap::new();
        settings_env.insert("VERCEL_OIDC_TOKEN".to_string(), "oidc-token".to_string());
        settings_env.insert("AI_GATEWAY_API_KEY".to_string(), "api-key".to_string());

        let providers = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
        let vercel_ai_gateway = providers
            .get(&ProviderId::parse("vercel-ai-gateway").unwrap())
            .unwrap();

        assert_eq!(
            vercel_ai_gateway.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(vercel_ai_gateway.auth.source, CredentialSource::Env);
        assert_eq!(vercel_ai_gateway.auth.kind, CredentialKind::BearerToken);
        assert_eq!(
            vercel_ai_gateway.auth.env.as_deref(),
            Some("VERCEL_OIDC_TOKEN")
        );
        assert_eq!(vercel_ai_gateway.credential.as_deref(), Some("oidc-token"));
    }

    #[test]
    fn vercel_ai_gateway_falls_back_to_api_key_auth() {
        let _env = EnvVarGuard::unset_many(VERCEL_AI_GATEWAY_ENV_KEYS);
        let mut settings_env = HashMap::new();
        settings_env.insert("AI_GATEWAY_API_KEY".to_string(), "api-key".to_string());

        let providers = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
        let vercel_ai_gateway = providers
            .get(&ProviderId::parse("vercel-ai-gateway").unwrap())
            .unwrap();

        assert_eq!(vercel_ai_gateway.auth.source, CredentialSource::Env);
        assert_eq!(vercel_ai_gateway.auth.kind, CredentialKind::ApiKey);
        assert_eq!(
            vercel_ai_gateway.auth.env.as_deref(),
            Some("AI_GATEWAY_API_KEY")
        );
        assert_eq!(vercel_ai_gateway.credential.as_deref(), Some("api-key"));
    }

    #[test]
    fn built_in_provider_default_config_resolves_known_and_unknown_provider() {
        let settings_env = HashMap::new();

        let known = super::built_in_provider_default_config_with_settings(
            &ProviderId::parse("zai").unwrap(),
            &settings_env,
        )
        .unwrap()
        .expect("expected built-in default");
        assert_eq!(known.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(known.base_url, "https://api.z.ai/api/anthropic");
        assert_eq!(known.auth.source, CredentialSource::None);
        assert_eq!(known.auth.kind, CredentialKind::None);
        assert!(known.auth.env.is_none());

        let unknown = super::built_in_provider_default_config_with_settings(
            &ProviderId::parse("unknown-provider").unwrap(),
            &settings_env,
        )
        .unwrap();
        assert!(unknown.is_none());
    }

    #[test]
    fn materialize_provider_config_preserves_builtin_runtime_fields() {
        let settings_env = HashMap::new();
        let mut built_ins = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
        let id = ProviderId::openai_codex();
        let built_in = built_ins.remove(&id).unwrap();

        let runtime = super::materialize_provider_config(
            id,
            ProviderConfigFile {
                transport: ProviderTransportKind::OpenAiCodexResponses,
                base_url: "https://codex.example/backend-api".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::ExternalCli,
                    kind: CredentialKind::SessionToken,
                    env: None,
                    profile: None,
                    external: Some("codex_cli".into()),
                },
                reasoning_effort: None,
                builtin_web_search: None,
            },
            &settings_env,
            &CredentialStoreFile::default(),
            Some(built_in),
        )
        .unwrap();

        assert_eq!(runtime.base_url, "https://codex.example/backend-api");
        assert!(runtime.codex_home.is_some());
        assert_eq!(runtime.originator.as_deref(), Some("codex_cli_rs"));
        assert_eq!(runtime.reasoning_effort.as_deref(), Some("low"));
    }

    #[test]
    fn model_selection_explicit_config_wins() {
        let providers = provider_registry_for_tests(
            Some("openai-key"),
            Some("anthropic-token"),
            tempdir().unwrap().path().join("codex-home"),
        );

        let (default_model, fallback_models) = super::resolve_model_selection_from_explicit(
            Some(ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap()),
            Some(vec![
                ModelRef::parse("openai/gpt-5.4").unwrap(),
                ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            ]),
            &providers,
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(default_model.as_string(), "anthropic/claude-sonnet-4-6");
        assert_eq!(
            fallback_models
                .into_iter()
                .map(|model| model.as_string())
                .collect::<Vec<_>>(),
            vec!["openai/gpt-5.4"]
        );
    }

    #[test]
    fn model_selection_derives_from_authenticated_providers() {
        let providers = provider_registry_for_tests(
            Some("openai-key"),
            Some("anthropic-token"),
            tempdir().unwrap().path().join("codex-home"),
        );

        let (default_model, fallback_models) =
            super::resolve_model_selection_from_explicit(None, None, &providers, &HashMap::new())
                .unwrap();

        assert_eq!(default_model.as_string(), "openai/gpt-5.4");
        assert_eq!(
            fallback_models
                .into_iter()
                .map(|model| model.as_string())
                .collect::<Vec<_>>(),
            vec!["anthropic/claude-sonnet-4-6"]
        );
    }

    #[test]
    fn model_selection_fails_without_explicit_config_or_auth() {
        let providers =
            provider_registry_for_tests(None, None, tempdir().unwrap().path().join("codex-home"));

        let err =
            super::resolve_model_selection_from_explicit(None, None, &providers, &HashMap::new())
                .unwrap_err();

        assert!(err.to_string().contains("no default model configured"));
        assert!(err.to_string().contains("configure provider credentials"));
    }

    #[test]
    fn config_inspection_loads_provider_state_without_resolved_model() {
        let home = tempdir().unwrap();
        let codex_home = home.path().join("codex-home");
        let _env_guard = EnvVarGuard::set_and_unset(
            &[
                ("HOME", home.path().as_os_str()),
                ("CODEX_HOME", codex_home.as_os_str()),
            ],
            &[
                "HOLON_MODEL",
                "HOLON_MODEL_FALLBACKS",
                "OPENAI_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ARCEE_API_KEY",
                "BYTEPLUS_API_KEY",
                "BYTEPLUS_CODING_API_KEY",
                "CHUTES_API_KEY",
                "DEEPSEEK_API_KEY",
                "FIREWORKS_API_KEY",
                "HUGGINGFACE_API_KEY",
                "HF_TOKEN",
                "KILOCODE_API_KEY",
                "LITELLM_API_KEY",
                "MISTRAL_API_KEY",
                "MOONSHOT_API_KEY",
                "NEARAI_API_KEY",
                "NVIDIA_API_KEY",
                "OPENCODE_GO_API_KEY",
                "OPENROUTER_API_KEY",
                "QIANFAN_API_KEY",
                "QWEN_API_KEY",
                "DASHSCOPE_API_KEY",
                "STEPFUN_API_KEY",
                "STEPFUN_PLAN_API_KEY",
                "SYNTHETIC_API_KEY",
                "TOKENHUB_API_KEY",
                "TOGETHER_API_KEY",
                "VENICE_API_KEY",
                "VOLCENGINE_API_KEY",
                "VOLCENGINE_CODING_API_KEY",
                "ARK_API_KEY",
                "XIAOMI_API_KEY",
                "XIAOMI_TOKEN_PLAN_API_KEY",
                "XAI_API_KEY",
                "ZAI_API_KEY",
                "BIGMODEL_API_KEY",
                "MINIMAX_API_KEY",
                "AI_GATEWAY_API_KEY",
                "VERCEL_AI_GATEWAY_API_KEY",
                "HOLON_TEST_MISSING_CUSTOM_OPENAI_API_KEY",
            ],
        );
        let provider_id = ProviderId::parse("custom-openai").unwrap();
        save_persisted_config_at(
            &persisted_config_path(home.path()),
            &HolonConfigFile {
                providers: BTreeMap::from([(
                    provider_id.clone(),
                    ProviderConfigFile {
                        transport: ProviderTransportKind::OpenAiChatCompletions,
                        base_url: "https://custom.example/v1".into(),
                        auth: ProviderAuthConfig {
                            source: CredentialSource::Env,
                            kind: CredentialKind::ApiKey,
                            env: Some("HOLON_TEST_MISSING_CUSTOM_OPENAI_API_KEY".into()),
                            profile: None,
                            external: None,
                        },
                        reasoning_effort: None,
                        builtin_web_search: None,
                    },
                )]),
                ..HolonConfigFile::default()
            },
        )
        .unwrap();

        let runtime_error = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap_err();
        assert!(runtime_error
            .to_string()
            .contains("no default model configured"));

        let config =
            AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf()))
                .unwrap();
        let provider = config.providers.get(&provider_id).unwrap();
        let view = super::provider_config_view(&config, provider);

        assert_eq!(view.id, "custom-openai");
        assert!(view.configured_in_config);
        assert!(!view.credential_configured);
        assert_eq!(config.default_model.as_string(), "openai/unknown");
    }

    #[test]
    fn model_selection_derives_custom_provider_from_catalog_override() {
        let mut providers = ProviderRegistry::new();
        let id = ProviderId::parse("custom-openai").unwrap();
        providers.insert(
            id.clone(),
            ProviderRuntimeConfig {
                id: id.clone(),
                transport: ProviderTransportKind::OpenAiChatCompletions,
                base_url: "https://custom.example/v1".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::Env,
                    kind: CredentialKind::ApiKey,
                    env: Some("CUSTOM_API_KEY".into()),
                    profile: None,
                    external: None,
                },
                credential: Some("custom-key".into()),
                credential_store_path: None,
                codex_home: None,
                originator: None,
                reasoning_effort: None,
                context_management: Default::default(),
                builtin_web_search: None,
            },
        );
        let mut overrides = HashMap::new();
        overrides.insert(
            ModelRef::parse("custom-openai/model-b").unwrap(),
            ModelRuntimeOverride::default(),
        );
        overrides.insert(
            ModelRef::parse("custom-openai/model-a").unwrap(),
            ModelRuntimeOverride::default(),
        );

        let (default_model, fallback_models) =
            super::resolve_model_selection_from_explicit(None, None, &providers, &overrides)
                .unwrap();

        assert_eq!(default_model.as_string(), "custom-openai/model-a");
        assert!(fallback_models.is_empty());
    }

    #[test]
    fn url_parser_rejects_invalid_or_non_http_urls() {
        let err = parse_url_value("providers.openai.base_url", "").unwrap_err();
        assert!(err.to_string().contains("non-empty URL"));

        let err = parse_url_value("providers.openai.base_url", "notaurl").unwrap_err();
        assert!(err.to_string().contains("valid absolute URL"));

        let err = parse_url_value("providers.openai.base_url", "ftp://example.com").unwrap_err();
        assert!(err.to_string().contains("http or https URL"));

        parse_url_value("providers.openai.base_url", "https://api.openai.com/v1").unwrap();
    }

    #[test]
    fn model_ref_requires_provider_prefix() {
        let err = ModelRef::parse("gpt-5.4").unwrap_err();
        assert!(err.to_string().contains("expected provider/model"));
    }

    #[test]
    fn model_ref_parses_provider_and_model() {
        let parsed = ModelRef::parse("openai-codex/gpt-5.4").unwrap();
        assert_eq!(parsed.provider, ProviderId::openai_codex());
        assert_eq!(parsed.model, "gpt-5.4");
        assert_eq!(parsed.as_string(), "openai-codex/gpt-5.4");
    }

    #[test]
    fn provider_id_accepts_custom_values() {
        let parsed = ProviderId::parse("vertex").unwrap();
        assert_eq!(parsed.as_str(), "vertex");
    }

    #[test]
    fn provider_chain_preserves_effective_order_and_deduplicates() {
        let fixture = test_app_config(
            "openai/gpt-5.4",
            &[
                "openai/gpt-5.4",
                "anthropic/claude-sonnet-4-6",
                "openai-codex/gpt-5.4",
                "anthropic/claude-sonnet-4-6",
            ],
        );

        let chain = fixture
            .config
            .provider_chain()
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>();
        assert_eq!(
            chain,
            vec![
                "openai/gpt-5.4",
                "anthropic/claude-sonnet-4-6",
                "openai-codex/gpt-5.4",
            ]
        );
    }

    #[test]
    fn provider_chain_returns_only_effective_model_when_fallback_disabled() {
        let mut fixture = test_app_config(
            "openai/gpt-5.4",
            &["anthropic/claude-sonnet-4-6", "openai-codex/gpt-5.4"],
        );
        fixture
            .config
            .stored_config
            .runtime
            .disable_provider_fallback = Some(true);
        fixture.config.disable_provider_fallback = true;

        let chain = fixture
            .config
            .provider_chain()
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>();

        assert_eq!(chain, vec!["openai/gpt-5.4"]);
    }

    #[test]
    fn provider_chain_with_override_inserts_override_before_runtime_default() {
        let fixture = test_app_config(
            "anthropic/claude-sonnet-4-6",
            &["openai/gpt-5.4", "anthropic/claude-sonnet-4-6"],
        );

        let override_model = ModelRef::parse("openai/gpt-5.4-mini").unwrap();
        let chain = fixture
            .config
            .provider_chain_with_override(Some(&override_model))
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>();

        assert_eq!(
            chain,
            vec![
                "openai/gpt-5.4-mini",
                "anthropic/claude-sonnet-4-6",
                "openai/gpt-5.4",
            ]
        );
    }

    #[test]
    fn provider_chain_for_turn_starts_at_pending_fallback_model() {
        let fixture = test_app_config(
            "openai/gpt-5.4",
            &[
                "anthropic/claude-sonnet-4-6",
                "openai-codex/gpt-5.3-codex-spark",
            ],
        );
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);
        let pending = ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap();

        let chain = catalog
            .provider_chain_for_turn(None, Some(&pending))
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>();

        assert_eq!(
            chain,
            vec![
                "anthropic/claude-sonnet-4-6",
                "openai-codex/gpt-5.3-codex-spark",
            ]
        );
    }

    #[test]
    fn provider_chain_with_override_returns_only_override_when_fallback_disabled() {
        let mut fixture = test_app_config(
            "anthropic/claude-sonnet-4-6",
            &["openai/gpt-5.4", "openai-codex/gpt-5.4"],
        );
        fixture
            .config
            .stored_config
            .runtime
            .disable_provider_fallback = Some(true);
        fixture.config.disable_provider_fallback = true;

        let override_model = ModelRef::parse("openai/gpt-5.4-mini").unwrap();
        let chain = fixture
            .config
            .provider_chain_with_override(Some(&override_model))
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>();

        assert_eq!(chain, vec!["openai/gpt-5.4-mini"]);
    }

    #[test]
    fn model_ref_serializes_as_string() {
        let model_ref = ModelRef::parse("openai/gpt-5.4").unwrap();
        let encoded = serde_json::to_value(&model_ref).unwrap();
        assert_eq!(encoded, json!("openai/gpt-5.4"));

        let decoded: ModelRef = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, model_ref);
    }

    #[test]
    fn persisted_config_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = HolonConfigFile::default();
        set_config_key(&mut config, "model.default", "openai/gpt-5.4").unwrap();

        save_persisted_config_at(&path, &config).unwrap();
        let loaded = load_persisted_config_at(&path).unwrap();
        assert_eq!(
            get_config_key(&loaded, "model.default").unwrap(),
            json!("openai/gpt-5.4")
        );
    }

    #[test]
    fn schema_contains_expected_keys() {
        let keys = config_schema()
            .into_iter()
            .map(|entry| entry.key)
            .collect::<Vec<_>>();
        assert!(keys.contains(&"model.default"));
        assert!(keys.contains(&"models.catalog"));
        assert!(keys.contains(&"model.unknown_fallback"));
        assert!(!keys.contains(&"providers.openai-codex.auth_source"));
        assert!(keys.contains(&"runtime.max_output_tokens"));
        assert!(keys.contains(&"runtime.default_tool_output_tokens"));
        assert!(keys.contains(&"runtime.max_tool_output_tokens"));
        assert!(keys.contains(&"runtime.disable_provider_fallback"));
        assert!(keys.contains(&"tui.alternate_screen"));
        assert!(keys.contains(&"web.fetch.enabled"));
        assert!(keys.contains(&"web.fetch.allowed_hosts"));
        assert!(keys.contains(&"web.fetch.max_response_bytes"));
        assert!(keys.contains(&"web.fetch.timeout_seconds"));
        assert!(keys.contains(&"web.fetch.max_redirects"));
        assert!(keys.contains(&"web.search.provider"));
        assert!(keys.contains(&"web.providers.<name>.capabilities"));
    }

    #[test]
    fn load_persisted_config_reads_provider_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let legacy_payload = serde_json::json!({
            "model": {
                "default": "anthropic/claude-sonnet-4-6"
            },
            "runtime": {
                "max_output_tokens": 4096
            },
            "providers": {
                "openai": {
                    "transport": "openai_responses",
                    "base_url": "https://example.openai.com/v1",
                    "auth": {
                        "source": "env",
                        "kind": "api_key",
                        "env": "OPENAI_API_KEY"
                    }
                }
            },
            "tui": {
                "alternate_screen": "never"
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy_payload).unwrap()).unwrap();

        let config = load_persisted_config_at(&path).unwrap();
        assert_eq!(
            config.model.default,
            Some("anthropic/claude-sonnet-4-6".to_string())
        );
        assert_eq!(config.runtime.max_output_tokens, Some(4_096));
        assert_eq!(
            config.tui.alternate_screen,
            Some(crate::config::AltScreenMode::Never)
        );
        assert_eq!(
            config
                .providers
                .get(&ProviderId::openai())
                .unwrap()
                .base_url,
            "https://example.openai.com/v1"
        );
    }

    #[test]
    fn unknown_provider_subkeys_report_unknown_key() {
        let config = HolonConfigFile::default();
        let err = get_config_key(&config, "providers.openai.auth_source").unwrap_err();
        assert!(err.to_string().contains("unknown config key"));
    }

    #[test]
    fn resolve_disable_provider_fallback_rejects_invalid_env_override() {
        let err = super::resolve_disable_provider_fallback_override(
            Some("maybe"),
            &HolonConfigFile::default(),
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("HOLON_DISABLE_PROVIDER_FALLBACK expects a boolean"));
    }

    #[test]
    fn runtime_model_catalog_resolves_config_override_and_unknown_fallback() {
        let mut fixture = test_app_config("anthropic/claude-sonnet-4-6", &[]);
        let known_override = ModelRuntimeOverride {
            prompt_budget_estimated_tokens: Some(24_000),
            runtime_max_output_tokens: Some(4_096),
            ..ModelRuntimeOverride::default()
        };
        fixture
            .config
            .stored_config
            .models
            .catalog
            .insert("anthropic/claude-sonnet-4-6".into(), known_override.clone());
        fixture.config.validated_model_overrides.insert(
            ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            known_override,
        );
        let unknown_fallback = ModelRuntimeOverride {
            prompt_budget_estimated_tokens: Some(12_000),
            compaction_trigger_estimated_tokens: Some(10_000),
            ..ModelRuntimeOverride::default()
        };
        fixture.config.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
        fixture.config.validated_unknown_model_fallback = Some(unknown_fallback);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);
        let base_context = ContextConfig {
            recent_messages: fixture.config.context_window_messages,
            recent_briefs: fixture.config.context_window_briefs,
            compaction_trigger_messages: fixture.config.compaction_trigger_messages,
            compaction_keep_recent_messages: fixture.config.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: fixture.config.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: fixture.config.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: fixture
                .config
                .compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: fixture.config.recent_episode_candidates,
            max_relevant_episodes: fixture.config.max_relevant_episodes,
            ..ContextConfig::default()
        };

        let known = catalog.resolved_model_policy(&base_context, None);
        assert_eq!(known.prompt_budget_estimated_tokens, 24_000);
        assert_eq!(known.runtime_max_output_tokens, 4_096);

        let unknown = catalog.resolved_model_policy(
            &base_context,
            Some(&ModelRef::parse("openai/custom-model").unwrap()),
        );
        assert_eq!(unknown.prompt_budget_estimated_tokens, 12_000);
        assert_eq!(unknown.compaction_trigger_estimated_tokens, 10_000);
        assert_eq!(
            unknown.source,
            crate::model_catalog::ModelMetadataSource::UnknownFallback
        );
    }

    #[test]
    fn view_image_vision_selection_uses_primary_when_image_capable() {
        let mut fixture = test_app_config("openai/gpt-5.4", &["anthropic/claude-sonnet-4-6"]);
        fixture.config.vision_candidate_models = vec![ModelRef::parse("openai/gpt-5.4").unwrap()];
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::NativeImageWithObservation
        );
        assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
        assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            selection.selection_reason,
            "current_primary_model_supports_image_input"
        );
    }

    #[test]
    fn view_image_vision_selection_uses_explicit_vision_model() {
        let mut fixture = test_app_config("arcee/trinity-mini", &["openai/gpt-5.4-mini"]);
        fixture.config.vision_model = Some(ModelRef::parse("openai/gpt-5.4").unwrap());
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::VisionAdapter
        );
        assert_eq!(selection.primary_provider.as_deref(), Some("arcee"));
        assert_eq!(selection.primary_model.as_deref(), Some("trinity-mini"));
        assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
        assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            selection.selection_reason,
            "explicit_vision_model_supports_image_input"
        );
        assert_eq!(selection.candidates.len(), 1);
        assert!(selection.candidates[0].image_input);
    }

    #[test]
    fn view_image_vision_selection_auto_discovers_authenticated_adapter() {
        let mut fixture = test_app_config("arcee/trinity-mini", &[]);
        fixture.config.vision_candidate_models =
            vec![ModelRef::parse("openai/gpt-5.4-mini").unwrap()];
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::VisionAdapter
        );
        assert_eq!(selection.primary_provider.as_deref(), Some("arcee"));
        assert_eq!(selection.primary_model.as_deref(), Some("trinity-mini"));
        assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
        assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
        assert_eq!(
            selection.selection_reason,
            "auto_discovered_vision_model_supports_image_input"
        );
        assert_eq!(selection.candidates.len(), 2);
        assert!(!selection.candidates[0].image_input);
        assert!(selection.candidates[1].image_input);
    }

    #[test]
    fn view_image_vision_selection_uses_configured_custom_auto_discovery_capabilities() {
        let mut fixture = test_app_config("arcee/trinity-mini", &[]);
        let custom_model = ModelRef::parse("custom-openai/my-vision-model").unwrap();
        fixture.config.vision_candidate_models = vec![custom_model.clone()];
        fixture.config.providers.insert(
            custom_model.provider.clone(),
            ProviderRuntimeConfig {
                id: custom_model.provider.clone(),
                transport: ProviderTransportKind::OpenAiResponses,
                base_url: "https://api.example.com/v1".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::None,
                    kind: CredentialKind::None,
                    env: None,
                    profile: None,
                    external: None,
                },
                credential: None,
                credential_store_path: None,
                codex_home: None,
                originator: None,
                reasoning_effort: None,
                context_management: Default::default(),
                builtin_web_search: None,
            },
        );
        let override_config = ModelRuntimeOverride {
            capabilities: Some(ModelCapabilityOverride {
                image_input: Some(true),
                ..ModelCapabilityOverride::default()
            }),
            ..ModelRuntimeOverride::default()
        };
        fixture
            .config
            .validated_model_overrides
            .insert(custom_model.clone(), override_config.clone());
        fixture
            .config
            .stored_config
            .models
            .catalog
            .insert(custom_model.as_string(), override_config);
        let unknown_fallback = ModelRuntimeOverride {
            capabilities: Some(ModelCapabilityOverride {
                image_input: Some(false),
                ..ModelCapabilityOverride::default()
            }),
            ..ModelRuntimeOverride::default()
        };
        fixture.config.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
        fixture.config.validated_unknown_model_fallback = Some(unknown_fallback);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::VisionAdapter
        );
        assert_eq!(selection.vision_provider.as_deref(), Some("custom-openai"));
        assert_eq!(selection.vision_model.as_deref(), Some("my-vision-model"));
        assert_eq!(
            selection.selection_reason,
            "auto_discovered_vision_model_supports_image_input"
        );
        assert_eq!(selection.candidates.len(), 2);
        assert!(!selection.candidates[0].image_input);
        assert!(selection.candidates[1].image_input);
    }

    #[test]
    fn view_image_vision_selection_uses_anthropic_messages_when_image_capable() {
        let mut fixture = test_app_config("anthropic/claude-sonnet-4-6", &[]);
        fixture.config.vision_candidate_models =
            vec![ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap()];
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::NativeImageWithObservation
        );
        assert_eq!(selection.primary_provider.as_deref(), Some("anthropic"));
        assert_eq!(
            selection.primary_model.as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(selection.vision_provider.as_deref(), Some("anthropic"));
        assert_eq!(selection.vision_model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(
            selection.selection_reason,
            "current_primary_model_supports_image_input"
        );
        assert_eq!(
            selection.candidates[0].reason,
            "model_advertises_image_input"
        );
    }

    #[test]
    fn view_image_vision_selection_skips_image_capable_unsupported_transports() {
        let mut fixture = test_app_config("gemini/gemini-2.5-pro", &[]);
        fixture.config.vision_candidate_models =
            vec![ModelRef::parse("openai/gpt-5.4-mini").unwrap()];
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::VisionAdapter
        );
        assert_eq!(selection.primary_provider.as_deref(), Some("gemini"));
        assert_eq!(selection.primary_model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
        assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
        assert_eq!(
            selection.selection_reason,
            "auto_discovered_vision_model_supports_image_input"
        );
        assert_eq!(selection.candidates.len(), 2);
        assert_eq!(
            selection.candidates[0].reason,
            "provider_transport_unsupported_for_view_image_observation"
        );
        assert_eq!(
            selection.candidates[1].reason,
            "model_advertises_image_input"
        );
    }

    #[test]
    fn view_image_vision_selection_keeps_fallback_chain_as_compatibility_candidates() {
        let fixture = test_app_config("arcee/trinity-mini", &["openai/gpt-5.4-mini"]);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::VisionAdapter
        );
        assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
        assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
        assert_eq!(
            selection.selection_reason,
            "auto_discovered_vision_model_supports_image_input"
        );
    }

    #[test]
    fn view_image_vision_selection_reports_unavailable_without_image_capable_model() {
        let fixture = test_app_config("arcee/trinity-mini", &["arcee/trinity-large-preview"]);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::Unavailable
        );
        assert_eq!(
            selection.selection_reason,
            "no_configured_model_supports_view_image_observation"
        );
        assert!(selection.vision_provider.is_none());
        assert_eq!(selection.candidates.len(), 2);
        assert!(selection
            .candidates
            .iter()
            .all(|candidate| !candidate.image_input));
    }

    #[test]
    fn view_image_vision_selection_prefers_primary_over_other_candidates() {
        // Primary (openai/gpt-5.4) supports image_input. vision_candidate_models contains
        // a different image-capable model (anthropic/claude-sonnet-4-6). The primary should
        // be selected first because it is tried before other candidates.
        let mut fixture = test_app_config("openai/gpt-5.4", &["anthropic/claude-sonnet-4-6"]);
        fixture.config.vision_candidate_models = vec![
            ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            ModelRef::parse("openai/gpt-5.4").unwrap(),
        ];
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);

        let selection =
            catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

        assert_eq!(
            selection.selected_mode,
            crate::types::ViewImageSelectedMode::NativeImageWithObservation
        );
        assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
        assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            selection.selection_reason,
            "current_primary_model_supports_image_input"
        );
        // Primary appears first in candidates
        assert_eq!(selection.candidates[0].provider, "openai");
        assert_eq!(selection.candidates[0].model, "gpt-5.4");
    }

    #[test]
    fn runtime_model_catalog_uses_discovery_cache_between_overrides_and_builtins() {
        let mut fixture = test_app_config("openrouter/anthropic/claude-3.5-sonnet", &[]);
        let remote_model_ref = ModelRef::parse("openrouter/anthropic/claude-3.5-sonnet").unwrap();
        fixture.config.model_discovery_cache.providers.insert(
            ProviderId::parse("openrouter").unwrap(),
            ProviderModelDiscoveryCache {
                provider: ProviderId::parse("openrouter").unwrap(),
                fetched_at: chrono::Utc::now(),
                source_url: Some("https://openrouter.ai/api/v1/models".into()),
                response_hash: Some("sha256:test".into()),
                models: vec![BuiltInModelMetadata {
                    model_ref: remote_model_ref.clone(),
                    display_name: "Remote Claude".into(),
                    description: "Remote discovered model.".into(),
                    context_window_tokens: Some(123_456),
                    effective_context_window_percent: 95,
                    auto_compact_token_limit: None,
                    default_max_output_tokens: Some(7_777),
                    max_output_tokens_upper_limit: Some(7_777),
                    default_verbosity: None,
                    tool_output_truncation_estimated_tokens: None,
                    capabilities: Default::default(),
                    source: ModelMetadataSource::RemoteDiscovered,
                }],
            },
        );
        let unknown_fallback = ModelRuntimeOverride {
            prompt_budget_estimated_tokens: Some(12_000),
            ..ModelRuntimeOverride::default()
        };
        fixture.config.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
        fixture.config.validated_unknown_model_fallback = Some(unknown_fallback);

        let catalog = RuntimeModelCatalog::from_config(&fixture.config);
        let base_context = ContextConfig {
            recent_messages: fixture.config.context_window_messages,
            recent_briefs: fixture.config.context_window_briefs,
            compaction_trigger_messages: fixture.config.compaction_trigger_messages,
            compaction_keep_recent_messages: fixture.config.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: fixture.config.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: fixture.config.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: fixture
                .config
                .compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: fixture.config.recent_episode_candidates,
            max_relevant_episodes: fixture.config.max_relevant_episodes,
            ..ContextConfig::default()
        };

        let remote = catalog.resolved_model_policy(&base_context, None);
        assert_eq!(remote.source, ModelMetadataSource::RemoteDiscovered);
        assert_eq!(remote.prompt_budget_estimated_tokens, 117_283);
        assert_eq!(remote.runtime_max_output_tokens, 7_777);

        let available = catalog.available_models();
        let listed = available
            .iter()
            .find(|model| model.model_ref == remote_model_ref)
            .expect("remote model should be listed");
        assert_eq!(listed.source, ModelMetadataSource::RemoteDiscovered);

        let override_config = ModelRuntimeOverride {
            display_name: Some("Configured Claude".into()),
            prompt_budget_estimated_tokens: Some(42_000),
            ..ModelRuntimeOverride::default()
        };
        fixture
            .config
            .validated_model_overrides
            .insert(remote_model_ref.clone(), override_config.clone());
        fixture
            .config
            .stored_config
            .models
            .catalog
            .insert(remote_model_ref.as_string(), override_config);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);
        let overridden = catalog.resolved_model_policy(&base_context, None);
        assert_eq!(overridden.source, ModelMetadataSource::ConfigOverride);
        assert_eq!(overridden.display_name, "Configured Claude");
        assert_eq!(overridden.prompt_budget_estimated_tokens, 42_000);
    }

    #[test]
    fn runtime_model_catalog_resolves_codex_spark_to_builtin_budget() {
        let fixture = test_app_config("openai-codex/gpt-5.3-codex-spark", &[]);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);
        let base_context = ContextConfig {
            recent_messages: fixture.config.context_window_messages,
            recent_briefs: fixture.config.context_window_briefs,
            compaction_trigger_messages: fixture.config.compaction_trigger_messages,
            compaction_keep_recent_messages: fixture.config.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: fixture.config.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: fixture.config.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: fixture
                .config
                .compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: fixture.config.recent_episode_candidates,
            max_relevant_episodes: fixture.config.max_relevant_episodes,
            ..ContextConfig::default()
        };

        let resolved = catalog.resolved_model_policy(&base_context, None);
        assert_eq!(
            resolved.model_ref.as_string(),
            "openai-codex/gpt-5.3-codex-spark"
        );
        assert_eq!(resolved.prompt_budget_estimated_tokens, 121_600);
        assert_eq!(resolved.compaction_trigger_estimated_tokens, 109_440);
        assert_eq!(resolved.compaction_keep_recent_estimated_tokens, 41_587);
        assert_eq!(
            resolved.source,
            crate::model_catalog::ModelMetadataSource::BuiltInCatalog
        );
    }

    #[test]
    fn invalid_model_override_percent_is_rejected() {
        let mut config = HolonConfigFile::default();
        let err = set_config_key(
            &mut config,
            "models.catalog",
            r#"{"anthropic/claude-sonnet-4-6":{"effective_context_window_percent":0}}"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("effective_context_window_percent expects an integer from 1 to 100"));
    }

    #[test]
    fn provider_doc_entries_are_sorted_and_populated() {
        let entries =
            built_in_provider_doc_entries().expect("built_in_provider_doc_entries should succeed");
        assert!(!entries.is_empty(), "should have at least one provider");
        // Verify entries are sorted by id
        for i in 1..entries.len() {
            assert!(
                entries[i - 1].id.as_str() <= entries[i].id.as_str(),
                "entries must be sorted by id"
            );
        }
    }
}
