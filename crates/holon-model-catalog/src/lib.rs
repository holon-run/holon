//! Offline Holon model catalog, policy resolution, and models.dev validation.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use holon_domain::model_values::built_in_provider_endpoint_identity;
#[cfg(test)]
use holon_domain::model_values::provider_definitions;
use holon_domain::model_values::{
    default_compaction_keep_recent_estimated_tokens, default_compaction_trigger_estimated_tokens,
};
use holon_domain::model_values::{ModelRef, ModelRouteRef, ProviderEndpointId, ProviderId};
use holon_domain::ContextConfig;

mod metadata_selection;

#[cfg(test)]
mod metadata_selection_tests;
use metadata_selection::{
    ModelMetadataSelection, SelectedConstraint, SelectedEvidence, SelectedReasoningOptions,
};

use holon_domain::model_values::default_agent_turn_capability;
pub use holon_domain::model_values::{
    AnthropicCacheControlSource, BuiltInModelMetadata, DecisionProtocol, EndpointModelPolicy,
    ModelCapabilityFlags, ModelIntrinsicCapabilities, ModelMetadataConstraint,
    ModelMetadataConstraintKind, ModelMetadataConstraintSource, ModelMetadataEvidence,
    ModelMetadataField, ModelMetadataOrigin, ModelMetadataSource, ModelModality,
    ModelParameterSupport, ModelReasoningCapability, ModelVerbosity, ResolvedAnthropicCachePolicy,
    ResolvedRuntimeModelPolicy,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ModelRouteCacheCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ModelRouteRuntimeOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_capabilities: Option<ModelRouteCacheCapabilities>,
}

impl ModelRouteRuntimeOverride {
    pub fn anthropic_cache_control(&self) -> Option<bool> {
        self.cache_capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.cache_control)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelCapabilityOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_turn: Option<bool>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<bool>,
}

impl ModelCapabilityOverride {
    pub fn is_empty(&self) -> bool {
        self.agent_turn.is_none()
            && self.parallel_tool_calls.is_none()
            && self.image_input.is_none()
            && self.image_generation.is_none()
            && self.supports_reasoning.is_none()
            && self.interactive_exec.is_none()
            && self.decision.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct BuiltInModelRoutePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_capabilities: Option<ModelRouteCacheCapabilities>,
    display_name: Option<String>,
    description: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_route_override"
    )]
    context_window_tokens: Option<Option<usize>>,
    effective_context_window_percent: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_route_override"
    )]
    auto_compact_token_limit: Option<Option<usize>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_route_override"
    )]
    default_max_output_tokens: Option<Option<u32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_route_override"
    )]
    max_output_tokens_upper_limit: Option<Option<u32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_route_override"
    )]
    default_verbosity: Option<Option<ModelVerbosity>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_route_override"
    )]
    tool_output_truncation_estimated_tokens: Option<Option<usize>>,
    capabilities: ModelCapabilityOverride,
    /// Explicit Decision wire protocol declared by the route. Decision route
    /// selection must never infer the protocol from the provider, model id,
    /// endpoint, or transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision_protocol: Option<DecisionProtocol>,
    reasoning_effort_options: Option<Vec<String>>,
}

