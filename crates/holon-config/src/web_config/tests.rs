use super::*;
use crate::{
    CredentialProfileFile, CredentialStoreFile, WebCommandProviderConfigFile, WebProviderConfigFile,
};
use std::collections::BTreeMap;

fn credential_store_with(profile: &str, material: &str) -> CredentialStoreFile {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        profile.to_string(),
        CredentialProfileFile {
            kind: crate::CredentialKind::ApiKey,
            material: material.to_string(),
        },
    );
    CredentialStoreFile { profiles }
}

fn provider_file(kind: WebProviderKind) -> WebProviderConfigFile {
    WebProviderConfigFile {
        kind,
        base_url: None,
        engine_id: None,
        credential_profile: None,
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    }
}

#[test]
fn materialize_resolves_api_key_from_credential_profile() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "my_brave".to_string(),
        WebProviderConfigFile {
            kind: WebProviderKind::Brave,
            base_url: None,
            engine_id: None,
            credential_profile: Some("brave_key".to_string()),
            model_route: None,
            command: None,
            output: None,
            limits: Default::default(),
        },
    );
    let file = crate::WebConfigFile {
        fetch: Default::default(),
        search: Default::default(),
        providers,
    };
    let store = credential_store_with("brave_key", "test-api-key-123");
    let config = materialize_web_config(&file, &store).unwrap();
    let provider = config.providers.get("my_brave").unwrap();
    assert_eq!(provider.api_key, "test-api-key-123");
    assert_eq!(provider.kind, WebProviderKind::Brave);
}

#[test]
fn materialize_empty_api_key_without_credential_profile() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "my_tavily".to_string(),
        WebProviderConfigFile {
            kind: WebProviderKind::Tavily,
            base_url: None,
            engine_id: None,
            credential_profile: None,
            model_route: None,
            command: None,
            output: None,
            limits: Default::default(),
        },
    );
    let file = crate::WebConfigFile {
        fetch: Default::default(),
        search: Default::default(),
        providers,
    };
    let store = CredentialStoreFile::default();
    let config = materialize_web_config(&file, &store).unwrap();
    let provider = config.providers.get("my_tavily").unwrap();
    assert!(provider.api_key.is_empty());
}

#[test]
fn materialize_missing_credential_profile_yields_empty_key() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "my_brave".to_string(),
        WebProviderConfigFile {
            kind: WebProviderKind::Brave,
            base_url: None,
            engine_id: None,
            credential_profile: Some("missing_profile".to_string()),
            model_route: None,
            command: None,
            output: None,
            limits: Default::default(),
        },
    );
    let file = crate::WebConfigFile {
        fetch: Default::default(),
        search: Default::default(),
        providers,
    };
    // Credential store does not contain "missing_profile"
    let store = credential_store_with("other", "irrelevant");
    let config = materialize_web_config(&file, &store).unwrap();
    let provider = config.providers.get("my_brave").unwrap();
    assert!(provider.api_key.is_empty());
}

#[test]
fn materialize_non_api_key_credential_kind_yields_empty_key() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "my_brave".to_string(),
        WebProviderConfigFile {
            kind: WebProviderKind::Brave,
            base_url: None,
            engine_id: None,
            credential_profile: Some("session_token".to_string()),
            model_route: None,
            command: None,
            output: None,
            limits: Default::default(),
        },
    );
    let file = crate::WebConfigFile {
        fetch: Default::default(),
        search: Default::default(),
        providers,
    };
    // Profile exists but kind is SessionToken, not ApiKey
    let mut store = CredentialStoreFile::default();
    store.profiles.insert(
        "session_token".to_string(),
        crate::CredentialProfileFile {
            kind: crate::CredentialKind::SessionToken,
            material: "some-session-hash".to_string(),
        },
    );
    let config = materialize_web_config(&file, &store).unwrap();
    let provider = config.providers.get("my_brave").unwrap();
    assert!(provider.api_key.is_empty());
}

#[test]
fn web_search_config_trims_primary_provider() {
    let file = crate::WebSearchConfigFile {
        provider: Some("  openai-native  ".to_string()),
        ..Default::default()
    };

    let config = WebSearchConfig::from_file(&file).unwrap();

    assert_eq!(config.provider, "openai-native");
}

