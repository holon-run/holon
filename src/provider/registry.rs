use std::{path::Path, sync::Arc};

use anyhow::Result;

use crate::config::{ProviderTransportKind, ResolvedModelRoute, TransportCapabilities};

use super::{
    transports::{
        GeminiProvider, OpenAiChatCompletionsProvider, OpenAiCodexProvider, OpenAiCompactionPolicy,
        OpenAiProvider,
    },
    AgentProvider, AnthropicProvider,
};

mod providers;

pub(crate) use providers::{
    provider_definition, provider_definitions, ModelDiscoveryAuth, ModelDiscoveryDecoder,
    ModelDiscoveryDefinition, ModelDiscoveryRoute, ProviderCatalogPolicy,
    ProviderCatalogRegistration, ProviderContextManagement, ProviderDefinition,
    ProviderMaterializer, ProviderWebSearch,
};

type ProviderBuilder = fn(&Path, &ResolvedModelRoute) -> Result<Arc<dyn AgentProvider>>;

pub(crate) struct ProviderTransportDefinition {
    pub(crate) kind: ProviderTransportKind,
    pub(crate) wire_name: &'static str,
    pub(crate) capabilities: TransportCapabilities,
    builder: ProviderBuilder,
}

const TRANSPORT_DEFINITIONS: &[ProviderTransportDefinition] = &[
    ProviderTransportDefinition {
        kind: ProviderTransportKind::OpenAiCodexResponses,
        wire_name: "openai_codex_responses",
        capabilities: TransportCapabilities {
            image_input: true,
            image_output: true,
        },
        builder: build_openai_codex_provider,
    },
    ProviderTransportDefinition {
        kind: ProviderTransportKind::OpenAiResponses,
        wire_name: "openai_responses",
        capabilities: TransportCapabilities {
            image_input: true,
            image_output: true,
        },
        builder: build_openai_provider,
    },
    ProviderTransportDefinition {
        kind: ProviderTransportKind::OpenAiChatCompletions,
        wire_name: "openai_chat_completions",
        capabilities: TransportCapabilities {
            image_input: true,
            image_output: true,
        },
        builder: build_openai_chat_completions_provider,
    },
    ProviderTransportDefinition {
        kind: ProviderTransportKind::AnthropicMessages,
        wire_name: "anthropic_messages",
        capabilities: TransportCapabilities {
            image_input: true,
            image_output: false,
        },
        builder: build_anthropic_provider,
    },
    ProviderTransportDefinition {
        kind: ProviderTransportKind::GeminiGenerateContent,
        wire_name: "gemini_generate_content",
        capabilities: TransportCapabilities {
            image_input: false,
            image_output: false,
        },
        builder: build_gemini_provider,
    },
];

pub(crate) fn provider_transport_definitions() -> &'static [ProviderTransportDefinition] {
    TRANSPORT_DEFINITIONS
}

pub(crate) fn provider_transport_definition(
    kind: ProviderTransportKind,
) -> &'static ProviderTransportDefinition {
    TRANSPORT_DEFINITIONS
        .iter()
        .find(|definition| definition.kind == kind)
        .expect("every provider transport kind must have one definition")
}

pub(crate) fn provider_transport_definition_by_wire_name(
    wire_name: &str,
) -> Option<&'static ProviderTransportDefinition> {
    TRANSPORT_DEFINITIONS
        .iter()
        .find(|definition| definition.wire_name == wire_name)
}

