use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::{
    built_in_provider_endpoint_identity, ModelRef, ModelRouteRef, ProviderEndpointId, ProviderId,
};
use crate::context::ContextConfig;

const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u8 = 95;
const DEFAULT_COMPACTION_TRIGGER_PERCENT: u8 = 90;
const DEFAULT_KEEP_RECENT_PERCENT: u8 = 38;
const DEFAULT_UNKNOWN_FALLBACK_PROMPT_BUDGET_ESTIMATED_TOKENS: usize = 128_000;
const DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS: usize = 2_500;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataSource {
    BuiltInCatalog,
    ConservativeBuiltin,
    ConfigOverride,
    RemoteDiscovered,
    UnknownFallback,
}

impl Default for ModelMetadataSource {
    fn default() -> Self {
        Self::UnknownFallback
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelCapabilityFlags {
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub image_generation: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub interactive_exec: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModelModality {
    Text,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelReasoningCapability {
    None,
    Fixed,
    Effort,
    Budget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelIntrinsicCapabilities {
    pub input_modalities: Vec<ModelModality>,
    pub output_modalities: Vec<ModelModality>,
    pub reasoning: ModelReasoningCapability,
    pub parallel_tool_calls: bool,
    pub interactive_exec: bool,
}

impl ModelCapabilityFlags {
    pub fn intrinsic(&self) -> ModelIntrinsicCapabilities {
        let mut input_modalities = vec![ModelModality::Text];
        if self.image_input {
            input_modalities.push(ModelModality::Image);
        }
        let mut output_modalities = vec![ModelModality::Text];
        if self.image_generation {
            output_modalities.push(ModelModality::Image);
        }
        ModelIntrinsicCapabilities {
            input_modalities,
            output_modalities,
            reasoning: if self.supports_reasoning {
                ModelReasoningCapability::Fixed
            } else {
                ModelReasoningCapability::None
            },
            parallel_tool_calls: self.parallel_tool_calls,
            interactive_exec: self.interactive_exec,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelParameterSupport {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EndpointModelPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_parameters: Vec<ModelParameterSupport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelCapabilityOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_input: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive_exec: Option<bool>,
}

impl ModelCapabilityOverride {
    pub fn is_empty(&self) -> bool {
        self.parallel_tool_calls.is_none()
            && self.image_input.is_none()
            && self.image_generation.is_none()
            && self.supports_reasoning.is_none()
            && self.interactive_exec.is_none()
    }

    fn apply_to(&self, base: &mut ModelCapabilityFlags) {
        if let Some(value) = self.parallel_tool_calls {
            base.parallel_tool_calls = value;
        }
        if let Some(value) = self.image_input {
            base.image_input = value;
        }
        if let Some(value) = self.image_generation {
            base.image_generation = value;
        }
        if let Some(value) = self.supports_reasoning {
            base.supports_reasoning = value;
        }
        if let Some(value) = self.interactive_exec {
            base.interactive_exec = value;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuiltInModelMetadata {
    pub model_ref: ModelRef,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub context_window_tokens: Option<usize>,
    #[serde(default = "default_effective_context_window_percent")]
    pub effective_context_window_percent: u8,
    #[serde(default)]
    pub auto_compact_token_limit: Option<usize>,
    #[serde(default)]
    pub default_max_output_tokens: Option<u32>,
    #[serde(default)]
    pub max_output_tokens_upper_limit: Option<u32>,
    #[serde(default)]
    pub default_verbosity: Option<ModelVerbosity>,
    #[serde(default)]
    pub tool_output_truncation_estimated_tokens: Option<usize>,
    #[serde(default)]
    pub capabilities: ModelCapabilityFlags,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_effort_options: Vec<String>,
    pub source: ModelMetadataSource,
    /// Non-default endpoint this model belongs to under its canonical provider.
    /// When `None`, the model uses the provider's default endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ProviderEndpointId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelRuntimeOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_context_window_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_budget_estimated_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_trigger_estimated_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_keep_recent_estimated_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<ModelVerbosity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output_truncation_estimated_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilityOverride>,
}

impl ModelRuntimeOverride {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.description.is_none()
            && self.context_window_tokens.is_none()
            && self.effective_context_window_percent.is_none()
            && self.auto_compact_token_limit.is_none()
            && self.prompt_budget_estimated_tokens.is_none()
            && self.compaction_trigger_estimated_tokens.is_none()
            && self.compaction_keep_recent_estimated_tokens.is_none()
            && self.runtime_max_output_tokens.is_none()
            && self.verbosity.is_none()
            && self.tool_output_truncation_estimated_tokens.is_none()
            && self
                .capabilities
                .as_ref()
                .map(ModelCapabilityOverride::is_empty)
                .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelVerbosity {
    Low,
    Medium,
    High,
}

impl ModelVerbosity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedRuntimeModelPolicy {
    pub model_ref: ModelRef,
    pub display_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,
    pub effective_context_window_percent: u8,
    pub prompt_budget_estimated_tokens: usize,
    pub compaction_trigger_estimated_tokens: usize,
    pub compaction_keep_recent_estimated_tokens: usize,
    pub runtime_max_output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<ModelVerbosity>,
    pub tool_output_truncation_estimated_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens_upper_limit: Option<u32>,
    #[serde(default)]
    pub capabilities: ModelCapabilityFlags,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_effort_options: Vec<String>,
    pub source: ModelMetadataSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningEffortValidationError {
    message: String,
}

impl ReasoningEffortValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ReasoningEffortValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReasoningEffortValidationError {}

impl Default for ResolvedRuntimeModelPolicy {
    fn default() -> Self {
        Self {
            model_ref: ModelRef::new(ProviderId::openai(), "unknown"),
            display_name: "Unknown model".into(),
            description: "Legacy model state without resolved runtime policy.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            prompt_budget_estimated_tokens: DEFAULT_UNKNOWN_FALLBACK_PROMPT_BUDGET_ESTIMATED_TOKENS,
            compaction_trigger_estimated_tokens: percent_of(
                DEFAULT_UNKNOWN_FALLBACK_PROMPT_BUDGET_ESTIMATED_TOKENS,
                usize::from(DEFAULT_COMPACTION_TRIGGER_PERCENT),
            ),
            compaction_keep_recent_estimated_tokens: percent_of(
                percent_of(
                    DEFAULT_UNKNOWN_FALLBACK_PROMPT_BUDGET_ESTIMATED_TOKENS,
                    usize::from(DEFAULT_COMPACTION_TRIGGER_PERCENT),
                ),
                usize::from(DEFAULT_KEEP_RECENT_PERCENT),
            ),
            runtime_max_output_tokens: 8192,
            verbosity: None,
            tool_output_truncation_estimated_tokens:
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            max_output_tokens_upper_limit: None,
            capabilities: ModelCapabilityFlags::default(),
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::UnknownFallback,
        }
    }
}

impl ResolvedRuntimeModelPolicy {
    pub fn validate_reasoning_effort(
        &self,
        value: &str,
    ) -> Result<(), ReasoningEffortValidationError> {
        if self
            .reasoning_effort_options
            .iter()
            .any(|option| option == value)
        {
            return Ok(());
        }

        let model = self.model_ref.as_string();
        if value == "ultra" && self.model_ref.provider == ProviderId::openai_codex() {
            return Err(ReasoningEffortValidationError::new(format!(
                "reasoning_effort 'ultra' is unavailable for model {model}; \
                 Holon does not yet implement the required orchestration semantics"
            )));
        }
        if self.reasoning_effort_options.is_empty() {
            return Err(ReasoningEffortValidationError::new(format!(
                "model {model} does not support configurable reasoning_effort"
            )));
        }
        Err(ReasoningEffortValidationError::new(format!(
            "reasoning_effort '{value}' is not supported by model {model}; supported values: {}",
            self.reasoning_effort_options.join(", ")
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInModelCatalog {
    entries: HashMap<ModelRef, BuiltInModelMetadata>,
    preferred_models: HashMap<ProviderId, ModelRef>,
    route_entries: HashMap<ModelRouteRef, BuiltInModelMetadata>,
    preferred_routes: HashMap<ProviderId, ModelRouteRef>,
    preferred_routes_by_model: HashMap<ModelRef, ModelRouteRef>,
    aliases: HashMap<ModelRef, ModelRef>,
}

impl BuiltInModelCatalog {
    pub fn new() -> Self {
        let mut entries = HashMap::new();
        let mut preferred_models = HashMap::new();
        let mut route_entries = HashMap::new();
        let mut preferred_routes = HashMap::new();
        let mut preferred_routes_by_model = HashMap::new();
        let mut aliases = Self::legacy_aliases();
        for mut entry in built_in_entries() {
            let legacy_model_ref = entry.model_ref.clone();
            let (provider, provider_endpoint) =
                built_in_provider_endpoint_identity(&legacy_model_ref.provider)
                    .expect("built-in provider identity must be valid");
            let endpoint = if provider_endpoint == ProviderEndpointId::default_endpoint() {
                entry
                    .endpoint
                    .clone()
                    .unwrap_or_else(ProviderEndpointId::default_endpoint)
            } else {
                provider_endpoint
            };
            let canonical_model_ref = ModelRef::new(provider.clone(), &legacy_model_ref.model);
            let route_ref =
                ModelRouteRef::new(provider, endpoint, canonical_model_ref.model.clone());
            if legacy_model_ref != canonical_model_ref {
                aliases.insert(legacy_model_ref.clone(), canonical_model_ref.clone());
            }
            if is_turn_default_candidate(&entry) {
                preferred_models
                    .entry(legacy_model_ref.provider.clone())
                    .or_insert_with(|| canonical_model_ref.clone());
                preferred_routes
                    .entry(legacy_model_ref.provider)
                    .or_insert_with(|| route_ref.clone());
            }
            entry.model_ref = canonical_model_ref.clone();
            entry.endpoint = None;
            route_entries
                .entry(route_ref.clone())
                .or_insert_with(|| entry.clone());
            preferred_routes_by_model
                .entry(canonical_model_ref.clone())
                .and_modify(|preferred: &mut ModelRouteRef| {
                    if preferred.endpoint != ProviderEndpointId::default_endpoint()
                        && route_ref.endpoint == ProviderEndpointId::default_endpoint()
                    {
                        *preferred = route_ref.clone();
                    }
                })
                .or_insert(route_ref);
            entries.entry(canonical_model_ref).or_insert(entry);
        }
        Self {
            entries,
            preferred_models,
            route_entries,
            preferred_routes,
            preferred_routes_by_model,
            aliases,
        }
    }

    pub fn get(&self, model_ref: &ModelRef) -> Option<&BuiltInModelMetadata> {
        self.entries.get(model_ref)
    }

    /// Resolve a legacy model ref to its canonical form.
    /// Returns the input unchanged if no alias exists.
    pub fn canonicalize_model_ref(&self, model_ref: &ModelRef) -> ModelRef {
        self.aliases
            .get(model_ref)
            .cloned()
            .unwrap_or_else(|| model_ref.clone())
    }

    /// Legacy model ref → canonical model ref aliases for backward compatibility.
    fn legacy_aliases() -> HashMap<ModelRef, ModelRef> {
        let mut aliases = HashMap::new();
        aliases.insert(
            ModelRef::new(
                provider_id("volcengine-image-openai"),
                "doubao-seedream-5.0-lite",
            ),
            ModelRef::new(provider_id("volcengine"), "doubao-seedream-5.0-lite"),
        );
        aliases.insert(
            ModelRef::new(provider_id("dashscope-token-plan"), "qwen-3.7"),
            ModelRef::new(provider_id("dashscope"), "qwen3.7-max"),
        );
        aliases.insert(
            ModelRef::new(provider_id("dashscope"), "qwen-3.7"),
            ModelRef::new(provider_id("dashscope"), "qwen3.7-max"),
        );
        aliases.insert(
            ModelRef::new(provider_id("mistral"), "devstral-medium-latest"),
            ModelRef::new(provider_id("mistral"), "mistral-medium-latest"),
        );
        aliases.insert(
            ModelRef::new(provider_id("mistral"), "magistral-small"),
            ModelRef::new(provider_id("mistral"), "mistral-small-latest"),
        );
        aliases.insert(
            ModelRef::new(provider_id("mistral"), "mistral-medium-2508"),
            ModelRef::new(provider_id("mistral"), "mistral-medium-latest"),
        );
        aliases.insert(
            ModelRef::new(provider_id("mistral"), "pixtral-large-latest"),
            ModelRef::new(provider_id("mistral"), "mistral-medium-latest"),
        );
        aliases
    }

    pub fn list(&self) -> Vec<BuiltInModelMetadata> {
        let mut entries = self.entries.values().cloned().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        entries
    }

    pub fn list_routes(&self) -> Vec<ModelRouteRef> {
        let mut routes = self.route_entries.keys().cloned().collect::<Vec<_>>();
        routes.sort_by_key(ModelRouteRef::as_string);
        routes
    }

    pub fn preferred_model_for_provider(&self, provider: &ProviderId) -> Option<ModelRef> {
        self.preferred_models.get(provider).cloned()
    }

    pub fn preferred_route_for_provider(&self, provider: &ProviderId) -> Option<ModelRouteRef> {
        self.preferred_routes.get(provider).cloned()
    }

    pub fn preferred_route_for_model(&self, model_ref: &ModelRef) -> Option<ModelRouteRef> {
        self.preferred_routes_by_model.get(model_ref).cloned()
    }

    pub fn get_route(&self, route_ref: &ModelRouteRef) -> Option<&BuiltInModelMetadata> {
        self.route_entries.get(route_ref)
    }

    pub fn resolve_route_policy(
        &self,
        route_ref: &ModelRouteRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
    ) -> ResolvedRuntimeModelPolicy {
        let model_ref = route_ref.model_ref();
        self.resolve_policy_with_builtin(
            &model_ref,
            self.route_entries.get(route_ref),
            overrides,
            discovered_models,
            unknown_fallback,
            base_context_config,
            configured_runtime_max_output_tokens,
            Some(&route_ref.endpoint),
        )
    }

    pub fn resolve_policy(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
    ) -> ResolvedRuntimeModelPolicy {
        self.resolve_policy_with_builtin(
            model_ref,
            self.get(model_ref),
            overrides,
            discovered_models,
            unknown_fallback,
            base_context_config,
            configured_runtime_max_output_tokens,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_policy_with_builtin(
        &self,
        model_ref: &ModelRef,
        route_builtin: Option<&BuiltInModelMetadata>,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
        endpoint: Option<&ProviderEndpointId>,
    ) -> ResolvedRuntimeModelPolicy {
        let discovered = discovered_models.get(model_ref);
        let built_in = discovered.or(route_builtin);
        let override_config = overrides.get(model_ref);
        let fallback_override = if built_in.is_none() {
            unknown_fallback
        } else {
            None
        };
        let source = if override_config.is_some() || fallback_override.is_some() {
            if built_in.is_some() {
                ModelMetadataSource::ConfigOverride
            } else {
                ModelMetadataSource::UnknownFallback
            }
        } else {
            built_in
                .map(|entry| entry.source)
                .unwrap_or(ModelMetadataSource::UnknownFallback)
        };
        let display_name = override_config
            .and_then(|value| value.display_name.clone())
            .or_else(|| built_in.map(|entry| entry.display_name.clone()))
            .or_else(|| fallback_override.and_then(|value| value.display_name.clone()))
            .unwrap_or_else(|| model_ref.as_string());
        let description = override_config
            .and_then(|value| value.description.clone())
            .or_else(|| built_in.map(|entry| entry.description.clone()))
            .or_else(|| fallback_override.and_then(|value| value.description.clone()))
            .unwrap_or_else(|| "Explicit unknown-model fallback policy".to_string());
        let context_window_tokens = override_config
            .and_then(|value| value.context_window_tokens)
            .or_else(|| built_in.and_then(|entry| entry.context_window_tokens))
            .or_else(|| fallback_override.and_then(|value| value.context_window_tokens));
        let effective_context_window_percent = validated_percent(
            override_config
                .and_then(|value| value.effective_context_window_percent)
                .or_else(|| built_in.map(|entry| entry.effective_context_window_percent))
                .or_else(|| {
                    fallback_override.and_then(|value| value.effective_context_window_percent)
                })
                .unwrap_or(DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT),
        );
        let prompt_budget_estimated_tokens = override_config
            .and_then(|value| value.prompt_budget_estimated_tokens)
            .or_else(|| {
                context_window_tokens
                    .map(|window| percent_of(window, usize::from(effective_context_window_percent)))
            })
            .or_else(|| fallback_override.and_then(|value| value.prompt_budget_estimated_tokens))
            .unwrap_or_else(|| {
                if built_in.is_none() {
                    DEFAULT_UNKNOWN_FALLBACK_PROMPT_BUDGET_ESTIMATED_TOKENS
                } else {
                    base_context_config.prompt_budget_estimated_tokens
                }
            });
        let auto_compact_token_limit = override_config
            .and_then(|value| value.auto_compact_token_limit)
            .or_else(|| built_in.and_then(|entry| entry.auto_compact_token_limit))
            .or_else(|| fallback_override.and_then(|value| value.auto_compact_token_limit));
        let compaction_trigger_estimated_tokens = override_config
            .and_then(|value| value.compaction_trigger_estimated_tokens)
            .or_else(|| {
                fallback_override.and_then(|value| value.compaction_trigger_estimated_tokens)
            })
            .or(auto_compact_token_limit)
            .or_else(|| {
                Some(percent_of(
                    prompt_budget_estimated_tokens,
                    usize::from(DEFAULT_COMPACTION_TRIGGER_PERCENT),
                ))
            })
            .unwrap_or(base_context_config.compaction_trigger_estimated_tokens);
        let compaction_keep_recent_estimated_tokens = override_config
            .and_then(|value| value.compaction_keep_recent_estimated_tokens)
            .or_else(|| {
                fallback_override.and_then(|value| value.compaction_keep_recent_estimated_tokens)
            })
            .or_else(|| {
                Some(percent_of(
                    compaction_trigger_estimated_tokens,
                    usize::from(DEFAULT_KEEP_RECENT_PERCENT),
                ))
            })
            .unwrap_or(base_context_config.compaction_keep_recent_estimated_tokens);
        let runtime_max_output_tokens = override_config
            .and_then(|value| value.runtime_max_output_tokens)
            .or_else(|| built_in.and_then(|entry| entry.default_max_output_tokens))
            .or_else(|| fallback_override.and_then(|value| value.runtime_max_output_tokens))
            .unwrap_or(configured_runtime_max_output_tokens);
        let max_output_tokens_upper_limit =
            built_in.and_then(|entry| entry.max_output_tokens_upper_limit);
        // Clamp runtime_max_output_tokens to the model's declared upper limit so
        // wire-level requests (e.g. Anthropic max_tokens) never exceed what the
        // provider accepts.
        let runtime_max_output_tokens = match max_output_tokens_upper_limit {
            Some(upper) => runtime_max_output_tokens.min(upper),
            None => runtime_max_output_tokens,
        };
        let verbosity = override_config
            .and_then(|value| value.verbosity)
            .or_else(|| built_in.and_then(|entry| entry.default_verbosity))
            .or_else(|| fallback_override.and_then(|value| value.verbosity))
            .or_else(|| default_verbosity_for_model(model_ref));
        let tool_output_truncation_estimated_tokens = override_config
            .and_then(|value| value.tool_output_truncation_estimated_tokens)
            .or_else(|| built_in.and_then(|entry| entry.tool_output_truncation_estimated_tokens))
            .or_else(|| {
                fallback_override.and_then(|value| value.tool_output_truncation_estimated_tokens)
            })
            .unwrap_or(DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS);
        let mut capabilities = built_in
            .map(|entry| entry.capabilities.clone())
            .unwrap_or_default();
        if let Some(fallback_capabilities) =
            fallback_override.and_then(|value| value.capabilities.as_ref())
        {
            fallback_capabilities.apply_to(&mut capabilities);
        }
        if let Some(override_capabilities) =
            override_config.and_then(|value| value.capabilities.as_ref())
        {
            override_capabilities.apply_to(&mut capabilities);
        }
        let reasoning_effort_options = built_in
            .map(|entry| entry.reasoning_effort_options.clone())
            .filter(|options| !options.is_empty())
            .unwrap_or_else(|| reasoning_effort_options(model_ref, endpoint, &capabilities));

        ResolvedRuntimeModelPolicy {
            model_ref: model_ref.clone(),
            display_name,
            description,
            context_window_tokens,
            effective_context_window_percent,
            prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens,
            runtime_max_output_tokens,
            verbosity,
            tool_output_truncation_estimated_tokens,
            max_output_tokens_upper_limit,
            capabilities,
            reasoning_effort_options,
            source,
        }
    }

    pub fn apply_policy(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
    ) -> (ContextConfig, ResolvedRuntimeModelPolicy) {
        let policy = self.resolve_policy(
            model_ref,
            overrides,
            discovered_models,
            unknown_fallback,
            base_context_config,
            configured_runtime_max_output_tokens,
        );
        let context_config = ContextConfig {
            recent_messages: base_context_config.recent_messages,
            recent_briefs: base_context_config.recent_briefs,
            compaction_trigger_messages: base_context_config.compaction_trigger_messages,
            compaction_keep_recent_messages: base_context_config.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: policy.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: policy.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: policy.compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: base_context_config.recent_episode_candidates,
            max_relevant_episodes: base_context_config.max_relevant_episodes,
            turn_projection_budget_ratio: base_context_config.turn_projection_budget_ratio,
            turn_projection_min_budget: base_context_config.turn_projection_min_budget,
            turn_projection_max_budget: base_context_config.turn_projection_max_budget,
            callback_base_url: String::new(),
        };
        (context_config, policy)
    }
}

impl Default for BuiltInModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}

fn default_effective_context_window_percent() -> u8 {
    DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT
}

fn validated_percent(percent: u8) -> u8 {
    percent.clamp(1, 100)
}

fn percent_of(total: usize, percent: usize) -> usize {
    total.saturating_mul(percent) / 100
}

fn provider_id(provider: &str) -> ProviderId {
    ProviderId::parse(provider).expect("valid built-in provider id")
}

fn reasoning_effort_options(
    model_ref: &ModelRef,
    endpoint: Option<&ProviderEndpointId>,
    capabilities: &ModelCapabilityFlags,
) -> Vec<String> {
    if !capabilities.supports_reasoning {
        return Vec::new();
    }

    let options = match (model_ref.provider.as_str(), model_ref.model.as_str()) {
        ("openai-codex", "gpt-5.6-sol" | "gpt-5.6-terra") => {
            &["low", "medium", "high", "xhigh", "max"][..]
        }
        ("openai-codex", "gpt-5.6-luna") => &["low", "medium", "high", "xhigh", "max"][..],
        ("openai", _) | ("openai-codex", _) => &["low", "medium", "high", "xhigh"][..],
        ("xai", "grok-4.3") => &["none", "low", "medium", "high"][..],
        ("xai", "grok-4.5") => &["low", "medium", "high"][..],
        ("stepfun", "step-3.7-flash") => &["low", "medium", "high"][..],
        ("stepfun", "step-3.5-flash-2603") => &["low", "high"][..],
        ("fireworks", "accounts/fireworks/models/deepseek-v4-flash")
        | ("fireworks", "accounts/fireworks/models/deepseek-v4-pro") => {
            &["none", "low", "medium", "high", "xhigh", "max"][..]
        }
        ("fireworks", "accounts/fireworks/models/glm-5p2") => &["none", "high", "max"][..],
        ("fireworks", "accounts/fireworks/models/gpt-oss-120b")
        | ("fireworks", "accounts/fireworks/models/minimax-m2p7") => &["low", "medium", "high"][..],
        ("huggingface", "openai/gpt-oss-120b") => &["low", "medium", "high"][..],
        ("venice", "zai-org-glm-4.7" | "qwen3-235b-a22b-thinking-2507") => {
            &["low", "medium", "high"][..]
        }
        ("zai" | "bigmodel", "glm-5.2") => &["high", "max"][..],
        ("xiaomi" | "xiaomi-token-plan", "mimo-v2.5-pro" | "mimo-v2.5") => &["none", "high"][..],
        ("volcengine", _)
            if endpoint.is_none_or(|endpoint| {
                matches!(endpoint.as_str(), "default" | "coding" | "plan")
            }) && !matches!(model_ref.model.as_str(), "kimi-k2.6" | "kimi-k2.7-code") =>
        {
            &["low", "medium", "high"][..]
        }
        _ => &[][..],
    };
    options.iter().map(|option| (*option).to_string()).collect()
}

fn catalog_model(
    provider: &str,
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: u32,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id(provider), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon built-in runtime metadata for the {provider}/{model} compatible provider model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: Some(max_output_tokens),
        max_output_tokens_upper_limit: Some(max_output_tokens),
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::BuiltInCatalog,
        endpoint: None,
    }
}

fn opencode_go_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: Option<u32>,
    supports_reasoning: bool,
    image_input: bool,
    endpoint: Option<&str>,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("opencode-go"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the OpenCode Go {model} route."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: max_output_tokens,
        max_output_tokens_upper_limit: max_output_tokens,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: endpoint.map(|endpoint| {
            ProviderEndpointId::parse(endpoint).expect("valid OpenCode Go endpoint id")
        }),
    }
}

fn venice_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: u32,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("venice"), model);
    let capabilities = ModelCapabilityFlags {
        image_input,
        supports_reasoning,
        ..ModelCapabilityFlags::default()
    };
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref: model_ref.clone(),
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Venice {model} text model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: Some(max_output_tokens),
        max_output_tokens_upper_limit: Some(max_output_tokens),
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        reasoning_effort_options: reasoning_effort_options(&model_ref, None, &capabilities),
        capabilities,
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn chutes_model_without_published_capabilities(
    model: &str,
    context_window_tokens: usize,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("chutes"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: model.into(),
        description: format!(
            "Holon conservative built-in metadata for the Chutes {model} model; the public model entry does not publish complete capability fields."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags::default(),
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn arcee_model(model: &str, display_name: &str) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("arcee"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Arcee hosted {model} model."
        ),
        context_window_tokens: Some(131_072),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags::default(),
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn huggingface_gpt_oss_model() -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("huggingface"), "openai/gpt-oss-120b");
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: "OpenAI GPT OSS 120B".into(),
        description:
            "Holon conservative built-in metadata for the Hugging Face Inference Providers quick-start chat model."
                .into(),
        context_window_tokens: Some(131_072),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: vec!["low".into(), "medium".into(), "high".into()],
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn kilocode_auto_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: u32,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("kilocode"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Kilo Gateway {display_name} virtual model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: Some(max_output_tokens),
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn fireworks_model(
    model: &str,
    display_name: &str,
    context_window_tokens: Option<usize>,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("fireworks"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Fireworks serverless {model} model."
        ),
        context_window_tokens,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn nvidia_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("nvidia"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the NVIDIA-hosted {model} model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn together_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("together"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Together serverless {model} model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn stepfun_model(
    provider: &str,
    model: &str,
    display_name: &str,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id(provider), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!("Holon built-in runtime metadata for the StepFun {model} model."),
        context_window_tokens: Some(262_144),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::BuiltInCatalog,
        endpoint: None,
    }
}

fn tencent_tokenhub_model(
    provider: &str,
    model: &str,
    display_name: &str,
    context_window_tokens: Option<usize>,
    max_output_tokens: Option<u32>,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id(provider), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Tencent TokenHub {model} model."
        ),
        context_window_tokens,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: max_output_tokens,
        max_output_tokens_upper_limit: max_output_tokens,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

type TencentTokenHubModelSpec = (
    &'static str,
    &'static str,
    Option<usize>,
    Option<u32>,
    bool,
    bool,
);

const TENCENT_TOKENHUB_MODELS: &[TencentTokenHubModelSpec] = &[
    ("hy3", "Hy3", Some(256_000), Some(128_000), true, false),
    (
        "hy3-preview",
        "Hy3 Preview",
        Some(256_000),
        Some(128_000),
        true,
        false,
    ),
    ("hy-mt2-pro", "Hy-MT2-Pro", None, None, false, false),
    ("hy-mt2-plus", "Hy-MT2-Plus", None, None, false, false),
    ("hy-mt2-lite", "Hy-MT2-Lite", None, None, false, false),
    (
        "hunyuan-role-latest",
        "Hy-Role-Latest",
        None,
        None,
        false,
        false,
    ),
    ("hy-role", "Hy-Role", None, None, false, false),
    (
        "deepseek-v4-flash",
        "DeepSeek V4 Flash",
        Some(1_000_000),
        Some(384_000),
        true,
        false,
    ),
    (
        "deepseek-v4-pro",
        "DeepSeek V4 Pro",
        Some(1_000_000),
        Some(384_000),
        true,
        false,
    ),
    (
        "deepseek-v3.2",
        "DeepSeek V3.2",
        Some(128_000),
        Some(32_768),
        true,
        false,
    ),
    (
        "glm-5.2",
        "GLM-5.2",
        Some(1_000_000),
        Some(131_072),
        true,
        false,
    ),
    (
        "glm-5.1",
        "GLM-5.1",
        Some(202_800),
        Some(131_072),
        true,
        false,
    ),
    (
        "glm-5v-turbo",
        "GLM-5V Turbo",
        Some(202_800),
        Some(131_072),
        true,
        true,
    ),
    (
        "glm-5-turbo",
        "GLM-5 Turbo",
        Some(202_800),
        Some(131_072),
        true,
        false,
    ),
    ("glm-5", "GLM-5", Some(202_800), Some(131_072), true, false),
    (
        "kimi-k2.7-code-highspeed",
        "Kimi K2.7 Code HighSpeed",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.7-code",
        "Kimi K2.7 Code",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.6",
        "Kimi K2.6",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.5",
        "Kimi K2.5",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "minimax-m3",
        "MiniMax M3",
        Some(1_000_000),
        None,
        true,
        true,
    ),
    (
        "minimax-m2.7",
        "MiniMax M2.7",
        Some(204_800),
        None,
        true,
        false,
    ),
    (
        "minimax-m2.5",
        "MiniMax M2.5",
        Some(196_608),
        Some(32_768),
        true,
        false,
    ),
    (
        "qwen3.5-flash",
        "Qwen3.5 Flash",
        Some(1_000_000),
        Some(65_536),
        true,
        true,
    ),
    (
        "qwen3.5-plus",
        "Qwen3.5 Plus",
        Some(1_000_000),
        Some(65_536),
        true,
        true,
    ),
    ("youtu-vita", "YT-VITA", None, None, false, true),
    (
        "hy-vision-2.0-instruct",
        "HY-Vision-2.0-Instruct",
        None,
        None,
        false,
        true,
    ),
    (
        "hunyuan-t1-vision-20250916",
        "HY-Vision-1.5-Thinking",
        None,
        None,
        true,
        true,
    ),
];

pub(crate) fn is_tencent_tokenhub_model_id(id: &str) -> bool {
    TENCENT_TOKENHUB_MODELS.iter().any(|model| model.0 == id)
}

fn tencent_tokenhub_model_entries() -> Vec<BuiltInModelMetadata> {
    ["tencent-tokenhub", "tencent-tokenhub-messages"]
        .into_iter()
        .flat_map(|provider| {
            TENCENT_TOKENHUB_MODELS.iter().map(move |model| {
                tencent_tokenhub_model(
                    provider, model.0, model.1, model.2, model.3, model.4, model.5,
                )
            })
        })
        .collect()
}

fn is_turn_default_candidate(entry: &BuiltInModelMetadata) -> bool {
    entry.context_window_tokens.is_some()
        || entry.capabilities.parallel_tool_calls
        || entry.capabilities.image_input
        || entry.capabilities.supports_reasoning
        || entry.capabilities.interactive_exec
}

fn built_in_entries() -> Vec<BuiltInModelMetadata> {
    let mut entries = vec![
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-fable-5"),
            display_name: "Claude Fable 5".into(),
            description: "Anthropic Claude Fable 5 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: Some(128_000),
            max_output_tokens_upper_limit: Some(128_000),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-opus-4-8"),
            display_name: "Claude Opus 4.8".into(),
            description: "Anthropic Claude Opus 4.8 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: Some(128_000),
            max_output_tokens_upper_limit: Some(128_000),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-sonnet-5"),
            display_name: "Claude Sonnet 5".into(),
            description: "Anthropic Claude Sonnet 5 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: Some(128_000),
            max_output_tokens_upper_limit: Some(128_000),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-haiku-4-5"),
            display_name: "Claude Haiku 4.5".into(),
            description: "Anthropic Claude Haiku 4.5 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(200_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(180_000),
            default_max_output_tokens: Some(64_000),
            max_output_tokens_upper_limit: Some(64_000),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.5"),
            display_name: "GPT-5.5 (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.6-sol"),
            display_name: "GPT-5.6-Sol (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(372_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.6-terra"),
            display_name: "GPT-5.6-Terra (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(372_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.6-luna"),
            display_name: "GPT-5.6-Luna (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(372_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.4"),
            display_name: "GPT-5.4 (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.4-mini"),
            display_name: "GPT-5.4-Mini (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Medium),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.3-codex-spark"),
            display_name: "GPT-5.3 Codex Spark (Codex)".into(),
            description: "Codex Spark runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-image-2"),
            display_name: "GPT Image 2".into(),
            description: "OpenAI image generation model for the Images API.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_generation: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.4"),
            display_name: "GPT-5.4".into(),
            description: "Conservative GPT-5.4 runtime defaults aligned with local Codex model behavior.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.3"),
            display_name: "GPT-5.3".into(),
            description: "Conservative GPT-5.3 runtime defaults used when explicit model metadata is not available locally.".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.4-mini"),
            display_name: "GPT-5.4 Mini".into(),
            description: "Conservative GPT-5.4 Mini runtime defaults used when explicit model metadata is not available locally.".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: None,
        },
    ];
    entries.extend(compatible_provider_model_entries());
    entries
}

fn default_verbosity_for_model(model_ref: &ModelRef) -> Option<ModelVerbosity> {
    (model_ref.provider == ProviderId::openai_codex()).then_some(ModelVerbosity::Low)
}

fn compatible_provider_model_entries() -> Vec<BuiltInModelMetadata> {
    let mut entries = vec![
        catalog_model(
            "openai",
            "gpt-5.6-sol",
            "GPT-5.6 Sol",
            372_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "openai",
            "gpt-5.6-terra",
            "GPT-5.6 Terra",
            372_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "openai",
            "gpt-5.6-luna",
            "GPT-5.6 Luna",
            372_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-3.5-flash",
            "Gemini 3.5 Flash",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-3.1-pro-preview",
            "Gemini 3.1 Pro Preview",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-3.1-flash-lite",
            "Gemini 3.1 Flash-Lite",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-2.5-pro",
            "Gemini 2.5 Pro",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-2.5-flash",
            "Gemini 2.5 Flash",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-2.5-flash-lite",
            "Gemini 2.5 Flash-Lite",
            1_048_576,
            65_536,
            true,
            true,
        ),
        arcee_model("trinity-mini", "Trinity Mini 26B"),
        arcee_model("trinity-large-preview", "Trinity Large Preview"),
        catalog_model(
            "chutes",
            "moonshotai/Kimi-K2.6-TEE",
            "moonshotai/Kimi-K2.6-TEE",
            262_144,
            65_535,
            true,
            true,
        ),
        catalog_model("chutes", "zai-org/GLM-5.2-TEE", "zai-org/GLM-5.2-TEE", 1_048_576, 65_535, true, false),
        catalog_model("chutes", "zai-org/GLM-5.1-TEE", "zai-org/GLM-5.1-TEE", 202_752, 65_535, true, false),
        catalog_model("chutes", "zai-org/GLM-5-TEE", "zai-org/GLM-5-TEE", 202_752, 65_535, true, false),
        catalog_model("chutes", "deepseek-ai/DeepSeek-V3.2-TEE", "deepseek-ai/DeepSeek-V3.2-TEE", 131_072, 65_536, true, false),
        catalog_model("chutes", "MiniMaxAI/MiniMax-M2.5-TEE", "MiniMaxAI/MiniMax-M2.5-TEE", 196_608, 65_536, true, false),
        catalog_model("chutes", "moonshotai/Kimi-K2.5-TEE", "moonshotai/Kimi-K2.5-TEE", 262_144, 65_535, true, true),
        catalog_model("chutes", "Qwen/Qwen3.6-27B-TEE", "Qwen/Qwen3.6-27B-TEE", 262_144, 65_536, true, true),
        catalog_model("chutes", "Qwen/Qwen3.5-397B-A17B-TEE", "Qwen/Qwen3.5-397B-A17B-TEE", 262_144, 65_536, true, true),
        catalog_model("chutes", "Qwen/Qwen3-235B-A22B-Thinking-2507-TEE", "Qwen/Qwen3-235B-A22B-Thinking-2507-TEE", 262_144, 262_144, true, false),
        catalog_model("chutes", "Qwen/Qwen3-32B-TEE", "Qwen/Qwen3-32B-TEE", 40_960, 40_960, true, false),
        catalog_model("chutes", "google/gemma-4-31B-turbo-TEE", "google/gemma-4-31B-turbo-TEE", 131_072, 65_536, true, true),
        chutes_model_without_published_capabilities(
            "unsloth/Mistral-Nemo-Instruct-2407-TEE",
            131_072,
        ),
        catalog_model(
            "deepseek",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "deepseek",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            384_000,
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/kimi-k2p6",
            "Kimi K2.6",
            Some(262_144),
            true,
            true,
        ),
        fireworks_model(
            "accounts/fireworks/models/kimi-k2p7-code",
            "Kimi K2.7 Code",
            Some(262_144),
            true,
            true,
        ),
        fireworks_model(
            "accounts/fireworks/models/deepseek-v4-flash",
            "DeepSeek V4 Flash",
            Some(1_048_576),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/deepseek-v4-pro",
            "DeepSeek V4 Pro",
            Some(1_048_576),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/glm-5p1",
            "GLM 5.1",
            Some(202_752),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/glm-5p2",
            "GLM 5.2",
            Some(1_048_576),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/gpt-oss-120b",
            "GPT-OSS 120B",
            Some(131_072),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/minimax-m2p7",
            "MiniMax M2.7",
            Some(196_608),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/minimax-m3",
            "MiniMax M3",
            Some(524_288),
            true,
            true,
        ),
        fireworks_model(
            "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
            "Nemotron 3 Ultra NVFP4",
            Some(262_144),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/qwen3p6-plus",
            "Qwen 3.6 Plus",
            None,
            true,
            true,
        ),
        fireworks_model(
            "accounts/fireworks/models/qwen3p7-plus",
            "Qwen 3.7 Plus",
            Some(262_144),
            true,
            true,
        ),
        huggingface_gpt_oss_model(),
        kilocode_auto_model(
            "kilo-auto/frontier",
            "Kilo Auto Frontier",
            1_000_000,
            128_000,
            true,
        ),
        kilocode_auto_model(
            "kilo-auto/balanced",
            "Kilo Auto Balanced",
            1_000_000,
            65_536,
            true,
        ),
        kilocode_auto_model(
            "kilo-auto/efficient",
            "Kilo Auto Efficient",
            1_000_000,
            65_536,
            true,
        ),
        kilocode_auto_model(
            "kilo-auto/free",
            "Kilo Auto Free",
            256_000,
            10_000,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M3",
            "MiniMax M3",
            1_000_000,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.7",
            "MiniMax M2.7",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.7-highspeed",
            "MiniMax M2.7 Highspeed",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.5",
            "MiniMax M2.5",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.5-highspeed",
            "MiniMax M2.5 Highspeed",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.1",
            "MiniMax M2.1",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.1-highspeed",
            "MiniMax M2.1 Highspeed",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2",
            "MiniMax M2",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "mistral",
            "codestral-latest",
            "Codestral (latest)",
            128_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "mistral",
            "mistral-large-latest",
            "Mistral Large 3 (latest)",
            256_000,
            16_384,
            false,
            true,
        ),
        catalog_model(
            "mistral",
            "mistral-medium-latest",
            "Mistral Medium 3.5 (latest)",
            256_000,
            8_192,
            true,
            true,
        ),
        catalog_model(
            "mistral",
            "mistral-small-latest",
            "Mistral Small 4 (latest)",
            256_000,
            16_384,
            false,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.7-code-highspeed",
            "Kimi K2.7 Code HighSpeed",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.6",
            "Kimi K2.6",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.5",
            "Kimi K2.5",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "moonshot",
            "moonshot-v1-8k",
            "Moonshot V1 8K",
            8_192,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "moonshot",
            "moonshot-v1-32k",
            "Moonshot V1 32K",
            32_768,
            32_768,
            false,
            false,
        ),
        catalog_model(
            "moonshot",
            "moonshot-v1-128k",
            "Moonshot V1 128K",
            131_072,
            131_072,
            false,
            false,
        ),
        catalog_model(
            "moonshot",
            "moonshot-v1-auto",
            "Moonshot V1 Auto",
            131_072,
            131_072,
            false,
            false,
        ),
        catalog_model(
            "moonshot",
            "moonshot-v1-8k-vision-preview",
            "Moonshot V1 8K Vision Preview",
            8_192,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "moonshot",
            "moonshot-v1-32k-vision-preview",
            "Moonshot V1 32K Vision Preview",
            32_768,
            32_768,
            false,
            true,
        ),
        catalog_model(
            "moonshot",
            "moonshot-v1-128k-vision-preview",
            "Moonshot V1 128K Vision Preview",
            131_072,
            131_072,
            false,
            true,
        ),
        catalog_model(
            "nearai",
            "zai-org/GLM-5.1-FP8",
            "GLM 5.1 (NEAR AI Cloud TEE)",
            202_752,
            16_384,
            true,
            false,
        ),
        catalog_model(
            "nearai",
            "Qwen/Qwen3.6-35B-A3B-FP8",
            "Qwen 3.6 35B A3B FP8 (NEAR AI Cloud TEE)",
            262_144,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "nearai",
            "Qwen/Qwen3.5-122B-A10B",
            "Qwen 3.5 122B A10B (NEAR AI Cloud TEE)",
            262_144,
            16_384,
            true,
            true,
        ),
        catalog_model(
            "nearai",
            "Qwen/Qwen3-VL-30B-A3B-Instruct",
            "Qwen3 VL 30B A3B Instruct (NEAR AI Cloud TEE)",
            16_384,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "nearai",
            "google/gemma-4-31B-it",
            "Gemma 4 31B Instruct (NEAR AI Cloud TEE)",
            262_144,
            8_192,
            true,
            true,
        ),
        nvidia_model(
            "nvidia/nemotron-3-super-120b-a12b",
            "NVIDIA Nemotron 3 Super 120B",
            1_000_000,
            true,
            false,
        ),
        nvidia_model(
            "moonshotai/kimi-k2.6",
            "Kimi K2.6",
            262_144,
            true,
            true,
        ),
        nvidia_model(
            "minimaxai/minimax-m2.7",
            "MiniMax M2.7",
            204_800,
            true,
            false,
        ),
        nvidia_model(
            "minimaxai/minimax-m3",
            "MiniMax M3",
            1_000_000,
            true,
            true,
        ),
        nvidia_model("z-ai/glm-5.2", "GLM-5.2", 1_000_000, true, false),
        opencode_go_model(
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            Some(384_000),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            Some(384_000),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "glm-5.2",
            "GLM-5.2",
            1_000_000,
            Some(131_072),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "glm-5.1",
            "GLM-5.1",
            202_800,
            Some(131_072),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            262_144,
            Some(262_144),
            true,
            true,
            None,
        ),
        opencode_go_model(
            "kimi-k2.6",
            "Kimi K2.6",
            262_144,
            Some(262_144),
            true,
            true,
            None,
        ),
        opencode_go_model(
            "mimo-v2.5-pro",
            "MiMo V2.5 Pro",
            1_048_576,
            Some(131_072),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "mimo-v2.5",
            "MiMo V2.5",
            1_048_576,
            Some(131_072),
            true,
            true,
            None,
        ),
        opencode_go_model(
            "minimax-m3",
            "MiniMax M3",
            1_000_000,
            None,
            true,
            true,
            Some("messages"),
        ),
        opencode_go_model(
            "minimax-m2.7",
            "MiniMax M2.7",
            204_800,
            None,
            true,
            false,
            Some("messages"),
        ),
        opencode_go_model(
            "minimax-m2.5",
            "MiniMax M2.5",
            196_608,
            Some(32_768),
            true,
            false,
            Some("messages"),
        ),
        opencode_go_model(
            "qwen3.7-max",
            "Qwen3.7 Max",
            1_000_000,
            Some(65_536),
            true,
            false,
            Some("messages"),
        ),
        opencode_go_model(
            "qwen3.7-plus",
            "Qwen3.7 Plus",
            1_000_000,
            Some(65_536),
            true,
            true,
            Some("messages"),
        ),
        opencode_go_model(
            "qwen3.6-plus",
            "Qwen3.6 Plus",
            1_000_000,
            Some(65_536),
            true,
            true,
            Some("messages"),
        ),
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("openrouter"), "auto"),
            display_name: "OpenRouter Auto".into(),
            description: "OpenRouter Auto Router metadata aligned with the official Models API; the selected upstream model and provider are dynamic.".into(),
            context_window_tokens: Some(2_000_000),
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: None,
        },
        catalog_model(
            "qianfan",
            "deepseek-v3.2",
            "DEEPSEEK V3.2",
            131_072,
            32_768,
            false,
            false,
        ),
        catalog_model(
            "qianfan",
            "deepseek-v3.2-think",
            "DEEPSEEK V3.2 Think",
            163_840,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "qianfan",
            "ernie-5.0",
            "ERNIE 5.0",
            248_832,
            65_536,
            false,
            true,
        ),
        catalog_model(
            "qianfan",
            "ernie-5.0-thinking-preview",
            "ERNIE 5.0 Thinking Preview",
            248_832,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "qianfan",
            "ernie-5.1",
            "ERNIE 5.1",
            248_832,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "qianfan",
            "ernie-x1.1",
            "ERNIE X1.1",
            121_856,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-plus",
            "qwen3.7-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-plus-2026-05-26",
            "qwen3.7-plus-2026-05-26",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-max",
            "qwen3.7-max",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-max-2026-06-08",
            "qwen3.7-max-2026-06-08",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-max-2026-05-20",
            "qwen3.7-max-2026-05-20",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3.5-plus",
            "qwen3.5-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.6-plus",
            "qwen3.6-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.6-flash",
            "qwen3.6-flash",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.5-flash",
            "qwen3.5-flash",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3-max-2026-01-23",
            "qwen3-max-2026-01-23",
            262_144,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3-coder-next",
            "qwen3-coder-next",
            262_144,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3-coder-plus",
            "qwen3-coder-plus",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3-coder-flash",
            "qwen3-coder-flash",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "qwen3.7-max",
            "qwen3.7-max",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "qwen3.7-plus",
            "qwen3.7-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope-token-plan",
            "qwen3.6-plus",
            "qwen3.6-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope-token-plan",
            "qwen3.6-flash",
            "qwen3.6-flash",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope-token-plan",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            65_536, // Capped: DashScope gateway leaks </think> when max_output_tokens > 65536
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            65_536, // Capped: DashScope gateway leaks </think> when max_output_tokens > 65536
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "deepseek-v3.2",
            "DeepSeek V3.2",
            128_000,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "kimi-k2.7-code",
            "kimi-k2.7-code",
            262_144,
            65_536, // Capped: DashScope gateway leaks </think> when max_output_tokens > 65536
            true,
            true,
        ),
        catalog_model(
            "dashscope-token-plan",
            "kimi-k2.6",
            "kimi-k2.6",
            262_144,
            65_536, // Capped: DashScope gateway leaks </think> when max_output_tokens > 65536
            true,
            true,
        ),
        catalog_model(
            "dashscope-token-plan",
            "kimi-k2.5",
            "kimi-k2.5",
            262_144,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "dashscope-token-plan",
            "glm-5.2",
            "glm-5.2",
            1_000_000,
            65_536, // Capped: DashScope gateway leaks </think> when max_output_tokens > 65536
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "glm-5.1",
            "glm-5.1",
            202_752,
            65_536, // Capped: DashScope gateway leaks </think> when max_output_tokens > 65536
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "glm-5",
            "glm-5",
            202_752,
            16_384,
            true,
            false,
        ),
        catalog_model(
            "dashscope-token-plan",
            "MiniMax-M2.5",
            "MiniMax-M2.5",
            196_608,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "qwen3.7-plus",
            "qwen3.7-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "qwen3.6-plus",
            "qwen3.6-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "qwen3.5-plus",
            "qwen3.5-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "qwen3-max-2026-01-23",
            "qwen3-max-2026-01-23",
            262_144,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "qwen3-coder-next",
            "qwen3-coder-next",
            262_144,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "qwen3-coder-plus",
            "qwen3-coder-plus",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "MiniMax-M2.5",
            "MiniMax-M2.5",
            196_608,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "glm-5",
            "glm-5",
            202_752,
            16_384,
            true,
            false,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "glm-4.7",
            "glm-4.7",
            202_752,
            16_384,
            true,
            false,
        ),
        catalog_model(
            "dashscope-coding-plan",
            "kimi-k2.5",
            "kimi-k2.5",
            262_144,
            32_768,
            true,
            true,
        ),
        stepfun_model("stepfun", "step-3.7-flash", "Step 3.7 Flash", true),
        stepfun_model(
            "stepfun",
            "step-3.5-flash-2603",
            "Step 3.5 Flash 2603",
            false,
        ),
        stepfun_model("stepfun", "step-3.5-flash", "Step 3.5 Flash", false),
        stepfun_model(
            "stepfun-plan",
            "step-3.7-flash",
            "Step 3.7 Flash",
            true,
        ),
        stepfun_model(
            "stepfun-plan",
            "step-3.5-flash-2603",
            "Step 3.5 Flash 2603",
            false,
        ),
        stepfun_model(
            "stepfun-plan",
            "step-3.5-flash",
            "Step 3.5 Flash",
            false,
        ),
        catalog_model("synthetic", "syn:large:text", "Synthetic Large Text", 524_288, 65_536, true, false),
        catalog_model("synthetic", "syn:small:text", "Synthetic Small Text", 196_608, 65_536, true, false),
        catalog_model("synthetic", "syn:large:vision", "Synthetic Large Vision", 262_144, 65_536, true, true),
        catalog_model("synthetic", "syn:small:vision", "Synthetic Small Vision", 262_144, 65_536, true, true),
        catalog_model("synthetic", "hf:openai/gpt-oss-120b", "OpenAI GPT OSS 120B", 131_072, 65_536, true, false),
        catalog_model("synthetic", "hf:zai-org/GLM-5.2", "Z.ai GLM-5.2", 524_288, 65_536, true, false),
        catalog_model("synthetic", "hf:moonshotai/Kimi-K2.7-Code", "Moonshot AI Kimi K2.7 Code", 262_144, 65_536, true, true),
        catalog_model("synthetic", "hf:Qwen/Qwen3.6-27B", "Qwen3.6 27B", 262_144, 65_536, true, true),
        catalog_model("synthetic", "hf:MiniMaxAI/MiniMax-M3", "MiniMax M3", 262_144, 65_536, true, true),
        catalog_model("synthetic", "hf:zai-org/GLM-4.7-Flash", "Z.ai GLM-4.7 Flash", 196_608, 65_536, true, false),
        catalog_model("synthetic", "hf:nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-NVFP4", "NVIDIA Nemotron 3 Super 120B A12B NVFP4", 262_144, 65_536, true, false),
        together_model(
            "MiniMaxAI/MiniMax-M3",
            "MiniMax M3",
            524_288,
            false,
            true,
        ),
        together_model(
            "MiniMaxAI/MiniMax-M2.7",
            "MiniMax M2.7",
            202_752,
            true,
            false,
        ),
        together_model(
            "Qwen/Qwen3.5-9B",
            "Qwen3.5 9B",
            262_144,
            true,
            true,
        ),
        together_model(
            "moonshotai/Kimi-K2.7-Code",
            "Kimi K2.7 Code",
            262_144,
            false,
            true,
        ),
        together_model(
            "moonshotai/Kimi-K2.6",
            "Kimi K2.6",
            262_144,
            true,
            true,
        ),
        together_model("zai-org/GLM-5.2", "GLM-5.2", 262_144, false, false),
        together_model(
            "openai/gpt-oss-120b",
            "GPT-OSS 120B",
            128_000,
            true,
            false,
        ),
        together_model(
            "deepseek-ai/DeepSeek-V4-Pro",
            "DeepSeek V4 Pro",
            512_000,
            true,
            false,
        ),
        together_model(
            "nvidia/nemotron-3-ultra-550b-a55b",
            "NVIDIA Nemotron 3 Ultra 550B",
            512_300,
            true,
            false,
        ),
        together_model(
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "Llama 3.3 70B Instruct Turbo",
            131_072,
            false,
            false,
        ),
        venice_model(
            "zai-org-glm-4.7",
            "GLM 4.7 (via Venice)",
            198_000,
            16_384,
            true,
            false,
        ),
        venice_model(
            "qwen3-235b-a22b-thinking-2507",
            "Qwen3 235B A22B Thinking 2507 (via Venice)",
            128_000,
            16_384,
            true,
            false,
        ),
        venice_model(
            "qwen3-vl-235b-a22b",
            "Qwen3 VL 235B (via Venice)",
            128_000,
            16_384,
            false,
            true,
        ),
        venice_model(
            "qwen3-coder-480b-a35b-instruct-turbo",
            "Qwen3 Coder 480B Turbo (via Venice)",
            256_000,
            65_536,
            false,
            false,
        ),
        venice_model(
            "venice-uncensored-1-2",
            "Venice Uncensored 1.2",
            128_000,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "anthropic/claude-opus-4.6",
            "Claude Opus 4.6",
            1_000_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "openai/gpt-5.4",
            "GPT 5.4",
            1_050_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "openai/gpt-5.4-pro",
            "GPT 5.4 Pro",
            1_050_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "moonshotai/kimi-k2.6",
            "Kimi K2.6",
            262_000,
            262_000,
            true,
            true,
        ),
        catalog_model(
            "volcengine",
            "doubao-seed-1-8-251228",
            "Doubao Seed 1.8",
            256_000,
            4_096,
            false,
            true,
        ),
        catalog_model(
            "volcengine",
            "deepseek-v3-2-251201",
            "DeepSeek V3.2",
            128_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine",
            "doubao-seed-2-0-pro-260215",
            "Doubao Seed 2.0 Pro",
            256_000,
            4_096,
            true,
            true,
        ),
        catalog_model(
            "volcengine",
            "doubao-seed-2-0-code-preview-260215",
            "Doubao Seed 2.0 Code Preview",
            256_000,
            4_096,
            false,
            true,
        ),
        catalog_model(
            "volcengine",
            "doubao-seed-2-0-lite-260215",
            "Doubao Seed 2.0 Lite",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "ark-code-latest",
            "Ark Code Latest",
            256_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "doubao-seed-2-0-code-preview-260215",
            "Doubao Seed 2.0 Code",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "doubao-seed-2-0-pro-260215",
            "Doubao Seed 2.0 Pro",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "doubao-seed-2-0-lite-260215",
            "Doubao Seed 2.0 Lite",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "deepseek-v3-2-251201",
            "DeepSeek V3.2",
            128_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "kimi-k2.6",
            "Kimi K2.6",
            262_144,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            262_144,
            // Volcengine API rejects max_tokens > 32768 for kimi-k2.7-code
            32_768,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "glm-5.2",
            "GLM-5.2",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "ark-code-latest",
            "Ark Code Latest",
            256_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "doubao-seed-2-0-code-preview-260215",
            "Doubao Seed 2.0 Code",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "doubao-seed-2-0-pro-260215",
            "Doubao Seed 2.0 Pro",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "doubao-seed-2-0-lite-260215",
            "Doubao Seed 2.0 Lite",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "kimi-k2.6",
            "Kimi K2.6",
            262_144,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            262_144,
            // Volcengine API rejects max_tokens > 32768 for kimi-k2.7-code
            32_768,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "glm-5.2",
            "GLM-5.2",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "doubao-seed-2-0-mini-260215",
            "Doubao Seed 2.0 Mini",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "MiniMax-M3",
            "MiniMax M3",
            1_000_000,
            8_192,
            true,
            false,
        ),
        BuiltInModelMetadata {
            model_ref: ModelRef::new(
                provider_id("volcengine"),
                "doubao-seedream-5.0-lite",
            ),
            display_name: "Doubao Seedream 5.0 Lite".into(),
            description: "Volcengine Ark Seedream image generation model for the OpenAI Images-compatible API.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                image_generation: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        },
        catalog_model(
            "xiaomi",
            "mimo-v2.5-pro",
            "Xiaomi MiMo V2.5 Pro",
            1_048_576,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "xiaomi",
            "mimo-v2.5",
            "Xiaomi MiMo V2.5",
            1_048_576,
            131_072,
            true,
            true,
        ),
        catalog_model(
            "xiaomi-token-plan",
            "mimo-v2.5-pro",
            "Xiaomi MiMo V2.5 Pro",
            1_048_576,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "xiaomi-token-plan",
            "mimo-v2.5",
            "Xiaomi MiMo V2.5",
            1_048_576,
            131_072,
            true,
            true,
        ),
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xai"), "grok-4.3"),
            display_name: "Grok 4.3".into(),
            description:
                "xAI Grok 4.3 runtime defaults aligned with the official model page.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xai"), "grok-4.5"),
            display_name: "Grok 4.5".into(),
            description:
                "xAI Grok 4.5 runtime defaults aligned with the official model page.".into(),
            context_window_tokens: Some(500_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(450_000),
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        catalog_model("zai", "glm-5.2", "GLM-5.2", 1_000_000, 131_072, true, false),
        catalog_model("zai", "glm-5.1", "GLM-5.1", 202_800, 131_072, true, false),
        catalog_model("zai", "glm-5", "GLM-5", 202_800, 131_072, true, false),
        catalog_model(
            "zai",
            "glm-5-turbo",
            "GLM-5 Turbo",
            202_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-5v-turbo",
            "GLM-5V Turbo",
            202_800,
            131_072,
            true,
            true,
        ),
        catalog_model("zai", "glm-4.7", "GLM-4.7", 204_800, 131_072, true, false),
        catalog_model(
            "zai",
            "glm-4.7-flash",
            "GLM-4.7 Flash",
            200_000,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.7-flashx",
            "GLM-4.7 FlashX",
            200_000,
            128_000,
            true,
            false,
        ),
        catalog_model("zai", "glm-4.6", "GLM-4.6", 204_800, 131_072, true, false),
        catalog_model("zai", "glm-4.6v", "GLM-4.6V", 128_000, 32_768, true, true),
        catalog_model("zai", "glm-4.5", "GLM-4.5", 131_072, 98_304, true, false),
        catalog_model(
            "zai",
            "glm-4.5-air",
            "GLM-4.5 Air",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.5-x",
            "GLM-4.5 X",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.5-airx",
            "GLM-4.5 AirX",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.5-flash",
            "GLM-4.5 Flash",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model("zai", "glm-4.5v", "GLM-4.5V", 64_000, 16_384, true, true),
        catalog_model(
            "zai",
            "glm-4.6v-flashx",
            "GLM-4.6V FlashX",
            128_000,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "zai",
            "glm-4.6v-flash",
            "GLM-4.6V Flash",
            128_000,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "zai",
            "glm-4-32b-0414-128k",
            "GLM-4 32B 0414 128K",
            131_072,
            16_384,
            false,
            false,
        ),
    ];
    entries.extend([
        catalog_model(
            "bigmodel", "glm-5.2", "GLM-5.2", 1_000_000, 131_072, true, false,
        ),
        catalog_model(
            "bigmodel", "glm-5.1", "GLM-5.1", 204_800, 131_072, true, false,
        ),
        catalog_model("bigmodel", "glm-5", "GLM-5", 204_800, 131_072, true, false),
        catalog_model(
            "bigmodel",
            "glm-5-turbo",
            "GLM-5 Turbo",
            204_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-5v-turbo",
            "GLM-5V Turbo",
            204_800,
            131_072,
            true,
            true,
        ),
        catalog_model(
            "bigmodel", "glm-4.7", "GLM-4.7", 204_800, 131_072, true, false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.7-flashx",
            "GLM-4.7 FlashX",
            204_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.7-flash",
            "GLM-4.7 Flash",
            204_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "bigmodel", "glm-4.6", "GLM-4.6", 204_800, 131_072, true, false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.5-air",
            "GLM-4.5 Air",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.5-airx",
            "GLM-4.5 AirX",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.5-flash",
            "GLM-4.5 Flash",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4-long",
            "GLM-4 Long",
            1_000_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4-flashx-250414",
            "GLM-4 FlashX 250414",
            131_072,
            16_384,
            false,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4-flash-250414",
            "GLM-4 Flash 250414",
            131_072,
            16_384,
            false,
            false,
        ),
        catalog_model(
            "bigmodel", "glm-4.6v", "GLM-4.6V", 131_072, 32_768, true, true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.6v-flash",
            "GLM-4.6V Flash",
            131_072,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.1v-thinking-flashx",
            "GLM-4.1V Thinking FlashX",
            65_536,
            16_384,
            true,
            true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.1v-thinking-flash",
            "GLM-4.1V Thinking Flash",
            65_536,
            16_384,
            true,
            true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4v-flash",
            "GLM-4V Flash",
            16_384,
            1_024,
            false,
            true,
        ),
        catalog_model(
            "dashscope",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "ZHIPU/GLM-5.2",
            "ZHIPU/GLM-5.2",
            1_000_000,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "glm-5.2",
            "glm-5.2",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "glm-5.1",
            "glm-5.1",
            202_752,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "kimi-k2.7-code",
            "kimi-k2.7-code",
            262_144,
            98_304,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "kimi-k2.6",
            "kimi-k2.6",
            262_144,
            98_304,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "MiniMax/MiniMax-M3",
            "MiniMax/MiniMax-M3",
            196_608,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "MiniMax-M2.5",
            "MiniMax-M2.5",
            196_608,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "mimo-v2.5-pro",
            "MiMo V2.5 Pro",
            1_000_000,
            131_072,
            true,
            false,
        ),
        catalog_model("dashscope", "glm-5", "glm-5", 202_752, 16_384, true, false),
        catalog_model(
            "dashscope",
            "glm-4.7",
            "glm-4.7",
            202_752,
            16_384,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "kimi-k2.5",
            "kimi-k2.5",
            262_144,
            32_768,
            true,
            true,
        ),
    ]);
    entries.extend(tencent_tokenhub_model_entries());
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_context() -> ContextConfig {
        ContextConfig {
            recent_messages: 12,
            recent_briefs: 8,
            compaction_trigger_messages: 20,
            compaction_keep_recent_messages: 8,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            ..ContextConfig::default()
        }
    }

    #[test]
    fn resolves_built_in_policy_from_known_model() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::openai_codex(), "gpt-5.4"),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.context_window_tokens, Some(272_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, 258_400);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 232_560);
        assert_eq!(policy.source, ModelMetadataSource::BuiltInCatalog);
    }

    #[test]
    fn resolves_built_in_policy_for_codex_spark_benchmark_model() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::openai_codex(), "gpt-5.3-codex-spark"),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.context_window_tokens, Some(128_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, 121_600);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 109_440);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 41_587);
        assert_eq!(policy.source, ModelMetadataSource::BuiltInCatalog);
    }

    #[test]
    fn arcee_catalog_tracks_current_hosted_models_conservatively() {
        let catalog = BuiltInModelCatalog::new();
        let arcee = ProviderId::parse("arcee").unwrap();
        let models = catalog
            .list()
            .into_iter()
            .filter(|entry| entry.model_ref.provider == arcee)
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 2);
        for model in models {
            assert!(matches!(
                model.model_ref.model.as_str(),
                "trinity-mini" | "trinity-large-preview"
            ));
            assert_eq!(model.context_window_tokens, Some(131_072));
            assert!(model.default_max_output_tokens.is_none());
            assert!(model.max_output_tokens_upper_limit.is_none());
            assert_eq!(model.capabilities, ModelCapabilityFlags::default());
            assert!(model.reasoning_effort_options.is_empty());
            assert_eq!(model.source, ModelMetadataSource::ConservativeBuiltin);
        }
        assert!(catalog
            .get(&ModelRef::new(arcee, "trinity-large-thinking"))
            .is_none());
    }

    #[test]
    fn deployment_defined_openai_compatible_providers_have_no_static_models() {
        let catalog = BuiltInModelCatalog::new();

        for provider in ["litellm", "vllm"] {
            let provider = ProviderId::parse(provider).unwrap();
            assert!(
                catalog
                    .list()
                    .into_iter()
                    .all(|entry| entry.model_ref.provider != provider),
                "{provider:?} models must come from deployment discovery"
            );
        }
    }

    #[test]
    fn anthropic_catalog_tracks_current_generally_available_models() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("claude-fable-5", 1_000_000, 128_000),
            ("claude-opus-4-8", 1_000_000, 128_000),
            ("claude-sonnet-5", 1_000_000, 128_000),
            ("claude-haiku-4-5", 200_000, 64_000),
        ];

        for (model, context_window, max_output) in expected {
            let metadata = catalog
                .get(&ModelRef::new(ProviderId::anthropic(), model))
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(metadata.context_window_tokens, Some(context_window));
            assert_eq!(metadata.default_max_output_tokens, Some(max_output));
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(max_output));
            assert!(metadata.capabilities.image_input);
            assert!(metadata.capabilities.supports_reasoning);
            assert!(reasoning_effort_options(
                &metadata.model_ref,
                metadata.endpoint.as_ref(),
                &metadata.capabilities,
            )
            .is_empty());
        }