#[test]
fn native_search_provider_uses_normalized_provider_id() {
    let mut config = WebConfig::default();
    config.search.provider = " openai-native ".to_string();
    config.providers.insert(
        "openai-native".to_string(),
        WebProviderConfig {
            kind: WebProviderKind::OpenAiNative,
            base_url: None,
            engine_id: None,
            api_key: String::new(),
            model_route: None,
            command: None,
            output: None,
            limits: WebProviderLimitsConfig::default(),
        },
    );

    assert_eq!(
        config.native_search_provider(),
        Some(("openai-native".to_string(), WebProviderKind::OpenAiNative))
    );
}

#[test]
fn try_from_web_config_file_propagates_provider_errors() {
    let mut file = crate::WebConfigFile::default();
    let mut provider = provider_file(WebProviderKind::Command);
    provider.command = Some(WebCommandProviderConfigFile {
        argv: vec!["search".to_string()],
    });
    file.providers.insert("cmd".to_string(), provider);

    let error = WebConfig::try_from(&file).unwrap_err().to_string();

    assert!(error.contains("requires output.mapping"));
}

#[test]
fn materialize_rejects_command_fields_on_non_command_provider() {
    let mut file = crate::WebConfigFile::default();
    let mut provider = provider_file(WebProviderKind::Brave);
    provider.command = Some(WebCommandProviderConfigFile {
        argv: vec!["search".to_string()],
    });
    file.providers.insert("brave".to_string(), provider);

    let error = materialize_web_config(&file, &CredentialStoreFile::default())
        .unwrap_err()
        .to_string();

    assert!(error.contains("must not configure command.argv"));
}

#[test]
fn materialize_rejects_command_limits_on_non_command_provider() {
    let mut file = crate::WebConfigFile::default();
    let mut provider = provider_file(WebProviderKind::Brave);
    provider.limits.timeout_ms = Some(1000);
    file.providers.insert("brave".to_string(), provider);

    let error = materialize_web_config(&file, &CredentialStoreFile::default())
        .unwrap_err()
        .to_string();

    assert!(error.contains("must not configure command limits"));
}

#[test]
fn materialize_rejects_google_provider_without_engine_id() {
    let mut file = crate::WebConfigFile::default();
    file.providers
        .insert("google".to_string(), provider_file(WebProviderKind::Google));

    let error = materialize_web_config(&file, &CredentialStoreFile::default())
        .unwrap_err()
        .to_string();

    assert!(error.contains("kind=google requires engine_id"));
}

#[test]
fn materialize_rejects_google_provider_with_blank_engine_id() {
    let mut file = crate::WebConfigFile::default();
    let mut provider = provider_file(WebProviderKind::Google);
    provider.engine_id = Some("   ".to_string());
    file.providers.insert("google".to_string(), provider);

    let error = materialize_web_config(&file, &CredentialStoreFile::default())
        .unwrap_err()
        .to_string();

    assert!(error.contains("kind=google requires engine_id"));
}

#[test]
fn materialize_rejects_engine_id_on_non_google_provider() {
    let mut file = crate::WebConfigFile::default();
    let mut provider = provider_file(WebProviderKind::Brave);
    provider.engine_id = Some("engine-cx-123".to_string());
    file.providers.insert("brave".to_string(), provider);

    let error = materialize_web_config(&file, &CredentialStoreFile::default())
        .unwrap_err()
        .to_string();

    assert!(error.contains("must not configure engine_id"));
}

#[test]
fn materialize_carries_google_engine_id_and_api_key() {
    let mut file = crate::WebConfigFile::default();
    let mut provider = provider_file(WebProviderKind::Google);
    provider.engine_id = Some("017576662512468239146:omuauf_lfve".to_string());
    provider.credential_profile = Some("google_key".to_string());
    file.providers.insert("google".to_string(), provider);
    let store = credential_store_with("google_key", "test-api-key-123");

    let config = materialize_web_config(&file, &store).unwrap();

    let provider = config.providers.get("google").unwrap();
    assert_eq!(provider.kind, WebProviderKind::Google);
    assert_eq!(
        provider.engine_id.as_deref(),
        Some("017576662512468239146:omuauf_lfve")
    );
    assert_eq!(provider.api_key, "test-api-key-123");
}

