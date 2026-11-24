pub use holon_provider::model_discovery::*;

#[cfg(all(test, holon_lib_test_shard = "c"))]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use super::*;
    use crate::config::{ModelRef, ProviderEndpointId, ProviderId};
    use crate::model_catalog::{BuiltInModelMetadata, ModelMetadataSource};
    use holon_provider::model_discovery::decode_opencode_go_models;

    #[test]
    fn opencode_go_discovered_native_metadata_stays_on_its_resolved_endpoint() {
        use crate::{
            config::ModelRouteRef,
            context::ContextConfig,
            model_catalog::{BuiltInModelCatalog, ModelRuntimeOverride},
        };

        let catalog = BuiltInModelCatalog::new();
        let provider = ProviderId::parse("opencode-go").unwrap();
        for (id, native_endpoint, api, base_url, context, output, efforts) in [
            (
                "kimi-k3.1-pro",
                "default",
                "chat-completions",
                "https://opencode.ai/inference/go/openai/v1",
                1_048_576,
                131_072,
                vec!["low", "high", "max"],
            ),
            (
                "minimax-m3",
                "messages",
                "messages",
                "https://opencode.ai/inference/go/anthropic/v1",
                1_000_000,
                131_072,
                vec![],
            ),
            (
                "grok-4.7",
                "responses",
                "responses",
                "https://opencode.ai/inference/go/openai/v1",
                500_000,
                500_000,
                vec!["low", "medium", "high", "xhigh"],
            ),
            (
                "crouton-alpha-free",
                "responses",
                "responses",
                "https://opencode.ai/inference/go/openai/v1",
                1_048_576,
                270_000,
                vec!["minimal", "low", "medium", "high", "xhigh", "max"],
            ),
        ] {
            let decoded = decode_opencode_go_models(
                &provider,
                serde_json::json!({"data": [{
                    "id": id,
                    "modalities": {"input": ["text", "image/jpeg"], "output": ["text"]},
                    "capabilities": {"reasoning": {"supported": true, "effort": efforts}},
                    "limit": {"context": context, "output": output},
                    "api": {(api): {"base_url": base_url}}
                }]}),
            )
            .unwrap()
            .pop()
            .unwrap();
            let model_ref = decoded.model_ref.clone();
            let discovered =
                holon_model_catalog::discovered_by_route([(model_ref.clone(), decoded)]);
            let empty_discovered = HashMap::new();
            let preferred = catalog.resolve_policy(
                &model_ref,
                &HashMap::new(),
                &discovered,
                None,
                &ContextConfig::default(),
                8192,
            );
            assert_eq!(preferred.context_window_tokens, Some(context), "{id}");
            assert_eq!(
                preferred.source,
                ModelMetadataSource::RemoteDiscovered,
                "{id}"
            );

            for endpoint in ["default", "messages", "responses"] {
                let route = ModelRouteRef::parse(&format!("opencode-go@{endpoint}/{id}")).unwrap();
                let resolve =
                    |overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
                     discovered: &HashMap<ModelRouteRef, BuiltInModelMetadata>| {
                        catalog.resolve_route_policy(
                            &route,
                            overrides,
                            discovered,
                            None,
                            &ContextConfig::default(),
                            8192,
                        )
                    };
                let policy = resolve(&HashMap::new(), &discovered);
                if endpoint == native_endpoint {
                    assert_eq!(policy.context_window_tokens, Some(context), "{route:?}");
                    assert_eq!(
                        policy.max_output_tokens_upper_limit,
                        Some(output),
                        "{route:?}"
                    );
                    assert_eq!(
                        policy.source,
                        ModelMetadataSource::RemoteDiscovered,
                        "{route:?}"
                    );
                } else {
                    let without_discovery = resolve(&HashMap::new(), &empty_discovered);
                    assert_eq!(without_discovery.context_window_tokens, None, "{route:?}");
                    assert_eq!(policy, without_discovery, "{route:?}");
                }
                if native_endpoint == "default" {
                    let mut explicit_default = discovered.clone();
                    if let Some(entry) = explicit_default.get_mut(&ModelRouteRef::new(
                        model_ref.provider.clone(),
                        ProviderEndpointId::default_endpoint(),
                        model_ref.model.clone(),
                    )) {
                        entry.endpoint = Some(ProviderEndpointId::default_endpoint());
                    }
                    assert_eq!(
                        resolve(&HashMap::new(), &explicit_default),
                        policy,
                        "{route:?}"
                    );
                }
                let overrides = HashMap::from([(
                    model_ref.clone(),
                    ModelRuntimeOverride {
                        context_window_tokens: Some(16_384),
                        ..Default::default()
                    },
                )]);
                let overridden = resolve(&overrides, &discovered);
                assert_eq!(overridden.context_window_tokens, Some(16_384), "{route:?}");
                assert_eq!(
                    overridden.source,
                    ModelMetadataSource::ConfigOverride,
                    "{route:?}"
                );
            }
        }
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
            discovery_cache_status_for_provider(
                litellm,
                None,
                &cache,
                Duration::from_secs(60),
                false
            )
            .state,
            ModelDiscoveryCacheState::Unauthenticated
        );
        assert_eq!(
            provider_models_url(litellm, None).unwrap(),
            "http://localhost:4000/v1/models"
        );
        assert_eq!(
            discovery_cache_status_for_provider(vllm, None, &cache, Duration::from_secs(60), false)
                .state,
            ModelDiscoveryCacheState::Missing
        );
        assert_eq!(
            provider_models_url(vllm, None).unwrap(),
            "http://127.0.0.1:8000/v1/models"
        );
    }
}
