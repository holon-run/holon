use super::*;

fn curated_catalog() -> BuiltInModelCatalog {
    // Curation-tracking tests document the compiled-in Rust catalog;
    // models.dev supplement entries are a separate reviewed data layer.
    legacy_builtin_catalog().expect("legacy catalog must be valid")
}

use holon_domain::model_values::{
    provider_definitions, ProviderCatalogPolicy, ProviderMaterializer,
};

fn base_context() -> ContextConfig {
    ContextConfig::default()
}

#[test]
fn reviewed_codex_terra_keeps_endpoint_metadata_separate() {
    let catalog = BuiltInModelCatalog::new();
    let api = catalog
        .get(&ModelRef::parse("openai/gpt-6.1-terra").unwrap())
        .expect("reviewed API model");
    assert_eq!(api.context_window_tokens, Some(1_050_000));
    assert_eq!(api.max_output_tokens_upper_limit, Some(128_000));
    assert!(catalog
        .get_route(&ModelRouteRef::parse("openai@default/gpt-6.1-terra").unwrap())
        .is_some());
    for catalog in [catalog, curated_catalog()] {
        let model_ref = ModelRef::parse("openai-codex/gpt-6.1-terra").unwrap();
        let codex = catalog.get(&model_ref).expect("reviewed Codex model");
        assert_eq!(codex.context_window_tokens, Some(500_000));
        assert_eq!(codex.default_max_output_tokens, None);
        assert_eq!(codex.max_output_tokens_upper_limit, None);
        assert!(catalog
            .get_route(&ModelRouteRef::parse("openai-codex@default/gpt-6.1-terra").unwrap())
            .is_some());
        let policy = catalog.resolve_policy(
            &model_ref,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, 475_000);
        assert_eq!(policy.runtime_max_output_tokens, 8192);
        assert_eq!(policy.verbosity, Some(ModelVerbosity::Low));
        assert!(policy.capabilities.agent_turn);
        assert!(policy.capabilities.parallel_tool_calls);
        assert!(policy.capabilities.image_input);
        assert!(policy.capabilities.interactive_exec);
        assert!(!policy.capabilities.image_generation);
        assert_eq!(
            policy.reasoning_effort_options,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert!(policy.validate_reasoning_effort("ultra").is_ok());
        assert!(policy.validate_reasoning_effort("none").is_err());
        assert_eq!(
            catalog.preferred_model_for_provider(&ProviderId::openai_codex()),
            Some(ModelRef::new(ProviderId::openai_codex(), "gpt-6-astra"))
        );
    }
    let catalog = BuiltInModelCatalog::new();
    assert!(catalog
        .get_route(&ModelRouteRef::parse("openai-codex@default/gpt-daybreak-blue-latest").unwrap())
        .is_some());
}

#[test]
fn discovery_catalog_policies_match_static_catalog_registration() {
    let catalog = BuiltInModelCatalog::new();
    for definition in provider_definitions() {
        let provider = ProviderId::parse(definition.route_provider).unwrap();
        let has_static_entry = catalog
            .entries
            .keys()
            .any(|model_ref| model_ref.provider == provider);
        match definition.catalog_policy {
            ProviderCatalogPolicy::StaticAndDiscovery => assert!(
                has_static_entry,
                "{} declares static discovery metadata without a static catalog entry",
                definition.legacy_provider
            ),
            ProviderCatalogPolicy::DiscoveryOnly => {
                assert!(definition.discovery.is_some());
                assert!(matches!(
                    definition.materializer,
                    ProviderMaterializer::Generic | ProviderMaterializer::GitHubCopilot
                ));
            }
            ProviderCatalogPolicy::StaticOnly => {}
        }
    }
}

#[test]
fn resolves_built_in_policy_from_known_model() {
    let catalog = BuiltInModelCatalog::new();
    let policy = catalog.resolve_policy(
        &ModelRef::new(ProviderId::openai_codex(), "gpt-6-sol"),
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
fn typesafe_route_declares_decision_capability_and_jev_protocol() {
    let catalog = BuiltInModelCatalog::new();
    let route_ref = ModelRouteRef::new(
        provider_id("typesafe"),
        ProviderEndpointId::parse("default").expect("valid endpoint"),
        "jev-latest",
    );
    let policy = catalog.resolve_route_policy(
        &route_ref,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert_eq!(policy.decision_protocol, Some(DecisionProtocol::Jev));
    assert!(policy.decision_capable);
    assert!(!policy.agent_turn);
    assert!(!holon_domain::model_values::ModelRouteCapability::Turn.model_supports(&policy));
    assert!(holon_domain::model_values::ModelRouteCapability::Decision.model_supports(&policy));
}

#[test]
fn retired_chatgpt_codex_models_are_not_catalog_candidates() {
    for catalog in [BuiltInModelCatalog::new(), curated_catalog()] {
        for model_id in [
            "gpt-5.2",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.5",
        ] {
            assert!(catalog
                .get(&ModelRef::new(ProviderId::openai_codex(), model_id))
                .is_none());
        }
    }
}

#[test]
fn codex_context_defaults_do_not_inherit_general_api_limits() {
    for catalog in [BuiltInModelCatalog::new(), curated_catalog()] {
        assert_eq!(
            catalog.preferred_model_for_provider(&ProviderId::openai_codex()),
            Some(ModelRef::new(ProviderId::openai_codex(), "gpt-6-astra"))
        );
        for model in [
            "gpt-6.1-sol",
            "gpt-6-luna",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ] {
            let model_ref = ModelRef::new(ProviderId::openai_codex(), model);
            let metadata = catalog.get(&model_ref).unwrap();
            let context = if model == "gpt-6.1-sol" {
                500_000
            } else {
                272_000
            };
            assert_eq!(metadata.context_window_tokens, Some(context), "{model}");
            assert_eq!(metadata.default_max_output_tokens, None, "{model}");
            assert_eq!(metadata.max_output_tokens_upper_limit, None, "{model}");
            assert_eq!(metadata.effective_context_window_percent, 95, "{model}");
            assert_eq!(metadata.source, ModelMetadataSource::BuiltInCatalog);
        }
    }
    let catalog = curated_catalog();
    for model in ["gpt-6.1-sol", "gpt-6-luna"] {
        let model_ref = ModelRef::new(ProviderId::openai_codex(), model);
        let policy = catalog.resolve_policy(
            &model_ref,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(
            policy.prompt_budget_estimated_tokens,
            if model == "gpt-6.1-sol" {
                475_000
            } else {
                258_400
            }
        );
        assert_eq!(policy.runtime_max_output_tokens, 8192);
        assert!(policy.capabilities.parallel_tool_calls);
        assert!(policy.capabilities.image_input);
        assert!(policy.capabilities.interactive_exec);
        assert!(policy.validate_reasoning_effort("max").is_ok());
        assert!(policy.validate_reasoning_effort("none").is_err());
        assert_eq!(
            policy.validate_reasoning_effort("ultra").is_ok(),
            model == "gpt-6.1-sol"
        );
        let api_catalog = BuiltInModelCatalog::new();
        let api = api_catalog
            .get(&ModelRef::new(ProviderId::openai(), model))
            .unwrap();
        assert_eq!(api.context_window_tokens, Some(1_050_000));
        assert_eq!(api.max_output_tokens_upper_limit, Some(128_000));
    }
}

#[test]
fn arcee_catalog_tracks_current_hosted_models_conservatively() {
    let catalog = curated_catalog();
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
    let catalog = curated_catalog();
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
    let catalog = curated_catalog();
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
    let catalog = curated_catalog();
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
fn deepseek_flash_responses_vision_does_not_inherit_v4_metadata() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    let model_ref = ModelRef::parse("deepseek/deepseek-flash").unwrap();
    let metadata = catalog.get(&model_ref).unwrap();
    assert!(metadata.capabilities.image_input);
    assert!(!metadata.capabilities.image_generation);
    assert!(metadata.capabilities.supports_reasoning);
    assert_eq!(metadata.context_window_tokens, Some(1_000_000));
    assert_eq!(metadata.default_max_output_tokens, None);
    assert_eq!(metadata.max_output_tokens_upper_limit, Some(384_000));
    assert_eq!(metadata.auto_compact_token_limit, None);
    let responses = ModelRouteRef::parse("deepseek@responses/deepseek-flash").unwrap();
    assert!(catalog.get_route(&responses).is_some());
    assert_eq!(
        catalog.preferred_route_for_model(&model_ref),
        Some(responses)
    );
    assert!(catalog
        .get_route(&ModelRouteRef::parse("deepseek@default/deepseek-flash").unwrap())
        .is_none());
    for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
        let metadata = catalog
            .get(&ModelRef::new(provider_id("deepseek"), model))
            .unwrap();
        assert!(!metadata.capabilities.image_input, "{model}");
    }
    for model in ["deepseek-flash-preview", "deepseek-v4.1-flash"] {
        assert!(catalog
            .get(&ModelRef::new(provider_id("deepseek"), model))
            .is_none());
    }
}

#[test]
fn deepseek_catalog_tracks_current_api_models() {
    let catalog = curated_catalog();

    for model in ["deepseek-v4-flash", "deepseek-v4-pro"] {
        let metadata = catalog
            .get(&ModelRef::new(provider_id("deepseek"), model))
            .unwrap_or_else(|| panic!("{model} should be registered"));
        assert_eq!(metadata.context_window_tokens, Some(1_000_000));
        assert_eq!(metadata.default_max_output_tokens, Some(384_000));
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(384_000));
        assert!(metadata.capabilities.supports_reasoning);
        assert!(!metadata.capabilities.image_input);
        assert_eq!(
            reasoning_effort_options(
                &metadata.model_ref,
                metadata.endpoint.as_ref(),
                &metadata.capabilities,
            ),
            ["low", "high", "max"]
        );
        let responses_route = ModelRouteRef::new(
            provider_id("deepseek"),
            ProviderEndpointId::parse("responses").unwrap(),
            model,
        );
        catalog
            .get_route(&responses_route)
            .unwrap_or_else(|| panic!("{model}@responses should be registered"));
        let responses_policy = catalog.resolve_route_policy(
            &responses_route,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(
            responses_policy.reasoning_effort_options,
            ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            catalog
                .preferred_route_for_model(&metadata.model_ref)
                .unwrap()
                .endpoint,
            ProviderEndpointId::default_endpoint()
        );
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
    assert_eq!(codex_sol.context_window_tokens, Some(272_000));
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
    let codex_astra = catalog.resolve_policy(
        &ModelRef::parse("openai-codex/gpt-6-astra").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert_eq!(codex_astra.display_name, "GPT-6-Astra (Codex)");
    assert_eq!(codex_astra.context_window_tokens, Some(272_000));
    assert_eq!(codex_astra.prompt_budget_estimated_tokens, 258_400);
    assert_eq!(codex_astra.runtime_max_output_tokens, 8192);
    assert_eq!(codex_astra.verbosity, Some(ModelVerbosity::Low));
    assert!(codex_astra.capabilities.parallel_tool_calls);
    assert!(codex_astra.capabilities.image_input);
    assert!(codex_astra.capabilities.image_generation);
    assert!(codex_astra.capabilities.interactive_exec);
    assert!(codex_astra.capabilities.supports_reasoning);
    assert_eq!(
        codex_astra.reasoning_effort_options,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(codex_astra.validate_reasoning_effort("max").is_ok());
    assert!(codex_astra
        .validate_reasoning_effort("ultra")
        .unwrap_err()
        .to_string()
        .contains("orchestration semantics"));

    let codex_sol = catalog.resolve_policy(
        &ModelRef::parse("openai-codex/gpt-6-sol").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert_eq!(codex_sol.display_name, "GPT-6-Sol (Codex)");
    assert_eq!(codex_sol.context_window_tokens, Some(272_000));
    assert_eq!(codex_sol.prompt_budget_estimated_tokens, 258_400);
    assert_eq!(codex_sol.runtime_max_output_tokens, 8192);
    assert_eq!(codex_sol.verbosity, Some(ModelVerbosity::Low));
    assert!(codex_sol.capabilities.agent_turn);
    assert!(codex_sol.capabilities.image_input);
    assert!(codex_sol.capabilities.image_generation);
    assert!(codex_sol.capabilities.interactive_exec);
    assert!(codex_sol.capabilities.supports_reasoning);
    assert_eq!(
        codex_sol.reasoning_effort_options,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(codex_sol.validate_reasoning_effort("max").is_ok());

    let openai_astra = catalog.resolve_policy(
        &ModelRef::parse("openai/gpt-6-astra").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert_eq!(openai_astra.display_name, "GPT-6 Astra");
    assert_eq!(openai_astra.context_window_tokens, Some(1_050_000));
    assert_eq!(openai_astra.runtime_max_output_tokens, 128_000);
    assert!(!openai_astra.capabilities.parallel_tool_calls);
    assert!(!openai_astra.capabilities.image_generation);
    assert!(!openai_astra.capabilities.interactive_exec);
    assert_eq!(
        openai_astra.reasoning_effort_options,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        openai_astra.source,
        ModelMetadataSource::ModelsDevSupplement
    );

    let openai_sol = catalog.resolve_policy(
        &ModelRef::parse("openai/gpt-6-sol").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert_eq!(openai_sol.display_name, "GPT-6 Sol");
    assert_eq!(openai_sol.context_window_tokens, Some(1_050_000));
    assert_eq!(openai_sol.runtime_max_output_tokens, 128_000);
    assert_eq!(
        openai_sol.reasoning_effort_options,
        ["none", "low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(openai_sol.source, ModelMetadataSource::ModelsDevSupplement);

    let codex_sol = catalog.resolve_policy(
        &ModelRef::parse("openai-codex/gpt-6-sol").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    let unsupported = codex_sol
        .validate_reasoning_effort("minimal")
        .unwrap_err()
        .to_string();
    assert!(unsupported.contains("openai-codex/gpt-6-sol"));
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
    let catalog = curated_catalog();

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

    assert!(catalog
        .get(&ModelRef::parse("xiaomi/mimo-v2-pro").unwrap())
        .is_none());
    assert!(catalog
        .get(&ModelRef::parse("xiaomi/mimo-v2.5-pro-ultraspeed").unwrap())
        .is_none());
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

    let zai_53 = catalog.resolve_policy(
        &ModelRef::parse("zai/glm-5.3").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert_eq!(zai_53.display_name, "GLM-5.3");
    assert_eq!(zai_53.context_window_tokens, Some(1_000_000));
    assert_eq!(zai_53.runtime_max_output_tokens, 131_072);
    assert_eq!(zai_53.reasoning_effort_options, ["low", "high", "max"]);
    assert_eq!(zai_53.source, ModelMetadataSource::BuiltInCatalog);

    for provider in ["zai", "bigmodel"] {
        let flash = catalog.resolve_policy(
            &ModelRef::parse(&format!("{provider}/glm-5.3-flash")).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(flash.display_name, "GLM-5.3 Flash");
        assert_eq!(flash.context_window_tokens, Some(1_000_000));
        assert_eq!(flash.runtime_max_output_tokens, 131_072);
        assert!(flash.capabilities.image_input);
        assert_eq!(flash.reasoning_effort_options, ["low", "high", "max"]);
        assert_eq!(flash.source, ModelMetadataSource::BuiltInCatalog);
    }

    assert_eq!(
        catalog
            .preferred_model_for_provider(&ProviderId::parse("zai").unwrap())
            .unwrap()
            .as_string(),
        "zai/glm-5.3"
    );
    assert_eq!(
        catalog
            .preferred_model_for_provider(&ProviderId::parse("bigmodel").unwrap())
            .unwrap()
            .as_string(),
        "bigmodel/glm-5.3"
    );

    for model in [
        "glm-5.3",
        "glm-5.3-flash",
        "glm-5.2",
        "glm-5.1",
        "glm-5",
        "glm-4.7",
        "glm-4.6",
    ] {
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
        .get(&ModelRef::parse("dashscope/qwen3.8-max-preview").unwrap())
        .is_some());
    assert!(catalog
        .get(&ModelRef::parse("dashscope/qwen3.8-max").unwrap())
        .is_some());
    assert!(catalog
        .get(&ModelRef::parse("dashscope/qwen3.8-flash").unwrap())
        .is_some());
    assert!(catalog
        .get(&ModelRef::parse("dashscope/deepseek-v4-flash-0731").unwrap())
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
        .get(&ModelRef::parse("dashscope/ZHIPU/GLM-5.3").unwrap())
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
    let catalog = curated_catalog();
    let expected = [
        ("qwen3.5-plus", 1_000_000, true, true),
        ("qwen3.8-max-preview", 1_000_000, true, true),
        ("qwen3.8-max", 1_000_000, true, true),
        ("qwen3.8-flash", 1_000_000, true, true),
        ("deepseek-v4-flash-0731", 1_000_000, true, false),
        ("qwen3-coder-next", 262_144, true, false),
        ("qwen3-coder-plus", 1_000_000, true, false),
        ("glm-5", 202_752, true, false),
        ("glm-4.7", 202_752, true, false),
        ("kimi-k2.5", 262_144, true, true),
        ("kimi-k2.6", 262_144, true, true),
        ("MiniMax/MiniMax-M3", 196_608, true, false),
    ];

    for (model, context_window, reasoning, image_input) in expected {
        let model_ref = ModelRef::parse(&format!("dashscope/{model}")).unwrap();
        let metadata = catalog.get(&model_ref).unwrap();
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
        if model == "qwen3.8-flash" {
            let policy = catalog.resolve_policy(
                &model_ref,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert_eq!(policy.runtime_max_output_tokens, 131_072);
            assert_eq!(policy.reasoning_effort_options, ["low", "medium", "xhigh"]);
        }
    }
}

#[test]
fn chutes_catalog_tracks_the_public_live_model_directory() {
    let catalog = curated_catalog();
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
fn fireworks_catalog_preserves_reviewed_chat_metadata_without_certifying_availability() {
    let catalog = curated_catalog();
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
        (
            "accounts/fireworks/models/minimax-m2p7",
            Some(196_608),
            false,
        ),
        ("accounts/fireworks/models/minimax-m3", Some(512_000), true),
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
        assert_eq!(
            metadata.max_output_tokens_upper_limit,
            (model == "accounts/fireworks/models/minimax-m3").then_some(512_000),
            "{model}"
        );
        assert!(metadata.default_max_output_tokens.is_none(), "{model}");
        assert_eq!(metadata.source, ModelMetadataSource::ConservativeBuiltin);
    }

    assert!(catalog
        .get(&ModelRef::parse("fireworks/accounts/fireworks/routers/kimi-k2p5-turbo").unwrap())
        .is_none());

    let minimax = catalog.resolve_policy(
        &ModelRef::parse("fireworks/accounts/fireworks/models/minimax-m3").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert_eq!(minimax.runtime_max_output_tokens, 8192);
    assert_eq!(minimax.max_output_tokens_upper_limit, Some(512_000));

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
    let catalog = curated_catalog();
    let expected = [
        (
            "nvidia/nemotron-3-super-120b-a12b",
            1_000_000,
            false,
            32_768,
        ),
        ("moonshotai/kimi-k2.6", 262_144, true, 65_536),
        ("minimaxai/minimax-m2.7", 204_800, false, 16_384),
        ("minimaxai/minimax-m3", 1_000_000, true, 16_384),
        ("z-ai/glm-5.2", 1_000_000, false, 32_768),
    ];

    for (model, context_window, image_input, output_ceiling) in expected {
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
        assert_eq!(
            metadata.max_output_tokens_upper_limit,
            Some(output_ceiling),
            "{model}"
        );
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
    let catalog = curated_catalog();
    let expected = [
        ("MiniMaxAI/MiniMax-M3", 524_288, true, true),
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
fn together_new_chat_models_keep_unconfirmed_token_limits_unknown() {
    for catalog in [curated_catalog(), BuiltInModelCatalog::new()] {
        for (id, effort) in [
            (
                "MiniMaxAI/MiniMax-M3.1-Flash",
                &["low", "medium", "high", "xhigh", "max"][..],
            ),
            ("deepseek-ai/DeepSeek-V4.1-Flash-1017", &[][..]),
        ] {
            let model = catalog
                .get(&ModelRef::parse(&format!("together/{id}")).unwrap())
                .unwrap();
            assert_eq!(model.context_window_tokens, None);
            assert_eq!(model.default_max_output_tokens, None);
            assert_eq!(model.max_output_tokens_upper_limit, None);
            assert!(model.capabilities.image_input);
            assert!(model.capabilities.supports_reasoning);
            assert_eq!(model.reasoning_effort_options, effort);
            for endpoint in ["messages", "responses"] {
                assert!(catalog
                    .get_route(&ModelRouteRef::parse(&format!("together@{endpoint}/{id}")).unwrap())
                    .is_none());
            }
        }
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
    let catalog = curated_catalog();
    let expected = [
        ("kilo-auto/frontier", 1_000_000, 128_000, true),
        ("kilo-auto/balanced", 1_000_000, 65_536, true),
        ("kilo-auto/efficient", 1_000_000, 65_536, true),
        ("kilo-auto/free", 256_000, 32_768, false),
        ("kilo-auto/small", 262_144, 32_768, true),
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
    let catalog = curated_catalog();
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
    let catalog = curated_catalog();
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
    let catalog = curated_catalog();
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
fn xiaomi_v26_reasoning_controls_are_route_scoped() {
    let catalog = BuiltInModelCatalog::new();
    for model in [
        "mimo-v2.6-pro",
        "mimo-v2.6-flash",
        "mimo-v2.6-pro-ultraspeed",
    ] {
        let model_ref = ModelRef::parse(&format!("xiaomi/{model}")).unwrap();
        let metadata = catalog.get(&model_ref).unwrap();
        for endpoint in ["default", "token-plan"] {
            let endpoint_id = ProviderEndpointId::parse(endpoint).unwrap();
            let route_ref = ModelRouteRef::parse(&format!("xiaomi@{endpoint}/{model}")).unwrap();
            if model == "mimo-v2.6-pro-ultraspeed" && endpoint == "token-plan" {
                assert!(catalog.get_route(&route_ref).is_none());
                assert!(reasoning_effort_options(
                    &model_ref,
                    Some(&endpoint_id),
                    &metadata.capabilities,
                )
                .is_empty());
                continue;
            }
            assert!(catalog.get_route(&route_ref).is_some());
            let policy = catalog.resolve_route_policy(
                &route_ref,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert_eq!(
                policy.reasoning_effort_options,
                ["none", "low", "medium", "high"],
                "{}",
                route_ref.as_string()
            );
            for effort in ["none", "low", "medium", "high"] {
                assert!(policy.validate_reasoning_effort(effort).is_ok());
            }
            // The API accepts more aliases, but the official client menu
            // exposes only these four values, without a service default.
            for effort in ["off", "minimal", "xhigh", "max", "ultra"] {
                assert!(policy.validate_reasoning_effort(effort).is_err());
            }
        }
        assert!(reasoning_effort_options(
            &model_ref,
            Some(&ProviderEndpointId::parse("unreviewed").unwrap()),
            &metadata.capabilities,
        )
        .is_empty());
    }
    assert!(reasoning_effort_options(
        &ModelRef::parse("tencent-tokenhub/mimo-v2.6-pro").unwrap(),
        None,
        &ModelCapabilityFlags {
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
    )
    .is_empty());
}

#[test]
fn xiaomi_ultraspeed_has_no_unconditional_token_plan_route() {
    let catalog = BuiltInModelCatalog::new();
    assert!(catalog
        .get(&ModelRef::parse("xiaomi/mimo-v2.6-pro-ultraspeed").unwrap())
        .is_some());
    assert!(catalog
        .get_route(&ModelRouteRef::parse("xiaomi@default/mimo-v2.6-pro-ultraspeed").unwrap())
        .is_some());
    assert!(catalog
        .get_route(&ModelRouteRef::parse("xiaomi@token-plan/mimo-v2.6-pro-ultraspeed").unwrap())
        .is_none());
    for model in ["mimo-v2.6-pro", "mimo-v2.6-flash"] {
        assert!(catalog
            .get_route(&ModelRouteRef::parse(&format!("xiaomi@token-plan/{model}")).unwrap())
            .is_some());
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
                .is_some_and(|policy| policy == &BuiltInModelRoutePolicy::default()),
            "{route_ref} should be registered"
        );
    }

    let default_policy = catalog.resolve_route_policy(
        &ModelRouteRef::parse("stepfun@default/step-3.7-flash").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8_192,
    );
    let plan_policy = catalog.resolve_route_policy(
        &ModelRouteRef::parse("stepfun@plan/step-3.7-flash").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8_192,
    );
    assert_eq!(plan_policy.capabilities, default_policy.capabilities);
    assert_eq!(
        plan_policy.context_window_tokens,
        default_policy.context_window_tokens
    );

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
    let catalog = curated_catalog();
    let minimax = ProviderId::parse("minimax").unwrap();
    let models = catalog
        .list()
        .into_iter()
        .filter(|entry| {
            entry.model_ref.provider == minimax
                && catalog
                    .get_route(&ModelRouteRef::new(
                        minimax.clone(),
                        ProviderEndpointId::default_endpoint(),
                        entry.model_ref.model.clone(),
                    ))
                    .is_some()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        models
            .iter()
            .map(|entry| entry.model_ref.model.as_str())
            .collect::<Vec<_>>(),
        vec!["MiniMax-M3", "MiniMax-M3.1-Flash"]
    );

    let m3 = catalog
        .get(&ModelRef::parse("minimax/MiniMax-M3").unwrap())
        .unwrap();
    assert_eq!(m3.context_window_tokens, Some(1_000_000));
    assert_eq!(m3.default_max_output_tokens, Some(131_072));
    assert_eq!(m3.max_output_tokens_upper_limit, Some(524_288));
    assert!(m3.capabilities.supports_reasoning);
    assert!(m3.capabilities.image_input);
    assert!(!m3.reasoning_effort_options.contains(&"max".into()));
}

#[test]
fn minimax_m3_api_eligibility_and_retirements_are_region_scoped() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        for provider in ["minimax", "minimax-cn"] {
            let route = ModelRouteRef::parse(&format!("{provider}@default/MiniMax-M3")).unwrap();
            assert!(catalog.get_route(&route).is_some());
            let metadata = catalog.get(&route.model_ref()).unwrap();
            assert_eq!(metadata.context_window_tokens, Some(1_000_000));
            assert_eq!(metadata.default_max_output_tokens, Some(131_072));
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(524_288));
            assert!(metadata.capabilities.image_input);
            assert!(!metadata.reasoning_effort_options.contains(&"max".into()));
            for endpoint in ["m-plan", "responses", "token-plan"] {
                let route =
                    ModelRouteRef::parse(&format!("{provider}@{endpoint}/MiniMax-M3")).unwrap();
                assert!(catalog.get_route(&route).is_none());
            }
        }
        for model in ["MiniMax-M2", "MiniMax-M2.1", "MiniMax-M2.1-highspeed"] {
            assert!(catalog
                .get(&ModelRef::new(provider_id("minimax"), model))
                .is_none());
        }
        for provider in ["minimax", "minimax-cn"] {
            for model in [
                "MiniMax-M2.5",
                "MiniMax-M2.5-highspeed",
                "MiniMax-M2.7",
                "MiniMax-M2.7-highspeed",
            ] {
                let route = ModelRouteRef::parse(&format!("{provider}@default/{model}")).unwrap();
                assert!(catalog.get_route(&route).is_none());
            }
            let preview =
                ModelRouteRef::parse(&format!("{provider}@m-plan/MiniMax-M3.1-Flash-Preview"))
                    .unwrap();
            assert!(catalog.get_route(&preview).is_some());
        }
    }
}

#[test]
fn minimax_m31_official_id_has_region_scoped_api_and_subscription_routes() {
    let catalog = curated_catalog();
    let model = "MiniMax-M3.1-Flash";
    for provider in ["minimax", "minimax-cn"] {
        let model_ref = ModelRef::parse(&format!("{provider}/{model}")).unwrap();
        let metadata = catalog.get(&model_ref).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_000_000));
        assert_eq!(metadata.default_max_output_tokens, Some(131_072));
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(524_288));
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.supports_reasoning);
        for endpoint in ["default", "m-plan"] {
            let route_ref =
                ModelRouteRef::parse(&format!("{provider}@{endpoint}/{model}")).unwrap();
            assert!(
                catalog.get_route(&route_ref).is_some(),
                "{}",
                route_ref.as_string()
            );
            let policy = catalog.resolve_route_policy(
                &route_ref,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert_eq!(policy.context_window_tokens, Some(1_000_000));
            assert_eq!(policy.max_output_tokens_upper_limit, Some(524_288));
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                assert!(policy.validate_reasoning_effort(effort).is_ok());
            }
            for effort in ["none", "off", "minimal"] {
                assert!(policy.validate_reasoning_effort(effort).is_err());
            }
        }
        for endpoint in ["responses", "m-plan-responses", "token-plan"] {
            assert!(catalog
                .get_route(
                    &ModelRouteRef::parse(&format!("{provider}@{endpoint}/{model}")).unwrap()
                )
                .is_none());
        }
        assert_eq!(
            catalog.canonicalize_model_ref(
                &ModelRef::parse(&format!("{provider}-m-plan/{model}")).unwrap()
            ),
            model_ref
        );
    }
}

#[test]
fn minimax_m31_is_available_only_on_region_scoped_m_plan_routes() {
    let catalog = curated_catalog();
    let model = "MiniMax-M3.1-Flash-Preview";
    for provider in ["minimax", "minimax-cn"] {
        let model_ref = ModelRef::parse(&format!("{provider}/{model}")).unwrap();
        let metadata = catalog.get(&model_ref).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_000_000));
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, None);
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.supports_reasoning);
        assert_eq!(
            metadata.reasoning_effort_options,
            ["low", "medium", "high", "xhigh", "max"]
        );
        let endpoint = ProviderEndpointId::parse("m-plan").unwrap();
        assert_eq!(
            reasoning_effort_options(&model_ref, Some(&endpoint), &metadata.capabilities),
            ["low", "medium", "high", "xhigh", "max"]
        );
        let route_ref = ModelRouteRef::parse(&format!("{provider}@m-plan/{model}")).unwrap();
        assert!(catalog.get_route(&route_ref).is_some());
        let policy = catalog.resolve_route_policy(
            &route_ref,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            assert!(policy.validate_reasoning_effort(effort).is_ok());
        }
        for effort in ["none", "off", "minimal"] {
            assert!(policy.validate_reasoning_effort(effort).is_err());
        }
        for endpoint in ["default", "responses", "m-plan-responses"] {
            assert!(catalog
                .get_route(
                    &ModelRouteRef::parse(&format!("{provider}@{endpoint}/{model}")).unwrap()
                )
                .is_none());
            assert!(reasoning_effort_options(
                &model_ref,
                Some(&ProviderEndpointId::parse(endpoint).unwrap()),
                &metadata.capabilities,
            )
            .is_empty());
        }
    }
}

#[test]
fn qianfan_catalog_tracks_current_v2_models_limits_and_modalities() {
    let catalog = curated_catalog();
    let qianfan = ProviderId::parse("qianfan").unwrap();
    let models = catalog
        .list()
        .into_iter()
        .filter(|entry| entry.model_ref.provider == qianfan)
        .collect::<Vec<_>>();
    let mut model_ids = models
        .iter()
        .map(|entry| entry.model_ref.model.as_str())
        .collect::<Vec<_>>();
    model_ids.sort_unstable();

    assert_eq!(
        model_ids,
        vec![
            "deepseek-v4-flash-0731",
            "deepseek-v4-pro",
            "deepseek-v4-pro-0813",
            "deepseek-v4.1-flash",
            "deepseek-v4.1-flash-1017",
            "ernie-5.0-thinking-exp",
            "ernie-5.1",
            "ernie-light-dt-1.0",
            "glm-5.1",
            "glm-5.2",
            "glm-5.3",
            "glm-5.3-flash",
            "qwen3.5-122b-a10b",
            "qwen3.5-27b",
            "qwen3.5-35b-a3b",
            "qwen3.5-397b-a17b",
        ]
    );

    let expected = [("ernie-5.1", None, 65_536, false, false)];
    for (model, context_window, max_output, reasoning, image_input) in expected {
        let metadata = catalog
            .get(&ModelRef::parse(&format!("qianfan/{model}")).unwrap())
            .unwrap();
        assert_eq!(metadata.context_window_tokens, context_window);
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(max_output));
        assert_eq!(metadata.capabilities.supports_reasoning, reasoning);
        assert_eq!(metadata.capabilities.image_input, image_input);
    }
}

#[test]
fn byteplus_catalog_does_not_infer_models_from_volcengine() {
    let catalog = BuiltInModelCatalog::new();
    for model in [
        "ark-code-latest",
        "dola-seed-2.0-pro",
        "dola-seed-2.0-lite",
        "dola-seed-2.0-code",
        "bytedance-seed-code",
        "glm-5.3-flash",
        "glm-5.2",
        "glm-5.1",
        "kimi-k2.5",
        "gpt-oss-120b",
        "deepseek-v4.1-flash",
        "deepseek-v4-flash",
        "deepseek-v4-pro",
    ] {
        let coding = ModelRouteRef::parse(&format!("byteplus@coding/{model}")).unwrap();
        assert!(
            catalog.get_route(&coding).is_some(),
            "{}",
            coding.as_string()
        );
        let api = ModelRouteRef::parse(&format!("byteplus@default/{model}")).unwrap();
        assert!(catalog.get_route(&api).is_none(), "{}", api.as_string());
        let metadata = catalog
            .get(&ModelRef::parse(&format!("byteplus/{model}")).unwrap())
            .unwrap();
        assert_eq!(metadata.context_window_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, None);
    }
    for unsupported in [
        "byteplus@coding/doubao-seed-2.1-pro",
        "byteplus@coding/doubao-seed-2-0-pro-260215",
        "byteplus@coding/Auto",
    ] {
        assert!(catalog
            .get_route(&ModelRouteRef::parse(unsupported).unwrap())
            .is_none());
    }
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
        "dashscope@token-plan/qwen3.8-max-preview",
        "dashscope@token-plan/qwen3.8-max",
        "dashscope@token-plan/qwen3.8-flash",
        "dashscope@token-plan/deepseek-v4-flash-0731",
        "dashscope@token-plan/deepseek-v4.1-flash",
        "dashscope@token-plan/deepseek-v4.1-flash-1017",
        "dashscope@token-plan/deepseek-v4-pro-0813",
        "dashscope@token-plan/kimi-k2.7-code",
        "dashscope@token-plan/glm-5.2",
        "dashscope@token-plan/glm-5.3",
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
        "dashscope@default/glm-5.3",
        "dashscope@default/deepseek-v4.1-flash",
        "dashscope@default/deepseek-v4.1-flash-1017",
        "dashscope@default/deepseek-v4-pro-0813",
        "dashscope@coding-plan/deepseek-v4.1-flash",
        "dashscope@coding-plan/deepseek-v4.1-flash-1017",
        "dashscope@coding-plan-intl/deepseek-v4.1-flash-1017",
        "dashscope@coding-plan/deepseek-v4-pro-0813",
        "dashscope@coding-plan/glm-5.2",
        "dashscope@coding-plan/glm-5.3",
        "dashscope@coding-plan/kimi-k2.7-code",
    ] {
        assert!(
            catalog
                .get_route(&ModelRouteRef::parse(unsupported).unwrap())
                .is_none(),
            "{unsupported} should not be inferred from another DashScope catalog"
        );
    }

    for (model, output, image) in [
        ("glm-5.3", 131_072, false),
        ("deepseek-v4.1-flash", 384_000, true),
        ("deepseek-v4-pro-0813", 384_000, false),
    ] {
        let metadata = catalog
            .get(&ModelRef::parse(&format!("dashscope/{model}")).unwrap())
            .unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_000_000));
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(output));
        assert_eq!(metadata.capabilities.image_input, image);
        assert!(metadata.capabilities.agent_turn);
        assert!(metadata.capabilities.supports_reasoning);
        assert!(metadata.description.contains("alibaba-token-plan-cn"));
    }
    let dated = catalog
        .get(&ModelRef::parse("dashscope/deepseek-v4.1-flash-1017").unwrap())
        .unwrap();
    assert!(dated.capabilities.supports_reasoning);
    assert!(dated.capabilities.image_input);
    assert_eq!(dated.context_window_tokens, Some(1_000_000));
    assert_eq!(dated.default_max_output_tokens, None);
    assert_eq!(dated.max_output_tokens_upper_limit, Some(384_000));
    assert!(dated.reasoning_effort_options.is_empty());
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
        true,
    );
    model_builtin.default_verbosity = Some(ModelVerbosity::High);
    let mut route_builtin = model_builtin.clone();
    route_builtin.display_name = "Route Builtin".into();
    route_builtin.context_window_tokens = Some(120_000);
    route_builtin.default_max_output_tokens = Some(4_000);
    route_builtin.max_output_tokens_upper_limit = Some(8_000);
    route_builtin.capabilities.image_input = true;
    route_builtin.reasoning_effort_options = vec!["route".into()];
    let route_builtin =
        BuiltInModelRoutePolicy::from_legacy_route_entry(&route_builtin, &model_builtin);
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

    let endpoint = ProviderEndpointId::default_endpoint();
    let resolved = catalog.resolve_metadata(
        &model_ref,
        Some(&route_builtin),
        Some(&model_builtin),
        &overrides,
        &discovered_models,
        None,
        &base_context(),
        8_192,
        Some(&endpoint),
    );
    assert_eq!(
        resolved
            .clone()
            .into_catalog_entry()
            .default_max_output_tokens,
        Some(8_000)
    );
    let policy = resolved.into_policy();

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
fn route_policy_roundtrip_preserves_inherited_cleared_and_concrete_fields() {
    let inherited = BuiltInModelRoutePolicy::default();
    let cleared = BuiltInModelRoutePolicy {
        context_window_tokens: Some(None),
        auto_compact_token_limit: Some(None),
        default_max_output_tokens: Some(None),
        max_output_tokens_upper_limit: Some(None),
        default_verbosity: Some(None),
        tool_output_truncation_estimated_tokens: Some(None),
        ..BuiltInModelRoutePolicy::default()
    };
    let concrete = BuiltInModelRoutePolicy {
        context_window_tokens: Some(Some(100_000)),
        auto_compact_token_limit: Some(Some(80_000)),
        default_max_output_tokens: Some(Some(4_096)),
        max_output_tokens_upper_limit: Some(Some(8_192)),
        default_verbosity: Some(Some(ModelVerbosity::Low)),
        tool_output_truncation_estimated_tokens: Some(Some(2_500)),
        ..BuiltInModelRoutePolicy::default()
    };
    for policy in [&inherited, &cleared, &concrete] {
        let json = serde_json::to_value(policy).unwrap();
        assert_eq!(
            serde_json::from_value::<BuiltInModelRoutePolicy>(json).unwrap(),
            *policy,
        );
    }
    let inherited_json = serde_json::to_value(inherited).unwrap();
    let cleared_json = serde_json::to_value(cleared).unwrap();
    for field in [
        "context_window_tokens",
        "auto_compact_token_limit",
        "default_max_output_tokens",
        "max_output_tokens_upper_limit",
        "default_verbosity",
        "tool_output_truncation_estimated_tokens",
    ] {
        assert!(inherited_json.get(field).is_none(), "{field}");
        assert_eq!(
            cleared_json.get(field),
            Some(&serde_json::Value::Null),
            "{field}"
        );
    }
}

#[test]
fn route_policy_cannot_enable_an_intrinsic_capability() {
    let model = catalog_model(
        "openai",
        "route-validation",
        "Route Validation",
        100_000,
        8_192,
        true,
        false,
    );
    let route = BuiltInModelRoutePolicy {
        capabilities: ModelCapabilityOverride {
            image_input: Some(true),
            ..ModelCapabilityOverride::default()
        },
        ..BuiltInModelRoutePolicy::default()
    };

    assert_eq!(
        route.validate_narrowing(&model),
        Err(
            "route policy cannot enable intrinsic capability image_input for \
             openai/route-validation"
                .into()
        )
    );
}

#[test]
fn capability_precedence_treats_discovered_false_as_unknown_but_preserves_explicit_false() {
    let catalog = BuiltInModelCatalog::new();
    let model_ref = ModelRef::new(ProviderId::openai(), "capability-matrix");
    let model_builtin = catalog_model(
        "openai",
        "capability-matrix",
        "Model",
        100_000,
        8_192,
        true,
        true,
    );
    let route_builtin = BuiltInModelRoutePolicy {
        capabilities: ModelCapabilityOverride {
            image_input: Some(false),
            ..ModelCapabilityOverride::default()
        },
        ..BuiltInModelRoutePolicy::default()
    };
    let mut discovered_true = model_builtin.clone();
    discovered_true.capabilities.image_input = true;
    discovered_true.source = ModelMetadataSource::RemoteDiscovered;
    let discovered_models = HashMap::from([(model_ref.clone(), discovered_true)]);

    let route_selected = catalog
        .resolve_metadata(
            &model_ref,
            Some(&route_builtin),
            Some(&model_builtin),
            &HashMap::new(),
            &discovered_models,
            None,
            &base_context(),
            8_192,
            Some(&ProviderEndpointId::default_endpoint()),
        )
        .into_policy();
    assert!(!route_selected.capabilities.image_input);
    assert_eq!(
        route_selected
            .evidence
            .source_for(ModelMetadataField::ImageInput),
        Some(ModelMetadataOrigin::RemoteDiscovered)
    );
    assert_eq!(
        route_selected.evidence.constraints,
        [ModelMetadataConstraint {
            field: ModelMetadataField::ImageInput,
            kind: ModelMetadataConstraintKind::Disabled,
            source: ModelMetadataConstraintSource::EndpointPolicy,
            requested: "true".into(),
            effective: "false".into(),
        }]
    );

    let mut discovered_false = model_builtin.clone();
    discovered_false.capabilities.image_input = false;
    discovered_false.source = ModelMetadataSource::RemoteDiscovered;
    let discovered_models = HashMap::from([(model_ref.clone(), discovered_false)]);
    let inherited_model_builtin = catalog
        .resolve_metadata(
            &model_ref,
            Some(&route_builtin),
            Some(&model_builtin),
            &HashMap::new(),
            &discovered_models,
            None,
            &base_context(),
            8_192,
            Some(&ProviderEndpointId::default_endpoint()),
        )
        .into_policy();
    assert!(!inherited_model_builtin.capabilities.image_input);
    assert_eq!(
        inherited_model_builtin
            .evidence
            .source_for(ModelMetadataField::ImageInput),
        Some(ModelMetadataOrigin::ModelBuiltin)
    );
    assert_eq!(
        inherited_model_builtin.evidence.constraints,
        [ModelMetadataConstraint {
            field: ModelMetadataField::ImageInput,
            kind: ModelMetadataConstraintKind::Disabled,
            source: ModelMetadataConstraintSource::EndpointPolicy,
            requested: "true".into(),
            effective: "false".into(),
        }]
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
    let explicit_false = catalog
        .resolve_metadata(
            &model_ref,
            Some(&route_builtin),
            Some(&model_builtin),
            &overrides,
            &discovered_models,
            None,
            &base_context(),
            8_192,
            Some(&ProviderEndpointId::default_endpoint()),
        )
        .into_policy();
    assert!(!explicit_false.capabilities.image_input);
    assert_eq!(
        explicit_false
            .evidence
            .source_for(ModelMetadataField::ImageInput),
        Some(ModelMetadataOrigin::ExplicitOverride)
    );
    assert!(explicit_false.evidence.constraints.is_empty());
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
fn budget_fallback_is_independent_of_partial_metadata() {
    let catalog = BuiltInModelCatalog::new();
    let mut model = catalog_model(
        "openai",
        "budget-model",
        "Budget model",
        128_000,
        8_192,
        false,
        false,
    );
    model.context_window_tokens = None;
    model.default_max_output_tokens = None;
    model.max_output_tokens_upper_limit = None;
    let model_ref = model.model_ref.clone();
    let mut discovered = model.clone();
    discovered.source = ModelMetadataSource::RemoteDiscovered;
    let discovered_models = HashMap::from([(model_ref.clone(), discovered)]);
    let overrides = HashMap::new();

    for (builtin, discovery, expected_source) in [
        (None, HashMap::new(), ModelMetadataSource::UnknownFallback),
        (
            Some(&model),
            HashMap::new(),
            ModelMetadataSource::BuiltInCatalog,
        ),
        (
            None,
            discovered_models,
            ModelMetadataSource::RemoteDiscovered,
        ),
    ] {
        let resolved = catalog.resolve_metadata(
            &model_ref,
            None,
            builtin,
            &overrides,
            &discovery,
            None,
            &base_context(),
            8_192,
            None,
        );
        let catalog_entry = resolved.clone().into_catalog_entry();
        let policy = resolved.into_policy();
        assert_eq!(policy.context_window_tokens, None);
        assert_eq!(catalog_entry.context_window_tokens, None);
        assert_eq!(policy.prompt_budget_estimated_tokens, 128_000);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 115_200);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 43_776);
        assert_eq!(policy.source, expected_source);
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::PromptBudgetEstimatedTokens),
            Some(ModelMetadataOrigin::RuntimeDefault),
        );
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::CompactionTriggerEstimatedTokens),
            Some(ModelMetadataOrigin::Derived),
        );
    }
}

#[test]
fn budget_partial_metadata_keeps_independently_known_context() {
    let catalog = BuiltInModelCatalog::new();
    for discovered_has_context in [false, true] {
        let mut builtin = catalog_model(
            "openai",
            "budget-model",
            "Budget model",
            8_000,
            1_000,
            false,
            false,
        );
        let mut discovered = builtin.clone();
        discovered.source = ModelMetadataSource::RemoteDiscovered;
        if discovered_has_context {
            builtin.context_window_tokens = None;
        } else {
            discovered.context_window_tokens = None;
        }
        let discovery = HashMap::from([(builtin.model_ref.clone(), discovered)]);
        let fallback = ModelRuntimeOverride {
            prompt_budget_estimated_tokens: Some(128_000),
            ..ModelRuntimeOverride::default()
        };
        let policy = catalog
            .resolve_metadata(
                &builtin.model_ref,
                None,
                Some(&builtin),
                &HashMap::new(),
                &discovery,
                Some(&fallback),
                &base_context(),
                1_000,
                None,
            )
            .into_policy();
        assert_eq!(policy.context_window_tokens, Some(8_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, 7_600);
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::ContextWindowTokens),
            Some(if discovered_has_context {
                ModelMetadataOrigin::RemoteDiscovered
            } else {
                ModelMetadataOrigin::ModelBuiltin
            }),
        );
    }
}

#[test]
fn budget_fallback_does_not_reinstate_context_cleared_by_an_endpoint() {
    let catalog = BuiltInModelCatalog::new();
    let model = catalog_model(
        "openai",
        "budget-model",
        "Budget model",
        256_000,
        8_192,
        false,
        false,
    );
    let route = BuiltInModelRoutePolicy {
        context_window_tokens: Some(None),
        ..BuiltInModelRoutePolicy::default()
    };
    let fallback = ModelRuntimeOverride {
        context_window_tokens: Some(200_000),
        prompt_budget_estimated_tokens: Some(64_000),
        ..ModelRuntimeOverride::default()
    };
    let overrides = HashMap::new();
    let discovered_models = HashMap::new();
    let resolved = catalog.resolve_metadata(
        &model.model_ref,
        Some(&route),
        Some(&model),
        &overrides,
        &discovered_models,
        Some(&fallback),
        &base_context(),
        8_192,
        None,
    );
    let catalog_entry = resolved.clone().into_catalog_entry();
    let policy = resolved.into_policy();
    assert_eq!(policy.context_window_tokens, None);
    assert_eq!(catalog_entry.context_window_tokens, None);
    assert_eq!(policy.prompt_budget_estimated_tokens, 64_000);
}

#[test]
fn budget_fallback_preserves_explicit_small_and_zero_runtime_budgets() {
    let catalog = BuiltInModelCatalog::new();
    for budget in [0, 512, 4_096] {
        let base = ContextConfig {
            prompt_budget_estimated_tokens: budget,
            ..base_context()
        };
        let policy = catalog.resolve_policy(
            &ModelRef::parse("openai/budget-model").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base,
            8_192,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, budget);
        assert_eq!(
            policy.compaction_trigger_estimated_tokens,
            default_compaction_trigger_estimated_tokens(budget),
        );
        assert_eq!(
            policy.compaction_keep_recent_estimated_tokens,
            default_compaction_keep_recent_estimated_tokens(
                policy.compaction_trigger_estimated_tokens,
            ),
        );
    }
}

#[test]
fn budget_fallback_overrides_are_fieldwise_and_do_not_override_known_context() {
    let catalog = BuiltInModelCatalog::new();
    let mut model = catalog_model(
        "openai",
        "budget-model",
        "Budget model",
        8_000,
        1_000,
        false,
        false,
    );
    let fallback = ModelRuntimeOverride {
        context_window_tokens: Some(16_000),
        prompt_budget_estimated_tokens: Some(4_000),
        compaction_trigger_estimated_tokens: Some(3_000),
        ..ModelRuntimeOverride::default()
    };
    for (window, budget, trigger, origin) in [
        (Some(8_000), 7_600, 6_840, ModelMetadataOrigin::Derived),
        (None, 4_000, 3_000, ModelMetadataOrigin::UnknownFallback),
    ] {
        model.context_window_tokens = window;
        let policy = catalog
            .resolve_metadata(
                &model.model_ref,
                None,
                Some(&model),
                &HashMap::new(),
                &HashMap::new(),
                Some(&fallback),
                &base_context(),
                1_000,
                None,
            )
            .into_policy();
        assert_eq!(policy.context_window_tokens, window.or(Some(16_000)));
        assert_eq!(policy.prompt_budget_estimated_tokens, budget);
        assert_eq!(policy.compaction_trigger_estimated_tokens, trigger);
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::PromptBudgetEstimatedTokens),
            Some(origin),
        );
    }
}

#[test]
fn budget_environment_overrides_keep_model_and_fallback_precedence() {
    let catalog = BuiltInModelCatalog::new();
    let model_ref = ModelRef::parse("openai/budget-model").unwrap();
    let base = ContextConfig {
        budget_overrides: holon_domain::ContextBudgetOverrides {
            prompt_budget_estimated_tokens: Some(64_000),
            compaction_trigger_estimated_tokens: Some(30_000),
            compaction_keep_recent_estimated_tokens: Some(10_000),
        },
        ..base_context()
    };
    let fallback = ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(48_000),
        ..ModelRuntimeOverride::default()
    };
    let overrides = HashMap::from([(
        model_ref.clone(),
        ModelRuntimeOverride {
            prompt_budget_estimated_tokens: Some(32_000),
            compaction_trigger_estimated_tokens: Some(28_000),
            ..ModelRuntimeOverride::default()
        },
    )]);
    for (overrides, fallback, budget, trigger, origin) in [
        (
            HashMap::new(),
            None,
            64_000,
            30_000,
            ModelMetadataOrigin::EnvironmentOverride,
        ),
        (
            HashMap::new(),
            Some(&fallback),
            48_000,
            30_000,
            ModelMetadataOrigin::UnknownFallback,
        ),
        (
            overrides,
            Some(&fallback),
            32_000,
            28_000,
            ModelMetadataOrigin::ExplicitOverride,
        ),
    ] {
        let policy = catalog.resolve_policy(
            &model_ref,
            &overrides,
            &HashMap::new(),
            fallback,
            &base,
            8_192,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, budget);
        assert_eq!(policy.compaction_trigger_estimated_tokens, trigger);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 10_000);
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::PromptBudgetEstimatedTokens),
            Some(origin),
        );
    }
}