#[test]
fn google_capabilities_advertise_region_or_language_support() {
    let capabilities = WebProviderKind::Google.capabilities();
    assert_eq!(capabilities.auth, WebProviderAuthClass::ApiKey);
    assert_eq!(capabilities.cost_class, WebProviderCostClass::Paid);
    assert!(capabilities.supports_region_or_language);
    assert!(!capabilities.supports_full_content);
    assert!(!capabilities.supports_domain_filter);
    assert_eq!(capabilities.status, WebProviderSupportStatus::Supported);
}

#[test]
fn deepseek_capabilities_do_not_advertise_region_or_language_support() {
    // DeepSeek's search API has no language or region parameter surface.
    assert!(
        !WebProviderKind::DeepSeek
            .capabilities()
            .supports_region_or_language
    );
}

#[test]
fn searxng_and_brave_capabilities_advertise_region_or_language_support() {
    assert!(
        WebProviderKind::Searxng
            .capabilities()
            .supports_region_or_language
    );
    assert!(
        WebProviderKind::Brave
            .capabilities()
            .supports_region_or_language
    );
}

#[test]
fn provider_capabilities_mark_reserved_kinds() {
    assert_eq!(
        WebProviderKind::OpenAiNative.capabilities().status,
        WebProviderSupportStatus::NativeOnly
    );
    assert_eq!(WebProviderKind::Brave.capabilities().default_priority, 80);
    assert_eq!(
        WebProviderKind::Command.capabilities().status,
        WebProviderSupportStatus::Supported
    );
}

#[test]
fn deepseek_kind_and_endpoint_contract() {
    let mut provider =
        WebProviderConfig::from_file("deepseek", &provider_file(WebProviderKind::DeepSeek))
            .unwrap();
    assert_eq!(serde_json::to_value(provider.kind).unwrap(), "deepseek");
    assert_eq!(
        serde_json::from_str::<WebProviderKind>("\"deepseek\"").unwrap(),
        WebProviderKind::DeepSeek
    );
    assert_eq!(
        deepseek_tool_url(&provider, "fetch").unwrap().as_str(),
        "https://api.deepseek.com/v1/tools/fetch"
    );
    for base in [
        "https://api.deepseek.com",
        "https://api.deepseek.com/",
        "https://api.deepseek.com/v1",
        "https://api.deepseek.com/v1/",
    ] {
        provider.base_url = Some(base.into());
        assert_eq!(
            deepseek_tool_url(&provider, "search").unwrap().as_str(),
            "https://api.deepseek.com/v1/tools/search"
        );
    }
    for base in [
        "not a url",
        "file:///etc/passwd",
        "https://user:secret@example.com",
        "https://example.com?secret=1",
        "https://example.com/#secret",
    ] {
        provider.base_url = Some(base.into());
        let error = deepseek_tool_url(&provider, "fetch")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret"));
    }
    let metadata = provider.kind.capabilities();
    assert_eq!(metadata.auth, WebProviderAuthClass::ApiKey);
    assert_eq!(metadata.status, WebProviderSupportStatus::Supported);
}

#[test]
fn tavily_extract_endpoint_contract() {
    let mut provider =
        WebProviderConfig::from_file("tavily", &provider_file(WebProviderKind::Tavily)).unwrap();
    assert_eq!(
        tavily_extract_url(&provider).unwrap().as_str(),
        "https://api.tavily.com/extract"
    );
    for base in [
        "https://api.tavily.com",
        "https://api.tavily.com/",
        "https://proxy.internal/tavily",
        "https://proxy.internal/tavily/",
    ] {
        provider.base_url = Some(base.into());
        assert!(tavily_extract_url(&provider)
            .unwrap()
            .as_str()
            .ends_with("/extract"));
    }
    provider.base_url = Some("https://proxy.internal/tavily".into());
    assert_eq!(
        tavily_extract_url(&provider).unwrap().as_str(),
        "https://proxy.internal/tavily/extract"
    );
    for base in [
        "not a url",
        "file:///etc/passwd",
        "https://user:secret@example.com",
        "https://example.com?secret=1",
        "https://example.com/#secret",
    ] {
        provider.base_url = Some(base.into());
        let error = tavily_extract_url(&provider).unwrap_err().to_string();
        assert!(!error.contains("secret"));
    }
}

