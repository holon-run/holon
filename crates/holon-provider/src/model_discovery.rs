use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{
    CredentialKind, CredentialSource, ProviderEndpointId, ProviderId, ProviderRuntimeConfig,
    ProviderTransportKind, GITHUB_COPILOT_PROVIDER_ID,
};
use crate::provider::{transports::build_oauth_http_client, OpenAiBearerAuth};
use holon_domain::model_values::{
    provider_definition, tencent_tokenhub::is_tencent_tokenhub_model_id, BuiltInModelMetadata,
    ModelCapabilityFlags, ModelDiscoveryAuth, ModelDiscoveryDecoder, ModelDiscoveryDefinition,
    ModelDiscoveryRoute, ModelMetadataSource, ModelRef, ProviderCatalogPolicy,
};

const OPENAI_COMPATIBLE_MODELS_PATH: &str = "/models";
const OLLAMA_TAGS_PATH: &str = "/api/tags";
const OLLAMA_SHOW_PATH: &str = "/api/show";
const COPILOT_CHAT_ENDPOINT: &str = "chat-completions";
pub const DEFAULT_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Transport-level default discovery used for `openai_chat_completions`
/// providers that have no built-in discovery definition. Custom OpenAI-compatible
/// providers (for example a local Ollama endpoint) then participate in model
/// discovery through the standard `GET {base_url}/models` endpoint without a
/// provider-specific definition; built-in providers keep winning because their
/// explicit definitions are resolved first.
const OPENAI_COMPATIBLE_DEFAULT_DISCOVERY: ModelDiscoveryDefinition = ModelDiscoveryDefinition {
    auth: ModelDiscoveryAuth::Optional,
    route: ModelDiscoveryRoute::OpenAiCompatible,
    decoder: ModelDiscoveryDecoder::OpenAiCompatible,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelDiscoveryCacheFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<ProviderId, ProviderModelDiscoveryCache>,
}