#[test]
fn budget_limits_narrow_without_inflating_small_windows_or_overrides() {
    let catalog = BuiltInModelCatalog::new();
    let model = catalog_model(
        "openai",
        "budget-model",
        "Budget model",
        1_000,
        100,
        false,
        false,
    );
    for (requested, expected) in [(128_000, 1_000), (512, 512), (0, 0)] {
        let base = ContextConfig {
            budget_overrides: holon_domain::ContextBudgetOverrides {
                prompt_budget_estimated_tokens: Some(requested),
                compaction_trigger_estimated_tokens: Some(5_000),
                compaction_keep_recent_estimated_tokens: Some(2_000),
            },
            ..base_context()
        };
        let policy = catalog
            .resolve_metadata(
                &model.model_ref,
                None,
                Some(&model),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base,
                100,
                None,
            )
            .into_policy();
        assert_eq!(policy.context_window_tokens, Some(1_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, expected);
        assert_eq!(policy.compaction_trigger_estimated_tokens, expected);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, expected);
        assert!(policy.evidence.constraints.iter().any(|constraint| {
            constraint.field == ModelMetadataField::CompactionTriggerEstimatedTokens
                && constraint.source == ModelMetadataConstraintSource::RuntimeBudget
        }));
    }
}

#[test]
fn budget_input_policy_does_not_subtract_the_output_allowance_again() {
    let catalog = BuiltInModelCatalog::new();
    let mut model = catalog_model(
        "openai",
        "budget-model",
        "Budget model",
        100_000,
        8_192,
        false,
        false,
    );
    model.default_max_output_tokens = None;
    model.max_output_tokens_upper_limit = None;
    for output in [1_000, 8_192] {
        let policy = catalog.resolve_policy(
            &ModelRef::parse("openai/budget-model").unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            output,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, 128_000);
        assert_eq!(policy.runtime_max_output_tokens, output);
        assert_eq!(policy.context_window_tokens, None);
        let known = catalog
            .resolve_metadata(
                &model.model_ref,
                None,
                Some(&model),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                output,
                None,
            )
            .into_policy();
        assert_eq!(known.prompt_budget_estimated_tokens, 95_000);
        assert_eq!(known.runtime_max_output_tokens, output);
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

    for route_ref in ["volcengine@plan/glm-5.2", "volcengine@plan/glm-5.3"] {
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
fn retired_xiaomi_chat_ids_do_not_remove_aggregator_or_ultraspeed_models() {
    for catalog in [curated_catalog(), BuiltInModelCatalog::new()] {
        for id in ["mimo-v2.5-pro", "mimo-v2.5"] {
            assert!(catalog
                .get(&ModelRef::new(provider_id("xiaomi"), id))
                .is_none());
            for endpoint in ["default", "token-plan"] {
                let route = ModelRouteRef::parse(&format!("xiaomi@{endpoint}/{id}")).unwrap();
                assert!(catalog.get_route(&route).is_none(), "{}", route.as_string());
            }
        }
        assert!(catalog
            .get(&ModelRef::parse("tencent-tokenhub/mimo-v2.5-pro").unwrap())
            // Tencent TokenHub retired its own aggregator copy on
            // 2026-10-20 (announcement 2494); the surviving mimo-v2.6
            // aggregator entries prove the Xiaomi retirements did not
            // cascade to the aggregator.
            .is_none());
        assert!(catalog
            .get(&ModelRef::parse("tencent-tokenhub/mimo-v2.6-pro").unwrap())
            .is_some());
        for (provider, endpoint) in [("xiaomi", "default"), ("xiaomi-token-plan", "token-plan")] {
            assert_eq!(
                catalog.preferred_route_for_provider(&provider_id(provider)),
                Some(ModelRouteRef::parse(&format!("xiaomi@{endpoint}/mimo-v2.6-pro")).unwrap())
            );
        }
    }
    let catalog = BuiltInModelCatalog::new();
    assert!(catalog
        .get(&ModelRef::parse("xiaomi/mimo-v2.5-pro-ultraspeed").unwrap())
        .is_some());
}

#[test]
fn volcengine_economy_is_subscription_only_with_unknown_exact_limits() {
    let catalog = BuiltInModelCatalog::new();
    for endpoint in ["coding", "plan"] {
        let route = ModelRouteRef::parse(&format!(
            "volcengine@{endpoint}/deepseek-v4.1-flash-economy"
        ))
        .unwrap();
        assert!(catalog.get_route(&route).is_some());
        let policy = catalog.resolve_route_policy(
            &route,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert!(policy.capabilities.image_input);
        assert!(policy.capabilities.supports_reasoning);
        assert_eq!(policy.context_window_tokens, None);
        assert_eq!(policy.max_output_tokens_upper_limit, None);
        assert_eq!(policy.runtime_max_output_tokens, 8192);
        assert!(policy.reasoning_effort_options.is_empty());
    }
    assert!(catalog
        .get_route(&ModelRouteRef::parse("volcengine@default/deepseek-v4.1-flash-economy").unwrap())
        .is_none());
}

#[test]
fn newly_reviewed_subscription_and_messages_routes_do_not_inherit_limits() {
    for catalog in [curated_catalog(), BuiltInModelCatalog::new()] {
        for endpoint in ["coding", "plan"] {
            let route =
                ModelRouteRef::parse(&format!("volcengine@{endpoint}/kimi-k3.1-pro")).unwrap();
            assert!(catalog.get_route(&route).is_some());
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8192,
            );
            assert_eq!(policy.context_window_tokens, None);
            assert_eq!(policy.max_output_tokens_upper_limit, None);
            assert_eq!(policy.runtime_max_output_tokens, 8192);
            assert!(policy.reasoning_effort_options.is_empty());
        }
        assert!(catalog
            .get_route(&ModelRouteRef::parse("volcengine@default/kimi-k3.1-pro").unwrap())
            .is_none());
        for id in [
            "qwen3.8-2.4t-a95b",
            "qwen3.8-27b",
            "qwen3.8-max-0902",
            "qwen-plus-latest",
            "qwen-flash-2025-07-28",
            "qwen3.5-flash-2026-02-23",
            "qwen3.5-plus-2026-02-15",
            "qwen3.5-plus-2026-04-20",
            "qwen3.6-flash-2026-04-16",
            "qwen3.6-plus-2026-04-02",
            "qwen3.7-flash-2026-07-15",
        ] {
            let model = catalog
                .get(&ModelRef::parse(&format!("dashscope/{id}")).unwrap())
                .unwrap();
            let has_model_card = id.starts_with("qwen3.8-");
            assert_eq!(model.context_window_tokens, Some(1_000_000), "{id}");
            assert_eq!(
                model.default_max_output_tokens,
                has_model_card.then_some(131_072),
                "{id}"
            );
            assert_eq!(
                model.max_output_tokens_upper_limit,
                Some(match id {
                    "qwen-plus-latest" | "qwen-flash-2025-07-28" => 32_768,
                    "qwen3.5-flash-2026-02-23"
                    | "qwen3.5-plus-2026-02-15"
                    | "qwen3.5-plus-2026-04-20"
                    | "qwen3.6-flash-2026-04-16"
                    | "qwen3.6-plus-2026-04-02" => 65_536,
                    _ => 131_072,
                }),
                "{id}"
            );
            assert_eq!(
                model.capabilities.supports_reasoning, has_model_card,
                "{id}"
            );
            assert_eq!(
                model.capabilities.image_input,
                matches!(id, "qwen3.8-27b" | "qwen3.8-max-0902"),
                "{id}"
            );
            assert!(model.reasoning_effort_options.is_empty(), "{id}");
            for endpoint in ["token-plan", "coding-plan", "coding-plan-intl"] {
                assert!(catalog
                    .get_route(
                        &ModelRouteRef::parse(&format!("dashscope@{endpoint}/{id}")).unwrap()
                    )
                    .is_none());
            }
        }
    }
}

#[test]
fn volcengine_reasoning_effort_options_follow_route_contract() {
    let catalog = BuiltInModelCatalog::new();

    for route_ref in [
        "volcengine@default/doubao-seed-2-0-pro-260215",
        "volcengine@coding/glm-5.2",
        "volcengine@coding/glm-5.3",
        "volcengine@plan/glm-5.2",
        "volcengine@plan/glm-5.3",
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
        "volcengine@coding/kimi-k3.1-pro",
        "volcengine@plan/kimi-k3.1-pro",
    ] {
        let route = ModelRouteRef::parse(route_ref).unwrap();
        assert!(catalog.get_route(&route).is_some(), "{route_ref}");
        let policy = catalog.resolve_route_policy(
            &route,
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
fn volcengine_doubao_routes_inherit_canonical_intrinsic_capabilities() {
    let catalog = BuiltInModelCatalog::new();

    for endpoint in ["default", "coding", "plan"] {
        let pro = catalog.resolve_route_policy(
            &ModelRouteRef::parse(&format!("volcengine@{endpoint}/doubao-seed-2-0-pro-260215"))
                .unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert!(pro.capabilities.supports_reasoning, "{endpoint}");
        assert!(pro.capabilities.image_input, "{endpoint}");

        let code = catalog.resolve_route_policy(
            &ModelRouteRef::parse(&format!(
                "volcengine@{endpoint}/doubao-seed-2-0-code-preview-260215"
            ))
            .unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert!(code.capabilities.image_input, "{endpoint}");
    }
}

#[test]
fn moonshot_catalog_tracks_current_models_and_retirements() {
    let catalog = curated_catalog();
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
            "kimi-k2.6",
            "kimi-k2.7-code",
            "kimi-k2.7-code-highspeed",
            "kimi-k3",
        ]
    );

    for model_ref in [
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

    let kimi_k3 = catalog.resolve_policy(
        &ModelRef::parse("moonshot/kimi-k3").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &base_context(),
        8192,
    );
    assert!(kimi_k3.capabilities.supports_reasoning, "moonshot/kimi-k3");
    assert!(kimi_k3.capabilities.image_input, "moonshot/kimi-k3");
    assert_eq!(
        kimi_k3.reasoning_effort_options,
        ["low", "high", "max"],
        "moonshot/kimi-k3 reasoning effort"
    );

    for model_ref in [
        "moonshot/kimi-k2.5",
        "moonshot/moonshot-v1-8k",
        "moonshot/moonshot-v1-32k",
        "moonshot/moonshot-v1-128k",
        "moonshot/moonshot-v1-auto",
        "moonshot/moonshot-v1-8k-vision-preview",
        "moonshot/moonshot-v1-32k-vision-preview",
        "moonshot/moonshot-v1-128k-vision-preview",
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
    let catalog = curated_catalog();
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
fn moonshot_k3_family_resolves_documented_default_and_explicit_override() {
    let catalog = BuiltInModelCatalog::new();
    for model in ["kimi-k3", "kimi-k3.1-pro", "kimi-k3.1-pro-highspeed"] {
        let route = ModelRouteRef::parse(&format!("moonshot@default/{model}")).unwrap();
        let policy = catalog.resolve_route_policy(
            &route,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.context_window_tokens, Some(1_048_576));
        assert_eq!(policy.runtime_max_output_tokens, 131_072);
        assert_eq!(policy.max_output_tokens_upper_limit, Some(1_048_576));
        assert_eq!(policy.reasoning_effort_options, ["low", "high", "max"]);
        let overrides = HashMap::from([(
            ModelRef::new(provider_id("moonshot"), model),
            ModelRuntimeOverride {
                runtime_max_output_tokens: Some(65_536),
                ..Default::default()
            },
        )]);
        let overridden = catalog.resolve_route_policy(
            &route,
            &overrides,
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(overridden.runtime_max_output_tokens, 65_536);
        for endpoint in ["code-plan", "coding-plan"] {
            assert!(catalog
                .get_route(&ModelRouteRef::parse(&format!("moonshot@{endpoint}/{model}")).unwrap())
                .is_none());
        }
    }
}

#[test]
fn opencode_iceman_preserves_training_warning_with_native_metadata() {
    for catalog in [curated_catalog(), BuiltInModelCatalog::new()] {
        let model_ref = ModelRef::parse("opencode-go/iceman-contributor-free").unwrap();
        let model = catalog.get(&model_ref).unwrap();
        assert_eq!(model.context_window_tokens, Some(262_144));
        assert_eq!(model.default_max_output_tokens, None);
        assert_eq!(model.max_output_tokens_upper_limit, Some(131_072));
        assert_eq!(
            model.capabilities,
            ModelCapabilityFlags {
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            }
        );
        assert!(model.reasoning_effort_options.is_empty());
        assert!(model.description.contains("not ZDR"));
        assert_eq!(
            catalog
                .preferred_route_for_model(&model_ref)
                .unwrap()
                .endpoint
                .as_str(),
            "messages"
        );
        for endpoint in ["messages", "responses"] {
            assert!(catalog
                .get_route(
                    &ModelRouteRef::parse(&format!(
                        "opencode-go@{endpoint}/iceman-contributor-free"
                    ))
                    .unwrap()
                )
                .is_some());
        }
    }
}

#[test]
fn opencode_go_catalog_tracks_the_current_three_transport_model_table() {
    let catalog = curated_catalog();
    let provider = provider_id("opencode-go");
    let models = catalog
        .list()
        .into_iter()
        .filter(|model| model.model_ref.provider == provider)
        .collect::<Vec<_>>();

    assert_eq!(models.len(), 45);
    for model in &models {
        assert_eq!(
            catalog
                .preferred_route_for_model(&model.model_ref)
                .unwrap()
                .endpoint
                .as_str(),
            "messages",
            "{}",
            model.model_ref.as_string()
        );
        assert_eq!(
            model.capabilities.supports_reasoning,
            !matches!(model.model_ref.model.as_str(), "gemini-4-argon")
        );
        assert_eq!(model.source, ModelMetadataSource::ConservativeBuiltin);
    }
    assert_eq!(
        models
            .iter()
            .filter(|model| !model.reasoning_effort_options.is_empty())
            .count(),
        22
    );

    for model in [
        "iceman-contributor-free",
        "deepseek-v4.1-flash",
        "deepseek-v4-pro",
        "deepseek-v4-flash",
        "glm-5.3",
        "glm-5.3-flash",
        "glm-5.1",
        "kimi-k3",
        "kimi-k3.1-pro",
        "kimi-k2.7-code",
        "kimi-k2.6",
        "longcat-2.0",
        "longcat-2.5-preview-free",
        "mimo-v2.6-pro",
        "mimo-v2.6-flash",
        "mimo-v2.5-pro",
        "mimo-v2.5",
        "hy4",
        "hy4-preview",
        "hy3",
        "space-bunny",
        "step-5-preview-free",
        "cat-jam-free",
        "step-5-preview",
        "beam-free",
        "minimax-m3.1-flash",
        "minimax-m3.1-flash-free",
    ] {
        assert_eq!(
            catalog
                .preferred_routes_by_model
                .get(&ModelRef::new(provider.clone(), model))
                .unwrap()
                .endpoint
                .as_str(),
            "default",
            "{model}"
        );
    }
    for model in [
        "claude-haiku-5-5",
        "minimax-m3",
        "minimax-m2.7",
        "minimax-m2.5",
        "qwen3.8-max",
        "qwen3.8-flash",
        "qwen3.7-max",
        "qwen3.7-plus",
        "qwen3.6-plus",
    ] {
        assert_eq!(
            catalog
                .preferred_routes_by_model
                .get(&ModelRef::new(provider.clone(), model))
                .unwrap()
                .endpoint
                .as_str(),
            "messages",
            "{model}"
        );
    }
    for model in [
        "grok-4.7",
        "gpt-6-luna",
        "gpt-5.6-luna",
        "muse-spark-1.3-contributor",
        "muse-spark-1.2-contributor",
    ] {
        assert_eq!(
            catalog
                .preferred_routes_by_model
                .get(&ModelRef::new(provider.clone(), model))
                .unwrap()
                .endpoint
                .as_str(),
            "responses",
            "{model}"
        );
    }
    for model in ["grok-4.6", "gemini-4-argon-contributor"] {
        assert!(catalog
            .get(&ModelRef::new(provider.clone(), model))
            .is_none());
    }
    for endpoint in ["messages", "responses"] {
        assert!(catalog
            .get_route(
                &ModelRouteRef::parse(&format!("opencode-go@{endpoint}/minimax-m3.1-flash"))
                    .unwrap()
            )
            .is_some());
        assert!(catalog
            .get_route(
                &ModelRouteRef::parse(&format!("opencode-go@{endpoint}/minimax-m3.1-flash-free"))
                    .unwrap()
            )
            .is_none());
    }
}

#[test]
fn opencode_go_preserves_historical_ids_without_aliasing_current_ids() {
    let catalog = curated_catalog();
    let provider = provider_id("opencode-go");
    for (id, older_id) in [
        ("step-5-preview", "step-5-preview-free"),
        ("beam-free", "cat-jam-free"),
        ("minimax-m3.1-flash", "minimax-m3.1-flash-free"),
    ] {
        let model_ref = ModelRef::new(provider.clone(), id);
        let metadata = catalog.get(&model_ref).unwrap();
        assert_eq!(metadata.model_ref, model_ref);
        assert_eq!(catalog.canonicalize_model_ref(&model_ref), model_ref);

        // Missing from today's public list is not retirement evidence.
        let older_ref = ModelRef::new(provider.clone(), older_id);
        assert!(catalog.get(&older_ref).is_some(), "{older_id}");
        assert_eq!(catalog.canonicalize_model_ref(&older_ref), older_ref);
        assert_ne!(older_ref, model_ref);
    }
}

#[test]
fn opencode_go_retired_glm_52_routes_stay_absent_without_touching_other_glm_endpoints() {
    for catalog in [curated_catalog(), BuiltInModelCatalog::new()] {
        for endpoint in ["default", "messages", "responses"] {
            let route = ModelRouteRef::parse(&format!("opencode-go@{endpoint}/glm-5.2")).unwrap();
            assert!(
                catalog.get_route(&route).is_none(),
                "retired OpenCode Go glm-5.2 route {} must stay absent",
                route.as_string()
            );
        }
        // Current OpenCode Go GLM service ids remain routed.
        for endpoint in ["default", "messages", "responses"] {
            let route = ModelRouteRef::parse(&format!("opencode-go@{endpoint}/glm-5.3")).unwrap();
            assert!(catalog.get_route(&route).is_some(), "{endpoint}");
        }
        // The retirement is endpoint-specific: the native GLM providers and the
        // other gateways keep their own live glm-5.2 evidence and routes.
        for route_ref in [
            "zai@default/glm-5.2",
            "bigmodel@default/glm-5.2",
            "dashscope@default/glm-5.2",
            "volcengine@plan/glm-5.2",
            "qianfan@default/glm-5.2",
        ] {
            let route = ModelRouteRef::parse(route_ref).unwrap();
            assert!(
                catalog.get_route(&route).is_some(),
                "{} keeps its independently reviewed glm-5.2 route",
                route.as_string()
            );
        }
    }
}

#[test]
fn opencode_go_k31_supports_translation_without_inheriting_native_specs() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        let route = ModelRouteRef::parse("opencode-go@default/kimi-k3.1-pro").unwrap();
        assert!(catalog.get_route(&route).is_some());
        let metadata = catalog.get(&route.model_ref()).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_048_576));
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(1_048_576));
        assert_eq!(metadata.auto_compact_token_limit, None);
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.supports_reasoning);
        assert_eq!(metadata.reasoning_effort_options, ["low", "high", "max"]);
        for endpoint in ["messages", "responses"] {
            assert!(catalog
                .get_route(
                    &ModelRouteRef::parse(&format!("opencode-go@{endpoint}/kimi-k3.1-pro"))
                        .unwrap()
                )
                .is_some());
        }
    }
}

#[test]
fn opencode_go_current_text_models_have_conservative_translated_routes() {
    let ids = [
        "grok-4.7",
        "gpt-6-luna",
        "gpt-5.6-luna",
        "claude-haiku-5-5",
        "gemini-4-argon",
        "glm-5.3-flash",
        "glm-5.3",
        "kimi-k3.1-pro",
        "kimi-k3",
        "kimi-k2.7-code",
        "longcat-2.0",
        "deepseek-v4.1-flash",
        "deepseek-v4-pro",
        "mimo-v2.6-flash",
        "mimo-v2.6-pro",
        "mimo-v2.5",
        "mimo-v2.5-pro",
        "minimax-m3.1-flash",
        "minimax-m3",
        "muse-spark-1.3-contributor",
        "muse-spark-1.2-contributor",
        "qwen3.8-max",
        "qwen3.8-flash",
        "qwen3.7-plus",
        "hy4-preview",
        "hy3",
        "step-5-preview",
        "beam-free",
        "iceman-contributor-free",
    ];
    for catalog in [curated_catalog(), BuiltInModelCatalog::new()] {
        for id in ids {
            let model = ModelRef::new(provider_id("opencode-go"), id);
            let preferred = catalog.preferred_route_for_model(&model).unwrap();
            let native = match id {
                "grok-4.7"
                | "gpt-6-luna"
                | "gpt-5.6-luna"
                | "muse-spark-1.3-contributor"
                | "muse-spark-1.2-contributor" => "responses",
                "claude-haiku-5-5" | "minimax-m3" | "qwen3.8-max" | "qwen3.8-flash"
                | "qwen3.7-plus" => "messages",
                _ => "default",
            };
            assert_eq!(preferred.endpoint.as_str(), "messages", "{id}");
            for endpoint in ["default", "messages", "responses"] {
                let route = ModelRouteRef::parse(&format!("opencode-go@{endpoint}/{id}")).unwrap();
                let route_entry = catalog.get_route(&route).expect("reviewed Go text route");
                if endpoint == native {
                    continue;
                }
                assert_eq!(route_entry.context_window_tokens, Some(None));
                assert_eq!(route_entry.default_max_output_tokens, Some(None));
                assert_eq!(route_entry.max_output_tokens_upper_limit, Some(None));
                assert_eq!(route_entry.default_verbosity, Some(None));
                assert_eq!(
                    route_entry
                        .cache_capabilities
                        .as_ref()
                        .unwrap()
                        .cache_control,
                    Some(false),
                );
                let policy = catalog.resolve_route_policy(
                    &route,
                    &HashMap::new(),
                    &HashMap::new(),
                    None,
                    &base_context(),
                    8192,
                );
                assert_eq!(policy.context_window_tokens, None, "{}", route.as_string());
                assert_eq!(policy.max_output_tokens_upper_limit, None);
                assert_eq!(policy.runtime_max_output_tokens, 8192);
                assert!(!policy.capabilities.image_input);
                assert!(!policy.capabilities.image_generation);
                assert!(!policy.capabilities.supports_reasoning);
                assert!(!policy.capabilities.parallel_tool_calls);
                assert!(policy.reasoning_effort_options.is_empty());
            }
        }
    }
}

#[test]
fn opencode_go_new_metadata_uses_advisory_go_specific_models_dev_limits() {
    let catalog = curated_catalog();
    for (id, context, output, image_input) in [
        ("grok-4.7", 500_000, 500_000, true),
        ("gpt-6-luna", 1_050_000, 128_000, true),
        ("gpt-5.6-luna", 1_050_000, 128_000, true),
        ("claude-haiku-5-5", 1_000_000, 128_000, true),
        ("glm-5.3-flash", 1_000_000, 131_072, true),
        ("glm-5.3", 1_000_000, 131_072, false),
        ("longcat-2.0", 1_000_000, 131_072, false),
        ("deepseek-v4.1-flash", 1_000_000, 384_000, true),
        ("mimo-v2.6-flash", 1_048_576, 131_072, true),
        ("mimo-v2.6-pro", 1_048_576, 131_072, true),
        ("muse-spark-1.3-contributor", 1_048_576, 131_072, true),
        ("muse-spark-1.2-contributor", 1_048_576, 131_072, true),
        ("qwen3.8-max", 1_000_000, 131_072, true),
        ("qwen3.8-flash", 1_000_000, 131_072, true),
        ("hy4-preview", 1_024_000, 64_000, false),
        ("hy3", 256_000, 128_000, false),
        ("space-bunny", 1_048_576, 524_288, true),
        ("longcat-2.5-preview-free", 1_000_000, 131_072, true),
        ("step-5-preview-free", 1_000_000, 65_536, true),
        ("cat-jam-free", 262_144, 131_072, false),
        ("step-5-preview", 1_000_000, 65_536, true),
        ("beam-free", 1_048_576, 131_072, false),
        ("minimax-m3.1-flash", 1_000_000, 512_000, true),
        ("minimax-m3.1-flash-free", 1_000_000, 512_000, true),
    ] {
        let metadata = catalog
            .get(&ModelRef::new(provider_id("opencode-go"), id))
            .unwrap();
        assert_eq!(metadata.context_window_tokens, Some(context), "{id}");
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(output), "{id}");
        assert_eq!(metadata.default_max_output_tokens, Some(output), "{id}");
        assert_eq!(metadata.capabilities.image_input, image_input, "{id}");
        assert!(metadata.description.contains("models.dev's opencode-go"));
        assert!(metadata
            .description
            .contains("not independently verified service hard limits"));
    }
}

#[test]
fn built_in_catalog_registers_each_canonical_model_metadata_once() {
    let mut registrations = HashMap::<ModelRef, Vec<ModelRef>>::new();
    for entry in built_in_entries() {
        let legacy_model_ref = entry.model_ref;
        let (provider, _) =
            built_in_provider_endpoint_identity(&legacy_model_ref.provider).unwrap();
        registrations
            .entry(ModelRef::new(provider, legacy_model_ref.model.clone()))
            .or_default()
            .push(legacy_model_ref);
    }

    let duplicates = registrations
        .into_iter()
        .filter(|(_, legacy_refs)| legacy_refs.len() > 1)
        .collect::<Vec<_>>();
    assert!(
        duplicates.is_empty(),
        "canonical model metadata must be registered once: {duplicates:#?}"
    );
}

#[test]
fn tencent_chat_k3_fields_do_not_leak_into_other_protocols() {
    let catalog = curated_catalog();
    for model in ["kimi-k3", "kimi-k3.1-pro", "kimi-k3.1-pro-highspeed"] {
        for endpoint in ["default", "messages", "responses"] {
            let route =
                ModelRouteRef::parse(&format!("tencent-tokenhub@{endpoint}/{model}")).unwrap();
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &base_context(),
                8_192,
            );
            assert_eq!(policy.context_window_tokens, None);
            if endpoint == "default" {
                assert_eq!(policy.runtime_max_output_tokens, 131_072);
                assert_eq!(policy.max_output_tokens_upper_limit, Some(1_048_576));
                assert!(policy.capabilities.supports_reasoning);
                assert!(policy.capabilities.image_input);
                assert_eq!(policy.reasoning_effort_options, ["low", "high", "max"]);
            } else {
                assert_eq!(policy.runtime_max_output_tokens, 8_192);
                assert_eq!(policy.max_output_tokens_upper_limit, None);
                assert!(!policy.capabilities.supports_reasoning);
                assert!(!policy.capabilities.image_input);
                assert!(policy.reasoning_effort_options.is_empty());
            }
        }
    }
}

#[test]
fn tencent_tokenhub_catalog_tracks_current_language_and_image_understanding_models() {
    let catalog = curated_catalog();
    let provider = provider_id("tencent-tokenhub");
    let models = catalog
        .list()
        .into_iter()
        .filter(|model| model.model_ref.provider == provider)
        .collect::<Vec<_>>();

    assert_eq!(models.len(), 39);
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
    }

    let hy3 = catalog
        .get(&ModelRef::new(provider.clone(), "hy3"))
        .unwrap();
    assert_eq!(hy3.context_window_tokens, Some(256_000));
    assert_eq!(hy3.max_output_tokens_upper_limit, Some(128_000));

    for vision in ["hy-vision-2.0-instruct", "hunyuan-t1-vision-20250916"] {
        assert!(
            catalog
                .get(&ModelRef::new(provider.clone(), vision))
                .unwrap()
                .capabilities
                .image_input
        );
    }
    for retired_or_unsupported in [
        "glm-5v-turbo",
        "youtu-vita",
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
    let catalog = curated_catalog();
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
