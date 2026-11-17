use super::*;
use tempfile::tempdir;

fn route_ref(value: &str) -> ModelRouteRef {
    ModelRouteRef::parse_compatible(value).unwrap()
}

#[test]
fn runtime_db_readers_config_round_trip_defaults_and_limits() {
    use holon_storage::RuntimeDbReaderConfig;
    let ordinary = "runtime.db_readers.ordinary_max_connections";
    let preparation = "runtime.db_readers.preparation_max_connections";
    let mut config = HolonConfigFile::default();
    assert!(set_config_key(&mut config, ordinary, "63").is_err());
    assert!(config.runtime.db_readers.is_empty());
    assert_eq!(
        config.runtime.db_readers.resolve().unwrap(),
        RuntimeDbReaderConfig::default()
    );
    assert!(serde_json::to_value(&config)
        .unwrap()
        .get("runtime")
        .is_none());
    let schema = config_schema();
    for (key, default) in [(ordinary, 16), (preparation, 4)] {
        assert_eq!(
            schema
                .iter()
                .find(|entry| entry.key == key)
                .unwrap()
                .default,
            json!(default)
        );
        assert_eq!(get_config_key(&config, key).unwrap(), Value::Null);
        for invalid in ["0", "-1", "1.5", "65", "4294967295", "4294967296"] {
            assert!(
                set_config_key(&mut config, key, invalid).is_err(),
                "{key}={invalid}"
            );
            assert!(config.runtime.db_readers.is_empty());
        }
    }
    set_config_key(&mut config, preparation, "1").unwrap();
    set_config_key(&mut config, ordinary, "63").unwrap();
    assert_eq!(get_config_key(&config, ordinary).unwrap(), json!(63));
    assert!(unset_config_key(&mut config, preparation).is_err());
    assert_eq!(get_config_key(&config, preparation).unwrap(), json!(1));
    let restored: HolonConfigFile =
        serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
    assert_eq!(restored.runtime.db_readers, config.runtime.db_readers);
    unset_config_key(&mut config, ordinary).unwrap();
    unset_config_key(&mut config, preparation).unwrap();
    assert!(config.runtime.db_readers.is_empty());
    assert!(config.runtime.is_empty());
}

#[test]
fn runtime_timezone_configuration_validation_and_compatibility() {
    let mut config: HolonConfigFile = toml::from_str("").unwrap();
    assert!(config.runtime.timezone.is_none());
    set_config_key(&mut config, "runtime.timezone", "Asia/Shanghai").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.timezone").unwrap(),
        serde_json::json!("Asia/Shanghai")
    );
    assert!(set_config_key(&mut config, "runtime.timezone", "invalid/zone").is_err());
    assert_eq!(config.runtime.timezone.as_deref(), Some("Asia/Shanghai"));
    let encoded = toml::to_string(&config).unwrap();
    let decoded: HolonConfigFile = toml::from_str(&encoded).unwrap();
    assert_eq!(decoded.runtime.timezone, config.runtime.timezone);
    assert!(toml::from_str::<HolonConfigFile>("[runtime]\ntimezone = 'invalid/zone'").is_err());
    unset_config_key(&mut config, "runtime.timezone").unwrap();
    assert!(config.runtime.timezone.is_none());
}

#[test]
fn built_in_openai_codex_defaults_to_holon_oauth_profile() {
    let settings_env = HashMap::new();
    let registry = built_in_provider_registry_with_settings(&settings_env).unwrap();
    let openai_codex = registry.get(&ProviderId::openai_codex()).unwrap();

    assert_eq!(openai_codex.auth.source, CredentialSource::AuthProfile);
    assert_eq!(openai_codex.auth.kind, CredentialKind::OAuth);
    assert_eq!(
        openai_codex.auth.profile.as_deref(),
        Some(OPENAI_CODEX_CREDENTIAL_PROFILE)
    );
    assert_eq!(openai_codex.auth.external.as_deref(), Some("codex_cli"));
    assert!(openai_codex.credential.is_none());
}