// Missing fields inherit; present null explicitly clears a nullable model field.
fn deserialize_route_override<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl BuiltInModelRoutePolicy {
    #[cfg(test)]
    fn from_legacy_route_entry(
        route_entry: &BuiltInModelMetadata,
        model_entry: &BuiltInModelMetadata,
    ) -> Self {
        Self {
            cache_capabilities: None,
            display_name: field_diff(
                route_entry.display_name.clone(),
                model_entry.display_name.clone(),
            ),
            description: field_diff(
                route_entry.description.clone(),
                model_entry.description.clone(),
            ),
            context_window_tokens: field_diff(
                route_entry.context_window_tokens,
                model_entry.context_window_tokens,
            ),
            effective_context_window_percent: field_diff(
                route_entry.effective_context_window_percent,
                model_entry.effective_context_window_percent,
            ),
            auto_compact_token_limit: field_diff(
                route_entry.auto_compact_token_limit,
                model_entry.auto_compact_token_limit,
            ),
            default_max_output_tokens: field_diff(
                route_entry.default_max_output_tokens,
                model_entry.default_max_output_tokens,
            ),
            max_output_tokens_upper_limit: field_diff(
                route_entry.max_output_tokens_upper_limit,
                model_entry.max_output_tokens_upper_limit,
            ),
            default_verbosity: field_diff(
                route_entry.default_verbosity,
                model_entry.default_verbosity,
            ),
            tool_output_truncation_estimated_tokens: field_diff(
                route_entry.tool_output_truncation_estimated_tokens,
                model_entry.tool_output_truncation_estimated_tokens,
            ),
            capabilities: ModelCapabilityOverride {
                agent_turn: field_diff(
                    route_entry.capabilities.agent_turn,
                    model_entry.capabilities.agent_turn,
                ),
                parallel_tool_calls: field_diff(
                    route_entry.capabilities.parallel_tool_calls,
                    model_entry.capabilities.parallel_tool_calls,
                ),
                image_input: field_diff(
                    route_entry.capabilities.image_input,
                    model_entry.capabilities.image_input,
                ),
                image_generation: field_diff(
                    route_entry.capabilities.image_generation,
                    model_entry.capabilities.image_generation,
                ),
                supports_reasoning: field_diff(
                    route_entry.capabilities.supports_reasoning,
                    model_entry.capabilities.supports_reasoning,
                ),
                interactive_exec: field_diff(
                    route_entry.capabilities.interactive_exec,
                    model_entry.capabilities.interactive_exec,
                ),
                decision: None,
            },
            decision_protocol: None,
            reasoning_effort_options: field_diff(
                route_entry.reasoning_effort_options.clone(),
                model_entry.reasoning_effort_options.clone(),
            ),
        }
    }

    fn validate_narrowing(&self, model: &BuiltInModelMetadata) -> Result<(), String> {
        for (name, route, intrinsic) in [
            (
                "agent_turn",
                self.capabilities.agent_turn,
                model.capabilities.agent_turn,
            ),
            (
                "parallel_tool_calls",
                self.capabilities.parallel_tool_calls,
                model.capabilities.parallel_tool_calls,
            ),
            (
                "image_input",
                self.capabilities.image_input,
                model.capabilities.image_input,
            ),
            (
                "image_generation",
                self.capabilities.image_generation,
                model.capabilities.image_generation,
            ),
            (
                "supports_reasoning",
                self.capabilities.supports_reasoning,
                model.capabilities.supports_reasoning,
            ),
            (
                "interactive_exec",
                self.capabilities.interactive_exec,
                model.capabilities.interactive_exec,
            ),
        ] {
            if route == Some(true) && !intrinsic {
                return Err(format!(
                    "route policy cannot enable intrinsic capability {name} for {}",
                    model.model_ref.as_string()
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
struct BuiltInModelRouteDefinition {
    legacy_provider: ProviderId,
    model_ref: ModelRef,
    endpoint: ProviderEndpointId,
    policy: BuiltInModelRoutePolicy,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_protocol: Option<DecisionProtocol>,
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
            && self.decision_protocol.is_none()
    }
}

pub use holon_domain::model_values::ReasoningEffortValidationError;
use holon_domain::model_values::{
    DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT, DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInModelCatalog {
    entries: HashMap<ModelRef, BuiltInModelMetadata>,
    preferred_models: HashMap<ProviderId, ModelRef>,
    route_entries: HashMap<ModelRouteRef, BuiltInModelRoutePolicy>,
    preferred_routes: HashMap<ProviderId, ModelRouteRef>,
    preferred_routes_by_model: HashMap<ModelRef, ModelRouteRef>,
    aliases: HashMap<ModelRef, ModelRef>,
}

impl BuiltInModelCatalog {
    pub fn new() -> Self {
        Self::shared().clone()
    }

    pub fn shared() -> &'static Self {
        // Owning catalogs clone this immutable process snapshot; list-summary
        // views borrow it without rebuilding the registry maps.
        static BUILT_IN_CATALOG: OnceLock<BuiltInModelCatalog> = OnceLock::new();
        BUILT_IN_CATALOG.get_or_init(|| {
            snapshot::built_in_catalog()
                .unwrap_or_else(|error| panic!("invalid built-in model registry snapshot: {error}"))
        })
    }

    #[cfg(test)]
    fn from_legacy_definitions() -> Self {
        let mut route_entries = HashMap::new();
        let mut entries = HashMap::new();
        let mut preferred_models = HashMap::new();
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
            let reviewed_endpoint = reviewed_default_endpoint(&legacy_model_ref.provider);
            if is_turn_default_candidate(&entry)
                && prefers_provider_route(
                    preferred_routes.get(&legacy_model_ref.provider),
                    &route_ref,
                    reviewed_endpoint,
                )
            {
                preferred_models.insert(
                    legacy_model_ref.provider.clone(),
                    canonical_model_ref.clone(),
                );
                preferred_routes.insert(legacy_model_ref.provider, route_ref.clone());
            }
            entry.model_ref = canonical_model_ref.clone();
            entry.endpoint = None;
            let model_entry = entries
                .entry(canonical_model_ref.clone())
                .or_insert_with(|| entry.clone());
            let route_policy =
                BuiltInModelRoutePolicy::from_legacy_route_entry(&entry, model_entry);
            route_entries
                .entry(route_ref.clone())
                .or_insert(route_policy);
            if prefers_model_route(
                preferred_routes_by_model.get(&canonical_model_ref),
                &route_ref,
                reviewed_endpoint,
            ) {
                preferred_routes_by_model.insert(canonical_model_ref.clone(), route_ref.clone());
            }
        }
        for definition in built_in_route_definitions() {
            let legacy_model_ref = ModelRef::new(
                definition.legacy_provider.clone(),
                definition.model_ref.model.clone(),
            );
            let model_entry = entries.get(&definition.model_ref).unwrap_or_else(|| {
                panic!(
                    "route model {} must be registered",
                    definition.model_ref.as_string()
                )
            });
            definition
                .policy
                .validate_narrowing(model_entry)
                .unwrap_or_else(|error| panic!("invalid built-in route policy: {error}"));
            let route_ref = ModelRouteRef::new(
                definition.model_ref.provider.clone(),
                definition.endpoint,
                definition.model_ref.model.clone(),
            );
            if legacy_model_ref != definition.model_ref {
                aliases.insert(legacy_model_ref, definition.model_ref.clone());
            }
            let reviewed_endpoint = reviewed_default_endpoint(&definition.legacy_provider);
            let is_candidate = is_turn_default_route_candidate(model_entry, &definition.policy);
            if is_candidate
                && prefers_provider_route(
                    preferred_routes.get(&definition.legacy_provider),
                    &route_ref,
                    reviewed_endpoint,
                )
            {
                preferred_models.insert(
                    definition.legacy_provider.clone(),
                    definition.model_ref.clone(),
                );
                preferred_routes.insert(definition.legacy_provider.clone(), route_ref.clone());
            }
            // The promoted protocol of an internal sub-endpoint also becomes
            // the parent provider's default route, because the parent owns the
            // canonical model ref.
            if let Some(parent) = promoted_parent(&definition.legacy_provider) {
                let parent_endpoint = reviewed_default_endpoint(&parent);
                if is_candidate
                    && parent_endpoint == Some(route_ref.endpoint.as_str())
                    && prefers_provider_route(
                        preferred_routes.get(&parent),
                        &route_ref,
                        parent_endpoint,
                    )
                {
                    preferred_models.insert(parent.clone(), definition.model_ref.clone());
                    preferred_routes.insert(parent, route_ref.clone());
                }
            }
            // A model entry always registers its own route with an empty policy
            // diff, so an explicit definition may refine that placeholder route
            // while any other duplicate route stays a hard error.
            let replaced = route_entries.insert(route_ref.clone(), definition.policy);
            assert!(
                replaced.is_none_or(|policy| policy == BuiltInModelRoutePolicy::default()),
                "duplicate built-in route {}",
                route_ref.as_string()
            );
            if prefers_definition_model_route(
                preferred_routes_by_model.get(&definition.model_ref),
                &route_ref,
                reviewed_model_endpoint(&definition.legacy_provider),
            ) {
                preferred_routes_by_model.insert(definition.model_ref, route_ref);
            }
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
        self.canonical_model_ref(model_ref).clone()
    }

    pub fn canonical_model_ref<'a>(&'a self, model_ref: &'a ModelRef) -> &'a ModelRef {
        self.aliases.get(model_ref).unwrap_or(model_ref)
    }

    /// Legacy model ref → canonical model ref aliases for backward compatibility.
    #[cfg(test)]
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
        let route = self.preferred_routes.get(provider)?.clone();
        if provider.as_str() == "opencode-go" {
            return self.preferred_route_for_model(&route.model_ref());
        }
        Some(route)
    }

    pub fn preferred_route_for_model(&self, model_ref: &ModelRef) -> Option<ModelRouteRef> {
        // Standard Go references use one protocol; native route metadata remains
        // evidence for explicit routes, not an automatic protocol selector.
        if model_ref.provider.as_str() == "opencode-go" {
            return Some(ModelRouteRef::new(
                model_ref.provider.clone(),
                ProviderEndpointId::parse("messages").expect("valid Go endpoint"),
                model_ref.model.clone(),
            ));
        }
        self.preferred_routes_by_model.get(model_ref).cloned()
    }

    pub fn get_route(&self, route_ref: &ModelRouteRef) -> Option<&BuiltInModelRoutePolicy> {
        self.route_entries.get(route_ref)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn insert_route_policy(
        &mut self,
        route_ref: ModelRouteRef,
        policy: BuiltInModelRoutePolicy,
    ) {
        self.route_entries.insert(route_ref, policy);
    }

    pub fn resolve_route_policy(
        &self,
        route_ref: &ModelRouteRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRouteRef, BuiltInModelMetadata>,
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
        .into_policy()
    }

    pub fn try_resolve_route_policy(
        &self,
        route_ref: &ModelRouteRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRouteRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
    ) -> anyhow::Result<ResolvedRuntimeModelPolicy> {
        holon_system::resource_material::charge_owned_metadata(
            (std::mem::size_of::<ModelRef>()
                + route_ref.provider.as_str().len()
                + route_ref.model.len()) as u64,
            2,
        )?;
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
        .try_into_policy()
    }

    pub fn resolve_policy(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRouteRef, BuiltInModelMetadata>,
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
        .into_policy()
    }

    pub fn resolve_catalog_entry(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRouteRef, BuiltInModelMetadata>,
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
        .into_catalog_entry()
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_metadata<'a>(
        &self,
        model_ref: &'a ModelRef,
        route_builtin: Option<&'a BuiltInModelRoutePolicy>,
        model_builtin: Option<&'a BuiltInModelMetadata>,
        overrides: &'a HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &'a HashMap<ModelRouteRef, BuiltInModelMetadata>,
        unknown_fallback: Option<&'a ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
        endpoint: Option<&'a ProviderEndpointId>,
    ) -> ModelMetadataSelection<'a> {
        // These reviewed model facts apply only to the documented endpoint.
        // A missing route policy must not turn them into a sibling-endpoint fallback.
        let reviewed_endpoint = match (model_ref.provider.as_str(), model_ref.model.as_str()) {
            ("opencode-go", "longcat-2.5") => Some("default"),
            ("volcengine", "glm-5.4") => Some("plan"),
            _ => None,
        };
        let model_builtin = model_builtin.filter(|_| {
            route_builtin.is_some()
                || !reviewed_endpoint
                    .zip(endpoint)
                    .is_some_and(|(reviewed, requested)| reviewed != requested.as_str())
        });
        // Discovery is endpoint-scoped: the same model id advertised by two
        // endpoints of one provider is two distinct entries, so selection is by
        // the full route identity rather than by provider and model alone.
        let discovered = select_discovered_metadata(discovered_models, model_ref, endpoint);
        let override_config = overrides.get(model_ref);
        let has_metadata =
            route_builtin.is_some() || model_builtin.is_some() || discovered.is_some();
        let fallback_override = if !has_metadata {
            unknown_fallback
        } else {
            None
        };
        let mut evidence = SelectedEvidence::default();
        let source = if override_config.is_some() {
            ModelMetadataSource::ConfigOverride
        } else if fallback_override.is_some() {
            ModelMetadataSource::UnknownFallback
        } else if discovered.is_some() {
            ModelMetadataSource::RemoteDiscovered
        } else {
            model_builtin
                .map(|entry| entry.source)
                .unwrap_or(ModelMetadataSource::UnknownFallback)
        };
        let display_name = select_field(
            &mut evidence,
            ModelMetadataField::DisplayName,
            [
                (
                    override_config.and_then(|value| value.display_name.as_ref()),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    discovered.map(|entry| &entry.display_name),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    route_builtin.and_then(|entry| entry.display_name.as_ref()),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.map(|entry| &entry.display_name),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    fallback_override.and_then(|value| value.display_name.as_ref()),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        );
        if display_name.is_none() {
            record_origin(
                &mut evidence,
                ModelMetadataField::DisplayName,
                ModelMetadataOrigin::Derived,
            );
        }
        let description = select_field(
            &mut evidence,
            ModelMetadataField::Description,
            [
                (
                    override_config.and_then(|value| value.description.as_deref()),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    discovered.map(|entry| entry.description.as_str()),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    route_builtin.and_then(|entry| entry.description.as_deref()),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin.map(|entry| entry.description.as_str()),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    fallback_override.and_then(|value| value.description.as_deref()),
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
            "Explicit unknown-model fallback policy"
        });
        let context_window_tokens = select_field(
            &mut evidence,
            ModelMetadataField::ContextWindowTokens,
            [
                (
                    override_config
                        .and_then(|value| value.context_window_tokens)
                        .map(Some),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    discovered
                        .and_then(|entry| entry.context_window_tokens)
                        .map(Some),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    route_builtin.and_then(|entry| entry.context_window_tokens),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin
                        .and_then(|entry| entry.context_window_tokens)
                        .map(Some),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    unknown_fallback
                        .and_then(|value| value.context_window_tokens)
                        .map(Some),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .flatten();
        if context_window_tokens.is_none() {
            record_origin(
                &mut evidence,
                ModelMetadataField::ContextWindowTokens,
                ModelMetadataOrigin::Derived,
            );
        }
        // A name or capability is not a context limit; budget fallback is field-based.
        let budget_fallback_override = if context_window_tokens.is_none()
            || !has_metadata
            || evidence.source_for(ModelMetadataField::ContextWindowTokens)
                == Some(ModelMetadataOrigin::UnknownFallback)
        {
            unknown_fallback
        } else {
            None
        };
        let environment_budget = base_context_config.budget_overrides;
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
                        route_builtin.and_then(|entry| entry.effective_context_window_percent),
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
                        budget_fallback_override
                            .and_then(|value| value.effective_context_window_percent),
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
                    budget_fallback_override.and_then(|value| value.prompt_budget_estimated_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
                (
                    environment_budget.prompt_budget_estimated_tokens,
                    ModelMetadataOrigin::EnvironmentOverride,
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
            } else {
                record_origin(
                    &mut evidence,
                    ModelMetadataField::PromptBudgetEstimatedTokens,
                    ModelMetadataOrigin::RuntimeDefault,
                );
                base_context_config.prompt_budget_estimated_tokens
            }
        });
        let prompt_budget_estimated_tokens = match context_window_tokens {
            Some(window) => constrain_budget(
                &mut evidence,
                ModelMetadataField::PromptBudgetEstimatedTokens,
                prompt_budget_estimated_tokens,
                window,
                ModelMetadataConstraintSource::ModelUpperLimit,
            ),
            None => prompt_budget_estimated_tokens,
        };
        let auto_compact_token_limit = select_field(
            &mut evidence,
            ModelMetadataField::AutoCompactTokenLimit,
            [
                (
                    override_config
                        .and_then(|value| value.auto_compact_token_limit)
                        .map(Some),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.auto_compact_token_limit),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin
                        .and_then(|entry| entry.auto_compact_token_limit)
                        .map(Some),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered
                        .and_then(|entry| entry.auto_compact_token_limit)
                        .map(Some),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    budget_fallback_override
                        .and_then(|value| value.auto_compact_token_limit)
                        .map(Some),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .flatten();
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
                    budget_fallback_override
                        .and_then(|value| value.compaction_trigger_estimated_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
                (
                    environment_budget.compaction_trigger_estimated_tokens,
                    ModelMetadataOrigin::EnvironmentOverride,
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
            default_compaction_trigger_estimated_tokens(prompt_budget_estimated_tokens)
        });
        let compaction_trigger_estimated_tokens = constrain_budget(
            &mut evidence,
            ModelMetadataField::CompactionTriggerEstimatedTokens,
            compaction_trigger_estimated_tokens,
            prompt_budget_estimated_tokens,
            ModelMetadataConstraintSource::RuntimeBudget,
        );
        let compaction_keep_recent_estimated_tokens = select_field(
            &mut evidence,
            ModelMetadataField::CompactionKeepRecentEstimatedTokens,
            [
                (
                    override_config.and_then(|value| value.compaction_keep_recent_estimated_tokens),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    budget_fallback_override
                        .and_then(|value| value.compaction_keep_recent_estimated_tokens),
                    ModelMetadataOrigin::UnknownFallback,
                ),
                (
                    environment_budget.compaction_keep_recent_estimated_tokens,
                    ModelMetadataOrigin::EnvironmentOverride,
                ),
            ],
        )
        .unwrap_or_else(|| {
            record_origin(
                &mut evidence,
                ModelMetadataField::CompactionKeepRecentEstimatedTokens,
                ModelMetadataOrigin::Derived,
            );
            default_compaction_keep_recent_estimated_tokens(compaction_trigger_estimated_tokens)
        });
        let compaction_keep_recent_estimated_tokens = constrain_budget(
            &mut evidence,
            ModelMetadataField::CompactionKeepRecentEstimatedTokens,
            compaction_keep_recent_estimated_tokens,
            compaction_trigger_estimated_tokens,
            ModelMetadataConstraintSource::RuntimeBudget,
        );
        let default_max_output_tokens = select_field(
            &mut evidence,
            ModelMetadataField::RuntimeMaxOutputTokens,
            [
                (
                    override_config
                        .and_then(|value| value.runtime_max_output_tokens)
                        .map(Some),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.default_max_output_tokens),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin
                        .and_then(|entry| entry.default_max_output_tokens)
                        .map(Some),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered
                        .and_then(|entry| entry.default_max_output_tokens)
                        .map(Some),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    budget_fallback_override
                        .and_then(|value| value.runtime_max_output_tokens)
                        .map(Some),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .flatten();
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
                    discovered
                        .and_then(|entry| entry.max_output_tokens_upper_limit)
                        .map(Some),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    model_builtin
                        .and_then(|entry| entry.max_output_tokens_upper_limit)
                        .map(Some),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
            ],
        )
        .flatten();
        if max_output_tokens_upper_limit.is_none() {
            record_origin(
                &mut evidence,
                ModelMetadataField::MaxOutputTokensUpperLimit,
                ModelMetadataOrigin::Derived,
            );
        }
        let runtime_max_output_tokens = match max_output_tokens_upper_limit {
            Some(upper) if requested_runtime_max_output_tokens > upper => {
                evidence.constrain(SelectedConstraint {
                    field: ModelMetadataField::RuntimeMaxOutputTokens,
                    source: ModelMetadataConstraintSource::ModelUpperLimit,
                    clamp: Some((requested_runtime_max_output_tokens as usize, upper as usize)),
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
                    override_config.and_then(|value| value.verbosity).map(Some),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.default_verbosity),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin
                        .and_then(|entry| entry.default_verbosity)
                        .map(Some),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered
                        .and_then(|entry| entry.default_verbosity)
                        .map(Some),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    fallback_override
                        .and_then(|value| value.verbosity)
                        .map(Some),
                    ModelMetadataOrigin::UnknownFallback,
                ),
                (
                    default_verbosity_for_model(model_ref).map(Some),
                    ModelMetadataOrigin::Derived,
                ),
            ],
        )
        .flatten();
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
                    override_config
                        .and_then(|value| value.tool_output_truncation_estimated_tokens)
                        .map(Some),
                    ModelMetadataOrigin::ExplicitOverride,
                ),
                (
                    route_builtin.and_then(|entry| entry.tool_output_truncation_estimated_tokens),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    model_builtin
                        .and_then(|entry| entry.tool_output_truncation_estimated_tokens)
                        .map(Some),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
                (
                    discovered
                        .and_then(|entry| entry.tool_output_truncation_estimated_tokens)
                        .map(Some),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    fallback_override
                        .and_then(|value| value.tool_output_truncation_estimated_tokens)
                        .map(Some),
                    ModelMetadataOrigin::UnknownFallback,
                ),
            ],
        )
        .flatten();
        let tool_output_truncation_estimated_tokens =
            configured_tool_output_truncation_estimated_tokens.unwrap_or_else(|| {
                record_origin(
                    &mut evidence,
                    ModelMetadataField::ToolOutputTruncationEstimatedTokens,
                    ModelMetadataOrigin::Derived,
                );
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS
            });
        let mut capabilities = ModelCapabilityFlags {
            agent_turn: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::AgentTurn,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.agent_turn),
                discovered.map(|entry| entry.capabilities.agent_turn),
                model_builtin.map(|entry| entry.capabilities.agent_turn),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.agent_turn),
                default_agent_turn_capability(),
            ),
            parallel_tool_calls: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::ParallelToolCalls,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.parallel_tool_calls),
                discovered.map(|entry| entry.capabilities.parallel_tool_calls),
                model_builtin.map(|entry| entry.capabilities.parallel_tool_calls),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.parallel_tool_calls),
                false,
            ),
            image_input: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::ImageInput,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_input),
                discovered.map(|entry| entry.capabilities.image_input),
                model_builtin.map(|entry| entry.capabilities.image_input),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_input),
                false,
            ),
            image_generation: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::ImageGeneration,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_generation),
                discovered.map(|entry| entry.capabilities.image_generation),
                model_builtin.map(|entry| entry.capabilities.image_generation),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.image_generation),
                false,
            ),
            supports_reasoning: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::SupportsReasoning,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.supports_reasoning),
                discovered.map(|entry| entry.capabilities.supports_reasoning),
                model_builtin.map(|entry| entry.capabilities.supports_reasoning),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.supports_reasoning),
                false,
            ),
            interactive_exec: resolve_capability_field(
                &mut evidence,
                ModelMetadataField::InteractiveExec,
                override_config
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.interactive_exec),
                discovered.map(|entry| entry.capabilities.interactive_exec),
                model_builtin.map(|entry| entry.capabilities.interactive_exec),
                fallback_override
                    .and_then(|value| value.capabilities.as_ref())
                    .and_then(|value| value.interactive_exec),
                false,
            ),
        };
        if let Some(route) = route_builtin {
            apply_route_capability_constraint(
                &mut capabilities.agent_turn,
                route.capabilities.agent_turn,
                ModelMetadataField::AgentTurn,
                &mut evidence,
            );
            apply_route_capability_constraint(
                &mut capabilities.parallel_tool_calls,
                route.capabilities.parallel_tool_calls,
                ModelMetadataField::ParallelToolCalls,
                &mut evidence,
            );
            apply_route_capability_constraint(
                &mut capabilities.image_input,
                route.capabilities.image_input,
                ModelMetadataField::ImageInput,
                &mut evidence,
            );
            apply_route_capability_constraint(
                &mut capabilities.image_generation,
                route.capabilities.image_generation,
                ModelMetadataField::ImageGeneration,
                &mut evidence,
            );
            apply_route_capability_constraint(
                &mut capabilities.supports_reasoning,
                route.capabilities.supports_reasoning,
                ModelMetadataField::SupportsReasoning,
                &mut evidence,
            );
            apply_route_capability_constraint(
                &mut capabilities.interactive_exec,
                route.capabilities.interactive_exec,
                ModelMetadataField::InteractiveExec,
                &mut evidence,
            );
        }
        let decision_declared = override_config
            .and_then(|value| value.capabilities.as_ref())
            .and_then(|value| value.decision)
            .or_else(|| route_builtin.and_then(|route| route.capabilities.decision))
            .unwrap_or(false);
        let decision_protocol = override_config
            .and_then(|value| value.decision_protocol)
            .or_else(|| route_builtin.and_then(|route| route.decision_protocol));
        // Decision requires an explicit capability opt-in *and* an explicit
        // protocol; an unknown protocol must never fall back to a guess.
        let decision_capable = decision_declared && decision_protocol.is_some();
        let reasoning_effort_options = select_field(
            &mut evidence,
            ModelMetadataField::ReasoningEffortOptions,
            [
                (
                    route_builtin.and_then(|entry| entry.reasoning_effort_options.as_deref()),
                    ModelMetadataOrigin::RouteBuiltin,
                ),
                (
                    discovered.and_then(|entry| {
                        (!entry.reasoning_effort_options.is_empty()
                            || entry.capabilities.supports_reasoning)
                            .then_some(entry.reasoning_effort_options.as_slice())
                    }),
                    ModelMetadataOrigin::RemoteDiscovered,
                ),
                (
                    model_builtin
                        .map(|entry| entry.reasoning_effort_options.as_slice())
                        .filter(|options| !options.is_empty()),
                    ModelMetadataOrigin::ModelBuiltin,
                ),
            ],
        )
        .map(SelectedReasoningOptions::Configured)
        .unwrap_or_else(|| {
            let options = borrowed_reasoning_effort_options(model_ref, endpoint, &capabilities);
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
            SelectedReasoningOptions::Derived(options)
        });

        ModelMetadataSelection {
            model_ref,
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
            decision_capable,
            decision_protocol,
            reasoning_effort_options,
            source,
            evidence,
            auto_compact_token_limit,
            effective_default_max_output_tokens,
            configured_tool_output_truncation_estimated_tokens,
            endpoint,
        }
    }

    pub fn apply_policy(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        discovered_models: &HashMap<ModelRouteRef, BuiltInModelMetadata>,
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
            budget_overrides: base_context_config.budget_overrides,
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

/// The compiled-in built-in catalog without the models.dev supplemental
/// entries. This is the drafting baseline for
/// `models_dev::supplement::generate` and the collision-check base for
/// `holon models-dev validate`.
pub fn legacy_builtin_catalog() -> Result<BuiltInModelCatalog, String> {
    snapshot::legacy_catalog()
}

impl Default for BuiltInModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}

