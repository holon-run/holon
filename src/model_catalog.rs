use std::collections::{BTreeMap, HashMap};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataOrigin {
    ExplicitOverride,
    RemoteDiscovered,
    RouteBuiltin,
    ModelBuiltin,
    UnknownFallback,
    RuntimeDefault,
    Derived,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataField {
    DisplayName,
    Description,
    ContextWindowTokens,
    EffectiveContextWindowPercent,
    AutoCompactTokenLimit,
    PromptBudgetEstimatedTokens,
    CompactionTriggerEstimatedTokens,
    CompactionKeepRecentEstimatedTokens,
    RuntimeMaxOutputTokens,
    Verbosity,
    ToolOutputTruncationEstimatedTokens,
    MaxOutputTokensUpperLimit,
    ParallelToolCalls,
    ImageInput,
    ImageGeneration,
    SupportsReasoning,
    InteractiveExec,
    ReasoningEffortOptions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataConstraintKind {
    Clamped,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataConstraintSource {
    ModelUpperLimit,
    TransportCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelMetadataConstraint {
    pub field: ModelMetadataField,
    pub kind: ModelMetadataConstraintKind,
    pub source: ModelMetadataConstraintSource,
    pub requested: String,
    pub effective: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelMetadataEvidence {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<ModelMetadataField, ModelMetadataOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ModelMetadataConstraint>,
}

impl ModelMetadataEvidence {
    pub fn source_for(&self, field: ModelMetadataField) -> Option<ModelMetadataOrigin> {
        self.fields.get(&field).copied()
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
    #[serde(default)]
    pub evidence: ModelMetadataEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningEffortValidationError {
    message: String,
}

impl ReasoningEffortValidationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
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
            evidence: ModelMetadataEvidence::default(),
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
struct ResolvedModelMetadata {
    policy: ResolvedRuntimeModelPolicy,
    catalog_entry: BuiltInModelMetadata,
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
        self.resolve_metadata(
            &model_ref,
            self.route_entries.get(route_ref),
            self.get(&model_ref),
            overrides,
            discovered_models,
            unknown_fallback,
            base_context_config,
            configured_runtime_max_output_tokens,
            Some(&route_ref.endpoint),
        )
        .policy
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
        self.resolve_metadata(
            model_ref,
            None,
            self.get(model_ref),
            overrides,
            discovered_models,
            unknown_fallback,
            base_context_config,
            configured_runtime_max_output_tokens,
            None,
        )
        .policy
    }

    pub(crate) fn resolve_catalog_entry(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
    ) -> BuiltInModelMetadata {
        self.resolve_metadata(
            model_ref,
            None,
            self.get(model_ref),
            overrides,
            discovered_models,
            unknown_fallback,
            base_context_config,
            configured_runtime_max_output_tokens,
            None,
        )
        .catalog_entry
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_metadata(
        &self,
        model_ref: &ModelRef,
        route_builtin: Option<&BuiltInModelMetadata>,
        model_builtin: Option<&BuiltInModelMetadata>,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
        endpoint: Option<&ProviderEndpointId>,
    ) -> ResolvedModelMetadata {
        let discovered = discovered_models.get(model_ref);
        let override_config = overrides.get(model_ref);
        let has_metadata =
            route_builtin.is_some() || model_builtin.is_some() || discovered.is_some();
        let fallback_override = if !has_metadata {
            unknown_fallback
        } else {
            None
        };
        let mut evidence = ModelMetadataEvidence::default();
        let source = if override_config.is_some() {
            ModelMetadataSource::ConfigOverride
        } else if fallback_override.is_some() {
            ModelMetadataSource::UnknownFallback
        } else if discovered.is_some() {
            ModelMetadataSource::RemoteDiscovered
        } else {
            route_builtin
                .or(model_builtin)
                .map(|entry| entry.source)
                .unwrap_or(ModelMetadataSource::UnknownFallback)
        };
        let display_name = select_field(
            &mut evidence,
            ModelMetadataField::DisplayName,
            [
                (
                    override_config.and_then(|value| value.display_name.clone()),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    discovered.map(|entry| entry.display_name.clone()),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    route_builtin.map(|entry| entry.display_name.clone()),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.map(|entry| entry.display_name.clone()),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    fallback_override.and_then(|value| value.display_name.clone()),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .unwrap_or_else(|| {
            record_origin(
                &mut evidence,
                ModelMetadataField::DisplayName,
                ModelMetadataOrigin::Derived,
            );
            model_ref.as_string()
        });
        let description = select_field(
            &mut evidence,
            ModelMetadataField::Description,
            [
                (
                    override_config.and_then(|value| value.description.clone()),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    discovered.map(|entry| entry.description.clone()),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    route_builtin.map(|entry| entry.description.clone()),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.map(|entry| entry.description.clone()),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    fallback_override.and_then(|value| value.description.clone()),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .unwrap_or_else(|| {
            record_origin(
                &mut evidence,
                ModelMetadataField::Description,
                ModelMetadataOrigin::Derived,
            );
            "Explicit unknown-model fallback policy".to_string()
        });
        let context_window_tokens = select_field(
            &mut evidence,
            ModelMetadataField::ContextWindowTokens,
            [
                (
                    override_config.and_then(|value| value.context_window_tokens),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    discovered.and_then(|entry| entry.context_window_tokens),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    route_builtin.and_then(|entry| entry.context_window_tokens),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.and_then(|entry| entry.context_window_tokens),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    fallback_override.and_then(|value| value.context_window_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        );
        if context_window_tokens.is_none() {
            record_origin(
                &mut evidence,
                ModelMetadataField::ContextWindowTokens,
                ModelMetadataOrigin::Derived,
            );
        }
        let effective_context_window_percent = validated_percent(
            select_field(
                &mut evidence,
                ModelMetadataField::EffectiveContextWindowPercent,
                [
                    (
                        override_config.and_then(|value| value.effective_context_window_percent),
                        ModelMetadataOrigin::ExplicitOverride,
                    ),
                    (
                        route_builtin.map(|entry| entry.effective_context_window_percent),
                        ModelMetadataOrigin::RouteBuiltin,
                    ),
                    (
                        model_builtin.map(|entry| entry.effective_context_window_percent),
                        ModelMetadataOrigin::ModelBuiltin,
                    ),
                    (
                        discovered.map(|entry| entry.effective_context_window_percent),
                        ModelMetadataOrigin::RemoteDiscovered,
                    ),
                    (
                        fallback_override.and_then(|value| value.effective_context_window_percent),
                        ModelMetadataOrigin::UnknownFallback,
                    ),
                ],
            )
            .unwrap_or_else(|| {
                record_origin(
                    &mut evidence,
                    ModelMetadataField::EffectiveContextWindowPercent,
                    ModelMetadataOrigin::Derived,
                );
                DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT
            }),
        );
        let prompt_budget_estimated_tokens = select_field(
            &mut evidence,
            ModelMetadataField::PromptBudgetEstimatedTokens,
            [
                (
                    override_config.and_then(|value| value.prompt_budget_estimated_tokens),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    fallback_override.and_then(|value| value.prompt_budget_estimated_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .unwrap_or_else(|| {
            if let Some(window) = context_window_tokens {
                record_origin(
                    &mut evidence,
                    ModelMetadataField::PromptBudgetEstimatedTokens,
                    ModelMetadataOrigin::Derived,
                );
                percent_of(window, usize::from(effective_context_window_percent))
            } else if has_metadata {
                record_origin(
                    &mut evidence,
                    ModelMetadataField::PromptBudgetEstimatedTokens,
                    ModelMetadataOrigin::RuntimeDefault,
                );
                base_context_config.prompt_budget_estimated_tokens
            } else {
                record_origin(
                    &mut evidence,
                    ModelMetadataField::PromptBudgetEstimatedTokens,
                    ModelMetadataOrigin::Derived,
                );
                DEFAULT_UNKNOWN_FALLBACK_PROMPT_BUDGET_ESTIMATED_TOKENS
            }
        });
        let auto_compact_token_limit = select_field(
            &mut evidence,
            ModelMetadataField::AutoCompactTokenLimit,
            [
                (
                    override_config.and_then(|value| value.auto_compact_token_limit),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.auto_compact_token_limit),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.and_then(|entry| entry.auto_compact_token_limit),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered.and_then(|entry| entry.auto_compact_token_limit),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    fallback_override.and_then(|value| value.auto_compact_token_limit),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        );
        if auto_compact_token_limit.is_none() {
            record_origin(
                &mut evidence,
                ModelMetadataField::AutoCompactTokenLimit,
                ModelMetadataOrigin::Derived,
            );
        }
        let compaction_trigger_estimated_tokens = select_field(
            &mut evidence,
            ModelMetadataField::CompactionTriggerEstimatedTokens,
            [
                (
                    override_config.and_then(|value| value.compaction_trigger_estimated_tokens),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    fallback_override.and_then(|value| value.compaction_trigger_estimated_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
                (auto_compact_token_limit, ModelMetadataOrigin::Derived),
            ],
        )
        .unwrap_or_else(|| {
            record_origin(
                &mut evidence,
                ModelMetadataField::CompactionTriggerEstimatedTokens,
                ModelMetadataOrigin::Derived,
            );
            percent_of(
                prompt_budget_estimated_tokens,
                usize::from(DEFAULT_COMPACTION_TRIGGER_PERCENT),
            )
        });
        let compaction_keep_recent_estimated_tokens = select_field(
            &mut evidence,
            ModelMetadataField::CompactionKeepRecentEstimatedTokens,
            [
                (
                    override_config.and_then(|value| value.compaction_keep_recent_estimated_tokens),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    fallback_override
                        .and_then(|value| value.compaction_keep_recent_estimated_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .unwrap_or_else(|| {
            record_origin(
                &mut evidence,
                ModelMetadataField::CompactionKeepRecentEstimatedTokens,
                ModelMetadataOrigin::Derived,
            );
            percent_of(
                compaction_trigger_estimated_tokens,
                usize::from(DEFAULT_KEEP_RECENT_PERCENT),
            )
        });
        let default_max_output_tokens = select_field(
            &mut evidence,
            ModelMetadataField::RuntimeMaxOutputTokens,
            [
                (
                    override_config.and_then(|value| value.runtime_max_output_tokens),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.default_max_output_tokens),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.and_then(|entry| entry.default_max_output_tokens),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered.and_then(|entry| entry.default_max_output_tokens),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    fallback_override.and_then(|value| value.runtime_max_output_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        );
        let requested_runtime_max_output_tokens = default_max_output_tokens.unwrap_or_else(|| {
            record_origin(
                &mut evidence,
                ModelMetadataField::RuntimeMaxOutputTokens,
                ModelMetadataOrigin::RuntimeDefault,
            );
            configured_runtime_max_output_tokens
        });
        let max_output_tokens_upper_limit = select_field(
            &mut evidence,
            ModelMetadataField::MaxOutputTokensUpperLimit,
            [
                (
                    route_builtin.and_then(|entry| entry.max_output_tokens_upper_limit),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    discovered.and_then(|entry| entry.max_output_tokens_upper_limit),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    model_builtin.and_then(|entry| entry.max_output_tokens_upper_limit),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
            ],
        );
        if max_output_tokens_upper_limit.is_none() {
            record_origin(
                &mut evidence,
                ModelMetadataField::MaxOutputTokensUpperLimit,
                ModelMetadataOrigin::Derived,
            );
        }
        let runtime_max_output_tokens = match max_output_tokens_upper_limit {
            Some(upper) if requested_runtime_max_output_tokens > upper => {
                evidence.constraints.push(ModelMetadataConstraint {
                    field: ModelMetadataField::RuntimeMaxOutputTokens,
                    kind: ModelMetadataConstraintKind::Clamped,
                    source: ModelMetadataConstraintSource::ModelUpperLimit,
                    requested: requested_runtime_max_output_tokens.to_string(),
                    effective: upper.to_string(),
                });
                upper
            }
            _ => requested_runtime_max_output_tokens,
        };
        let effective_default_max_output_tokens =
            default_max_output_tokens.map(|_| runtime_max_output_tokens);
        let verbosity = select_field(
            &mut evidence,
            ModelMetadataField::Verbosity,
            [
                (
                    override_config.and_then(|value| value.verbosity),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.default_verbosity),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.and_then(|entry| entry.default_verbosity),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered.and_then(|entry| entry.default_verbosity),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    fallback_override.and_then(|value| value.verbosity),
                    ModelMetadataOrigin::UnknownFallback,
                ),
                (
                    default_verbosity_for_model(model_ref),
                    ModelMetadataOrigin::Derived,
                ),
            ],
        );
        if verbosity.is_none() {
            record_origin(
                &mut evidence,
                ModelMetadataField::Verbosity,
                ModelMetadataOrigin::Derived,
            );
        }
        let configured_tool_output_truncation_estimated_tokens = select_field(
            &mut evidence,
            ModelMetadataField::ToolOutputTruncationEstimatedTokens,
            [
                (
                    override_config.and_then(|value| value.tool_output_truncation_estimated_tokens),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.tool_output_truncation_estimated_tokens),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.and_then(|entry| entry.tool_output_truncation_estimated_tokens),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered.and_then(|entry| entry.tool_output_truncation_estimated_tokens),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    fallback_override
                        .and_then(|value| value.tool_output_truncation_estimated_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        );
        let tool_output_truncation_estimated_tokens =
            configured_tool_output_truncation_estimated_tokens.unwrap_or_else(|| {
                record_origin(
                    &mut evidence,
                    ModelMetadataField::ToolOutputTruncationEstimatedTokens,
                    ModelMetadataOrigin::Derived,
                );
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS
            });
        let capabilities = ModelCapabilityFlags {
            parallel_tool_calls: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::ParallelToolCalls,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.parallel_tool_calls),
                discovered.map(|entry| entry.capabilities.parallel_tool_calls),
                route_builtin.map(|entry| entry.capabilities.parallel_tool_calls),
                model_builtin.map(|entry| entry.capabilities.parallel_tool_calls),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.parallel_tool_calls),
            ),
            image_input: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::ImageInput,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_input),
                discovered.map(|entry| entry.capabilities.image_input),
                route_builtin.map(|entry| entry.capabilities.image_input),
                model_builtin.map(|entry| entry.capabilities.image_input),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_input),
            ),
            image_generation: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::ImageGeneration,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_generation),
                discovered.map(|entry| entry.capabilities.image_generation),
                route_builtin.map(|entry| entry.capabilities.image_generation),
                model_builtin.map(|entry| entry.capabilities.image_generation),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_generation),
            ),
            supports_reasoning: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::SupportsReasoning,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.supports_reasoning),
                discovered.map(|entry| entry.capabilities.supports_reasoning),
                route_builtin.map(|entry| entry.capabilities.supports_reasoning),
                model_builtin.map(|entry| entry.capabilities.supports_reasoning),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.supports_reasoning),
            ),
            interactive_exec: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::InteractiveExec,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.interactive_exec),
                discovered.map(|entry| entry.capabilities.interactive_exec),
                route_builtin.map(|entry| entry.capabilities.interactive_exec),
                model_builtin.map(|entry| entry.capabilities.interactive_exec),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.interactive_exec),
            ),
        };
        let reasoning_effort_options = select_field(
            &mut evidence,
            ModelMetadataField::ReasoningEffortOptions,
            [
                (
                    route_builtin
                        .map(|entry| entry.reasoning_effort_options.clone())
                        .filter(|options| !options.is_empty()),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    discovered.and_then(|entry| {
                        (!entry.reasoning_effort_options.is_empty()
                            || entry.capabilities.supports_reasoning)
                            .then(|| entry.reasoning_effort_options.clone())
                    }),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    model_builtin
                        .map(|entry| entry.reasoning_effort_options.clone())
                        .filter(|options| !options.is_empty()),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
            ],
        )
        .unwrap_or_else(|| {
            let options = reasoning_effort_options(model_ref, endpoint, &capabilities);
            let origin = if !options.is_empty() && route_builtin.is_some() {
                ModelMetadataOrigin::RouteBuiltin
            } else if !options.is_empty() && model_builtin.is_some() {
                ModelMetadataOrigin::ModelBuiltin
            } else {
                ModelMetadataOrigin::Derived
            };
            record_origin(
                &mut evidence,
                ModelMetadataField::ReasoningEffortOptions,
                origin,
            );
            options
        });

        let policy = ResolvedRuntimeModelPolicy {
            model_ref: model_ref.clone(),
            display_name: display_name.clone(),
            description: description.clone(),
            context_window_tokens,
            effective_context_window_percent,
            prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens,
            runtime_max_output_tokens,
            verbosity,
            tool_output_truncation_estimated_tokens,
            max_output_tokens_upper_limit,
            capabilities: capabilities.clone(),
            reasoning_effort_options: reasoning_effort_options.clone(),
            source,
            evidence,
        };
        let catalog_entry = BuiltInModelMetadata {
            model_ref: model_ref.clone(),
            display_name,
            description,
            context_window_tokens,
            effective_context_window_percent,
            auto_compact_token_limit,
            default_max_output_tokens: effective_default_max_output_tokens,
            max_output_tokens_upper_limit,
            default_verbosity: verbosity,
            tool_output_truncation_estimated_tokens:
                configured_tool_output_truncation_estimated_tokens,
            capabilities,
            reasoning_effort_options,
            source,
            endpoint: endpoint.cloned(),
        };
        ResolvedModelMetadata {
            policy,
            catalog_entry,
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

fn record_origin(
    evidence: &mut ModelMetadataEvidence,
    field: ModelMetadataField,
    origin: ModelMetadataOrigin,
) {
    evidence.fields.entry(field).or_insert(origin);
}

fn select_field<T, const N: usize>(
    evidence: &mut ModelMetadataEvidence,
    field: ModelMetadataField,
    candidates: [(Option<T>, ModelMetadataOrigin); N],
) -> Option<T> {
    for (value, origin) in candidates {
        if value.is_some() {
            record_origin(evidence, field, origin);
            return value;
        }
    }
    None
}

fn resolve_capability_field(
    evidence: &mut ModelMetadataEvidence,
    field: ModelMetadataField,
    explicit_override: Option<bool>,
    discovered: Option<bool>,
    route_builtin: Option<bool>,
    model_builtin: Option<bool>,
    unknown_fallback: Option<bool>,
) -> bool {
    select_field(
        evidence,
        field,
        [
            (explicit_override, ModelMetadataOrigin::ExplicitOverride),
            (
                discovered.filter(|value| *value),
                ModelMetadataOrigin::RemoteDiscovered,
            ),
            (route_builtin, ModelMetadataOrigin::RouteBuiltin),
            (model_builtin, ModelMetadataOrigin::ModelBuiltin),
            (unknown_fallback, ModelMetadataOrigin::UnknownFallback),
        ],
    )
    .unwrap_or_else(|| {
        record_origin(evidence, field, ModelMetadataOrigin::Derived);
        false
    })
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

mod providers;
#[cfg(test)]
use providers::common::catalog_model;

pub(crate) use providers::is_tencent_tokenhub_model_id;

fn built_in_entries() -> Vec<BuiltInModelMetadata> {
    providers::built_in_entries()
}

fn is_turn_default_candidate(entry: &BuiltInModelMetadata) -> bool {
    entry.context_window_tokens.is_some()
        || entry.capabilities.parallel_tool_calls
        || entry.capabilities.image_input
        || entry.capabilities.supports_reasoning
        || entry.capabilities.interactive_exec
}

fn default_verbosity_for_model(model_ref: &ModelRef) -> Option<ModelVerbosity> {
    (model_ref.provider == ProviderId::openai_codex()).then_some(ModelVerbosity::Low)
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
    fn metadata_precedence_matrix_resolves_fields_independently_with_evidence() {
        let catalog = BuiltInModelCatalog::new();
        let model_ref = ModelRef::new(ProviderId::openai(), "matrix-model");
        let mut model_builtin = catalog_model(
            "openai",
            "matrix-model",
            "Model Builtin",
            100_000,
            6_000,
            true,
            false,
        );
        model_builtin.default_verbosity = Some(ModelVerbosity::High);
        let mut route_builtin = model_builtin.clone();
        route_builtin.display_name = "Route Builtin".into();
        route_builtin.context_window_tokens = Some(120_000);
        route_builtin.default_max_output_tokens = Some(4_000);
        route_builtin.max_output_tokens_upper_limit = Some(8_000);
        route_builtin.capabilities.image_input = true;
        route_builtin.reasoning_effort_options = vec!["route".into()];
        let mut discovered = model_builtin.clone();
        discovered.display_name = "Remote Discovered".into();
        discovered.description = "Remote description".into();
        discovered.context_window_tokens = Some(200_000);
        discovered.default_max_output_tokens = Some(16_000);
        discovered.max_output_tokens_upper_limit = Some(16_000);
        discovered.default_verbosity = Some(ModelVerbosity::Low);
        discovered.capabilities.image_input = false;
        discovered.reasoning_effort_options = vec!["remote".into()];
        discovered.source = ModelMetadataSource::RemoteDiscovered;
        let discovered_models = HashMap::from([(model_ref.clone(), discovered)]);
        let overrides = HashMap::from([(
            model_ref.clone(),
            ModelRuntimeOverride {
                display_name: Some("Explicit Override".into()),
                runtime_max_output_tokens: Some(12_000),
                verbosity: Some(ModelVerbosity::Medium),
                capabilities: Some(ModelCapabilityOverride {
                    image_input: Some(false),
                    ..ModelCapabilityOverride::default()
                }),
                ..ModelRuntimeOverride::default()
            },
        )]);

        let resolved = catalog.resolve_metadata(
            &model_ref,
            Some(&route_builtin),
            Some(&model_builtin),
            &overrides,
            &discovered_models,
            None,
            &base_context(),
            8_192,
            Some(&ProviderEndpointId::default_endpoint()),
        );
        assert_eq!(
            resolved.catalog_entry.default_max_output_tokens,
            Some(8_000)
        );
        let policy = resolved.policy;

        assert_eq!(policy.display_name, "Explicit Override");
        assert_eq!(policy.description, "Remote description");
        assert_eq!(policy.context_window_tokens, Some(200_000));
        assert_eq!(policy.runtime_max_output_tokens, 8_000);
        assert_eq!(policy.max_output_tokens_upper_limit, Some(8_000));
        assert_eq!(policy.verbosity, Some(ModelVerbosity::Medium));
        assert!(!policy.capabilities.image_input);
        assert_eq!(policy.reasoning_effort_options, ["route"]);
        assert_eq!(
            policy.evidence.source_for(ModelMetadataField::DisplayName),
            Some(ModelMetadataOrigin::ExplicitOverride)
        );
        assert_eq!(
            policy.evidence.source_for(ModelMetadataField::Description),
            Some(ModelMetadataOrigin::RemoteDiscovered)
        );
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::ContextWindowTokens),
            Some(ModelMetadataOrigin::RemoteDiscovered)
        );
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::MaxOutputTokensUpperLimit),
            Some(ModelMetadataOrigin::RouteBuiltin)
        );
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::ReasoningEffortOptions),
            Some(ModelMetadataOrigin::RouteBuiltin)
        );
        assert_eq!(
            policy.evidence.constraints,
            [ModelMetadataConstraint {
                field: ModelMetadataField::RuntimeMaxOutputTokens,
                kind: ModelMetadataConstraintKind::Clamped,
                source: ModelMetadataConstraintSource::ModelUpperLimit,
                requested: "12000".into(),
                effective: "8000".into(),
            }]
        );
    }

    #[test]
    fn capability_precedence_treats_discovered_false_as_unknown_but_preserves_explicit_false() {
        let catalog = BuiltInModelCatalog::new();
        let model_ref = ModelRef::new(ProviderId::openai(), "capability-matrix");
        let mut route_builtin = catalog_model(
            "openai",
            "capability-matrix",
            "Route",
            100_000,
            8_192,
            true,
            true,
        );
        route_builtin.capabilities.image_input = true;
        let mut discovered = route_builtin.clone();
        discovered.capabilities.image_input = false;
        discovered.source = ModelMetadataSource::RemoteDiscovered;
        let discovered_models = HashMap::from([(model_ref.clone(), discovered)]);

        let route_selected = catalog.resolve_metadata(
            &model_ref,
            Some(&route_builtin),
            None,
            &HashMap::new(),
            &discovered_models,
            None,
            &base_context(),
            8_192,
            Some(&ProviderEndpointId::default_endpoint()),
        );
        assert!(route_selected.policy.capabilities.image_input);
        assert_eq!(
            route_selected
                .policy
                .evidence
                .source_for(ModelMetadataField::ImageInput),
            Some(ModelMetadataOrigin::RouteBuiltin)
        );

        let overrides = HashMap::from([(
            model_ref.clone(),
            ModelRuntimeOverride {
                capabilities: Some(ModelCapabilityOverride {
                    image_input: Some(false),
                    ..ModelCapabilityOverride::default()
                }),
                ..ModelRuntimeOverride::default()
            },
        )]);
        let explicit_false = catalog.resolve_metadata(
            &model_ref,
            Some(&route_builtin),
            None,
            &overrides,
            &discovered_models,
            None,
            &base_context(),
            8_192,
            Some(&ProviderEndpointId::default_endpoint()),
        );
        assert!(!explicit_false.policy.capabilities.image_input);
        assert_eq!(
            explicit_false
                .policy
                .evidence
                .source_for(ModelMetadataField::ImageInput),
            Some(ModelMetadataOrigin::ExplicitOverride)
        );
    }

    #[test]
    fn discovered_fixed_reasoning_preserves_an_explicit_empty_effort_contract() {
        let catalog = BuiltInModelCatalog::new();
        let model_ref = ModelRef::new(ProviderId::openai(), "fixed-reasoning");
        let mut model_builtin = catalog_model(
            "openai",
            "fixed-reasoning",
            "Fixed Reasoning",
            100_000,
            8_192,
            true,
            false,
        );
        model_builtin.reasoning_effort_options = vec!["low".into(), "high".into()];
        let mut discovered = model_builtin.clone();
        discovered.reasoning_effort_options.clear();
        discovered.source = ModelMetadataSource::RemoteDiscovered;
        let discovered_models = HashMap::from([(model_ref.clone(), discovered)]);

        let policy = catalog.resolve_policy(
            &model_ref,
            &HashMap::new(),
            &discovered_models,
            None,
            &base_context(),
            8_192,
        );

        assert!(policy.reasoning_effort_options.is_empty());
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::ReasoningEffortOptions),
            Some(ModelMetadataOrigin::RemoteDiscovered)
        );
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
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::PromptBudgetEstimatedTokens),
            Some(ModelMetadataOrigin::UnknownFallback)
        );
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
        assert_eq!(policy.source, ModelMetadataSource::ConfigOverride);
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
        assert!(policy.evidence.constraints.is_empty());

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
        assert_eq!(
            policy.evidence.constraints,
            [ModelMetadataConstraint {
                field: ModelMetadataField::RuntimeMaxOutputTokens,
                kind: ModelMetadataConstraintKind::Clamped,
                source: ModelMetadataConstraintSource::ModelUpperLimit,
                requested: "200000".into(),
                effective: "131072".into(),
            }]
        );
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
