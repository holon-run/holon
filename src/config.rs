use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};

use crate::{
    context::ContextConfig,
    model_catalog::{
        BuiltInModelCatalog, BuiltInModelMetadata, ModelRuntimeOverride, ResolvedRuntimeModelPolicy,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAuthMode {
    Auto,
    Required,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlTransportKind {
    Tcp,
    Unix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Env,
    ExternalCli,
    #[serde(rename = "credential_profile", alias = "auth_profile")]
    AuthProfile,
    CredentialProcess,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    BearerToken,
    OAuth,
    SessionToken,
    AwsSdk,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTransportKind {
    #[serde(rename = "openai_codex_responses")]
    OpenAiCodexResponses,
    #[serde(rename = "openai_responses")]
    #[default]
    OpenAiResponses,
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub const OPENAI_CODEX: &'static str = "openai-codex";
    pub const OPENAI: &'static str = "openai";
    pub const ANTHROPIC: &'static str = "anthropic";
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModelCatalog {
    pub default_model: ModelRef,
    pub fallback_models: Vec<ModelRef>,
    pub disable_provider_fallback: bool,
    pub built_in_catalog: BuiltInModelCatalog,
    pub model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    pub unknown_model_fallback: Option<ModelRuntimeOverride>,
    pub configured_runtime_max_output_tokens: u32,
}

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
    pub config_file_path: PathBuf,
    pub stored_config: HolonConfigFile,
    pub default_model: ModelRef,
    pub fallback_models: Vec<ModelRef>,
    pub runtime_max_output_tokens: u32,
    pub disable_provider_fallback: bool,
    pub tui_alternate_screen: AltScreenMode,
    pub validated_model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    pub validated_unknown_model_fallback: Option<ModelRuntimeOverride>,
    pub providers: ProviderRegistry,
}

pub type ProviderRegistry = BTreeMap<ProviderId, ProviderRuntimeConfig>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRuntimeConfig {
    pub id: ProviderId,
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth: ProviderAuthConfig,
    pub credential: Option<String>,
    pub codex_home: Option<PathBuf>,
    pub originator: Option<String>,
    pub context_management: AnthropicContextManagementConfig,
}

impl ProviderRuntimeConfig {
    pub fn has_configured_credential(&self) -> bool {
        self.credential
            .as_ref()
            .map(|credential| !credential.trim().is_empty())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAuthConfig {
    pub source: CredentialSource,
    pub kind: CredentialKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicContextManagementConfig {
    pub enabled: bool,
    pub trigger_input_tokens: u32,
    pub keep_recent_tool_uses: u32,
    pub clear_at_least_input_tokens: Option<u32>,
    pub cache_strategy: AnthropicCacheStrategy,
    pub betas: Vec<String>,
}

impl Default for AnthropicContextManagementConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger_input_tokens: 100_000,
            keep_recent_tool_uses: 3,
            clear_at_least_input_tokens: None,
            cache_strategy: AnthropicCacheStrategy::Current,
            betas: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicCacheStrategy {
    Current,
    ClaudeCliLike,
}

impl AnthropicCacheStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::ClaudeCliLike => "claude_cli_like",
        }
    }
}

impl Default for AnthropicCacheStrategy {
    fn default() -> Self {
        Self::Current
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HolonConfigFile {
    #[serde(default, skip_serializing_if = "ModelConfigFile::is_empty")]
    pub model: ModelConfigFile,
    #[serde(default, skip_serializing_if = "ModelsConfigFile::is_empty")]
    pub models: ModelsConfigFile,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: ProvidersConfigFile,
    #[serde(default, skip_serializing_if = "RuntimeConfigFile::is_empty")]
    pub runtime: RuntimeConfigFile,
    #[serde(default, skip_serializing_if = "TuiConfigFile::is_empty")]
    pub tui: TuiConfigFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_fallback: Option<ModelRuntimeOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelsConfigFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub catalog: BTreeMap<String, ModelRuntimeOverride>,
}

pub type ProvidersConfigFile = BTreeMap<ProviderId, ProviderConfigFile>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfigFile {
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth: ProviderAuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialStoreFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, CredentialProfileFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialProfileFile {
    pub kind: CredentialKind,
    pub material: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialProfileStatus {
    pub profile: String,
    pub kind: String,
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfigView {
    pub id: String,
    pub transport: String,
    pub base_url: String,
    pub auth: ProviderAuthView,
    pub credential_configured: bool,
    pub configured_in_config: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAuthView {
    pub source: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
}

impl Default for ProviderAuthConfig {
    fn default() -> Self {
        Self {
            source: CredentialSource::None,
            kind: CredentialKind::None,
            env: None,
            profile: None,
            external: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_provider_fallback: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TuiConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_screen: Option<AltScreenMode>,
}

#[derive(Debug, Deserialize)]
struct ClaudeSettings {
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigSchemaEntry {
    pub key: &'static str,
    pub kind: &'static str,
    pub description: &'static str,
    pub default: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<&'static str>,
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
        let settings_env = load_settings_env().unwrap_or_default();
        let home_dir = home_override.unwrap_or_else(|| {
            env::var("HOLON_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_holon_home())
        });
        let config_file_path = persisted_config_path(&home_dir);
        let stored_config = load_persisted_config_at(&config_file_path)?;
        let credential_store = if config_uses_credential_profiles(&stored_config) {
            load_credential_store_at(&credential_store_path(&home_dir))?
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
        let default_agent_id = env::var("HOLON_AGENT_ID").unwrap_or_else(|_| "default".into());
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

        let anthropic_default_model = "claude-sonnet-4-6".to_string();
        let default_model = resolve_default_model(&stored_config)?;
        let disable_provider_fallback = resolve_disable_provider_fallback(&stored_config)?;
        let fallback_models =
            resolve_fallback_models(&stored_config, &anthropic_default_model, &default_model)?;
        let validated_model_overrides = resolve_model_catalog(&stored_config)?;
        let validated_unknown_model_fallback =
            validate_optional_model_runtime_override(stored_config.model.unknown_fallback.clone())?;
        let providers =
            resolve_provider_registry(&stored_config, &settings_env, &credential_store)?;
        let tui_alternate_screen = env::var("HOLON_TUI_ALTERNATE_SCREEN")
            .ok()
            .map(|value| AltScreenMode::parse(&value))
            .transpose()?
            .or(stored_config.tui.alternate_screen)
            .unwrap_or(AltScreenMode::Auto);

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
            config_file_path,
            stored_config,
            default_model,
            fallback_models,
            runtime_max_output_tokens,
            disable_provider_fallback,
            tui_alternate_screen,
            validated_model_overrides,
            validated_unknown_model_fallback,
            providers,
        })
    }

    pub fn run_dir(&self) -> PathBuf {
        self.home_dir.join("run")
    }

    pub fn agent_root_dir(&self) -> PathBuf {
        self.data_dir.join("agents")
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

impl RuntimeModelCatalog {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            default_model: config.default_model.clone(),
            fallback_models: config.fallback_models.clone(),
            disable_provider_fallback: config.provider_fallback_disabled(),
            built_in_catalog: BuiltInModelCatalog::default(),
            model_overrides: config.validated_model_overrides.clone(),
            unknown_model_fallback: config.validated_unknown_model_fallback.clone(),
            configured_runtime_max_output_tokens: config.runtime_max_output_tokens,
        }
    }

    pub fn provider_chain(&self, model_override: Option<&ModelRef>) -> Vec<ModelRef> {
        if self.disable_provider_fallback {
            return vec![self.effective_model(model_override)];
        }
        let mut chain = Vec::new();
        if let Some(model_override) = model_override {
            chain.push(model_override.clone());
        }
        chain.push(self.default_model.clone());
        for model in &self.fallback_models {
            if !chain.iter().any(|existing| existing == model) {
                chain.push(model.clone());
            }
        }
        chain
    }
    pub fn effective_model(&self, model_override: Option<&ModelRef>) -> ModelRef {
        model_override
            .cloned()
            .unwrap_or_else(|| self.default_model.clone())
    }

    pub fn resolved_model_policy(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRef>,
    ) -> ResolvedRuntimeModelPolicy {
        let model_ref = self.effective_model(model_override);
        self.built_in_catalog.resolve_policy(
            &model_ref,
            &self.model_overrides,
            self.unknown_model_fallback.as_ref(),
            base_context_config,
            self.configured_runtime_max_output_tokens,
        )
    }

    pub fn resolved_context_config(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRef>,
    ) -> ContextConfig {
        self.built_in_catalog
            .apply_policy(
                &self.effective_model(model_override),
                &self.model_overrides,
                self.unknown_model_fallback.as_ref(),
                base_context_config,
                self.configured_runtime_max_output_tokens,
            )
            .0
    }

    pub fn available_models(&self) -> Vec<BuiltInModelMetadata> {
        self.built_in_catalog.list()
    }
}

impl Default for RuntimeModelCatalog {
    fn default() -> Self {
        Self {
            default_model: ModelRef::parse("openai/gpt-5.4").expect("valid default model ref"),
            fallback_models: Vec::new(),
            disable_provider_fallback: false,
            built_in_catalog: BuiltInModelCatalog::default(),
            model_overrides: HashMap::new(),
            unknown_model_fallback: None,
            configured_runtime_max_output_tokens: 8192,
        }
    }
}

impl ControlAuthMode {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "required" => Ok(Self::Required),
            "disabled" => Ok(Self::Disabled),
            other => Err(anyhow!(
                "invalid HOLON_CONTROL_AUTH_MODE {other}; expected auto|required|disabled"
            )),
        }
    }
}

impl CredentialSource {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "env" => Ok(Self::Env),
            "external_cli" => Ok(Self::ExternalCli),
            "credential_profile" | "auth_profile" => Ok(Self::AuthProfile),
            "credential_process" => Ok(Self::CredentialProcess),
            "none" => Ok(Self::None),
            other => Err(anyhow!(
                "invalid credential source {other}; expected env|external_cli|credential_profile|credential_process|none"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::ExternalCli => "external_cli",
            Self::AuthProfile => "credential_profile",
            Self::CredentialProcess => "credential_process",
            Self::None => "none",
        }
    }
}

impl CredentialKind {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "api_key" => Ok(Self::ApiKey),
            "bearer_token" => Ok(Self::BearerToken),
            "oauth" => Ok(Self::OAuth),
            "session_token" => Ok(Self::SessionToken),
            "aws_sdk" => Ok(Self::AwsSdk),
            "none" => Ok(Self::None),
            other => Err(anyhow!(
                "invalid credential kind {other}; expected api_key|bearer_token|oauth|session_token|aws_sdk|none"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::BearerToken => "bearer_token",
            Self::OAuth => "oauth",
            Self::SessionToken => "session_token",
            Self::AwsSdk => "aws_sdk",
            Self::None => "none",
        }
    }
}

impl ProviderTransportKind {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai_codex_responses" => Ok(Self::OpenAiCodexResponses),
            "openai_responses" => Ok(Self::OpenAiResponses),
            "openai_chat_completions" => Ok(Self::OpenAiChatCompletions),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            other => Err(anyhow!(
                "invalid provider transport {other}; expected openai_codex_responses|openai_responses|openai_chat_completions|anthropic_messages"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCodexResponses => "openai_codex_responses",
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::AnthropicMessages => "anthropic_messages",
        }
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

impl ProviderId {
    pub fn parse(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(anyhow!("provider id must not be empty"));
        }
        if !normalized
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(anyhow!(
                "invalid provider id {normalized}; expected lowercase ascii, digits, '-' or '_'"
            ));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn openai_codex() -> Self {
        Self(Self::OPENAI_CODEX.to_string())
    }

    pub fn openai() -> Self {
        Self(Self::OPENAI.to_string())
    }

    pub fn anthropic() -> Self {
        Self(Self::ANTHROPIC.to_string())
    }

    pub fn is_openai_codex(&self) -> bool {
        self.as_str() == Self::OPENAI_CODEX
    }

    pub fn is_openai(&self) -> bool {
        self.as_str() == Self::OPENAI
    }

    pub fn is_anthropic(&self) -> bool {
        self.as_str() == Self::ANTHROPIC
    }
}

impl ModelRef {
    pub fn new(provider: ProviderId, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("model ref must not be empty"));
        }
        let (provider, model) = trimmed
            .split_once('/')
            .ok_or_else(|| anyhow!("invalid model ref {trimmed}; expected provider/model"))?;
        let provider = ProviderId::parse(provider)?;
        let model = model.trim();
        if model.is_empty() {
            return Err(anyhow!(
                "invalid model ref {trimmed}; model part must not be empty"
            ));
        }
        Ok(Self {
            provider,
            model: model.to_string(),
        })
    }

    pub fn from_legacy_anthropic_model(model: &str) -> Result<Self> {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("model ref must not be empty"));
        }
        Ok(Self {
            provider: ProviderId::anthropic(),
            model: trimmed.to_string(),
        })
    }

    pub fn as_string(&self) -> String {
        format!("{}/{}", self.provider.as_str(), self.model)
    }
}

impl Serialize for ModelRef {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_string())
    }
}

impl Serialize for ProviderId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ProviderId::parse(&raw).map_err(D::Error::custom)
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ModelRef::parse(&raw).map_err(D::Error::custom)
    }
}

impl ModelConfigFile {
    fn is_empty(&self) -> bool {
        self.default.is_none()
            && self.fallbacks.is_empty()
            && self
                .unknown_fallback
                .as_ref()
                .map(ModelRuntimeOverride::is_empty)
                .unwrap_or(true)
    }
}

impl ModelsConfigFile {
    fn is_empty(&self) -> bool {
        self.catalog.is_empty()
    }
}

fn resolve_model_catalog(
    stored_config: &HolonConfigFile,
) -> Result<HashMap<ModelRef, ModelRuntimeOverride>> {
    stored_config
        .models
        .catalog
        .iter()
        .map(|(model_ref, override_config)| {
            Ok((
                ModelRef::parse(model_ref)?,
                validate_model_runtime_override(override_config.clone())?,
            ))
        })
        .collect()
}

fn validate_optional_model_runtime_override(
    override_config: Option<ModelRuntimeOverride>,
) -> Result<Option<ModelRuntimeOverride>> {
    override_config
        .map(validate_model_runtime_override)
        .transpose()
        .map(|value| value.filter(|entry| !entry.is_empty()))
}

impl RuntimeConfigFile {
    fn is_empty(&self) -> bool {
        self.max_output_tokens.is_none() && self.disable_provider_fallback.is_none()
    }
}

impl TuiConfigFile {
    fn is_empty(&self) -> bool {
        self.alternate_screen.is_none()
    }
}

pub fn persisted_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join("config.json")
}

pub fn credential_store_path(home_dir: &Path) -> PathBuf {
    home_dir.join("credentials.json")
}

pub fn load_persisted_config_at(path: &Path) -> Result<HolonConfigFile> {
    if !path.exists() {
        return Ok(HolonConfigFile::default());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_persisted_config_at(path: &Path, config: &HolonConfigFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(config).context("failed to serialize config")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
}

pub fn load_credential_store_at(path: &Path) -> Result<CredentialStoreFile> {
    if !path.exists() {
        return Ok(CredentialStoreFile::default());
    }
    ensure_owner_only_file(path)?;
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_credential_store_at(path: &Path, store: &CredentialStoreFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(store).context("failed to serialize credential store")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
}

pub fn set_credential_profile_at(
    path: &Path,
    profile: &str,
    kind: CredentialKind,
    material: String,
) -> Result<CredentialProfileStatus> {
    let profile = normalize_credential_profile_id(profile)?;
    validate_stored_credential_kind(kind)?;
    if material.trim().is_empty() {
        return Err(anyhow!("credential material must not be empty"));
    }
    let mut store = load_credential_store_at(path)?;
    store
        .profiles
        .insert(profile.clone(), CredentialProfileFile { kind, material });
    save_credential_store_at(path, &store)?;
    Ok(CredentialProfileStatus {
        profile,
        kind: kind.as_str().to_string(),
        configured: true,
    })
}

pub fn remove_credential_profile_at(path: &Path, profile: &str) -> Result<CredentialProfileStatus> {
    let profile = normalize_credential_profile_id(profile)?;
    let mut store = load_credential_store_at(path)?;
    let removed = store.profiles.remove(&profile);
    save_credential_store_at(path, &store)?;
    Ok(CredentialProfileStatus {
        profile,
        kind: removed
            .map(|entry| entry.kind.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        configured: false,
    })
}

pub fn list_credential_profiles_at(path: &Path) -> Result<Vec<CredentialProfileStatus>> {
    let store = load_credential_store_at(path)?;
    Ok(store
        .profiles
        .into_iter()
        .map(|(profile, entry)| CredentialProfileStatus {
            profile,
            kind: entry.kind.as_str().to_string(),
            configured: !entry.material.trim().is_empty(),
        })
        .collect())
}

pub fn validate_provider_config(
    provider_id: &ProviderId,
    provider_config: &ProviderConfigFile,
) -> Result<()> {
    parse_url_value("providers.<id>.base_url", &provider_config.base_url)?;
    validate_provider_auth(provider_id, &provider_config.auth)
}

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
    }
}

pub fn config_schema() -> Vec<ConfigSchemaEntry> {
    vec![
        ConfigSchemaEntry {
            key: "model.default",
            kind: "model_ref",
            description: "Default provider/model ref used by the runtime.",
            default: json!("openai/gpt-5.4"),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.fallbacks",
            kind: "model_ref_list",
            description:
                "Fallback provider/model refs tried when earlier candidates are unavailable.",
            default: json!(["openai/gpt-5.4", "anthropic/claude-sonnet-4-6"]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "models.catalog",
            kind: "json_object",
            description: "Per-model runtime metadata and policy keyed by provider/model ref.",
            default: json!({}),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback",
            kind: "json_object",
            description: "Explicit runtime policy fallback used for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.context_window_tokens",
            kind: "positive_integer",
            description: "Optional fallback context window for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.effective_context_window_percent",
            kind: "percentage_integer",
            description: "Optional usable-context percent for unknown-model fallback.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.prompt_budget_estimated_tokens",
            kind: "positive_integer",
            description: "Fallback prompt budget used when model metadata is unknown.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.compaction_trigger_estimated_tokens",
            kind: "positive_integer",
            description: "Fallback compaction trigger used when model metadata is unknown.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.compaction_keep_recent_estimated_tokens",
            kind: "positive_integer",
            description: "Fallback uncompacted recent-context budget for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.runtime_max_output_tokens",
            kind: "positive_integer",
            description: "Fallback max output token budget for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.max_output_tokens",
            kind: "positive_integer",
            description: "Default max output token budget for providers.",
            default: json!(8192),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.prompt_budget_estimated_tokens",
            kind: "positive_integer",
            description: "Estimated token budget for one assembled context projection.",
            default: json!(4096),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.compaction_trigger_estimated_tokens",
            kind: "positive_integer",
            description: "Estimated visible-token threshold that triggers legacy message compaction fallback.",
            default: json!(2048),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.compaction_keep_recent_estimated_tokens",
            kind: "positive_integer",
            description: "Estimated visible-token budget kept un-compacted in the legacy message fallback.",
            default: json!(768),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.recent_episode_candidates",
            kind: "positive_integer",
            description: "Number of archived episodes considered for relevance ranking during prompt assembly.",
            default: json!(12),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.max_relevant_episodes",
            kind: "positive_integer",
            description: "Maximum number of archived episodes rendered into prompt context.",
            default: json!(3),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.disable_provider_fallback",
            kind: "boolean",
            description: "Disable provider/model fallback and require deterministic single-provider execution.",
            default: json!(false),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "tui.alternate_screen",
            kind: "enum",
            description: "Whether the TUI uses the terminal alternate screen buffer.",
            default: json!("auto"),
            allowed_values: vec!["auto", "always", "never"],
        },
    ]
}

pub fn get_config_key(config: &HolonConfigFile, key: &str) -> Result<Value> {
    match key {
        "model.default" => Ok(config
            .model
            .default
            .as_ref()
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null)),
        "model.fallbacks" => Ok(Value::Array(
            config
                .model
                .fallbacks
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        )),
        "models.catalog" => Ok(serde_json::to_value(&config.models.catalog)?),
        "model.unknown_fallback" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.context_window_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.context_window_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.effective_context_window_percent" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.effective_context_window_percent)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.prompt_budget_estimated_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.prompt_budget_estimated_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.compaction_trigger_estimated_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.compaction_trigger_estimated_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.compaction_keep_recent_estimated_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.compaction_keep_recent_estimated_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.runtime_max_output_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.runtime_max_output_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "runtime.max_output_tokens" => Ok(config
            .runtime
            .max_output_tokens
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "runtime.disable_provider_fallback" => Ok(config
            .runtime
            .disable_provider_fallback
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "tui.alternate_screen" => Ok(config
            .tui
            .alternate_screen
            .map(|value| Value::String(value.as_str().to_string()))
            .unwrap_or(Value::Null)),
        _ => Err(unknown_config_key(key)),
    }
}

pub fn set_config_key(config: &mut HolonConfigFile, key: &str, raw_value: &str) -> Result<()> {
    match key {
        "model.default" => {
            let parsed = ModelRef::parse(raw_value)?;
            config.model.default = Some(parsed.as_string());
        }
        "model.fallbacks" => {
            config.model.fallbacks = parse_model_ref_list(raw_value)?
                .into_iter()
                .map(|model| model.as_string())
                .collect();
        }
        "models.catalog" => {
            config.models.catalog = parse_model_catalog_value(raw_value)?;
        }
        "model.unknown_fallback" => {
            config.model.unknown_fallback = parse_optional_model_runtime_override(raw_value)?;
        }
        "model.unknown_fallback.context_window_tokens" => {
            ensure_unknown_model_fallback(config).context_window_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.effective_context_window_percent" => {
            ensure_unknown_model_fallback(config).effective_context_window_percent =
                Some(parse_percentage_u8_key(key, raw_value)?);
        }
        "model.unknown_fallback.prompt_budget_estimated_tokens" => {
            ensure_unknown_model_fallback(config).prompt_budget_estimated_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.compaction_trigger_estimated_tokens" => {
            ensure_unknown_model_fallback(config).compaction_trigger_estimated_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.compaction_keep_recent_estimated_tokens" => {
            ensure_unknown_model_fallback(config).compaction_keep_recent_estimated_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.runtime_max_output_tokens" => {
            ensure_unknown_model_fallback(config).runtime_max_output_tokens =
                Some(parse_positive_u32_key(key, raw_value)?);
        }
        "runtime.max_output_tokens" => {
            let value = parse_positive_u32_key(key, raw_value)?;
            config.runtime.max_output_tokens = Some(value);
        }
        "runtime.disable_provider_fallback" => {
            config.runtime.disable_provider_fallback = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "tui.alternate_screen" => {
            config.tui.alternate_screen = Some(AltScreenMode::parse(raw_value)?);
        }
        _ => return Err(unknown_config_key(key)),
    }
    Ok(())
}

pub fn unset_config_key(config: &mut HolonConfigFile, key: &str) -> Result<()> {
    match key {
        "model.default" => config.model.default = None,
        "model.fallbacks" => config.model.fallbacks.clear(),
        "models.catalog" => config.models.catalog.clear(),
        "model.unknown_fallback" => config.model.unknown_fallback = None,
        "model.unknown_fallback.context_window_tokens" => {
            clear_unknown_model_fallback_field(config, |value| value.context_window_tokens = None);
        }
        "model.unknown_fallback.effective_context_window_percent" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.effective_context_window_percent = None;
            });
        }
        "model.unknown_fallback.prompt_budget_estimated_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.prompt_budget_estimated_tokens = None;
            });
        }
        "model.unknown_fallback.compaction_trigger_estimated_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.compaction_trigger_estimated_tokens = None;
            });
        }
        "model.unknown_fallback.compaction_keep_recent_estimated_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.compaction_keep_recent_estimated_tokens = None;
            });
        }
        "model.unknown_fallback.runtime_max_output_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.runtime_max_output_tokens = None;
            });
        }
        "runtime.max_output_tokens" => config.runtime.max_output_tokens = None,
        "runtime.disable_provider_fallback" => config.runtime.disable_provider_fallback = None,
        "tui.alternate_screen" => config.tui.alternate_screen = None,
        _ => return Err(unknown_config_key(key)),
    }
    Ok(())
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

