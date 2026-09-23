#[cfg(test)]
use super::super::*;

/// TypeSafe serves Jev through one typed evaluation endpoint, so the route is
/// Decision-only: it declares `capabilities.decision` and an explicit
/// `decision_protocol` instead of an agent-turn or provider-name inference.
#[cfg(test)]
pub(super) fn entries() -> Vec<BuiltInModelMetadata> {
    vec![BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id("typesafe"), "jev-latest"),
        display_name: "JEV Latest".into(),
        description:
            "TypeSafe System One JEV decision model served through the typed evaluation endpoint."
                .into(),
        context_window_tokens: None,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        default_verbosity: None,
        tool_output_truncation_estimated_tokens: None,
        capabilities: ModelCapabilityFlags {
            agent_turn: false,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::BuiltInCatalog,
        endpoint: None,
    }]
}

#[cfg(test)]
pub(super) fn route_definitions() -> Vec<BuiltInModelRouteDefinition> {
    vec![BuiltInModelRouteDefinition {
        legacy_provider: provider_id("typesafe"),
        model_ref: ModelRef::new(provider_id("typesafe"), "jev-latest"),
        endpoint: ProviderEndpointId::parse("default").expect("valid built-in endpoint"),
        policy: BuiltInModelRoutePolicy {
            capabilities: ModelCapabilityOverride {
                decision: Some(true),
                ..ModelCapabilityOverride::default()
            },
            decision_protocol: Some(DecisionProtocol::Jev),
            ..BuiltInModelRoutePolicy::default()
        },
    }]
}
