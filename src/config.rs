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
    auth::{codex_cli_auth_file_exists, load_codex_cli_credential},
    context::ContextConfig,
    model_catalog::{
        BuiltInModelCatalog, BuiltInModelMetadata, ModelRuntimeOverride, ResolvedRuntimeModelPolicy,
    },
    web::{WebProviderKind, WebSearchMode},
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
    pub web_config: crate::web::WebConfig,
    pub default_model: ModelRef,
    pub fallback_models: Vec<ModelRef>,
    pub runtime_max_output_tokens: u32,
    pub default_tool_output_tokens: u32,
    pub max_tool_output_tokens: u32,
    pub disable_provider_fallback: bool,
    pub tui_alternate_screen: AltScreenMode,
    pub validated_model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    pub validated_unknown_model_fallback: Option<ModelRuntimeOverride>,
    pub providers: ProviderRegistry,
}

pub const DEFAULT_LOCAL_AGENT_ID: &str = "main";

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
    pub reasoning_effort: Option<String>,
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
            cache_strategy: AnthropicCacheStrategy::MessagesNative,
            betas: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicCacheStrategy {
    MessagesNative,
    ClaudeCodePromptCache,
}

impl AnthropicCacheStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MessagesNative => "messages_native",
            Self::ClaudeCodePromptCache => "claude_code_prompt_cache",
        }
    }
}