#[test]
fn built_in_github_copilot_defaults_to_oauth_profile() {
    let settings_env = HashMap::new();
    let registry = built_in_provider_registry_with_settings(&settings_env).unwrap();
    let github_copilot = registry
        .get(&ProviderId::parse("github-copilot").unwrap())
        .unwrap();

    assert_eq!(
        github_copilot.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(github_copilot.base_url, "https://api.githubcopilot.com");
    assert_eq!(github_copilot.auth.source, CredentialSource::AuthProfile);
    assert_eq!(github_copilot.auth.kind, CredentialKind::OAuth);
    assert_eq!(
        github_copilot.auth.profile.as_deref(),
        Some(GITHUB_COPILOT_CREDENTIAL_PROFILE)
    );
    assert!(github_copilot.credential.is_none());
    let integration_id = github_copilot
        .headers
        .iter()
        .find(|(name, _)| *name == "copilot-integration-id")
        .map(|(_, value)| value.to_string());
    assert_eq!(integration_id.as_deref(), Some("holon"));
}

#[test]
fn github_copilot_catalog_is_discovery_only() {
    use holon_domain::model_values::{
        provider_definition, ModelDiscoveryAuth, ModelDiscoveryRoute, ProviderCatalogPolicy,
    };
    let definition = provider_definition("github-copilot").unwrap();
    assert_eq!(
        definition.catalog_policy,
        ProviderCatalogPolicy::DiscoveryOnly
    );
    let discovery = definition.discovery.unwrap();
    assert_eq!(discovery.auth, ModelDiscoveryAuth::Optional);
    assert_eq!(discovery.route, ModelDiscoveryRoute::OpenAiCompatible);
}

#[test]
fn provider_auth_config_client_id_round_trip() {
    let auth: ProviderAuthConfig = serde_json::from_str(
        r#"{"source":"credential_profile","kind":"o_auth","profile":"github-copilot","client_id":"Iv1.client"}"#,
    )
    .unwrap();
    assert_eq!(auth.client_id.as_deref(), Some("Iv1.client"));
    assert_eq!(
        serde_json::to_value(&auth).unwrap().get("client_id"),
        Some(&json!("Iv1.client"))
    );

    let without: ProviderAuthConfig = serde_json::from_str(
        r#"{"source":"credential_profile","kind":"o_auth","profile":"github-copilot"}"#,
    )
    .unwrap();
    assert!(without.client_id.is_none());
    assert!(serde_json::to_value(&without)
        .unwrap()
        .get("client_id")
        .is_none());
}

#[test]
fn github_copilot_configured_headers_keep_builtin_integration_id() {
    let mut config = HolonConfigFile::default();
    let id = ProviderId::parse("github-copilot").unwrap();
    config.providers.insert(
        id.clone(),
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://api.githubcopilot.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(GITHUB_COPILOT_CREDENTIAL_PROFILE.into()),
                external: None,
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
    );
    let registry =
        resolve_provider_registry(&config, &HashMap::new(), &CredentialStoreFile::default())
            .unwrap();
    let runtime = registry.get(&id).unwrap();
    assert_eq!(
        runtime
            .headers
            .iter()
            .find(|(name, _)| *name == "copilot-integration-id")
            .map(|(_, value)| value),
        Some("holon")
    );

    let mut with_override = config;
    with_override.providers.get_mut(&id).unwrap().headers =
        ProviderHeaders::new([("X-Copilot-Test".to_string(), "1".to_string())]).unwrap();
    let registry = resolve_provider_registry(
        &with_override,
        &HashMap::new(),
        &CredentialStoreFile::default(),
    )
    .unwrap();
    let runtime = registry.get(&id).unwrap();
    assert_eq!(
        runtime
            .headers
            .iter()
            .find(|(name, _)| *name == "copilot-integration-id")
            .map(|(_, value)| value),
        Some("holon")
    );
    assert_eq!(
        runtime
            .headers
            .iter()
            .find(|(name, _)| *name == "x-copilot-test")
            .map(|(_, value)| value),
        Some("1")
    );
}

#[test]
fn plan_endpoint_alias_headers_recompute_without_default_endpoint_leak() {
    let mut config = HolonConfigFile::default();
    let id = ProviderId::parse("github-copilot").unwrap();
    let mut endpoints = BTreeMap::new();
    endpoints.insert(
        ProviderEndpointId::parse("default").unwrap(),
        ProviderEndpointConfigFile {
            transport: None,
            base_url: None,
            auth: None,
            headers: ProviderHeaders::new([(
                "x-default-only".to_string(),
                "default-layer".to_string(),
            )])
            .unwrap(),
        },
    );
    endpoints.insert(
        ProviderEndpointId::parse("coding").unwrap(),
        ProviderEndpointConfigFile {
            transport: None,
            base_url: Some("https://api.githubcopilot.com".to_string()),
            auth: None,
            headers: ProviderHeaders::new([("x-coding".to_string(), "coding-layer".to_string())])
                .unwrap(),
        },
    );
    let mut plans = BTreeMap::new();
    plans.insert(
        "coding".to_string(),
        ProviderPlanConfigFile {
            endpoint: Some(ProviderEndpointId::parse("coding").unwrap()),
        },
    );
    config.providers.insert(
        id.clone(),
        ProviderConfigFile {
            headers: ProviderHeaders::new([(
                "x-provider".to_string(),
                "provider-layer".to_string(),
            )])
            .unwrap(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://api.githubcopilot.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(GITHUB_COPILOT_CREDENTIAL_PROFILE.into()),
                external: None,
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints,
            plans,
        },
    );

    let registry =
        resolve_provider_registry(&config, &HashMap::new(), &CredentialStoreFile::default())
            .unwrap();
    let alias = registry
        .get(&ProviderId::parse("github-copilot-coding").unwrap())
        .unwrap();
    assert_eq!(alias.route_endpoint.as_str(), "coding");
    // The built-in identity header and the provider-level header survive.
    assert_eq!(
        alias
            .headers
            .iter()
            .find(|(name, _)| *name == "copilot-integration-id")
            .map(|(_, value)| value),
        Some("holon")
    );
    assert_eq!(
        alias
            .headers
            .iter()
            .find(|(name, _)| *name == "x-provider")
            .map(|(_, value)| value),
        Some("provider-layer")
    );
    // The selected endpoint layer applies...
    assert_eq!(
        alias
            .headers
            .iter()
            .find(|(name, _)| *name == "x-coding")
            .map(|(_, value)| value),
        Some("coding-layer")
    );
    // ...and the default endpoint's own layer does not leak to the alias.
    assert_eq!(
        alias
            .headers
            .iter()
            .find(|(name, _)| *name == "x-default-only")
            .map(|(_, value)| value),
        None
    );

    // The canonical default endpoint keeps its own header layer.
    let canonical = registry.get(&id).unwrap();
    assert_eq!(canonical.route_endpoint.as_str(), "default");
    assert_eq!(
        canonical
            .headers
            .iter()
            .find(|(name, _)| *name == "x-default-only")
            .map(|(_, value)| value),
        Some("default-layer")
    );
}

#[test]
fn provider_auth_client_id_config_keys_round_trip() {
    let mut config = HolonConfigFile::default();
    let id = ProviderId::parse("github-copilot").unwrap();
    config.providers.insert(
        id.clone(),
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://api.githubcopilot.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(GITHUB_COPILOT_CREDENTIAL_PROFILE.into()),
                external: None,
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
    );

    set_provider_config_key(
        &mut config,
        "providers.github-copilot.auth.client_id",
        "Iv1.root-client",
    )
    .unwrap();
    assert_eq!(
        get_provider_config_key(&config, "providers.github-copilot.auth.client_id").unwrap(),
        serde_json::json!("Iv1.root-client")
    );

    set_provider_config_key(
        &mut config,
        "providers.github-copilot.endpoints.default.auth.client_id",
        "Iv1.endpoint-client",
    )
    .unwrap();
    assert_eq!(
        config.providers[&id].endpoints[&ProviderEndpointId::parse("default").unwrap()]
            .auth
            .as_ref()
            .and_then(|auth| auth.client_id.as_deref()),
        Some("Iv1.endpoint-client")
    );
    assert_eq!(
        get_provider_config_key(
            &config,
            "providers.github-copilot.endpoints.default.auth.client_id"
        )
        .unwrap(),
        serde_json::json!("Iv1.endpoint-client")
    );
    // HTTP login and onboarding both read this resolved runtime auth.
    let registry =
        resolve_provider_registry(&config, &HashMap::new(), &CredentialStoreFile::default())
            .unwrap();
    assert_eq!(
        registry.get(&id).unwrap().auth.client_id.as_deref(),
        Some("Iv1.endpoint-client")
    );

    unset_provider_config_key(&mut config, "providers.github-copilot.auth.client_id").unwrap();
    assert_eq!(
        get_provider_config_key(&config, "providers.github-copilot.auth.client_id").unwrap(),
        serde_json::Value::Null
    );
    unset_provider_config_key(
        &mut config,
        "providers.github-copilot.endpoints.default.auth.client_id",
    )
    .unwrap();
    assert_eq!(
        get_provider_config_key(
            &config,
            "providers.github-copilot.endpoints.default.auth.client_id"
        )
        .unwrap(),
        serde_json::Value::Null
    );
    assert_eq!(
        config.providers[&id].endpoints[&ProviderEndpointId::parse("default").unwrap()]
            .auth
            .as_ref()
            .and_then(|auth| auth.client_id.as_deref()),
        None
    );
}

#[test]
fn opencode_go_registers_messages_and_legacy_chat_but_not_responses() {
    let entries = built_in_provider_doc_entries().unwrap();
    let mut endpoints = entries
        .into_iter()
        .filter(|entry| entry.provider.as_str() == "opencode-go")
        .map(|entry| {
            (
                entry.endpoint.as_str().to_string(),
                entry.legacy_provider.as_str().to_string(),
                entry.transport,
                entry.base_url,
            )
        })
        .collect::<Vec<_>>();
    endpoints.sort_by(|left, right| left.0.cmp(&right.0));

    assert_eq!(
        endpoints,
        [
            (
                "default".to_string(),
                "opencode-go".to_string(),
                ProviderTransportKind::OpenAiChatCompletions,
                "https://opencode.ai/inference/go/openai/v1".to_string(),
            ),
            (
                "messages".to_string(),
                "opencode-go-messages".to_string(),
                ProviderTransportKind::AnthropicMessages,
                "https://opencode.ai/inference/go/anthropic/v1".to_string(),
            ),
        ]
    );
}

#[test]
fn tencent_tokenhub_registers_chat_messages_and_responses_endpoints() {
    let entries = built_in_provider_doc_entries().unwrap();
    let mut endpoints = entries
        .into_iter()
        .filter(|entry| entry.provider.as_str() == "tencent-tokenhub")
        .map(|entry| {
            (
                entry.endpoint.as_str().to_string(),
                entry.legacy_provider.as_str().to_string(),
                entry.transport,
                entry.base_url,
            )
        })
        .collect::<Vec<_>>();
    endpoints.sort_by(|left, right| left.0.cmp(&right.0));

    assert_eq!(
        endpoints,
        [
            (
                "default".to_string(),
                "tencent-tokenhub".to_string(),
                ProviderTransportKind::OpenAiChatCompletions,
                "https://tokenhub.tencentmaas.com/v1".to_string(),
            ),
            (
                "messages".to_string(),
                "tencent-tokenhub-messages".to_string(),
                ProviderTransportKind::AnthropicMessages,
                "https://tokenhub.tencentmaas.com".to_string(),
            ),
            (
                "responses".to_string(),
                "tencent-tokenhub-responses".to_string(),
                ProviderTransportKind::OpenAiResponses,
                "https://tokenhub.tencentmaas.com/v1".to_string(),
            ),
        ]
    );
}

#[test]
fn control_auth_mode_parses_known_values() {
    assert_eq!(
        ControlAuthMode::parse("auto").unwrap(),
        ControlAuthMode::Auto
    );
    assert_eq!(
        ControlAuthMode::parse("required").unwrap(),
        ControlAuthMode::Required
    );
    assert_eq!(
        ControlAuthMode::parse("disabled").unwrap(),
        ControlAuthMode::Disabled
    );
}

#[test]
fn anthropic_cache_strategy_parses_supported_values() {
    assert_eq!(
        parse_anthropic_cache_strategy("messages_native").unwrap(),
        AnthropicCacheStrategy::MessagesNative
    );
    assert_eq!(
        parse_anthropic_cache_strategy("claude-code-prompt-cache").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
    assert_eq!(
        parse_anthropic_cache_strategy("current").unwrap(),
        AnthropicCacheStrategy::MessagesNative
    );
    assert_eq!(
        parse_anthropic_cache_strategy("claude-cli-like").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
    let err = parse_anthropic_cache_strategy("unknown")
        .err()
        .expect("unknown strategy should fail");
    assert!(err.to_string().contains("messages_native"));
    assert!(err.to_string().contains("claude_code_prompt_cache"));
}

#[test]
fn anthropic_runtime_cache_strategy_empty_env_uses_default() {
    assert_eq!(
        parse_anthropic_cache_strategy_env("").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
    assert_eq!(
        parse_anthropic_cache_strategy_env("  ").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
}

#[test]
fn anthropic_context_management_struct_default_stays_neutral() {
    assert_eq!(
        AnthropicContextManagementConfig::default().cache_strategy,
        AnthropicCacheStrategy::MessagesNative
    );
    assert!(AnthropicContextManagementConfig::default().betas.is_empty());
}

#[test]
fn comma_separated_values_drop_empty_items() {
    assert_eq!(
        parse_comma_separated_values(" claude-code-20250219, ,prompt-caching-scope-2026-01-05 "),
        vec![
            "claude-code-20250219".to_string(),
            "prompt-caching-scope-2026-01-05".to_string()
        ]
    );
}

#[test]
fn set_get_and_unset_round_trip_model_default() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "model.default", "openai-codex/gpt-5.4").unwrap();
    assert_eq!(
        get_config_key(&config, "model.default").unwrap(),
        json!("openai-codex@default/gpt-5.4")
    );

    unset_config_key(&mut config, "model.default").unwrap();
    assert_eq!(
        get_config_key(&config, "model.default").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_runtime_disable_provider_fallback() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "runtime.disable_provider_fallback", "true").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.disable_provider_fallback").unwrap(),
        Value::Bool(true)
    );

    unset_config_key(&mut config, "runtime.disable_provider_fallback").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.disable_provider_fallback").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_decision_local_onnx_fields() {
    let mut config = HolonConfigFile::default();
    set_config_key(
        &mut config,
        "decision.local_onnx.model_dir",
        "/models/decision",
    )
    .unwrap();
    set_config_key(&mut config, "decision.local_onnx.checksum", "sha256:abc").unwrap();
    assert_eq!(
        get_config_key(&config, "decision.local_onnx.model_dir").unwrap(),
        json!("/models/decision")
    );
    assert_eq!(
        get_config_key(&config, "decision.local_onnx.checksum").unwrap(),
        json!("sha256:abc")
    );

    unset_config_key(&mut config, "decision.local_onnx.model_dir").unwrap();
    unset_config_key(&mut config, "decision.local_onnx.checksum").unwrap();
    assert_eq!(
        get_config_key(&config, "decision.local_onnx.model_dir").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "decision.local_onnx.checksum").unwrap(),
        Value::Null
    );
    assert!(config.decision.local_onnx.is_none());
}

#[test]
fn runtime_scheduler_is_not_a_mutable_config_key() {
    let mut config = HolonConfigFile::default();
    assert!(set_config_key(&mut config, "runtime.scheduler", "canonical").is_err());
    assert!(get_config_key(&config, "runtime.scheduler").is_err());
    assert!(unset_config_key(&mut config, "runtime.scheduler").is_err());
}

#[test]
fn set_get_and_unset_round_trip_api_cors_config() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "api.cors.enabled", "true").unwrap();
    set_config_key(
        &mut config,
        "api.cors.allowed_origins",
        r#"["http://192.168.1.10:5173"]"#,
    )
    .unwrap();
    set_config_key(&mut config, "api.cors.allowed_methods", r#"["GET","POST"]"#).unwrap();
    set_config_key(
        &mut config,
        "api.cors.allowed_headers",
        r#"["content-type","authorization"]"#,
    )
    .unwrap();
    set_config_key(&mut config, "api.cors.allow_credentials", "false").unwrap();
    set_config_key(&mut config, "api.cors.max_age_seconds", "120").unwrap();

    assert_eq!(
        get_config_key(&config, "api.cors.enabled").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_origins").unwrap(),
        json!(["http://192.168.1.10:5173"])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_methods").unwrap(),
        json!(["GET", "POST"])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_headers").unwrap(),
        json!(["content-type", "authorization"])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allow_credentials").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        get_config_key(&config, "api.cors.max_age_seconds").unwrap(),
        json!(120)
    );

    unset_config_key(&mut config, "api.cors.enabled").unwrap();
    unset_config_key(&mut config, "api.cors.allowed_origins").unwrap();
    unset_config_key(&mut config, "api.cors.allowed_methods").unwrap();
    unset_config_key(&mut config, "api.cors.allowed_headers").unwrap();
    unset_config_key(&mut config, "api.cors.allow_credentials").unwrap();
    unset_config_key(&mut config, "api.cors.max_age_seconds").unwrap();
    assert_eq!(
        get_config_key(&config, "api.cors.enabled").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_origins").unwrap(),
        json!([])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_methods").unwrap(),
        json!(default_api_cors_allowed_methods())
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_headers").unwrap(),
        json!(default_api_cors_allowed_headers())
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allow_credentials").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "api.cors.max_age_seconds").unwrap(),
        json!(600)
    );
}

#[test]
fn api_cors_rejects_credentials_with_wildcard_origin() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "api.cors.allowed_origins", r#"["*"]"#).unwrap();

    let error = set_config_key(&mut config, "api.cors.allow_credentials", "true")
        .expect_err("credentials plus wildcard origin should be rejected");

    assert!(
        error.to_string().contains("allow_credentials"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn set_get_and_unset_round_trip_api_csrf_config() {
    let mut config = HolonConfigFile::default();
    set_config_key(
        &mut config,
        "api.csrf.trusted_origins",
        r#"["https://admin.example:8443","http://localhost:7878"]"#,
    )
    .unwrap();

    assert_eq!(
        get_config_key(&config, "api.csrf.trusted_origins").unwrap(),
        json!(["https://admin.example:8443", "http://localhost:7878"])
    );

    unset_config_key(&mut config, "api.csrf.trusted_origins").unwrap();
    assert_eq!(
        get_config_key(&config, "api.csrf.trusted_origins").unwrap(),
        json!([])
    );
}

#[test]
fn api_csrf_rejects_wildcard_and_non_http_origins() {
    for value in [
        r#"["*"]"#,
        r#"["ftp://admin.example"]"#,
        r#"["https://admin.example/path"]"#,
    ] {
        let mut config = HolonConfigFile::default();
        assert!(
            set_config_key(&mut config, "api.csrf.trusted_origins", value).is_err(),
            "invalid CSRF origin list should be rejected: {value}"
        );
    }
}

#[test]
fn set_get_and_unset_round_trip_tool_output_budgets() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "runtime.default_tool_output_tokens", "1500").unwrap();
    set_config_key(&mut config, "runtime.max_tool_output_tokens", "6000").unwrap();

    assert_eq!(
        get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
        json!(1_500)
    );
    assert_eq!(
        get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
        json!(6_000)
    );

    set_config_key(&mut config, "runtime.default_tool_output_tokens", "100000").unwrap();
    set_config_key(&mut config, "runtime.max_tool_output_tokens", "100000").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
        json!(MAX_TOOL_OUTPUT_TOKENS)
    );
    assert_eq!(
        get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
        json!(MAX_TOOL_OUTPUT_TOKENS)
    );

    unset_config_key(&mut config, "runtime.default_tool_output_tokens").unwrap();
    unset_config_key(&mut config, "runtime.max_tool_output_tokens").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_command_task_output_safety_policy() {
    let mut config = HolonConfigFile::default();
    set_config_key(
        &mut config,
        "runtime.command_task_output_retention_bytes",
        "1024",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "runtime.command_task_output_quota_bytes",
        "65536",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "runtime.command_task_min_free_disk_bytes",
        "1048576",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "runtime.command_task_min_free_disk_percent",
        "150",
    )
    .unwrap();

    assert_eq!(
        get_config_key(&config, "runtime.command_task_output_retention_bytes").unwrap(),
        json!(4 * 1024)
    );
    assert_eq!(
        get_config_key(&config, "runtime.command_task_output_quota_bytes").unwrap(),
        json!(65_536)
    );
    assert_eq!(
        get_config_key(&config, "runtime.command_task_min_free_disk_bytes").unwrap(),
        json!(1_048_576)
    );
    assert_eq!(
        get_config_key(&config, "runtime.command_task_min_free_disk_percent").unwrap(),
        json!(100)
    );

    for key in [
        "runtime.command_task_output_retention_bytes",
        "runtime.command_task_output_quota_bytes",
        "runtime.command_task_min_free_disk_bytes",
        "runtime.command_task_min_free_disk_percent",
    ] {
        unset_config_key(&mut config, key).unwrap();
        assert_eq!(get_config_key(&config, key).unwrap(), Value::Null);
    }
}

#[test]
fn set_get_and_unset_round_trip_web_config() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "web.providers.deepseek.kind", "deepseek").unwrap();
    set_config_key(
        &mut config,
        "web.providers.deepseek.credential_profile",
        "deepseek:default",
    )
    .unwrap();
    assert_eq!(
        get_config_key(&config, "web.providers.deepseek.kind").unwrap(),
        json!("deepseek")
    );
    assert_eq!(
        get_config_key(&config, "web.providers.deepseek.credential_profile").unwrap(),
        json!("deepseek:default")
    );
    unset_config_key(&mut config, "web.providers.deepseek").unwrap();
    set_config_key(&mut config, "web.fetch.enabled", "true").unwrap();
    set_config_key(
        &mut config,
        "web.fetch.allowed_hosts",
        "localhost:3000,127.0.0.1:5173",
    )
    .unwrap();
    set_config_key(&mut config, "web.search.provider", "duckduckgo").unwrap();
    set_config_key(&mut config, "web.search.builtin_provider.enabled", "false").unwrap();
    set_config_key(&mut config, "web.search.max_results", "3").unwrap();
    set_config_key(&mut config, "web.search.mode", "aggregate").unwrap();
    set_config_key(&mut config, "web.search.providers", "searx,brave").unwrap();
    set_config_key(&mut config, "web.search.max_provider_attempts", "2").unwrap();
    set_config_key(&mut config, "web.providers.brave.kind", "brave").unwrap();
    set_config_key(&mut config, "web.fetch.max_response_bytes", "12345").unwrap();
    set_config_key(&mut config, "web.fetch.timeout_seconds", "7").unwrap();
    set_config_key(&mut config, "web.fetch.max_redirects", "2").unwrap();

    assert_eq!(
        get_config_key(&config, "web.fetch.enabled").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.allowed_hosts").unwrap(),
        json!(["localhost:3000", "127.0.0.1:5173"])
    );
    assert_eq!(
        get_config_key(&config, "web.search.provider").unwrap(),
        json!("duckduckgo")
    );
    assert_eq!(
        get_config_key(&config, "web.search.builtin_provider.enabled").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        get_config_key(&config, "web.search.max_results").unwrap(),
        json!(3)
    );
    assert_eq!(
        get_config_key(&config, "web.search.mode").unwrap(),
        json!("aggregate")
    );
    assert_eq!(
        get_config_key(&config, "web.search.providers").unwrap(),
        json!(["searx", "brave"])
    );
    assert_eq!(
        get_config_key(&config, "web.search.max_provider_attempts").unwrap(),
        json!(2)
    );
    assert_eq!(
        get_config_key(&config, "web.providers.searx.capabilities").unwrap(),
        Value::Null
    );
    let capabilities = get_config_key(&config, "web.providers.brave.capabilities").unwrap();
    assert_eq!(capabilities["auth"], json!("api_key"));
    assert_eq!(capabilities["status"], json!("supported"));
    assert_eq!(capabilities["default_priority"], json!(80));
    let read_only_capabilities_error =
            "web.providers.brave.capabilities is derived read-only capability metadata; configure web.providers.<name>.kind instead";
    assert_eq!(
        set_config_key(
            &mut config,
            "web.providers.brave.capabilities",
            r#"{"status":"supported"}"#
        )
        .unwrap_err()
        .to_string(),
        read_only_capabilities_error
    );
    assert_eq!(
        unset_config_key(&mut config, "web.providers.brave.capabilities")
            .unwrap_err()
            .to_string(),
        read_only_capabilities_error
    );
    assert_eq!(
        get_config_key(&config, "web.providers.brave.kind").unwrap(),
        json!("brave")
    );
    assert_eq!(
        get_config_key(&config, "web.providers.brave.base_url").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.max_response_bytes").unwrap(),
        json!(12_345)
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.timeout_seconds").unwrap(),
        json!(7)
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.max_redirects").unwrap(),
        json!(2)
    );

    unset_config_key(&mut config, "web.fetch.allowed_hosts").unwrap();
    unset_config_key(&mut config, "web.search.provider").unwrap();
    unset_config_key(&mut config, "web.search.builtin_provider.enabled").unwrap();
    unset_config_key(&mut config, "web.search.mode").unwrap();
    unset_config_key(&mut config, "web.search.providers").unwrap();
    unset_config_key(&mut config, "web.search.max_provider_attempts").unwrap();
    unset_config_key(&mut config, "web.providers.brave").unwrap();
    unset_config_key(&mut config, "web.fetch.max_response_bytes").unwrap();
    unset_config_key(&mut config, "web.fetch.timeout_seconds").unwrap();
    unset_config_key(&mut config, "web.fetch.max_redirects").unwrap();
    assert_eq!(
        get_config_key(&config, "web.fetch.allowed_hosts").unwrap(),
        json!([])
    );
    assert_eq!(
        get_config_key(&config, "web.search.provider").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.search.builtin_provider.enabled").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.search.mode").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.search.providers").unwrap(),
        json!([])
    );
    assert_eq!(
        get_config_key(&config, "web.search.max_provider_attempts").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.max_response_bytes").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_model_fallbacks_accepts_auto_marker() {
    let mut config = HolonConfigFile::default();
    set_config_key(
        &mut config,
        "model.fallbacks",
        r#"["auto","anthropic/claude-sonnet-4"]"#,
    )
    .unwrap();
    assert_eq!(
        get_config_key(&config, "model.fallbacks").unwrap(),
        Value::Array(vec![
            Value::String("auto".to_string()),
            Value::String("anthropic@default/claude-sonnet-4".to_string()),
        ])
    );

    unset_config_key(&mut config, "model.fallbacks").unwrap();
    assert_eq!(
        get_config_key(&config, "model.fallbacks").unwrap(),
        Value::Array(Vec::new())
    );
}

