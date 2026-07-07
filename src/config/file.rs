use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HolonConfigFile {
    #[serde(default, skip_serializing_if = "ApiConfigFile::is_empty")]
    pub api: ApiConfigFile,
    #[serde(default, skip_serializing_if = "ModelConfigFile::is_empty")]
    pub model: ModelConfigFile,
    #[serde(default, skip_serializing_if = "ModelsConfigFile::is_empty")]
    pub models: ModelsConfigFile,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: ProvidersConfigFile,
    #[serde(default, skip_serializing_if = "RuntimeConfigFile::is_empty")]
    pub runtime: RuntimeConfigFile,
    #[serde(default, skip_serializing_if = "VisionConfigFile::is_empty")]
    pub vision: VisionConfigFile,
    #[serde(default, skip_serializing_if = "ImageGenerationConfigFile::is_empty")]
    pub image_generation: ImageGenerationConfigFile,
    #[serde(default, skip_serializing_if = "TuiConfigFile::is_empty")]
    pub tui: TuiConfigFile,
    #[serde(default, skip_serializing_if = "WebConfigFile::is_empty")]
    pub web: WebConfigFile,
    #[serde(default, skip_serializing_if = "AgentTemplatesConfigFile::is_empty")]
    pub agent_templates: AgentTemplatesConfigFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ApiConfigFile {
    #[serde(default, skip_serializing_if = "ApiCorsConfigFile::is_empty")]
    pub cors: ApiCorsConfigFile,
}

impl ApiConfigFile {
    pub fn is_empty(&self) -> bool {
        self.cors.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiCorsConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
    #[serde(
        default = "default_api_cors_allowed_methods",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub allowed_methods: Vec<String>,
    #[serde(
        default = "default_api_cors_allowed_headers",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub allowed_headers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_credentials: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_seconds: Option<u64>,
}

impl Default for ApiCorsConfigFile {
    fn default() -> Self {
        Self {
            enabled: None,
            allowed_origins: vec![],
            allowed_methods: default_api_cors_allowed_methods(),
            allowed_headers: default_api_cors_allowed_headers(),
            allow_credentials: None,
            max_age_seconds: Some(600),
        }
    }
}

impl ApiCorsConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.allowed_origins.is_empty()
            && self.allowed_methods == default_api_cors_allowed_methods()
            && self.allowed_headers == default_api_cors_allowed_headers()
            && self.allow_credentials.is_none()
            && self.max_age_seconds == Some(600)
    }

    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn allow_credentials(&self) -> bool {
        self.allow_credentials.unwrap_or(false)
    }

    pub fn max_age_seconds(&self) -> u64 {
        self.max_age_seconds.unwrap_or(600)
    }
}

pub(crate) fn default_api_cors_allowed_methods() -> Vec<String> {
    ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub(crate) fn default_api_cors_allowed_headers() -> Vec<String> {
    ["content-type", "authorization"]
        .into_iter()
        .map(str::to_string)
        .collect()
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VisionConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageGenerationConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

pub type ProvidersConfigFile = BTreeMap<ProviderId, ProviderConfigFile>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfigFile {
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth: ProviderAuthConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
}

pub(crate) fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentTemplatesConfigFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub remote_sources: BTreeMap<String, AgentTemplateRemoteSourceConfigFile>,
}

impl AgentTemplatesConfigFile {
    pub fn is_empty(&self) -> bool {
        self.remote_sources.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentTemplateRemoteSourceConfigFile {
    pub url: String,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_profile: Option<String>,
}

impl AgentTemplateRemoteSourceConfigFile {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
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
    #[serde(
        default,
        skip_serializing_if = "WebSearchBuiltinProviderConfigFile::is_empty"
    )]
    pub builtin_provider: WebSearchBuiltinProviderConfigFile,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebSearchBuiltinProviderConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl WebSearchBuiltinProviderConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

impl WebSearchConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.builtin_provider.is_empty()
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
pub(crate) struct ClaudeSettings {
    #[serde(default)]
    pub(crate) env: HashMap<String, String>,
}

impl ModelConfigFile {
    pub(crate) fn is_empty(&self) -> bool {
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
    pub(crate) fn is_empty(&self) -> bool {
        self.catalog.is_empty()
    }
}

impl VisionConfigFile {
    pub(crate) fn is_empty(&self) -> bool {
        self.default.is_none()
    }
}

impl ImageGenerationConfigFile {
    pub(crate) fn is_empty(&self) -> bool {
        self.default.is_none()
    }
}

impl RuntimeConfigFile {
    pub(crate) fn is_empty(&self) -> bool {
        self.max_output_tokens.is_none() && self.disable_provider_fallback.is_none()
    }
}

impl TuiConfigFile {
    pub(crate) fn is_empty(&self) -> bool {
        self.alternate_screen.is_none()
    }
}

pub(crate) fn default_web_command_output() -> WebCommandOutputConfigFile {
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