#[test]
fn firecrawl_scrape_endpoint_contract() {
    let mut provider =
        WebProviderConfig::from_file("firecrawl", &provider_file(WebProviderKind::Firecrawl))
            .unwrap();
    assert_eq!(serde_json::to_value(provider.kind).unwrap(), "firecrawl");
    assert_eq!(
        firecrawl_scrape_url(&provider).unwrap().as_str(),
        "https://api.firecrawl.dev/v2/scrape"
    );
    // A version segment in the base URL is normalized to the scrape v2
    // endpoint instead of being doubled.
    for (base, expected) in [
        (
            "https://api.firecrawl.dev/",
            "https://api.firecrawl.dev/v2/scrape",
        ),
        (
            "https://api.firecrawl.dev/v2",
            "https://api.firecrawl.dev/v2/scrape",
        ),
        (
            "https://api.firecrawl.dev/v2/",
            "https://api.firecrawl.dev/v2/scrape",
        ),
        (
            "https://api.firecrawl.dev/v1",
            "https://api.firecrawl.dev/v2/scrape",
        ),
        (
            "https://proxy.internal/firecrawl",
            "https://proxy.internal/firecrawl/v2/scrape",
        ),
        (
            "https://proxy.internal/firecrawl/",
            "https://proxy.internal/firecrawl/v2/scrape",
        ),
    ] {
        provider.base_url = Some(base.into());
        assert_eq!(firecrawl_scrape_url(&provider).unwrap().as_str(), expected,);
    }
    for base in [
        "not a url",
        "file:///etc/passwd",
        "https://user:secret@example.com",
        "https://example.com?secret=1",
        "https://example.com/#secret",
    ] {
        provider.base_url = Some(base.into());
        let error = firecrawl_scrape_url(&provider).unwrap_err().to_string();
        assert!(!error.contains("secret"), "{base}");
    }
}

#[test]
fn deepseek_reuses_llm_credential_profile_without_serializing_key() {
    let mut file = crate::WebConfigFile::default();
    let mut provider = provider_file(WebProviderKind::DeepSeek);
    provider.credential_profile = Some("deepseek:default".into());
    file.providers.insert("deepseek".into(), provider);
    let config = materialize_web_config(
        &file,
        &credential_store_with("deepseek:default", "secret-fixture"),
    )
    .unwrap();
    assert_eq!(config.providers["deepseek"].api_key, "secret-fixture");
    assert!(!serde_json::to_string(&config)
        .unwrap()
        .contains("secret-fixture"));
}

