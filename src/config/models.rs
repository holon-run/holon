//! Model reference and runtime model catalog types.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};

use crate::context::ContextConfig;
use crate::model_catalog::{
    BuiltInModelCatalog, BuiltInModelMetadata, ModelRuntimeOverride, ResolvedRuntimeModelPolicy,
};
use crate::types::{ViewImageSelectedMode, ViewImageVisionCandidate, ViewImageVisionSelection};

use super::file::{HolonConfigFile, ModelConfigFile, ModelsConfigFile, VisionConfigFile};
use super::providers::{ProviderId, ProviderTransportKind};
use super::validate_model_runtime_override;
use super::AppConfig;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModelCatalog {
    pub default_model: ModelRef,
    pub fallback_models: Vec<ModelRef>,
    pub vision_model: Option<ModelRef>,
    pub vision_candidate_models: Vec<ModelRef>,
    pub disable_provider_fallback: bool,
    pub provider_transports: HashMap<ProviderId, ProviderTransportKind>,
    pub built_in_catalog: BuiltInModelCatalog,
    pub discovered_models: HashMap<ModelRef, BuiltInModelMetadata>,
    pub model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    pub unknown_model_fallback: Option<ModelRuntimeOverride>,
    pub configured_runtime_max_output_tokens: u32,
}

impl RuntimeModelCatalog {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            default_model: config.default_model.clone(),
            fallback_models: config.fallback_models.clone(),
            vision_model: config.vision_model.clone(),
            vision_candidate_models: config.vision_candidate_models.clone(),
            disable_provider_fallback: config.provider_fallback_disabled(),
            provider_transports: config
                .providers
                .iter()
                .map(|(provider, config)| (provider.clone(), config.transport))
                .collect(),
            built_in_catalog: BuiltInModelCatalog::default(),
            discovered_models: config
                .model_discovery_cache
                .models()
                .into_iter()
                .collect::<HashMap<_, _>>(),
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

    pub fn provider_chain_for_turn(
        &self,
        model_override: Option<&ModelRef>,
        pending_fallback_model: Option<&ModelRef>,
    ) -> Vec<ModelRef> {
        let chain = self.provider_chain(model_override);
        let Some(pending_fallback_model) = pending_fallback_model else {
            return chain;
        };
        chain
            .iter()
            .position(|model| model == pending_fallback_model)
            .map(|index| chain[index..].to_vec())
            .unwrap_or_else(|| vec![pending_fallback_model.clone()])
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
            &self.discovered_models,
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
                &self.discovered_models,
                self.unknown_model_fallback.as_ref(),
                base_context_config,
                self.configured_runtime_max_output_tokens,
            )
            .0
    }

    pub fn available_models(&self) -> Vec<BuiltInModelMetadata> {
        let mut models = self.built_in_catalog.list();
        for discovered in self.discovered_models.values() {
            if !self.model_overrides.contains_key(&discovered.model_ref) {
                models.retain(|model| model.model_ref != discovered.model_ref);
                models.push(discovered.clone());
            }
        }
        for (model_ref, override_config) in &self.model_overrides {
            let base = self
                .discovered_models
                .get(model_ref)
                .or_else(|| self.built_in_catalog.get(model_ref));
            models.retain(|model| &model.model_ref != model_ref);
            models.push(BuiltInModelMetadata {
                model_ref: model_ref.clone(),
                display_name: override_config
                    .display_name
                    .clone()
                    .or_else(|| base.map(|model| model.display_name.clone()))
                    .unwrap_or_else(|| model_ref.as_string()),
                description: override_config
                    .description
                    .clone()
                    .or_else(|| base.map(|model| model.description.clone()))
                    .unwrap_or_else(|| "User-configured model metadata override.".to_string()),
                context_window_tokens: override_config
                    .context_window_tokens
                    .or_else(|| base.and_then(|model| model.context_window_tokens)),
                effective_context_window_percent: override_config
                    .effective_context_window_percent
                    .unwrap_or_else(|| {
                        base.map(|model| model.effective_context_window_percent)
                            .unwrap_or(95)
                    }),
                auto_compact_token_limit: override_config
                    .auto_compact_token_limit
                    .or_else(|| base.and_then(|model| model.auto_compact_token_limit)),
                default_max_output_tokens: override_config
                    .runtime_max_output_tokens
                    .or_else(|| base.and_then(|model| model.default_max_output_tokens)),
                max_output_tokens_upper_limit: base
                    .and_then(|model| model.max_output_tokens_upper_limit),
                default_verbosity: override_config
                    .verbosity
                    .or_else(|| base.and_then(|model| model.default_verbosity)),
                tool_output_truncation_estimated_tokens: override_config
                    .tool_output_truncation_estimated_tokens
                    .or_else(|| {
                        base.and_then(|model| model.tool_output_truncation_estimated_tokens)
                    }),
                capabilities: merged_model_capabilities(
                    base.map(|model| &model.capabilities),
                    override_config.capabilities.as_ref(),
                ),
                source: crate::model_catalog::ModelMetadataSource::ConfigOverride,
            });
        }
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }

    pub fn select_view_image_vision_model(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRef>,
        pending_fallback_model: Option<&ModelRef>,
    ) -> ViewImageVisionSelection {
        let chain = self.provider_chain_for_turn(model_override, pending_fallback_model);
        let primary = chain
            .first()
            .cloned()
            .unwrap_or_else(|| self.effective_model(model_override));
        if let Some(model_ref) = &self.vision_model {
            return self.select_view_image_vision_model_from_candidates(
                base_context_config,
                primary,
                vec![model_ref.clone()],
                "explicit_vision_model_supports_image_input",
                "explicit_vision_model_unavailable",
            );
        }

        let mut candidates = Vec::new();
        candidates.push(primary.clone());
        for model_ref in &self.vision_candidate_models {
            if !candidates.iter().any(|existing| existing == model_ref) {
                candidates.push(model_ref.clone());
            }
        }
        for model_ref in chain {
            if !candidates.iter().any(|existing| existing == &model_ref) {
                candidates.push(model_ref);
            }
        }

        self.select_view_image_vision_model_from_candidates(
            base_context_config,
            primary,
            candidates,
            "auto_discovered_vision_model_supports_image_input",
            "no_configured_model_supports_view_image_observation",
        )
    }

    fn select_view_image_vision_model_from_candidates(
        &self,
        base_context_config: &ContextConfig,
        primary: ModelRef,
        model_refs: Vec<ModelRef>,
        selected_adapter_reason: &str,
        unavailable_reason: &str,
    ) -> ViewImageVisionSelection {
        let mut candidates = Vec::new();
        let mut selected = None;

        for model_ref in &model_refs {
            let policy = self.built_in_catalog.resolve_policy(
                model_ref,
                &self.model_overrides,
                &self.discovered_models,
                self.unknown_model_fallback.as_ref(),
                base_context_config,
                self.configured_runtime_max_output_tokens,
            );
            let image_input = policy.capabilities.image_input;
            let supported_transport = self
                .provider_transports
                .get(&model_ref.provider)
                .is_some_and(|transport| transport.supports_view_image_observation_generation());
            let reason = if !image_input {
                "model_lacks_image_input"
            } else if supported_transport {
                "model_advertises_image_input"
            } else {
                "provider_transport_unsupported_for_view_image_observation"
            };
            candidates.push(ViewImageVisionCandidate {
                provider: model_ref.provider.as_str().to_string(),
                model: model_ref.model.clone(),
                model_ref: model_ref.as_string(),
                image_input,
                reason: reason.to_string(),
            });
            if image_input && supported_transport && selected.is_none() {
                selected = Some(model_ref.clone());
            }
        }

        let Some(selected) = selected else {
            return ViewImageVisionSelection {
                selected_mode: ViewImageSelectedMode::Unavailable,
                vision_provider: None,
                vision_model: None,
                selection_reason: unavailable_reason.to_string(),
                primary_provider: Some(primary.provider.as_str().to_string()),
                primary_model: Some(primary.model),
                candidates,
            };
        };

        let primary_supports_image = selected == primary;
        ViewImageVisionSelection {
            selected_mode: if primary_supports_image {
                ViewImageSelectedMode::NativeImageWithObservation
            } else {
                ViewImageSelectedMode::VisionAdapter
            },
            vision_provider: Some(selected.provider.as_str().to_string()),
            vision_model: Some(selected.model.clone()),
            selection_reason: if primary_supports_image {
                "current_primary_model_supports_image_input"
            } else {
                selected_adapter_reason
            }
            .to_string(),
            primary_provider: Some(primary.provider.as_str().to_string()),
            primary_model: Some(primary.model),
            candidates,
        }
    }

    pub fn provider_supports_view_image_observation(&self, provider: &str) -> bool {
        self.provider_transports
            .iter()
            .any(|(provider_id, transport)| {
                provider_id.as_str() == provider
                    && transport.supports_view_image_observation_generation()
            })
    }
}

