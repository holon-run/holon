use super::*;
use url::{Host, Url};

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

pub use holon_domain::model_values::{ProviderId, ProviderTransportKind, TransportCapabilities};

pub const DEFAULT_LOCAL_AGENT_ID: &str = "main";

pub type ProviderRegistry = BTreeMap<ProviderId, ProviderRuntimeConfig>;

pub const OPENAI_CODEX_CREDENTIAL_PROFILE: &str = "openai-codex";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRuntimeConfig {
    pub id: ProviderId,
    pub route_provider: ProviderId,
    pub route_endpoint: ProviderEndpointId,
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth: ProviderAuthConfig,
    pub headers: ProviderHeaders,
    pub credential: Option<String>,
    pub credential_store_path: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub originator: Option<String>,
    pub reasoning_effort: Option<String>,
    pub context_management: AnthropicContextManagementConfig,
    pub builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
}

impl ProviderRuntimeConfig {
    pub fn has_configured_credential(&self) -> bool {
        self.credential
            .as_ref()
            .map(|credential| !credential.trim().is_empty())
            .unwrap_or(false)
    }

    pub fn unauthenticated_cleartext_remote_warning(&self) -> Option<&'static str> {
        if self.auth.kind != CredentialKind::None {
            return None;
        }
        let url = Url::parse(&self.base_url).ok()?;
        if url.scheme() != "http" {
            return None;
        }
        let loopback = match url.host()? {
            Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
            Host::Ipv4(address) => address.is_loopback(),
            Host::Ipv6(address) => address.is_loopback(),
        };
        (!loopback).then_some(
            "Provider endpoint is remote, cleartext HTTP, and unauthenticated; network peers may observe or modify model traffic.",
        )
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

/// Per-endpoint prompt-cache capabilities for Anthropic-compatible
/// providers.
///
/// Endpoints with implicit-only caching ignore explicit `cache_control`
/// breakpoints (deepseek ignores them entirely; bigmodel/GLM caches
/// implicitly). Declaring that here lets the transport skip Claude-Code
/// cache mimicry (billing header, `metadata.user_id`, forced
/// `temperature=1.0`) instead of changing sampling behavior for no cache
/// benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicCacheCapabilities {
    /// Endpoint honors explicit `cache_control` breakpoints.
    pub cache_control: bool,
}

impl Default for AnthropicCacheCapabilities {
    fn default() -> Self {
        Self {
            cache_control: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicContextManagementConfig {
    pub enabled: bool,
    pub trigger_input_tokens: u32,
    pub keep_recent_tool_uses: u32,
    pub clear_at_least_input_tokens: Option<u32>,
    pub cache_strategy: AnthropicCacheStrategy,
    pub betas: Vec<String>,
    pub cache_capabilities: AnthropicCacheCapabilities,
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
            cache_capabilities: AnthropicCacheCapabilities::default(),
        }
    }
}

impl AnthropicContextManagementConfig {
    /// Marks the endpoint as implicit-cache-only: explicit
    /// `cache_control` breakpoints and Claude-Code cache mimicry
    /// (billing header, `metadata.user_id`, forced `temperature=1.0`)
    /// are skipped. The request then omits `temperature`, so the
    /// endpoint default applies; a caller-facing temperature
    /// passthrough does not exist yet.
    pub fn with_implicit_prompt_caching(mut self) -> Self {
        self.cache_capabilities.cache_control = false;
        self
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfigView {
    pub id: String,
    pub transport: String,
    pub base_url: String,
    pub auth: ProviderAuthView,
    pub credential_configured: bool,
    pub configured_in_config: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_names: Vec<String>,
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

impl ControlAuthMode {
    pub(crate) fn parse(value: &str) -> Result<Self> {
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