impl ModelDiscoveryCacheFile {
    pub fn models(&self) -> HashMap<ModelRef, BuiltInModelMetadata> {
        self.providers
            .values()
            .flat_map(|provider| provider.models.iter().cloned())
            .map(|model| (model.model_ref.clone(), model))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModelDiscoveryCache {
    pub provider: ProviderId,
    pub fetched_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_hash: Option<String>,
    #[serde(default)]
    pub models: Vec<BuiltInModelMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDiscoveryRefreshReport {
    pub provider: ProviderId,
    pub fetched_at: DateTime<Utc>,
    pub model_count: usize,
    pub cache_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelDiscoveryCacheState {
    Fresh,
    Missing,
    Stale,
    Unsupported,
    Unauthenticated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelDiscoveryCacheStatus {
    pub provider: ProviderId,
    pub state: ModelDiscoveryCacheState,
    pub supports_auto_refresh: bool,
    pub credential_configured: bool,
    pub refresh_in_flight: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    pub ttl_seconds: u64,
    pub model_count: usize,
}

pub fn discovery_cache_path(home_dir: &Path) -> PathBuf {
    home_dir.join("model-discovery-cache.json")
}

pub fn provider_supports_model_discovery(provider: &ProviderRuntimeConfig) -> bool {
    provider_discovery_definition(provider).is_some()
}

pub fn provider_discovers_model_capabilities(provider: &ProviderRuntimeConfig) -> bool {
    provider_discovery_definition(provider)
        .is_some_and(|definition| definition.decoder == ModelDiscoveryDecoder::Ollama)
}

fn provider_model_discovery_requires_credential(provider: &ProviderRuntimeConfig) -> bool {
    provider_discovery_definition(provider)
        .is_some_and(|definition| definition.auth == ModelDiscoveryAuth::Required)
}

fn provider_discovery_definition(
    provider: &ProviderRuntimeConfig,
) -> Option<&'static ModelDiscoveryDefinition> {
    // Built-in providers win: their explicit discovery definitions (when any)
    // govern response parsing and endpoint construction, so existing behavior
    // is unchanged.
    if let Some(definition) = provider_definition(provider.id.as_str()) {
        let discovery = definition.discovery.as_ref()?;
        debug_assert_ne!(
            definition.catalog_policy,
            ProviderCatalogPolicy::StaticOnly,
            "providers with discovery must declare a discovery catalog policy"
        );
        return Some(discovery);
    }

    // Custom OpenAI-compatible providers fall back to a transport-level default
    // so they participate in model discovery without a built-in definition.
    if provider.transport == ProviderTransportKind::OpenAiChatCompletions {
        return Some(&OPENAI_COMPATIBLE_DEFAULT_DISCOVERY);
    }

    None
}

pub fn discovery_cache_status_for_provider(
    provider: &ProviderRuntimeConfig,
    cache: &ModelDiscoveryCacheFile,
    ttl: Duration,
    refresh_in_flight: bool,
) -> ModelDiscoveryCacheStatus {
    let supports_auto_refresh = provider_supports_model_discovery(provider);
    let credential_configured = provider.has_configured_credential();
    let entry = cache.providers.get(&provider.id);
    let age = entry.and_then(|entry| {
        Utc::now()
            .signed_duration_since(entry.fetched_at)
            .to_std()
            .ok()
    });
    let state = if !supports_auto_refresh {
        ModelDiscoveryCacheState::Unsupported
    } else if provider_model_discovery_requires_credential(provider) && !credential_configured {
        ModelDiscoveryCacheState::Unauthenticated
    } else if let Some(age) = age {
        if age <= ttl {
            ModelDiscoveryCacheState::Fresh
        } else {
            ModelDiscoveryCacheState::Stale
        }
    } else {
        ModelDiscoveryCacheState::Missing
    };

    ModelDiscoveryCacheStatus {
        provider: provider.id.clone(),
        state,
        supports_auto_refresh,
        credential_configured,
        refresh_in_flight,
        fetched_at: entry.map(|entry| entry.fetched_at),
        age_seconds: age.map(|age| age.as_secs()),
        ttl_seconds: ttl.as_secs(),
        model_count: entry.map(|entry| entry.models.len()).unwrap_or(0),
    }
}

pub fn discovery_cache_needs_refresh(
    provider: &ProviderRuntimeConfig,
    cache: &ModelDiscoveryCacheFile,
    ttl: Duration,
) -> bool {
    matches!(
        discovery_cache_status_for_provider(provider, cache, ttl, false).state,
        ModelDiscoveryCacheState::Missing | ModelDiscoveryCacheState::Stale
    )
}

pub fn load_discovery_cache_at(path: &Path) -> Result<ModelDiscoveryCacheFile> {
    if !path.exists() {
        return Ok(ModelDiscoveryCacheFile::default());
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read model discovery cache {}", path.display()))?;
    let mut cache: ModelDiscoveryCacheFile = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse model discovery cache {}", path.display()))?;
    let copilot = ProviderId::parse(GITHUB_COPILOT_PROVIDER_ID)?;
    if let Some(entry) = cache.providers.get_mut(&copilot) {
        for model in &mut entry.models {
            // Old Copilot caches used an absent endpoint for Chat and
            // "responses" for Responses. New entries record both explicitly.
            match model.endpoint.as_ref().map(ProviderEndpointId::as_str) {
                None => model.endpoint = Some(ProviderEndpointId::parse(COPILOT_CHAT_ENDPOINT)?),
                Some("responses") => model.endpoint = Some(ProviderEndpointId::default_endpoint()),
                _ => {}
            }
        }
    }
    Ok(cache)
}

pub fn save_discovery_cache_at(path: &Path, cache: &ModelDiscoveryCacheFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create model discovery cache dir {}",
                parent.display()
            )
        })?;
    }
    let bytes =
        serde_json::to_vec_pretty(cache).context("failed to serialize model discovery cache")?;
    fs::write(path, bytes)
        .with_context(|| format!("failed to write model discovery cache {}", path.display()))
}

pub async fn refresh_provider_models(
    provider: &ProviderRuntimeConfig,
    cache_path: &Path,
) -> Result<ModelDiscoveryRefreshReport> {
    let discovery = provider_discovery_definition(provider).ok_or_else(|| {
        anyhow!(
            "provider {} does not support model discovery yet",
            provider.id.as_str()
        )
    })?;

    let source_url = provider_models_url(provider)?;
    if discovery.decoder == ModelDiscoveryDecoder::Ollama {
        return refresh_ollama_models(provider, cache_path, source_url).await;
    }
    let mut response = provider_models_request(provider, discovery, &source_url)
        .await?
        .send()
        .await
        .with_context(|| format!("{} model discovery request failed", provider.id.as_str()))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        && discovery.auth != ModelDiscoveryAuth::Public
    {
        if let Some(auth) = discovery_oauth_auth(provider)? {
            let authorization = auth
                .refresh_authorization_header()
                .await?
                .context("OAuth model discovery refresh produced no authorization")?;
            response = provider_models_request_with_authorization(
                provider,
                &source_url,
                Some(authorization),
            )?
            .send()
            .await
            .with_context(|| format!("{} model discovery request failed", provider.id.as_str()))?;
        }
    }
    let response = response.error_for_status().with_context(|| {
        format!(
            "{} model discovery returned an error status",
            provider.id.as_str()
        )
    })?;
    let raw = response.bytes().await.with_context(|| {
        format!(
            "failed to read {} model discovery response",
            provider.id.as_str()
        )
    })?;
    let response_hash = format!("sha256:{}", sha256_hex(&raw));
    let fetched_at = Utc::now();
    let models = match discovery.decoder {
        ModelDiscoveryDecoder::Arcee => serde_json::from_slice::<ArceeModelsResponse>(&raw)
            .context("failed to parse Arcee model discovery response")?
            .into_model_metadata(&provider.id),
        ModelDiscoveryDecoder::HuggingFace => {
            serde_json::from_slice::<HuggingFaceModelsResponse>(&raw)
                .context("failed to parse Hugging Face model discovery response")?
                .into_model_metadata(&provider.id)
        }
        ModelDiscoveryDecoder::Kilo => serde_json::from_slice::<KiloModelsResponse>(&raw)
            .context("failed to parse Kilo Gateway model discovery response")?
            .into_model_metadata(&provider.id),
        ModelDiscoveryDecoder::OpenAiCompatible => {
            serde_json::from_slice::<OpenAiCompatibleModelsResponse>(&raw)
                .context("failed to parse OpenAI-compatible model discovery response")?
                .into_model_metadata(&provider.id)
        }
        ModelDiscoveryDecoder::Ollama => unreachable!("Ollama discovery is handled above"),
        ModelDiscoveryDecoder::NearAi => serde_json::from_slice::<NearAiModelsResponse>(&raw)
            .context("failed to parse NEAR AI model discovery response")?
            .into_model_metadata(&provider.id),
        ModelDiscoveryDecoder::OpenCodeGo => {
            serde_json::from_slice::<OpenCodeGoModelsResponse>(&raw)
                .context("failed to parse OpenCode Go model discovery response")?
                .into_model_metadata(&provider.id)
        }
        ModelDiscoveryDecoder::OpenRouter => {
            serde_json::from_slice::<OpenRouterModelsResponse>(&raw)
                .context("failed to parse OpenRouter model discovery response")?
                .into_model_metadata(&provider.id)
        }
        ModelDiscoveryDecoder::Synthetic => serde_json::from_slice::<SyntheticModelsResponse>(&raw)
            .context("failed to parse Synthetic model discovery response")?
            .into_model_metadata(&provider.id),
        ModelDiscoveryDecoder::TencentTokenHub => {
            serde_json::from_slice::<TencentTokenHubModelsResponse>(&raw)
                .context("failed to parse Tencent TokenHub model discovery response")?
                .into_model_metadata(&provider.id)
        }
        ModelDiscoveryDecoder::Venice => serde_json::from_slice::<VeniceModelsResponse>(&raw)
            .context("failed to parse Venice model discovery response")?
            .into_model_metadata(&provider.id, fetched_at),
        ModelDiscoveryDecoder::VercelAiGateway => {
            serde_json::from_slice::<VercelModelsResponse>(&raw)
                .context("failed to parse Vercel AI Gateway model discovery response")?
                .into_model_metadata(&provider.id)
        }
    };

    let mut cache = load_discovery_cache_at(cache_path)?;
    cache.providers.insert(
        provider.id.clone(),
        ProviderModelDiscoveryCache {
            provider: provider.id.clone(),
            fetched_at,
            source_url: Some(source_url),
            response_hash: Some(response_hash),
            models: models.clone(),
        },
    );
    save_discovery_cache_at(cache_path, &cache)?;

    Ok(ModelDiscoveryRefreshReport {
        provider: provider.id.clone(),
        fetched_at,
        model_count: models.len(),
        cache_path: cache_path.to_path_buf(),
    })
}

async fn provider_models_request(
    provider: &ProviderRuntimeConfig,
    discovery: &ModelDiscoveryDefinition,
    source_url: &str,
) -> Result<reqwest::RequestBuilder> {
    let authorization = match discovery.auth {
        ModelDiscoveryAuth::Public => None,
        ModelDiscoveryAuth::Required | ModelDiscoveryAuth::Optional => {
            if let Some(auth) = discovery_oauth_auth(provider)? {
                auth.resolve_authorization_header().await?
            } else {
                provider
                    .credential
                    .as_ref()
                    .map(|value| format!("Bearer {value}"))
            }
        }
    };
    provider_models_request_with_authorization(provider, source_url, authorization)
}

fn provider_models_request_with_authorization(
    provider: &ProviderRuntimeConfig,
    source_url: &str,
    authorization: Option<String>,
) -> Result<reqwest::RequestBuilder> {
    let mut request = reqwest::Client::builder()
        .user_agent("holon-model-discovery")
        .build()
        .context("failed to build model discovery HTTP client")?
        .get(source_url);
    // Discovery is part of the same provider request, so the configured
    // provider headers apply here too (for example the Copilot integration
    // identity).
    for (name, value) in provider.headers.iter() {
        request = request.header(name, value);
    }
    if let Some(authorization) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }
    Ok(request)
}