impl Default for AnthropicCacheStrategy {
    fn default() -> Self {
        Self::MessagesNative
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
    #[serde(default, skip_serializing_if = "WebConfigFile::is_empty")]
    pub web: WebConfigFile,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
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
    pub default_tool_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_provider_fallback: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TuiConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_screen: Option<AltScreenMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebConfigFile {
    #[serde(default, skip_serializing_if = "WebFetchConfigFile::is_empty")]
    pub fetch: WebFetchConfigFile,
    #[serde(default, skip_serializing_if = "WebSearchConfigFile::is_empty")]
    pub search: WebSearchConfigFile,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, WebProviderConfigFile>,
}

impl WebConfigFile {
    pub fn is_empty(&self) -> bool {
        self.fetch.is_empty() && self.search.is_empty() && self.providers.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebFetchConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_response_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_hosts: Vec<String>,
}

impl WebFetchConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.max_chars.is_none()
            && self.max_response_bytes.is_none()
            && self.timeout_seconds.is_none()
            && self.max_redirects.is_none()
            && self.allowed_hosts.is_empty()
            && self.denied_hosts.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebSearchConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<WebSearchMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_provider_attempts: Option<usize>,
}

impl WebSearchConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.provider.is_none()
            && self.mode.is_none()
            && self.providers.is_empty()
            && self.max_results.is_none()
            && self.max_provider_attempts.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebProviderConfigFile {
    pub kind: WebProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Named credential profile to load the API key from.
    /// When set, the profile must exist in the credential store
    /// and must be of kind `api_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<WebCommandProviderConfigFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<WebCommandOutputConfigFile>,
    #[serde(
        default,
        skip_serializing_if = "WebProviderLimitsConfigFile::is_default"
    )]
    pub limits: WebProviderLimitsConfigFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCommandProviderConfigFile {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCommandOutputConfigFile {
    #[serde(default)]
    pub format: WebCommandOutputFormatFile,
    pub mapping: WebCommandResultMappingFile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebCommandOutputFormatFile {
    #[default]
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCommandResultMappingFile {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct WebProviderLimitsConfigFile {
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
}

impl WebProviderLimitsConfigFile {
    pub fn is_default(value: &Self) -> bool {
        value == &Self::default()
    }
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
        Self::load_with_home_and_mode(home_override, ConfigLoadMode::Runtime)
    }

    pub fn load_for_config_inspection() -> Result<Self> {
        Self::load_with_home_for_config_inspection(None)
    }

    pub fn load_with_home_for_config_inspection(home_override: Option<PathBuf>) -> Result<Self> {
        Self::load_with_home_and_mode(home_override, ConfigLoadMode::ConfigInspection)
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
        let providers =
            resolve_provider_registry(&stored_config, &settings_env, &credential_store)?;
        let explicit_default = resolve_default_model(&stored_config)?;
        let explicit_fallbacks = resolve_fallback_models(&stored_config)?;
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
            config_file_path,
            stored_config,
            web_config,
            default_model,
            fallback_models,
            runtime_max_output_tokens,
            default_tool_output_tokens,
            max_tool_output_tokens,
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

pub fn built_in_provider_default_config(
    provider_id: &ProviderId,
) -> Result<Option<ProviderConfigFile>> {
    let settings_env = load_settings_env()?;
    built_in_provider_default_config_with_settings(provider_id, &settings_env)
}

fn built_in_provider_default_config_with_settings(
    provider_id: &ProviderId,
    settings_env: &HashMap<String, String>,
) -> Result<Option<ProviderConfigFile>> {
    let registry = built_in_provider_registry(&settings_env)?;
    Ok(registry
        .get(provider_id)
        .map(|provider| ProviderConfigFile {
            transport: provider.transport,
            base_url: provider.base_url.clone(),
            auth: ProviderAuthConfig::default(),
            reasoning_effort: None,
        }))
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
            description: "Explicit default provider/model ref. When unset, the runtime derives one from authenticated providers.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.fallbacks",
            kind: "model_ref_list",
            description:
                "Explicit fallback provider/model refs. Null or an empty persisted list means unset; when unset, the runtime derives fallbacks from authenticated providers.",
            default: Value::Null,
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
            key: "runtime.default_tool_output_tokens",
            kind: "positive_integer",
            description: "Default model-visible output token budget for local command tools.",
            default: json!(crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.max_tool_output_tokens",
            kind: "positive_integer",
            description: "Upper model-visible output token budget for local command tools.",
            default: json!(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS),
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
        ConfigSchemaEntry {
            key: "web.fetch.enabled",
            kind: "boolean",
            description: "Enable the runtime-native WebFetch tool.",
            default: json!(true),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "web.fetch.max_chars",
            kind: "positive_integer",
            description: "Maximum model-visible characters returned by WebFetch.",
            default: json!(crate::web::WebFetchConfig::default().max_chars),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.max_response_bytes",
            kind: "positive_integer",
            description: "Maximum response bytes read by WebFetch before truncation.",
            default: json!(crate::web::WebFetchConfig::default().max_response_bytes),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.timeout_seconds",
            kind: "positive_integer",
            description: "Per-request timeout for WebFetch and managed WebSearch providers.",
            default: json!(crate::web::WebFetchConfig::default().timeout_seconds),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.max_redirects",
            kind: "positive_integer",
            description: "Maximum redirect hops followed by WebFetch.",
            default: json!(crate::web::WebFetchConfig::default().max_redirects),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.allowed_hosts",
            kind: "string_list",
            description: "Hosts or host:port entries allowed by WebFetch, including explicit dev loopback exceptions.",
            default: json!([]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.denied_hosts",
            kind: "string_list",
            description: "Hosts or host:port entries blocked by WebFetch.",
            default: json!([]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.enabled",
            kind: "boolean",
            description: "Enable the WebSearch provider-routed tool.",
            default: json!(true),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "web.search.provider",
            kind: "string",
            description: "Default WebSearch provider id, or auto for configured routing policy.",
            default: json!("auto"),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.mode",
            kind: "enum",
            description: "WebSearch routing mode: single, fallback, or aggregate.",
            default: json!("fallback"),
            allowed_values: vec!["single", "fallback", "aggregate"],
        },
        ConfigSchemaEntry {
            key: "web.search.providers",
            kind: "string_list",
            description: "Explicit WebSearch provider attempt order for auto fallback or aggregate mode.",
            default: json!([]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.max_results",
            kind: "positive_integer",
            description: "Maximum number of WebSearch results returned to the model.",
            default: json!(crate::web::WebSearchConfig::default().max_results),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.max_provider_attempts",
            kind: "positive_integer",
            description: "Maximum WebSearch providers attempted for fallback or aggregate routing.",
            default: json!(crate::web::WebSearchConfig::default().max_provider_attempts),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.kind",
            kind: "string",
            description: "Web search provider kind: duck_duck_go, searxng, brave, tavily, exa, perplexity, firecrawl, open_ai_native, anthropic_native, gemini_native.",
            default: Value::Null,
            allowed_values: vec!["duck_duck_go", "searxng", "brave", "tavily", "exa", "perplexity", "firecrawl", "open_ai_native", "anthropic_native", "gemini_native"],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.base_url",
            kind: "string",
            description: "Optional custom base URL for the web search provider.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.capabilities",
            kind: "json_object",
            description: "Derived WebSearch provider capability metadata used for routing diagnostics.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.credential_profile",
            kind: "string",
            description: "Named credential profile to load the API key from. The profile must be of kind api_key.",
            default: Value::Null,
            allowed_values: vec![],
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
        "runtime.default_tool_output_tokens" => Ok(config
            .runtime
            .default_tool_output_tokens
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "runtime.max_tool_output_tokens" => Ok(config
            .runtime
            .max_tool_output_tokens
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
        "web.fetch.enabled" => Ok(config
            .web
            .fetch
            .enabled
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "web.fetch.max_chars" => Ok(config
            .web
            .fetch
            .max_chars
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.max_response_bytes" => Ok(config
            .web
            .fetch
            .max_response_bytes
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.timeout_seconds" => Ok(config
            .web
            .fetch
            .timeout_seconds
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.max_redirects" => Ok(config
            .web
            .fetch
            .max_redirects
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.allowed_hosts" => Ok(json!(config.web.fetch.allowed_hosts)),
        "web.fetch.denied_hosts" => Ok(json!(config.web.fetch.denied_hosts)),
        "web.search.enabled" => Ok(config
            .web
            .search
            .enabled
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "web.search.provider" => Ok(config
            .web
            .search
            .provider
            .as_ref()
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null)),
        "web.search.mode" => Ok(config
            .web
            .search
            .mode
            .map(|value| Value::String(value.as_str().to_string()))
            .unwrap_or(Value::Null)),
        "web.search.providers" => Ok(json!(config.web.search.providers)),
        "web.search.max_results" => Ok(config
            .web
            .search
            .max_results
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.search.max_provider_attempts" => Ok(config
            .web
            .search
            .max_provider_attempts
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.providers" => Ok(serde_json::to_value(&config.web.providers)?),
        key if key.starts_with("web.providers.") => {
            let name = key.strip_prefix("web.providers.").unwrap();
            if let Some(provider_name) = name.strip_suffix(".kind") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .map(|p| Value::String(p.kind.as_str().to_string()))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".base_url") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.base_url.as_ref())
                    .map(|v| Value::String(v.clone()))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".capabilities") {
                return match config.web.providers.get(provider_name) {
                    Some(provider) => Ok(serde_json::to_value(provider.kind.capabilities())?),
                    None => Ok(Value::Null),
                };
            }
            if let Some(provider_name) = name.strip_suffix(".credential_profile") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.credential_profile.as_ref())
                    .map(|v| Value::String(v.clone()))
                    .unwrap_or(Value::Null));
            }
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name> requires a non-empty provider name"
                ));
            }
            match config.web.providers.get(name) {
                Some(provider) => Ok(serde_json::to_value(provider)?),
                None => Ok(Value::Null),
            }
        }
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
        "runtime.default_tool_output_tokens" => {
            config.runtime.default_tool_output_tokens = Some(
                parse_positive_u32_key(key, raw_value)?
                    .min(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32),
            );
        }
        "runtime.max_tool_output_tokens" => {
            config.runtime.max_tool_output_tokens = Some(
                parse_positive_u32_key(key, raw_value)?
                    .min(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32),
            );
        }
        "runtime.disable_provider_fallback" => {
            config.runtime.disable_provider_fallback = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "tui.alternate_screen" => {
            config.tui.alternate_screen = Some(AltScreenMode::parse(raw_value)?);
        }
        "web.fetch.enabled" => {
            config.web.fetch.enabled = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "web.fetch.max_chars" => {
            config.web.fetch.max_chars = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.fetch.max_response_bytes" => {
            config.web.fetch.max_response_bytes = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.fetch.timeout_seconds" => {
            config.web.fetch.timeout_seconds = Some(parse_positive_u64_key(key, raw_value)?);
        }
        "web.fetch.max_redirects" => {
            config.web.fetch.max_redirects = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.fetch.allowed_hosts" => {
            config.web.fetch.allowed_hosts = parse_string_list(raw_value)?;
        }
        "web.fetch.denied_hosts" => {
            config.web.fetch.denied_hosts = parse_string_list(raw_value)?;
        }
        "web.search.enabled" => {
            config.web.search.enabled = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "web.search.provider" => {
            let provider = raw_value.trim();
            if provider.is_empty() {
                return Err(anyhow!("{key} expects a non-empty provider id"));
            }
            config.web.search.provider = Some(provider.to_string());
        }
        "web.search.mode" => {
            config.web.search.mode = Some(
                serde_json::from_value(serde_json::Value::String(raw_value.trim().to_string()))
                    .with_context(|| format!("invalid web search mode: {}", raw_value))?,
            );
        }
        "web.search.providers" => {
            config.web.search.providers = parse_string_list(raw_value)?;
        }
        "web.search.max_results" => {
            config.web.search.max_results = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.search.max_provider_attempts" => {
            config.web.search.max_provider_attempts =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        key if key.starts_with("web.providers.") && key.ends_with(".kind") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".kind").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name>.kind requires a non-empty provider name"
                ));
            }
            let kind: WebProviderKind =
                serde_json::from_value(serde_json::Value::String(raw_value.trim().to_string()))
                    .with_context(|| format!("invalid web provider kind: {}", raw_value))?;
            config
                .web
                .providers
                .entry(name.to_string())
                .or_insert_with(|| WebProviderConfigFile {
                    kind,
                    base_url: None,
                    credential_profile: None,
                    command: None,
                    output: None,
                    limits: Default::default(),
                })
                .kind = kind;
        }
        key if key.starts_with("web.providers.") && key.ends_with(".capabilities") => {
            return Err(read_only_web_provider_capabilities_key_error(key));
        }
        key if key.starts_with("web.providers.") && key.ends_with(".base_url") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".base_url").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name>.base_url requires a non-empty provider name"
                ));
            }
            let provider = config.web.providers.get_mut(name).ok_or_else(|| {
                anyhow!("web provider {name} not found; set web.providers.{name}.kind first")
            })?;
            let value = raw_value.trim();
            provider.base_url = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        key if key.starts_with("web.providers.") && key.ends_with(".credential_profile") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".credential_profile").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name>.credential_profile requires a non-empty provider name"
                ));
            }
            let provider = config.web.providers.get_mut(name).ok_or_else(|| {
                anyhow!("web provider {name} not found; set web.providers.{name}.kind first")
            })?;
            let value = raw_value.trim();
            provider.credential_profile = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
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
        "runtime.default_tool_output_tokens" => config.runtime.default_tool_output_tokens = None,
        "runtime.max_tool_output_tokens" => config.runtime.max_tool_output_tokens = None,
        "runtime.disable_provider_fallback" => config.runtime.disable_provider_fallback = None,
        "tui.alternate_screen" => config.tui.alternate_screen = None,
        "web.fetch.enabled" => config.web.fetch.enabled = None,
        "web.fetch.max_chars" => config.web.fetch.max_chars = None,
        "web.fetch.max_response_bytes" => config.web.fetch.max_response_bytes = None,
        "web.fetch.timeout_seconds" => config.web.fetch.timeout_seconds = None,
        "web.fetch.max_redirects" => config.web.fetch.max_redirects = None,
        "web.fetch.allowed_hosts" => config.web.fetch.allowed_hosts.clear(),
        "web.fetch.denied_hosts" => config.web.fetch.denied_hosts.clear(),
        "web.search.enabled" => config.web.search.enabled = None,
        "web.search.provider" => config.web.search.provider = None,
        "web.search.mode" => config.web.search.mode = None,
        "web.search.providers" => config.web.search.providers.clear(),
        "web.search.max_results" => config.web.search.max_results = None,
        "web.search.max_provider_attempts" => config.web.search.max_provider_attempts = None,
        key if key.starts_with("web.providers.") && key.ends_with(".kind") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".kind").unwrap();
            if let Some(provider) = config.web.providers.get_mut(name) {
                provider.kind = WebProviderKind::DuckDuckGo;
            } else {
                return Err(anyhow!("web provider {name} not found"));
            }
        }
        key if key.starts_with("web.providers.") && key.ends_with(".capabilities") => {
            return Err(read_only_web_provider_capabilities_key_error(key));
        }
        key if key.starts_with("web.providers.") && key.ends_with(".base_url") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".base_url").unwrap();
            if let Some(provider) = config.web.providers.get_mut(name) {
                provider.base_url = None;
            } else {
                return Err(anyhow!("web provider {name} not found"));
            }
        }
        key if key.starts_with("web.providers.") && key.ends_with(".credential_profile") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".credential_profile").unwrap();
            if let Some(provider) = config.web.providers.get_mut(name) {
                provider.credential_profile = None;
            } else {
                return Err(anyhow!("web provider {name} not found"));
            }
        }
        key if key.starts_with("web.providers.") => {
            let name = key.strip_prefix("web.providers.").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name> requires a non-empty provider name"
                ));
            }
            if config.web.providers.remove(name).is_none() {
                return Err(anyhow!("web provider {name} not found"));
            }
            return Ok(());
        }
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
    let openai_codex_reasoning_effort = env::var("HOLON_OPENAI_CODEX_REASONING_EFFORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some("low".to_string()))
        .map(|value| validate_openai_reasoning_effort(&value).map(|_| value))
        .transpose()?;
    registry.insert(
        openai_codex.clone(),
        ProviderRuntimeConfig {
            id: openai_codex,
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: env::var("HOLON_OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api/codex".to_string()),
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
            reasoning_effort: openai_codex_reasoning_effort,
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
            reasoning_effort: None,
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
            reasoning_effort: None,
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
    insert_anthropic_compatible_provider(
        &mut registry,
        "deepseek",
        "https://api.deepseek.com/anthropic",
        &["DEEPSEEK_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "deepseek-anthropic",
        "https://api.deepseek.com/anthropic",
        &["DEEPSEEK_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "deepseek-openai",
        "https://api.deepseek.com/v1",
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
    insert_anthropic_compatible_provider(
        &mut registry,
        "xiaomi",
        "https://api.xiaomimimo.com/anthropic",
        &["XIAOMI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "xiaomi-anthropic",
        "https://api.xiaomimimo.com/anthropic",
        &["XIAOMI_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xiaomi-openai",
        "https://api.xiaomimimo.com/v1",
        &["XIAOMI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "xiaomi-token-plan",
        "https://token-plan-cn.xiaomimimo.com/anthropic",
        &["XIAOMI_TOKEN_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "xiaomi-token-plan-anthropic",
        "https://token-plan-cn.xiaomimimo.com/anthropic",
        &["XIAOMI_TOKEN_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xiaomi-token-plan-openai",
        "https://token-plan-cn.xiaomimimo.com/v1",
        &["XIAOMI_TOKEN_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xai",
        "https://api.x.ai/v1",
        &["XAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "zai",
        "https://api.z.ai/api/anthropic",
        &["ZAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "zai-anthropic",
        "https://api.z.ai/api/anthropic",
        &["ZAI_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "zai-openai",
        "https://api.z.ai/api/paas/v4",
        &["ZAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "bigmodel",
        "https://open.bigmodel.cn/api/anthropic",
        &["BIGMODEL_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "bigmodel-anthropic",
        "https://open.bigmodel.cn/api/anthropic",
        &["BIGMODEL_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "bigmodel-openai",
        "https://open.bigmodel.cn/api/paas/v4",
        &["BIGMODEL_API_KEY"],
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

/// Insert an Anthropic-compatible provider using Claude Code-like prompt-cache
/// lowering while avoiding implicit Claude-specific beta injection. Operators can
/// still override cache strategy and betas explicitly with env config.
fn insert_anthropic_compatible_provider(
    registry: &mut ProviderRegistry,
    provider: &str,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    insert_builtin_http_provider_with_context_management(
        registry,
        provider,
        ProviderTransportKind::AnthropicMessages,
        default_base_url,
        env_names,
        settings_env,
        resolve_anthropic_compatible_context_management_config()?,
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
    insert_builtin_http_provider_with_context_management(
        registry,
        provider,
        transport,
        default_base_url,
        env_names,
        settings_env,
        Default::default(),
    )
}

fn insert_builtin_http_provider_with_context_management(
    registry: &mut ProviderRegistry,
    provider: &str,
    transport: ProviderTransportKind,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
    context_management: AnthropicContextManagementConfig,
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
            reasoning_effort: None,
            context_management,
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
        reasoning_effort: None,
        context_management: Default::default(),
    });
    if let Some(reasoning_effort) = provider_config.reasoning_effort.as_deref() {
        validate_openai_reasoning_effort(reasoning_effort)?;
    }
    runtime.id = id;
    runtime.transport = provider_config.transport;
    runtime.base_url = provider_config.base_url;
    runtime.auth = provider_config.auth;
    runtime.credential = credential;
    if provider_config.reasoning_effort.is_some() {
        runtime.reasoning_effort = provider_config.reasoning_effort;
    }
    Ok(runtime)
}

fn validate_openai_reasoning_effort(value: &str) -> Result<()> {
    match value {
        "low" | "medium" | "high" | "xhigh" => Ok(()),
        _ => Err(anyhow!(
            "invalid OpenAI Codex reasoning_effort '{value}'; must be one of low, medium, high, xhigh"
        )),
    }
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
            base_url: "https://chatgpt.com/backend-api/codex".into(),
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
            reasoning_effort: Some("low".into()),
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
            reasoning_effort: None,
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
            reasoning_effort: None,
            context_management: Default::default(),
        },
    );
    registry
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

fn resolve_model_selection_from_explicit(
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

fn resolve_default_model(stored_config: &HolonConfigFile) -> Result<Option<ModelRef>> {
    if let Ok(value) = env::var("HOLON_MODEL") {
        return ModelRef::parse(&value).map(Some);
    }
    if let Some(value) = &stored_config.model.default {
        return ModelRef::parse(value).map(Some);
    }
    Ok(None)
}

fn resolve_fallback_models(stored_config: &HolonConfigFile) -> Result<Option<Vec<ModelRef>>> {
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

fn authenticated_model_candidates(
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

fn provider_has_usable_auth(provider: &ProviderRuntimeConfig) -> bool {
    match provider.auth.source {
        CredentialSource::Env | CredentialSource::AuthProfile => {
            provider.has_configured_credential()
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

fn provider_auth_priority(provider: &ProviderId) -> usize {
    match provider.as_str() {
        ProviderId::OPENAI_CODEX => 0,
        ProviderId::OPENAI => 1,
        ProviderId::ANTHROPIC => 2,
        _ => 100,
    }
}

fn preferred_override_model_for_provider(
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

fn dedupe_fallback_models(configured: Vec<ModelRef>, default_model: &ModelRef) -> Vec<ModelRef> {
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

fn parse_string_list(raw_value: &str) -> Result<Vec<String>> {
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

fn parse_positive_u64_key(key: &str, raw_value: &str) -> Result<u64> {
    raw_value
        .trim()
        .parse::<u64>()
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
        || config
            .web
            .providers
            .values()
            .any(|p| p.credential_profile.is_some())
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

fn read_only_web_provider_capabilities_key_error(key: &str) -> anyhow::Error {
    anyhow!(
        "{key} is derived read-only capability metadata; configure web.providers.<name>.kind instead"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard};

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
        CredentialSource, CredentialStoreFile, HolonConfigFile, ModelConfigFile, ModelRef,
        ProviderAuthConfig, ProviderConfigFile, ProviderId, ProviderRegistry,
        ProviderRuntimeConfig, ProviderTransportKind, RuntimeModelCatalog, DEFAULT_LOCAL_AGENT_ID,
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarSnapshot {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    struct EnvVarGuard {
        snapshots: Vec<EnvVarSnapshot>,
        _lock: MutexGuard<'static, ()>,
    }

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
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            Self {
                snapshots: Vec::new(),
                _lock: lock,
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
            config_file_path: home_path.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse(default_model).unwrap(),
            fallback_models: fallback_models
                .iter()
                .map(|value| ModelRef::parse(value).unwrap())
                .collect(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: HashMap::new(),
            validated_unknown_model_fallback: None,
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
                reasoning_effort: None,
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

        let deepseek = providers
            .get(&ProviderId::parse("deepseek").unwrap())
            .unwrap();
        assert_eq!(deepseek.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(deepseek.base_url, "https://api.deepseek.com/anthropic");
        assert_eq!(deepseek.credential.as_deref(), Some("deepseek-key"));

        let deepseek_anthropic = providers
            .get(&ProviderId::parse("deepseek-anthropic").unwrap())
            .unwrap();
        assert_eq!(
            deepseek_anthropic.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            deepseek_anthropic.base_url,
            "https://api.deepseek.com/anthropic"
        );
        assert_eq!(
            deepseek_anthropic.credential.as_deref(),
            Some("deepseek-key")
        );
        assert_eq!(
            deepseek_anthropic.context_management.cache_strategy,
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );

        let deepseek_openai = providers
            .get(&ProviderId::parse("deepseek-openai").unwrap())
            .unwrap();
        assert_eq!(
            deepseek_openai.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(deepseek_openai.base_url, "https://api.deepseek.com/v1");
        assert_eq!(deepseek_openai.credential.as_deref(), Some("deepseek-key"));

        let xiaomi = providers
            .get(&ProviderId::parse("xiaomi").unwrap())
            .unwrap();
        assert_eq!(xiaomi.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(xiaomi.base_url, "https://api.xiaomimimo.com/anthropic");
        assert_eq!(xiaomi.credential.as_deref(), Some("xiaomi-key"));
        assert_eq!(
            xiaomi.context_management.cache_strategy,
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );

        let xiaomi_anthropic = providers
            .get(&ProviderId::parse("xiaomi-anthropic").unwrap())
            .unwrap();
        assert_eq!(
            xiaomi_anthropic.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            xiaomi_anthropic.base_url,
            "https://api.xiaomimimo.com/anthropic"
        );
        assert_eq!(xiaomi_anthropic.credential.as_deref(), Some("xiaomi-key"));

        let xiaomi_openai = providers
            .get(&ProviderId::parse("xiaomi-openai").unwrap())
            .unwrap();
        assert_eq!(
            xiaomi_openai.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(xiaomi_openai.base_url, "https://api.xiaomimimo.com/v1");
        assert_eq!(xiaomi_openai.credential.as_deref(), Some("xiaomi-key"));

        let xiaomi_token_plan = providers
            .get(&ProviderId::parse("xiaomi-token-plan").unwrap())
            .unwrap();
        assert_eq!(
            xiaomi_token_plan.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            xiaomi_token_plan.base_url,
            "https://token-plan-cn.xiaomimimo.com/anthropic"
        );
        assert_eq!(
            xiaomi_token_plan.credential.as_deref(),
            Some("xiaomi-token-plan-key")
        );
        assert_eq!(
            xiaomi_token_plan.context_management.cache_strategy,
            AnthropicCacheStrategy::ClaudeCodePromptCache
        );

        let xiaomi_token_plan_anthropic = providers
            .get(&ProviderId::parse("xiaomi-token-plan-anthropic").unwrap())
            .unwrap();
        assert_eq!(
            xiaomi_token_plan_anthropic.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            xiaomi_token_plan_anthropic.base_url,
            "https://token-plan-cn.xiaomimimo.com/anthropic"
        );
        assert_eq!(
            xiaomi_token_plan_anthropic.credential.as_deref(),
            Some("xiaomi-token-plan-key")
        );

        let xiaomi_token_plan_openai = providers
            .get(&ProviderId::parse("xiaomi-token-plan-openai").unwrap())
            .unwrap();
        assert_eq!(
            xiaomi_token_plan_openai.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(
            xiaomi_token_plan_openai.base_url,
            "https://token-plan-cn.xiaomimimo.com/v1"
        );
        assert_eq!(
            xiaomi_token_plan_openai.credential.as_deref(),
            Some("xiaomi-token-plan-key")
        );

        let zai = providers.get(&ProviderId::parse("zai").unwrap()).unwrap();
        assert_eq!(zai.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(zai.base_url, "https://api.z.ai/api/anthropic");
        assert_eq!(zai.credential.as_deref(), Some("zai-key"));

        let zai_anthropic = providers
            .get(&ProviderId::parse("zai-anthropic").unwrap())
            .unwrap();
        assert_eq!(
            zai_anthropic.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(zai_anthropic.base_url, "https://api.z.ai/api/anthropic");

        let zai_openai = providers
            .get(&ProviderId::parse("zai-openai").unwrap())
            .unwrap();
        assert_eq!(
            zai_openai.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(zai_openai.base_url, "https://api.z.ai/api/paas/v4");
        assert_eq!(zai_openai.credential.as_deref(), Some("zai-key"));

        let bigmodel = providers
            .get(&ProviderId::parse("bigmodel").unwrap())
            .unwrap();
        assert_eq!(bigmodel.transport, ProviderTransportKind::AnthropicMessages);
        assert_eq!(bigmodel.base_url, "https://open.bigmodel.cn/api/anthropic");
        assert_eq!(bigmodel.credential.as_deref(), Some("bigmodel-key"));

        let bigmodel_anthropic = providers
            .get(&ProviderId::parse("bigmodel-anthropic").unwrap())
            .unwrap();
        assert_eq!(
            bigmodel_anthropic.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            bigmodel_anthropic.base_url,
            "https://open.bigmodel.cn/api/anthropic"
        );

        let bigmodel_openai = providers
            .get(&ProviderId::parse("bigmodel-openai").unwrap())
            .unwrap();
        assert_eq!(
            bigmodel_openai.transport,
            ProviderTransportKind::OpenAiChatCompletions
        );
        assert_eq!(
            bigmodel_openai.base_url,
            "https://open.bigmodel.cn/api/paas/v4"
        );
        assert_eq!(bigmodel_openai.credential.as_deref(), Some("bigmodel-key"));

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

        for provider in [
            "deepseek",
            "deepseek-anthropic",
            "xiaomi",
            "xiaomi-anthropic",
            "xiaomi-token-plan",
            "xiaomi-token-plan-anthropic",
            "zai",
            "zai-anthropic",
            "bigmodel",
            "bigmodel-anthropic",
            "minimax",
            "minimax-portal",
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
    fn built_in_provider_default_config_resolves_known_and_unknown_provider() {
        let settings_env = HashMap::new();

        let known = super::built_in_provider_default_config_with_settings(
            &ProviderId::parse("zai-anthropic").unwrap(),
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
                reasoning_effort: None,
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
                codex_home: None,
                originator: None,
                reasoning_effort: None,
                context_management: Default::default(),
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
