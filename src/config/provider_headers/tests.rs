use super::*;
use crate::config::{
    built_in_provider_registry_with_settings, provider_config_view, resolve_provider_registry,
    AppConfig, CredentialKind, CredentialSource, CredentialStoreFile, ProviderEndpointConfigFile,
    ProviderPlanConfigFile, ProviderTransportKind,
};
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
fn provider_headers_roundtrip_normalizes_names_and_omits_empty_maps() {
    let parsed: ProviderHeaders =
        serde_json::from_str(r#"{"X-Example":"value=with=equals","X-Empty":""}"#).unwrap();
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::json!({"x-example":"value=with=equals", "x-empty":""})
    );
    let old: ProviderConfigFile = serde_json::from_value(serde_json::json!({
        "transport":"openai_chat_completions",
        "base_url":"https://example.invalid/v1",
        "auth":{"source":"none","kind":"none"}
    }))
    .unwrap();
    assert!(old.headers.is_empty());
    assert!(serde_json::to_value(old).unwrap().get("headers").is_none());
    assert!(serde_json::to_value(ProviderEndpointConfigFile::default())
        .unwrap()
        .get("headers")
        .is_none());
}

#[test]
fn provider_headers_reject_duplicate_json_and_constructor_names() {
    for input in [
        r#"{"X-Example":"first","x-example":"second"}"#,
        r#"{"x-example":"first","x-example":"second"}"#,
    ] {
        assert!(serde_json::from_str::<ProviderHeaders>(input).is_err());
    }
    assert!(ProviderHeaders::new([
        ("X-Example".into(), "first".into()),
        ("x-example".into(), "second".into()),
    ])
    .is_err());
}