/// Discovery shares chat's refresh-and-persist lifecycle for managed OAuth
/// profiles; legacy opaque credentials keep their static bearer behavior.
fn discovery_oauth_auth(provider: &ProviderRuntimeConfig) -> Result<Option<OpenAiBearerAuth>> {
    let Some(credential) = provider.credential.as_deref() else {
        return Ok(None);
    };
    if !matches!(
        (provider.auth.source, provider.auth.kind),
        (CredentialSource::AuthProfile, CredentialKind::OAuth)
    ) {
        return Ok(None);
    }
    let profile = provider
        .auth
        .profile
        .as_deref()
        .unwrap_or_else(|| provider.id.as_str());
    // Only legacy opaque tokens may bypass material parsing. Malformed
    // structured documents fail closed before any credential is sent.
    let Some(_) = crate::auth::oauth_profile_material_credential(credential, profile)? else {
        return Ok(None);
    };
    OpenAiBearerAuth::from_runtime_config(provider, build_oauth_http_client()?).map(Some)
}

#[doc(hidden)]
pub fn provider_models_url(provider: &ProviderRuntimeConfig) -> Result<String> {
    let discovery = provider_discovery_definition(provider).ok_or_else(|| {
        anyhow!(
            "provider {} does not support model discovery yet",
            provider.id.as_str()
        )
    })?;
    match discovery.route {
        ModelDiscoveryRoute::Fixed(url) => Ok(url.to_string()),
        ModelDiscoveryRoute::OpenAiCompatible => openai_compatible_models_url(&provider.base_url),
        ModelDiscoveryRoute::Ollama => ollama_tags_url(&provider.base_url),
        ModelDiscoveryRoute::OpenAiV1 => openai_v1_models_url(&provider.base_url),
        ModelDiscoveryRoute::Venice => venice_models_url(&provider.base_url),
        ModelDiscoveryRoute::VercelAiGateway => vercel_models_url(&provider.base_url),
    }
}

