use super::*;
use holon_model_catalog::ModelCapabilityOverride;

#[test]
fn view_image_vision_selection_uses_primary_when_image_capable() {
    let mut fixture = catalog_fixture("openai/gpt-5.4", &["anthropic/claude-sonnet-4-6"]);
    fixture.vision_candidate_models = vec![route_ref("openai/gpt-5.4")];
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
}

#[test]
fn view_image_vision_selection_uses_explicit_vision_model() {
    let mut fixture = catalog_fixture("arcee/trinity-mini", &["openai/gpt-5.4-mini"]);
    fixture.vision_model = Some(route_ref("openai/gpt-5.4"));
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("arcee"));
    assert_eq!(selection.primary_model.as_deref(), Some("trinity-mini"));
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(
        selection.selection_reason,
        "explicit_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 1);
    assert!(selection.candidates[0].image_input);
}

#[test]
fn view_image_vision_selection_auto_discovers_authenticated_adapter() {
    let mut fixture = catalog_fixture("arcee/trinity-mini", &[]);
    fixture.vision_candidate_models = vec![route_ref("openai/gpt-5.4-mini")];
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("arcee"));
    assert_eq!(selection.primary_model.as_deref(), Some("trinity-mini"));
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 2);
    assert!(!selection.candidates[0].image_input);
    assert!(selection.candidates[1].image_input);
}

#[test]
fn view_image_vision_selection_uses_configured_custom_auto_discovery_capabilities() {
    let mut fixture = catalog_fixture("arcee/trinity-mini", &[]);
    let custom_model = ModelRef::parse("custom-openai/my-vision-model").unwrap();
    fixture.vision_candidate_models = vec![ModelRouteRef::from_legacy_model_ref(&custom_model)];
    fixture.providers.insert(
        custom_model.provider.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: custom_model.provider.clone(),
            route_provider: custom_model.provider.clone(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.example.com/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::None,
                kind: CredentialKind::None,
                env: None,
                profile: None,
                external: None,
                client_id: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    let override_config = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            image_input: Some(true),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .validated_model_overrides
        .insert(custom_model.clone(), override_config.clone());
    fixture
        .stored_config
        .models
        .catalog
        .insert(custom_model.as_string(), override_config);
    let unknown_fallback = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            image_input: Some(false),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    fixture.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
    fixture.validated_unknown_model_fallback = Some(unknown_fallback);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("custom-openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("my-vision-model"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 2);
    assert!(!selection.candidates[0].image_input);
    assert!(selection.candidates[1].image_input);
}

#[test]
fn view_image_vision_selection_uses_anthropic_messages_when_image_capable() {
    let mut fixture = catalog_fixture("anthropic/claude-sonnet-5", &[]);
    fixture.vision_candidate_models = vec![route_ref("anthropic/claude-sonnet-5")];
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("anthropic"));
    assert_eq!(selection.primary_model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(selection.vision_provider.as_deref(), Some("anthropic"));
    assert_eq!(selection.vision_model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
    assert_eq!(
        selection.candidates[0].reason,
        "model_advertises_image_input"
    );
}

#[test]
fn view_image_vision_selection_uses_native_gemini_when_image_capable() {
    let fixture = catalog_fixture("gemini/gemini-2.5-pro", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("gemini"));
    assert_eq!(selection.vision_model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
}

#[test]
fn view_image_vision_selection_skips_image_capable_unsupported_transports() {
    let mut fixture = catalog_fixture("gemini/gemini-2.5-pro", &[]);
    fixture.vision_candidate_models = vec![route_ref("openai/gpt-5.4-mini")];
    fixture
        .providers
        .get_mut(&ProviderId::gemini())
        .unwrap()
        .transport = ProviderTransportKind::AiEvaluationModel;
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("gemini"));
    assert_eq!(selection.primary_model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 2);
    assert_eq!(
        selection.candidates[0].reason,
        "provider_transport_unsupported_for_view_image_observation"
    );
    assert_eq!(
        selection.candidates[1].reason,
        "model_advertises_image_input"
    );
}

#[test]
fn view_image_vision_selection_keeps_fallback_chain_as_compatibility_candidates() {
    let fixture = catalog_fixture("arcee/trinity-mini", &["openai/gpt-5.4-mini"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
}

#[test]
fn view_image_vision_selection_reports_unavailable_without_image_capable_model() {
    let fixture = catalog_fixture("arcee/trinity-mini", &["arcee/trinity-large-preview"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(selection.selected_mode, ViewImageSelectedMode::Unavailable);
    assert_eq!(
        selection.selection_reason,
        "no_configured_model_supports_view_image_observation"
    );
    assert!(selection.vision_provider.is_none());
    assert_eq!(selection.candidates.len(), 2);
    assert!(selection
        .candidates
        .iter()
        .all(|candidate| !candidate.image_input));
}

#[test]
fn view_image_vision_selection_prefers_primary_over_other_candidates() {
    // Primary (openai/gpt-5.4) supports image_input. vision_candidate_models contains
    // a different image-capable model (anthropic/claude-sonnet-4-6). The primary should
    // be selected first because it is tried before other candidates.
    let mut fixture = catalog_fixture("openai/gpt-5.4", &["anthropic/claude-sonnet-4-6"]);
    fixture.vision_candidate_models = vec![
        route_ref("anthropic/claude-sonnet-4-6"),
        route_ref("openai/gpt-5.4"),
    ];
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
    // Primary appears first in candidates
    assert_eq!(selection.candidates[0].provider, "openai");
    assert_eq!(selection.candidates[0].model, "gpt-5.4");
}

#[test]
fn view_image_vision_selection_canonicalizes_and_deduplicates_model_aliases() {
    let mut fixture = catalog_fixture("arcee/trinity-mini", &[]);
    let mistral = ProviderId::parse("mistral").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture
        .providers
        .insert(mistral.clone(), built_ins.get(&mistral).unwrap().clone());
    fixture.vision_candidate_models = vec![
        ModelRouteRef::parse("mistral@default/pixtral-large-latest").unwrap(),
        ModelRouteRef::parse("mistral@default/mistral-medium-latest").unwrap(),
    ];
    let catalog = RuntimeModelCatalog::from_config(&fixture);

    let selection =
        catalog.select_view_image_vision_model_with_route(&ContextConfig::default(), None, None);

    assert_eq!(
        selection
            .selected_route
            .as_ref()
            .map(ModelRouteRef::as_string),
        Some("mistral@default/mistral-medium-latest".to_string())
    );
    assert_eq!(
        selection
            .selection
            .candidates
            .iter()
            .filter(|candidate| candidate.model_ref == "mistral@default/mistral-medium-latest")
            .count(),
        1
    );
    assert_eq!(selection.selection.candidates.len(), 2);
    assert!(selection
        .selection
        .candidates
        .iter()
        .all(|candidate| candidate.model_ref != "mistral@default/pixtral-large-latest"));
}
