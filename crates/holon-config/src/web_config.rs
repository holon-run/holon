use crate::models::{
    authenticated_provider_ids, preferred_override_model_for_provider, ModelRef, ModelRouteRef,
};
use crate::CredentialStoreFile;
use crate::{ProviderId, ProviderRegistry, ProviderRuntimeConfig};

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use holon_model_catalog::{BuiltInModelCatalog, ModelRuntimeOverride};
use holon_provider::provider::ProviderNativeWebSearchKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[doc(hidden)]
pub fn deepseek_tool_url(provider: &WebProviderConfig, tool: &str) -> Result<url::Url> {
    let base = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.deepseek.com/v1");
    let mut url = url::Url::parse(base).map_err(|_| anyhow!("invalid DeepSeek base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "DeepSeek base URL must be HTTP(S) with a host and no credentials, query, or fragment"
        ));
    }
    let path = url.path().trim_end_matches('/');
    let prefix = if path.is_empty() { "/v1" } else { path };
    url.set_path(&format!("{prefix}/tools/{tool}"));
    Ok(url)
}

/// Builds the documented Tavily Extract endpoint from a provider's base URL.
///
/// Mirrors `deepseek_tool_url` so provider discovery and the outbound call share
/// one base URL shape check instead of an unchecked string concatenation.
pub fn tavily_extract_url(provider: &WebProviderConfig) -> Result<url::Url> {
    let base = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.tavily.com");
    let mut url = url::Url::parse(base).map_err(|_| anyhow!("invalid Tavily base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "Tavily base URL must be HTTP(S) with a host and no credentials, query, or fragment"
        ));
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{path}/extract"));
    Ok(url)
}

/// Builds the documented Firecrawl Scrape v2 endpoint from a provider's base URL.
///
/// Mirrors the DeepSeek/Tavily endpoint builders so provider discovery and the
/// outbound call share one base URL shape check. A trailing `/v1` or `/v2`
/// version segment is normalized away before appending `/v2/scrape`, so the
/// official base (which already contains `/v2`) never ends up doubled.
pub fn firecrawl_scrape_url(provider: &WebProviderConfig) -> Result<url::Url> {
    let base = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.firecrawl.dev");
    let mut url = url::Url::parse(base).map_err(|_| anyhow!("invalid Firecrawl base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "Firecrawl base URL must be HTTP(S) with a host and no credentials, query, or fragment"
        ));
    }
    let path = url.path().trim_end_matches('/');
    let path = path
        .strip_suffix("/v1")
        .or_else(|| path.strip_suffix("/v2"))
        .unwrap_or(path);
    url.set_path(&format!("{path}/v2/scrape"));
    Ok(url)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebConfig {
    pub fetch: WebFetchConfig,
    pub search: WebSearchConfig,
    pub providers: BTreeMap<String, WebProviderConfig>,
}

impl WebConfig {
    pub fn native_search_provider(&self) -> Option<(String, WebProviderKind)> {
        let provider_id = self.search.provider.trim();
        if !self.search.enabled || provider_id.is_empty() || provider_id == "auto" {
            return None;
        }
        self.providers.get(provider_id).and_then(|provider| {
            provider
                .kind
                .is_native_search()
                .then(|| (provider_id.to_string(), provider.kind))
        })
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            fetch: WebFetchConfig::default(),
            search: WebSearchConfig::default(),
            providers: BTreeMap::new(),
        }
    }
}

impl TryFrom<&crate::WebConfigFile> for WebConfig {
    type Error = anyhow::Error;