fn ollama_tags_url(base_url: &str) -> Result<String> {
    ollama_api_url(base_url, OLLAMA_TAGS_PATH)
}

fn ollama_show_url(base_url: &str) -> Result<String> {
    ollama_api_url(base_url, OLLAMA_SHOW_PATH)
}

fn ollama_api_url(base_url: &str, api_path: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base_url)
        .with_context(|| format!("invalid Ollama base_url {base_url:?}"))?;
    let path = url.path().trim_end_matches('/');
    let path = path
        .strip_suffix("/v1")
        .unwrap_or(path)
        .trim_end_matches('/');
    url.set_path(&format!("{path}{api_path}"));
    Ok(url.to_string())
}

async fn refresh_ollama_models(
    provider: &ProviderRuntimeConfig,
    cache_path: &Path,
    source_url: String,
) -> Result<ModelDiscoveryRefreshReport> {
    let client = reqwest::Client::builder()
        .user_agent("holon-model-discovery")
        .build()
        .context("failed to build model discovery HTTP client")?;
    let request = client.get(&source_url);
    let request = if let Some(credential) = provider.credential.as_deref() {
        request.bearer_auth(credential)
    } else {
        request
    };
    let raw = request
        .send()
        .await
        .with_context(|| format!("{} model discovery request failed", provider.id.as_str()))?
        .error_for_status()
        .with_context(|| {
            format!(
                "{} model discovery returned an error status",
                provider.id.as_str()
            )
        })?
        .bytes()
        .await
        .context("failed to read Ollama tags response")?;
    let mut tags = serde_json::from_slice::<OllamaTagsResponse>(&raw)
        .context("failed to parse Ollama tags response")?;
    tags.models
        .sort_by(|left, right| left.name.cmp(&right.name));
    tags.models.dedup_by(|left, right| left.name == right.name);

    let show_url = ollama_show_url(&provider.base_url)?;
    let mut models = Vec::new();
    let mut show_failure_count = 0_usize;
    let mut first_show_failure = None;
    for tagged in tags.models {
        let name = tagged.name.trim();
        if name.is_empty() {
            continue;
        }
        match fetch_ollama_model(&client, provider, &show_url, name).await {
            Ok(model) => models.push(model),
            Err(error) => {
                show_failure_count += 1;
                first_show_failure.get_or_insert_with(|| format!("{error:#}"));
                tracing::warn!(
                    provider = provider.id.as_str(),
                    model = name,
                    error = ?error,
                    "skipping Ollama model whose show request failed"
                );
            }
        }
    }
    if models.is_empty() && show_failure_count > 0 {
        return Err(anyhow!(
            "all {show_failure_count} Ollama model show requests failed for provider {}; first failure: {}",
            provider.id.as_str(),
            first_show_failure.as_deref().unwrap_or("unknown error")
        ));
    }
    models.sort_by(|left, right| left.model_ref.model.cmp(&right.model_ref.model));

    let response_hash = format!(
        "sha256:{}",
        sha256_hex(
            &serde_json::to_vec(&models)
                .context("failed to serialize Ollama discovery result for hashing")?
        )
    );
    let fetched_at = Utc::now();
    let mut cache = load_discovery_cache_at(cache_path)?;
    cache.providers.insert(
        provider.id.clone(),
        ProviderModelDiscoveryCache {
            provider: provider.id.clone(),
            fetched_at,
            source_url: Some(source_url),
            response_hash: Some(response_hash),
            models: models.clone(),
        },
    );
    save_discovery_cache_at(cache_path, &cache)?;

    Ok(ModelDiscoveryRefreshReport {
        provider: provider.id.clone(),
        fetched_at,
        model_count: models.len(),
        cache_path: cache_path.to_path_buf(),
    })
}

