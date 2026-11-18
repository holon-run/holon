use super::*;
use holon_provider::model_discovery::ModelDiscoveryCacheFile;

mod image_generation;
mod routing;
mod vision;

struct CatalogFixture {
    stored_config: HolonConfigFile,
    default_model: ModelRouteRef,
    fallback_models: Vec<ModelRouteRef>,
    vision_model: Option<ModelRouteRef>,
    image_generation_model: Option<ModelRouteRef>,
    vision_candidate_models: Vec<ModelRouteRef>,
    providers: ProviderRegistry,
    model_discovery_cache: ModelDiscoveryCacheFile,
    validated_model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    validated_unknown_model_fallback: Option<ModelRuntimeOverride>,
}

impl ModelCatalogSource for CatalogFixture {
    fn model_catalog_config(&self) -> ModelCatalogConfig<'_> {
        ModelCatalogConfig {
            stored_config: &self.stored_config,
            default_model: &self.default_model,
            fallback_models: &self.fallback_models,
            vision_model: self.vision_model.as_ref(),
            image_generation_model: self.image_generation_model.as_ref(),
            vision_candidate_models: &self.vision_candidate_models,
            disable_provider_fallback: false,
            providers: &self.providers,
            model_discovery_cache: &self.model_discovery_cache,
            validated_model_overrides: &self.validated_model_overrides,
            validated_unknown_model_fallback: self.validated_unknown_model_fallback.as_ref(),
            runtime_max_output_tokens: 8192,
        }
    }
}

fn route_ref(value: &str) -> ModelRouteRef {
    ModelRouteRef::parse_compatible(value).unwrap()
}

fn catalog_fixture(default_model: &str, fallback_models: &[&str]) -> CatalogFixture {
    CatalogFixture {
        stored_config: HolonConfigFile::default(),
        default_model: route_ref(default_model),
        fallback_models: fallback_models
            .iter()
            .map(|value| route_ref(value))
            .collect(),
        vision_model: None,
        image_generation_model: None,
        vision_candidate_models: Vec::new(),
        providers: provider_registry_for_tests(
            Some("openai-key"),
            Some("anthropic-token"),
            PathBuf::from("/tmp/codex-home"),
        ),
        model_discovery_cache: ModelDiscoveryCacheFile::default(),
        validated_model_overrides: HashMap::new(),
        validated_unknown_model_fallback: None,
    }
}

fn test_context() -> ContextConfig {
    ContextConfig {
        recent_messages: 8,
        recent_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        ..ContextConfig::default()
    }
}

#[test]
fn legacy_model_route_normalization_matches_the_runtime_catalog() {
    let catalog = BuiltInModelCatalog::default();
    let references = catalog
        .list()
        .into_iter()
        .map(|model| model.model_ref)
        .chain(
            catalog
                .list_routes()
                .into_iter()
                .map(|route| route.model_ref()),
        )
        .chain(
            holon_domain::model_values::provider_definitions()
                .iter()
                .map(|definition| {
                    ModelRef::new(
                        ProviderId::parse(definition.legacy_provider).unwrap(),
                        "unknown",
                    )
                }),
        )
        .chain([
            ModelRef::parse("dashscope-token-plan/qwen-3.7").unwrap(),
            ModelRef::parse("volcengine-image-openai/doubao-seedream-5.0-lite").unwrap(),
            ModelRef::parse("mistral/devstral-medium-latest").unwrap(),
        ]);
    for legacy in references {
        let (provider, endpoint) = built_in_provider_endpoint_identity(&legacy.provider).unwrap();
        let canonical = catalog.canonicalize_model_ref(&legacy);
        let endpoint = if provider == canonical.provider
            && endpoint == ProviderEndpointId::default_endpoint()
        {
            catalog
                .preferred_route_for_model(&canonical)
                .map(|route| route.endpoint)
                .unwrap_or(endpoint)
        } else {
            endpoint
        };
        assert_eq!(
            ModelRouteRef::parse_compatible(&legacy.as_string()).unwrap(),
            ModelRouteRef::new(provider, endpoint, canonical.model),
            "{}",
            legacy.as_string()
        );
    }
}

