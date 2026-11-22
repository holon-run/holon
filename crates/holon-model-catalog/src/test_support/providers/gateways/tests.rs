use super::*;

#[test]
fn opencode_go_flash_and_kimi_capacities_remain_native_only() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        for (id, default, ceiling) in [
            ("hy4-flash-free", None, 65_536),
            ("kimi-k3", Some(131_072), 1_048_576),
            ("kimi-k3.1-pro", None, 1_048_576),
        ] {
            let native = ModelRouteRef::parse(&format!("opencode-go@default/{id}")).unwrap();
            assert!(catalog.get_route(&native).is_some());
            let metadata = catalog.get(&native.model_ref()).unwrap();
            assert_eq!(metadata.context_window_tokens, Some(1_048_576));
            assert_eq!(metadata.default_max_output_tokens, default);
            assert_eq!(metadata.max_output_tokens_upper_limit, Some(ceiling));
            if id == "hy4-flash-free" {
                assert_eq!(metadata.reasoning_effort_options, ["none", "high"]);
            }
            for endpoint in ["messages", "responses"] {
                let translated =
                    ModelRouteRef::parse(&format!("opencode-go@{endpoint}/{id}")).unwrap();
                let policy = catalog.get_route(&translated).unwrap();
                assert_eq!(policy.context_window_tokens, Some(None));
                assert_eq!(policy.default_max_output_tokens, Some(None));
                assert_eq!(policy.max_output_tokens_upper_limit, Some(None));
                assert_eq!(policy.capabilities.image_input, Some(false));
                assert_eq!(policy.capabilities.supports_reasoning, Some(false));
                if id == "hy4-flash-free" {
                    assert_eq!(policy.capabilities.agent_turn, Some(false));
                }
            }
        }
    }
}

#[test]
fn opencode_go_hy4_native_metadata_does_not_certify_translation() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        let route = ModelRouteRef::parse("opencode-go@default/hy4").unwrap();
        assert!(catalog.get_route(&route).is_some());
        let metadata = catalog.get(&route.model_ref()).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_048_576));
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(65_536));
        assert_eq!(metadata.reasoning_effort_options, ["none", "high"]);
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.supports_reasoning);
        for endpoint in ["messages", "responses"] {
            let route = ModelRouteRef::parse(&format!("opencode-go@{endpoint}/hy4")).unwrap();
            assert!(catalog.get_route(&route).is_some());
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(policy.context_window_tokens, None);
            assert_eq!(policy.max_output_tokens_upper_limit, None);
            assert!(!policy.capabilities.supports_reasoning);
            assert!(!policy.capabilities.image_input);
            assert!(!policy.agent_turn);
            assert!(policy.reasoning_effort_options.is_empty());
        }
    }
}

#[test]
fn opencode_go_crouton_alpha_free_native_responses_metadata_does_not_certify_translation() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        let route = ModelRouteRef::parse("opencode-go@responses/crouton-alpha-free").unwrap();
        assert!(catalog.get_route(&route).is_some());
        let metadata = catalog.get(&route.model_ref()).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_048_576));
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(270_000));
        assert_eq!(
            metadata.reasoning_effort_options,
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.supports_reasoning);
        for endpoint in ["default", "messages"] {
            let route = ModelRouteRef::parse(&format!("opencode-go@{endpoint}/crouton-alpha-free"))
                .unwrap();
            assert!(catalog.get_route(&route).is_some());
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(policy.context_window_tokens, None);
            assert_eq!(policy.max_output_tokens_upper_limit, None);
            assert!(!policy.capabilities.supports_reasoning);
            assert!(!policy.capabilities.image_input);
            assert!(!policy.agent_turn);
            assert!(policy.reasoning_effort_options.is_empty());
        }
    }
}

