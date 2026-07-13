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
    config::{ModelRef, ProviderId, ProviderRuntimeConfig},
    model_catalog::{BuiltInModelMetadata, ModelCapabilityFlags, ModelMetadataSource},
};

const OPENAI_COMPATIBLE_MODELS_PATH: &str = "/models";
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
        "nearai" | "openrouter" | "vercel-ai-gateway"
    )
}

fn provider_model_discovery_requires_credential(provider: &ProviderRuntimeConfig) -> bool {
    !matches!(provider.id.as_str(), "nearai" | "vercel-ai-gateway")
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
        "nearai" => serde_json::from_slice::<NearAiModelsResponse>(&raw)
            .context("failed to parse NEAR AI model discovery response")?
            .into_model_metadata(&provider.id),
        "openrouter" => serde_json::from_slice::<OpenRouterModelsResponse>(&raw)
            .context("failed to parse OpenRouter model discovery response")?
            .into_model_metadata(&provider.id),
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
        "nearai" => openai_compatible_models_url(&provider.base_url),
        "openrouter" => openrouter_models_url(&provider.base_url),
        "vercel-ai-gateway" => vercel_models_url(&provider.base_url),
        _ => Err(anyhow!(
            "provider {} does not support model discovery yet",
            provider.id.as_str()
        )),
    }
}

fn openrouter_models_url(base_url: &str) -> Result<String> {
    openai_compatible_models_url(base_url)
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
