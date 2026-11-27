use super::*;
use holon_model_catalog::{ModelCapabilityOverride, ModelMetadataSource};
use holon_provider::model_discovery::ProviderModelDiscoveryCache;

#[test]
fn provider_chain_refs_match_independent_owned_selection_matrix() {
    let fixture = catalog_fixture("openai@default/gpt-5.4", &[]);
    let mut catalog = RuntimeModelCatalog::from_config(&fixture);
    let routes = [
        catalog.default_model.clone(),
        ModelRouteRef::parse("openai@alternate/gpt-5.4").unwrap(),
        ModelRouteRef::parse("anthropic@default/fallback-汉字🦀").unwrap(),
        ModelRouteRef::parse("openai@default/override").unwrap(),
    ];
    for length in 0..=5 {
        for mut code in 0..4usize.pow(length) {
            catalog.fallback_models = (0..length)
                .map(|_| {
                    let route = routes[code % 4].clone();
                    code /= 4;
                    route
                })
                .collect();
            for model_override in [None, Some(&routes[0]), Some(&routes[3])] {
                for disabled in [false, true] {
                    catalog.disable_provider_fallback = disabled;
                    let primary = model_override.unwrap_or(&catalog.default_model);
                    // Original owning algorithm, independent of the iterator.
                    let mut expected = vec![primary.clone()];
                    if !disabled {
                        for route in &catalog.fallback_models {
                            if !expected.contains(route) {
                                expected.push(route.clone());
                            }
                        }
                    }
                    assert_eq!(
                        catalog
                            .provider_chain_refs(model_override)
                            .cloned()
                            .collect::<Vec<_>>(),
                        expected
                    );
                    assert_eq!(catalog.provider_chain(model_override), expected);
                }
            }
        }
    }
}

#[test]
fn provider_chain_refs_borrow_primary_and_first_fallback_without_copying() {
    let fixture = catalog_fixture("openai@default/gpt-5.4", &[]);
    let mut catalog = RuntimeModelCatalog::from_config(&fixture);
    let primary = catalog.default_model.clone();
    let fallback = ModelRouteRef::parse("openai@alternate/gpt-5.4").unwrap();
    let other = ModelRouteRef::parse("anthropic@default/other").unwrap();
    catalog.fallback_models = vec![
        primary.clone(),
        fallback.clone(),
        fallback,
        primary.clone(),
        other,
    ];
    {
        let mut chain = catalog.provider_chain_refs(None);
        assert!(std::ptr::eq(chain.next().unwrap(), &catalog.default_model));
        assert!(std::ptr::eq(
            chain.next().unwrap(),
            &catalog.fallback_models[1]
        ));
        // Dropping a partially consumed iterator owns no route material.
    }
    let mut chain = catalog.provider_chain_refs(Some(&primary));
    assert!(std::ptr::eq(chain.next().unwrap(), &primary));
    for index in [1, 4] {
        assert!(std::ptr::eq(
            chain.next().unwrap(),
            &catalog.fallback_models[index]
        ));
    }
    assert!(chain.next().is_none());
    assert!(chain.next().is_none());
    drop(chain);
    catalog.disable_provider_fallback = true;
    let mut chain = catalog.provider_chain_refs(None);
    assert!(std::ptr::eq(chain.next().unwrap(), &catalog.default_model));
    assert!(chain.next().is_none());
}

#[test]
fn resolves_compatible_reasoning_effort_for_non_codex_route() {
    let fixture = catalog_fixture("anthropic@default/claude-sonnet-4-6", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("anthropic@default/claude-sonnet-4-6"),
            ModelRouteCapability::Turn,
        )
        .unwrap();

    assert!(route.validate_reasoning_effort("xhigh").is_ok());
    assert!(route.validate_reasoning_effort("none").is_ok());
    assert!(route.validate_reasoning_effort("arbitrary").is_err());
}

