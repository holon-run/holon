use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    config::{ModelRef, ProviderEndpointId, ProviderId, ProviderRuntimeConfig},
    model_catalog::{
        is_tencent_tokenhub_model_id, BuiltInModelMetadata, ModelCapabilityFlags,
        ModelMetadataSource,
    },
};

const OPENAI_COMPATIBLE_MODELS_PATH: &str = "/models";
const SYNTHETIC_MODELS_URL: &str = "https://api.synthetic.new/openai/v1/models";
pub const DEFAULT_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

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
    matches!(
        provider.id.as_str(),
        "arcee"
            | "huggingface"
            | "kilocode"
            | "litellm"
            | "nearai"
            | "opencode-go"
            | "openrouter"
            | "synthetic"
            | "tencent-tokenhub"
            | "venice"
            | "vercel-ai-gateway"
            | "vllm"
    )
}

fn provider_model_discovery_requires_credential(provider: &ProviderRuntimeConfig) -> bool {
    !matches!(
        provider.id.as_str(),
        "huggingface"
            | "kilocode"
            | "nearai"
            | "opencode-go"
            | "synthetic"
            | "venice"
            | "vercel-ai-gateway"
            | "vllm"
    )
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
    serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse model discovery cache {}", path.display()))
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
    if !provider_supports_model_discovery(provider) {
        return Err(anyhow!(
            "provider {} does not support model discovery yet",
            provider.id.as_str()
        ));
    }

    let source_url = provider_models_url(provider)?;
    let request = reqwest::Client::builder()
        .user_agent("holon-model-discovery")
        .build()
        .context("failed to build model discovery HTTP client")?
        .get(&source_url);
    let request = if let Some(credential) = provider.credential.as_deref() {
        request.bearer_auth(credential)
    } else {
        request
    };
    let response = request
        .send()
        .await
        .with_context(|| format!("{} model discovery request failed", provider.id.as_str()))?
        .error_for_status()
        .with_context(|| {
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
    let models = match provider.id.as_str() {
        "arcee" => serde_json::from_slice::<ArceeModelsResponse>(&raw)
            .context("failed to parse Arcee model discovery response")?
            .into_model_metadata(&provider.id),
        "huggingface" => serde_json::from_slice::<HuggingFaceModelsResponse>(&raw)
            .context("failed to parse Hugging Face model discovery response")?
            .into_model_metadata(&provider.id),
        "kilocode" => serde_json::from_slice::<KiloModelsResponse>(&raw)
            .context("failed to parse Kilo Gateway model discovery response")?
            .into_model_metadata(&provider.id),
        "litellm" | "vllm" => serde_json::from_slice::<OpenAiCompatibleModelsResponse>(&raw)
            .context("failed to parse OpenAI-compatible model discovery response")?
            .into_model_metadata(&provider.id),
        "nearai" => serde_json::from_slice::<NearAiModelsResponse>(&raw)
            .context("failed to parse NEAR AI model discovery response")?
            .into_model_metadata(&provider.id),
        "opencode-go" => serde_json::from_slice::<OpenCodeGoModelsResponse>(&raw)
            .context("failed to parse OpenCode Go model discovery response")?
            .into_model_metadata(&provider.id),
        "openrouter" => serde_json::from_slice::<OpenRouterModelsResponse>(&raw)
            .context("failed to parse OpenRouter model discovery response")?
            .into_model_metadata(&provider.id),
        "synthetic" => serde_json::from_slice::<SyntheticModelsResponse>(&raw)
            .context("failed to parse Synthetic model discovery response")?
            .into_model_metadata(&provider.id),
        "tencent-tokenhub" => serde_json::from_slice::<TencentTokenHubModelsResponse>(&raw)
            .context("failed to parse Tencent TokenHub model discovery response")?
            .into_model_metadata(&provider.id),
        "venice" => serde_json::from_slice::<VeniceModelsResponse>(&raw)
            .context("failed to parse Venice model discovery response")?
            .into_model_metadata(&provider.id, Utc::now()),
        "vercel-ai-gateway" => serde_json::from_slice::<VercelModelsResponse>(&raw)
            .context("failed to parse Vercel AI Gateway model discovery response")?
            .into_model_metadata(&provider.id),
        _ => unreachable!("unsupported providers returned above"),
    };
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

fn provider_models_url(provider: &ProviderRuntimeConfig) -> Result<String> {
    match provider.id.as_str() {
        "arcee" => Ok("https://api.arcee.ai/api/v1/models".to_string()),
        "huggingface" => openai_compatible_models_url(&provider.base_url),
        "kilocode" => openai_compatible_models_url(&provider.base_url),
        "litellm" => openai_v1_models_url(&provider.base_url),
        "nearai" => openai_compatible_models_url(&provider.base_url),
        "opencode-go" => openai_compatible_models_url(&provider.base_url),
        "openrouter" => openrouter_models_url(&provider.base_url),
        "synthetic" => Ok(SYNTHETIC_MODELS_URL.to_string()),
        "tencent-tokenhub" => openai_compatible_models_url(&provider.base_url),
        "venice" => venice_models_url(&provider.base_url),
        "vercel-ai-gateway" => vercel_models_url(&provider.base_url),
        "vllm" => openai_compatible_models_url(&provider.base_url),
        _ => Err(anyhow!(
            "provider {} does not support model discovery yet",
            provider.id.as_str()
        )),
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

fn openrouter_models_url(base_url: &str) -> Result<String> {
    openai_compatible_models_url(base_url)
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
}

impl OpenAiCompatibleModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        if id.is_empty() {
            return None;
        }
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
            endpoint: None,
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
}

impl OpenCodeGoModel {
    fn into_model_metadata(self, provider: &ProviderId) -> Option<BuiltInModelMetadata> {
        let id = self.id.trim();
        let endpoint = match id {
            "deepseek-v4-pro" | "deepseek-v4-flash" | "glm-5.2" | "glm-5.1" | "kimi-k2.7-code"
            | "kimi-k2.6" | "mimo-v2.5-pro" | "mimo-v2.5" => None,
            "minimax-m3" | "minimax-m2.7" | "minimax-m2.5" | "qwen3.7-max" | "qwen3.7-plus"
            | "qwen3.6-plus" => {
                Some(ProviderEndpointId::parse("messages").expect("valid OpenCode Go endpoint id"))
            }
            _ => return None,
        };
        Some(BuiltInModelMetadata {
            model_ref: ModelRef::new(provider.clone(), id.to_string()),
            display_name: id.to_string(),
            description:
                "Remote discovered OpenCode Go route confirmed by the official endpoint table."
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
mod tests {
    use super::*;

    fn arcee_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("arcee").unwrap(),
            route_provider: ProviderId::parse("arcee").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://api.arcee.ai/v1".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    fn openrouter_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("openrouter").unwrap(),
            route_provider: ProviderId::parse("openrouter").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://openrouter.ai/api/v1".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
                env: None,
                profile: None,
                external: None,
            },
            credential: Some("test-key".into()),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        }
    }

    fn huggingface_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("huggingface").unwrap(),
            route_provider: ProviderId::parse("huggingface").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://router.huggingface.co/v1".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    fn vercel_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("vercel-ai-gateway").unwrap(),
            route_provider: ProviderId::parse("vercel-ai-gateway").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::AnthropicMessages,
            base_url: "https://ai-gateway.vercel.sh".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::BearerToken,
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
        }
    }

    fn kilocode_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("kilocode").unwrap(),
            route_provider: ProviderId::parse("kilocode").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://api.kilo.ai/api/gateway".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    fn opencode_go_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("opencode-go").unwrap(),
            route_provider: ProviderId::parse("opencode-go").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://opencode.ai/zen/go/v1".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    fn nearai_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("nearai").unwrap(),
            route_provider: ProviderId::parse("nearai").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://cloud-api.near.ai/v1".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    fn synthetic_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("synthetic").unwrap(),
            route_provider: ProviderId::parse("synthetic").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::AnthropicMessages,
            base_url: "https://api.synthetic.new/anthropic".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    fn venice_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("venice").unwrap(),
            route_provider: ProviderId::parse("venice").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://api.venice.ai/api/v1".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    fn tencent_tokenhub_provider() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::parse("tencent-tokenhub").unwrap(),
            route_provider: ProviderId::parse("tencent-tokenhub").unwrap(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://tokenhub.tencentmaas.com/v1".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::Env,
                kind: crate::config::CredentialKind::ApiKey,
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
        }
    }

    #[test]
    fn maps_arcee_models_without_inventing_capabilities_or_output_limits() {
        let payload: ArceeModelsResponse = serde_json::from_str(
            r#"{"object":"list","data":[
                {"id":"trinity-mini","object":"model","owned_by":"arcee-ai"},
                {"id":"trinity-large-preview","object":"model","owned_by":"arcee-ai"},
                {"id":"future-model","object":"model","owned_by":"arcee-ai"},
                {"id":" ","object":"model","owned_by":"arcee-ai"}
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("arcee").unwrap());

        assert_eq!(models.len(), 3);
        let mini = models
            .iter()
            .find(|model| model.model_ref.model == "trinity-mini")
            .unwrap();
        assert_eq!(mini.context_window_tokens, Some(131_072));
        assert!(mini.default_max_output_tokens.is_none());
        assert!(mini.max_output_tokens_upper_limit.is_none());
        assert_eq!(mini.capabilities, ModelCapabilityFlags::default());
        assert_eq!(mini.source, ModelMetadataSource::RemoteDiscovered);

        let unknown = models
            .iter()
            .find(|model| model.model_ref.model == "future-model")
            .unwrap();
        assert!(unknown.context_window_tokens.is_none());
        assert_eq!(unknown.capabilities, ModelCapabilityFlags::default());
    }

    #[test]
    fn arcee_discovery_requires_a_credential_and_uses_its_models_api() {
        let provider = arcee_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Unauthenticated);
        assert!(!discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://api.arcee.ai/api/v1/models"
        );
    }

    #[test]
    fn maps_only_live_huggingface_routes_conservatively() {
        let payload: HuggingFaceModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"openai/gpt-oss-120b",
                    "architecture":{"input_modalities":["text"]},
                    "providers":[
                        {"provider":"groq","status":"live","context_length":131072,"supports_tools":true},
                        {"provider":"example","status":"error","context_length":262144}
                    ]
                },
                {
                    "id":"Qwen/Qwen-VL",
                    "architecture":{"input_modalities":["text","image"]},
                    "providers":[
                        {"provider":"example","status":"live","context_length":65536}
                    ]
                },
                {
                    "id":"offline/model",
                    "providers":[
                        {"provider":"example","status":"error","context_length":32768}
                    ]
                }
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("huggingface").unwrap());

        assert_eq!(models.len(), 2);
        let gpt_oss = models
            .iter()
            .find(|model| model.model_ref.model == "openai/gpt-oss-120b")
            .unwrap();
        assert_eq!(gpt_oss.context_window_tokens, Some(131_072));
        assert!(gpt_oss.capabilities.supports_reasoning);
        assert!(!gpt_oss.capabilities.parallel_tool_calls);
        assert_eq!(gpt_oss.reasoning_effort_options, ["low", "medium", "high"]);
        assert!(gpt_oss.max_output_tokens_upper_limit.is_none());

        let vision = models
            .iter()
            .find(|model| model.model_ref.model == "Qwen/Qwen-VL")
            .unwrap();
        assert!(vision.capabilities.image_input);
        assert!(!vision.capabilities.supports_reasoning);
        assert!(!models
            .iter()
            .any(|model| model.model_ref.model == "offline/model"));
    }

    #[test]
    fn huggingface_discovery_is_public_but_inference_still_requires_authentication() {
        let provider = huggingface_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://router.huggingface.co/v1/models"
        );
    }

    #[test]
    fn maps_only_available_venice_text_models_and_reported_capabilities() {
        let payload: VeniceModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"zai-org-glm-4.7",
                    "type":"text",
                    "context_length":128000,
                    "model_spec":{
                        "name":"GLM 4.7",
                        "description":"default",
                        "availableContextTokens":198000,
                        "maxCompletionTokens":16384,
                        "offline":false,
                        "capabilities":{
                            "supportsVision":false,
                            "supportsReasoning":true,
                            "supportsReasoningEffort":true,
                            "reasoningEffortOptions":["low","medium","high"]
                        }
                    }
                },
                {
                    "id":"qwen3-vl-235b-a22b",
                    "type":"text",
                    "context_length":128000,
                    "model_spec":{
                        "name":"Qwen3 VL 235B",
                        "maxCompletionTokens":16384,
                        "capabilities":{
                            "supportsVision":true,
                            "supportsReasoning":false,
                            "supportsReasoningEffort":false,
                            "reasoningEffortOptions":["low"]
                        }
                    }
                },
                {"id":"offline","type":"text","model_spec":{"offline":true}},
                {"id":"image-model","type":"image","model_spec":{}},
                {
                    "id":"removed",
                    "type":"text",
                    "model_spec":{"deprecation":{"removesAt":"2025-01-01T00:00:00Z"}}
                }
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(
            &ProviderId::parse("venice").unwrap(),
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );

        assert_eq!(models.len(), 2);
        let glm = models
            .iter()
            .find(|model| model.model_ref.model == "zai-org-glm-4.7")
            .unwrap();
        assert_eq!(glm.context_window_tokens, Some(198_000));
        assert_eq!(glm.max_output_tokens_upper_limit, Some(16_384));
        assert!(!glm.capabilities.image_input);
        assert!(glm.capabilities.supports_reasoning);
        assert_eq!(glm.reasoning_effort_options, ["low", "medium", "high"]);

        let vision = models
            .iter()
            .find(|model| model.model_ref.model == "qwen3-vl-235b-a22b")
            .unwrap();
        assert!(vision.capabilities.image_input);
        assert!(!vision.capabilities.supports_reasoning);
        assert!(vision.reasoning_effort_options.is_empty());
    }

    #[test]
    fn maps_openrouter_models_to_remote_metadata() {
        let payload: OpenRouterModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"openrouter/auto",
                    "name":"Auto Router",
                    "context_length":2000000,
                    "architecture":{"input_modalities":["text","image","audio","file","video"]},
                    "top_provider":{"max_completion_tokens":null},
                    "supported_parameters":["reasoning","reasoning_effort"],
                    "reasoning":null
                },
                {
                    "id":"anthropic/claude-sonnet-5",
                    "name":"Claude Sonnet 5",
                    "description":"test",
                    "context_length":1000000,
                    "architecture":{"input_modalities":["text","image"]},
                    "top_provider":{"max_completion_tokens":128000},
                    "supported_parameters":["reasoning","reasoning_effort"],
                    "reasoning":{
                        "mandatory":false,
                        "default_enabled":true,
                        "supported_efforts":["max","xhigh","high","medium","low"]
                    }
                }
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("openrouter").unwrap());

        assert_eq!(models.len(), 2);
        let model = models
            .iter()
            .find(|model| model.model_ref.model == "anthropic/claude-sonnet-5")
            .unwrap();
        assert_eq!(
            model.model_ref.as_string(),
            "openrouter/anthropic/claude-sonnet-5"
        );
        assert_eq!(model.display_name, "Claude Sonnet 5");
        assert_eq!(model.context_window_tokens, Some(1_000_000));
        assert_eq!(model.max_output_tokens_upper_limit, Some(128_000));
        assert!(model.capabilities.image_input);
        assert!(model.capabilities.supports_reasoning);
        assert_eq!(
            model.reasoning_effort_options,
            ["max", "xhigh", "high", "medium", "low"]
        );
        assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);

        let auto = models
            .iter()
            .find(|model| model.model_ref.model == "openrouter/auto")
            .unwrap();
        assert_eq!(auto.context_window_tokens, Some(2_000_000));
        assert!(auto.max_output_tokens_upper_limit.is_none());
        assert!(auto.capabilities.image_input);
        assert!(auto.capabilities.supports_reasoning);
        assert!(auto.reasoning_effort_options.is_empty());
    }

    #[test]
    fn maps_vercel_language_models_conservatively() {
        let payload: VercelModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"anthropic/claude-opus-4.6",
                    "name":"Claude Opus 4.6",
                    "description":"test",
                    "context_window":1000000,
                    "max_tokens":128000,
                    "type":"language",
                    "tags":["vision","reasoning","tool-use"]
                },
                {
                    "id":"openai/dall-e-3",
                    "name":"DALL-E 3",
                    "type":"image",
                    "tags":["image-generation"]
                },
                {
                    "id":"alibaba/qwen-3-14b",
                    "name":"Qwen3-14B",
                    "context_window":40960,
                    "max_tokens":16384,
                    "type":"language",
                    "tags":["reasoning","tool-use"]
                }
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("vercel-ai-gateway").unwrap());

        assert_eq!(models.len(), 2);
        let claude = models
            .iter()
            .find(|model| model.model_ref.model == "anthropic/claude-opus-4.6")
            .unwrap();
        assert_eq!(
            claude.model_ref.as_string(),
            "vercel-ai-gateway/anthropic/claude-opus-4.6"
        );
        assert_eq!(claude.context_window_tokens, Some(1_000_000));
        assert_eq!(claude.max_output_tokens_upper_limit, Some(128_000));
        assert!(claude.capabilities.image_input);
        assert!(claude.capabilities.supports_reasoning);
        assert!(!claude.capabilities.parallel_tool_calls);
        assert!(claude.reasoning_effort_options.is_empty());
        assert_eq!(claude.source, ModelMetadataSource::RemoteDiscovered);

        let qwen = models
            .iter()
            .find(|model| model.model_ref.model == "alibaba/qwen-3-14b")
            .unwrap();
        assert!(!qwen.capabilities.image_input);
        assert!(qwen.capabilities.supports_reasoning);
        assert!(!models
            .iter()
            .any(|model| model.model_ref.model == "openai/dall-e-3"));
    }

    #[test]
    fn maps_kilocode_models_from_published_gateway_metadata() {
        let payload: KiloModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"kilo-auto/balanced",
                    "name":"Auto Balanced",
                    "description":"test",
                    "context_length":1000000,
                    "architecture":{"input_modalities":["text","image"]},
                    "top_provider":{"max_completion_tokens":65536},
                    "supported_parameters":["reasoning","include_reasoning"]
                },
                {
                    "id":"example/reasoning-model",
                    "context_length":262144,
                    "architecture":{"input_modalities":["text"]},
                    "top_provider":{"max_completion_tokens":32768},
                    "supported_parameters":["reasoning","reasoning_effort"],
                    "opencode":{"variants":{
                        "none":{"reasoning":{"enabled":false}},
                        "low":{"reasoning":{"effort":"low"}},
                        "high":{"reasoning":{"effort":"high"}},
                        "thinking":{"reasoning":{"enabled":true}}
                    }}
                },
                {"id":" "}
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("kilocode").unwrap());

        assert_eq!(models.len(), 2);
        let balanced = models
            .iter()
            .find(|model| model.model_ref.model == "kilo-auto/balanced")
            .unwrap();
        assert_eq!(balanced.context_window_tokens, Some(1_000_000));
        assert_eq!(balanced.max_output_tokens_upper_limit, Some(65_536));
        assert!(balanced.capabilities.image_input);
        assert!(balanced.capabilities.supports_reasoning);
        assert!(balanced.reasoning_effort_options.is_empty());

        let reasoning = models
            .iter()
            .find(|model| model.model_ref.model == "example/reasoning-model")
            .unwrap();
        assert_eq!(reasoning.reasoning_effort_options, ["high", "low", "none"]);
        assert!(!reasoning.capabilities.image_input);
        assert_eq!(reasoning.source, ModelMetadataSource::RemoteDiscovered);
    }

    #[test]
    fn maps_opencode_go_models_to_the_published_transport_endpoints() {
        let payload: OpenCodeGoModelsResponse = serde_json::from_str(
            r#"{"data":[
                {"id":"minimax-m3"},
                {"id":"kimi-k2.7-code"},
                {"id":"qwen3.7-plus"},
                {"id":"glm-5"},
                {"id":"kimi-k2.5"},
                {"id":" "}
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("opencode-go").unwrap());
        assert_eq!(
            models
                .iter()
                .map(|model| model.model_ref.model.as_str())
                .collect::<Vec<_>>(),
            ["kimi-k2.7-code", "minimax-m3", "qwen3.7-plus"]
        );
        assert_eq!(
            models
                .iter()
                .find(|model| model.model_ref.model == "kimi-k2.7-code")
                .unwrap()
                .endpoint,
            None
        );
        for model in ["minimax-m3", "qwen3.7-plus"] {
            assert_eq!(
                models
                    .iter()
                    .find(|metadata| metadata.model_ref.model == model)
                    .unwrap()
                    .endpoint
                    .as_ref()
                    .map(ProviderEndpointId::as_str),
                Some("messages")
            );
        }
        assert!(models
            .iter()
            .all(|model| model.source == ModelMetadataSource::RemoteDiscovered));
    }

    #[test]
    fn maps_only_current_tencent_tokenhub_turn_models() {
        let payload: TencentTokenHubModelsResponse = serde_json::from_str(
            r#"{"data":[
                {"id":"hy3"},
                {"id":"glm-5v-turbo"},
                {"id":"deepseek-r1-0528"},
                {"id":"HY-Image-V3.0"},
                {"id":"future-model"},
                {"id":" "}
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("tencent-tokenhub").unwrap());
        assert_eq!(
            models
                .iter()
                .map(|model| model.model_ref.model.as_str())
                .collect::<Vec<_>>(),
            ["glm-5v-turbo", "hy3"]
        );
        assert!(models.iter().all(|model| {
            model.endpoint.is_none()
                && model.source == ModelMetadataSource::RemoteDiscovered
                && model.capabilities == ModelCapabilityFlags::default()
        }));
    }

    #[test]
    fn maps_only_ready_nearai_models_from_published_metadata() {
        let payload: NearAiModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"Qwen/Qwen3.5-122B-A10B",
                    "name":"Qwen3.5 122B A10B",
                    "description":"test",
                    "context_length":262144,
                    "max_output_length":16384,
                    "input_modalities":["text","image"],
                    "supported_features":["tools","structured_outputs","reasoning"],
                    "is_ready":true
                },
                {
                    "id":"anthropic/claude-opus-4-6",
                    "name":"Claude Opus 4.6",
                    "context_length":200000,
                    "max_output_length":32768,
                    "input_modalities":["text"],
                    "supported_features":["tools","reasoning"],
                    "is_ready":false
                },
                {
                    "id":"missing-ready",
                    "name":"Missing Ready"
                }
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("nearai").unwrap());

        assert_eq!(models.len(), 1);
        let qwen = &models[0];
        assert_eq!(qwen.model_ref.as_string(), "nearai/Qwen/Qwen3.5-122B-A10B");
        assert_eq!(qwen.context_window_tokens, Some(262_144));
        assert_eq!(qwen.max_output_tokens_upper_limit, Some(16_384));
        assert!(qwen.capabilities.image_input);
        assert!(qwen.capabilities.supports_reasoning);
        assert!(!qwen.capabilities.parallel_tool_calls);
        assert!(qwen.reasoning_effort_options.is_empty());
        assert_eq!(qwen.source, ModelMetadataSource::RemoteDiscovered);
    }

    #[test]
    fn maps_only_synthetic_always_on_models_from_published_metadata() {
        let payload: SyntheticModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"syn:large:vision",
                    "name":"syn:large:vision",
                    "always_on":true,
                    "context_length":262144,
                    "max_output_length":65536,
                    "input_modalities":["text","image"],
                    "supported_features":["tools","structured_outputs","reasoning"]
                },
                {
                    "id":"hf:rotating/model",
                    "name":"Rotating Model",
                    "always_on":false,
                    "context_length":131072,
                    "max_output_length":8192,
                    "input_modalities":["text"],
                    "supported_features":["tools"]
                }
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("synthetic").unwrap());

        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.model_ref.as_string(), "synthetic/syn:large:vision");
        assert_eq!(model.context_window_tokens, Some(262_144));
        assert_eq!(model.default_max_output_tokens, None);
        assert_eq!(model.max_output_tokens_upper_limit, Some(65_536));
        assert!(model.capabilities.image_input);
        assert!(model.capabilities.supports_reasoning);
        assert!(model.reasoning_effort_options.is_empty());
        assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);
    }

    #[test]
    fn nearai_discovery_is_public_but_inference_remains_unauthenticated() {
        let provider = nearai_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://cloud-api.near.ai/v1/models"
        );
    }

    #[test]
    fn synthetic_discovery_is_public_and_uses_the_openai_models_endpoint() {
        let provider = synthetic_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://api.synthetic.new/openai/v1/models"
        );
    }

    #[test]
    fn vercel_discovery_is_public_but_inference_remains_unauthenticated() {
        let provider = vercel_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            vercel_models_url(&provider.base_url).unwrap(),
            "https://ai-gateway.vercel.sh/v1/models"
        );
    }

    #[test]
    fn kilocode_discovery_is_public_but_inference_still_requires_authentication() {
        let provider = kilocode_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://api.kilo.ai/api/gateway/models"
        );
    }

    #[test]
    fn opencode_go_discovery_is_public_and_uses_the_openai_models_endpoint() {
        let provider = opencode_go_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://opencode.ai/zen/go/v1/models"
        );
    }

    #[test]
    fn tencent_tokenhub_discovery_requires_a_credential_and_uses_its_models_endpoint() {
        let provider = tencent_tokenhub_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Unauthenticated);
        assert!(!discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://tokenhub.tencentmaas.com/v1/models"
        );
    }

    #[test]
    fn venice_discovery_is_public_and_filters_to_text_models() {
        let provider = venice_provider();
        let cache = ModelDiscoveryCacheFile::default();
        let status =
            discovery_cache_status_for_provider(&provider, &cache, Duration::from_secs(60), false);

        assert!(status.supports_auto_refresh);
        assert!(!status.credential_configured);
        assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(
            &provider,
            &cache,
            Duration::from_secs(60)
        ));
        assert_eq!(
            provider_models_url(&provider).unwrap(),
            "https://api.venice.ai/api/v1/models?type=text"
        );
    }

    #[test]
    fn local_openai_compatible_servers_use_deployment_model_discovery() {
        let providers =
            crate::config::built_in_provider_registry_with_settings(&Default::default()).unwrap();
        let litellm = providers
            .get(&ProviderId::parse("litellm").unwrap())
            .unwrap();
        let vllm = providers.get(&ProviderId::parse("vllm").unwrap()).unwrap();
        let cache = ModelDiscoveryCacheFile::default();

        assert_eq!(
            discovery_cache_status_for_provider(litellm, &cache, Duration::from_secs(60), false)
                .state,
            ModelDiscoveryCacheState::Unauthenticated
        );
        assert_eq!(
            provider_models_url(litellm).unwrap(),
            "http://localhost:4000/v1/models"
        );
        assert_eq!(
            discovery_cache_status_for_provider(vllm, &cache, Duration::from_secs(60), false).state,
            ModelDiscoveryCacheState::Missing
        );
        assert_eq!(
            provider_models_url(vllm).unwrap(),
            "http://127.0.0.1:8000/v1/models"
        );
    }

    #[test]
    fn openai_compatible_discovery_keeps_only_non_empty_server_model_ids() {
        let payload: OpenAiCompatibleModelsResponse = serde_json::from_str(
            r#"{"object":"list","data":[
                {"id":"deployment-alias","object":"model"},
                {"id":"org/model","object":"model"},
                {"id":" ","object":"model"}
            ]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("vllm").unwrap());
        assert_eq!(
            models
                .iter()
                .map(|model| model.model_ref.model.as_str())
                .collect::<Vec<_>>(),
            vec!["deployment-alias", "org/model"]
        );
        assert!(models
            .iter()
            .all(|model| model.source == ModelMetadataSource::RemoteDiscovered));
        assert!(models
            .iter()
            .all(|model| model.capabilities == ModelCapabilityFlags::default()));
    }

    #[test]
    fn discovery_cache_status_reports_missing_stale_and_fresh() {
        let provider = openrouter_provider();
        let ttl = Duration::from_secs(60);
        let mut cache = ModelDiscoveryCacheFile::default();

        let missing = discovery_cache_status_for_provider(&provider, &cache, ttl, false);
        assert_eq!(missing.state, ModelDiscoveryCacheState::Missing);
        assert!(discovery_cache_needs_refresh(&provider, &cache, ttl));

        cache.providers.insert(
            provider.id.clone(),
            ProviderModelDiscoveryCache {
                provider: provider.id.clone(),
                fetched_at: Utc::now() - chrono::TimeDelta::seconds(120),
                source_url: None,
                response_hash: None,
                models: Vec::new(),
            },
        );
        let stale = discovery_cache_status_for_provider(&provider, &cache, ttl, true);
        assert_eq!(stale.state, ModelDiscoveryCacheState::Stale);
        assert!(stale.refresh_in_flight);
        assert!(discovery_cache_needs_refresh(&provider, &cache, ttl));

        cache.providers.get_mut(&provider.id).unwrap().fetched_at = Utc::now();
        let fresh = discovery_cache_status_for_provider(&provider, &cache, ttl, false);
        assert_eq!(fresh.state, ModelDiscoveryCacheState::Fresh);
        assert!(!discovery_cache_needs_refresh(&provider, &cache, ttl));
    }
}
