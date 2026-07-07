use super::*;

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
    GeminiGenerateContent,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub const OPENAI_CODEX: &'static str = "openai-codex";
    pub const OPENAI: &'static str = "openai";
    pub const ANTHROPIC: &'static str = "anthropic";
    pub const GEMINI: &'static str = "gemini";
}

pub const DEFAULT_LOCAL_AGENT_ID: &str = "main";

pub type ProviderRegistry = BTreeMap<ProviderId, ProviderRuntimeConfig>;

pub const OPENAI_CODEX_CREDENTIAL_PROFILE: &str = "openai-codex";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRuntimeConfig {
    pub id: ProviderId,
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth: ProviderAuthConfig,
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

impl ProviderTransportKind {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai_codex_responses" => Ok(Self::OpenAiCodexResponses),
            "openai_responses" => Ok(Self::OpenAiResponses),
            "openai_chat_completions" => Ok(Self::OpenAiChatCompletions),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            "gemini_generate_content" => Ok(Self::GeminiGenerateContent),
            other => Err(anyhow!(
                "invalid provider transport {other}; expected openai_codex_responses|openai_responses|openai_chat_completions|anthropic_messages|gemini_generate_content"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCodexResponses => "openai_codex_responses",
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::AnthropicMessages => "anthropic_messages",
            Self::GeminiGenerateContent => "gemini_generate_content",
        }
    }

    pub fn supports_view_image_observation_generation(self) -> bool {
        matches!(
            self,
            Self::OpenAiCodexResponses
                | Self::OpenAiResponses
                | Self::OpenAiChatCompletions
                | Self::AnthropicMessages
        )
    }

    pub fn supports_image_generation(self) -> bool {
        matches!(
            self,
            Self::OpenAiCodexResponses | Self::OpenAiResponses | Self::OpenAiChatCompletions
        )
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

    pub fn gemini() -> Self {
        Self(Self::GEMINI.to_string())
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

    pub fn is_gemini(&self) -> bool {
        self.as_str() == Self::GEMINI
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