fn record_origin(
    evidence: &mut SelectedEvidence,
    field: ModelMetadataField,
    origin: ModelMetadataOrigin,
) {
    evidence.record(field, origin);
}

fn select_field<T, const N: usize>(
    evidence: &mut SelectedEvidence,
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

#[cfg(test)]
fn field_diff<T: PartialEq>(route: T, model: T) -> Option<T> {
    (route != model).then_some(route)
}

fn resolve_capability_field(
    evidence: &mut SelectedEvidence,
    field: ModelMetadataField,
    explicit_override: Option<bool>,
    discovered: Option<bool>,
    model_builtin: Option<bool>,
    unknown_fallback: Option<bool>,
    default: bool,
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
            (model_builtin, ModelMetadataOrigin::ModelBuiltin),
            (unknown_fallback, ModelMetadataOrigin::UnknownFallback),
        ],
    )
    .unwrap_or_else(|| {
        record_origin(evidence, field, ModelMetadataOrigin::Derived);
        default
    })
}

fn apply_route_capability_constraint(
    effective: &mut bool,
    route_value: Option<bool>,
    field: ModelMetadataField,
    evidence: &mut SelectedEvidence,
) {
    if route_value == Some(false) && *effective {
        evidence.constrain(SelectedConstraint {
            field,
            source: ModelMetadataConstraintSource::EndpointPolicy,
            clamp: None,
        });
        *effective = false;
    }
}