fn model_provider_registry(authenticated: &[&str]) -> crate::ProviderRegistry {
    let built_ins = crate::builtin_providers::built_in_provider_registry_with_settings(
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let mut registry = crate::ProviderRegistry::new();
    for id in authenticated {
        let provider_id = crate::ProviderId::parse(id).unwrap();
        let mut provider = built_ins
            .get(&provider_id)
            .cloned()
            .unwrap_or_else(|| panic!("builtin registry must contain {id}"));
        provider.credential = Some("test-credential".into());
        registry.insert(provider_id, provider);
    }
    registry
}

fn expected_preferred_route(provider: &str) -> String {
    BuiltInModelCatalog::default()
        .preferred_route_for_provider(&crate::ProviderId::parse(provider).unwrap())
        .unwrap()
        .as_string()
}

#[test]
fn native_search_default_parses_override_values() {
    assert_eq!(
        NativeSearchDefault::parse(None).unwrap(),
        NativeSearchDefault::Auto
    );
    assert_eq!(
        NativeSearchDefault::parse(Some("  ")).unwrap(),
        NativeSearchDefault::Auto
    );
    assert_eq!(
        NativeSearchDefault::parse(Some("auto")).unwrap(),
        NativeSearchDefault::Auto
    );
    assert_eq!(
        NativeSearchDefault::parse(Some("OFF")).unwrap(),
        NativeSearchDefault::Off
    );
    assert_eq!(
        NativeSearchDefault::parse(Some("openai-codex")).unwrap(),
        NativeSearchDefault::Provider("openai-codex".into())
    );
    let error = NativeSearchDefault::parse(Some("not a provider id!")).unwrap_err();
    assert!(error
        .to_string()
        .contains("web.search.native_default must be auto, off, or a valid provider id"));
}

#[test]
fn derived_native_search_providers_cover_only_dispatchable_authenticated_kinds() {
    let mut web_config = WebConfig::default();
    // openai-codex declares the OpenAI native kind (standalone dispatchable);
    // deepseek declares the DeepSeek native kind (conversation-only today).
    let providers = model_provider_registry(&["openai-codex", "deepseek"]);
    let derived =
        derived_native_search_providers(&web_config, &providers, &std::collections::HashMap::new());
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].0, "openai-codex");
    assert_eq!(derived[0].1.kind, WebProviderKind::OpenAiNative);
    assert_eq!(
        derived[0].1.model_route.as_deref(),
        Some(expected_preferred_route("openai-codex").as_str())
    );

    // Unauthenticated providers never derive entries.
    let mut providers = model_provider_registry(&["openai-codex"]);
    let id = crate::ProviderId::parse("openai-codex").unwrap();
    let provider = providers.get_mut(&id).unwrap();
    // Simulate a host without a codex CLI login: keep the builtin
    // AuthProfile shape, drop the materialized credential, and point
    // `codex_home` at a directory that cannot contain an auth file.
    provider.credential = None;
    provider.codex_home = Some(std::path::PathBuf::from(
        "/nonexistent/holon-test-codex-home",
    ));
    assert!(derived_native_search_providers(
        &web_config,
        &providers,
        &std::collections::HashMap::new()
    )
    .is_empty());

    // Disabled search disables derivation.
    web_config.search.enabled = false;
    let providers = model_provider_registry(&["openai-codex"]);
    assert!(derived_native_search_providers(
        &web_config,
        &providers,
        &std::collections::HashMap::new()
    )
    .is_empty());
}

#[test]
fn derived_native_search_providers_respect_explicit_entries_and_override() {
    let providers = model_provider_registry(&["openai", "openai-codex"]);

    // Explicit web.providers entries win over derived entries.
    let mut web_config = WebConfig::default();
    web_config.providers.insert(
        "openai-codex".into(),
        WebProviderConfig {
            kind: WebProviderKind::OpenAiNative,
            base_url: None,
            engine_id: None,
            api_key: String::new(),
            model_route: Some("openai-codex@default/explicit-route".into()),
            command: None,
            output: None,
            limits: Default::default(),
        },
    );
    let derived =
        derived_native_search_providers(&web_config, &providers, &std::collections::HashMap::new());
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].0, "openai");

    // `off` disables derivation entirely.
    let mut web_config = WebConfig::default();
    web_config.search.native_search_default = NativeSearchDefault::Off;
    assert!(derived_native_search_providers(
        &web_config,
        &providers,
        &std::collections::HashMap::new()
    )
    .is_empty());

    // An explicit provider pin derives only that provider, and pinning a
    // provider without a dispatchable native kind derives nothing.
    let mut web_config = WebConfig::default();
    web_config.search.native_search_default = NativeSearchDefault::Provider("openai".into());
    let derived =
        derived_native_search_providers(&web_config, &providers, &std::collections::HashMap::new());
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].0, "openai");
    web_config.search.native_search_default = NativeSearchDefault::Provider("deepseek".into());
    let deepseek_only = model_provider_registry(&["openai", "openai-codex", "deepseek"]);
    assert!(derived_native_search_providers(
        &web_config,
        &deepseek_only,
        &std::collections::HashMap::new()
    )
    .is_empty());
}

#[test]
fn derived_standalone_search_providers_discover_official_deepseek_credential() {
    // An official DeepSeek model credential derives the stable standalone
    // entry without any `web.providers` declaration.
    let web_config = WebConfig::default();
    let providers = model_provider_registry(&["deepseek"]);
    let derived = derived_standalone_search_providers(&web_config, &providers);
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].0, DERIVED_DEEPSEEK_SEARCH_PROVIDER_ID);
    let provider = &derived[0].1;
    assert_eq!(provider.kind, WebProviderKind::DeepSeek);
    assert_eq!(provider.api_key, "test-credential");
    // The discovered key stays pinned to the official endpoint: the derived
    // entry never carries the discovered base URL.
    assert_eq!(provider.base_url, None);
    assert!(provider.model_route.is_none());
}

