use super::*;
use crate::config::{
    built_in_provider_registry_with_settings, provider_config_view, resolve_provider_registry,
    AppConfig, CredentialKind, CredentialSource, CredentialStoreFile, ProviderEndpointConfigFile,
    ProviderPlanConfigFile, ProviderTransportKind,
};
use std::collections::BTreeMap;
use std::collections::HashMap;

fn headers(pairs: &[(&str, &str)]) -> ProviderHeaders {
    ProviderHeaders::new(
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string())),
    )
    .unwrap()
}

fn custom_config() -> (HolonConfigFile, ProviderId) {
    let id = ProviderId::parse("custom-headers").unwrap();
    let config = ProviderConfigFile {
        transport: ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://example.invalid/v1".into(),
        headers: headers(&[("X-Base", "base"), ("X-Shared", "provider")]),
        endpoints: BTreeMap::from([
            (
                ProviderEndpointId::parse("coding").unwrap(),
                ProviderEndpointConfigFile {
                    headers: headers(&[("x-shared", "endpoint")]),
                    ..Default::default()
                },
            ),
            (
                ProviderEndpointId::parse("other").unwrap(),
                ProviderEndpointConfigFile::default(),
            ),
        ]),
        plans: BTreeMap::from([(
            "coding".into(),
            ProviderPlanConfigFile {
                endpoint: Some(ProviderEndpointId::parse("coding").unwrap()),
            },
        )]),
        ..Default::default()
    };
    (
        HolonConfigFile {
            providers: BTreeMap::from([(id.clone(), config)]),
            ..Default::default()
        },
        id,
    )
}

#[test]
fn provider_headers_materialize_endpoint_inheritance_and_route_isolation() {
    let _env = crate::test_env::lock_env();
    let (stored, id) = custom_config();
    let registry =
        resolve_provider_registry(&stored, &HashMap::new(), &CredentialStoreFile::default())
            .unwrap();
    let base = &registry[&id].headers;
    let coding = &registry[&ProviderId::parse("custom-headers-coding").unwrap()].headers;
    assert_eq!(
        base,
        &headers(&[("x-base", "base"), ("x-shared", "provider")])
    );
    assert_eq!(
        coding,
        &headers(&[("x-base", "base"), ("x-shared", "endpoint")])
    );
    assert!(registry[&ProviderId::openai()].headers.is_empty());
    let other = effective_provider_headers(
        &stored.providers[&id],
        &ProviderEndpointId::parse("other").unwrap(),
    )
    .unwrap();
    assert_eq!(&other, base);
}

#[test]
fn provider_headers_canonical_builtin_config_applies_only_its_route_layers() {
    let _env = crate::test_env::lock_env();
    let mut stored = HolonConfigFile::default();
    let id = ProviderId::parse("opencode-go").unwrap();
    set_provider_headers(&mut stored, &id, None, &["X-Shared=base".into()]).unwrap();
    set_provider_headers(
        &mut stored,
        &id,
        Some(&ProviderEndpointId::default_endpoint()),
        &["X-Default=default-only".into()],
    )
    .unwrap();
    set_provider_headers(
        &mut stored,
        &id,
        Some(&ProviderEndpointId::parse("messages").unwrap()),
        &["X-Shared=messages".into()],
    )
    .unwrap();
    let registry =
        resolve_provider_registry(&stored, &HashMap::new(), &CredentialStoreFile::default())
            .unwrap();
    assert_eq!(
        registry[&id].headers,
        headers(&[("x-shared", "base"), ("x-default", "default-only")])
    );
    assert_eq!(
        registry[&ProviderId::parse("opencode-go-messages").unwrap()].headers,
        headers(&[("x-shared", "messages")])
    );
    assert!(!registry.contains_key(&ProviderId::parse("opencode-go-responses").unwrap()));
    assert!(registry[&ProviderId::openai()].headers.is_empty());
}

#[test]
fn provider_headers_builtin_materialization_preserves_default_auth() {
    let _env = crate::test_env::lock_env();
    let expected = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    for id in [
        ProviderId::openai_codex(),
        ProviderId::openai(),
        ProviderId::anthropic(),
    ] {
        let mut config = HolonConfigFile::default();
        set_provider_headers(&mut config, &id, None, &["x-example=value".into()]).unwrap();
        assert_eq!(config.providers[&id].auth, expected[&id].auth);
        assert_ne!(config.providers[&id].auth.source, CredentialSource::None);
        assert_ne!(config.providers[&id].auth.kind, CredentialKind::None);
    }
    let mut config = HolonConfigFile::default();
    assert!(set_provider_headers(
        &mut config,
        &ProviderId::parse("missing-custom").unwrap(),
        None,
        &["x-example=value".into()],
    )
    .is_err());
    assert!(config.providers.is_empty());
}

#[test]
fn provider_headers_debug_and_provider_views_never_expose_values() {
    let _env = crate::test_env::lock_env();
    let secret = "random-secret-like-header-value";
    let mut runtime = built_in_provider_registry_with_settings(&HashMap::new())
        .unwrap()
        .remove(&ProviderId::openai())
        .unwrap();
    runtime.headers = headers(&[("x-example", secret)]);
    let home = tempfile::tempdir().unwrap();
    let config =
        AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf())).unwrap();
    let view = provider_config_view(&config, &runtime);
    assert_eq!(view.header_names, vec!["x-example"]);
    assert!(!serde_json::to_string(&view).unwrap().contains(secret));
    assert!(!format!("{runtime:?}").contains(secret));
    assert!(!format!("{:?}", runtime.headers).contains(secret));
}