#[test]
fn set_get_and_unset_round_trip_unknown_model_fallback_field() {
    let mut config = HolonConfigFile::default();
    set_config_key(
        &mut config,
        "model.unknown_fallback.prompt_budget_estimated_tokens",
        "64000",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "model.unknown_fallback.runtime_max_output_tokens",
        "4096",
    )
    .unwrap();
    assert_eq!(
        get_config_key(
            &config,
            "model.unknown_fallback.prompt_budget_estimated_tokens"
        )
        .unwrap(),
        json!(64_000)
    );
    assert_eq!(
        get_config_key(&config, "model.unknown_fallback.runtime_max_output_tokens").unwrap(),
        json!(4_096)
    );

    unset_config_key(
        &mut config,
        "model.unknown_fallback.prompt_budget_estimated_tokens",
    )
    .unwrap();
    assert_eq!(
        get_config_key(
            &config,
            "model.unknown_fallback.prompt_budget_estimated_tokens"
        )
        .unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "model.unknown_fallback.runtime_max_output_tokens").unwrap(),
        json!(4_096)
    );

    unset_config_key(
        &mut config,
        "model.unknown_fallback.runtime_max_output_tokens",
    )
    .unwrap();
    assert_eq!(
        get_config_key(&config, "model.unknown_fallback").unwrap(),
        Value::Null
    );
}

