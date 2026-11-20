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

    let config = WebSearchConfig::from(&file);

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