#[test]
fn opencode_go_longcat_25_is_native_chat_only() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        let route = ModelRouteRef::parse("opencode-go@default/longcat-2.5").unwrap();
        assert!(catalog.get_route(&route).is_some());
        let metadata = catalog.get(&route.model_ref()).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_000_000));
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(262_144));
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.supports_reasoning);
        assert!(metadata.reasoning_effort_options.is_empty());
        assert_eq!(
            catalog.preferred_route_for_model(&route.model_ref()),
            Some(ModelRouteRef::parse("opencode-go@messages/longcat-2.5").unwrap())
        );
        let resolve = |route: &ModelRouteRef, discovered: &HashMap<_, _>| {
            catalog
                .try_resolve_route_policy(
                    route,
                    &HashMap::new(),
                    discovered,
                    None,
                    &ContextConfig::default(),
                    8_192,
                )
                .unwrap()
        };
        let chat = resolve(&route, &HashMap::new());
        assert_eq!(chat.context_window_tokens, Some(1_000_000));
        assert_eq!(chat.max_output_tokens_upper_limit, Some(262_144));
        assert!(chat.capabilities.image_input);
        assert!(chat.capabilities.supports_reasoning);

        let discovered = HashMap::from([(route.model_ref(), metadata.clone())]);
        for endpoint in ["messages", "responses"] {
            let route =
                ModelRouteRef::parse(&format!("opencode-go@{endpoint}/longcat-2.5")).unwrap();
            assert!(catalog.get_route(&route).is_none());
            for discovered in [&HashMap::new(), &discovered] {
                let policy = resolve(&route, discovered);
                assert_eq!(policy.context_window_tokens, None);
                assert_eq!(policy.max_output_tokens_upper_limit, None);
                assert!(!policy.capabilities.image_input);
                assert!(!policy.capabilities.supports_reasoning);
                assert!(policy.reasoning_effort_options.is_empty());
            }
        }
    }
}

#[test]
fn opencode_go_longcat_25_free_native_chat_does_not_certify_translation() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        let route = ModelRouteRef::parse("opencode-go@default/longcat-2.5-free").unwrap();
        assert!(catalog.get_route(&route).is_some());
        let metadata = catalog.get(&route.model_ref()).unwrap();
        assert_eq!(metadata.context_window_tokens, Some(1_000_000));
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(262_144));
        assert!(metadata.capabilities.image_input);
        assert!(metadata.capabilities.supports_reasoning);
        assert!(metadata.reasoning_effort_options.is_empty());
        assert_eq!(
            catalog
                .preferred_route_for_model(&route.model_ref())
                .unwrap(),
            ModelRouteRef::parse("opencode-go@messages/longcat-2.5-free").unwrap()
        );
        for endpoint in ["messages", "responses"] {
            let route =
                ModelRouteRef::parse(&format!("opencode-go@{endpoint}/longcat-2.5-free")).unwrap();
            assert!(catalog.get_route(&route).is_some());
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(policy.context_window_tokens, None);
            assert_eq!(policy.max_output_tokens_upper_limit, None);
            assert!(!policy.capabilities.supports_reasoning);
            assert!(!policy.capabilities.image_input);
            assert!(!policy.agent_turn);
            assert!(policy.reasoning_effort_options.is_empty());
        }
    }
}