    fn try_from(value: &crate::WebConfigFile) -> Result<Self> {
        Ok(Self {
            fetch: WebFetchConfig::from(&value.fetch),
            search: WebSearchConfig::from_file(&value.search)?,
            providers: value
                .providers
                .iter()
                .map(|(id, provider)| {
                    WebProviderConfig::from_file(id, provider)
                        .map(|provider| (id.clone(), provider))
                })
                .collect::<Result<BTreeMap<_, _>>>()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebFetchConfig {
    pub enabled: bool,
    pub max_chars: usize,
    pub max_response_bytes: usize,
    pub timeout_seconds: u64,
    pub max_redirects: usize,
    pub allowed_hosts: Vec<String>,
    pub denied_hosts: Vec<String>,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_chars: 20_000,
            max_response_bytes: 750_000,
            timeout_seconds: 20,
            max_redirects: 5,
            allowed_hosts: Vec::new(),
            denied_hosts: Vec::new(),
        }
    }
}

impl From<&crate::WebFetchConfigFile> for WebFetchConfig {
    fn from(value: &crate::WebFetchConfigFile) -> Self {
        let fallback = Self::default();
        Self {
            enabled: value.enabled.unwrap_or(fallback.enabled),
            max_chars: value.max_chars.unwrap_or(fallback.max_chars),
            max_response_bytes: value
                .max_response_bytes
                .unwrap_or(fallback.max_response_bytes),
            timeout_seconds: value.timeout_seconds.unwrap_or(fallback.timeout_seconds),
            max_redirects: value.max_redirects.unwrap_or(fallback.max_redirects),
            allowed_hosts: value.allowed_hosts.clone(),
            denied_hosts: value.denied_hosts.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchConfig {
    pub enabled: bool,
    pub builtin_provider_enabled: bool,
    pub provider: String,
    pub native_search_default: NativeSearchDefault,
    pub mode: WebSearchMode,
    pub providers: Vec<String>,
    pub max_results: usize,
    pub max_provider_attempts: usize,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtin_provider_enabled: true,
            provider: "auto".into(),
            native_search_default: NativeSearchDefault::default(),
            mode: WebSearchMode::Fallback,
            providers: Vec::new(),
            max_results: 5,
            max_provider_attempts: 3,
        }
    }
}

/// Explicit override for deriving standalone native WebSearch entries from
/// authenticated model providers (see `derived_native_search_providers`).
///
/// Mirrors `vision.default`: an explicit value pins the selection and `auto`
/// clears the override back to automatic derivation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeSearchDefault {
    /// Derive one native entry per eligible authenticated model provider.
    Auto,
    /// Disable derivation entirely.
    Off,
    /// Derive only the named model provider.
    Provider(String),
}

impl Default for NativeSearchDefault {
    fn default() -> Self {
        Self::Auto
    }
}

impl NativeSearchDefault {
    /// Parse the `web.search.native_default` value: unset, empty, or `auto`
    /// clears the override, `off` disables derivation, and any other value
    /// must be a valid model provider id.
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(Self::Auto);
        };
        if raw.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        if raw.eq_ignore_ascii_case("off") {
            return Ok(Self::Off);
        }
        ProviderId::parse(raw)
            .map(|_| Self::Provider(raw.to_string()))
            .map_err(|error| {
                anyhow!(
                    "web.search.native_default must be auto, off, or a valid provider id: {error}"
                )
            })
    }
}