fn validated_percent(percent: u8) -> u8 {
    percent.clamp(1, 100)
}

fn percent_of(total: usize, percent: usize) -> usize {
    total.saturating_mul(percent) / 100
}

fn constrain_budget(
    evidence: &mut SelectedEvidence,
    field: ModelMetadataField,
    requested: usize,
    upper: usize,
    source: ModelMetadataConstraintSource,
) -> usize {
    let effective = requested.min(upper);
    if effective != requested {
        evidence.constrain(SelectedConstraint {
            field,
            source,
            clamp: Some((requested, effective)),
        });
    }
    effective
}

#[cfg(test)]
fn provider_id(provider: &str) -> ProviderId {
    ProviderId::parse(provider).expect("valid built-in provider id")
}

#[cfg(test)]
fn reasoning_effort_options(
    model_ref: &ModelRef,
    endpoint: Option<&ProviderEndpointId>,
    capabilities: &ModelCapabilityFlags,
) -> Vec<String> {
    borrowed_reasoning_effort_options(model_ref, endpoint, capabilities)
        .iter()
        .map(|option| (*option).to_owned())
        .collect()
}

fn borrowed_reasoning_effort_options(
    model_ref: &ModelRef,
    endpoint: Option<&ProviderEndpointId>,
    capabilities: &ModelCapabilityFlags,
) -> &'static [&'static str] {
    if !capabilities.supports_reasoning {
        return &[];
    }

    match (model_ref.provider.as_str(), model_ref.model.as_str()) {
        ("openai-codex", "gpt-6.1-sol" | "gpt-6.1-terra") => {
            &["low", "medium", "high", "xhigh", "max", "ultra"][..]
        }
        ("openai-codex", "gpt-6-luna") => &["low", "medium", "high", "xhigh", "max"][..],
        ("openai-codex", "gpt-6-astra" | "gpt-6-sol") => {
            &["low", "medium", "high", "xhigh", "max"][..]
        }
        ("openai-codex", "gpt-5.6-sol" | "gpt-5.6-terra") => {
            &["low", "medium", "high", "xhigh", "max"][..]
        }
        ("openai-codex", "gpt-5.6-luna") => &["low", "medium", "high", "xhigh", "max"][..],
        ("openai", _) | ("openai-codex", _) => &["low", "medium", "high", "xhigh"][..],
        ("deepseek", _) if endpoint.is_some_and(|endpoint| endpoint.as_str() == "responses") => {
            &["none", "minimal", "low", "medium", "high", "xhigh", "max"][..]
        }
        ("deepseek", _) => &["low", "high", "max"][..],
        ("minimax" | "minimax-cn", "MiniMax-M3.1-Flash") => {
            &["low", "medium", "high", "xhigh", "max"][..]
        }
        ("minimax" | "minimax-cn", "MiniMax-M3.1-Flash-Preview")
            if endpoint.is_some_and(|endpoint| endpoint.as_str() == "m-plan") =>
        {
            &["low", "medium", "high", "xhigh", "max"][..]
        }
        ("xai", "grok-4.3") => &["none", "low", "medium", "high"][..],
        ("xai", "grok-4.5") => &["low", "medium", "high"][..],
        ("stepfun", "step-3.7-flash") => &["low", "medium", "high"][..],
        ("stepfun", "step-3.5-flash-2603") => &["low", "high"][..],
        ("fireworks", "accounts/fireworks/models/deepseek-v4-flash")
        | ("fireworks", "accounts/fireworks/models/deepseek-v4-pro") => {
            &["none", "low", "medium", "high", "xhigh", "max"][..]
        }
        ("fireworks", "accounts/fireworks/models/minimax-m2p7") => &["low", "medium", "high"][..],
        ("huggingface", "openai/gpt-oss-120b") => &["low", "medium", "high"][..],
        ("venice", "zai-org-glm-4.7" | "qwen3-235b-a22b-thinking-2507") => {
            &["low", "medium", "high"][..]
        }
        ("zai" | "bigmodel", "glm-5.2") => &["high", "max"][..],
        ("dashscope", "qwen3.8-flash") => &["low", "medium", "xhigh"][..],
        ("zai" | "bigmodel", "glm-5.3" | "glm-5.3-flash") => &["low", "high", "max"][..],
        ("moonshot", "kimi-k3") => &["low", "high", "max"][..],
        ("xiaomi", "mimo-v2.6-pro" | "mimo-v2.6-flash")
            if endpoint
                .is_none_or(|endpoint| matches!(endpoint.as_str(), "default" | "token-plan")) =>
        {
            &["none", "low", "medium", "high"][..]
        }
        ("xiaomi", "mimo-v2.6-pro-ultraspeed")
            if endpoint.is_none_or(|endpoint| endpoint.as_str() == "default") =>
        {
            &["none", "low", "medium", "high"][..]
        }
        ("xiaomi", "mimo-v2.5-pro" | "mimo-v2.5")
            if endpoint
                .is_none_or(|endpoint| matches!(endpoint.as_str(), "default" | "token-plan")) =>
        {
            &["none", "high"][..]
        }
        ("volcengine", _)
            if endpoint.is_none_or(|endpoint| {
                matches!(endpoint.as_str(), "default" | "coding" | "plan")
            }) && !matches!(
                model_ref.model.as_str(),
                "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k3.1-pro" | "deepseek-v4.1-flash-economy"
            ) =>
        {
            &["low", "medium", "high"][..]
        }
        _ => &[][..],
    }
}

