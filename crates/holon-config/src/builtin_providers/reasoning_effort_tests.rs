use super::*;

#[test]
fn tokenhub_chat_provider_accepts_no_think_without_expanding_other_providers() {
    for (provider, transport, accepted) in [
        (
            "tencent-tokenhub",
            ProviderTransportKind::OpenAiChatCompletions,
            true,
        ),
        (
            "tencent-tokenhub",
            ProviderTransportKind::AnthropicMessages,
            false,
        ),
        (
            "tencent-tokenhub",
            ProviderTransportKind::OpenAiResponses,
            false,
        ),
        (
            "tencent-tokenhub-messages",
            ProviderTransportKind::AnthropicMessages,
            false,
        ),
        (
            "tencent-tokenhub-responses",
            ProviderTransportKind::OpenAiResponses,
            false,
        ),
        ("openai", ProviderTransportKind::OpenAiResponses, false),
        (
            "custom-tokenhub",
            ProviderTransportKind::OpenAiChatCompletions,
            false,
        ),
    ] {
        let id = ProviderId::parse(provider).unwrap();
        let mut config = HolonConfigFile::default();
        config.providers.insert(
            id.clone(),
            ProviderConfigFile {
                transport,
                base_url: "https://example.invalid/v1".into(),
                reasoning_effort: Some("no_think".into()),
                ..Default::default()
            },
        );
        let result =
            resolve_provider_registry(&config, &HashMap::new(), &CredentialStoreFile::default());
        assert_eq!(
            result.is_ok(),
            accepted,
            "{provider}/{transport:?}: {result:?}"
        );
        if accepted {
            assert_eq!(
                result.unwrap()[&id].reasoning_effort.as_deref(),
                Some("no_think")
            );
        }
    }
    assert!(validate_openai_reasoning_effort("no_think").is_err());
}

#[test]
fn codex_reasoning_effort_vocabulary_allows_max_but_not_ultra() {
    validate_openai_reasoning_effort("max").unwrap();
    validate_openai_reasoning_effort("none").unwrap();
    let error = validate_openai_reasoning_effort("ultra").unwrap_err();
    assert!(error.to_string().contains("orchestration semantics"));
}