pub(crate) fn build_provider_for_route(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<Arc<dyn AgentProvider>> {
    (provider_transport_definition(route.provider_config().transport).builder)(home_dir, route)
}

fn openai_compaction_policy(route: &ResolvedModelRoute) -> OpenAiCompactionPolicy {
    OpenAiCompactionPolicy {
        trigger_input_tokens: route.policy.compaction_trigger_estimated_tokens as u64,
    }
}

fn build_openai_codex_provider(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<Arc<dyn AgentProvider>> {
    Ok(Arc::new(
        OpenAiCodexProvider::from_runtime_config_with_compaction_policy(
            route.provider_config(),
            &route.model_ref.model,
            route.policy.runtime_max_output_tokens,
            home_dir,
            openai_compaction_policy(route),
            route.policy.verbosity,
            route.policy.capabilities.supports_reasoning,
        )?,
    ))
}

fn build_openai_provider(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<Arc<dyn AgentProvider>> {
    Ok(Arc::new(
        OpenAiProvider::from_runtime_config_with_compaction_policy(
            route.provider_config(),
            &route.model_ref.model,
            route.policy.runtime_max_output_tokens,
            home_dir,
            openai_compaction_policy(route),
        )?,
    ))
}

fn build_anthropic_provider(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<Arc<dyn AgentProvider>> {
    Ok(Arc::new(AnthropicProvider::from_runtime_config(
        route.provider_config(),
        &route.model_ref.model,
        route.policy.runtime_max_output_tokens,
        home_dir,
        route.policy.capabilities.supports_reasoning,
    )?))
}

fn build_openai_chat_completions_provider(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<Arc<dyn AgentProvider>> {
    Ok(Arc::new(
        OpenAiChatCompletionsProvider::from_resolved_runtime_config(
            route.provider_config(),
            &route.model_ref.model,
            route.policy.runtime_max_output_tokens,
            home_dir,
        )?,
    ))
}

fn build_gemini_provider(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<Arc<dyn AgentProvider>> {
    Ok(Arc::new(GeminiProvider::from_runtime_config(
        route.provider_config(),
        &route.model_ref.model,
        route.policy.runtime_max_output_tokens,
        home_dir,
    )?))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::config::{built_in_provider_registry_with_settings, ProviderEndpointId, ProviderId};

    #[test]
    fn transport_definitions_are_complete_and_unique() {
        let expected = [
            ProviderTransportKind::OpenAiCodexResponses,
            ProviderTransportKind::OpenAiResponses,
            ProviderTransportKind::OpenAiChatCompletions,
            ProviderTransportKind::AnthropicMessages,
            ProviderTransportKind::GeminiGenerateContent,
        ];
        let definitions = provider_transport_definitions();
        assert_eq!(definitions.len(), expected.len());

        let mut kinds = Vec::new();
        let mut wire_names = HashSet::new();
        for definition in definitions {
            assert!(!kinds.contains(&definition.kind));
            kinds.push(definition.kind);
            assert!(wire_names.insert(definition.wire_name));
            assert!(!definition.wire_name.is_empty());
        }
        for kind in expected {
            assert_eq!(provider_transport_definition(kind).kind, kind);
        }
    }

    #[test]
    fn provider_definitions_are_unique_and_materialize_registered_endpoints() {
        let definitions = provider_definitions();
        let mut legacy_providers = HashSet::new();
        let mut canonical_routes = HashSet::new();
        let mut catalog_registrations = HashSet::new();
        for definition in definitions {
            assert!(legacy_providers.insert(definition.legacy_provider));
            if definition.materializer != ProviderMaterializer::AliasOnly {
                assert!(canonical_routes
                    .insert((definition.route_provider, definition.route_endpoint,)));
            }
            assert!(provider_transport_definitions()
                .iter()
                .any(|transport| transport.kind == definition.transport));
            if definition.discovery.is_some() {
                assert_ne!(definition.catalog_policy, ProviderCatalogPolicy::StaticOnly);
            }
            if let Some(registration) = definition.catalog_registration {
                assert!(catalog_registrations.insert(registration));
            }
        }
        assert_eq!(catalog_registrations.len(), 9);

        let registry = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
        let materialized = definitions
            .iter()
            .filter(|definition| definition.materializer != ProviderMaterializer::AliasOnly)
            .collect::<Vec<_>>();
        assert_eq!(registry.len(), materialized.len());
        for definition in materialized {
            let id = ProviderId::parse(definition.legacy_provider).unwrap();
            let runtime = registry.get(&id).unwrap();
            assert_eq!(
                runtime.route_provider,
                ProviderId::parse(definition.route_provider).unwrap()
            );
            assert_eq!(
                runtime.route_endpoint,
                ProviderEndpointId::parse(definition.route_endpoint).unwrap()
            );
            assert_eq!(runtime.transport, definition.transport);
        }
    }
}
