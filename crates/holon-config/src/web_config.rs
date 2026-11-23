use crate::models::{
    authenticated_provider_ids, preferred_override_model_for_provider, provider_has_usable_auth,
    ModelRef, ModelRouteRef,
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

/// Official API roots for the Z.AI and BigModel standalone search endpoints.
/// The derived entries pin these roots, so a discovered model credential is
/// never sent to a custom address.
const ZAI_OFFICIAL_SEARCH_BASE: &str = "https://api.z.ai/api";
const BIGMODEL_OFFICIAL_SEARCH_BASE: &str = "https://open.bigmodel.cn/api";

/// Search engines accepted by Z.AI's `POST /paas/v4/web_search`.
///
/// Z.AI's OpenAPI reference lists `search-prime` as the only accepted
/// `search_engine` value. Its `count` and filter fields are documented as
/// supported only by `search_pro_jina`, which is not an accepted engine, so
/// those fields are not part of this contract.
pub const ZAI_SEARCH_ENGINES: [&str; 1] = ["search-prime"];
pub const ZAI_DEFAULT_SEARCH_ENGINE: &str = "search-prime";

/// Search engines accepted by BigModel's `POST /paas/v4/web_search`.
///
/// Kept separate from the Z.AI list on purpose: engine codes are station
/// contracts and must not be assumed to be interchangeable.
pub const BIGMODEL_SEARCH_ENGINES: [&str; 4] = [
    "search_std",
    "search_pro",
    "search_pro_sogou",
    "search_pro_quark",
];
pub const BIGMODEL_DEFAULT_SEARCH_ENGINE: &str = "search_pro";

/// Builds the documented Z.AI standalone search endpoint from a provider's
/// base URL, defaulting to the official API root.
pub fn zai_web_search_url(provider: &WebProviderConfig) -> Result<url::Url> {
    station_web_search_url(provider, ZAI_OFFICIAL_SEARCH_BASE, "Z.AI")
}

/// Builds the documented BigModel standalone search endpoint from a provider's
/// base URL, defaulting to the official API root.
pub fn bigmodel_web_search_url(provider: &WebProviderConfig) -> Result<url::Url> {
    station_web_search_url(provider, BIGMODEL_OFFICIAL_SEARCH_BASE, "BigModel")
}

fn station_web_search_url(
    provider: &WebProviderConfig,
    official_base: &str,
    label: &str,
) -> Result<url::Url> {
    let base = provider.base_url.as_deref().unwrap_or(official_base);
    let mut url = url::Url::parse(base).map_err(|_| anyhow!("invalid {label} base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "{label} base URL must be HTTP(S) with a host and no credentials, query, or fragment"
        ));
    }
    let path = url.path().trim_end_matches('/');
    // Empty paths get the documented `/api` root; an existing `/api` prefix is
    // kept so both `https://api.z.ai` and `https://api.z.ai/api` resolve to the
    // documented endpoint.
    let prefix = if path.is_empty() { "/api" } else { path };
    url.set_path(&format!("{prefix}/paas/v4/web_search"));
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
    /// Ids of providers derived from credential discovery rather than declared
    /// in `web.providers`. They stay listable and callable by explicit name but
    /// never join the default, `auto`, or `aggregate` candidate chain.
    #[serde(skip)]
    pub derived_providers: std::collections::BTreeSet<String>,
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
            derived_providers: std::collections::BTreeSet::new(),
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
            // Derived entries are discovered at runtime and never come from
            // the file config.
            derived_providers: std::collections::BTreeSet::new(),
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
/// stay conversation-only so every derived entry is actually callable; the
/// derived entry's kind follows the declared native search kind.
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
        if !crate::models::provider_has_usable_auth(provider) {
            continue;
        }
        let Some(provider_entry_kind) = native_search_derivation_kind(provider) else {
            continue;
        };
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
                kind: provider_entry_kind,
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
/// this Holon version; the returned kind selects the derived provider
/// entry's WebProviderKind.
fn native_search_derivation_kind(provider: &ProviderRuntimeConfig) -> Option<WebProviderKind> {
    provider
        .builtin_web_search
        .as_ref()
        .filter(|capability| capability.enabled)
        .and_then(|capability| match capability.kind {
            ProviderNativeWebSearchKind::OpenAi => Some(WebProviderKind::OpenAiNative),
            ProviderNativeWebSearchKind::Gemini => Some(WebProviderKind::GeminiNative),
            ProviderNativeWebSearchKind::Anthropic
            | ProviderNativeWebSearchKind::DeepSeek
            | ProviderNativeWebSearchKind::Xai => None,
        })
}

/// Stable WebSearch provider ids derived from official model credentials.
pub const DERIVED_DEEPSEEK_SEARCH_PROVIDER_ID: &str = "deepseek";
pub const DERIVED_ZAI_SEARCH_PROVIDER_ID: &str = "zai";
pub const DERIVED_BIGMODEL_SEARCH_PROVIDER_ID: &str = "bigmodel";

/// Official API hosts. A discovered credential is pinned to its station host;
/// keys resolved for proxies or compatible-protocol endpoints never derive a
/// standalone search entry, and one station never borrows another's key.
const DEEPSEEK_OFFICIAL_HOST: &str = "api.deepseek.com";
const ZAI_OFFICIAL_HOST: &str = "api.z.ai";
const BIGMODEL_OFFICIAL_HOST: &str = "open.bigmodel.cn";

struct StandaloneSearchSource {
    provider_id: &'static str,
    official_host: &'static str,
    kind: WebProviderKind,
}

const STANDALONE_SEARCH_SOURCES: [StandaloneSearchSource; 3] = [
    StandaloneSearchSource {
        provider_id: DERIVED_DEEPSEEK_SEARCH_PROVIDER_ID,
        official_host: DEEPSEEK_OFFICIAL_HOST,
        kind: WebProviderKind::DeepSeek,
    },
    StandaloneSearchSource {
        provider_id: DERIVED_ZAI_SEARCH_PROVIDER_ID,
        official_host: ZAI_OFFICIAL_HOST,
        kind: WebProviderKind::Zai,
    },
    StandaloneSearchSource {
        provider_id: DERIVED_BIGMODEL_SEARCH_PROVIDER_ID,
        official_host: BIGMODEL_OFFICIAL_HOST,
        kind: WebProviderKind::BigModel,
    },
];

/// Derive standalone WebSearch entries from official model credentials, so an
/// existing official key is callable without a separate `web.providers`
/// declaration. Each station derives at most once; binding both Z.AI and
/// BigModel yields one entry each.
///
/// A derived entry never carries the discovered endpoint: it pins the station's
/// official search endpoint, so a discovered key is never sent to a custom
/// address. Entries are explicit-only — callers hold them out of the default,
/// `auto`, and `aggregate` chains — and explicit `web.providers` entries win,
/// so callers must skip an id that is already configured.
pub fn derived_standalone_search_providers(
    web_config: &WebConfig,
    providers: &ProviderRegistry,
) -> Vec<(String, WebProviderConfig)> {
    if !web_config.search.enabled {
        return Vec::new();
    }
    let mut derived = Vec::new();
    for source in STANDALONE_SEARCH_SOURCES {
        let Some(provider) = providers.values().find(|provider| {
            (provider.id.as_str() == source.provider_id
                || provider.route_provider.as_str() == source.provider_id)
                && provider_has_usable_auth(provider)
                && is_official_endpoint(&provider.base_url, source.official_host)
        }) else {
            continue;
        };
        let Some(api_key) = provider
            .credential
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        derived.push((
            source.provider_id.to_string(),
            WebProviderConfig {
                kind: source.kind,
                // Pinned to the official endpoint: never the discovered base URL.
                base_url: None,
                engine_id: None,
                api_key: api_key.to_string(),
                model_route: None,
                command: None,
                output: None,
                limits: Default::default(),
            },
        ));
    }
    derived
}

/// True only for the official station host, so proxy or compatible-protocol
/// endpoints cannot borrow the derivation. Plain-`http` endpoints are rejected
/// too: the discovered key must never travel in cleartext.
fn is_official_endpoint(base_url: &str, official_host: &str) -> bool {
    url::Url::parse(base_url).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(official_host))
    })
}