impl Default for RuntimeModelCatalog {
    fn default() -> Self {
        Self {
            default_model: ModelRef::parse("openai/gpt-5.4").expect("valid default model ref"),
            fallback_models: Vec::new(),
            vision_model: None,
            vision_candidate_models: Vec::new(),
            disable_provider_fallback: false,
            provider_transports: HashMap::new(),
            built_in_catalog: BuiltInModelCatalog::default(),
            discovered_models: HashMap::new(),
            model_overrides: HashMap::new(),
            unknown_model_fallback: None,
            configured_runtime_max_output_tokens: 8192,
        }
    }
}

pub fn merged_model_capabilities(
    base: Option<&crate::model_catalog::ModelCapabilityFlags>,
    override_config: Option<&crate::model_catalog::ModelCapabilityOverride>,
) -> crate::model_catalog::ModelCapabilityFlags {
    let mut capabilities = base.cloned().unwrap_or_default();
    if let Some(override_config) = override_config {
        if let Some(value) = override_config.parallel_tool_calls {
            capabilities.parallel_tool_calls = value;
        }
        if let Some(value) = override_config.reasoning_summaries {
            capabilities.reasoning_summaries = value;
        }
        if let Some(value) = override_config.image_input {
            capabilities.image_input = value;
        }
        if let Some(value) = override_config.interactive_exec {
            capabilities.interactive_exec = value;
        }
    }
    capabilities
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
    pub fn is_empty(&self) -> bool {
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
    pub fn is_empty(&self) -> bool {
        self.catalog.is_empty()
    }
}

impl VisionConfigFile {
    pub fn is_empty(&self) -> bool {
        self.default.is_none()
    }
}

pub fn resolve_model_catalog(
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

pub fn validate_optional_model_runtime_override(
    override_config: Option<ModelRuntimeOverride>,
) -> Result<Option<ModelRuntimeOverride>> {
    override_config
        .map(validate_model_runtime_override)
        .transpose()
        .map(|value| value.filter(|entry| !entry.is_empty()))
}