pub mod models_dev;
#[cfg(test)]
#[path = "test_support/providers/mod.rs"]
mod providers;
mod snapshot;
#[cfg(test)]
use providers::common::catalog_model;

#[cfg(test)]
fn built_in_entries() -> Vec<BuiltInModelMetadata> {
    let mut registrations = provider_definitions()
        .iter()
        .filter_map(|definition| definition.catalog_registration)
        .collect::<Vec<_>>();
    registrations.sort();
    registrations
        .into_iter()
        .flat_map(providers::entries_for_registration)
        .collect()
}

#[cfg(test)]
fn built_in_route_definitions() -> Vec<BuiltInModelRouteDefinition> {
    let mut definitions = providers::route_definitions();
    let provider = provider_id("opencode-go");
    for &(id, preferred) in holon_domain::model_values::OPENCODE_GO_TEXT_MODELS {
        for endpoint in ["default", "messages", "responses"] {
            if endpoint == preferred {
                continue;
            }
            definitions.push(BuiltInModelRouteDefinition {
                legacy_provider: provider.clone(),
                model_ref: ModelRef::new(provider.clone(), id),
                endpoint: ProviderEndpointId::parse(endpoint).expect("valid Go endpoint"),
                policy: BuiltInModelRoutePolicy {
                    description: Some(
                        "OpenCode Go translated text route; capacity, vision, reasoning controls, and explicit cache support are unconfirmed."
                            .into(),
                    ),
                    context_window_tokens: Some(None),
                    auto_compact_token_limit: Some(None),
                    default_max_output_tokens: Some(None),
                    max_output_tokens_upper_limit: Some(None),
                    default_verbosity: Some(None),
                    capabilities: ModelCapabilityOverride {
                        parallel_tool_calls: Some(false),
                        image_input: Some(false),
                        image_generation: Some(false),
                        supports_reasoning: Some(false),
                        interactive_exec: Some(false),
                        agent_turn: matches!(id, "hy4" | "hy4-flash-free" | "crouton-alpha-free" | "longcat-2.5-free")
                            .then_some(false),
                        ..ModelCapabilityOverride::default()
                    },
                    reasoning_effort_options: Some(Vec::new()),
                    cache_capabilities: Some(ModelRouteCacheCapabilities {
                        cache_control: Some(false),
                    }),
                    ..BuiltInModelRoutePolicy::default()
                },
            });
        }
    }
    definitions
}

