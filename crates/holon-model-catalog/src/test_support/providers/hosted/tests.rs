use super::*;

#[test]
fn nvidia_request_limits_do_not_inherit_manufacturer_defaults() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        for (id, default, ceiling) in [
            ("nvidia/nemotron-3-super-120b-a12b", 16_384, 32_768),
            ("z-ai/glm-5.2", 16_384, 32_768),
            ("moonshotai/kimi-k2.6", 16_384, 65_536),
            ("minimaxai/minimax-m2.7", 8_192, 16_384),
            ("minimaxai/minimax-m3", 8_192, 16_384),
            ("minimaxai/minimax-m3.1-flash", 16_384, 65_536),
        ] {
            let route = ModelRouteRef::parse(&format!("nvidia@default/{id}")).unwrap();
            assert!(catalog.get_route(&route).is_some());
            let metadata = catalog.get(&route.model_ref()).unwrap();
            assert_eq!(metadata.default_max_output_tokens, Some(default));
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(ceiling));
            if id == "nvidia/nemotron-3-super-120b-a12b" {
                assert_eq!(metadata.reasoning_effort_options, ["none", "low", "high"]);
            }
            if id == "minimaxai/minimax-m3.1-flash" {
                assert_eq!(metadata.context_window_tokens, None);
                assert_eq!(metadata.auto_compact_token_limit, None);
                assert!(metadata.reasoning_effort_options.is_empty());
                assert!(metadata.capabilities.image_input);
            }
        }
        let fireworks = catalog
            .get(
                &ModelRef::parse("fireworks/accounts/fireworks/models/minimax-m3p1-flash").unwrap(),
            )
            .unwrap();
        assert_eq!(fireworks.context_window_tokens, Some(1_048_576));
        assert_eq!(fireworks.default_max_output_tokens, None);
        assert_eq!(fireworks.max_output_tokens_upper_limit, None);
        assert!(fireworks.reasoning_effort_options.is_empty());
        for (provider, id) in [
            ("nvidia", "minimaxai/minimax-m3.1-flash"),
            ("fireworks", "accounts/fireworks/models/minimax-m3p1-flash"),
        ] {
            for endpoint in ["messages", "responses"] {
                assert!(catalog
                    .get_route(
                        &ModelRouteRef::parse(&format!("{provider}@{endpoint}/{id}")).unwrap()
                    )
                    .is_none());
            }
        }
    }
}

#[test]
fn fireworks_k31_uses_provider_context_without_inventing_output_or_effort() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        let route =
            ModelRouteRef::parse("fireworks@default/accounts/fireworks/models/kimi-k3p1-pro")
                .unwrap();
        assert!(catalog.get_route(&route).is_some());
        let metadata = catalog.get(&route.model_ref()).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_048_576));
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, None);
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.agent_turn);
        assert!(metadata.reasoning_effort_options.is_empty());
        assert_eq!(
            catalog
                .preferred_route_for_provider(&provider_id("fireworks"))
                .unwrap()
                .as_string(),
            "fireworks@default/accounts/fireworks/models/minimax-m3"
        );
    }
}

#[test]
fn fireworks_retired_serverless_ids_preserve_custom_deployments_and_peers() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        assert_eq!(
            catalog
                .preferred_route_for_provider(&provider_id("fireworks"))
                .unwrap()
                .as_string(),
            "fireworks@default/accounts/fireworks/models/minimax-m3"
        );
        for model in [
            "kimi-k2p6",
            "kimi-k2p7-code",
            "glm-5p2",
            "gpt-oss-120b",
            "nemotron-3-ultra-nvfp4",
        ] {
            let route = ModelRouteRef::parse(&format!(
                "fireworks@default/accounts/fireworks/models/{model}"
            ))
            .unwrap();
            assert!(catalog.get_route(&route).is_none(), "{}", route.as_string());
            assert!(
                catalog
                    .get(
                        &ModelRef::parse(&format!("fireworks/accounts/fireworks/models/{model}"))
                            .unwrap()
                    )
                    .is_none(),
                "{model} must not retain public serverless metadata"
            );
        }
        for route in [
            "fireworks@default/accounts/fireworks/models/minimax-m3",
            "fireworks@default/accounts/fireworks/models/deepseek-v4-flash",
            "tencent-tokenhub@default/kimi-k2.7-code",
            "dashscope@token-plan/glm-5.2",
        ] {
            assert!(
                catalog
                    .get_route(&ModelRouteRef::parse(route).unwrap())
                    .is_some(),
                "{route}"
            );
        }
        let deployment =
            ModelRouteRef::parse("fireworks@default/accounts/example/deployments/reviewed-kimi")
                .unwrap();
        let policy = catalog.resolve_route_policy(
            &deployment,
            &HashMap::new(),
            &HashMap::new(),
            Some(&ModelRuntimeOverride {
                prompt_budget_estimated_tokens: Some(64_000),
                ..ModelRuntimeOverride::default()
            }),
            &ContextConfig::default(),
            8_192,
        );
        assert_eq!(policy.source, ModelMetadataSource::UnknownFallback);
        assert_eq!(policy.prompt_budget_estimated_tokens, 64_000);
    }
}

#[test]
fn together_minimax_m3_supports_reasoning_without_inventing_limits_or_effort() {
    let model = late_entries()
        .into_iter()
        .find(|model| {
            model.model_ref == ModelRef::new(provider_id("together"), "MiniMaxAI/MiniMax-M3")
        })
        .expect("Together MiniMax M3 builtin");

    assert!(model.capabilities.supports_reasoning);
    assert!(model.capabilities.image_input);
    assert!(model.reasoning_effort_options.is_empty());
    assert_eq!(model.context_window_tokens, Some(524_288));
    assert_eq!(model.default_max_output_tokens, None);
    assert_eq!(model.max_output_tokens_upper_limit, None);
}

#[test]
fn fireworks_minimax_m3_output_ceiling_preserves_default_and_other_models() {
    let model = early_entries()
        .into_iter()
        .find(|model| {
            model.model_ref
                == ModelRef::new(
                    provider_id("fireworks"),
                    "accounts/fireworks/models/minimax-m3",
                )
        })
        .expect("Fireworks MiniMax M3 builtin");

    assert_eq!(model.max_output_tokens_upper_limit, Some(512_000));
    assert_eq!(model.default_max_output_tokens, None);
    assert_eq!(model.context_window_tokens, Some(512_000));
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert!(model.reasoning_effort_options.is_empty());

    for id in ["accounts/fireworks/models/minimax-m2p7"] {
        let other = early_entries()
            .into_iter()
            .find(|model| model.model_ref == ModelRef::new(provider_id("fireworks"), id))
            .expect("existing Fireworks builtin");
        assert_eq!(other.default_max_output_tokens, None, "{id}");
        assert_eq!(other.max_output_tokens_upper_limit, None, "{id}");
    }
}