#[test]
fn built_in_provider_registry_declares_provider_specific_builtin_search() {
    let registry = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();

    let openai_codex = registry.get(&ProviderId::openai_codex()).unwrap();
    let openai_codex_search = openai_codex.builtin_web_search.as_ref().unwrap();
    assert_eq!(
        openai_codex_search.kind,
        ProviderNativeWebSearchKind::OpenAi
    );
    assert_eq!(openai_codex_search.advertised_tool_type, "web_search");
    assert_eq!(openai_codex_search.backend_kind, "openai_codex_web_search");

    let anthropic = registry.get(&ProviderId::anthropic()).unwrap();
    let anthropic_search = anthropic.builtin_web_search.as_ref().unwrap();
    assert_eq!(
        anthropic_search.kind,
        ProviderNativeWebSearchKind::Anthropic
    );
    assert_eq!(anthropic_search.advertised_tool_type, "web_search_20250305");
    assert_eq!(anthropic_search.backend_kind, "anthropic_web_search");

    let zai = registry.get(&ProviderId::parse("zai").unwrap()).unwrap();
    let zai_search = zai.builtin_web_search.as_ref().unwrap();
    assert_eq!(zai_search.kind, ProviderNativeWebSearchKind::Anthropic);
    assert_eq!(zai_search.advertised_tool_type, "web_search_20250305");
    assert_eq!(zai_search.backend_kind, "zai_web_search_prime");

    let deepseek = registry
        .get(&ProviderId::parse("deepseek").unwrap())
        .unwrap();
    let deepseek_search = deepseek.builtin_web_search.as_ref().unwrap();
    assert_eq!(deepseek_search.kind, ProviderNativeWebSearchKind::DeepSeek);
    assert_eq!(deepseek_search.advertised_tool_type, "web_search_20250305");
    assert_eq!(deepseek_search.backend_kind, "deepseek_web_search");

    let xai = registry.get(&ProviderId::parse("xai").unwrap()).unwrap();
    assert_eq!(xai.transport, ProviderTransportKind::OpenAiResponses);
    assert_eq!(xai.reasoning_effort.as_deref(), Some("medium"));
    assert!(xai.builtin_web_search.is_none());
}

#[test]
fn set_get_and_unset_round_trip_models_catalog_object() {
    let mut config = HolonConfigFile::default();
    set_config_key(
            &mut config,
            "models.catalog",
            r#"{"anthropic/claude-sonnet-4-6":{"prompt_budget_estimated_tokens":32000,"capabilities":{"image_input":true}}}"#,
        )
        .unwrap();
    assert_eq!(
        get_config_key(&config, "models.catalog").unwrap(),
        json!({
            "anthropic/claude-sonnet-4-6": {
                "capabilities": {
                    "image_input": true
                },
                "prompt_budget_estimated_tokens": 32_000
            }
        })
    );

    unset_config_key(&mut config, "models.catalog").unwrap();
    assert_eq!(
        get_config_key(&config, "models.catalog").unwrap(),
        json!({})
    );
}