#[cfg(test)]
fn is_turn_default_candidate(entry: &BuiltInModelMetadata) -> bool {
    is_turn_default_route_candidate(entry, &BuiltInModelRoutePolicy::default())
}

/// Reviewed default endpoint per legacy provider.
///
/// Protocol priority is Anthropic Messages > OpenAI Responses > OpenAI Chat
/// Completions. Only providers whose protocol endpoints were reviewed for
/// promotion appear here; every other endpoint of these providers stays
/// routable as an explicit route.
#[cfg(test)]
const REVIEWED_DEFAULT_ENDPOINTS: &[(&str, &str)] = &[
    ("tencent-tokenhub", "messages"),
    ("opencode-go", "messages"),
];

#[cfg(test)]
fn reviewed_default_endpoint(legacy_provider: &ProviderId) -> Option<&'static str> {
    REVIEWED_DEFAULT_ENDPOINTS
        .iter()
        .find(|(provider, _)| *provider == legacy_provider.as_str())
        .map(|(_, endpoint)| *endpoint)
}

/// Internal protocol sub-endpoints register their routes under their own legacy
/// id, but the promoted protocol belongs to the parent provider.
#[cfg(test)]
const SUB_ENDPOINT_PARENTS: &[(&str, &str)] = &[("tencent-tokenhub-messages", "tencent-tokenhub")];