impl WebSearchConfig {
    fn from_file(value: &crate::WebSearchConfigFile) -> Result<Self> {
        let fallback = Self::default();
        Ok(Self {
            enabled: value.enabled.unwrap_or(fallback.enabled),
            builtin_provider_enabled: value
                .builtin_provider
                .enabled
                .unwrap_or(fallback.builtin_provider_enabled),
            provider: value
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|provider| !provider.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or(fallback.provider),
            native_search_default: NativeSearchDefault::parse(value.native_default.as_deref())?,
            mode: value.mode.unwrap_or(fallback.mode),
            providers: value
                .providers
                .iter()
                .map(|provider| provider.trim().to_string())
                .filter(|provider| !provider.is_empty())
                .collect(),
            max_results: value.max_results.unwrap_or(fallback.max_results),
            max_provider_attempts: value
                .max_provider_attempts
                .unwrap_or(fallback.max_provider_attempts)
                .max(1),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchMode {
    Single,
    Fallback,
    Aggregate,
}

impl WebSearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Fallback => "fallback",
            Self::Aggregate => "aggregate",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderAuthClass {
    None,
    ApiKey,
    /// Works without a key (public endpoint or free tier); a configured key
    /// only raises rate limits or unlocks paid usage.
    ApiKeyOptional,
    NativeProvider,
    SelfHosted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderCostClass {
    Free,
    SelfHosted,
    Paid,
    ProviderMetered,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderQualityHint {
    HtmlFallback,
    Keyword,
    Semantic,
    Research,
    Native,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderSupportStatus {
    Supported,
    Unsupported,
    NativeOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WebProviderCapabilityMetadata {
    pub auth: WebProviderAuthClass,
    pub cost_class: WebProviderCostClass,
    pub quality_hint: WebProviderQualityHint,
    pub supports_domain_filter: bool,
    pub supports_freshness: bool,
    pub supports_region_or_language: bool,
    pub supports_full_content: bool,
    pub supports_native_citations: bool,
    pub default_priority: u16,
    pub status: WebProviderSupportStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebProviderConfig {
    pub kind: WebProviderKind,
    pub base_url: Option<String>,
    /// Programmable Search engine id (`cx`); `Some` only for `kind=google`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_id: Option<String>,
    /// Resolved API key from a credential profile (for API-backed providers).
    /// Empty when no credential profile is configured.
    #[serde(skip)]
    pub api_key: String,
    /// Model route binding for native search kinds; `None` for other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_route: Option<String>,
    pub command: Option<WebCommandProviderConfig>,
    pub output: Option<WebCommandOutputConfig>,
    pub limits: WebProviderLimitsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCommandProviderConfig {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCommandOutputConfig {
    pub format: WebCommandOutputFormat,
    pub mapping: WebCommandResultMapping,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebCommandOutputFormat {
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCommandResultMapping {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebProviderLimitsConfig {
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl Default for WebProviderLimitsConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            max_output_bytes: 200_000,
        }
    }
}

impl WebProviderConfig {
    fn from_file(id: &str, value: &crate::WebProviderConfigFile) -> Result<Self> {
        validate_provider_config(id, value)?;
        Ok(Self {
            kind: value.kind,
            base_url: value.base_url.clone(),
            engine_id: value.engine_id.clone(),
            api_key: String::new(),
            model_route: value.model_route.clone(),
            command: value
                .command
                .as_ref()
                .map(|command| WebCommandProviderConfig {
                    argv: command.argv.clone(),
                }),
            output: value.output.as_ref().map(|output| WebCommandOutputConfig {
                format: output.format.into(),
                mapping: WebCommandResultMapping {
                    title: output.mapping.title.clone(),
                    url: output.mapping.url.clone(),
                    snippet: output.mapping.snippet.clone(),
                    published_at: output.mapping.published_at.clone(),
                },
            }),
            limits: WebProviderLimitsConfig {
                timeout_ms: value.limits.timeout_ms.unwrap_or(10_000).max(1),
                max_output_bytes: value.limits.max_output_bytes.unwrap_or(200_000).max(1),
            },
        })
    }
}

impl From<crate::WebCommandOutputFormatFile> for WebCommandOutputFormat {
    fn from(value: crate::WebCommandOutputFormatFile) -> Self {
        match value {
            crate::WebCommandOutputFormatFile::Json => Self::Json,
        }
    }
}

/// Materialize a resolved WebConfig from the file config and credential store.
pub fn materialize_web_config(
    file: &crate::WebConfigFile,
    credential_store: &CredentialStoreFile,
) -> Result<WebConfig> {
    let providers = file
        .providers
        .iter()
        .map(|(id, provider)| -> Result<(String, WebProviderConfig)> {
            let api_key = provider
                .credential_profile
                .as_deref()
                .and_then(|profile| {
                    credential_store
                        .profiles
                        .get(profile)
                        .filter(|entry| entry.kind == crate::CredentialKind::ApiKey)
                })
                .map(|entry| entry.material.clone())
                .unwrap_or_default();
            let mut provider_config = WebProviderConfig::from_file(id, provider)?;
            provider_config.api_key = api_key;
            Ok((id.clone(), provider_config))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    Ok(WebConfig {
        fetch: WebFetchConfig::from(&file.fetch),
        search: WebSearchConfig::from_file(&file.search)?,
        providers,
    })
}

/// Derive standalone native WebSearch provider entries for authenticated
/// model providers, mirroring the ViewImage vision selection layers: explicit
/// `web.providers` entries win over derived entries, an explicit
/// `web.search.native_default` value pins or disables derivation, and the
/// default derives one entry per eligible provider whose model route comes
/// from the model catalog preferred route (the same source of truth as the
/// model chain `auto` expansion).
///
/// Only providers whose declared builtin web search uses the native search
/// kind with standalone dispatch support are derivable; other declared kinds
/// stay conversation-only so every derived entry is actually callable, and
/// the reserved native web kinds are never materialized here.
pub fn derived_native_search_providers(
    web_config: &WebConfig,
    providers: &ProviderRegistry,
    model_overrides: &std::collections::HashMap<ModelRef, ModelRuntimeOverride>,
) -> Vec<(String, WebProviderConfig)> {
    if !web_config.search.enabled {
        return Vec::new();
    }
    let candidates = match &web_config.search.native_search_default {
        NativeSearchDefault::Off => return Vec::new(),
        NativeSearchDefault::Provider(pinned) => {
            let Some(provider_id) = ProviderId::parse(pinned).ok() else {
                return Vec::new();
            };
            vec![provider_id]
        }
        NativeSearchDefault::Auto => authenticated_provider_ids(providers),
    };
    let catalog = BuiltInModelCatalog::default();
    let mut derived = Vec::new();
    for provider_id in candidates {
        let Some(provider) = providers.get(&provider_id) else {
            continue;
        };
        if !crate::models::provider_has_usable_auth(provider)
            || !native_search_derivation_eligible(provider)
        {
            continue;
        }
        let id = provider_id.as_str().to_string();
        if matches!(id.as_str(), "auto" | "aggregate") || web_config.providers.contains_key(&id) {
            continue;
        }
        let Some(route) = catalog
            .preferred_route_for_provider(&provider_id)
            .or_else(|| {
                preferred_override_model_for_provider(&provider_id, model_overrides)
                    .map(|model_ref| ModelRouteRef::from_legacy_model_ref(&model_ref))
            })
        else {
            continue;
        };
        derived.push((
            id,
            WebProviderConfig {
                kind: WebProviderKind::OpenAiNative,
                base_url: None,
                engine_id: None,
                api_key: String::new(),
                model_route: Some(route.as_string()),
                command: None,
                output: None,
                limits: Default::default(),
            },
        ));
    }
    derived
}

/// A model provider is derivable when its declared builtin web search is
/// enabled and uses a native search kind with standalone dispatch support in
/// this Holon version.
fn native_search_derivation_eligible(provider: &ProviderRuntimeConfig) -> bool {
    provider
        .builtin_web_search
        .as_ref()
        .is_some_and(|capability| {
            capability.enabled && capability.kind == ProviderNativeWebSearchKind::OpenAi
        })
}

fn validate_provider_config(id: &str, provider: &crate::WebProviderConfigFile) -> Result<()> {
    if provider.kind == WebProviderKind::Google {
        let engine_id = provider.engine_id.as_deref().map(str::trim).unwrap_or("");
        if engine_id.is_empty() {
            return Err(anyhow!(
                "web provider `{id}` kind=google requires engine_id (the Programmable Search engine id, `cx`)"
            ));
        }
    } else if provider.engine_id.is_some() {
        return Err(anyhow!(
            "web provider `{id}` kind={} must not configure engine_id; engine_id is only supported for kind=google",
            provider.kind.as_str()
        ));
    }
    if provider.kind != WebProviderKind::Command {
        if provider.command.is_some() {
            return Err(anyhow!(
                "web provider `{id}` kind={} must not configure command.argv; command is only supported for kind=command",
                provider.kind.as_str()
            ));
        }
        if provider.output.is_some() {
            return Err(anyhow!(
                "web provider `{id}` kind={} must not configure output.mapping; command output is only supported for kind=command",
                provider.kind.as_str()
            ));
        }
        if provider.limits.timeout_ms.is_some() || provider.limits.max_output_bytes.is_some() {
            return Err(anyhow!(
                "web provider `{id}` kind={} must not configure command limits; limits are only supported for kind=command",
                provider.kind.as_str()
            ));
        }
        if !provider.kind.is_native_search() {
            if provider.model_route.is_some() {
                return Err(anyhow!(
                    "web provider `{id}` kind={} must not configure model_route; model_route is only supported for native search kinds",
                    provider.kind.as_str()
                ));
            }
        } else if provider
            .model_route
            .as_ref()
            .is_some_and(|route| route.trim().is_empty())
        {
            return Err(anyhow!(
                "web provider `{id}` kind={} has an empty model_route; bind a configured model route or remove model_route",
                provider.kind.as_str()
            ));
        }
        return Ok(());
    }
    let command = provider
        .command
        .as_ref()
        .ok_or_else(|| anyhow!("web provider `{id}` kind=command requires command.argv"))?;
    provider
        .output
        .as_ref()
        .ok_or_else(|| anyhow!("web provider `{id}` kind=command requires output.mapping"))?;
    let binary = command.argv.first().map(|arg| arg.trim()).unwrap_or("");
    if binary.is_empty() {
        return Err(anyhow!(
            "web provider `{id}` command.argv must not be empty"
        ));
    }
    let basename = binary.rsplit(['/', '\\']).next().unwrap_or(binary);
    if matches!(
        basename,
        "sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh"
    ) {
        return Err(anyhow!(
            "web provider `{id}` command binary `{binary}` is unsafe"
        ));
    }
    if command.argv.iter().any(|arg| arg.contains('\0')) {
        return Err(anyhow!(
            "web provider `{id}` command argv contains a NUL byte"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderKind {
    DuckDuckGo,
    Searxng,
    Brave,
    Google,
    #[serde(rename = "deepseek")]
    DeepSeek,
    TencentCloudWsa,
    Bocha,
    Tavily,
    Keenable,
    Exa,
    Perplexity,
    Firecrawl,
    OpenAiNative,
    AnthropicNative,
    GeminiNative,
    Command,
}

impl WebProviderKind {
    /// Must list every `WebProviderKind` variant exactly once.
    ///
    /// Missing entries are not caught at compile time, so keep this in sync
    /// when adding or removing enum variants.
    pub const ALL: [Self; 16] = [
        Self::DuckDuckGo,
        Self::Searxng,
        Self::Brave,
        Self::Google,
        Self::DeepSeek,
        Self::TencentCloudWsa,
        Self::Bocha,
        Self::Tavily,
        Self::Keenable,
        Self::Exa,
        Self::Perplexity,
        Self::Firecrawl,
        Self::OpenAiNative,
        Self::AnthropicNative,
        Self::GeminiNative,
        Self::Command,
    ];

    pub fn is_native_search(self) -> bool {
        matches!(
            self,
            WebProviderKind::OpenAiNative
                | WebProviderKind::AnthropicNative
                | WebProviderKind::GeminiNative
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            WebProviderKind::DuckDuckGo => "duck_duck_go",
            WebProviderKind::Searxng => "searxng",
            WebProviderKind::Brave => "brave",
            WebProviderKind::Google => "google",
            WebProviderKind::DeepSeek => "deepseek",
            WebProviderKind::TencentCloudWsa => "tencent_cloud_wsa",
            WebProviderKind::Bocha => "bocha",
            WebProviderKind::Tavily => "tavily",
            WebProviderKind::Keenable => "keenable",
            WebProviderKind::Exa => "exa",
            WebProviderKind::Perplexity => "perplexity",
            WebProviderKind::Firecrawl => "firecrawl",
            WebProviderKind::OpenAiNative => "open_ai_native",
            WebProviderKind::AnthropicNative => "anthropic_native",
            WebProviderKind::GeminiNative => "gemini_native",
            WebProviderKind::Command => "command",
        }
    }

    pub fn capabilities(&self) -> WebProviderCapabilityMetadata {
        match self {
            WebProviderKind::DeepSeek => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: true,
                supports_freshness: true,
                // DeepSeek's search API has no language or region parameter
                // surface (only an IP-based `localization` field), so these
                // hints cannot be consumed.
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 75,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::DuckDuckGo => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::None,
                cost_class: WebProviderCostClass::Free,
                quality_hint: WebProviderQualityHint::HtmlFallback,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 10,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Searxng => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::SelfHosted,
                cost_class: WebProviderCostClass::SelfHosted,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: true,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 50,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Brave => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: true,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 80,
                status: WebProviderSupportStatus::Supported,
            },
            // Custom Search JSON API: official Google endpoint behind a
            // Programmable Search engine; free tier then pay-per-query.
            WebProviderKind::Google => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: true,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 82,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::TencentCloudWsa => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: true,
                supports_freshness: true,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 78,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Bocha => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: false,
                supports_freshness: true,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 76,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Tavily => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: true,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: true,
                supports_native_citations: false,
                default_priority: 75,
                status: WebProviderSupportStatus::Supported,
            },
            // Keenable's public endpoint needs no key; a key only raises limits.
            WebProviderKind::Keenable => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKeyOptional,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 72,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Exa => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Semantic,
                supports_domain_filter: true,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: true,
                supports_native_citations: false,
                default_priority: 70,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Perplexity => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: false,
                supports_freshness: true,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: true,
                default_priority: 60,
                status: WebProviderSupportStatus::Supported,
            },
            // Firecrawl search works without a key (free tier); a key raises limits.
            WebProviderKind::Firecrawl => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKeyOptional,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: true,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: true,
                supports_native_citations: false,
                default_priority: 55,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::OpenAiNative
            | WebProviderKind::AnthropicNative
            | WebProviderKind::GeminiNative => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::NativeProvider,
                cost_class: WebProviderCostClass::ProviderMetered,
                quality_hint: WebProviderQualityHint::Native,
                supports_domain_filter: false,
                supports_freshness: true,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: true,
                default_priority: 65,
                status: WebProviderSupportStatus::NativeOnly,
            },
            WebProviderKind::Command => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::None,
                cost_class: WebProviderCostClass::Free,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 40,
                status: WebProviderSupportStatus::Supported,
            },
        }
    }
}

#[cfg(test)]
mod tests;