async fn fetch_ollama_model(
    client: &reqwest::Client,
    provider: &ProviderRuntimeConfig,
    show_url: &str,
    name: &str,
) -> Result<BuiltInModelMetadata> {
    let request = client
        .post(show_url)
        .json(&OllamaShowRequest { model: name });
    let request = if let Some(credential) = provider.credential.as_deref() {
        request.bearer_auth(credential)
    } else {
        request
    };
    let raw = request
        .send()
        .await
        .with_context(|| format!("Ollama show request failed for model {name:?}"))?
        .error_for_status()
        .with_context(|| format!("Ollama show returned an error status for model {name:?}"))?
        .bytes()
        .await
        .with_context(|| format!("failed to read Ollama show response for model {name:?}"))?;
    let show = serde_json::from_slice::<OllamaShowResponse>(&raw)
        .with_context(|| format!("failed to parse Ollama show response for model {name:?}"))?;
    Ok(show.into_model_metadata(&provider.id, name))
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaTaggedModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaTaggedModel {
    name: String,
}

#[derive(Serialize)]
struct OllamaShowRequest<'a> {
    model: &'a str,
}

#[derive(Debug, Deserialize)]
struct OllamaShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    model_info: BTreeMap<String, serde_json::Value>,
}

impl OllamaShowResponse {
    fn into_model_metadata(self, provider: &ProviderId, model: &str) -> BuiltInModelMetadata {
        let has_capability = |expected: &str| {
            self.capabilities
                .iter()
                .any(|capability| capability.eq_ignore_ascii_case(expected))
        };
        let context_window_tokens = self
            .model_info
            .iter()
            .filter(|(key, _)| key.ends_with(".context_length"))
            .filter_map(|(_, value)| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .or_else(|| value.as_str().and_then(|value| value.parse::<usize>().ok()))
            })
            .max();
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), model.to_string()),
            display_name: model.to_string(),
            description: "Remote discovered Ollama model.".to_string(),
            context_window_tokens,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input: has_capability("vision"),
                supports_reasoning: has_capability("thinking"),
                // Ollama's `tools` capability does not imply parallel tool calls.
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        }
    }
}

fn openai_v1_models_url(base_url: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base_url)
        .with_context(|| format!("invalid OpenAI-compatible base_url {base_url:?}"))?;
    let path = url.path().trim_end_matches('/');
    // LiteLLM commonly omits `/v1`; avoid duplicating it for providers that include it.
    if path.ends_with("/v1") {
        url.set_path(&format!("{path}/models"));
    } else {
        url.set_path(&format!("{path}/v1/models"));
    }
    Ok(url.to_string())
}

fn venice_models_url(base_url: &str) -> Result<String> {
    Ok(format!(
        "{}?type=text",
        openai_compatible_models_url(base_url)?
    ))
}

fn openai_compatible_models_url(base_url: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base_url)
        .with_context(|| format!("invalid OpenAI-compatible base_url {base_url:?}"))?;
    let path = url.path().trim_end_matches('/');
    url.set_path(&format!("{path}{OPENAI_COMPATIBLE_MODELS_PATH}"));
    Ok(url.to_string())
}

fn vercel_models_url(base_url: &str) -> Result<String> {
    // `base_url` is the gateway root (optionally with a deployment prefix), not a `/v1` API URL.
    let mut url = reqwest::Url::parse(base_url)
        .with_context(|| format!("invalid Vercel AI Gateway base_url {base_url:?}"))?;
    let path = url.path().trim_end_matches('/');
    url.set_path(&format!("{path}/v1/models"));
    Ok(url.to_string())
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiCompatibleModel>,
}

impl OpenAiCompatibleModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.model_ref.model.cmp(&right.model_ref.model));
        models
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleModel {
    id: String,
    #[serde(default)]
    supported_endpoints: Vec<String>,
}

impl OpenAiCompatibleModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        // Only Copilot uses supported_endpoints to select a transport. Record
        // its default explicitly to distinguish it from old cached Chat models.
        let endpoint = (provider.as_str() == GITHUB_COPILOT_PROVIDER_ID).then(|| {
            if self
                .supported_endpoints
                .iter()
                .any(|endpoint| endpoint == "/responses")
            {
                ProviderEndpointId::default_endpoint()
            } else {
                ProviderEndpointId::parse(COPILOT_CHAT_ENDPOINT)
                    .expect("valid Copilot chat-completions endpoint id")
            }
        });
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name: id.to_string(),
            description: "Remote discovered OpenAI-compatible model.".to_string(),
            context_window_tokens: None,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags::default(),
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint,
        })
    }
}

#[derive(Debug, Deserialize)]
struct TencentTokenHubModelsResponse {
    #[serde(default)]
    data: Vec<TencentTokenHubModel>,
}

impl TencentTokenHubModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.model_ref.model.cmp(&right.model_ref.model));
        models
    }
}

#[derive(Debug, Deserialize)]
struct TencentTokenHubModel {
    id: String,
}

impl TencentTokenHubModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        if !is_tencent_tokenhub_model_id(id) {
            return None;
        }
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name: id.to_string(),
            description:
                "Remote discovered Tencent TokenHub model confirmed by the official model table."
                    .to_string(),
            context_window_tokens: None,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags::default(),
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct VeniceModelsResponse {
    #[serde(default)]
    data: Vec<VeniceModel>,
}

