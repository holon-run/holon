use super::*;
use holon_model_catalog::ModelCapabilityOverride;

#[test]
fn runtime_model_catalog_resolves_image_generation_route() {
    let fixture = catalog_fixture("arcee/trinity-mini", &["openai/gpt-image-2"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(route.model_ref.as_string(), "openai/gpt-image-2");
    assert_eq!(
        route.requested_capability,
        ModelRouteCapability::ImageGeneration
    );
    assert_eq!(route.endpoint.provider.as_str(), "openai");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
}

#[test]
fn runtime_model_catalog_resolves_xai_imagine_image_generation_route() {
    let mut fixture = catalog_fixture("openai/gpt-5.4", &[]);
    let xai = ProviderId::parse("xai").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture
        .providers
        .insert(xai.clone(), built_ins.get(&xai).unwrap().clone());
    fixture.image_generation_model = Some(route_ref("xai@default/grok-imagine-image-2.0"));
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(route.model_ref.as_string(), "xai/grok-imagine-image-2.0");
    assert_eq!(
        route.requested_capability,
        ModelRouteCapability::ImageGeneration
    );
    assert_eq!(route.endpoint.provider.as_str(), "xai");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
}

#[test]
fn runtime_model_catalog_resolves_legacy_multi_endpoint_provider_identity() {
    let mut fixture = catalog_fixture("openai/gpt-5.4", &["volcengine/doubao-seedream-5.0-lite"]);
    let legacy_provider = ProviderId::parse("volcengine").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.providers.insert(
        legacy_provider.clone(),
        built_ins.get(&legacy_provider).unwrap().clone(),
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        route.model_ref.as_string(),
        "volcengine/doubao-seedream-5.0-lite"
    );
    assert_eq!(
        route.requested_capability,
        ModelRouteCapability::ImageGeneration
    );
    assert_eq!(route.endpoint.provider.as_str(), "volcengine");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
    assert_eq!(route.endpoint.runtime_config.id.as_str(), "volcengine");
}

#[test]
fn generate_image_selection_auto_uses_turn_chain() {
    let fixture = catalog_fixture("arcee/trinity-mini", &["openai/gpt-image-2"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(selected.as_string(), "openai@default/gpt-image-2");
}

#[test]
fn generate_image_selection_uses_current_turn_fallback_model() {
    let mut fixture = catalog_fixture("openai/gpt-image-2", &["openai/recovery-image"]);
    let recovery_model = ModelRef::parse("openai/recovery-image").unwrap();
    let recovery_override = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            image_generation: Some(true),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .validated_model_overrides
        .insert(recovery_model.clone(), recovery_override.clone());
    fixture
        .stored_config
        .models
        .catalog
        .insert(recovery_model.as_string(), recovery_override);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selected = catalog
        .select_generate_image_model(
            &ContextConfig::default(),
            None,
            Some(&route_ref("openai/recovery-image")),
        )
        .unwrap();

    assert_eq!(selected.as_string(), "openai@default/recovery-image");
}

#[test]
fn generate_image_selection_uses_explicit_image_generation_model() {
    let mut fixture = catalog_fixture("openai@default/gpt-image-2", &[]);
    fixture.image_generation_model = Some(route_ref("openai-codex@default/gpt-6-sol"));
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(selected.as_string(), "openai-codex@default/gpt-6-sol");
}

#[test]
fn generate_image_selection_requires_explicit_model_capability() {
    let mut fixture = catalog_fixture("openai@default/gpt-image-2", &[]);
    fixture.image_generation_model = Some(route_ref("openai@default/gpt-5.4"));
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    assert!(catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .is_none());
}

#[test]
fn generate_image_selection_accepts_volcengine_image_openai_seedream() {
    let mut fixture = catalog_fixture("openai@default/gpt-5.4", &[]);
    fixture.image_generation_model = Some(route_ref(
        "volcengine-image-openai/doubao-seedream-5.0-lite",
    ));
    fixture.providers.insert(
        ProviderId::parse("volcengine-image-openai").unwrap(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: ProviderId::parse("volcengine-image-openai").unwrap(),
            route_provider: ProviderId::parse("volcengine").unwrap(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://ark.cn-beijing.volces.com/api/v3".into(),
            auth: ProviderAuthConfig::default(),
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        selected.as_string(),
        "volcengine@default/doubao-seedream-5.0-lite"
    );
}

#[test]
fn generate_image_selection_accepts_canonical_volcengine_seedream() {
    let mut fixture = catalog_fixture("openai@default/gpt-5.4", &[]);
    fixture.image_generation_model = Some(route_ref("volcengine@default/doubao-seedream-5.0-lite"));
    let legacy_provider = ProviderId::parse("volcengine").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.providers.insert(
        legacy_provider.clone(),
        built_ins.get(&legacy_provider).unwrap().clone(),
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        selected.as_string(),
        "volcengine@default/doubao-seedream-5.0-lite"
    );
}

#[test]
fn runtime_model_catalog_resolves_canonical_seedream_route_endpoint() {
    let mut fixture = catalog_fixture("openai@default/gpt-5.4", &[]);
    fixture.image_generation_model = Some(route_ref("volcengine@default/doubao-seedream-5.0-lite"));
    let legacy_provider = ProviderId::parse("volcengine").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.providers.insert(
        legacy_provider.clone(),
        built_ins.get(&legacy_provider).unwrap().clone(),
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        route.model_ref.as_string(),
        "volcengine/doubao-seedream-5.0-lite"
    );
    assert_eq!(route.endpoint.provider.as_str(), "volcengine");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
    assert_eq!(route.endpoint.runtime_config.id.as_str(), "volcengine");
}