#[test]
fn models_catalog_rejects_invalid_model_refs() {
    let mut config = HolonConfigFile::default();
    let err = set_config_key(
        &mut config,
        "models.catalog",
        r#"{"gpt-5.4":{"prompt_budget_estimated_tokens":32000}}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("expected provider/model"));
}

#[test]
fn custom_provider_auth_requires_explicit_contract() {
    let id = ProviderId::parse("openrouter").unwrap();
    let auth = ProviderAuthConfig {
        source: CredentialSource::Env,
        kind: CredentialKind::ApiKey,
        env: None,
        profile: None,
        external: None,
        client_id: None,
    };
    let err = super::validate_provider_auth(&id, &auth).unwrap_err();
    assert!(err.to_string().contains("requires auth.env"));
}

#[test]
fn provider_builtin_web_search_rejects_empty_tool_metadata() {
    let id = ProviderId::parse("custom-anthropic").unwrap();
    let config = ProviderConfigFile {
        headers: Default::default(),
        cache_capabilities: None,
        transport: ProviderTransportKind::AnthropicMessages,
        base_url: "https://api.example.com".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::Env,
            kind: CredentialKind::ApiKey,
            env: Some("CUSTOM_API_KEY".into()),
            profile: None,
            external: None,
            client_id: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::Anthropic,
            advertised_tool_type: String::new(),
            backend_kind: "custom_backend".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("advertised_tool_type"));
    assert!(err.to_string().contains("must not be empty"));
}

#[test]
fn provider_builtin_web_search_rejects_transport_kind_mismatch() {
    let id = ProviderId::parse("custom-openai").unwrap();
    let config = ProviderConfigFile {
        headers: Default::default(),
        cache_capabilities: None,
        transport: ProviderTransportKind::OpenAiResponses,
        base_url: "https://api.example.com".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::Env,
            kind: CredentialKind::ApiKey,
            env: Some("CUSTOM_API_KEY".into()),
            profile: None,
            external: None,
            client_id: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::Anthropic,
            advertised_tool_type: "web_search_20250305".into(),
            backend_kind: "custom_backend".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("incompatible with transport"));
}

#[test]
fn provider_builtin_web_search_rejects_wrong_tool_type() {
    let id = ProviderId::parse("custom-openai").unwrap();
    let config = ProviderConfigFile {
        headers: Default::default(),
        cache_capabilities: None,
        transport: ProviderTransportKind::OpenAiResponses,
        base_url: "https://api.example.com".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::Env,
            kind: CredentialKind::ApiKey,
            env: Some("CUSTOM_API_KEY".into()),
            profile: None,
            external: None,
            client_id: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::OpenAi,
            advertised_tool_type: "web_search_20250305".into(),
            backend_kind: "custom_backend".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("web_search_preview"));
}

#[test]
fn provider_builtin_web_search_accepts_codex_tool_type() {
    let id = ProviderId::openai_codex();
    let config = ProviderConfigFile {
        headers: Default::default(),
        cache_capabilities: None,
        transport: ProviderTransportKind::OpenAiCodexResponses,
        base_url: "https://chatgpt.com/backend-api/codex".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::ExternalCli,
            kind: CredentialKind::SessionToken,
            env: None,
            profile: None,
            external: Some("codex_cli".into()),
            client_id: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::OpenAi,
            advertised_tool_type: "web_search".into(),
            backend_kind: "openai_codex_web_search".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    validate_provider_config(&id, &config).unwrap();
}

#[test]
fn provider_builtin_web_search_rejects_wrong_codex_tool_type() {
    let id = ProviderId::openai_codex();
    let config = ProviderConfigFile {
        headers: Default::default(),
        cache_capabilities: None,
        transport: ProviderTransportKind::OpenAiCodexResponses,
        base_url: "https://chatgpt.com/backend-api/codex".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::ExternalCli,
            kind: CredentialKind::SessionToken,
            env: None,
            profile: None,
            external: Some("codex_cli".into()),
            client_id: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::OpenAi,
            advertised_tool_type: "web_search_preview".into(),
            backend_kind: "openai_codex_web_search".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("web_search for OpenAI Codex"));
}

#[test]
fn materialize_provider_config_can_disable_builtin_web_search_for_builtin_provider() {
    let id = ProviderId::openai_codex();
    let built_in = built_in_provider_registry_with_settings(&HashMap::new())
        .unwrap()
        .remove(&id)
        .unwrap();
    assert!(built_in.builtin_web_search.is_some());

    let runtime = super::materialize_provider_config(
        id.clone(),
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                enabled: false,
                kind: ProviderNativeWebSearchKind::OpenAi,
                advertised_tool_type: "web_search".into(),
                backend_kind: "openai_codex_web_search".into(),
            }),
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &HashMap::new(),
        &CredentialStoreFile::default(),
        Some(built_in),
    )
    .unwrap();

    assert_eq!(runtime.id, id);
    assert!(runtime.builtin_web_search.is_none());
}

#[test]
fn builtin_implicit_cache_providers_declare_no_cache_control_capability() {
    let providers = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    for name in ["deepseek", "bigmodel"] {
        let provider = providers.get(&ProviderId::parse(name).unwrap()).unwrap();
        assert!(
            !provider.context_management.cache_capabilities.cache_control,
            "{name} caches implicitly and should not receive cache_control mimicry"
        );
    }
    for name in ["anthropic", "dashscope"] {
        let provider = providers.get(&ProviderId::parse(name).unwrap()).unwrap();
        assert!(
            provider.context_management.cache_capabilities.cache_control,
            "{name} honors explicit cache_control breakpoints"
        );
    }
}

#[test]
fn provider_config_file_overrides_cache_capabilities() {
    let id = ProviderId::parse("deepseek").unwrap();
    let built_in = built_in_provider_registry_with_settings(&HashMap::new())
        .unwrap()
        .remove(&id)
        .unwrap();
    assert!(!built_in.context_management.cache_capabilities.cache_control);

    let runtime = super::materialize_provider_config(
        id.clone(),
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: Some(AnthropicCacheCapabilities {
                cache_control: true,
            }),
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: "https://api.deepseek.com/anthropic".into(),
            auth: Default::default(),
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &HashMap::new(),
        &CredentialStoreFile::default(),
        Some(built_in),
    )
    .unwrap();

    assert!(runtime.context_management.cache_capabilities.cache_control);
}

#[test]
fn provider_cache_capabilities_config_key_round_trip() {
    let mut config = HolonConfigFile::default();
    let key = "providers.deepseek.cache_capabilities.cache_control";

    // No override persisted yet: get reports null instead of the builtin default.
    assert_eq!(get_config_key(&config, key).unwrap(), json!(null));

    set_config_key(&mut config, key, "true").unwrap();
    assert_eq!(get_config_key(&config, key).unwrap(), json!(true));
    assert_eq!(
        config
            .providers
            .get(&ProviderId::parse("deepseek").unwrap())
            .unwrap()
            .cache_capabilities,
        Some(AnthropicCacheCapabilities {
            cache_control: true
        })
    );

    set_config_key(&mut config, key, "false").unwrap();
    assert_eq!(get_config_key(&config, key).unwrap(), json!(false));

    unset_config_key(&mut config, key).unwrap();
    assert_eq!(get_config_key(&config, key).unwrap(), json!(null));
    assert_eq!(
        config
            .providers
            .get(&ProviderId::parse("deepseek").unwrap())
            .unwrap()
            .cache_capabilities,
        None
    );
}

#[test]
fn materialize_provider_config_resolves_env_credentials_from_settings() {
    let mut settings_env = HashMap::new();
    settings_env.insert("OPENROUTER_API_KEY".to_string(), "settings-key".to_string());
    let id = ProviderId::parse("openrouter").unwrap();
    let runtime = super::materialize_provider_config(
        id.clone(),
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://openrouter.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENROUTER_API_KEY".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &settings_env,
        &CredentialStoreFile::default(),
        None,
    )
    .unwrap();

    assert_eq!(runtime.id, id);
    assert_eq!(runtime.credential.as_deref(), Some("settings-key"));
}

#[test]
fn credential_source_accepts_credential_profile_alias() {
    assert_eq!(
        CredentialSource::parse("credential_profile").unwrap(),
        CredentialSource::AuthProfile
    );
    assert_eq!(
        CredentialSource::parse("auth_profile").unwrap(),
        CredentialSource::AuthProfile
    );
    assert_eq!(CredentialSource::AuthProfile.as_str(), "credential_profile");
}