#[test]
fn model_policy_preserves_exact_route_metadata_without_a_configured_endpoint() {
    let catalog = RuntimeModelCatalog::default();
    let route_ref = ModelRouteRef::parse("volcengine@plan/glm-5.2").unwrap();

    let policy = catalog.resolved_model_policy(&ContextConfig::default(), Some(&route_ref));

    assert_eq!(policy.reasoning_effort_options, ["low", "medium", "high"]);
    assert_eq!(
        policy
            .evidence
            .source_for(ModelMetadataField::ReasoningEffortOptions),
        Some(holon_model_catalog::ModelMetadataOrigin::RouteBuiltin)
    );
}

#[test]
fn resolved_capabilities_intersect_model_and_transport_image_output() {
    let mut policy = ResolvedRuntimeModelPolicy {
        capabilities: holon_model_catalog::ModelCapabilityFlags {
            image_generation: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let capabilities =
        resolve_model_capabilities(&mut policy, ProviderTransportKind::AnthropicMessages);

    assert!(capabilities
        .intrinsic
        .output_modalities
        .contains(&holon_model_catalog::ModelModality::Image));
    assert!(!capabilities.transport.image_output);
    assert!(!capabilities.image_output);
    assert!(!capabilities.supports(ModelRouteCapability::ImageGeneration));
    assert_eq!(
        policy.evidence.constraints,
        [ModelMetadataConstraint {
            field: ModelMetadataField::ImageGeneration,
            kind: ModelMetadataConstraintKind::Disabled,
            source: ModelMetadataConstraintSource::TransportCapability,
            requested: "true".into(),
            effective: "false".into(),
        }]
    );
}

#[test]
fn resolved_capabilities_record_each_transport_image_constraint_once() {
    let mut policy = ResolvedRuntimeModelPolicy {
        capabilities: holon_model_catalog::ModelCapabilityFlags {
            image_input: true,
            image_generation: true,
            ..Default::default()
        },
        ..Default::default()
    };

    for _ in 0..2 {
        let capabilities =
            resolve_model_capabilities(&mut policy, ProviderTransportKind::AiEvaluationModel);
        assert!(!capabilities.image_input);
        assert!(!capabilities.image_output);
    }

    assert_eq!(
        policy.evidence.constraints,
        [
            ModelMetadataConstraint {
                field: ModelMetadataField::ImageInput,
                kind: ModelMetadataConstraintKind::Disabled,
                source: ModelMetadataConstraintSource::TransportCapability,
                requested: "true".into(),
                effective: "false".into(),
            },
            ModelMetadataConstraint {
                field: ModelMetadataField::ImageGeneration,
                kind: ModelMetadataConstraintKind::Disabled,
                source: ModelMetadataConstraintSource::TransportCapability,
                requested: "true".into(),
                effective: "false".into(),
            },
        ]
    );
}

#[test]
fn resolved_capabilities_do_not_record_inactive_transport_constraints() {
    let mut policy = ResolvedRuntimeModelPolicy::default();

    let capabilities =
        resolve_model_capabilities(&mut policy, ProviderTransportKind::GeminiGenerateContent);

    assert!(!capabilities.image_input);
    assert!(!capabilities.image_output);
    assert!(policy.evidence.constraints.is_empty());
}

#[test]
fn gemini_transport_supports_vision_without_unimplemented_reasoning_controls() {
    let catalog = holon_model_catalog::BuiltInModelCatalog::new();
    let mut policy = catalog.resolve_policy(
        &ModelRef::parse("gemini/gemini-3.5-flash").unwrap(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &crate::ContextConfig::default(),
        8192,
    );

    assert!(policy.capabilities.image_input);
    assert!(policy.capabilities.supports_reasoning);
    assert!(policy.reasoning_effort_options.is_empty());

    let capabilities =
        resolve_model_capabilities(&mut policy, ProviderTransportKind::GeminiGenerateContent);
    assert!(capabilities.transport.image_input);
    assert!(capabilities.image_input);
    assert!(capabilities.supports(ModelRouteCapability::VisionObservation));
    assert!(capabilities
        .endpoint
        .accepted_parameters
        .iter()
        .all(|parameter| parameter.name != "reasoning_effort"));
}

#[test]
fn resolved_capabilities_preserve_reasoning_effort_allowed_values() {
    let mut policy = ResolvedRuntimeModelPolicy {
        reasoning_effort_options: vec!["low".into(), "high".into()],
        ..Default::default()
    };

    let capabilities =
        resolve_model_capabilities(&mut policy, ProviderTransportKind::OpenAiResponses);

    assert_eq!(
        capabilities.endpoint.accepted_parameters,
        vec![
            holon_model_catalog::ModelParameterSupport {
                name: "reasoning_effort".into(),
                allowed_values: vec!["low".into(), "high".into()],
            },
            holon_model_catalog::ModelParameterSupport {
                name: "max_output_tokens".into(),
                allowed_values: Vec::new(),
            },
        ]
    );
}

#[test]
fn model_route_ref_round_trips_model_ids_with_slashes() {
    let route_ref = ModelRouteRef::parse("openrouter@default/anthropic/claude-3.5-sonnet").unwrap();
    assert_eq!(route_ref.provider.as_str(), "openrouter");
    assert_eq!(route_ref.endpoint.as_str(), "default");
    assert_eq!(route_ref.model, "anthropic/claude-3.5-sonnet");
    assert_eq!(
        route_ref.as_string(),
        "openrouter@default/anthropic/claude-3.5-sonnet"
    );
}

#[test]
fn model_route_ref_compact_display_only_omits_default_endpoint() {
    let default_route =
        ModelRouteRef::parse("openrouter@default/anthropic/claude-3.5-sonnet").unwrap();
    assert_eq!(
        default_route.as_compact_display(),
        "openrouter/anthropic/claude-3.5-sonnet"
    );
    assert_eq!(
        default_route.as_string(),
        "openrouter@default/anthropic/claude-3.5-sonnet"
    );
    assert_eq!(
        serde_json::to_string(&default_route).unwrap(),
        r#""openrouter@default/anthropic/claude-3.5-sonnet""#
    );

    let plan_route = ModelRouteRef::parse("volcengine@plan/glm-5.2").unwrap();
    assert_eq!(plan_route.as_compact_display(), "volcengine@plan/glm-5.2");

    let token_plan_route = ModelRouteRef::parse("dashscope@token-plan/qwen3.7-max").unwrap();
    assert_eq!(
        token_plan_route.as_compact_display(),
        "dashscope@token-plan/qwen3.7-max"
    );
}

#[test]
fn compatible_model_route_ref_keeps_legacy_model_remainder() {
    let route_ref =
        ModelRouteRef::parse_compatible("openrouter/anthropic/claude-3.5-sonnet").unwrap();
    assert_eq!(route_ref.provider.as_str(), "openrouter");
    assert_eq!(route_ref.endpoint.as_str(), "default");
    assert_eq!(route_ref.model, "anthropic/claude-3.5-sonnet");
}

#[test]
fn opencode_go_standard_refs_use_messages_and_preserve_explicit_routes() {
    for model in ["glm-5.3-flash", "minimax-m3", "gpt-6-luna", "future-model"] {
        let standard = ModelRouteRef::parse_compatible(&format!("opencode-go/{model}")).unwrap();
        assert_eq!(standard.endpoint.as_str(), "messages");
        for endpoint in ["default", "messages", "responses"] {
            let explicit = format!("opencode-go@{endpoint}/{model}");
            let route = ModelRouteRef::parse_compatible(&explicit).unwrap();
            assert_eq!(route.as_string(), explicit);
            assert_eq!(
                ModelRouteRef::parse_compatible(&route.as_compact_display()).unwrap(),
                route
            );
            let json = serde_json::to_string(&route).unwrap();
            assert_eq!(serde_json::from_str::<ModelRouteRef>(&json).unwrap(), route);
        }
    }
    for (legacy, endpoint) in [("opencode-go-messages", "messages")] {
        let route = ModelRouteRef::parse_compatible(&format!("{legacy}/glm-5.3-flash")).unwrap();
        assert_eq!(route.provider.as_str(), "opencode-go");
        assert_eq!(route.endpoint.as_str(), endpoint);
    }
    let catalog = BuiltInModelCatalog::default();
    assert_eq!(
        catalog
            .preferred_route_for_provider(&ProviderId::parse("opencode-go").unwrap())
            .unwrap()
            .endpoint
            .as_str(),
        "messages"
    );
}

#[test]
fn compatible_model_route_ref_upgrades_builtin_endpoint() {
    let route_ref =
        ModelRouteRef::parse_compatible("volcengine-image-openai/doubao-seedream-5.0-lite")
            .unwrap();
    assert_eq!(route_ref.provider.as_str(), "volcengine");
    assert_eq!(route_ref.endpoint.as_str(), "default");
    assert_eq!(route_ref.model, "doubao-seedream-5.0-lite");
}

#[test]
fn compatible_model_route_ref_upgrades_legacy_plan_provider() {
    let route_ref = ModelRouteRef::parse_compatible("dashscope-token-plan/qwen3.7-max").unwrap();
    assert_eq!(route_ref.provider.as_str(), "dashscope");
    assert_eq!(route_ref.endpoint.as_str(), "token-plan");
    assert_eq!(route_ref.model, "qwen3.7-max");
}

#[test]
fn compatible_model_route_ref_preserves_endpoint_across_model_alias() {
    let route_ref = ModelRouteRef::parse_compatible("dashscope-token-plan/qwen-3.7").unwrap();
    assert_eq!(route_ref.provider.as_str(), "dashscope");
    assert_eq!(route_ref.endpoint.as_str(), "token-plan");
    assert_eq!(route_ref.model, "qwen3.7-max");
}

#[test]
fn compatible_model_route_ref_rejects_malformed_route_segments() {
    for value in [
        "openai@/gpt-5.4",
        "openai@@default/gpt-5.4",
        "openai@default/",
    ] {
        let error = ModelRouteRef::parse_compatible(value)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("expected provider@endpoint/model or legacy provider/model"),
            "unexpected error for {value}: {error}"
        );
    }
}

#[test]
fn compatible_model_route_ref_reports_both_accepted_formats() {
    let error = ModelRouteRef::parse_compatible("not-a-model-ref")
        .unwrap_err()
        .to_string();
    assert!(error.contains("expected provider@endpoint/model or legacy provider/model"));
}

#[test]
fn model_route_ref_deserializes_legacy_and_serializes_canonical() {
    let route_ref: ModelRouteRef = serde_json::from_str(r#""anthropic/claude-sonnet-4""#).unwrap();
    assert_eq!(
        serde_json::to_string(&route_ref).unwrap(),
        r#""anthropic@default/claude-sonnet-4""#
    );
}

#[test]
fn parse_fallback_entry_list_json_array() {
    let entries =
        parse_fallback_entry_list(r#"["openai-codex/gpt-5","anthropic/claude-sonnet-4"]"#).unwrap();
    assert_eq!(
        entries,
        vec![
            FallbackEntry::Route(ModelRouteRef::parse("openai-codex@default/gpt-5").unwrap()),
            FallbackEntry::Route(
                ModelRouteRef::parse("anthropic@default/claude-sonnet-4").unwrap()
            ),
        ]
    );
}

#[test]
fn parse_fallback_entry_list_accepts_auto_marker() {
    let entries = parse_fallback_entry_list(r#"["auto","openai-codex/gpt-5"]"#).unwrap();
    assert_eq!(
        entries,
        vec![
            FallbackEntry::Auto,
            FallbackEntry::Route(ModelRouteRef::parse("openai-codex@default/gpt-5").unwrap()),
        ]
    );
}

#[test]
fn parse_fallback_entry_list_auto_marker_is_case_insensitive() {
    let entries = parse_fallback_entry_list("AUTO, anthropic/claude-sonnet-4").unwrap();
    assert_eq!(
        entries,
        vec![
            FallbackEntry::Auto,
            FallbackEntry::Route(
                ModelRouteRef::parse("anthropic@default/claude-sonnet-4").unwrap()
            ),
        ]
    );
}

#[test]
fn parse_fallback_entry_list_comma_separated() {
    let entries =
        parse_fallback_entry_list("openai-codex/gpt-5, anthropic/claude-sonnet-4").unwrap();
    assert_eq!(entries.len(), 2);
    assert!(matches!(entries[0], FallbackEntry::Route(_)));
    assert!(matches!(entries[1], FallbackEntry::Route(_)));
}

#[test]
fn parse_fallback_entry_list_empty_json_array_rejected() {
    assert!(parse_fallback_entry_list("[]").is_err());
}