#[test]
fn provider_headers_reject_invalid_input_without_exposing_values() {
    let secret = "random-secret-like-header-payload";
    for (name, value) in [
        ("bad name", secret.to_string()),
        ("x-test", format!("{secret}\r\ninjected: true")),
        ("x-test", format!("{secret}\0")),
        ("x-test", format!("{secret}\u{7f}")),
        ("authorization", secret.to_string()),
    ] {
        let error = ProviderHeaders::new([(name.to_string(), value)]).unwrap_err();
        assert!(!format!("{error:#}").contains(secret));
    }
    let error = serde_json::from_str::<ProviderHeaders>(r#"{"x-test":123456789}"#).unwrap_err();
    assert!(!error.to_string().contains("123456789"));
}

#[test]
fn provider_headers_reserve_transport_owned_headers_but_allow_user_agent() {
    for name in RESERVED_HEADERS {
        assert!(ProviderHeaders::new([(name.to_uppercase(), "not-disclosed".into())]).is_err());
    }
    assert!(ProviderHeaders::new([("User-Agent".into(), "custom-client".into())]).is_ok());
    let session = headers(&[("x-opencode-session", "custom-session")]);
    for id in ["opencode-go", "opencode-go-messages"] {
        assert!(session
            .validate_for_provider(&ProviderId::parse(id).unwrap())
            .is_err());
    }
    session
        .validate_for_provider(&ProviderId::parse("custom-go-compatible").unwrap())
        .unwrap();
}

#[test]
fn provider_headers_reserve_codex_owned_headers() {
    for name in [
        "chatgpt-account-id",
        "originator",
        "session-id",
        "thread-id",
        "x-client-request-id",
        "x-codex-turn-state",
        "x-codex-turn-metadata",
        "x-codex-beta-features",
        "x-codex-routing-hint",
        "openai-beta",
    ] {
        assert!(
            ProviderHeaders::new([(name.to_string(), "not-disclosed".into())]).is_err(),
            "{name} must remain transport-owned"
        );
    }
}

#[test]
fn provider_headers_enforce_entry_value_and_total_byte_boundaries() {
    assert!(ProviderHeaders::new((0..32).map(|i| (format!("x-{i}"), String::new()))).is_ok());
    assert!(ProviderHeaders::new((0..33).map(|i| (format!("x-{i}"), String::new()))).is_err());
    assert!(ProviderHeaders::new([("x-test".into(), "v".repeat(4096))]).is_ok());
    assert!(ProviderHeaders::new([("x-test".into(), "v".repeat(4097))]).is_err());
    for (last, accepted) in [(4084, true), (4085, false)] {
        let result = ProviderHeaders::new((0..4).map(|i| {
            (
                format!("x-{i}"),
                "v".repeat(if i == 3 { last } else { 4096 }),
            )
        }));
        assert_eq!(result.is_ok(), accepted);
    }
    let base = ProviderHeaders::new((0..16).map(|i| (format!("x-{i}"), String::new()))).unwrap();
    let extra = ProviderHeaders::new((16..33).map(|i| (format!("x-{i}"), String::new()))).unwrap();
    assert!(base.merged(&extra).is_err());
    let base = ProviderHeaders::new((0..2).map(|i| (format!("x-{i}"), "v".repeat(4096)))).unwrap();
    let extra = ProviderHeaders::new((2..4).map(|i| (format!("x-{i}"), "v".repeat(4096)))).unwrap();
    assert!(base.merged(&extra).is_err());
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
fn provider_headers_header_only_updates_are_atomic_and_preserve_other_fields() {
    let (mut config, id) = custom_config();
    let before = serde_json::to_value(&config.providers[&id]).unwrap();
    set_provider_headers(
        &mut config,
        &id,
        None,
        &["X-Extra=a=b".into(), "X-Empty=".into()],
    )
    .unwrap();
    let mut after = serde_json::to_value(&config.providers[&id]).unwrap();
    after["headers"] = before["headers"].clone();
    assert_eq!(after, before);
    let before_failure = serde_json::to_value(&config).unwrap();
    for inputs in [
        vec!["X-Good=good".into(), "Authorization=never-show".into()],
        vec!["X-Good=first".into(), "x-good=second".into()],
        vec!["missing-equals-never-show".into()],
    ] {
        assert!(set_provider_headers(&mut config, &id, None, &inputs).is_err());
        assert_eq!(serde_json::to_value(&config).unwrap(), before_failure);
    }
    assert!(set_provider_headers(
        &mut config,
        &id,
        Some(&ProviderEndpointId::parse("missing").unwrap()),
        &["x-good=value".into()],
    )
    .is_err());
    assert_eq!(serde_json::to_value(&config).unwrap(), before_failure);
}

#[test]
fn provider_headers_updates_check_merged_budgets_before_committing() {
    let (mut config, id) = custom_config();
    let fill = (0..30).map(|i| format!("x-fill-{i}=")).collect::<Vec<_>>();
    set_provider_headers(&mut config, &id, None, &fill).unwrap();
    let before = serde_json::to_value(&config).unwrap();
    assert!(set_provider_headers(
        &mut config,
        &id,
        Some(&ProviderEndpointId::parse("coding").unwrap()),
        &["x-over-budget=value".into()],
    )
    .is_err());
    assert_eq!(serde_json::to_value(&config).unwrap(), before);
}

#[test]
fn provider_headers_unset_endpoint_override_restores_inheritance() {
    let (mut config, id) = custom_config();
    let coding = ProviderEndpointId::parse("coding").unwrap();
    unset_provider_headers(&mut config, &id, Some(&coding), &["X-Shared".into()]).unwrap();
    assert_eq!(
        effective_provider_headers(&config.providers[&id], &coding).unwrap(),
        config.providers[&id].headers
    );
    assert!(!config.providers[&id].headers.is_empty());
    let before = serde_json::to_value(&config).unwrap();
    assert!(
        unset_provider_headers(&mut config, &id, None, &["x-base".into(), "X-BASE".into()],)
            .is_err()
    );
    assert_eq!(serde_json::to_value(&config).unwrap(), before);
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