#[cfg(test)]
fn promoted_parent(legacy_provider: &ProviderId) -> Option<ProviderId> {
    SUB_ENDPOINT_PARENTS
        .iter()
        .find(|(sub_endpoint, _)| *sub_endpoint == legacy_provider.as_str())
        .map(|(_, parent)| provider_id(parent))
}

/// Providers whose reviewed default endpoint also becomes the preferred route
/// of every model that supports it. OpenCode Go is excluded: its official table
/// documents a native protocol per model, so only its provider-level default is
/// promoted.
#[cfg(test)]
const MODEL_ENDPOINT_PROMOTIONS: &[&str] = &["tencent-tokenhub"];

/// Reviewed endpoint for a route definition's model-level preference.
/// Sub-endpoint definitions resolve against the parent provider because their
/// canonical model ref belongs to it.
#[cfg(test)]
fn reviewed_model_endpoint(legacy_provider: &ProviderId) -> Option<&'static str> {
    let owner = promoted_parent(legacy_provider).unwrap_or_else(|| legacy_provider.clone());
    MODEL_ENDPOINT_PROMOTIONS
        .contains(&owner.as_str())
        .then(|| reviewed_default_endpoint(&owner))
        .flatten()
}

/// Provider-level preference: keep the first candidate unless a reviewed
/// default endpoint candidate appears.
#[cfg(test)]
fn prefers_provider_route(
    current: Option<&ModelRouteRef>,
    candidate: &ModelRouteRef,
    reviewed_endpoint: Option<&str>,
) -> bool {
    match (current, reviewed_endpoint) {
        (None, _) => true,
        (Some(current), Some(endpoint)) => {
            current.endpoint.as_str() != endpoint && candidate.endpoint.as_str() == endpoint
        }
        (Some(_), None) => false,
    }
}

