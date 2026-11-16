use super::*;
use crate::{ProviderEndpointConfigFile, ProviderPlanConfigFile, ProviderTransportKind};
use std::collections::BTreeMap;

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
fn provider_headers_reserve_opencode_go_session_across_aliases() {
    let session = headers(&[("x-opencode-session", "custom-session")]);
    for id in ["opencode-go", "opencode-go-messages"] {
        assert!(validate_headers_for_provider(&session, &ProviderId::parse(id).unwrap()).is_err());
    }
    validate_headers_for_provider(
        &session,
        &ProviderId::parse("custom-go-compatible").unwrap(),
    )
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