#[test]
fn derived_standalone_search_providers_skip_ineligible_sources() {
    // Disabled standalone search derives nothing.
    let mut web_config = WebConfig::default();
    web_config.search.enabled = false;
    let providers = model_provider_registry(&["deepseek"]);
    assert!(derived_standalone_search_providers(&web_config, &providers).is_empty());

    // A provider without a materialized credential derives nothing.
    let id = crate::ProviderId::parse("deepseek").unwrap();
    let mut providers = model_provider_registry(&["deepseek"]);
    providers.get_mut(&id).unwrap().credential = None;
    assert!(derived_standalone_search_providers(&WebConfig::default(), &providers).is_empty());

    // Proxy and compatible-protocol endpoints must not borrow an official key.
    for base_url in [
        "https://proxy.example.com/v1",
        "https://api.deepseek.com.evil.example/v1",
        "http://api.deepseek.com/v1",
    ] {
        let mut providers = model_provider_registry(&["deepseek"]);
        providers.get_mut(&id).unwrap().base_url = base_url.to_string();
        assert!(
            derived_standalone_search_providers(&WebConfig::default(), &providers).is_empty(),
            "base_url {base_url} must not derive a standalone entry"
        );
    }

    // Other authenticated model providers never derive a standalone deepseek
    // entry.
    let providers = model_provider_registry(&["openai"]);
    assert!(derived_standalone_search_providers(&WebConfig::default(), &providers).is_empty());
}

#[test]
fn derived_providers_stay_out_of_serialized_config() {
    // The discovery marker is runtime-only: it must not change the serialized
    // config contract, and a round-trip drops it (a derived entry is
    // re-discovered, never persisted).
    let mut web_config = WebConfig::default();
    web_config
        .search
        .derived_providers
        .insert(DERIVED_DEEPSEEK_SEARCH_PROVIDER_ID.to_string());
    let json = serde_json::to_value(&web_config.search).unwrap();
    assert!(json.get("derived_providers").is_none());
    let restored: WebSearchConfig = serde_json::from_value(json).unwrap();
    assert!(restored.derived_providers.is_empty());
}

#[test]
fn derived_standalone_search_providers_discover_zai_and_bigmodel_once_each() {
    let providers = model_provider_registry(&["zai", "bigmodel"]);

    let derived = derived_standalone_search_providers(&WebConfig::default(), &providers);
    let ids = derived
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    // Both stations bind, so both derive exactly once.
    assert_eq!(
        ids,
        [
            DERIVED_ZAI_SEARCH_PROVIDER_ID,
            DERIVED_BIGMODEL_SEARCH_PROVIDER_ID
        ]
    );

    let zai = &derived[0].1;
    assert_eq!(zai.kind, WebProviderKind::Zai);
    assert_eq!(zai.api_key, "test-credential");
    // The discovered key stays pinned to the official endpoint.
    assert_eq!(zai.base_url, None);
    assert_eq!(zai.engine_id, None);

    let bigmodel = &derived[1].1;
    assert_eq!(bigmodel.kind, WebProviderKind::BigModel);
    assert_eq!(bigmodel.base_url, None);
    assert_eq!(bigmodel.engine_id, None);
}