impl VeniceModelsResponse {
    fn into_model_metadata(
        self,
        provider: &ProviderId,
        now: DateTime<Utc>,
    ) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider, now))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct VeniceModel {
    id: String,
    #[serde(rename = "type")]
    model_type: String,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    model_spec: VeniceModelSpec,
}

#[derive(Debug, Default, Deserialize)]
struct VeniceModelSpec {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "availableContextTokens")]
    available_context_tokens: Option<usize>,
    #[serde(default, rename = "maxCompletionTokens")]
    max_completion_tokens: Option<u32>,
    #[serde(default)]
    offline: bool,
    #[serde(default)]
    capabilities: VeniceModelCapabilities,
    #[serde(default)]
    deprecation: Option<VeniceModelDeprecation>,
}

#[derive(Debug, Default, Deserialize)]
struct VeniceModelCapabilities {
    #[serde(default, rename = "supportsVision")]
    supports_vision: bool,
    #[serde(default, rename = "supportsReasoning")]
    supports_reasoning: bool,
    #[serde(default, rename = "supportsReasoningEffort")]
    supports_reasoning_effort: bool,
    #[serde(default, rename = "reasoningEffortOptions")]
    reasoning_effort_options: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct VeniceModelDeprecation {
    #[serde(default, rename = "removesAt")]
    removes_at: Option<DateTime<Utc>>,
}

impl VeniceModel {
    fn into_model_metadata(
        self,
        provider: &ProviderId,
        now: DateTime<Utc>,
    ) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        // Exclude models at or past their removal time.
        if id.is_empty()
            || self.model_type != "text"
            || self.model_spec.offline
            || self
                .model_spec
                .deprecation
                .as_ref()
                .and_then(|deprecation| deprecation.removes_at)
                .is_some_and(|removes_at| removes_at <= now)
        {
            return None;
        }
        let display_name = self
            .model_spec
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string();
        let description = self
            .model_spec
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Remote discovered Venice text model metadata.")
            .to_string();
        let reasoning_effort_options = self
            .model_spec
            .capabilities
            .supports_reasoning_effort
            .then_some(self.model_spec.capabilities.reasoning_effort_options)
            .unwrap_or_default();
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name,
            description,
            context_window_tokens: self
                .model_spec
                .available_context_tokens
                .or(self.context_length),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: self.model_spec.max_completion_tokens,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input: self.model_spec.capabilities.supports_vision,
                supports_reasoning: self.model_spec.capabilities.supports_reasoning,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options,
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

pub use holon_domain::model_values::OPENCODE_GO_TEXT_MODELS;

#[derive(Debug, Deserialize)]
struct OpenCodeGoModelsResponse {
    #[serde(default)]
    data: Vec<OpenCodeGoModel>,
}

impl OpenCodeGoModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.model_ref.model.cmp(&right.model_ref.model));
        models
    }
}

#[derive(Debug, Deserialize)]
struct OpenCodeGoModel {
    id: String,
    #[serde(default)]
    api: BTreeMap<String, OpenCodeGoApi>,
    #[serde(default)]
    modalities: OpenCodeGoModalities,
    #[serde(default)]
    capabilities: OpenCodeGoCapabilities,
    #[serde(default)]
    limit: OpenCodeGoLimit,
}

