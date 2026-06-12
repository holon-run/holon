use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    config::{ModelRef, ProviderId, ProviderRuntimeConfig},
    model_catalog::{BuiltInModelMetadata, ModelCapabilityFlags, ModelMetadataSource},
};

const OPENROUTER_MODELS_PATH: &str = "/models";

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

pub fn discovery_cache_path(home_dir: &Path) -> PathBuf {
    home_dir.join("model-discovery-cache.json")
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
    if provider.id.as_str() != "openrouter" {
        return Err(anyhow!(
            "provider {} does not support model discovery yet",
            provider.id.as_str()
        ));
    }

    let source_url = openrouter_models_url(&provider.base_url)?;
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
        .context("OpenRouter model discovery request failed")?
        .error_for_status()
        .context("OpenRouter model discovery returned an error status")?;
    let raw = response
        .bytes()
        .await
        .context("failed to read OpenRouter model discovery response")?;
    let response_hash = format!("sha256:{}", sha256_hex(&raw));
    let payload: OpenRouterModelsResponse = serde_json::from_slice(&raw)
        .context("failed to parse OpenRouter model discovery response")?;
    let fetched_at = Utc::now();
    let models = payload.into_model_metadata(&provider.id);

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

fn openrouter_models_url(base_url: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base_url)
        .with_context(|| format!("invalid OpenRouter base_url {base_url:?}"))?;
    let path = url.path().trim_end_matches('/');
    url.set_path(&format!("{path}{OPENROUTER_MODELS_PATH}"));
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
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::RemoteDiscovered,
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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_openrouter_models_to_remote_metadata() {
        let payload: OpenRouterModelsResponse = serde_json::from_str(
            r#"{"data":[{"id":"anthropic/claude-3.5-sonnet","name":"Claude 3.5 Sonnet","description":"test","context_length":200000,"architecture":{"input_modalities":["text","image"]},"top_provider":{"max_completion_tokens":8192}}]}"#,
        )
        .unwrap();

        let models = payload.into_model_metadata(&ProviderId::parse("openrouter").unwrap());

        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(
            model.model_ref.as_string(),
            "openrouter/anthropic/claude-3.5-sonnet"
        );
        assert_eq!(model.display_name, "Claude 3.5 Sonnet");
        assert_eq!(model.context_window_tokens, Some(200_000));
        assert_eq!(model.max_output_tokens_upper_limit, Some(8192));
        assert!(model.capabilities.image_input);
        assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);
    }
}