/// Validate an optional station-specific search engine against that station's
/// published enum. An absent value keeps the station default.
fn validate_station_engine(
    id: &str,
    engine_id: Option<&str>,
    supported: &[&str],
    default_engine: &str,
) -> Result<()> {
    let Some(engine_id) = engine_id else {
        return Ok(());
    };
    if engine_id.is_empty() {
        return Err(anyhow!(
            "web provider `{id}` engine_id must not be empty; omit it to use `{default_engine}`"
        ));
    }
    if !supported.contains(&engine_id) {
        return Err(anyhow!(
            "web provider `{id}` engine_id `{engine_id}` is not supported; use one of {}",
            supported.join(", ")
        ));
    }
    Ok(())
}

fn validate_provider_config(id: &str, provider: &crate::WebProviderConfigFile) -> Result<()> {
    let engine_id = provider.engine_id.as_deref().map(str::trim);
    match provider.kind {
        WebProviderKind::Google => {
            if engine_id.unwrap_or("").is_empty() {
                return Err(anyhow!(
                    "web provider `{id}` kind=google requires engine_id (the Programmable Search engine id, `cx`)"
                ));
            }
        }
        WebProviderKind::Zai => validate_station_engine(
            id,
            engine_id,
            &ZAI_SEARCH_ENGINES,
            ZAI_DEFAULT_SEARCH_ENGINE,
        )?,
        WebProviderKind::BigModel => validate_station_engine(
            id,
            engine_id,
            &BIGMODEL_SEARCH_ENGINES,
            BIGMODEL_DEFAULT_SEARCH_ENGINE,
        )?,
        _ => {
            if provider.engine_id.is_some() {
                return Err(anyhow!(
                    "web provider `{id}` kind={} must not configure engine_id; engine_id is only supported for kind=google, zai, and bigmodel",
                    provider.kind.as_str()
                ));
            }
        }
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
    Zai,
    #[serde(rename = "bigmodel")]
    BigModel,
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
    pub const ALL: [Self; 18] = [
        Self::DuckDuckGo,
        Self::Searxng,
        Self::Brave,
        Self::Google,
        Self::DeepSeek,
        Self::Zai,
        Self::BigModel,
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
            WebProviderKind::Zai => "zai",
            WebProviderKind::BigModel => "bigmodel",
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
            // Z.AI's accepted `search-prime` engine documents no filter or
            // count surface: its filter fields apply only to `search_pro_jina`,
            // which is not an accepted engine. Do not advertise them.
            WebProviderKind::Zai => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 74,
                status: WebProviderSupportStatus::Supported,
            },
            // BigModel's documented request supports `count`, a domain filter,
            // and a recency filter; it has no language/region surface.
            WebProviderKind::BigModel => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: true,
                supports_freshness: true,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 73,
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