#[test]
fn credential_store_lists_profiles_without_raw_material() {
    let dir = tempdir().unwrap();
    let path = credential_store_path(dir.path());

    let status = set_credential_profile_at(
        &path,
        "openai:default",
        CredentialKind::ApiKey,
        "sk-test-value".into(),
    )
    .unwrap();

    assert_eq!(status.profile, "openai:default");
    let profiles = list_credential_profiles_at(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&profiles).unwrap(),
        json!([{
            "profile": "openai:default",
            "kind": "api_key",
            "configured": true
        }])
    );
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("sk-test-value"));
    assert!(!serde_json::to_string(&profiles)
        .unwrap()
        .contains("sk-test-value"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn materialize_provider_config_resolves_credential_profile() {
    let settings_env = HashMap::new();
    let id = ProviderId::parse("openrouter").unwrap();
    let mut credential_store = CredentialStoreFile::default();
    credential_store.profiles.insert(
        "openrouter:default".into(),
        super::CredentialProfileFile {
            kind: CredentialKind::ApiKey,
            material: "profile-value".into(),
        },
    );

    let runtime = super::materialize_provider_config(
        id.clone(),
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://openrouter.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::ApiKey,
                env: None,
                profile: Some(" openrouter:default ".into()),
                external: None,
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &settings_env,
        &credential_store,
        None,
    )
    .unwrap();

    assert_eq!(runtime.id, id);
    assert_eq!(runtime.credential.as_deref(), Some("profile-value"));
}

#[test]
fn vercel_ai_gateway_prefers_oidc_bearer_token_over_api_key() {
    let mut settings_env = HashMap::new();
    settings_env.insert("VERCEL_OIDC_TOKEN".to_string(), "oidc-token".to_string());
    settings_env.insert("AI_GATEWAY_API_KEY".to_string(), "api-key".to_string());

    let providers = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
    let vercel_ai_gateway = providers
        .get(&ProviderId::parse("vercel-ai-gateway").unwrap())
        .unwrap();

    assert_eq!(
        vercel_ai_gateway.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(vercel_ai_gateway.auth.source, CredentialSource::Env);
    assert_eq!(vercel_ai_gateway.auth.kind, CredentialKind::BearerToken);
    assert_eq!(
        vercel_ai_gateway.auth.env.as_deref(),
        Some("VERCEL_OIDC_TOKEN")
    );
    assert_eq!(vercel_ai_gateway.credential.as_deref(), Some("oidc-token"));
}

#[test]
fn built_in_provider_default_config_resolves_known_and_unknown_provider() {
    let settings_env = HashMap::new();

    let known = super::built_in_provider_default_config_with_settings(
        &ProviderId::parse("zai").unwrap(),
        &settings_env,
    )
    .unwrap()
    .expect("expected built-in default");
    assert_eq!(known.transport, ProviderTransportKind::AnthropicMessages);
    assert_eq!(known.base_url, "https://api.z.ai/api/anthropic");
    assert_eq!(known.auth.source, CredentialSource::None);
    assert_eq!(known.auth.kind, CredentialKind::None);
    assert!(known.auth.env.is_none());

    let unknown = super::built_in_provider_default_config_with_settings(
        &ProviderId::parse("unknown-provider").unwrap(),
        &settings_env,
    )
    .unwrap();
    assert!(unknown.is_none());
}

#[test]
fn materialize_provider_config_preserves_builtin_runtime_fields() {
    let settings_env = HashMap::new();
    let mut built_ins = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
    let id = ProviderId::openai_codex();
    let built_in = built_ins.remove(&id).unwrap();

    let runtime = super::materialize_provider_config(
        id,
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://codex.example/backend-api".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &settings_env,
        &CredentialStoreFile::default(),
        Some(built_in),
    )
    .unwrap();

    assert_eq!(runtime.base_url, "https://codex.example/backend-api");
    assert!(runtime.codex_home.is_some());
    assert_eq!(runtime.originator.as_deref(), Some("codex_cli_rs"));
    assert_eq!(runtime.reasoning_effort.as_deref(), Some("low"));
}

#[test]
fn model_selection_explicit_config_wins() {
    let providers = provider_registry_for_tests(
        Some("openai-key"),
        Some("anthropic-token"),
        tempdir().unwrap().path().join("codex-home"),
    );

    let (default_model, fallback_models) = super::resolve_model_selection_from_explicit(
        Some(route_ref("anthropic/claude-sonnet-4-6")),
        Some(vec![
            super::FallbackEntry::Route(route_ref("openai/gpt-5.4")),
            super::FallbackEntry::Route(route_ref("anthropic/claude-sonnet-4-6")),
        ]),
        &providers,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(
        default_model.as_string(),
        "anthropic@default/claude-sonnet-4-6"
    );
    assert_eq!(
        fallback_models
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>(),
        vec![
            "openai@default/gpt-5.4",
            "anthropic@default/claude-sonnet-4-6"
        ]
    );
}

#[test]
fn model_selection_derives_from_authenticated_providers() {
    let providers = provider_registry_for_tests(
        Some("openai-key"),
        Some("anthropic-token"),
        tempdir().unwrap().path().join("codex-home"),
    );

    let (default_model, fallback_models) =
        super::resolve_model_selection_from_explicit(None, None, &providers, &HashMap::new())
            .unwrap();

    assert_eq!(default_model.as_string(), "openai@default/gpt-5.4");
    assert_eq!(
        fallback_models
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>(),
        vec!["openai@default/gpt-5.4", "anthropic@default/claude-fable-5"]
    );
}

#[test]
fn model_selection_keeps_xiaomi_candidates_after_native_retirement() {
    for (provider, key, endpoint) in [
        ("xiaomi", "XIAOMI_API_KEY", "default"),
        (
            "xiaomi-token-plan",
            "XIAOMI_TOKEN_PLAN_API_KEY",
            "token-plan",
        ),
    ] {
        let mut providers = super::built_in_provider_registry_with_settings(&HashMap::from([(
            key.to_string(),
            "test-key".to_string(),
        )]))
        .unwrap();
        providers.retain(|id, _| id.as_str() == provider);
        let (default_model, fallbacks) =
            super::resolve_model_selection_from_explicit(None, None, &providers, &HashMap::new())
                .unwrap();
        let expected = route_ref(&format!("xiaomi@{endpoint}/mimo-v2.6-pro"));
        assert_eq!(default_model, expected);
        assert_eq!(fallbacks, vec![expected]);
    }
}

#[test]
fn model_selection_explicit_auto_expands_to_authenticated_candidates() {
    let providers = provider_registry_for_tests(
        Some("openai-key"),
        Some("anthropic-token"),
        tempdir().unwrap().path().join("codex-home"),
    );

    let (default_model, fallback_models) = super::resolve_model_selection_from_explicit(
        Some(route_ref("anthropic/claude-sonnet-4-6")),
        Some(vec![super::FallbackEntry::Auto]),
        &providers,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(
        default_model.as_string(),
        "anthropic@default/claude-sonnet-4-6"
    );
    assert_eq!(
        fallback_models
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>(),
        vec!["openai@default/gpt-5.4", "anthropic@default/claude-fable-5"]
    );
}

#[test]
fn model_selection_mixed_auto_and_explicit_fallbacks_expand_in_order() {
    let providers = provider_registry_for_tests(
        Some("openai-key"),
        Some("anthropic-token"),
        tempdir().unwrap().path().join("codex-home"),
    );

    let (default_model, fallback_models) = super::resolve_model_selection_from_explicit(
        Some(route_ref("openai/gpt-5.4")),
        Some(vec![
            super::FallbackEntry::Route(route_ref("minimax/MiniMax-M3")),
            super::FallbackEntry::Auto,
        ]),
        &providers,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(default_model.as_string(), "openai@default/gpt-5.4");
    assert_eq!(
        fallback_models
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>(),
        vec![
            "minimax@default/MiniMax-M3",
            "openai@default/gpt-5.4",
            "anthropic@default/claude-fable-5"
        ]
    );
}

#[test]
fn model_selection_fails_without_explicit_config_or_auth() {
    let providers =
        provider_registry_for_tests(None, None, tempdir().unwrap().path().join("codex-home"));

    let err = super::resolve_model_selection_from_explicit(None, None, &providers, &HashMap::new())
        .unwrap_err();

    assert!(err.to_string().contains("no default model configured"));
    assert!(err.to_string().contains("configure provider credentials"));
}

#[test]
fn model_selection_derives_custom_provider_from_catalog_override() {
    let mut providers = ProviderRegistry::new();
    let id = ProviderId::parse("custom-openai").unwrap();
    providers.insert(
        id.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: id.clone(),
            route_provider: id.clone(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://custom.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("CUSTOM_API_KEY".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            credential: Some("custom-key".into()),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    let mut overrides = HashMap::new();
    overrides.insert(
        ModelRef::parse("custom-openai/model-b").unwrap(),
        ModelRuntimeOverride::default(),
    );
    overrides.insert(
        ModelRef::parse("custom-openai/model-a").unwrap(),
        ModelRuntimeOverride::default(),
    );

    let (default_model, fallback_models) =
        super::resolve_model_selection_from_explicit(None, None, &providers, &overrides).unwrap();

    assert_eq!(default_model.as_string(), "custom-openai@default/model-a");
    assert_eq!(
        fallback_models
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>(),
        vec!["custom-openai@default/model-a"]
    );
}

#[test]
fn url_parser_rejects_invalid_or_non_http_urls() {
    let err = parse_url_value("providers.openai.base_url", "").unwrap_err();
    assert!(err.to_string().contains("non-empty URL"));

    let err = parse_url_value("providers.openai.base_url", "notaurl").unwrap_err();
    assert!(err.to_string().contains("valid absolute URL"));

    let err = parse_url_value("providers.openai.base_url", "ftp://example.com").unwrap_err();
    assert!(err.to_string().contains("http or https URL"));

    parse_url_value("providers.openai.base_url", "https://api.openai.com/v1").unwrap();
}

#[test]
fn model_ref_requires_provider_prefix() {
    let err = ModelRef::parse("gpt-5.4").unwrap_err();
    assert!(err.to_string().contains("expected provider/model"));
}

#[test]
fn model_ref_parses_provider_and_model() {
    let parsed = ModelRef::parse("openai-codex/gpt-5.4").unwrap();
    assert_eq!(parsed.provider, ProviderId::openai_codex());
    assert_eq!(parsed.model, "gpt-5.4");
    assert_eq!(parsed.as_string(), "openai-codex/gpt-5.4");
}

#[test]
fn provider_id_accepts_custom_values() {
    let parsed = ProviderId::parse("vertex").unwrap();
    assert_eq!(parsed.as_str(), "vertex");
}

#[test]
fn model_route_ref_serializes_as_string() {
    let model_ref = ModelRouteRef::parse("openai@default/gpt-5.4").unwrap();
    let encoded = serde_json::to_value(&model_ref).unwrap();
    assert_eq!(encoded, json!("openai@default/gpt-5.4"));

    let decoded: ModelRouteRef = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, model_ref);
}

#[test]
fn persisted_config_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "model.default", "openai/gpt-5.4").unwrap();

    save_persisted_config_at(&path, &config).unwrap();
    let loaded = load_persisted_config_at(&path).unwrap();
    assert_eq!(
        get_config_key(&loaded, "model.default").unwrap(),
        json!("openai@default/gpt-5.4")
    );
}

#[test]
fn tailscale_serve_desired_state_is_available_through_config_schema() {
    let key = "tailscale_serve_desired_enabled";
    let mut config = HolonConfigFile::default();
    assert!(config_schema().iter().any(|entry| entry.key == key));
    assert_eq!(get_config_key(&config, key).unwrap(), Value::Null);
    set_config_key(&mut config, key, "true").unwrap();
    assert_eq!(get_config_key(&config, key).unwrap(), json!(true));
    assert!(set_config_key(&mut config, key, "unknown").is_err());
    assert_eq!(get_config_key(&config, key).unwrap(), json!(true));
    set_config_key(&mut config, key, "false").unwrap();
    assert_eq!(get_config_key(&config, key).unwrap(), json!(false));
    unset_config_key(&mut config, key).unwrap();
    assert_eq!(get_config_key(&config, key).unwrap(), Value::Null);
}

#[test]
fn schema_contains_expected_keys() {
    let keys = config_schema()
        .into_iter()
        .map(|entry| entry.key)
        .collect::<Vec<_>>();
    assert!(keys.contains(&"model.default"));
    assert!(keys.contains(&"auth.mode"));
    assert!(keys.contains(&"auth.oidc.issuer_url"));
    assert!(keys.contains(&"auth.session.absolute_ttl_seconds"));
    assert!(keys.contains(&"models.catalog"));
    assert!(keys.contains(&"model.unknown_fallback"));
    assert!(keys.contains(&"api.projection.max_leaders"));
    assert!(keys.contains(&"api.projection.cache_ttl_ms"));
    assert!(keys.contains(&"api.prompt.attachment_max_bytes"));
    assert!(keys.contains(&"providers.<id>.endpoints.<endpoint_id>.transport"));
    assert!(keys.contains(&"providers.<id>.endpoints.<endpoint_id>.base_url"));
    assert!(keys.contains(&"providers.<id>.plans.<plan_id>.endpoint"));
    assert!(!keys.contains(&"providers.openai-codex.auth_source"));
    assert!(keys.contains(&"runtime.max_output_tokens"));
    assert!(keys.contains(&"runtime.default_tool_output_tokens"));
    assert!(keys.contains(&"runtime.max_tool_output_tokens"));
    assert!(keys.contains(&"runtime.command_task_output_retention_bytes"));
    assert!(keys.contains(&"runtime.command_task_output_quota_bytes"));
    assert!(keys.contains(&"runtime.command_task_min_free_disk_bytes"));
    assert!(keys.contains(&"runtime.command_task_min_free_disk_percent"));
    assert!(keys.contains(&"runtime.disable_provider_fallback"));
    assert!(!keys.contains(&"runtime.scheduler"));
    assert!(keys.contains(&"tui.alternate_screen"));
    assert!(keys.contains(&"web.fetch.enabled"));
    assert!(keys.contains(&"web.fetch.allowed_hosts"));
    assert!(keys.contains(&"web.fetch.max_response_bytes"));
    assert!(keys.contains(&"web.fetch.timeout_seconds"));
    assert!(keys.contains(&"web.fetch.max_redirects"));
    assert!(keys.contains(&"web.search.provider"));
    assert!(keys.contains(&"web.providers.<name>.capabilities"));
}

#[test]
fn api_projection_config_round_trips_and_validates() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "api.projection.max_leaders", "32").unwrap();
    set_config_key(&mut config, "api.projection.cache_ttl_ms", "1000").unwrap();

    assert_eq!(
        get_config_key(&config, "api.projection.max_leaders").unwrap(),
        json!(32)
    );
    assert_eq!(
        get_config_key(&config, "api.projection.cache_ttl_ms").unwrap(),
        json!(1000)
    );

    unset_config_key(&mut config, "api.projection.max_leaders").unwrap();
    unset_config_key(&mut config, "api.projection.cache_ttl_ms").unwrap();
    assert_eq!(
        get_config_key(&config, "api.projection.max_leaders").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "api.projection.cache_ttl_ms").unwrap(),
        Value::Null
    );

    let mut invalid = HolonConfigFile::default();
    assert!(set_config_key(&mut invalid, "api.projection.max_leaders", "0").is_err());
    assert!(set_config_key(&mut invalid, "api.projection.cache_ttl_ms", "0").is_err());
}

#[test]
fn api_prompt_config_defaults_and_round_trips() {
    let mut config = HolonConfigFile::default();
    assert_eq!(config.api.prompt.attachment_max_bytes(), 64 * 1024 * 1024);
    assert_eq!(config.api.prompt.body_max_bytes(), 128 * 1024 * 1024);
    assert_eq!(
        config_schema()
            .into_iter()
            .find(|entry| entry.key == "api.prompt.attachment_max_bytes")
            .unwrap()
            .default,
        json!(64 * 1024 * 1024)
    );
    assert!(config.api.is_empty());

    set_config_key(&mut config, "api.prompt.attachment_max_bytes", "134217728").unwrap();
    assert_eq!(
        get_config_key(&config, "api.prompt.attachment_max_bytes").unwrap(),
        json!(128 * 1024 * 1024)
    );
    assert_eq!(config.api.prompt.body_max_bytes(), 256 * 1024 * 1024);
    assert!(!config.api.is_empty());
    let restored: HolonConfigFile =
        serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
    assert_eq!(restored.api.prompt, config.api.prompt);

    set_config_key(&mut config, "api.prompt.attachment_max_bytes", "3").unwrap();
    assert_eq!(config.api.prompt.body_max_bytes(), 32 * 1024 * 1024);
    unset_config_key(&mut config, "api.prompt.attachment_max_bytes").unwrap();
    assert_eq!(
        get_config_key(&config, "api.prompt.attachment_max_bytes").unwrap(),
        Value::Null
    );
    assert_eq!(config.api.prompt.attachment_max_bytes(), 64 * 1024 * 1024);
    assert!(config.api.is_empty());
}

#[test]
fn local_apps_config_defaults_are_backward_compatible() {
    for config in [
        HolonConfigFile::default(),
        serde_json::from_str::<HolonConfigFile>("{}").unwrap(),
        toml::from_str::<HolonConfigFile>("").unwrap(),
    ] {
        assert!(config.local_apps.is_empty());
        assert_eq!(config.local_apps.max_asset_bytes(), 64 * 1024 * 1024);
        assert!(config.local_apps.csp_header_value().unwrap().is_none());
        assert_eq!(
            get_config_key(&config, "local_apps.max_asset_bytes").unwrap(),
            Value::Null
        );
        assert_eq!(
            get_config_key(&config, "local_apps.csp").unwrap(),
            Value::Null
        );
        assert!(serde_json::to_value(&config)
            .unwrap()
            .get("local_apps")
            .is_none());
        assert!(!toml::to_string(&config).unwrap().contains("[local_apps]"));
    }
    let schema = config_schema();
    assert_eq!(
        schema
            .iter()
            .find(|entry| entry.key == "local_apps.max_asset_bytes")
            .unwrap()
            .default,
        json!(64 * 1024 * 1024)
    );
    assert_eq!(
        schema
            .iter()
            .find(|entry| entry.key == "local_apps.csp")
            .unwrap()
            .default,
        Value::Null
    );
}

#[test]
fn local_apps_config_accepts_positive_u64_limits_without_truncation() {
    let mut config = HolonConfigFile::default();
    for value in [1, u64::MAX] {
        set_config_key(
            &mut config,
            "local_apps.max_asset_bytes",
            &value.to_string(),
        )
        .unwrap();
        let restored: HolonConfigFile =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        crate::validate_local_apps_config(&restored.local_apps).unwrap();
        assert_eq!(restored.local_apps.max_asset_bytes(), value);
        assert_eq!(
            get_config_key(&restored, "local_apps.max_asset_bytes").unwrap(),
            json!(value)
        );
    }
    for value in ["0", "-1", "1.5", "64m", "18446744073709551616"] {
        assert!(set_config_key(&mut config, "local_apps.max_asset_bytes", value).is_err());
        assert_eq!(config.local_apps.max_asset_bytes(), u64::MAX);
    }
}

#[test]
fn local_apps_config_rejects_invalid_headers_without_replacing_policy() {
    let mut config = HolonConfigFile::default();
    let policy = "default-src 'self'";
    set_config_key(&mut config, "local_apps.csp", policy).unwrap();
    for invalid in [
        "default-src 'self'\nscript-src *",
        "default-src 'self'\r\nX-Injected: true",
        "default-src \0",
        "default-src \u{7f}",
    ] {
        let error = set_config_key(&mut config, "local_apps.csp", invalid).unwrap_err();
        assert!(error.to_string().contains("local_apps.csp"));
        assert_eq!(config.local_apps.csp.as_deref(), Some(policy));
        let mut invalid_config = config.local_apps.clone();
        invalid_config.csp = Some(invalid.to_string());
        assert!(invalid_config.csp_header_value().is_err());
    }
    set_config_key(&mut config, "local_apps.csp", "").unwrap();
    assert_eq!(
        config
            .local_apps
            .csp_header_value()
            .unwrap()
            .unwrap()
            .to_str()
            .unwrap(),
        ""
    );
}

#[test]
fn decision_tools_max_calls_per_turn_is_optional() {
    let mut config = HolonConfigFile::default();
    assert_eq!(
        get_config_key(&config, "decision.tools.max_calls_per_turn").unwrap(),
        Value::Null
    );

    set_config_key(&mut config, "decision.tools.max_calls_per_turn", "2").unwrap();
    assert_eq!(
        get_config_key(&config, "decision.tools.max_calls_per_turn").unwrap(),
        json!(2)
    );

    unset_config_key(&mut config, "decision.tools.max_calls_per_turn").unwrap();
    assert_eq!(
        get_config_key(&config, "decision.tools.max_calls_per_turn").unwrap(),
        Value::Null
    );

    assert!(set_config_key(&mut config, "decision.tools.max_calls_per_turn", "0").is_err());
}

#[test]
fn api_projection_defaults_match_gate_constants() {
    let projection = crate::ApiProjectionConfigFile::default();
    assert_eq!(
        projection.max_leaders(),
        crate::DEFAULT_API_PROJECTION_MAX_LEADERS as usize
    );
    assert_eq!(
        projection.cache_ttl_ms(),
        crate::DEFAULT_API_PROJECTION_CACHE_TTL_MS
    );
}

#[test]
fn auth_config_schema_round_trips_persisted_values() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "auth.mode", "oidc").unwrap();
    set_config_key(
        &mut config,
        "auth.oidc.issuer_url",
        "https://issuer.example",
    )
    .unwrap();
    set_config_key(&mut config, "auth.oidc.client_id", "client").unwrap();
    set_config_key(&mut config, "auth.session.idle_ttl_seconds", "120").unwrap();

    assert_eq!(get_config_key(&config, "auth.mode").unwrap(), json!("oidc"));
    assert_eq!(
        get_config_key(&config, "auth.oidc.issuer_url").unwrap(),
        json!("https://issuer.example")
    );
    assert_eq!(
        get_config_key(&config, "auth.session.idle_ttl_seconds").unwrap(),
        json!(120)
    );
}