/// Model-level preference: the default endpoint wins over siblings, and a
/// reviewed default endpoint outranks the default endpoint.
#[cfg(test)]
fn prefers_model_route(
    current: Option<&ModelRouteRef>,
    candidate: &ModelRouteRef,
    reviewed_endpoint: Option<&str>,
) -> bool {
    let Some(current) = current else {
        return true;
    };
    if let Some(endpoint) = reviewed_endpoint {
        let current_matches = current.endpoint.as_str() == endpoint;
        let candidate_matches = candidate.endpoint.as_str() == endpoint;
        if current_matches != candidate_matches {
            return candidate_matches;
        }
    }
    current.endpoint != ProviderEndpointId::default_endpoint()
        && candidate.endpoint == ProviderEndpointId::default_endpoint()
}

/// Definition-level model preference: a route definition only adds a route for
/// a model, unless it carries the reviewed default endpoint.
#[cfg(test)]
fn prefers_definition_model_route(
    current: Option<&ModelRouteRef>,
    candidate: &ModelRouteRef,
    reviewed_endpoint: Option<&str>,
) -> bool {
    match (current, reviewed_endpoint) {
        (None, _) => true,
        (Some(current), Some(endpoint)) => {
            current.endpoint.as_str() != endpoint && candidate.endpoint.as_str() == endpoint
        }
        (Some(_), None) => false,
    }
}

#[cfg(test)]
fn is_turn_default_route_candidate(
    entry: &BuiltInModelMetadata,
    route: &BuiltInModelRoutePolicy,
) -> bool {
    route
        .context_window_tokens
        .unwrap_or(entry.context_window_tokens)
        .is_some()
        || [
            (
                entry.capabilities.parallel_tool_calls,
                route.capabilities.parallel_tool_calls,
            ),
            (
                entry.capabilities.image_input,
                route.capabilities.image_input,
            ),
            (
                entry.capabilities.supports_reasoning,
                route.capabilities.supports_reasoning,
            ),
            (
                entry.capabilities.interactive_exec,
                route.capabilities.interactive_exec,
            ),
        ]
        .into_iter()
        .any(|(intrinsic, constraint)| intrinsic && constraint != Some(false))
}

fn default_verbosity_for_model(model_ref: &ModelRef) -> Option<ModelVerbosity> {
    (model_ref.provider == ProviderId::openai_codex()).then_some(ModelVerbosity::Low)
}

#[cfg(test)]
mod tests;

/// Select discovered metadata for one route identity.
///
/// Route-scoped lookup keeps sibling endpoints of the same provider separate.
/// When no endpoint is requested, the default endpoint wins over any sibling,
/// then the lowest endpoint id, so resolution stays deterministic.
fn select_discovered_metadata<'a>(
    discovered_models: &'a HashMap<ModelRouteRef, BuiltInModelMetadata>,
    model_ref: &ModelRef,
    endpoint: Option<&ProviderEndpointId>,
) -> Option<&'a BuiltInModelMetadata> {
    match endpoint {
        Some(endpoint) => discovered_models.get(&ModelRouteRef::new(
            model_ref.provider.clone(),
            endpoint.clone(),
            model_ref.model.clone(),
        )),
        None => discovered_models
            .iter()
            .filter(|(route, _)| {
                route.provider == model_ref.provider && route.model == model_ref.model
            })
            .min_by(|(left, _), (right, _)| {
                let left_default = left.endpoint.as_str() == ProviderEndpointId::DEFAULT;
                let right_default = right.endpoint.as_str() == ProviderEndpointId::DEFAULT;
                right_default
                    .cmp(&left_default)
                    .then_with(|| left.endpoint.cmp(&right.endpoint))
            })
            .map(|(_, metadata)| metadata),
    }
}

/// Build a route-keyed discovered-model map from `(model_ref, metadata)` pairs.
///
/// Endpoint attribution comes from the metadata when present, otherwise the
/// provider default endpoint.
#[cfg(any(test, feature = "test-support"))]
pub fn discovered_by_route(
    entries: impl IntoIterator<Item = (ModelRef, BuiltInModelMetadata)>,
) -> HashMap<ModelRouteRef, BuiltInModelMetadata> {
    entries
        .into_iter()
        .map(|(model_ref, metadata)| {
            let endpoint = metadata
                .endpoint
                .clone()
                .unwrap_or_else(ProviderEndpointId::default_endpoint);
            (
                ModelRouteRef::new(
                    model_ref.provider.clone(),
                    endpoint,
                    model_ref.model.clone(),
                ),
                metadata,
            )
        })
        .collect()
}