        for removed_model in [
            "claude-opus-4-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-sonnet-4-5",
            "claude-sonnet-4-6",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::new(ProviderId::anthropic(), removed_model))
                    .is_none(),
                "{removed_model} should not be registered"
            );
        }
    }

    #[test]
    fn gemini_catalog_tracks_current_generate_content_models() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            "gemini-3.5-flash",
            "gemini-3.1-pro-preview",
            "gemini-3.1-flash-lite",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
        ];

        for model in expected {
            let metadata = catalog
                .get(&ModelRef::new(ProviderId::gemini(), model))
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(metadata.context_window_tokens, Some(1_048_576));
            assert_eq!(metadata.default_max_output_tokens, Some(65_536));
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(65_536));
            assert!(metadata.capabilities.image_input);
            assert!(metadata.capabilities.supports_reasoning);
            assert!(reasoning_effort_options(
                &metadata.model_ref,
                metadata.endpoint.as_ref(),
                &metadata.capabilities,
            )
            .is_empty());
        }

        for removed_model in ["gemini-3-pro", "gemini-3-flash"] {
            assert!(
                catalog
                    .get(&ModelRef::new(ProviderId::gemini(), removed_model))
                    .is_none(),
                "{removed_model} should not be registered"
            );
        }
    }

    #[test]
    fn xai_catalog_tracks_current_recommended_models() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            (
                "grok-4.3",
                1_000_000,
                &["none", "low", "medium", "high"][..],
            ),
            ("grok-4.5", 500_000, &["low", "medium", "high"][..]),
        ];

        for (model, context_window, reasoning_options) in expected {
            let metadata = catalog
                .get(&ModelRef::new(provider_id("xai"), model))
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(metadata.context_window_tokens, Some(context_window));
            assert_eq!(metadata.default_max_output_tokens, None);
            assert_eq!(metadata.max_output_tokens_upper_limit, None);
            assert!(metadata.capabilities.image_input);
            assert!(metadata.capabilities.supports_reasoning);
            assert_eq!(
                reasoning_effort_options(
                    &metadata.model_ref,
                    metadata.endpoint.as_ref(),
                    &metadata.capabilities,
                ),
                reasoning_options
            );
        }

        for removed_model in [
            "grok-3",
            "grok-3-fast",
            "grok-3-mini",
            "grok-3-mini-fast",
            "grok-4",
            "grok-4-fast",
            "grok-4-fast-non-reasoning",
            "grok-4-1-fast",
            "grok-code-fast-1",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::new(provider_id("xai"), removed_model))
                    .is_none(),
                "{removed_model} should not be registered"
            );
        }
    }

    #[test]
    fn deepseek_catalog_tracks_current_api_models() {
        let catalog = BuiltInModelCatalog::new();

        for model in ["deepseek-v4-flash", "deepseek-v4-pro"] {
            let metadata = catalog
                .get(&ModelRef::new(provider_id("deepseek"), model))
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(metadata.context_window_tokens, Some(1_000_000));
            assert_eq!(metadata.default_max_output_tokens, Some(384_000));
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(384_000));
            assert!(metadata.capabilities.supports_reasoning);
            assert!(!metadata.capabilities.image_input);
            assert!(reasoning_effort_options(
                &metadata.model_ref,
                metadata.endpoint.as_ref(),
                &metadata.capabilities,
            )
            .is_empty());
        }

        for removed_model in ["deepseek-chat", "deepseek-reasoner"] {
            assert!(
                catalog
                    .get(&ModelRef::new(provider_id("deepseek"), removed_model))
                    .is_none(),
                "{removed_model} should not be registered"
            );
        }
    }

    #[test]
    fn resolves_compatible_provider_model_policy() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::parse("deepseek/deepseek-v4-flash").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );

        assert_eq!(policy.display_name, "DeepSeek V4 Flash");
        assert_eq!(policy.context_window_tokens, Some(1_000_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, 950_000);
        assert_eq!(policy.runtime_max_output_tokens, 384_000);
        assert!(policy.capabilities.supports_reasoning);
        assert_eq!(policy.source, ModelMetadataSource::BuiltInCatalog);

        // GPT-5.6 family is available through both the OpenAI API and Codex.
        let sol = catalog.resolve_policy(
            &ModelRef::parse("openai/gpt-5.6-sol").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(sol.display_name, "GPT-5.6 Sol");
        assert_eq!(sol.context_window_tokens, Some(372_000));
        assert_eq!(sol.runtime_max_output_tokens, 128_000);
        assert!(sol.capabilities.image_input);
        assert!(sol.capabilities.supports_reasoning);
        assert_eq!(sol.source, ModelMetadataSource::BuiltInCatalog);

        assert!(catalog
            .get(&ModelRef::parse("openai/gpt-5.6-terra").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("openai/gpt-5.6-luna").unwrap())
            .is_some());
        let codex_sol = catalog
            .get(&ModelRef::parse("openai-codex/gpt-5.6-sol").unwrap())
            .unwrap();
        assert_eq!(codex_sol.context_window_tokens, Some(372_000));
        assert_eq!(
            reasoning_effort_options(
                &codex_sol.model_ref,
                codex_sol.endpoint.as_ref(),
                &codex_sol.capabilities,
            ),
            ["low", "medium", "high", "xhigh", "max"]
        );
        let codex_luna = catalog.resolve_policy(
            &ModelRef::parse("openai-codex/gpt-5.6-luna").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert!(codex_luna.validate_reasoning_effort("max").is_ok());
        assert!(codex_luna
            .validate_reasoning_effort("ultra")
            .unwrap_err()
            .to_string()
            .contains("orchestration semantics"));
        let codex_55 = catalog.resolve_policy(
            &ModelRef::parse("openai-codex/gpt-5.5").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        let unsupported = codex_55
            .validate_reasoning_effort("max")
            .unwrap_err()
            .to_string();
        assert!(unsupported.contains("openai-codex/gpt-5.5"));
        assert!(unsupported.contains("low, medium, high, xhigh"));
        assert!(catalog
            .get(&ModelRef::parse("openai/gpt-5.5").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("openai/gpt-5.2").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("openai-codex/gpt-5.2").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("openai-codex/gpt-5.3-codex").unwrap())
            .is_none());
        let codex_mini = catalog
            .get(&ModelRef::parse("openai-codex/gpt-5.4-mini").unwrap())
            .unwrap();
        assert_eq!(codex_mini.context_window_tokens, Some(272_000));
        assert_eq!(codex_mini.default_verbosity, Some(ModelVerbosity::Medium));
        let codex_spark = catalog
            .get(&ModelRef::parse("openai-codex/gpt-5.3-codex-spark").unwrap())
            .unwrap();
        assert_eq!(codex_spark.context_window_tokens, Some(128_000));
        assert!(!codex_spark.capabilities.image_input);
        assert!(!codex_spark.capabilities.image_generation);

        let nearai = catalog.resolve_policy(
            &ModelRef::parse("nearai/zai-org/GLM-5.1-FP8").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(nearai.display_name, "GLM 5.1 (NEAR AI Cloud TEE)");
        assert_eq!(nearai.context_window_tokens, Some(202_752));
        assert_eq!(nearai.runtime_max_output_tokens, 16_384);
        assert!(nearai.capabilities.supports_reasoning);
        assert_eq!(nearai.source, ModelMetadataSource::BuiltInCatalog);
    }

    #[test]
    fn resolves_anthropic_compatible_provider_model_policy() {
        let catalog = BuiltInModelCatalog::new();

        let deepseek = catalog.resolve_policy(
            &ModelRef::parse("deepseek/deepseek-v4-flash").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(deepseek.display_name, "DeepSeek V4 Flash");
        assert_eq!(deepseek.context_window_tokens, Some(1_000_000));
        assert_eq!(deepseek.runtime_max_output_tokens, 384_000);
        assert_eq!(deepseek.source, ModelMetadataSource::BuiltInCatalog);

        let xiaomi = catalog.resolve_policy(
            &ModelRef::parse("xiaomi/mimo-v2.5-pro").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(xiaomi.display_name, "Xiaomi MiMo V2.5 Pro");
        assert_eq!(xiaomi.context_window_tokens, Some(1_048_576));
        assert_eq!(xiaomi.runtime_max_output_tokens, 131_072);
        assert!(xiaomi.capabilities.supports_reasoning);
        assert!(!xiaomi.capabilities.image_input);
        assert_eq!(xiaomi.reasoning_effort_options, ["none", "high"]);
        assert_eq!(xiaomi.source, ModelMetadataSource::BuiltInCatalog);

        let xiaomi_omni = catalog.resolve_policy(
            &ModelRef::parse("xiaomi/mimo-v2.5").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(xiaomi_omni.display_name, "Xiaomi MiMo V2.5");
        assert_eq!(xiaomi_omni.context_window_tokens, Some(1_048_576));
        assert_eq!(xiaomi_omni.runtime_max_output_tokens, 131_072);
        assert!(xiaomi_omni.capabilities.supports_reasoning);
        assert!(xiaomi_omni.capabilities.image_input);
        assert_eq!(xiaomi_omni.reasoning_effort_options, ["none", "high"]);

        assert!(catalog
            .get(&ModelRef::parse("xiaomi/mimo-v2-pro").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("xiaomi/mimo-v2.5-pro-ultraspeed").unwrap())
            .is_none());
        assert_eq!(
            catalog
                .canonicalize_model_ref(&ModelRef::parse("xiaomi-token-plan/mimo-v2.5").unwrap()),
            ModelRef::parse("xiaomi/mimo-v2.5").unwrap()
        );
        let token_plan = catalog.resolve_route_policy(
            &ModelRouteRef::parse("xiaomi@token-plan/mimo-v2.5").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert!(token_plan.capabilities.image_input);
        assert_eq!(token_plan.reasoning_effort_options, ["none", "high"]);
        assert!(catalog
            .get(&ModelRef::parse("xiaomi-token-plan/mimo-v2-flash").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("xiaomi-token-plan-anthropic/mimo-v2-flash").unwrap())
            .is_none());

        let zai = catalog.resolve_policy(
            &ModelRef::parse("zai/glm-5.2").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(zai.display_name, "GLM-5.2");
        assert_eq!(zai.context_window_tokens, Some(1_000_000));
        assert_eq!(zai.runtime_max_output_tokens, 131_072);
        assert_eq!(zai.reasoning_effort_options, ["high", "max"]);
        assert_eq!(zai.source, ModelMetadataSource::BuiltInCatalog);

        let bigmodel = catalog.resolve_policy(
            &ModelRef::parse("bigmodel/glm-5.2").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(bigmodel.display_name, "GLM-5.2");
        assert_eq!(bigmodel.context_window_tokens, Some(1_000_000));
        assert_eq!(bigmodel.runtime_max_output_tokens, 131_072);
        assert_eq!(bigmodel.reasoning_effort_options, ["high", "max"]);
        assert_eq!(bigmodel.source, ModelMetadataSource::BuiltInCatalog);

        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("zai").unwrap())
                .unwrap()
                .as_string(),
            "zai/glm-5.2"
        );
        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("bigmodel").unwrap())
                .unwrap()
                .as_string(),
            "bigmodel/glm-5.2"
        );

        for model in ["glm-5.2", "glm-5.1", "glm-5", "glm-4.7", "glm-4.6"] {
            assert!(catalog
                .get(&ModelRef::new(ProviderId::parse("zai").unwrap(), model))
                .is_some());
            assert!(catalog
                .get(&ModelRef::new(
                    ProviderId::parse("bigmodel").unwrap(),
                    model
                ))
                .is_some());
        }

        assert!(catalog
            .get(&ModelRef::parse("zai/glm-4.5-x").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("zai/glm-4.5v").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("bigmodel/glm-4.5-x").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("bigmodel/glm-4.5v").unwrap())
            .is_none());

        assert!(catalog
            .get(&ModelRef::parse("bigmodel/glm-4-long").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("bigmodel/glm-4.1v-thinking-flashx").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("zai/glm-4-long").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("zai/glm-4.1v-thinking-flashx").unwrap())
            .is_none());
    }

    #[test]
    fn dashscope_covers_qwen_models_without_separate_qwen_provider() {
        let catalog = BuiltInModelCatalog::new();

        assert!(catalog
            .preferred_model_for_provider(&ProviderId::parse("qwen").unwrap())
            .is_none());
        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("dashscope").unwrap())
                .unwrap()
                .as_string(),
            "dashscope/qwen3.7-plus"
        );

        assert!(catalog
            .get(&ModelRef::parse("qwen/qwen3.7-plus").unwrap())
            .is_none());

        assert!(catalog
            .get(&ModelRef::parse("dashscope/qwen3.7-plus").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope/qwen3.7-max").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope/MiniMax-M2.5").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope/MiniMax/MiniMax-M3").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope/glm-5.1").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope/ZHIPU/GLM-5.2").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope/kimi-k2.6").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope/kimi-k2.7-code").unwrap())
            .is_some());
        assert!(catalog
            .get(&ModelRef::parse("dashscope-openai/qwen3.7-plus").unwrap())
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("dashscope-openai/MiniMax-M2.5").unwrap())
            .is_none());
    }

    #[test]
    fn dashscope_catalog_tracks_current_modalities_and_reasoning() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("qwen3.5-plus", 1_000_000, true, true),
            ("qwen3-coder-next", 262_144, true, false),
            ("qwen3-coder-plus", 1_000_000, true, false),
            ("glm-5", 202_752, true, false),
            ("glm-4.7", 202_752, true, false),
            ("kimi-k2.5", 262_144, true, true),
            ("kimi-k2.6", 262_144, true, true),
            ("MiniMax/MiniMax-M3", 196_608, true, false),
        ];

        for (model, context_window, reasoning, image_input) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("dashscope/{model}")).unwrap())
                .unwrap();
            assert_eq!(
                metadata.context_window_tokens,
                Some(context_window),
                "{model} context window"
            );
            assert_eq!(
                metadata.capabilities.supports_reasoning, reasoning,
                "{model} reasoning"
            );
            assert_eq!(
                metadata.capabilities.image_input, image_input,
                "{model} image input"
            );
        }
    }

    #[test]
    fn chutes_catalog_tracks_the_public_live_model_directory() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("moonshotai/Kimi-K2.6-TEE", 262_144, Some(65_535), true),
            ("zai-org/GLM-5.2-TEE", 1_048_576, Some(65_535), false),
            (
                "deepseek-ai/DeepSeek-V3.2-TEE",
                131_072,
                Some(65_536),
                false,
            ),
            ("Qwen/Qwen3.6-27B-TEE", 262_144, Some(65_536), true),
            ("google/gemma-4-31B-turbo-TEE", 131_072, Some(65_536), true),
            (
                "unsloth/Mistral-Nemo-Instruct-2407-TEE",
                131_072,
                None,
                false,
            ),
        ];

        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("chutes").unwrap())
                .unwrap()
                .as_string(),
            "chutes/moonshotai/Kimi-K2.6-TEE"
        );

        for (model, context_window, max_output, image_input) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("chutes/{model}")).unwrap())
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(metadata.context_window_tokens, Some(context_window));
            assert_eq!(metadata.max_output_tokens_upper_limit, max_output);
            assert_eq!(metadata.capabilities.image_input, image_input);
        }

        for retired in ["zai-org/GLM-4.7-TEE", "openai/gpt-oss-120b-TEE"] {
            assert!(
                catalog
                    .get(&ModelRef::parse(&format!("chutes/{retired}")).unwrap())
                    .is_none(),
                "{retired} should not remain in the built-in catalog"
            );
        }

        let mistral = catalog
            .get(&ModelRef::parse("chutes/unsloth/Mistral-Nemo-Instruct-2407-TEE").unwrap())
            .unwrap();
        assert_eq!(mistral.source, ModelMetadataSource::ConservativeBuiltin);
        assert!(!mistral.capabilities.supports_reasoning);
        assert!(mistral.max_output_tokens_upper_limit.is_none());
    }

    #[test]
    fn fireworks_catalog_tracks_current_serverless_chat_models() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            (
                "accounts/fireworks/models/deepseek-v4-flash",
                Some(1_048_576),
                false,
            ),
            (
                "accounts/fireworks/models/deepseek-v4-pro",
                Some(1_048_576),
                false,
            ),
            ("accounts/fireworks/models/glm-5p1", Some(202_752), false),
            ("accounts/fireworks/models/glm-5p2", Some(1_048_576), false),
            (
                "accounts/fireworks/models/gpt-oss-120b",
                Some(131_072),
                false,
            ),
            ("accounts/fireworks/models/kimi-k2p6", Some(262_144), true),
            (
                "accounts/fireworks/models/kimi-k2p7-code",
                Some(262_144),
                true,
            ),
            (
                "accounts/fireworks/models/minimax-m2p7",
                Some(196_608),
                false,
            ),
            ("accounts/fireworks/models/minimax-m3", Some(524_288), true),
            (
                "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
                Some(262_144),
                false,
            ),
            ("accounts/fireworks/models/qwen3p6-plus", None, true),
            (
                "accounts/fireworks/models/qwen3p7-plus",
                Some(262_144),
                true,
            ),
        ];

        for (model, context_window, image_input) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("fireworks/{model}")).unwrap())
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(metadata.context_window_tokens, context_window, "{model}");
            assert_eq!(metadata.capabilities.image_input, image_input, "{model}");
            assert!(metadata.capabilities.supports_reasoning, "{model}");
            assert!(metadata.max_output_tokens_upper_limit.is_none(), "{model}");
            assert_eq!(metadata.source, ModelMetadataSource::ConservativeBuiltin);
        }

        assert!(catalog
            .get(&ModelRef::parse("fireworks/accounts/fireworks/routers/kimi-k2p5-turbo").unwrap())
            .is_none());

        let deepseek = catalog.resolve_policy(
            &ModelRef::parse("fireworks/accounts/fireworks/models/deepseek-v4-flash").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(
            deepseek.reasoning_effort_options,
            ["none", "low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn nvidia_catalog_tracks_the_public_hosted_model_directory() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("nvidia/nemotron-3-super-120b-a12b", 1_000_000, false),
            ("moonshotai/kimi-k2.6", 262_144, true),
            ("minimaxai/minimax-m2.7", 204_800, false),
            ("minimaxai/minimax-m3", 1_000_000, true),
            ("z-ai/glm-5.2", 1_000_000, false),
        ];

        for (model, context_window, image_input) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("nvidia/{model}")).unwrap())
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(
                metadata.context_window_tokens,
                Some(context_window),
                "{model}"
            );
            assert_eq!(metadata.capabilities.image_input, image_input, "{model}");
            assert!(metadata.capabilities.supports_reasoning, "{model}");
            assert!(metadata.max_output_tokens_upper_limit.is_none(), "{model}");
            assert_eq!(metadata.source, ModelMetadataSource::ConservativeBuiltin);
        }

        for retired in [
            "moonshotai/kimi-k2.5",
            "minimaxai/minimax-m2.5",
            "z-ai/glm5",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::parse(&format!("nvidia/{retired}")).unwrap())
                    .is_none(),
                "{retired} should not remain in the built-in catalog"
            );
        }
    }

    #[test]
    fn together_catalog_tracks_current_serverless_chat_models() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("MiniMaxAI/MiniMax-M3", 524_288, false, true),
            ("MiniMaxAI/MiniMax-M2.7", 202_752, true, false),
            ("Qwen/Qwen3.5-9B", 262_144, true, true),
            ("moonshotai/Kimi-K2.7-Code", 262_144, false, true),
            ("moonshotai/Kimi-K2.6", 262_144, true, true),
            ("zai-org/GLM-5.2", 262_144, false, false),
            ("openai/gpt-oss-120b", 128_000, true, false),
            ("deepseek-ai/DeepSeek-V4-Pro", 512_000, true, false),
            ("nvidia/nemotron-3-ultra-550b-a55b", 512_300, true, false),
            (
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                131_072,
                false,
                false,
            ),
        ];

        for (model, context_window, supports_reasoning, image_input) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("together/{model}")).unwrap())
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(
                metadata.context_window_tokens,
                Some(context_window),
                "{model}"
            );
            assert_eq!(
                metadata.capabilities.supports_reasoning, supports_reasoning,
                "{model}"
            );
            assert_eq!(metadata.capabilities.image_input, image_input, "{model}");
            assert!(metadata.default_max_output_tokens.is_none(), "{model}");
            assert!(metadata.max_output_tokens_upper_limit.is_none(), "{model}");
            assert_eq!(metadata.source, ModelMetadataSource::ConservativeBuiltin);
            assert!(
                reasoning_effort_options(
                    &metadata.model_ref,
                    metadata.endpoint.as_ref(),
                    &metadata.capabilities,
                )
                .is_empty(),
                "{model} should not expose controls unsupported by the transport"
            );
        }

        for retired in [
            "zai-org/GLM-4.7",
            "moonshotai/Kimi-K2.5",
            "meta-llama/Llama-4-Scout-17B-16E-Instruct",
            "meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8",
            "deepseek-ai/DeepSeek-V3.1",
            "deepseek-ai/DeepSeek-R1",
            "moonshotai/Kimi-K2-Instruct-0905",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::parse(&format!("together/{retired}")).unwrap())
                    .is_none(),
                "{retired} should not remain in the built-in catalog"
            );
        }
    }

    #[test]
    fn openrouter_catalog_keeps_only_the_dynamic_auto_router_default() {
        let catalog = BuiltInModelCatalog::new();
        let auto = catalog
            .get(&ModelRef::parse("openrouter/auto").unwrap())
            .expect("OpenRouter auto router should be registered");

        assert_eq!(auto.context_window_tokens, Some(2_000_000));
        assert!(auto.default_max_output_tokens.is_none());
        assert!(auto.max_output_tokens_upper_limit.is_none());
        assert!(auto.capabilities.image_input);
        assert!(auto.capabilities.supports_reasoning);
        assert!(auto.reasoning_effort_options.is_empty());
        assert_eq!(auto.source, ModelMetadataSource::ConservativeBuiltin);

        for dynamic_or_removed in [
            "moonshotai/kimi-k2.6",
            "openrouter/healer-alpha",
            "openrouter/hunter-alpha",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::parse(&format!("openrouter/{dynamic_or_removed}")).unwrap())
                    .is_none(),
                "{dynamic_or_removed} should come from discovery or explicit configuration"
            );
        }
    }

    #[test]
    fn huggingface_catalog_keeps_the_current_official_quick_start_default() {
        let catalog = BuiltInModelCatalog::new();
        let model = catalog
            .get(&ModelRef::parse("huggingface/openai/gpt-oss-120b").unwrap())
            .expect("Hugging Face quick-start model should be registered");

        assert_eq!(model.context_window_tokens, Some(131_072));
        assert!(model.default_max_output_tokens.is_none());
        assert!(model.max_output_tokens_upper_limit.is_none());
        assert!(model.capabilities.supports_reasoning);
        assert!(!model.capabilities.image_input);
        assert_eq!(model.reasoning_effort_options, ["low", "medium", "high"]);
        assert_eq!(model.source, ModelMetadataSource::ConservativeBuiltin);
        assert!(catalog
            .get(&ModelRef::parse("huggingface/moonshotai/Kimi-K2-Instruct").unwrap())
            .is_none());
    }

    #[test]
    fn kilocode_catalog_tracks_the_current_auto_virtual_models() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("kilo-auto/frontier", 1_000_000, 128_000, true),
            ("kilo-auto/balanced", 1_000_000, 65_536, true),
            ("kilo-auto/efficient", 1_000_000, 65_536, true),
            ("kilo-auto/free", 256_000, 10_000, false),
        ];

        for (model, context, output, image_input) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("kilocode/{model}")).unwrap())
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(metadata.context_window_tokens, Some(context));
            assert!(metadata.default_max_output_tokens.is_none());
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(output));
            assert_eq!(metadata.capabilities.image_input, image_input);
            assert!(metadata.capabilities.supports_reasoning);
            assert!(metadata.reasoning_effort_options.is_empty());
            assert_eq!(metadata.source, ModelMetadataSource::ConservativeBuiltin);
        }

        assert!(catalog
            .get(&ModelRef::parse("kilocode/kilo/auto").unwrap())
            .is_none());
    }

    #[test]
    fn synthetic_catalog_tracks_current_always_on_models() {
        let catalog = BuiltInModelCatalog::new();
        let synthetic = ProviderId::parse("synthetic").unwrap();
        let models = catalog
            .list()
            .into_iter()
            .filter(|entry| entry.model_ref.provider == synthetic)
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 11);
        for model in &models {
            assert!(model.capabilities.supports_reasoning);
            assert_eq!(
                model.max_output_tokens_upper_limit,
                Some(65_536),
                "{:?}",
                model.model_ref
            );
            assert!(
                model.reasoning_effort_options.is_empty(),
                "{:?} should not expose undocumented effort controls",
                model.model_ref
            );
        }
        for vision in [
            "syn:large:vision",
            "syn:small:vision",
            "hf:moonshotai/Kimi-K2.7-Code",
            "hf:Qwen/Qwen3.6-27B",
            "hf:MiniMaxAI/MiniMax-M3",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::parse(&format!("synthetic/{vision}")).unwrap())
                    .unwrap()
                    .capabilities
                    .image_input,
                "{vision}"
            );
        }
        assert_eq!(
            catalog
                .preferred_model_for_provider(&synthetic)
                .unwrap()
                .as_string(),
            "synthetic/syn:large:text"
        );
        assert!(catalog
            .get(&ModelRef::parse("synthetic/hf:moonshotai/Kimi-K2.5").unwrap())
            .is_none());
    }

    #[test]
    fn vercel_ai_gateway_catalog_tracks_current_picker_defaults() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("anthropic/claude-opus-4.6", 1_000_000, 128_000),
            ("openai/gpt-5.4", 1_050_000, 128_000),
            ("openai/gpt-5.4-pro", 1_050_000, 128_000),
            ("moonshotai/kimi-k2.6", 262_000, 262_000),
        ];

        for (model, context_window, max_output) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("vercel-ai-gateway/{model}")).unwrap())
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(
                metadata.context_window_tokens,
                Some(context_window),
                "{model} context window"
            );
            assert_eq!(
                metadata.default_max_output_tokens,
                Some(max_output),
                "{model} default output"
            );
            assert_eq!(
                metadata.max_output_tokens_upper_limit,
                Some(max_output),
                "{model} output limit"
            );
            assert!(metadata.capabilities.image_input, "{model} image input");
            assert!(
                metadata.capabilities.supports_reasoning,
                "{model} reasoning"
            );
            assert!(
                metadata.reasoning_effort_options.is_empty(),
                "{model} should not expose an undocumented discrete effort vocabulary"
            );
        }
    }

    #[test]
    fn stepfun_catalog_tracks_current_chat_models_and_reasoning_controls() {
        let catalog = BuiltInModelCatalog::new();
        let expected = [
            ("step-3.7-flash", true, &["low", "medium", "high"][..]),
            ("step-3.5-flash-2603", false, &["low", "high"][..]),
            ("step-3.5-flash", false, &[][..]),
        ];

        for (model, image_input, reasoning_options) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("stepfun/{model}")).unwrap())
                .unwrap_or_else(|| panic!("{model} should be registered"));
            assert_eq!(
                metadata.context_window_tokens,
                Some(262_144),
                "{model} context window"
            );
            assert_eq!(
                metadata.capabilities.image_input, image_input,
                "{model} image input"
            );
            assert!(
                metadata.capabilities.supports_reasoning,
                "{model} reasoning"
            );
            assert_eq!(metadata.default_max_output_tokens, None);
            assert_eq!(metadata.max_output_tokens_upper_limit, None);
            assert_eq!(
                reasoning_effort_options(
                    &metadata.model_ref,
                    metadata.endpoint.as_ref(),
                    &metadata.capabilities,
                ),
                reasoning_options,
                "{model} reasoning controls"
            );
        }
    }

    #[test]
    fn stepfun_plan_shares_canonical_models_with_exact_routes() {
        let catalog = BuiltInModelCatalog::new();

        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("stepfun").unwrap())
                .unwrap()
                .as_string(),
            "stepfun/step-3.7-flash"
        );
        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("stepfun-plan").unwrap())
                .unwrap()
                .as_string(),
            "stepfun/step-3.7-flash"
        );

        for route_ref in [
            "stepfun@default/step-3.7-flash",
            "stepfun@default/step-3.5-flash-2603",
            "stepfun@default/step-3.5-flash",
            "stepfun@plan/step-3.7-flash",
            "stepfun@plan/step-3.5-flash-2603",
            "stepfun@plan/step-3.5-flash",
        ] {
            assert!(
                catalog
                    .get_route(&ModelRouteRef::parse(route_ref).unwrap())
                    .is_some(),
                "{route_ref} should be registered"
            );
        }

        assert!(catalog
            .get(&ModelRef::parse("stepfun-plan/step-3.7-flash").unwrap())
            .is_none());
        assert_eq!(
            catalog
                .canonicalize_model_ref(&ModelRef::parse("stepfun-plan/step-3.7-flash").unwrap())
                .as_string(),
            "stepfun/step-3.7-flash"
        );
    }

    #[test]
    fn minimax_catalog_tracks_current_anthropic_models_and_modalities() {
        let catalog = BuiltInModelCatalog::new();
        let minimax = ProviderId::parse("minimax").unwrap();
        let models = catalog
            .list()
            .into_iter()
            .filter(|entry| entry.model_ref.provider == minimax)
            .collect::<Vec<_>>();

        assert_eq!(
            models
                .iter()
                .map(|entry| entry.model_ref.model.as_str())
                .collect::<Vec<_>>(),
            vec![
                "MiniMax-M2",
                "MiniMax-M2.1",
                "MiniMax-M2.1-highspeed",
                "MiniMax-M2.5",
                "MiniMax-M2.5-highspeed",
                "MiniMax-M2.7",
                "MiniMax-M2.7-highspeed",
                "MiniMax-M3",
            ]
        );

        let m3 = catalog
            .get(&ModelRef::parse("minimax/MiniMax-M3").unwrap())
            .unwrap();
        assert_eq!(m3.context_window_tokens, Some(1_000_000));
        assert_eq!(m3.max_output_tokens_upper_limit, Some(32_768));
        assert!(m3.capabilities.supports_reasoning);
        assert!(m3.capabilities.image_input);

        for model in models
            .into_iter()
            .filter(|entry| entry.model_ref.model != "MiniMax-M3")
        {
            assert_eq!(model.context_window_tokens, Some(204_800));
            assert_eq!(model.max_output_tokens_upper_limit, Some(128_000));
            assert!(model.capabilities.supports_reasoning);
            assert!(!model.capabilities.image_input);
        }
    }

    #[test]
    fn qianfan_catalog_tracks_current_v2_models_limits_and_modalities() {
        let catalog = BuiltInModelCatalog::new();
        let qianfan = ProviderId::parse("qianfan").unwrap();
        let models = catalog
            .list()
            .into_iter()
            .filter(|entry| entry.model_ref.provider == qianfan)
            .collect::<Vec<_>>();

        assert_eq!(
            models
                .iter()
                .map(|entry| entry.model_ref.model.as_str())
                .collect::<Vec<_>>(),
            vec![
                "deepseek-v3.2",
                "deepseek-v3.2-think",
                "ernie-5.0",
                "ernie-5.0-thinking-preview",
                "ernie-5.1",
                "ernie-x1.1",
            ]
        );

        let expected = [
            ("deepseek-v3.2", 131_072, 32_768, false, false),
            ("deepseek-v3.2-think", 163_840, 65_536, true, false),
            ("ernie-5.0", 248_832, 65_536, false, true),
            ("ernie-5.0-thinking-preview", 248_832, 65_536, true, true),
            ("ernie-5.1", 248_832, 65_536, false, false),
            ("ernie-x1.1", 121_856, 65_536, true, false),
        ];
        for (model, context_window, max_output, reasoning, image_input) in expected {
            let metadata = catalog
                .get(&ModelRef::parse(&format!("qianfan/{model}")).unwrap())
                .unwrap();
            assert_eq!(metadata.context_window_tokens, Some(context_window));
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(max_output));
            assert_eq!(metadata.capabilities.supports_reasoning, reasoning);
            assert_eq!(metadata.capabilities.image_input, image_input);
        }
    }

    #[test]
    fn byteplus_catalog_does_not_infer_models_from_volcengine() {
        let catalog = BuiltInModelCatalog::new();
        let byteplus_providers = [
            ProviderId::parse("byteplus").unwrap(),
            ProviderId::parse("byteplus-coding").unwrap(),
        ];

        assert!(
            catalog
                .list()
                .into_iter()
                .all(|entry| !byteplus_providers.contains(&entry.model_ref.provider)),
            "BytePlus model IDs and metadata require independent BytePlus documentation"
        );
    }

    #[test]
    fn dashscope_plan_providers_share_canonical_models_with_exact_routes() {
        let catalog = BuiltInModelCatalog::new();

        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("dashscope-token-plan").unwrap())
                .unwrap()
                .as_string(),
            "dashscope/qwen3.7-max"
        );
        assert_eq!(
            catalog
                .preferred_model_for_provider(&ProviderId::parse("dashscope-coding-plan").unwrap())
                .unwrap()
                .as_string(),
            "dashscope/qwen3.7-plus"
        );

        for route_ref in [
            "dashscope@token-plan/qwen3.7-max",
            "dashscope@token-plan/kimi-k2.7-code",
            "dashscope@token-plan/glm-5.2",
            "dashscope@token-plan/MiniMax-M2.5",
            "dashscope@coding-plan/qwen3-coder-plus",
            "dashscope@coding-plan/glm-5",
            "dashscope@coding-plan/kimi-k2.5",
            "dashscope@coding-plan/MiniMax-M2.5",
        ] {
            assert!(
                catalog
                    .get_route(&ModelRouteRef::parse(route_ref).unwrap())
                    .is_some(),
                "{route_ref} should be registered"
            );
        }

        for unsupported in [
            "dashscope@token-plan/ZHIPU/GLM-5.2",
            "dashscope@token-plan/MiniMax/MiniMax-M3",
            "dashscope@coding-plan/glm-5.2",
            "dashscope@coding-plan/kimi-k2.7-code",
        ] {
            assert!(
                catalog
                    .get_route(&ModelRouteRef::parse(unsupported).unwrap())
                    .is_none(),
                "{unsupported} should not be inferred from another DashScope catalog"
            );
        }
    }

    #[test]
    fn resolves_unknown_model_from_explicit_fallback() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::openai(), "custom-model"),
            &HashMap::new(),
            &HashMap::new(),
            Some(&ModelRuntimeOverride {
                prompt_budget_estimated_tokens: Some(64_000),
                compaction_trigger_estimated_tokens: Some(48_000),
                compaction_keep_recent_estimated_tokens: Some(24_000),
                ..ModelRuntimeOverride::default()
            }),
            &base_context(),
            8192,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, 64_000);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 48_000);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 24_000);
        assert_eq!(policy.source, ModelMetadataSource::UnknownFallback);
    }

    #[test]
    fn resolves_unknown_model_with_larger_default_fallback_budget() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::openai(), "custom-model"),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );

        assert_eq!(policy.prompt_budget_estimated_tokens, 128_000);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 115_200);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 43_776);
        assert_eq!(policy.source, ModelMetadataSource::UnknownFallback);
    }

    #[test]
    fn model_override_capabilities_take_precedence_over_unknown_fallback() {
        let catalog = BuiltInModelCatalog::new();
        let model_ref = ModelRef::new(ProviderId::openai(), "custom-model");
        let mut overrides = HashMap::new();
        overrides.insert(
            model_ref.clone(),
            ModelRuntimeOverride {
                capabilities: Some(ModelCapabilityOverride {
                    image_input: Some(true),
                    ..ModelCapabilityOverride::default()
                }),
                ..ModelRuntimeOverride::default()
            },
        );

        let policy = catalog.resolve_policy(
            &model_ref,
            &overrides,
            &HashMap::new(),
            Some(&ModelRuntimeOverride {
                capabilities: Some(ModelCapabilityOverride {
                    image_input: Some(false),
                    ..ModelCapabilityOverride::default()
                }),
                ..ModelRuntimeOverride::default()
            }),
            &base_context(),
            8192,
        );

        assert!(policy.capabilities.image_input);
        assert_eq!(policy.source, ModelMetadataSource::UnknownFallback);
    }

    #[test]
    fn model_override_can_replace_known_context_budget_fields() {
        let catalog = BuiltInModelCatalog::new();
        let mut overrides = HashMap::new();
        overrides.insert(
            ModelRef::new(ProviderId::anthropic(), "claude-haiku-4-5"),
            ModelRuntimeOverride {
                prompt_budget_estimated_tokens: Some(32_000),
                runtime_max_output_tokens: Some(4_096),
                ..ModelRuntimeOverride::default()
            },
        );
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::anthropic(), "claude-haiku-4-5"),
            &overrides,
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, 32_000);
        assert_eq!(policy.runtime_max_output_tokens, 4_096);
        assert_eq!(policy.source, ModelMetadataSource::ConfigOverride);
    }

    #[test]
    fn clamps_runtime_max_output_tokens_to_catalog_upper_limit() {
        // Regression test for #1962: when the configured runtime max output
        // tokens exceeds the model's declared upper limit, the resolved policy
        // must clamp it so wire-level requests (e.g. Anthropic max_tokens)
        // never exceed what the provider accepts.
        let catalog = BuiltInModelCatalog::new();

        // bigmodel/glm-5.1 has a catalog upper limit of 131_072.
        let policy = catalog.resolve_policy(
            &ModelRef::parse("bigmodel/glm-5.1").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            200_000, // configured runtime max well above the catalog limit
        );
        assert_eq!(policy.runtime_max_output_tokens, 131_072);
        assert_eq!(policy.max_output_tokens_upper_limit, Some(131_072));

        // Override that exceeds the limit should also be clamped.
        let mut overrides = HashMap::new();
        overrides.insert(
            ModelRef::parse("bigmodel/glm-5.1").unwrap(),
            ModelRuntimeOverride {
                runtime_max_output_tokens: Some(200_000),
                ..ModelRuntimeOverride::default()
            },
        );
        let policy = catalog.resolve_policy(
            &ModelRef::parse("bigmodel/glm-5.1").unwrap(),
            &overrides,
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.runtime_max_output_tokens, 131_072);
    }

    #[test]
    fn volcengine_catalog_respects_anthropic_compatible_output_limits() {
        // Regression test for #2095: All Volcengine tiers now use OpenAI
        // Responses. The 128000 limit on volcengine-agent models is a
        // conservative model definition choice, not an API constraint.
        let catalog = BuiltInModelCatalog::new();

        for removed_model_ref in [
            "volcengine/glm-4-7-251222",
            "volcengine-coding/glm-4-7-251222",
            "volcengine-agent/glm-4-7-251222",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::parse(removed_model_ref).unwrap())
                    .is_none(),
                "{removed_model_ref} should not be registered"
            );
        }

        for route_ref in ["volcengine@plan/glm-5.2"] {
            let policy = catalog.resolve_route_policy(
                &ModelRouteRef::parse(route_ref).unwrap(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                200_000,
            );
            assert_eq!(policy.runtime_max_output_tokens, 128_000);
            assert_eq!(policy.max_output_tokens_upper_limit, Some(128_000));
        }
    }

    #[test]
    fn volcengine_reasoning_effort_options_follow_route_contract() {
        let catalog = BuiltInModelCatalog::new();

        for route_ref in [
            "volcengine@default/doubao-seed-2-0-pro-260215",
            "volcengine@coding/glm-5.2",
            "volcengine@plan/glm-5.2",
        ] {
            let policy = catalog.resolve_route_policy(
                &ModelRouteRef::parse(route_ref).unwrap(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert_eq!(
                policy.reasoning_effort_options,
                ["low", "medium", "high"],
                "{route_ref}"
            );
        }

        for route_ref in [
            "volcengine@coding/kimi-k2.6",
            "volcengine@plan/kimi-k2.7-code",
        ] {
            let policy = catalog.resolve_route_policy(
                &ModelRouteRef::parse(route_ref).unwrap(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert!(policy.capabilities.supports_reasoning, "{route_ref}");
            assert!(policy.reasoning_effort_options.is_empty(), "{route_ref}");
        }
    }

    #[test]
    fn volcengine_deepseek_v3_2_is_not_a_vision_model() {
        let catalog = BuiltInModelCatalog::new();

        for model_ref in [
            "volcengine/deepseek-v3-2-251201",
            "volcengine-coding/deepseek-v3-2-251201",
            "volcengine-agent/deepseek-v3-2-251201",
        ] {
            let policy = catalog.resolve_policy(
                &ModelRef::parse(model_ref).unwrap(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert!(!policy.capabilities.image_input, "{model_ref}");
        }
    }

    #[test]
    fn moonshot_catalog_tracks_current_models_and_retirements() {
        let catalog = BuiltInModelCatalog::new();
        let moonshot = ProviderId::parse("moonshot").unwrap();
        let models = catalog
            .list()
            .into_iter()
            .filter(|model| model.model_ref.provider == moonshot)
            .map(|model| model.model_ref.model.clone())
            .collect::<Vec<_>>();

        assert_eq!(
            models,
            [
                "kimi-k2.5",
                "kimi-k2.6",
                "kimi-k2.7-code",
                "kimi-k2.7-code-highspeed",
                "moonshot-v1-128k",
                "moonshot-v1-128k-vision-preview",
                "moonshot-v1-32k",
                "moonshot-v1-32k-vision-preview",
                "moonshot-v1-8k",
                "moonshot-v1-8k-vision-preview",
                "moonshot-v1-auto",
            ]
        );

        for model_ref in [
            "moonshot/kimi-k2.5",
            "moonshot/kimi-k2.6",
            "moonshot/kimi-k2.7-code",
            "moonshot/kimi-k2.7-code-highspeed",
        ] {
            let policy = catalog.resolve_policy(
                &ModelRef::parse(model_ref).unwrap(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert!(policy.capabilities.supports_reasoning, "{model_ref}");
            assert!(policy.capabilities.image_input, "{model_ref}");
            assert!(policy.reasoning_effort_options.is_empty(), "{model_ref}");
        }

        for model_ref in [
            "moonshot/kimi-k2-thinking",
            "moonshot/kimi-k2-thinking-turbo",
            "moonshot/kimi-k2-turbo",
        ] {
            assert!(
                catalog.get(&ModelRef::parse(model_ref).unwrap()).is_none(),
                "{model_ref} should not be registered"
            );
        }
    }

    #[test]
    fn mistral_catalog_tracks_current_models_and_retirements() {
        let catalog = BuiltInModelCatalog::new();
        let mistral = ProviderId::parse("mistral").unwrap();
        let models = catalog
            .list()
            .into_iter()
            .filter(|model| model.model_ref.provider == mistral)
            .map(|model| model.model_ref.model.clone())
            .collect::<Vec<_>>();

        assert_eq!(
            models,
            [
                "codestral-latest",
                "mistral-large-latest",
                "mistral-medium-latest",
                "mistral-small-latest",
            ]
        );

        for model_ref in [
            "mistral/mistral-large-latest",
            "mistral/mistral-medium-latest",
            "mistral/mistral-small-latest",
        ] {
            let policy = catalog.resolve_policy(
                &ModelRef::parse(model_ref).unwrap(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert!(policy.capabilities.image_input, "{model_ref}");
        }

        let medium = catalog.resolve_policy(
            &ModelRef::parse("mistral/mistral-medium-latest").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert!(medium.capabilities.supports_reasoning);
        assert!(medium.reasoning_effort_options.is_empty());

        for model_ref in [
            "mistral/devstral-medium-latest",
            "mistral/magistral-small",
            "mistral/mistral-medium-2508",
            "mistral/pixtral-large-latest",
        ] {
            assert!(
                catalog.get(&ModelRef::parse(model_ref).unwrap()).is_none(),
                "{model_ref} should not be registered"
            );
        }

        for (legacy_model, replacement) in [
            (
                "mistral/devstral-medium-latest",
                "mistral/mistral-medium-latest",
            ),
            ("mistral/magistral-small", "mistral/mistral-small-latest"),
            (
                "mistral/mistral-medium-2508",
                "mistral/mistral-medium-latest",
            ),
            (
                "mistral/pixtral-large-latest",
                "mistral/mistral-medium-latest",
            ),
        ] {
            assert_eq!(
                catalog
                    .canonicalize_model_ref(&ModelRef::parse(legacy_model).unwrap())
                    .as_string(),
                replacement
            );
        }
    }

    #[test]
    fn opencode_go_catalog_tracks_the_current_dual_transport_model_table() {
        let catalog = BuiltInModelCatalog::new();
        let provider = provider_id("opencode-go");
        let models = catalog
            .list()
            .into_iter()
            .filter(|model| model.model_ref.provider == provider)
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 14);
        for model in &models {
            assert!(model.capabilities.supports_reasoning);
            assert!(model.reasoning_effort_options.is_empty());
            assert_eq!(model.source, ModelMetadataSource::ConservativeBuiltin);
        }

        for model in [
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "glm-5.2",
            "glm-5.1",
            "kimi-k2.7-code",
            "kimi-k2.6",
            "mimo-v2.5-pro",
            "mimo-v2.5",
        ] {
            assert_eq!(
                catalog
                    .preferred_route_for_model(&ModelRef::new(provider.clone(), model))
                    .unwrap()
                    .endpoint,
                ProviderEndpointId::default_endpoint(),
                "{model}"
            );
        }
        for model in [
            "minimax-m3",
            "minimax-m2.7",
            "minimax-m2.5",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.6-plus",
        ] {
            assert_eq!(
                catalog
                    .preferred_route_for_model(&ModelRef::new(provider.clone(), model))
                    .unwrap()
                    .endpoint
                    .as_str(),
                "messages",
                "{model}"
            );
        }
    }

    #[test]
    fn tencent_tokenhub_catalog_tracks_current_language_and_image_understanding_models() {
        let catalog = BuiltInModelCatalog::new();
        let provider = provider_id("tencent-tokenhub");
        let models = catalog
            .list()
            .into_iter()
            .filter(|model| model.model_ref.provider == provider)
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 27);
        for model in &models {
            assert_eq!(model.source, ModelMetadataSource::ConservativeBuiltin);
            assert!(model.reasoning_effort_options.is_empty());
            assert_eq!(
                catalog
                    .preferred_route_for_model(&model.model_ref)
                    .unwrap()
                    .endpoint,
                ProviderEndpointId::default_endpoint()
            );
            assert!(catalog
                .get_route(&ModelRouteRef::new(
                    provider.clone(),
                    ProviderEndpointId::parse("messages").unwrap(),
                    model.model_ref.model.clone(),
                ))
                .is_some());
        }

        let hy3 = catalog
            .get(&ModelRef::new(provider.clone(), "hy3"))
            .unwrap();
        assert_eq!(hy3.context_window_tokens, Some(256_000));
        assert_eq!(hy3.max_output_tokens_upper_limit, Some(128_000));

        for vision in [
            "glm-5v-turbo",
            "youtu-vita",
            "hy-vision-2.0-instruct",
            "hunyuan-t1-vision-20250916",
        ] {
            assert!(
                catalog
                    .get(&ModelRef::new(provider.clone(), vision))
                    .unwrap()
                    .capabilities
                    .image_input
            );
        }
        for retired_or_unsupported in [
            "deepseek-v3.1-terminus",
            "deepseek-r1-0528",
            "deepseek-v3-0324",
            "HY-Image-V3.0",
            "hunyuan-turbos-vision-video-20250728",
            "kinfra-text-embedding-0.6b",
        ] {
            assert!(catalog
                .get(&ModelRef::new(provider.clone(), retired_or_unsupported))
                .is_none());
        }
    }

    #[test]
    fn venice_catalog_tracks_stable_trait_defaults() {
        let catalog = BuiltInModelCatalog::new();
        let provider = provider_id("venice");
        let models = catalog
            .list()
            .into_iter()
            .filter(|model| model.model_ref.provider == provider)
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 5);
        assert_eq!(
            models
                .iter()
                .map(|model| model.model_ref.model.as_str())
                .collect::<Vec<_>>(),
            [
                "zai-org-glm-4.7",
                "qwen3-235b-a22b-thinking-2507",
                "qwen3-coder-480b-a35b-instruct-turbo",
                "qwen3-vl-235b-a22b",
                "venice-uncensored-1-2",
            ]
        );
        assert!(models
            .iter()
            .all(|model| model.source == ModelMetadataSource::ConservativeBuiltin));

        let default = catalog
            .get(&ModelRef::new(provider.clone(), "zai-org-glm-4.7"))
            .unwrap();
        assert_eq!(default.context_window_tokens, Some(198_000));
        assert_eq!(default.max_output_tokens_upper_limit, Some(16_384));
        assert!(default.capabilities.supports_reasoning);
        assert!(!default.capabilities.image_input);
        assert_eq!(default.reasoning_effort_options, ["low", "medium", "high"]);

        let vision = catalog
            .get(&ModelRef::new(provider, "qwen3-vl-235b-a22b"))
            .unwrap();
        assert!(vision.capabilities.image_input);
        assert!(!vision.capabilities.supports_reasoning);
    }
}