#[test]
fn opencode_go_native_metadata_preserves_defaults_and_exact_efforts() {
    let entries = opencode_entries();
    let find = |id: &str| {
        entries
            .iter()
            .find(|entry| entry.model_ref.model == id)
            .unwrap()
    };
    for (id, context, output, image) in [
        ("kimi-k3.1-pro", 1_048_576, 1_048_576, true),
        ("iceman-contributor-free", 262_144, 131_072, false),
        ("minimax-m3", 1_000_000, 131_072, true),
    ] {
        let model = find(id);
        assert_eq!(model.context_window_tokens, Some(context), "{id}");
        assert_eq!(model.max_output_tokens_upper_limit, Some(output), "{id}");
        assert_eq!(model.default_max_output_tokens, None, "{id}");
        assert_eq!(model.capabilities.image_input, image, "{id}");
        assert!(model.capabilities.supports_reasoning, "{id}");
    }
    for (id, endpoint, expected) in [
        (
            "deepseek-v4.1-flash",
            "default",
            &["low", "high", "max"][..],
        ),
        ("glm-5.3", "default", &["low", "high", "max"][..]),
        ("glm-5.3-flash", "default", &["low", "high", "max"][..]),
        ("kimi-k3.1-pro", "default", &["low", "high", "max"][..]),
        ("deepseek-v4-pro", "default", &["high", "max"][..]),
        ("hy3", "default", &["none", "low", "high"][..]),
        ("hy4-preview", "default", &["none", "high"][..]),
        ("kimi-k3", "default", &["max"][..]),
        (
            "minimax-m3.1-flash",
            "default",
            &["low", "medium", "high", "xhigh", "max"][..],
        ),
        (
            "beam-free",
            "default",
            &["low", "medium", "high", "xhigh", "max"][..],
        ),
        ("step-5-preview", "default", &["low", "medium", "high"][..]),
        (
            "claude-haiku-5-5",
            "messages",
            &["low", "medium", "high", "xhigh", "max"][..],
        ),
        ("qwen3.8-max", "messages", &["low", "medium", "xhigh"][..]),
        ("qwen3.8-flash", "messages", &["low", "medium", "xhigh"][..]),
        (
            "grok-4.7",
            "responses",
            &["low", "medium", "high", "xhigh"][..],
        ),
        (
            "muse-spark-1.2-contributor",
            "responses",
            &["minimal", "low", "medium", "high", "xhigh"][..],
        ),
        (
            "muse-spark-1.3-contributor",
            "responses",
            &["minimal", "low", "medium", "high", "xhigh"][..],
        ),
        (
            "gpt-5.6-luna",
            "responses",
            &["none", "low", "medium", "high", "xhigh", "max"][..],
        ),
        (
            "gpt-6-luna",
            "responses",
            &["none", "low", "medium", "high", "xhigh", "max"][..],
        ),
        (
            "crouton-alpha-free",
            "responses",
            &["minimal", "low", "medium", "high", "xhigh", "max"][..],
        ),
    ] {
        let model = find(id);
        assert_eq!(
            model
                .endpoint
                .as_ref()
                .map_or("default", ProviderEndpointId::as_str),
            endpoint
        );
        assert_eq!(model.reasoning_effort_options, expected, "{id}");
    }
    for id in [
        "iceman-contributor-free",
        "minimax-m3",
        "gemini-4-argon",
        "minimax-m3.1-flash-free",
    ] {
        assert!(find(id).reasoning_effort_options.is_empty(), "{id}");
    }
}

#[test]
fn opencode_go_gemini_uses_exact_id_without_inventing_capabilities() {
    let entries = opencode_entries();
    let gemini = entries
        .iter()
        .filter(|entry| entry.model_ref.provider == provider_id("opencode-go"))
        .filter(|entry| entry.model_ref.model.starts_with("gemini-"))
        .collect::<Vec<_>>();
    assert_eq!(gemini.len(), 1);
    let model = gemini[0];
    assert_eq!(model.model_ref.model, "gemini-4-argon");
    assert_eq!(model.display_name, "Gemini 4 Argon");
    assert_eq!(model.endpoint, None);
    assert_eq!(model.source, ModelMetadataSource::ConservativeBuiltin);
    assert_eq!(model.context_window_tokens, None);
    assert_eq!(model.auto_compact_token_limit, None);
    assert_eq!(model.default_max_output_tokens, None);
    assert_eq!(model.max_output_tokens_upper_limit, None);
    assert_eq!(model.default_verbosity, None);
    assert_eq!(model.capabilities, ModelCapabilityFlags::default());
    assert!(model.reasoning_effort_options.is_empty());
    assert!(holon_domain::model_values::OPENCODE_GO_TEXT_MODELS
        .contains(&("gemini-4-argon", "default")));
}