#[test]
fn load_persisted_config_reads_provider_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let legacy_payload = serde_json::json!({
        "model": {
            "default": "anthropic/claude-sonnet-4-6"
        },
        "runtime": {
            "max_output_tokens": 4096
        },
        "providers": {
            "openai": {
                "transport": "openai_responses",
                "base_url": "https://example.openai.com/v1",
                "auth": {
                    "source": "env",
                    "kind": "api_key",
                    "env": "OPENAI_API_KEY"
                }
            }
        },
        "tui": {
            "alternate_screen": "never"
        }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&legacy_payload).unwrap()).unwrap();

    let config = load_persisted_config_at(&path).unwrap();
    assert_eq!(
        config.model.default,
        Some("anthropic/claude-sonnet-4-6".to_string())
    );
    assert_eq!(config.runtime.max_output_tokens, Some(4_096));
    assert_eq!(
        config.tui.alternate_screen,
        Some(crate::AltScreenMode::Never)
    );
    assert_eq!(
        config
            .providers
            .get(&ProviderId::openai())
            .unwrap()
            .base_url,
        "https://example.openai.com/v1"
    );
}

#[test]
fn provider_endpoint_config_keys_round_trip_default_endpoint() {
    let mut config = HolonConfigFile::default();

    set_config_key(
        &mut config,
        "providers.openai.endpoints.default.transport",
        "openai_chat_completions",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "providers.openai.endpoints.default.base_url",
        "https://example.openai.test/v1",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "providers.openai.endpoints.default.auth.env",
        "CUSTOM_OPENAI_API_KEY",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "providers.openai.plans.coding.endpoint",
        "default",
    )
    .unwrap();

    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.transport").unwrap(),
        json!("openai_chat_completions")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.transport").unwrap(),
        json!("openai_responses")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.base_url").unwrap(),
        json!("https://example.openai.test/v1")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.auth.env").unwrap(),
        json!("CUSTOM_OPENAI_API_KEY")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.plans.coding.endpoint").unwrap(),
        json!("default")
    );

    unset_config_key(&mut config, "providers.openai.endpoints.default.auth.env").unwrap();
    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.auth.env").unwrap(),
        json!(null)
    );
}