#[derive(Debug, Deserialize)]
struct OpenCodeGoApi {
    base_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenCodeGoModalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenCodeGoCapabilities {
    #[serde(default)]
    reasoning: OpenCodeGoReasoning,
}

#[derive(Debug, Default, Deserialize)]
struct OpenCodeGoReasoning {
    #[serde(default)]
    supported: bool,
    #[serde(default)]
    effort: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenCodeGoLimit {
    context: Option<usize>,
    output: Option<u32>,
}

impl OpenCodeGoModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        // The reviewed LongCat 2.5 contract is native Go Chat only.
        let native_chat_only = id == "longcat-2.5";
        let preferred_endpoint = OPENCODE_GO_TEXT_MODELS
            .iter()
            .find_map(|(model, endpoint)| (*model == id).then_some(*endpoint))
            .or_else(|| match id {
                _ if native_chat_only => Some("default"),
                // Retain historical mappings without granting translation to
                // IDs no longer present in the reviewed current text table.
                "deepseek-v4-flash"
                | "glm-5.1"
                | "kimi-k2.6"
                | "longcat-2.5-preview-free"
                | "space-bunny"
                | "step-5-preview-free"
                | "cat-jam-free" => Some("default"),
                "minimax-m2.7" | "minimax-m2.5" | "qwen3.7-max" | "qwen3.6-plus" => {
                    Some("messages")
                }
                "grok-4.6" => Some("responses"),
                _ => None,
            })?;
        let endpoint = (preferred_endpoint != "default").then(|| {
            ProviderEndpointId::parse(preferred_endpoint).expect("valid OpenCode Go endpoint id")
        });
        let (api, base_url_provider) = match preferred_endpoint {
            "messages" => ("messages", "opencode-go-messages"),
            // Native metadata is independent of registered inference endpoints.
            // Responses uses the same published base URL as legacy Chat.
            "responses" => ("responses", "opencode-go"),
            _ => ("chat-completions", "opencode-go"),
        };
        // Global fields describe the published native API, not translated text
        // routes. Gemini's native Google API is not registered in Holon.
        let native = id != "gemini-4-argon"
            && self.api.get(api).is_some_and(|api| {
                provider_definition(base_url_provider).is_some_and(|definition| {
                    api.base_url.as_deref() == Some(definition.default_base_url)
                })
            });
        if native_chat_only && !native {
            return None;
        }
        let supports_reasoning = native && self.capabilities.reasoning.supported;
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name: id.to_string(),
            description:
                "Remote discovered OpenCode Go text model with its published native endpoint or retained legacy mapping; translated eligibility is limited to reviewed current IDs."
                    .to_string(),
            context_window_tokens: native.then_some(self.limit.context).flatten().filter(|v| *v > 0),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: native.then_some(self.limit.output).flatten().filter(|v| *v > 0),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input: native && self.modalities.input.iter().any(|input| {
                    input == "image" || input.starts_with("image/")
                }),
                supports_reasoning,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: if supports_reasoning {
                self.capabilities.reasoning.effort
            } else {
                Vec::new()
            },
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ArceeModelsResponse {
    #[serde(default)]
    data: Vec<ArceeModel>,
}

impl ArceeModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct ArceeModel {
    id: String,
}

impl ArceeModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        let (display_name, context_window_tokens) = match id {
            "trinity-mini" => ("Trinity Mini 26B", Some(131_072)),
            "trinity-large-preview" => ("Trinity Large Preview", Some(131_072)),
            _ => (id, None),
        };
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name: display_name.to_string(),
            description: "Remote discovered Arcee hosted model metadata.".to_string(),
            context_window_tokens,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags::default(),
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct HuggingFaceModelsResponse {
    #[serde(default)]
    data: Vec<HuggingFaceModel>,
}

impl HuggingFaceModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct HuggingFaceModel {
    id: String,
    #[serde(default)]
    architecture: Option<HuggingFaceArchitecture>,
    #[serde(default)]
    providers: Vec<HuggingFaceProvider>,
}

#[derive(Debug, Deserialize)]
struct HuggingFaceArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HuggingFaceProvider {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    context_length: Option<usize>,
}

impl HuggingFaceModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let live_providers = self
            .providers
            .iter()
            .filter(|route| route.status.as_deref() == Some("live"))
            .collect::<Vec<_>>();
        if live_providers.is_empty() {
            return None;
        }
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        let image_input = self.architecture.as_ref().is_some_and(|architecture| {
            architecture
                .input_modalities
                .iter()
                .any(|modality| modality.eq_ignore_ascii_case("image"))
        });
        let gpt_oss = matches!(id, "openai/gpt-oss-120b" | "openai/gpt-oss-20b");
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name: id.to_string(),
            description: "Remote discovered Hugging Face Inference Providers chat model metadata."
                .to_string(),
            context_window_tokens: live_providers
                .into_iter()
                .filter_map(|route| route.context_length)
                .max(),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input,
                supports_reasoning: gpt_oss,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: if gpt_oss {
                vec!["low".into(), "medium".into(), "high".into()]
            } else {
                Vec::new()
            },
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    #[serde(default)]
    data: Vec<OpenRouterModel>,
}

impl OpenRouterModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    architecture: Option<OpenRouterArchitecture>,
    #[serde(default)]
    top_provider: Option<OpenRouterTopProvider>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    reasoning: Option<OpenRouterReasoning>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterReasoning {
    #[serde(default)]
    mandatory: bool,
    #[serde(default)]
    default_enabled: bool,
    #[serde(default)]
    supported_efforts: Vec<String>,
}

impl OpenRouterModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        let display_name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string();
        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Remote discovered OpenRouter model metadata.")
            .to_string();
        let max_output_tokens_upper_limit = self
            .top_provider
            .as_ref()
            .and_then(|provider| provider.max_completion_tokens);
        let image_input = self
            .architecture
            .as_ref()
            .map(|architecture| {
                architecture
                    .input_modalities
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case("image"))
            })
            .unwrap_or(false);
        let supports_reasoning = self
            .supported_parameters
            .iter()
            .any(|parameter| parameter == "reasoning")
            || self.reasoning.as_ref().is_some_and(|reasoning| {
                reasoning.mandatory
                    || reasoning.default_enabled
                    || !reasoning.supported_efforts.is_empty()
            });
        let reasoning_effort_options = self
            .reasoning
            .as_ref()
            .filter(|_| {
                self.supported_parameters
                    .iter()
                    .any(|parameter| parameter == "reasoning_effort")
            })
            .map(|reasoning| reasoning.supported_efforts.clone())
            .unwrap_or_default();
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name,
            description,
            context_window_tokens: self.context_length,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input,
                supports_reasoning,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options,
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct NearAiModelsResponse {
    #[serde(default)]
    data: Vec<NearAiModel>,
}