#[test]
fn derived_standalone_search_providers_reject_proxy_and_cross_station_sources() {
    let default_config = WebConfig::default();

    // Third-party compatible/proxy endpoints and plain-http must not derive.
    for base_url in [
        "https://proxy.example.com/api/anthropic",
        "https://api.z.ai.evil.example/api/anthropic",
        "http://api.z.ai/api/anthropic",
    ] {
        let mut providers = model_provider_registry(&["zai"]);
        let id = crate::ProviderId::parse("zai").unwrap();
        providers.get_mut(&id).unwrap().base_url = base_url.to_string();
        assert!(
            derived_standalone_search_providers(&default_config, &providers).is_empty(),
            "base_url {base_url} must not derive a Z.AI entry"
        );
    }

    // A BigModel provider pointed at the Z.AI host must not borrow that station.
    let mut providers = model_provider_registry(&["bigmodel"]);
    let id = crate::ProviderId::parse("bigmodel").unwrap();
    providers.get_mut(&id).unwrap().base_url = "https://api.z.ai/api/anthropic".into();
    assert!(derived_standalone_search_providers(&default_config, &providers).is_empty());

    // No credential, and a disabled global search switch, derive nothing.
    let id = crate::ProviderId::parse("zai").unwrap();
    let mut providers = model_provider_registry(&["zai"]);
    providers.get_mut(&id).unwrap().credential = Some("   ".into());
    assert!(derived_standalone_search_providers(&default_config, &providers).is_empty());

    let mut disabled = WebConfig::default();
    disabled.search.enabled = false;
    assert!(derived_standalone_search_providers(
        &disabled,
        &model_provider_registry(&["zai", "bigmodel"])
    )
    .is_empty());
}

#[test]
fn zai_and_bigmodel_kind_and_endpoint_contracts() {
    for (kind, name, endpoint) in [
        (
            WebProviderKind::Zai,
            "zai",
            "https://api.z.ai/api/paas/v4/web_search",
        ),
        (
            WebProviderKind::BigModel,
            "bigmodel",
            "https://open.bigmodel.cn/api/paas/v4/web_search",
        ),
    ] {
        let mut provider = WebProviderConfig::from_file(name, &provider_file(kind)).unwrap();
        assert_eq!(serde_json::to_value(kind).unwrap(), name);
        assert_eq!(
            serde_json::from_str::<WebProviderKind>(&format!("\"{name}\"")).unwrap(),
            kind
        );
        let built = if kind == WebProviderKind::Zai {
            zai_web_search_url(&provider).unwrap()
        } else {
            bigmodel_web_search_url(&provider).unwrap()
        };
        assert_eq!(built.as_str(), endpoint);

        // A bare official host and an explicit `/api` root resolve to the same
        // documented endpoint.
        let official = if kind == WebProviderKind::Zai {
            "https://api.z.ai"
        } else {
            "https://open.bigmodel.cn"
        };
        provider.base_url = Some(official.into());
        let built = if kind == WebProviderKind::Zai {
            zai_web_search_url(&provider).unwrap()
        } else {
            bigmodel_web_search_url(&provider).unwrap()
        };
        assert_eq!(built.as_str(), endpoint);

        provider.base_url = Some("https://user:secret@example.com".into());
        let error = if kind == WebProviderKind::Zai {
            zai_web_search_url(&provider).unwrap_err().to_string()
        } else {
            bigmodel_web_search_url(&provider).unwrap_err().to_string()
        };
        assert!(
            error.contains("no credentials"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn station_engine_ids_are_validated_per_station() {
    // Defaults are accepted when engine_id is absent.
    assert!(WebProviderConfig::from_file("zai", &provider_file(WebProviderKind::Zai)).is_ok());
    assert!(
        WebProviderConfig::from_file("bigmodel", &provider_file(WebProviderKind::BigModel)).is_ok()
    );

    // Each station's own enum is accepted.
    let mut zai = provider_file(WebProviderKind::Zai);
    zai.engine_id = Some("search-prime".into());
    assert!(WebProviderConfig::from_file("zai", &zai).is_ok());
    let mut bigmodel = provider_file(WebProviderKind::BigModel);
    bigmodel.engine_id = Some("search_std".into());
    assert!(WebProviderConfig::from_file("bigmodel", &bigmodel).is_ok());

    // Engines are not shared across stations: BigModel's codes are rejected on
    // Z.AI and the Z.AI engine is rejected on BigModel.
    zai.engine_id = Some("search_pro".into());
    let error = WebProviderConfig::from_file("zai", &zai)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not supported"), "unexpected error: {error}");
    bigmodel.engine_id = Some("search-prime".into());
    let error = WebProviderConfig::from_file("bigmodel", &bigmodel)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not supported"), "unexpected error: {error}");

    // Other kinds still reject engine_id outright.
    let mut brave = provider_file(WebProviderKind::Brave);
    brave.engine_id = Some("search-prime".into());
    let error = WebProviderConfig::from_file("brave", &brave)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("must not configure engine_id"),
        "unexpected error: {error}"
    );
}