fn resolve_provider_registry(
    stored_config: &HolonConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<ProviderRegistry> {
    let mut registry = built_in_provider_registry(settings_env)?;
    for (id, provider_config) in &stored_config.providers {
        let built_in = registry.remove(id);
        let runtime = materialize_provider_config(
            id.clone(),
            provider_config.clone(),
            settings_env,
            credential_store,
            built_in,
        )?;
        registry.insert(id.clone(), runtime);
    }
    Ok(registry)
}

fn built_in_provider_registry(settings_env: &HashMap<String, String>) -> Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::new();
    let openai_codex = ProviderId::openai_codex();
    registry.insert(
        openai_codex.clone(),
        ProviderRuntimeConfig {
            id: openai_codex,
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: env::var("HOLON_OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
            },
            credential: None,
            codex_home: Some(
                env::var("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| default_codex_home()),
            ),
            originator: Some("codex_cli_rs".into()),
            context_management: Default::default(),
        },
    );
    let openai = ProviderId::openai();
    registry.insert(
        openai.clone(),
        ProviderRuntimeConfig {
            id: openai,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: env::var("HOLON_OPENAI_BASE_URL")
                .ok()
                .or_else(|| env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: env::var("OPENAI_API_KEY").ok(),
            codex_home: None,
            originator: None,
            context_management: Default::default(),
        },
    );
    let anthropic = ProviderId::anthropic();
    registry.insert(
        anthropic.clone(),
        ProviderRuntimeConfig {
            id: anthropic,
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: get_config_value("ANTHROPIC_BASE_URL", None, settings_env)
                .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("ANTHROPIC_AUTH_TOKEN".into()),
                profile: None,
                external: None,
            },
            credential: get_config_value("ANTHROPIC_AUTH_TOKEN", None, settings_env),
            codex_home: None,
            originator: None,
            context_management: resolve_anthropic_context_management_config()?,
        },
    );
    insert_openai_compatible_provider(
        &mut registry,
        "arcee",
        "https://api.arcee.ai/api/v1",
        &["ARCEE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "byteplus",
        "https://ark.ap-southeast.bytepluses.com/api/v3",
        &["BYTEPLUS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "byteplus-coding",
        "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
        &["BYTEPLUS_CODING_API_KEY", "BYTEPLUS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "chutes",
        "https://llm.chutes.ai/v1",
        &["CHUTES_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "deepseek",
        "https://api.deepseek.com",
        &["DEEPSEEK_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "fireworks",
        "https://api.fireworks.ai/inference/v1",
        &["FIREWORKS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "huggingface",
        "https://router.huggingface.co/v1",
        &["HUGGINGFACE_API_KEY", "HF_TOKEN"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "kilocode",
        "https://api.kilo.ai/api/gateway",
        &["KILOCODE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "litellm",
        "http://localhost:4000",
        &["LITELLM_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "mistral",
        "https://api.mistral.ai/v1",
        &["MISTRAL_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "moonshot",
        "https://api.moonshot.ai/v1",
        &["MOONSHOT_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "nvidia",
        "https://integrate.api.nvidia.com/v1",
        &["NVIDIA_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "opencode-go",
        "https://opencode.ai/zen/go/v1",
        &["OPENCODE_GO_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "openrouter",
        "https://openrouter.ai/api/v1",
        &["OPENROUTER_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "qianfan",
        "https://qianfan.baidubce.com/v2",
        &["QIANFAN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "qwen",
        "https://coding-intl.dashscope.aliyuncs.com/v1",
        &["QWEN_API_KEY", "DASHSCOPE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "stepfun",
        "https://api.stepfun.ai/v1",
        &["STEPFUN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "stepfun-plan",
        "https://api.stepfun.ai/step_plan/v1",
        &["STEPFUN_PLAN_API_KEY", "STEPFUN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "synthetic",
        "https://api.synthetic.new/anthropic",
        &["SYNTHETIC_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "tencent-tokenhub",
        "https://tokenhub.tencentmaas.com/v1",
        &["TOKENHUB_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "together",
        "https://api.together.xyz/v1",
        &["TOGETHER_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "venice",
        "https://api.venice.ai/api/v1",
        &["VENICE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "vllm",
        "http://127.0.0.1:8000/v1",
        &[],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "volcengine",
        "https://ark.cn-beijing.volces.com/api/v3",
        &["VOLCENGINE_API_KEY", "ARK_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "volcengine-coding",
        "https://ark.cn-beijing.volces.com/api/coding/v3",
        &[
            "VOLCENGINE_CODING_API_KEY",
            "VOLCENGINE_API_KEY",
            "ARK_API_KEY",
        ],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xiaomi",
        "https://api.xiaomimimo.com/v1",
        &["XIAOMI_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xai",
        "https://api.x.ai/v1",
        &["XAI_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "zai",
        "https://api.z.ai/api/paas/v4",
        &["ZAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "minimax",
        "https://api.minimax.io/anthropic",
        &["MINIMAX_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "minimax-portal",
        "https://api.minimax.io/anthropic",
        &["MINIMAX_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "vercel-ai-gateway",
        "https://ai-gateway.vercel.sh",
        &["AI_GATEWAY_API_KEY", "VERCEL_AI_GATEWAY_API_KEY"],
        settings_env,
    )?;
    Ok(registry)
}

fn insert_openai_compatible_provider(
    registry: &mut ProviderRegistry,
    provider: &str,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    insert_builtin_http_provider(
        registry,
        provider,
        ProviderTransportKind::OpenAiChatCompletions,
        default_base_url,
        env_names,
        settings_env,
    )
}

fn insert_anthropic_compatible_provider(
    registry: &mut ProviderRegistry,
    provider: &str,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    insert_builtin_http_provider(
        registry,
        provider,
        ProviderTransportKind::AnthropicMessages,
        default_base_url,
        env_names,
        settings_env,
    )
}

fn insert_builtin_http_provider(
    registry: &mut ProviderRegistry,
    provider: &str,
    transport: ProviderTransportKind,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let id = ProviderId::parse(provider)?;
    let base_url_env = format!("HOLON_{}_BASE_URL", env_key_fragment(provider));
    let base_url = get_config_value(&base_url_env, None, settings_env)
        .unwrap_or_else(|| default_base_url.to_string());
    let credential = resolve_first_env_value(env_names, settings_env);
    let env_name = credential
        .as_ref()
        .and_then(|resolution| resolution.env_name.clone())
        .or_else(|| {
            if env_names.is_empty() {
                None
            } else {
                Some(env_names.join(" or "))
            }
        });
    registry.insert(
        id.clone(),
        ProviderRuntimeConfig {
            id,
            transport,
            base_url,
            auth: env_name
                .as_ref()
                .map(|env| ProviderAuthConfig {
                    source: CredentialSource::Env,
                    kind: CredentialKind::ApiKey,
                    env: Some(env.clone()),
                    profile: None,
                    external: None,
                })
                .unwrap_or_default(),
            credential: credential.map(|resolution| resolution.value),
            codex_home: None,
            originator: None,
            context_management: Default::default(),
        },
    );
    Ok(())
}

fn env_key_fragment(provider: &str) -> String {
    provider
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn resolve_first_env_value(
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Option<ResolvedEnvValue> {
    env_names.iter().find_map(|env_name| {
        get_config_value(env_name, None, settings_env).map(|value| ResolvedEnvValue {
            env_name: Some((*env_name).to_string()),
            value,
        })
    })
}

struct ResolvedEnvValue {
    env_name: Option<String>,
    value: String,
}

fn materialize_provider_config(
    id: ProviderId,
    provider_config: ProviderConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
    built_in: Option<ProviderRuntimeConfig>,
) -> Result<ProviderRuntimeConfig> {
    validate_provider_auth(&id, &provider_config.auth)?;
    let credential =
        resolve_provider_credential(&provider_config.auth, settings_env, credential_store)?;
    let mut runtime = built_in.unwrap_or_else(|| ProviderRuntimeConfig {
        id: id.clone(),
        transport: provider_config.transport,
        base_url: provider_config.base_url.clone(),
        auth: provider_config.auth.clone(),
        credential: None,
        codex_home: None,
        originator: None,
        context_management: Default::default(),
    });
    runtime.id = id;
    runtime.transport = provider_config.transport;
    runtime.base_url = provider_config.base_url;
    runtime.auth = provider_config.auth;
    runtime.credential = credential;
    Ok(runtime)
}

fn resolve_provider_credential(
    auth: &ProviderAuthConfig,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<Option<String>> {
    match auth.source {
        CredentialSource::Env => Ok(auth
            .env
            .as_deref()
            .and_then(|key| get_config_value(key, None, settings_env))),
        CredentialSource::AuthProfile => auth
            .profile
            .as_deref()
            .map(normalize_credential_profile_id)
            .transpose()?
            .and_then(|profile| credential_store.profiles.get(&profile))
            .map(|entry| {
                if entry.kind != auth.kind {
                    return Err(anyhow!(
                        "credential profile {} has kind {}, but provider expects {}",
                        auth.profile.as_deref().unwrap_or_default(),
                        entry.kind.as_str(),
                        auth.kind.as_str()
                    ));
                }
                Ok(entry.material.clone())
            })
            .transpose(),
        CredentialSource::None
        | CredentialSource::ExternalCli
        | CredentialSource::CredentialProcess => Ok(None),
    }
}

fn validate_provider_auth(provider_id: &ProviderId, auth: &ProviderAuthConfig) -> Result<()> {
    match (auth.source, auth.kind) {
        (CredentialSource::Env, CredentialKind::ApiKey | CredentialKind::BearerToken) => {
            if auth.env.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(anyhow!(
                    "provider {} env auth requires auth.env",
                    provider_id.as_str()
                ));
            }
        }
        (CredentialSource::ExternalCli, CredentialKind::SessionToken) => {
            if auth
                .external
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(anyhow!(
                    "provider {} external_cli auth requires auth.external",
                    provider_id.as_str()
                ));
            }
        }
        (
            CredentialSource::AuthProfile,
            CredentialKind::ApiKey
            | CredentialKind::BearerToken
            | CredentialKind::OAuth
            | CredentialKind::SessionToken,
        ) => {
            let profile = auth.profile.as_deref().ok_or_else(|| {
                anyhow!(
                    "provider {} credential_profile auth requires auth.profile",
                    provider_id.as_str()
                )
            })?;
            if profile.trim().is_empty() {
                return Err(anyhow!(
                    "provider {} credential_profile auth requires auth.profile",
                    provider_id.as_str()
                ));
            }
            normalize_credential_profile_id(profile).with_context(|| {
                format!(
                    "provider {} credential_profile auth has invalid auth.profile",
                    provider_id.as_str()
                )
            })?;
        }
        (CredentialSource::None, CredentialKind::None) => {}
        _ => {
            return Err(anyhow!(
                "provider {} unsupported auth contract {}+{}",
                provider_id.as_str(),
                auth.source.as_str(),
                auth.kind.as_str()
            ));
        }
    }
    Ok(())
}

pub fn provider_registry_for_tests(
    openai_key: Option<&str>,
    anthropic_token: Option<&str>,
    codex_home: PathBuf,
) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let openai_codex = ProviderId::openai_codex();
    registry.insert(
        openai_codex.clone(),
        ProviderRuntimeConfig {
            id: openai_codex,
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
            },
            credential: None,
            codex_home: Some(codex_home),
            originator: Some("codex_cli_rs".into()),
            context_management: Default::default(),
        },
    );
    let openai = ProviderId::openai();
    registry.insert(
        openai.clone(),
        ProviderRuntimeConfig {
            id: openai,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: openai_key.map(ToString::to_string),
            codex_home: None,
            originator: None,
            context_management: Default::default(),
        },
    );
    let anthropic = ProviderId::anthropic();
    registry.insert(
        anthropic.clone(),
        ProviderRuntimeConfig {
            id: anthropic,
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: "https://api.anthropic.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("ANTHROPIC_AUTH_TOKEN".into()),
                profile: None,
                external: None,
            },
            credential: anthropic_token.map(ToString::to_string),
            codex_home: None,
            originator: None,
            context_management: Default::default(),
        },
    );
    registry
}

fn resolve_default_model(stored_config: &HolonConfigFile) -> Result<ModelRef> {
    if let Ok(value) = env::var("HOLON_MODEL") {
        return ModelRef::parse(&value);
    }
    if let Some(value) = &stored_config.model.default {
        return ModelRef::parse(value);
    }
    ModelRef::parse("openai/gpt-5.4")
}

fn resolve_fallback_models(
    stored_config: &HolonConfigFile,
    anthropic_default_model: &str,
    default_model: &ModelRef,
) -> Result<Vec<ModelRef>> {
    let configured = if let Ok(value) = env::var("HOLON_MODEL_FALLBACKS") {
        parse_model_ref_list(&value)?
    } else if !stored_config.model.fallbacks.is_empty() {
        stored_config
            .model
            .fallbacks
            .iter()
            .map(|value| ModelRef::parse(value))
            .collect::<Result<Vec<_>>>()?
    } else {
        vec![
            ModelRef::parse("openai/gpt-5.4")?,
            ModelRef::parse(&format!("anthropic/{anthropic_default_model}"))?,
        ]
    };

    Ok(configured
        .into_iter()
        .filter(|model| model != default_model)
        .fold(Vec::new(), |mut acc, model| {
            if !acc.iter().any(|existing| existing == &model) {
                acc.push(model);
            }
            acc
        }))
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

fn parse_model_ref_list(raw_value: &str) -> Result<Vec<ModelRef>> {
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

fn parse_model_catalog_value(raw_value: &str) -> Result<BTreeMap<String, ModelRuntimeOverride>> {
    let parsed: BTreeMap<String, ModelRuntimeOverride> =
        serde_json::from_str(raw_value).context("models.catalog expects a JSON object")?;
    let mut validated = BTreeMap::new();
    for (model_ref, override_config) in parsed {
        ModelRef::parse(&model_ref)?;
        validated.insert(model_ref, validate_model_runtime_override(override_config)?);
    }
    Ok(validated)
}

fn parse_optional_model_runtime_override(raw_value: &str) -> Result<Option<ModelRuntimeOverride>> {
    if raw_value.trim().eq_ignore_ascii_case("null") {
        return Ok(None);
    }
    let parsed: ModelRuntimeOverride =
        serde_json::from_str(raw_value).context("expected a JSON object or null")?;
    validate_optional_model_runtime_override(Some(parsed))
}

fn parse_bool_value(raw_value: &str) -> Result<Option<bool>> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "true" | "1" | "yes" | "on" => Ok(Some(true)),
        "false" | "0" | "no" | "off" => Ok(Some(false)),
        _ => Err(anyhow!("expected boolean true|false|1|0|yes|no|on|off")),
    }
}

fn resolve_anthropic_context_management_config() -> Result<AnthropicContextManagementConfig> {
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
        .unwrap_or_else(default_anthropic_runtime_cache_strategy);
    let betas_env = env::var("HOLON_ANTHROPIC_BETAS").ok();
    let betas = match betas_env {
        Some(value) => parse_comma_separated_values(&value),
        None if cache_strategy == AnthropicCacheStrategy::ClaudeCliLike => vec![
            "claude-code-20250219".to_string(),
            "prompt-caching-scope-2026-01-05".to_string(),
        ],
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
    AnthropicCacheStrategy::ClaudeCliLike
}

fn parse_anthropic_cache_strategy_env(raw_value: &str) -> Result<AnthropicCacheStrategy> {
    if raw_value.trim().is_empty() {
        return Ok(default_anthropic_runtime_cache_strategy());
    }
    parse_anthropic_cache_strategy(raw_value)
}

fn parse_anthropic_cache_strategy(raw_value: &str) -> Result<AnthropicCacheStrategy> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "current" => Ok(AnthropicCacheStrategy::Current),
        "claude_cli_like" | "claude-cli-like" | "claude" => {
            Ok(AnthropicCacheStrategy::ClaudeCliLike)
        }
        _ => Err(anyhow!(
            "HOLON_ANTHROPIC_CACHE_STRATEGY expects current, claude_cli_like, claude-cli-like, or claude"
        )),
    }
}

fn parse_comma_separated_values(raw_value: &str) -> Vec<String> {
    raw_value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_positive_u32_key(key: &str, raw_value: &str) -> Result<u32> {
    raw_value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

fn parse_positive_usize_key(key: &str, raw_value: &str) -> Result<usize> {
    raw_value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

fn parse_percentage_u8_key(key: &str, raw_value: &str) -> Result<u8> {
    raw_value
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|value| *value > 0 && *value <= 100)
        .ok_or_else(|| anyhow!("{key} expects an integer from 1 to 100"))
}

fn validate_model_runtime_override(
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

fn ensure_unknown_model_fallback(config: &mut HolonConfigFile) -> &mut ModelRuntimeOverride {
    config
        .model
        .unknown_fallback
        .get_or_insert_with(ModelRuntimeOverride::default)
}

fn clear_unknown_model_fallback_field(
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

fn parse_url_value(key: &str, raw_value: &str) -> Result<()> {
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

fn normalize_credential_profile_id(profile: &str) -> Result<String> {
    let trimmed = profile.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("credential profile id must not be empty"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(anyhow!(
            "credential profile id must not contain control characters"
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_stored_credential_kind(kind: CredentialKind) -> Result<()> {
    match kind {
        CredentialKind::ApiKey
        | CredentialKind::BearerToken
        | CredentialKind::OAuth
        | CredentialKind::SessionToken => Ok(()),
        CredentialKind::AwsSdk | CredentialKind::None => Err(anyhow!(
            "credential profiles support api_key|bearer_token|oauth|session_token"
        )),
    }
}

fn ensure_owner_only_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata =
            fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(anyhow!(
                "credential store {} must be owner-only; found mode {:o}. Fix it with: chmod 600 {}",
                path.display(),
                mode,
                path.display()
            ));
        }
    }
    Ok(())
}

fn config_uses_credential_profiles(config: &HolonConfigFile) -> bool {
    config
        .providers
        .values()
        .any(|provider| provider.auth.source == CredentialSource::AuthProfile)
}

fn is_startup_only_config_key(key: &str) -> bool {
    let _ = key;
    false
}

fn startup_only_config_key_error(key: &str) -> anyhow::Error {
    anyhow!(
        "config key {key} is startup-only; configure it via env vars or CLI startup flags instead of runtime config mutation"
    )
}

fn get_config_value(
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

fn unknown_config_key(key: &str) -> anyhow::Error {
    if is_startup_only_config_key(key) {
        return startup_only_config_key_error(key);
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::{json, Value};
    use tempfile::tempdir;

    use crate::context::ContextConfig;
    use crate::model_catalog::ModelRuntimeOverride;

    use super::{
        config_schema, credential_store_path, default_holon_home, get_config_key, get_config_value,
        list_credential_profiles_at, load_persisted_config_at, parse_anthropic_cache_strategy,
        parse_anthropic_cache_strategy_env, parse_comma_separated_values, parse_url_value,
        persisted_config_path, provider_registry_for_tests,
        resolve_anthropic_context_management_config, save_persisted_config_at, set_config_key,
        set_credential_profile_at, unset_config_key, AnthropicCacheStrategy,
        AnthropicContextManagementConfig, AppConfig, ControlAuthMode, CredentialKind,
        CredentialSource, CredentialStoreFile, HolonConfigFile, ModelRef, ProviderAuthConfig,
        ProviderConfigFile, ProviderId, ProviderTransportKind, RuntimeModelCatalog,
    };

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn unset(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
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
            config_file_path: home_path.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse(default_model).unwrap(),
            fallback_models: fallback_models
                .iter()
                .map(|value| ModelRef::parse(value).unwrap())
                .collect(),
            runtime_max_output_tokens: 8192,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: HashMap::new(),
            validated_unknown_model_fallback: None,
            providers: provider_registry_for_tests(
                Some("openai-key"),
                Some("anthropic-token"),
                PathBuf::from("/tmp/codex-home"),
            ),
        };
        TestAppConfigFixture {
            _home_dir: home_dir,
            _workspace_dir: workspace_dir,
            config,
        }
    }

    #[test]
    fn config_falls_back_to_settings_values() {
        let original_value = std::env::var("ANTHROPIC_BASE_URL").ok();
        std::env::remove_var("ANTHROPIC_BASE_URL");

        let mut settings = HashMap::new();
        settings.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "https://example.com".to_string(),
        );

        let value = get_config_value("ANTHROPIC_BASE_URL", None, &settings);
        assert_eq!(value.as_deref(), Some("https://example.com"));

        if let Some(orig) = original_value {
            std::env::set_var("ANTHROPIC_BASE_URL", orig);
        }
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
            parse_anthropic_cache_strategy("current").unwrap(),
            AnthropicCacheStrategy::Current
        );
        assert_eq!(
            parse_anthropic_cache_strategy("claude-cli-like").unwrap(),
            AnthropicCacheStrategy::ClaudeCliLike
        );
        let err = parse_anthropic_cache_strategy("unknown")
            .err()
            .expect("unknown strategy should fail");
        assert!(err.to_string().contains("claude-cli-like"));
        assert!(err.to_string().contains("claude"));
    }

    #[test]
    fn anthropic_runtime_cache_strategy_defaults_to_claude_cli_like() {
        let _strategy_guard = EnvVarGuard::unset("HOLON_ANTHROPIC_CACHE_STRATEGY");
        let _betas_guard = EnvVarGuard::unset("HOLON_ANTHROPIC_BETAS");

        let config = resolve_anthropic_context_management_config().unwrap();

        assert_eq!(config.cache_strategy, AnthropicCacheStrategy::ClaudeCliLike);
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
            AnthropicCacheStrategy::ClaudeCliLike
        );
        assert_eq!(
            parse_anthropic_cache_strategy_env("  ").unwrap(),
            AnthropicCacheStrategy::ClaudeCliLike
        );
    }

    #[test]
    fn anthropic_context_management_struct_default_stays_neutral() {
        assert_eq!(
            AnthropicContextManagementConfig::default().cache_strategy,
            AnthropicCacheStrategy::Current
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
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", "/tmp/holon-home-test");
        assert_eq!(
            default_holon_home(),
            Path::new("/tmp/holon-home-test/.holon")
        );
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
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
    fn set_get_and_unset_round_trip_models_catalog_object() {
        let mut config = HolonConfigFile::default();
        set_config_key(
            &mut config,
            "models.catalog",
            r#"{"anthropic/claude-sonnet-4-6":{"prompt_budget_estimated_tokens":32000}}"#,
        )
        .unwrap();
        assert_eq!(
            get_config_key(&config, "models.catalog").unwrap(),
            json!({
                "anthropic/claude-sonnet-4-6": {
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
    fn app_config_ignores_bad_credential_store_permissions_until_profile_auth_is_used() {
        let dir = tempdir().unwrap();
        let store_path = credential_store_path(dir.path());
        fs::write(&store_path, r#"{"profiles":{}}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&store_path, fs::Permissions::from_mode(0o644)).unwrap();
        }

        AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();

        let config_path = persisted_config_path(dir.path());
        let mut config = HolonConfigFile::default();
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
            },
        );
        save_persisted_config_at(&config_path, &config).unwrap();

        #[cfg(unix)]
        {
            let err = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap_err();
            assert!(err.to_string().contains("chmod 600"));
        }
    }

    #[test]
    fn built_in_provider_registry_includes_compatible_provider_defaults() {
        let mut settings_env = HashMap::new();
        settings_env.insert("OPENROUTER_API_KEY".to_string(), "settings-key".to_string());
        settings_env.insert(
            "HOLON_OPENROUTER_BASE_URL".to_string(),
            "https://openrouter.example/api/v3".to_string(),
        );
        settings_env.insert("DASHSCOPE_API_KEY".to_string(), "dashscope-key".to_string());

        let providers = super::built_in_provider_registry(&settings_env).unwrap();

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

        let qwen = providers.get(&ProviderId::parse("qwen").unwrap()).unwrap();
        assert_eq!(qwen.auth.env.as_deref(), Some("DASHSCOPE_API_KEY"));
        assert_eq!(qwen.credential.as_deref(), Some("dashscope-key"));

        let stepfun_plan = providers
            .get(&ProviderId::parse("stepfun-plan").unwrap())
            .unwrap();
        assert_eq!(
            stepfun_plan.auth.env.as_deref(),
            Some("STEPFUN_PLAN_API_KEY or STEPFUN_API_KEY")
        );

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
    }

    #[test]
    fn materialize_provider_config_preserves_builtin_runtime_fields() {
        let settings_env = HashMap::new();
        let mut built_ins = super::built_in_provider_registry(&settings_env).unwrap();
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
            },
            &settings_env,
            &CredentialStoreFile::default(),
            Some(built_in),
        )
        .unwrap();

        assert_eq!(runtime.base_url, "https://codex.example/backend-api");
        assert!(runtime.codex_home.is_some());
        assert_eq!(runtime.originator.as_deref(), Some("codex_cli_rs"));
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
        assert!(keys.contains(&"runtime.disable_provider_fallback"));
        assert!(keys.contains(&"tui.alternate_screen"));
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
}