impl NearAiModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct NearAiModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    max_output_length: Option<u32>,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    supported_features: Vec<String>,
    #[serde(default)]
    is_ready: Option<bool>,
}

impl NearAiModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        if self.is_ready != Some(true) {
            return None;
        }
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        let display_name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string();
        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Remote discovered NEAR AI Cloud model metadata.")
            .to_string();
        let has_input_modality = |expected: &str| {
            self.input_modalities
                .iter()
                .any(|modality| modality.eq_ignore_ascii_case(expected))
        };
        let has_feature = |expected: &str| {
            self.supported_features
                .iter()
                .any(|feature| feature.eq_ignore_ascii_case(expected))
        };
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name,
            description,
            context_window_tokens: self.context_length,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: self.max_output_length,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input: has_input_modality("image"),
                supports_reasoning: has_feature("reasoning"),
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SyntheticModelsResponse {
    #[serde(default)]
    data: Vec<SyntheticModel>,
}

impl SyntheticModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct SyntheticModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    always_on: bool,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    max_output_length: Option<u32>,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    supported_features: Vec<String>,
}

impl SyntheticModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        if !self.always_on {
            return None;
        }
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        let display_name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string();
        let has_input_modality = |expected: &str| {
            self.input_modalities
                .iter()
                .any(|modality| modality.eq_ignore_ascii_case(expected))
        };
        let has_feature = |expected: &str| {
            self.supported_features
                .iter()
                .any(|feature| feature.eq_ignore_ascii_case(expected))
        };
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name,
            description: "Remote discovered Synthetic always-on model metadata.".to_string(),
            context_window_tokens: self.context_length,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: self.max_output_length,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input: has_input_modality("image"),
                supports_reasoning: has_feature("reasoning"),
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTopProvider {
    #[serde(default)]
    max_completion_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct KiloModelsResponse {
    #[serde(default)]
    data: Vec<KiloModel>,
}

impl KiloModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct KiloModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    architecture: Option<OpenRouterArchitecture>,
    #[serde(default)]
    top_provider: Option<OpenRouterTopProvider>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    opencode: Option<KiloOpenCodeMetadata>,
}

#[derive(Debug, Deserialize)]
struct KiloOpenCodeMetadata {
    #[serde(default)]
    variants: BTreeMap<String, serde_json::Value>,
}

impl KiloModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        let display_name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string();
        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Remote discovered Kilo Gateway model metadata.")
            .to_string();
        let has_parameter = |expected: &str| {
            self.supported_parameters
                .iter()
                .any(|parameter| parameter == expected)
        };
        let image_input = self.architecture.as_ref().is_some_and(|architecture| {
            architecture
                .input_modalities
                .iter()
                .any(|value| value.eq_ignore_ascii_case("image"))
        });
        let reasoning_effort_options = if has_parameter("reasoning_effort") {
            self.opencode
                .as_ref()
                .map(|metadata| {
                    metadata
                        .variants
                        .keys()
                        .filter(|variant| {
                            matches!(
                                variant.as_str(),
                                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                            )
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name,
            description,
            context_window_tokens: self.context_length,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: self
                .top_provider
                .and_then(|provider| provider.max_completion_tokens),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input,
                supports_reasoning: has_parameter("reasoning")
                    || has_parameter("reasoning_effort")
                    || has_parameter("include_reasoning"),
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options,
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct VercelModelsResponse {
    #[serde(default)]
    data: Vec<VercelModel>,
}

impl VercelModelsResponse {
    fn into_model_metadata(self, provider: &ProviderId) -> Vec<BuiltInModelMetadata> {
        let mut models = self
            .data
            .into_iter()
            .filter_map(|model| model.into_model_metadata(provider))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }
}

#[derive(Debug, Deserialize)]
struct VercelModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    context_window: Option<usize>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(rename = "type", default)]
    model_type: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

impl VercelModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        if self.model_type.as_deref() != Some("language") {
            return None;
        }
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
        let display_name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string();
        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Remote discovered Vercel AI Gateway model metadata.")
            .to_string();
        let has_tag = |expected: &str| {
            self.tags
                .iter()
                .any(|tag| tag.eq_ignore_ascii_case(expected))
        };
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name,
            description,
            context_window_tokens: self.context_window,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: self.max_tokens,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input: has_tag("vision"),
                supports_reasoning: has_tag("reasoning"),
                // `tool-use` does not establish portable parallel tool-call semantics.
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::RemoteDiscovered,
            endpoint: None,
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests;

#[cfg(feature = "runtime-tests")]
#[doc(hidden)]
pub fn decode_opencode_go_models(
    provider: &ProviderId,
    payload: serde_json::Value,
) -> Result<Vec<BuiltInModelMetadata>> {
    Ok(serde_json::from_value::<OpenCodeGoModelsResponse>(payload)?.into_model_metadata(provider))
}