#[test]
fn unknown_provider_subkeys_report_unknown_key() {
    let config = HolonConfigFile::default();
    let err = get_config_key(&config, "providers.openai.auth_source").unwrap_err();
    assert!(err.to_string().contains("unknown config key"));
}

#[test]
fn resolve_disable_provider_fallback_rejects_invalid_env_override() {
    let err = super::resolve_disable_provider_fallback_override(
        Some("maybe"),
        &HolonConfigFile::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("HOLON_DISABLE_PROVIDER_FALLBACK expects a boolean"));
}

#[test]
fn visual_model_defaults_use_null_for_auto() {
    let mut config = HolonConfigFile::default();

    assert_eq!(
        get_config_key(&config, "vision.default").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "image_generation.default").unwrap(),
        Value::Null
    );

    set_config_key(&mut config, "vision.default", "auto").unwrap();
    assert!(config.vision.default.is_none());
    assert!(config.vision.is_empty());

    set_config_key(&mut config, "image_generation.default", "auto").unwrap();
    assert!(config.image_generation.default.is_none());
    assert!(config.image_generation.is_empty());
}

#[test]
fn image_generation_config_accepts_explicit_model_ref() {
    let mut config = HolonConfigFile::default();

    set_config_key(
        &mut config,
        "image_generation.default",
        "openai-codex/gpt-5.5",
    )
    .unwrap();

    assert_eq!(
        config.image_generation.default.as_deref(),
        Some("openai-codex@default/gpt-5.5")
    );
    assert_eq!(
        get_config_key(&config, "image_generation.default").unwrap(),
        json!("openai-codex@default/gpt-5.5")
    );

    unset_config_key(&mut config, "image_generation.default").unwrap();
    assert_eq!(
        get_config_key(&config, "image_generation.default").unwrap(),
        Value::Null
    );
}

#[test]
fn invalid_model_override_percent_is_rejected() {
    let mut config = HolonConfigFile::default();
    let err = set_config_key(
        &mut config,
        "models.catalog",
        r#"{"anthropic/claude-sonnet-4-6":{"effective_context_window_percent":0}}"#,
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("effective_context_window_percent expects an integer from 1 to 100"));
}

#[test]
fn provider_doc_entries_are_sorted_and_populated() {
    let entries =
        built_in_provider_doc_entries().expect("built_in_provider_doc_entries should succeed");
    assert!(!entries.is_empty(), "should have at least one provider");
    // Legacy provider refs remain sorted for stable generated docs.
    for i in 1..entries.len() {
        assert!(
            entries[i - 1].id.as_str() <= entries[i].id.as_str(),
            "entries must be sorted by id"
        );
    }

    let dashscope_token_plan = entries
        .iter()
        .find(|entry| entry.legacy_provider.as_str() == "dashscope-token-plan")
        .expect("dashscope-token-plan doc entry");
    assert_eq!(dashscope_token_plan.id.as_str(), "dashscope-token-plan");
    assert_eq!(dashscope_token_plan.provider.as_str(), "dashscope");
    assert_eq!(dashscope_token_plan.endpoint.as_str(), "token-plan");

    let volcengine_agent = entries
        .iter()
        .find(|entry| entry.legacy_provider.as_str() == "volcengine-agent")
        .expect("volcengine-agent doc entry");
    assert_eq!(volcengine_agent.provider.as_str(), "volcengine");
    assert_eq!(volcengine_agent.endpoint.as_str(), "plan");
}

#[test]
fn reclamation_config_roundtrips_and_requires_positive_grace() {
    let mut config = HolonConfigFile::default();
    for (key, value, expected) in [
        (
            "runtime.reclamation.idle_retirement_enabled",
            "true",
            serde_json::json!(true),
        ),
        (
            "runtime.reclamation.reminders_enabled",
            "true",
            serde_json::json!(true),
        ),
        (
            "runtime.reclamation.idle_grace_seconds",
            "60",
            serde_json::json!(60),
        ),
    ] {
        set_config_key(&mut config, key, value).unwrap();
        assert_eq!(get_config_key(&config, key).unwrap(), expected);
        unset_config_key(&mut config, key).unwrap();
        assert_eq!(
            get_config_key(&config, key).unwrap(),
            serde_json::Value::Null
        );
    }
    assert!(set_config_key(&mut config, "runtime.reclamation.idle_grace_seconds", "0").is_err());
}

#[test]
fn reclamation_reminders_default_to_enabled_in_schema() {
    let entry = config_schema()
        .into_iter()
        .find(|entry| entry.key == "runtime.reclamation.reminders_enabled")
        .expect("reminders_enabled schema entry");
    assert_eq!(entry.default, serde_json::json!(true));
    let retirement = config_schema()
        .into_iter()
        .find(|entry| entry.key == "runtime.reclamation.idle_retirement_enabled")
        .expect("idle_retirement_enabled schema entry");
    assert_eq!(retirement.default, serde_json::json!(false));
}

#[test]
fn state_memory_limits_have_explicit_defaults_and_positive_config_keys() {
    let mut config = HolonConfigFile::default();
    let defaults = super::StateMemoryLimits::default();
    assert_eq!(config.runtime.state_memory.resolve().unwrap(), defaults);
    assert!(config.runtime.is_empty());
    for (key, expected) in [
        ("runtime.state_memory.history_per_agent_bytes", 33554432u64),
        ("runtime.state_memory.history_host_bytes", 536870912u64),
        ("runtime.state_memory.pending_per_agent_count", 1024u64),
        ("runtime.state_memory.pending_per_agent_bytes", 33554432u64),
        ("runtime.state_memory.pending_host_bytes", 268435456u64),
        ("runtime.state_memory.record_limit", 65536u64),
    ] {
        let entry = config_schema()
            .into_iter()
            .find(|entry| entry.key == key)
            .unwrap();
        assert_eq!(entry.default, serde_json::json!(expected));
        set_config_key(&mut config, key, "17").unwrap();
        assert_eq!(get_config_key(&config, key).unwrap(), serde_json::json!(17));
        assert!(!config.runtime.is_empty());
        let roundtrip: HolonConfigFile =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            get_config_key(&roundtrip, key).unwrap(),
            serde_json::json!(17)
        );
        assert!(set_config_key(&mut config, key, "0").is_err());
        assert!(set_config_key(&mut config, key, "-1").is_err());
        unset_config_key(&mut config, key).unwrap();
        assert_eq!(
            get_config_key(&config, key).unwrap(),
            serde_json::Value::Null
        );
        let field = key.rsplit('.').next().unwrap();
        let raw = serde_json::json!({"runtime":{"state_memory":{(field):0}}});
        assert!(serde_json::from_value::<HolonConfigFile>(raw).is_err());
    }
    assert!(config.runtime.is_empty());
    config.runtime.state_memory.pending_host_bytes = Some(0);
    assert!(config.runtime.state_memory.resolve().is_err());
}