#[test]
fn runtime_model_catalog_separates_turn_and_decision_capabilities_per_model() {
    let mut fixture = catalog_fixture("openai@default/turn-model", &[]);
    let turn_model = ModelRef::parse("openai/turn-model").unwrap();
    let decision_model = ModelRef::parse("openai/decision-model").unwrap();
    let turn_override = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            agent_turn: Some(true),
            decision: Some(false),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    let decision_override = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            agent_turn: Some(false),
            decision: Some(true),
            ..ModelCapabilityOverride::default()
        }),
        decision_protocol: Some(holon_model_catalog::DecisionProtocol::Jev),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .validated_model_overrides
        .insert(turn_model, turn_override);
    fixture
        .validated_model_overrides
        .insert(decision_model, decision_override);

    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let context = ContextConfig::default();
    let turn_route = route_ref("openai@default/turn-model");
    let decision_route = route_ref("openai@default/decision-model");

    assert!(catalog
        .resolve_explicit_model_route(&context, &turn_route, ModelRouteCapability::Turn)
        .is_some());
    assert!(catalog
        .resolve_explicit_model_route(&context, &turn_route, ModelRouteCapability::Decision)
        .is_none());
    assert!(catalog
        .resolve_explicit_model_route(&context, &decision_route, ModelRouteCapability::Turn)
        .is_none());
    let decision = catalog
        .resolve_explicit_model_route(&context, &decision_route, ModelRouteCapability::Decision)
        .expect("Decision-capable model should resolve through the shared provider");
    assert_eq!(
        decision.policy.decision_protocol,
        Some(holon_model_catalog::DecisionProtocol::Jev)
    );
}

#[test]
fn provider_chain_for_turn_starts_at_pending_fallback_model() {
    let fixture = catalog_fixture(
        "openai@default/gpt-5.4",
        &[
            "anthropic@default/claude-sonnet-4-6",
            "openai-codex@default/gpt-5.3-codex-spark",
        ],
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let pending = route_ref("anthropic/claude-sonnet-4-6");

    let chain = catalog
        .provider_chain_for_turn(None, Some(&pending))
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(
        chain,
        vec![
            "anthropic@default/claude-sonnet-4-6",
            "openai-codex@default/gpt-5.3-codex-spark",
        ]
    );
}

#[test]
fn runtime_model_catalog_resolves_config_override_and_unknown_fallback() {
    let mut fixture = catalog_fixture("anthropic/claude-sonnet-4-6", &[]);
    let known_override = ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(24_000),
        runtime_max_output_tokens: Some(4_096),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .stored_config
        .models
        .catalog
        .insert("anthropic/claude-sonnet-4-6".into(), known_override.clone());
    fixture.validated_model_overrides.insert(
        ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
        known_override,
    );
    let unknown_fallback = ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(12_000),
        compaction_trigger_estimated_tokens: Some(10_000),
        ..ModelRuntimeOverride::default()
    };
    fixture.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
    fixture.validated_unknown_model_fallback = Some(unknown_fallback);
    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let base_context = ContextConfig {
        callback_base_url: "https://callback.example".into(),
        ..test_context()
    };

    let known = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(known.prompt_budget_estimated_tokens, 24_000);
    assert_eq!(known.runtime_max_output_tokens, 4_096);
    let resolved_context = catalog.resolved_context_config(&base_context, None);
    assert_eq!(resolved_context.prompt_budget_estimated_tokens, 24_000);
    assert_eq!(
        resolved_context.callback_base_url,
        "https://callback.example"
    );

    let unknown =
        catalog.resolved_model_policy(&base_context, Some(&route_ref("openai/custom-model")));
    assert_eq!(unknown.prompt_budget_estimated_tokens, 12_000);
    assert_eq!(unknown.compaction_trigger_estimated_tokens, 10_000);
    assert_eq!(
        unknown.source,
        holon_model_catalog::ModelMetadataSource::UnknownFallback
    );
}

#[test]
fn runtime_model_catalog_materializes_legacy_provider_as_default_route_endpoint() {
    let fixture = catalog_fixture("openai/gpt-5.4", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let route = catalog
        .resolve_model_route(
            &ContextConfig::default(),
            &ModelRef::parse("openai/gpt-5.4").unwrap(),
            ModelRouteCapability::Turn,
        )
        .unwrap();

    assert_eq!(route.model_ref.as_string(), "openai/gpt-5.4");
    assert_eq!(route.endpoint.provider.as_str(), "openai");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
    assert_eq!(
        route.endpoint.runtime_config.transport,
        ProviderTransportKind::OpenAiResponses
    );
}

#[test]
fn runtime_model_catalog_exposes_deepseek_responses_without_changing_default_route() {
    let mut fixture = catalog_fixture("deepseek/deepseek-v4-pro", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    fixture
        .providers
        .insert(deepseek.clone(), built_ins.get(&deepseek).unwrap().clone());
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let default_route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("deepseek@default/deepseek-v4-pro"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(
        default_route.endpoint.runtime_config.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(
        default_route.endpoint.runtime_config.base_url,
        "https://api.deepseek.com/anthropic"
    );

    let responses_route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("deepseek@responses/deepseek-v4-pro"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(
        responses_route.route_ref.as_string(),
        "deepseek@responses/deepseek-v4-pro"
    );
    assert_eq!(
        responses_route.endpoint.runtime_config.transport,
        ProviderTransportKind::OpenAiResponses
    );
    assert_eq!(
        responses_route.endpoint.runtime_config.base_url,
        "https://api.deepseek.com/v1"
    );
    let search = responses_route
        .endpoint
        .runtime_config
        .builtin_web_search
        .as_ref()
        .unwrap();
    assert!(search.enabled);
    assert_eq!(search.kind, ProviderNativeWebSearchKind::DeepSeek);
    assert_eq!(search.advertised_tool_type, "web_search");
    assert_eq!(search.backend_kind, "deepseek_web_search");
    assert_eq!(
        default_route
            .provider_config()
            .builtin_web_search
            .as_ref()
            .unwrap()
            .advertised_tool_type,
        "web_search_20250305"
    );
    // The default-derived endpoint retains the search seed; model resolution
    // above translates it to the documented Responses tool contract.
    assert_eq!(
        catalog
            .provider_endpoints
            .iter()
            .find(|endpoint| endpoint.provider.as_str() == "deepseek"
                && endpoint.endpoint.as_str() == "responses")
            .unwrap()
            .runtime_config
            .builtin_web_search
            .as_ref()
            .unwrap()
            .advertised_tool_type,
        "web_search_20250305"
    );

    assert_eq!(
        ModelRouteRef::parse_compatible("deepseek/deepseek-v4-pro")
            .unwrap()
            .as_string(),
        "deepseek@default/deepseek-v4-pro"
    );
}

#[test]
fn runtime_model_catalog_deepseek_responses_search_respects_disabled_configuration() {
    let mut fixture = catalog_fixture("deepseek/deepseek-v4-pro", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    for search in [
        None,
        Some(ProviderBuiltinWebSearchConfig {
            enabled: false,
            kind: ProviderNativeWebSearchKind::DeepSeek,
            advertised_tool_type: "web_search_20250305".into(),
            backend_kind: "deepseek_web_search".into(),
        }),
    ] {
        let mut runtime = built_ins.get(&deepseek).unwrap().clone();
        runtime.builtin_web_search = search;
        fixture.providers.insert(deepseek.clone(), runtime);
        let catalog = RuntimeModelCatalog::from_config(&fixture);
        let route = catalog
            .resolve_explicit_model_route(
                &ContextConfig::default(),
                &route_ref("deepseek@responses/deepseek-v4-pro"),
                ModelRouteCapability::Turn,
            )
            .unwrap();
        assert!(route.provider_config().builtin_web_search.is_none());
    }
}

#[test]
fn runtime_model_catalog_derives_deepseek_responses_from_an_explicit_allowlist() {
    let mut fixture = catalog_fixture("deepseek/deepseek-v4-pro", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    let mut runtime = built_ins.get(&deepseek).unwrap().clone();
    runtime.base_url = "https://proxy.example/deepseek/anthropic/".into();
    runtime.reasoning_effort = Some("high".into());
    runtime.codex_home = Some(PathBuf::from("/should-not-leak"));
    runtime.originator = Some("anthropic-only-originator".into());
    runtime.context_management.enabled = true;
    fixture.providers.insert(deepseek, runtime);

    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let responses_route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("deepseek@responses/deepseek-v4-pro"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    let runtime = &responses_route.endpoint.runtime_config;

    assert_eq!(runtime.base_url, "https://proxy.example/deepseek/v1");
    assert_eq!(runtime.reasoning_effort.as_deref(), Some("high"));
    assert!(runtime.codex_home.is_none());
    assert!(runtime.originator.is_none());
    assert!(!runtime.context_management.enabled);
}

#[test]
fn runtime_model_catalog_opencode_go_standard_route_does_not_follow_discovery_protocol() {
    let mut fixture = catalog_fixture("opencode-go/kimi-k2.7-code", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    for legacy_provider in ["opencode-go", "opencode-go-messages"] {
        let provider = ProviderId::parse(legacy_provider).unwrap();
        fixture
            .providers
            .insert(provider.clone(), built_ins.get(&provider).unwrap().clone());
    }
    let mut catalog = RuntimeModelCatalog::from_config(&fixture);
    let template = catalog
        .built_in_catalog
        .get(&ModelRef::parse("opencode-go/kimi-k2.7-code").unwrap())
        .unwrap()
        .clone();
    for (model, endpoint, transport) in [
        (
            "discovery-only-responses",
            "responses",
            ProviderTransportKind::OpenAiResponses,
        ),
        (
            "discovery-only-messages",
            "messages",
            ProviderTransportKind::AnthropicMessages,
        ),
        (
            "discovery-only-chat",
            "default",
            ProviderTransportKind::OpenAiChatCompletions,
        ),
    ] {
        let model_ref = ModelRef::parse(&format!("opencode-go/{model}")).unwrap();
        let mut metadata = template.clone();
        metadata.model_ref = model_ref.clone();
        metadata.endpoint = Some(ProviderEndpointId::parse(endpoint).unwrap());
        let route = ModelRouteRef::new(
            model_ref.provider.clone(),
            metadata.endpoint.clone().unwrap(),
            model_ref.model.clone(),
        );
        catalog.discovered_models.insert(route, metadata);
        let expected_route = route_ref(&format!("opencode-go@messages/{model}"));
        let routes = catalog.available_model_routes();
        assert!(routes.contains(&expected_route));
        assert!(!routes.contains(&route_ref(&format!("opencode-go@default/{model}"))));
        let resolved = catalog
            .resolve_model_route(
                &ContextConfig::default(),
                &model_ref,
                ModelRouteCapability::Turn,
            )
            .unwrap();
        assert_eq!(resolved.route_ref, expected_route);
        assert_eq!(
            resolved.endpoint.runtime_config.transport,
            ProviderTransportKind::AnthropicMessages
        );
        let explicit = catalog.resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref(&format!("opencode-go@{endpoint}/{model}")),
            ModelRouteCapability::Turn,
        );
        if endpoint == "responses" {
            assert!(explicit.is_none());
        } else {
            assert_eq!(
                explicit.unwrap().endpoint.runtime_config.transport,
                transport
            );
        }
    }

    // Discovery must not replace an explicitly reviewed built-in route.
    let mut conflicting = template.clone();
    conflicting.endpoint = Some(ProviderEndpointId::parse("messages").unwrap());
    let conflicting_route = ModelRouteRef::new(
        template.model_ref.provider.clone(),
        conflicting
            .endpoint
            .clone()
            .unwrap_or_else(ProviderEndpointId::default_endpoint),
        template.model_ref.model.clone(),
    );
    catalog
        .discovered_models
        .insert(conflicting_route, conflicting);
    let resolved = catalog
        .resolve_model_route(
            &ContextConfig::default(),
            &template.model_ref,
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(resolved.endpoint.endpoint.as_str(), "messages");
    assert!(catalog
        .available_model_routes()
        .contains(&route_ref("opencode-go@default/kimi-k2.7-code")));
}

#[test]
fn runtime_model_catalog_opencode_go_uses_messages_without_a_responses_endpoint() {
    let mut fixture = catalog_fixture("opencode-go/minimax-m3", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    for legacy_provider in ["opencode-go", "opencode-go-messages"] {
        let provider = ProviderId::parse(legacy_provider).unwrap();
        fixture
            .providers
            .insert(provider.clone(), built_ins.get(&provider).unwrap().clone());
    }
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    assert!(catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("opencode-go@responses/minimax-m3"),
            ModelRouteCapability::Turn,
        )
        .is_none());

    let messages = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("opencode-go@messages/minimax-m3"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(messages.endpoint.provider.as_str(), "opencode-go");
    assert_eq!(messages.endpoint.endpoint.as_str(), "messages");
    assert_eq!(
        messages.endpoint.runtime_config.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(
        messages.endpoint.runtime_config.base_url,
        "https://opencode.ai/inference/go/anthropic/v1"
    );

    let chat = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("opencode-go@default/kimi-k2.7-code"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(chat.endpoint.provider.as_str(), "opencode-go");
    assert_eq!(chat.endpoint.endpoint.as_str(), "default");
    assert_eq!(
        chat.endpoint.runtime_config.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(
        chat.endpoint.runtime_config.base_url,
        "https://opencode.ai/inference/go/openai/v1"
    );

    for model in [
        "minimax-m3",
        "kimi-k2.7-code",
        "glm-5.3-flash",
        "future-model",
        "grok-4.7",
        "gpt-6-luna",
        "gpt-5.6-luna",
        "muse-spark-1.3-contributor",
        "muse-spark-1.2-contributor",
    ] {
        let resolved = catalog
            .resolve_model_route(
                &ContextConfig::default(),
                &ModelRef::parse(&format!("opencode-go/{model}")).unwrap(),
                ModelRouteCapability::Turn,
            )
            .unwrap();
        assert_eq!(resolved.endpoint.provider.as_str(), "opencode-go");
        assert_eq!(resolved.endpoint.endpoint.as_str(), "messages");
        assert_eq!(
            resolved.endpoint.runtime_config.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            resolved.endpoint.runtime_config.base_url,
            "https://opencode.ai/inference/go/anthropic/v1"
        );
    }
}

#[test]
fn runtime_model_catalog_uses_discovery_cache_between_overrides_and_builtins() {
    let mut fixture = catalog_fixture("openrouter/anthropic/claude-3.5-sonnet", &[]);
    let openrouter = ProviderId::parse("openrouter").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.providers.insert(
        openrouter.clone(),
        built_ins.get(&openrouter).unwrap().clone(),
    );
    let remote_model_ref = ModelRef::parse("openrouter/anthropic/claude-3.5-sonnet").unwrap();
    fixture.model_discovery_cache.providers.insert(
        holon_provider::model_discovery::ModelDiscoveryCacheKey::new(
            openrouter.clone(),
            ProviderEndpointId::default_endpoint(),
        ),
        ProviderModelDiscoveryCache {
            provider: openrouter,
            fetched_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            source_url: Some("https://openrouter.ai/api/v1/models".into()),
            response_hash: Some("sha256:test".into()),
            models: vec![BuiltInModelMetadata {
                model_ref: remote_model_ref.clone(),
                display_name: "Remote Claude".into(),
                description: "Remote discovered model.".into(),
                context_window_tokens: Some(123_456),
                effective_context_window_percent: 95,
                auto_compact_token_limit: None,
                default_max_output_tokens: Some(7_777),
                max_output_tokens_upper_limit: Some(7_777),
                default_verbosity: None,
                tool_output_truncation_estimated_tokens: None,
                capabilities: Default::default(),
                reasoning_effort_options: Vec::new(),
                source: ModelMetadataSource::RemoteDiscovered,
                endpoint: None,
            }],
        },
    );
    let unknown_fallback = ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(12_000),
        ..ModelRuntimeOverride::default()
    };
    fixture.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
    fixture.validated_unknown_model_fallback = Some(unknown_fallback);

    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let base_context = test_context();

    let remote = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(remote.source, ModelMetadataSource::RemoteDiscovered);
    assert_eq!(remote.prompt_budget_estimated_tokens, 117_283);
    assert_eq!(remote.runtime_max_output_tokens, 7_777);

    let available = catalog.available_models();
    let listed = available
        .iter()
        .find(|model| model.model_ref == remote_model_ref)
        .expect("remote model should be listed");
    assert_eq!(listed.source, ModelMetadataSource::RemoteDiscovered);
    assert_eq!(listed.display_name, remote.display_name);
    assert_eq!(listed.context_window_tokens, remote.context_window_tokens);
    assert_eq!(
        listed.default_max_output_tokens,
        Some(remote.runtime_max_output_tokens)
    );
    let route = catalog
        .resolve_explicit_model_metadata(
            &base_context,
            &route_ref("openrouter/anthropic/claude-3.5-sonnet"),
        )
        .expect("configured OpenRouter route should resolve");
    assert_eq!(route.policy, remote);

    let override_config = ModelRuntimeOverride {
        display_name: Some("Configured Claude".into()),
        prompt_budget_estimated_tokens: Some(42_000),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .validated_model_overrides
        .insert(remote_model_ref.clone(), override_config.clone());
    fixture
        .stored_config
        .models
        .catalog
        .insert(remote_model_ref.as_string(), override_config);
    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let overridden = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(overridden.source, ModelMetadataSource::ConfigOverride);
    assert_eq!(overridden.display_name, "Configured Claude");
    assert_eq!(overridden.prompt_budget_estimated_tokens, 42_000);
    let listed = catalog
        .available_models()
        .into_iter()
        .find(|model| model.model_ref == remote_model_ref)
        .expect("overridden model should remain listed");
    assert_eq!(listed.display_name, overridden.display_name);
    assert_eq!(
        listed.context_window_tokens,
        overridden.context_window_tokens
    );
}

#[test]
fn runtime_model_catalog_resolves_codex_sol_to_builtin_budget() {
    let fixture = catalog_fixture("openai-codex/gpt-6-sol", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let base_context = test_context();

    let resolved = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(resolved.model_ref.as_string(), "openai-codex/gpt-6-sol");
    assert_eq!(resolved.prompt_budget_estimated_tokens, 258_400);
    assert_eq!(resolved.compaction_trigger_estimated_tokens, 232_560);
    assert_eq!(resolved.compaction_keep_recent_estimated_tokens, 88_372);
    assert_eq!(
        resolved.source,
        holon_model_catalog::ModelMetadataSource::BuiltInCatalog
    );
}

#[test]
fn discovery_cache_keeps_sibling_endpoint_models_separate() {
    let mut fixture = catalog_fixture("opencode-go/kimi-k3.1-pro", &[]);
    let opencode = ProviderId::parse("opencode-go").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture
        .providers
        .insert(opencode.clone(), built_ins.get(&opencode).unwrap().clone());
    let model_ref = ModelRef::parse("opencode-go/kimi-k3.1-pro").unwrap();
    for (index, endpoint) in ["default", "messages"].iter().enumerate() {
        let endpoint_id = ProviderEndpointId::parse(endpoint).unwrap();
        fixture.model_discovery_cache.providers.insert(
            holon_provider::model_discovery::ModelDiscoveryCacheKey::new(
                opencode.clone(),
                endpoint_id.clone(),
            ),
            ProviderModelDiscoveryCache {
                provider: opencode.clone(),
                fetched_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                source_url: Some(format!("https://opencode.example/{endpoint}/models")),
                response_hash: Some("sha256:test".into()),
                models: vec![BuiltInModelMetadata {
                    model_ref: model_ref.clone(),
                    display_name: format!("Remote {endpoint}"),
                    description: "Remote discovered model.".into(),
                    context_window_tokens: Some(100_000 + index),
                    effective_context_window_percent: 95,
                    auto_compact_token_limit: None,
                    default_max_output_tokens: None,
                    max_output_tokens_upper_limit: None,
                    default_verbosity: None,
                    tool_output_truncation_estimated_tokens: None,
                    capabilities: Default::default(),
                    reasoning_effort_options: Vec::new(),
                    source: ModelMetadataSource::RemoteDiscovered,
                    endpoint: (endpoint_id.as_str() != ProviderEndpointId::DEFAULT)
                        .then(|| endpoint_id.clone()),
                }],
            },
        );
    }

    let catalog = RuntimeModelCatalog::from_config(&fixture);
    let context = test_context();
    let default_policy = catalog
        .resolved_model_policy_for_route(&context, &route_ref("opencode-go@default/kimi-k3.1-pro"));
    assert_eq!(default_policy.context_window_tokens, Some(100_000));
    let messages_policy = catalog.resolved_model_policy_for_route(
        &context,
        &route_ref("opencode-go@messages/kimi-k3.1-pro"),
    );
    assert_eq!(
        messages_policy.context_window_tokens,
        Some(100_001),
        "each endpoint keeps the metadata discovered from its own model list"
    );
    assert_ne!(
        default_policy.context_window_tokens,
        messages_policy.context_window_tokens
    );
}

#[test]
fn disabled_discovery_drops_cached_endpoint_models() {
    let mut fixture = catalog_fixture("opencode-go/kimi-k3.1-pro", &[]);
    let opencode = ProviderId::parse("opencode-go").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture
        .providers
        .insert(opencode.clone(), built_ins.get(&opencode).unwrap().clone());
    let model_ref = ModelRef::parse("opencode-go/kimi-k3.1-pro").unwrap();
    fixture.model_discovery_cache.providers.insert(
        holon_provider::model_discovery::ModelDiscoveryCacheKey::new(
            opencode.clone(),
            ProviderEndpointId::default_endpoint(),
        ),
        ProviderModelDiscoveryCache {
            provider: opencode.clone(),
            fetched_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            source_url: Some("https://opencode.example/v1/models".into()),
            response_hash: Some("sha256:test".into()),
            models: vec![BuiltInModelMetadata {
                model_ref,
                display_name: "Remote".into(),
                description: "Remote discovered model.".into(),
                context_window_tokens: Some(424_242),
                effective_context_window_percent: 95,
                auto_compact_token_limit: None,
                default_max_output_tokens: None,
                max_output_tokens_upper_limit: None,
                default_verbosity: None,
                tool_output_truncation_estimated_tokens: None,
                capabilities: Default::default(),
                reasoning_effort_options: Vec::new(),
                source: ModelMetadataSource::RemoteDiscovered,
                endpoint: None,
            }],
        },
    );
    let context = test_context();
    let route = route_ref("opencode-go@default/kimi-k3.1-pro");
    let enabled = RuntimeModelCatalog::from_config(&fixture);
    assert_eq!(
        enabled
            .resolved_model_policy_for_route(&context, &route)
            .context_window_tokens,
        Some(424_242),
        "an enabled endpoint serves its cached discovery metadata"
    );

    fixture.stored_config.providers.insert(
        opencode,
        ProviderConfigFile {
            endpoints: [(
                ProviderEndpointId::default_endpoint(),
                ProviderEndpointConfigFile {
                    discovery: Some(ProviderDiscoveryConfig::disabled()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    let disabled = RuntimeModelCatalog::from_config(&fixture);
    assert_ne!(
        disabled
            .resolved_model_policy_for_route(&context, &route)
            .context_window_tokens,
        Some(424_242),
        "`disabled` must hide models cached before the opt-out"
    );
}

fn vllm_provider_id() -> ProviderId {
    ProviderId::parse("vllm").unwrap()
}

/// A cache entry of the shape recorded when an unrelated service answers on a
/// credential-free built-in provider's default port.
fn fixture_with_polluted_credential_free_cache() -> CatalogFixture {
    let mut fixture = catalog_fixture("openai@default/gpt-5.4", &[]);
    let vllm = vllm_provider_id();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture
        .providers
        .insert(vllm.clone(), built_ins.get(&vllm).unwrap().clone());
    fixture.model_discovery_cache.providers.insert(
        holon_provider::model_discovery::ModelDiscoveryCacheKey::new(
            vllm.clone(),
            ProviderEndpointId::default_endpoint(),
        ),
        ProviderModelDiscoveryCache {
            provider: vllm,
            fetched_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            source_url: Some("http://127.0.0.1:8000/v1/models".into()),
            response_hash: Some("sha256:test".into()),
            models: vec![BuiltInModelMetadata {
                model_ref: ModelRef::parse("vllm/edge0-model").unwrap(),
                display_name: "Edge0".into(),
                description: "Recorded from a non-vllm service on the same port.".into(),
                context_window_tokens: Some(424_242),
                effective_context_window_percent: 95,
                auto_compact_token_limit: None,
                default_max_output_tokens: None,
                max_output_tokens_upper_limit: None,
                default_verbosity: None,
                tool_output_truncation_estimated_tokens: None,
                capabilities: Default::default(),
                reasoning_effort_options: Vec::new(),
                source: ModelMetadataSource::RemoteDiscovered,
                endpoint: None,
            }],
        },
    );
    fixture
}

#[test]
fn credential_free_provider_cached_models_require_explicit_config() {
    let route = route_ref("vllm@default/edge0-model");

    let mut fixture = fixture_with_polluted_credential_free_cache();
    let unconfigured_config = fixture.model_catalog_config();
    assert!(
        !unconfigured_config.provider_auto_discovery_allowed(
            unconfigured_config
                .providers
                .get(&vllm_provider_id())
                .unwrap()
        ),
        "automatic refresh must skip a credential-free built-in the user never opted into"
    );
    let blocked = RuntimeModelCatalog::from_config(&fixture);
    assert!(
        !blocked.discovered_models.contains_key(&route),
        "an unconfigured, unreferenced credential-free built-in must not load a polluted cache entry"
    );

    fixture.stored_config.providers.insert(
        vllm_provider_id(),
        ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "http://127.0.0.1:8000/v1".into(),
            ..Default::default()
        },
    );
    let configured_config = fixture.model_catalog_config();
    assert!(
        configured_config.provider_auto_discovery_allowed(
            configured_config
                .providers
                .get(&vllm_provider_id())
                .unwrap()
        ),
        "declaring providers.vllm must opt the provider into automatic discovery"
    );
    let configured = RuntimeModelCatalog::from_config(&fixture);
    assert!(
        configured.discovered_models.contains_key(&route),
        "declaring providers.vllm must restore cached discovery models"
    );
}

#[test]
fn credential_free_provider_cached_models_load_when_a_model_route_references_them() {
    let route = route_ref("vllm@default/edge0-model");
    let assert_loaded = |fixture: CatalogFixture| {
        let catalog = RuntimeModelCatalog::from_config(&fixture);
        assert!(
            catalog.discovered_models.contains_key(&route),
            "a configured model reference must opt the credential-free provider into discovery"
        );
    };

    let mut default_referenced = fixture_with_polluted_credential_free_cache();
    default_referenced.default_model = route.clone();
    assert_loaded(default_referenced);

    let mut fallback_referenced = fixture_with_polluted_credential_free_cache();
    fallback_referenced.fallback_models = vec![route.clone()];
    assert_loaded(fallback_referenced);

    let mut vision_referenced = fixture_with_polluted_credential_free_cache();
    vision_referenced.vision_model = Some(route.clone());
    assert_loaded(vision_referenced);

    let mut image_generation_referenced = fixture_with_polluted_credential_free_cache();
    image_generation_referenced.image_generation_model = Some(route.clone());
    assert_loaded(image_generation_referenced);

    let mut vision_candidate_referenced = fixture_with_polluted_credential_free_cache();
    vision_candidate_referenced.vision_candidate_models = vec![route.clone()];
    assert_loaded(vision_candidate_referenced);

    let mut model_override_referenced = fixture_with_polluted_credential_free_cache();
    model_override_referenced.validated_model_overrides.insert(
        ModelRef::parse("vllm/edge0-model").unwrap(),
        ModelRuntimeOverride::default(),
    );
    assert_loaded(model_override_referenced);
}
