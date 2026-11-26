use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn arcee_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("arcee").unwrap(),
        route_provider: ProviderId::parse("arcee").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://api.arcee.ai/v1".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn openrouter_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("openrouter").unwrap(),
        route_provider: ProviderId::parse("openrouter").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://openrouter.ai/api/v1".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
            env: None,
            profile: None,
            external: None,
            client_id: None,
        },
        credential: Some("test-key".into()),
        credential_store_path: None,
        codex_home: None,
        originator: None,
        reasoning_effort: None,
        context_management: Default::default(),
        builtin_web_search: None,
    }
}

fn huggingface_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("huggingface").unwrap(),
        route_provider: ProviderId::parse("huggingface").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://router.huggingface.co/v1".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn vercel_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("vercel-ai-gateway").unwrap(),
        route_provider: ProviderId::parse("vercel-ai-gateway").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::AnthropicMessages,
        base_url: "https://ai-gateway.vercel.sh".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::BearerToken,
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
    }
}

fn kilocode_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("kilocode").unwrap(),
        route_provider: ProviderId::parse("kilocode").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://api.kilo.ai/api/gateway".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn opencode_go_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("opencode-go").unwrap(),
        route_provider: ProviderId::parse("opencode-go").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://opencode.ai/inference/go/openai/v1".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn nearai_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("nearai").unwrap(),
        route_provider: ProviderId::parse("nearai").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://cloud-api.near.ai/v1".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn synthetic_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("synthetic").unwrap(),
        route_provider: ProviderId::parse("synthetic").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::AnthropicMessages,
        base_url: "https://api.synthetic.new/anthropic".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn venice_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("venice").unwrap(),
        route_provider: ProviderId::parse("venice").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://api.venice.ai/api/v1".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn tencent_tokenhub_provider() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("tencent-tokenhub").unwrap(),
        route_provider: ProviderId::parse("tencent-tokenhub").unwrap(),
        route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
        transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
        base_url: "https://tokenhub.tencentmaas.com/v1".into(),
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::Env,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn custom_openai_provider(base_url: String) -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        headers: Default::default(),
        id: ProviderId::parse("local-custom").unwrap(),
        route_provider: ProviderId::parse("local-custom").unwrap(),
        route_endpoint: ProviderEndpointId::default_endpoint(),
        transport: ProviderTransportKind::OpenAiChatCompletions,
        base_url,
        auth: crate::config::ProviderAuthConfig {
            source: crate::config::CredentialSource::None,
            kind: crate::config::CredentialKind::ApiKey,
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
    }
}

fn ollama_provider(base_url: String) -> ProviderRuntimeConfig {
    let mut provider = custom_openai_provider(base_url);
    provider.id = ProviderId::parse("ollama").unwrap();
    provider.route_provider = provider.id.clone();
    provider
}

fn magpie_provider(base_url: String) -> ProviderRuntimeConfig {
    let mut provider = custom_openai_provider(base_url);
    provider.id = ProviderId::parse("magpie").unwrap();
    provider.route_provider = provider.id.clone();
    provider.transport = ProviderTransportKind::AnthropicMessages;
    provider
}

#[test]
fn maps_arcee_models_without_inventing_capabilities_or_output_limits() {
    let payload: ArceeModelsResponse = serde_json::from_str(
        r#"{"object":"list","data":[
                {"id":"trinity-mini","object":"model","owned_by":"arcee-ai"},
                {"id":"trinity-large-preview","object":"model","owned_by":"arcee-ai"},
                {"id":"future-model","object":"model","owned_by":"arcee-ai"},
                {"id":" ","object":"model","owned_by":"arcee-ai"}
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("arcee").unwrap());

    assert_eq!(models.len(), 3);
    let mini = models
        .iter()
        .find(|model| model.model_ref.model == "trinity-mini")
        .unwrap();
    assert_eq!(mini.context_window_tokens, Some(131_072));
    assert!(mini.default_max_output_tokens.is_none());
    assert!(mini.max_output_tokens_upper_limit.is_none());
    assert_eq!(mini.capabilities, ModelCapabilityFlags::default());
    assert_eq!(mini.source, ModelMetadataSource::RemoteDiscovered);

    let unknown = models
        .iter()
        .find(|model| model.model_ref.model == "future-model")
        .unwrap();
    assert!(unknown.context_window_tokens.is_none());
    assert_eq!(unknown.capabilities, ModelCapabilityFlags::default());
}

#[test]
fn arcee_discovery_requires_a_credential_and_uses_its_models_api() {
    let provider = arcee_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Unauthenticated);
    assert!(!discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://api.arcee.ai/api/v1/models"
    );
}

#[test]
fn maps_only_live_huggingface_routes_conservatively() {
    let payload: HuggingFaceModelsResponse = serde_json::from_str(
            r#"{"data":[
                {
                    "id":"openai/gpt-oss-120b",
                    "architecture":{"input_modalities":["text"]},
                    "providers":[
                        {"provider":"groq","status":"live","context_length":131072,"supports_tools":true},
                        {"provider":"example","status":"error","context_length":262144}
                    ]
                },
                {
                    "id":"Qwen/Qwen-VL",
                    "architecture":{"input_modalities":["text","image"]},
                    "providers":[
                        {"provider":"example","status":"live","context_length":65536}
                    ]
                },
                {
                    "id":"offline/model",
                    "providers":[
                        {"provider":"example","status":"error","context_length":32768}
                    ]
                }
            ]}"#,
        )
        .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("huggingface").unwrap());

    assert_eq!(models.len(), 2);
    let gpt_oss = models
        .iter()
        .find(|model| model.model_ref.model == "openai/gpt-oss-120b")
        .unwrap();
    assert_eq!(gpt_oss.context_window_tokens, Some(131_072));
    assert!(gpt_oss.capabilities.supports_reasoning);
    assert!(!gpt_oss.capabilities.parallel_tool_calls);
    assert_eq!(gpt_oss.reasoning_effort_options, ["low", "medium", "high"]);
    assert!(gpt_oss.max_output_tokens_upper_limit.is_none());

    let vision = models
        .iter()
        .find(|model| model.model_ref.model == "Qwen/Qwen-VL")
        .unwrap();
    assert!(vision.capabilities.image_input);
    assert!(!vision.capabilities.supports_reasoning);
    assert!(!models
        .iter()
        .any(|model| model.model_ref.model == "offline/model"));
}

#[test]
fn huggingface_discovery_is_public_but_inference_still_requires_authentication() {
    let provider = huggingface_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://router.huggingface.co/v1/models"
    );
}

#[test]
fn maps_only_available_venice_text_models_and_reported_capabilities() {
    let payload: VeniceModelsResponse = serde_json::from_str(
        r#"{"data":[
                {
                    "id":"zai-org-glm-4.7",
                    "type":"text",
                    "context_length":128000,
                    "model_spec":{
                        "name":"GLM 4.7",
                        "description":"default",
                        "availableContextTokens":198000,
                        "maxCompletionTokens":16384,
                        "offline":false,
                        "capabilities":{
                            "supportsVision":false,
                            "supportsReasoning":true,
                            "supportsReasoningEffort":true,
                            "reasoningEffortOptions":["low","medium","high"]
                        }
                    }
                },
                {
                    "id":"qwen3-vl-235b-a22b",
                    "type":"text",
                    "context_length":128000,
                    "model_spec":{
                        "name":"Qwen3 VL 235B",
                        "maxCompletionTokens":16384,
                        "capabilities":{
                            "supportsVision":true,
                            "supportsReasoning":false,
                            "supportsReasoningEffort":false,
                            "reasoningEffortOptions":["low"]
                        }
                    }
                },
                {"id":"offline","type":"text","model_spec":{"offline":true}},
                {"id":"image-model","type":"image","model_spec":{}},
                {
                    "id":"removed",
                    "type":"text",
                    "model_spec":{"deprecation":{"removesAt":"2025-01-01T00:00:00Z"}}
                }
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(
        &ProviderId::parse("venice").unwrap(),
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    );

    assert_eq!(models.len(), 2);
    let glm = models
        .iter()
        .find(|model| model.model_ref.model == "zai-org-glm-4.7")
        .unwrap();
    assert_eq!(glm.context_window_tokens, Some(198_000));
    assert_eq!(glm.max_output_tokens_upper_limit, Some(16_384));
    assert!(!glm.capabilities.image_input);
    assert!(glm.capabilities.supports_reasoning);
    assert_eq!(glm.reasoning_effort_options, ["low", "medium", "high"]);

    let vision = models
        .iter()
        .find(|model| model.model_ref.model == "qwen3-vl-235b-a22b")
        .unwrap();
    assert!(vision.capabilities.image_input);
    assert!(!vision.capabilities.supports_reasoning);
    assert!(vision.reasoning_effort_options.is_empty());
}

#[test]
fn maps_openrouter_models_to_remote_metadata() {
    let payload: OpenRouterModelsResponse = serde_json::from_str(
        r#"{"data":[
                {
                    "id":"openrouter/auto",
                    "name":"Auto Router",
                    "context_length":2000000,
                    "architecture":{"input_modalities":["text","image","audio","file","video"]},
                    "top_provider":{"max_completion_tokens":null},
                    "supported_parameters":["reasoning","reasoning_effort"],
                    "reasoning":null
                },
                {
                    "id":"anthropic/claude-sonnet-5",
                    "name":"Claude Sonnet 5",
                    "description":"test",
                    "context_length":1000000,
                    "architecture":{"input_modalities":["text","image"]},
                    "top_provider":{"max_completion_tokens":128000},
                    "supported_parameters":["reasoning","reasoning_effort"],
                    "reasoning":{
                        "mandatory":false,
                        "default_enabled":true,
                        "supported_efforts":["max","xhigh","high","medium","low"]
                    }
                }
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("openrouter").unwrap());

    assert_eq!(models.len(), 2);
    let model = models
        .iter()
        .find(|model| model.model_ref.model == "anthropic/claude-sonnet-5")
        .unwrap();
    assert_eq!(
        model.model_ref.as_string(),
        "openrouter/anthropic/claude-sonnet-5"
    );
    assert_eq!(model.display_name, "Claude Sonnet 5");
    assert_eq!(model.context_window_tokens, Some(1_000_000));
    assert_eq!(model.max_output_tokens_upper_limit, Some(128_000));
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert_eq!(
        model.reasoning_effort_options,
        ["max", "xhigh", "high", "medium", "low"]
    );
    assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);

    let auto = models
        .iter()
        .find(|model| model.model_ref.model == "openrouter/auto")
        .unwrap();
    assert_eq!(auto.context_window_tokens, Some(2_000_000));
    assert!(auto.max_output_tokens_upper_limit.is_none());
    assert!(auto.capabilities.image_input);
    assert!(auto.capabilities.supports_reasoning);
    assert!(auto.reasoning_effort_options.is_empty());
}

#[test]
fn maps_vercel_language_models_conservatively() {
    let payload: VercelModelsResponse = serde_json::from_str(
        r#"{"data":[
                {
                    "id":"anthropic/claude-opus-4.6",
                    "name":"Claude Opus 4.6",
                    "description":"test",
                    "context_window":1000000,
                    "max_tokens":128000,
                    "type":"language",
                    "tags":["vision","reasoning","tool-use"]
                },
                {
                    "id":"openai/dall-e-3",
                    "name":"DALL-E 3",
                    "type":"image",
                    "tags":["image-generation"]
                },
                {
                    "id":"alibaba/qwen-3-14b",
                    "name":"Qwen3-14B",
                    "context_window":40960,
                    "max_tokens":16384,
                    "type":"language",
                    "tags":["reasoning","tool-use"]
                }
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("vercel-ai-gateway").unwrap());

    assert_eq!(models.len(), 2);
    let claude = models
        .iter()
        .find(|model| model.model_ref.model == "anthropic/claude-opus-4.6")
        .unwrap();
    assert_eq!(
        claude.model_ref.as_string(),
        "vercel-ai-gateway/anthropic/claude-opus-4.6"
    );
    assert_eq!(claude.context_window_tokens, Some(1_000_000));
    assert_eq!(claude.max_output_tokens_upper_limit, Some(128_000));
    assert!(claude.capabilities.image_input);
    assert!(claude.capabilities.supports_reasoning);
    assert!(!claude.capabilities.parallel_tool_calls);
    assert!(claude.reasoning_effort_options.is_empty());
    assert_eq!(claude.source, ModelMetadataSource::RemoteDiscovered);

    let qwen = models
        .iter()
        .find(|model| model.model_ref.model == "alibaba/qwen-3-14b")
        .unwrap();
    assert!(!qwen.capabilities.image_input);
    assert!(qwen.capabilities.supports_reasoning);
    assert!(!models
        .iter()
        .any(|model| model.model_ref.model == "openai/dall-e-3"));
}

#[test]
fn maps_kilocode_models_from_published_gateway_metadata() {
    let payload: KiloModelsResponse = serde_json::from_str(
        r#"{"data":[
                {
                    "id":"kilo-auto/balanced",
                    "name":"Auto Balanced",
                    "description":"test",
                    "context_length":1000000,
                    "architecture":{"input_modalities":["text","image"]},
                    "top_provider":{"max_completion_tokens":65536},
                    "supported_parameters":["reasoning","include_reasoning"]
                },
                {
                    "id":"example/reasoning-model",
                    "context_length":262144,
                    "architecture":{"input_modalities":["text"]},
                    "top_provider":{"max_completion_tokens":32768},
                    "supported_parameters":["reasoning","reasoning_effort"],
                    "opencode":{"variants":{
                        "none":{"reasoning":{"enabled":false}},
                        "low":{"reasoning":{"effort":"low"}},
                        "high":{"reasoning":{"effort":"high"}},
                        "thinking":{"reasoning":{"enabled":true}}
                    }}
                },
                {"id":" "}
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("kilocode").unwrap());

    assert_eq!(models.len(), 2);
    let balanced = models
        .iter()
        .find(|model| model.model_ref.model == "kilo-auto/balanced")
        .unwrap();
    assert_eq!(balanced.context_window_tokens, Some(1_000_000));
    assert_eq!(balanced.max_output_tokens_upper_limit, Some(65_536));
    assert!(balanced.capabilities.image_input);
    assert!(balanced.capabilities.supports_reasoning);
    assert!(balanced.reasoning_effort_options.is_empty());

    let reasoning = models
        .iter()
        .find(|model| model.model_ref.model == "example/reasoning-model")
        .unwrap();
    assert_eq!(reasoning.reasoning_effort_options, ["high", "low", "none"]);
    assert!(!reasoning.capabilities.image_input);
    assert_eq!(reasoning.source, ModelMetadataSource::RemoteDiscovered);
}

#[test]
fn maps_opencode_go_models_to_the_published_transport_endpoints() {
    let routes = [
        (
            None,
            vec![
                "deepseek-v4.1-flash",
                "deepseek-v4-pro",
                "deepseek-v4-flash",
                "glm-5.3-flash",
                "glm-5.3",
                "glm-5.1",
                "kimi-k3",
                "kimi-k2.7-code",
                "kimi-k2.6",
                "longcat-2.0",
                "longcat-2.5-preview-free",
                "mimo-v2.6-flash",
                "mimo-v2.6-pro",
                "mimo-v2.5",
                "mimo-v2.5-pro",
                "hy4-preview",
                "hy3",
                "space-bunny",
                "step-5-preview-free",
                "cat-jam-free",
            ],
        ),
        (
            Some("messages"),
            vec![
                "claude-haiku-5-5",
                "minimax-m3",
                "minimax-m2.7",
                "minimax-m2.5",
                "qwen3.8-max",
                "qwen3.8-flash",
                "qwen3.7-plus",
                "qwen3.7-max",
                "qwen3.6-plus",
            ],
        ),
        (
            Some("responses"),
            vec![
                "grok-4.7",
                "grok-4.6",
                "gpt-6-luna",
                "gpt-5.6-luna",
                "muse-spark-1.3-contributor",
                "muse-spark-1.2-contributor",
            ],
        ),
    ];
    let mut data = routes
        .iter()
        .flat_map(|(_, ids)| ids)
        .map(|id| serde_json::json!({"id": id}))
        .collect::<Vec<_>>();
    for id in [
        "gemini-4-argon-contributor",
        "omen-alpha",
        "glm-5",
        "kimi-k2.5",
        " ",
    ] {
        data.push(serde_json::json!({"id": id}));
    }
    let payload: OpenCodeGoModelsResponse =
        serde_json::from_value(serde_json::json!({"data": data})).unwrap();
    let models = payload.into_model_metadata(&ProviderId::parse("opencode-go").unwrap());
    assert_eq!(models.len(), 35);
    assert!(models
        .windows(2)
        .all(|pair| pair[0].model_ref.model < pair[1].model_ref.model));
    for (endpoint, ids) in routes {
        for id in ids {
            assert_eq!(
                models
                    .iter()
                    .find(|metadata| metadata.model_ref.model == id)
                    .unwrap()
                    .endpoint
                    .as_ref()
                    .map(ProviderEndpointId::as_str),
                endpoint,
                "{id}"
            );
        }
    }
    assert!(models.iter().all(|model| {
        model.source == ModelMetadataSource::RemoteDiscovered
            && model.context_window_tokens.is_none()
            && model.max_output_tokens_upper_limit.is_none()
            && model.capabilities == ModelCapabilityFlags::default()
    }));
}

#[test]
fn opencode_go_native_metadata_discovery_requires_exact_protocol_and_host() {
    let native = serde_json::json!({
        "id": "kimi-k3.1-pro",
        "modalities": {"input": ["text", "image/jpeg"], "output": ["text"]},
        "capabilities": {"reasoning": {"supported": true, "effort": ["low", "high", "max"]}},
        "limit": {"context": 1048576, "output": 131072},
        "api": {"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}}
    });
    let provider = ProviderId::parse("opencode-go").unwrap();
    let decode = |record| {
        serde_json::from_value::<OpenCodeGoModelsResponse>(serde_json::json!({"data": [record]}))
            .unwrap()
            .into_model_metadata(&provider)
            .pop()
            .unwrap()
    };
    let model = decode(native.clone());
    assert_eq!(model.context_window_tokens, Some(1_048_576));
    assert_eq!(model.max_output_tokens_upper_limit, Some(131_072));
    assert_eq!(model.default_max_output_tokens, None);
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert_eq!(model.reasoning_effort_options, ["low", "high", "max"]);
    assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);

    let mut messages = native.clone();
    messages["id"] = serde_json::json!("minimax-m3");
    messages["api"] = serde_json::json!({"messages": {"base_url": "https://opencode.ai/inference/go/anthropic/v1"}});
    assert_eq!(
        decode(messages).max_output_tokens_upper_limit,
        Some(131_072)
    );

    let mut responses = native.clone();
    responses["id"] = serde_json::json!("grok-4.7");
    responses["api"] = serde_json::json!({"responses": {"base_url": "https://opencode.ai/inference/go/openai/v1"}});
    assert_eq!(decode(responses).context_window_tokens, Some(1_048_576));

    let mut crouton = native.clone();
    crouton["id"] = serde_json::json!("crouton-alpha-free");
    crouton["modalities"]["input"] = serde_json::json!(["text", "image"]);
    crouton["limit"]["output"] = serde_json::json!(270_000);
    crouton["capabilities"]["reasoning"]["effort"] =
        serde_json::json!(["minimal", "low", "medium", "high", "xhigh", "max"]);
    crouton["api"] = serde_json::json!({"responses": {"base_url": "https://opencode.ai/inference/go/openai/v1"}});
    let model = decode(crouton);
    assert_eq!(
        model.endpoint,
        Some(ProviderEndpointId::parse("responses").unwrap())
    );
    assert_eq!(model.context_window_tokens, Some(1_048_576));
    assert_eq!(model.max_output_tokens_upper_limit, Some(270_000));
    assert_eq!(model.default_max_output_tokens, None);
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert_eq!(
        model.reasoning_effort_options,
        ["minimal", "low", "medium", "high", "xhigh", "max"]
    );

    for (id, api) in [
        (
            "crouton-alpha-free",
            serde_json::json!({"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}}),
        ),
        (
            "crouton-alpha-free",
            serde_json::json!({"messages": {"base_url": "https://opencode.ai/inference/go/anthropic/v1"}}),
        ),
        (
            "crouton-alpha-free",
            serde_json::json!({"responses": {"base_url": "https://example.invalid/inference/go/openai/v1"}}),
        ),
        (
            "crouton-alpha-free",
            serde_json::json!({"responses": {"base_url": "https://opencode.ai/inference/zen/openai/v1"}}),
        ),
        (
            "kimi-k3.1-pro",
            serde_json::json!({"messages": {"base_url": "https://opencode.ai/inference/go/anthropic/v1"}}),
        ),
        (
            "kimi-k3.1-pro",
            serde_json::json!({"chat-completions": {"base_url": "https://example.invalid/v1"}}),
        ),
        (
            "kimi-k3.1-pro",
            serde_json::json!({"chat-completions": {"base_url": "https://opencode.ai/inference/go/anthropic/v1"}}),
        ),
        (
            "grok-4.7",
            serde_json::json!({"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}}),
        ),
        (
            "gemini-4-argon",
            serde_json::json!({"generate-content": {"base_url": "https://opencode.ai/inference/go/google/v1beta"}}),
        ),
        (
            "gemini-4-argon",
            serde_json::json!({"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}}),
        ),
    ] {
        let mut record = native.clone();
        record["id"] = serde_json::json!(id);
        record["api"] = api;
        let model = decode(record);
        assert_eq!(model.context_window_tokens, None, "{id}");
        assert_eq!(model.max_output_tokens_upper_limit, None, "{id}");
        assert_eq!(model.capabilities, ModelCapabilityFlags::default(), "{id}");
        assert!(model.reasoning_effort_options.is_empty(), "{id}");
    }
    let mut absent = native.clone();
    absent.as_object_mut().unwrap().remove("api");
    assert_eq!(decode(absent).context_window_tokens, None);
    let missing = decode(serde_json::json!({
        "id": "kimi-k3.1-pro",
        "api": {"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}}
    }));
    assert_eq!(missing.context_window_tokens, None);
    assert_eq!(missing.max_output_tokens_upper_limit, None);
    assert_eq!(missing.capabilities, ModelCapabilityFlags::default());
    assert!(missing.reasoning_effort_options.is_empty());
    let mut no_reasoning = native;
    no_reasoning["capabilities"]["reasoning"]["supported"] = serde_json::json!(false);
    let model = decode(no_reasoning);
    assert!(!model.capabilities.supports_reasoning);
    assert!(model.reasoning_effort_options.is_empty());
}

#[test]
fn opencode_go_longcat_25_requires_the_native_chat_contract() {
    let provider = ProviderId::parse("opencode-go").unwrap();
    let decode = |api| {
        serde_json::from_value::<OpenCodeGoModel>(serde_json::json!({
            "id": "longcat-2.5",
            "api": api,
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "capabilities": {"reasoning": {"supported": true, "toggle": true}},
            "limit": {"context": 1_000_000, "output": 262_144}
        }))
        .unwrap()
        .into_model_metadata(&provider)
    };
    let go_chat = "https://opencode.ai/inference/go/openai/v1";
    let model = decode(serde_json::json!({"chat-completions": {"base_url": go_chat}})).unwrap();
    assert_eq!(model.endpoint, None);
    assert_eq!(model.context_window_tokens, Some(1_000_000));
    assert_eq!(model.default_max_output_tokens, None);
    assert_eq!(model.max_output_tokens_upper_limit, Some(262_144));
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert!(model.reasoning_effort_options.is_empty());
    for (api, base_url) in [
        ("messages", go_chat),
        ("responses", go_chat),
        (
            "chat-completions",
            "https://opencode.ai/inference/openai/v1",
        ),
    ] {
        assert!(decode(serde_json::json!({api: {"base_url": base_url}})).is_none());
    }
}

#[test]
fn opencode_go_longcat_native_contract_is_chat_and_host_scoped() {
    let provider = ProviderId::parse("opencode-go").unwrap();
    let native = serde_json::json!({
        "id": "longcat-2.5-free",
        "api": {"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}},
        "modalities": {"input": ["text", "image"], "output": ["text"]},
        "capabilities": {"reasoning": {"supported": true, "toggle": true}},
        "limit": {"context": 1_000_000, "output": 262_144}
    });
    let decode = |value| {
        serde_json::from_value::<OpenCodeGoModel>(value)
            .unwrap()
            .into_model_metadata(&provider)
            .unwrap()
    };
    let model = decode(native.clone());
    assert_eq!(model.endpoint, None);
    assert_eq!(model.context_window_tokens, Some(1_000_000));
    assert_eq!(model.default_max_output_tokens, None);
    assert_eq!(model.max_output_tokens_upper_limit, Some(262_144));
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert!(model.reasoning_effort_options.is_empty());
    for (api, host) in [
        ("messages", "https://opencode.ai/inference/go/openai/v1"),
        ("responses", "https://opencode.ai/inference/go/openai/v1"),
        (
            "chat-completions",
            "https://opencode.ai/inference/openai/v1",
        ),
        (
            "chat-completions",
            "https://opencode.ai/inference/go/anthropic/v1",
        ),
        (
            "chat-completions",
            "https://opencode.ai/inference/go/openai/v1/other",
        ),
    ] {
        let mut value = native.clone();
        value["api"] = serde_json::json!({api: {"base_url": host}});
        let model = decode(value);
        assert_eq!(model.context_window_tokens, None);
        assert_eq!(model.max_output_tokens_upper_limit, None);
        assert!(!model.capabilities.image_input);
        assert!(!model.capabilities.supports_reasoning);
        assert!(model.reasoning_effort_options.is_empty());
    }
    let mut unknown = native;
    unknown["id"] = serde_json::json!("longcat-2.5-free-future");
    assert!(serde_json::from_value::<OpenCodeGoModel>(unknown)
        .unwrap()
        .into_model_metadata(&provider)
        .is_none());
}

#[test]
fn opencode_go_discovery_accepts_reviewed_text_ids_without_inventing_capabilities() {
    assert_eq!(OPENCODE_GO_TEXT_MODELS.len(), 33);
    let mut data = OPENCODE_GO_TEXT_MODELS
        .iter()
        .map(|(id, _)| serde_json::json!({"id": id}))
        .collect::<Vec<_>>();
    for id in [
        "future-model",
        "gemini-4-argon-contributor",
        "gpt-6-luna-future",
        " ",
    ] {
        data.push(serde_json::json!({"id": id}));
    }
    let payload: OpenCodeGoModelsResponse =
        serde_json::from_value(serde_json::json!({"data": data})).unwrap();
    let models = payload.into_model_metadata(&ProviderId::parse("opencode-go").unwrap());
    // Discovery is model-keyed, not route-keyed: retain one preferred record.
    assert_eq!(models.len(), OPENCODE_GO_TEXT_MODELS.len());
    assert!(models
        .windows(2)
        .all(|pair| pair[0].model_ref.model < pair[1].model_ref.model));
    for (id, preferred) in OPENCODE_GO_TEXT_MODELS {
        let model = models
            .iter()
            .find(|model| model.model_ref.model == *id)
            .unwrap();
        assert_eq!(
            model
                .endpoint
                .as_ref()
                .map_or("default", ProviderEndpointId::as_str),
            *preferred,
            "{id}"
        );
        assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);
        assert_eq!(model.context_window_tokens, None);
        assert_eq!(model.auto_compact_token_limit, None);
        assert_eq!(model.default_max_output_tokens, None);
        assert_eq!(model.max_output_tokens_upper_limit, None);
        assert_eq!(model.default_verbosity, None);
        assert_eq!(model.capabilities, ModelCapabilityFlags::default());
        assert!(model.reasoning_effort_options.is_empty());
    }
    for id in [
        "hy4",
        "gemini-4-argon",
        "kimi-k3.1-pro",
        "iceman-contributor-free",
        "minimax-m3.1-flash",
        "step-5-preview",
        "beam-free",
    ] {
        assert!(
            models.iter().any(|model| model.model_ref.model == id),
            "{id}"
        );
    }
}

#[test]
fn maps_only_current_tencent_tokenhub_turn_models() {
    let payload: TencentTokenHubModelsResponse = serde_json::from_str(
        r#"{"data":[
                {"id":"hy3"},
                {"id":"glm-5v-turbo"},
                {"id":"deepseek-v4.1-flash"},
                {"id":"deepseek-r1-0528"},
                {"id":"HY-Image-V3.0"},
                {"id":"future-model"},
                {"id":" "}
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("tencent-tokenhub").unwrap());
    assert_eq!(
        models
            .iter()
            .map(|model| model.model_ref.model.as_str())
            .collect::<Vec<_>>(),
        ["deepseek-v4.1-flash", "hy3"]
    );
    assert!(models.iter().all(|model| {
        model.endpoint.is_none()
            && model.source == ModelMetadataSource::RemoteDiscovered
            && model.capabilities == ModelCapabilityFlags::default()
    }));
}

#[test]
fn maps_only_ready_nearai_models_from_published_metadata() {
    let payload: NearAiModelsResponse = serde_json::from_str(
        r#"{"data":[
                {
                    "id":"Qwen/Qwen3.5-122B-A10B",
                    "name":"Qwen3.5 122B A10B",
                    "description":"test",
                    "context_length":262144,
                    "max_output_length":16384,
                    "input_modalities":["text","image"],
                    "supported_features":["tools","structured_outputs","reasoning"],
                    "is_ready":true
                },
                {
                    "id":"anthropic/claude-opus-4-6",
                    "name":"Claude Opus 4.6",
                    "context_length":200000,
                    "max_output_length":32768,
                    "input_modalities":["text"],
                    "supported_features":["tools","reasoning"],
                    "is_ready":false
                },
                {
                    "id":"missing-ready",
                    "name":"Missing Ready"
                }
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("nearai").unwrap());

    assert_eq!(models.len(), 1);
    let qwen = &models[0];
    assert_eq!(qwen.model_ref.as_string(), "nearai/Qwen/Qwen3.5-122B-A10B");
    assert_eq!(qwen.context_window_tokens, Some(262_144));
    assert_eq!(qwen.max_output_tokens_upper_limit, Some(16_384));
    assert!(qwen.capabilities.image_input);
    assert!(qwen.capabilities.supports_reasoning);
    assert!(!qwen.capabilities.parallel_tool_calls);
    assert!(qwen.reasoning_effort_options.is_empty());
    assert_eq!(qwen.source, ModelMetadataSource::RemoteDiscovered);
}

#[test]
fn maps_only_synthetic_always_on_models_from_published_metadata() {
    let payload: SyntheticModelsResponse = serde_json::from_str(
        r#"{"data":[
                {
                    "id":"syn:large:vision",
                    "name":"syn:large:vision",
                    "always_on":true,
                    "context_length":262144,
                    "max_output_length":65536,
                    "input_modalities":["text","image"],
                    "supported_features":["tools","structured_outputs","reasoning"]
                },
                {
                    "id":"hf:rotating/model",
                    "name":"Rotating Model",
                    "always_on":false,
                    "context_length":131072,
                    "max_output_length":8192,
                    "input_modalities":["text"],
                    "supported_features":["tools"]
                }
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("synthetic").unwrap());

    assert_eq!(models.len(), 1);
    let model = &models[0];
    assert_eq!(model.model_ref.as_string(), "synthetic/syn:large:vision");
    assert_eq!(model.context_window_tokens, Some(262_144));
    assert_eq!(model.default_max_output_tokens, None);
    assert_eq!(model.max_output_tokens_upper_limit, Some(65_536));
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert!(model.reasoning_effort_options.is_empty());
    assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);
}

#[test]
fn nearai_discovery_is_public_but_inference_remains_unauthenticated() {
    let provider = nearai_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://cloud-api.near.ai/v1/models"
    );
}

#[test]
fn synthetic_discovery_is_public_and_uses_the_openai_models_endpoint() {
    let provider = synthetic_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://api.synthetic.new/openai/v1/models"
    );
}

#[test]
fn vercel_discovery_is_public_but_inference_remains_unauthenticated() {
    let provider = vercel_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        vercel_models_url(&provider.base_url).unwrap(),
        "https://ai-gateway.vercel.sh/v1/models"
    );
}

#[test]
fn kilocode_discovery_is_public_but_inference_still_requires_authentication() {
    let provider = kilocode_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://api.kilo.ai/api/gateway/models"
    );
}

#[test]
fn opencode_go_discovery_is_public_and_separate_from_inference() {
    let provider = opencode_go_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://opencode.ai/inference/go/v1/models"
    );
}

#[tokio::test]
async fn public_model_discovery_request_omits_custom_gateway_credentials() {
    let mut provider = opencode_go_provider();
    provider.base_url = "https://gateway.example.invalid/openai/v1".into();
    provider.credential = Some("test-only-gateway-credential".into());
    let discovery = provider_discovery_definition(&provider).unwrap();
    let request = provider_models_request(
        &provider,
        &EffectiveDiscovery::builtin(discovery),
        &provider_models_url(&provider, None).unwrap(),
    )
    .await
    .unwrap()
    .build()
    .unwrap();

    assert_eq!(
        request.url().as_str(),
        "https://opencode.ai/inference/go/v1/models"
    );
    assert!(!request
        .headers()
        .contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(
        provider.credential.as_deref(),
        Some("test-only-gateway-credential")
    );
}

#[tokio::test]
async fn authenticated_model_discovery_request_preserves_required_and_optional_credentials() {
    for mut provider in [
        arcee_provider(),
        openrouter_provider(),
        custom_openai_provider("https://gateway.example.invalid/v1".into()),
    ] {
        provider.credential = Some("test-only-discovery-credential".into());
        let discovery = provider_discovery_definition(&provider).unwrap();
        let request = provider_models_request(
            &provider,
            &EffectiveDiscovery::builtin(discovery),
            &provider_models_url(&provider, None).unwrap(),
        )
        .await
        .unwrap()
        .build()
        .unwrap();

        assert_eq!(
            request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer test-only-discovery-credential"
        );
    }
}

#[tokio::test]
async fn oauth_discovery_refreshes_expired_credentials() {
    check_oauth_discovery_lifecycle(OAuthDiscoveryCase::Expired).await;
}

#[tokio::test]
async fn oauth_discovery_reloads_rotated_credentials() {
    check_oauth_discovery_lifecycle(OAuthDiscoveryCase::Rotated).await;
}

#[tokio::test]
async fn oauth_discovery_recovers_once_from_401() {
    check_oauth_discovery_lifecycle(OAuthDiscoveryCase::Unauthorized).await;
}

#[tokio::test]
async fn oauth_discovery_does_not_repeat_401_recovery() {
    check_oauth_discovery_lifecycle(OAuthDiscoveryCase::UnauthorizedAlways).await;
}

#[tokio::test]
async fn oauth_discovery_preserves_403_without_refresh() {
    check_oauth_discovery_lifecycle(OAuthDiscoveryCase::Forbidden).await;
}

#[tokio::test]
async fn oauth_discovery_redacts_expired_refresh_failure() {
    check_oauth_discovery_lifecycle(OAuthDiscoveryCase::ExpiredRefreshRejected).await;
}

#[tokio::test]
async fn oauth_discovery_redacts_401_refresh_failure() {
    check_oauth_discovery_lifecycle(OAuthDiscoveryCase::UnauthorizedRefreshRejected).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OAuthDiscoveryCase {
    Expired,
    Rotated,
    Unauthorized,
    UnauthorizedAlways,
    Forbidden,
    ExpiredRefreshRejected,
    UnauthorizedRefreshRejected,
}

async fn check_oauth_discovery_lifecycle(case: OAuthDiscoveryCase) {
    use crate::config::{save_credential_store_at, CredentialProfileFile, CredentialStoreFile};
    use axum::{
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    let bearers = Arc::new(Mutex::new(Vec::new()));
    let forms = Arc::new(Mutex::new(Vec::new()));
    let model_bearers = bearers.clone();
    let token_forms = forms.clone();
    let server = Router::new()
        .route(
            "/v1/models",
            get(move |headers: axum::http::HeaderMap| {
                let bearers = model_bearers.clone();
                async move {
                    let bearer = headers["authorization"].to_str().unwrap().to_string();
                    bearers.lock().unwrap().push(bearer.clone());
                    let status = match case {
                        OAuthDiscoveryCase::Forbidden => StatusCode::FORBIDDEN,
                        OAuthDiscoveryCase::UnauthorizedAlways => StatusCode::UNAUTHORIZED,
                        OAuthDiscoveryCase::Unauthorized
                        | OAuthDiscoveryCase::UnauthorizedRefreshRejected
                            if bearer == "Bearer old-access" =>
                        {
                            StatusCode::UNAUTHORIZED
                        }
                        _ => StatusCode::OK,
                    };
                    (
                        status,
                        Json(serde_json::json!({"data": [{"id": "discovered"}]})),
                    )
                }
            }),
        )
        .route(
            "/token",
            post(move |body: String| {
                let forms = token_forms.clone();
                async move {
                    forms.lock().unwrap().push(body);
                    if matches!(
                        case,
                        OAuthDiscoveryCase::ExpiredRefreshRejected
                            | OAuthDiscoveryCase::UnauthorizedRefreshRejected
                    ) {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error_description": "refresh-secret"})),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "access_token": "rotated-access",
                                "refresh_token": "rotated-refresh",
                                "expires_in": 28800
                            })),
                        )
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    let _lock = crate::auth::GITHUB_TOKEN_URL_TEST_ENV_LOCK.lock().unwrap();
    struct TokenUrlGuard(Option<std::ffi::OsString>);
    impl Drop for TokenUrlGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(previous) => std::env::set_var("GITHUB_TOKEN_URL_OVERRIDE", previous),
                None => std::env::remove_var("GITHUB_TOKEN_URL_OVERRIDE"),
            }
        }
    }
    let _guard = TokenUrlGuard(std::env::var_os("GITHUB_TOKEN_URL_OVERRIDE"));
    std::env::set_var("GITHUB_TOKEN_URL_OVERRIDE", format!("http://{addr}/token"));
    let home = tempfile::tempdir().unwrap();
    let store_path = home.path().join("credentials.json");
    let cache_path = home.path().join("models.json");
    let material = serde_json::json!({
        "tokens": {
            "access_token": "old-access",
            "refresh_token": if case == OAuthDiscoveryCase::Forbidden { "" } else { "old-refresh" }
        },
        "oauth_client_id": "Iv1.discovery-client",
        "expires_at": if matches!(
            case, OAuthDiscoveryCase::Expired | OAuthDiscoveryCase::ExpiredRefreshRejected
        ) {
            Utc::now() - chrono::Duration::minutes(1)
        } else {
            Utc::now() + chrono::Duration::hours(8)
        }
    });
    let mut stored = material.clone();
    if case == OAuthDiscoveryCase::Rotated {
        stored["tokens"]["access_token"] = "rotated-access".into();
    }
    let mut store = CredentialStoreFile::default();
    store.profiles.insert(
        "copilot-work".into(),
        CredentialProfileFile {
            kind: CredentialKind::OAuth,
            material: stored.to_string(),
        },
    );
    save_credential_store_at(&store_path, &store).unwrap();
    let mut provider = custom_openai_provider(format!("http://{addr}"));
    provider.id = ProviderId::parse("copilot-plan").unwrap();
    provider.route_provider = ProviderId::parse("github-copilot").unwrap();
    provider.auth.source = CredentialSource::AuthProfile;
    provider.auth.kind = CredentialKind::OAuth;
    provider.auth.profile = Some("copilot-work".into());
    provider.credential = Some(material.to_string());
    provider.credential_store_path = Some(store_path.clone());
    let result = refresh_provider_models(&provider, None, &cache_path).await;
    let refresh_rejected = matches!(
        case,
        OAuthDiscoveryCase::ExpiredRefreshRejected
            | OAuthDiscoveryCase::UnauthorizedRefreshRejected
    );
    if refresh_rejected {
        let error = result.unwrap_err();
        for rendered in [format!("{error:#}"), format!("{error:?}")] {
            for secret in ["refresh-secret", "old-access", "old-refresh"] {
                assert!(!rendered.contains(secret));
            }
            assert!(rendered.contains("400"));
        }
        assert!(!cache_path.exists());
        let store = crate::config::load_credential_store_at(&store_path).unwrap();
        assert_eq!(store.profiles["copilot-work"].material, stored.to_string());
    } else if matches!(
        case,
        OAuthDiscoveryCase::Forbidden | OAuthDiscoveryCase::UnauthorizedAlways
    ) {
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(if case == OAuthDiscoveryCase::Forbidden {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            })
        );
        assert!(!cache_path.exists());
    } else {
        assert_eq!(result.unwrap().model_count, 1);
    }
    let expected = match case {
        OAuthDiscoveryCase::ExpiredRefreshRejected => vec![],
        OAuthDiscoveryCase::UnauthorizedRefreshRejected => vec!["Bearer old-access"],
        OAuthDiscoveryCase::Forbidden => vec!["Bearer old-access"],
        OAuthDiscoveryCase::Unauthorized | OAuthDiscoveryCase::UnauthorizedAlways => {
            vec!["Bearer old-access", "Bearer rotated-access"]
        }
        _ => vec!["Bearer rotated-access"],
    };
    assert_eq!(*bearers.lock().unwrap(), expected);
    let should_refresh = matches!(
        case,
        OAuthDiscoveryCase::Expired
            | OAuthDiscoveryCase::Unauthorized
            | OAuthDiscoveryCase::UnauthorizedAlways
            | OAuthDiscoveryCase::ExpiredRefreshRejected
            | OAuthDiscoveryCase::UnauthorizedRefreshRejected
    );
    let forms = forms.lock().unwrap();
    assert_eq!(forms.len(), usize::from(should_refresh));
    if should_refresh {
        assert!(forms[0].contains("client_id=Iv1.discovery-client"));
        assert!(forms[0].contains("grant_type=refresh_token"));
        if !refresh_rejected {
            let store = crate::config::load_credential_store_at(&store_path).unwrap();
            assert!(store.profiles["copilot-work"]
                .material
                .contains("rotated-refresh"));
        }
    }
    handle.abort();
}

#[tokio::test]
async fn oauth_model_discovery_request_uses_the_profile_access_token() {
    let mut provider = custom_openai_provider("https://api.githubcopilot.com".into());
    provider.auth.source = CredentialSource::AuthProfile;
    provider.auth.kind = CredentialKind::OAuth;
    provider.auth.profile = Some("github-copilot".into());
    provider.credential = Some(
        serde_json::json!({
            "tokens": {
                "access_token": "gho_test_access",
                "refresh_token": "ghr_test_refresh"
            },
            "oauth_client_id": "Iv1.test-client",
            "last_refresh": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    );
    let discovery = provider_discovery_definition(&provider).unwrap();
    let request = provider_models_request(
        &provider,
        &EffectiveDiscovery::builtin(discovery),
        &provider_models_url(&provider, None).unwrap(),
    )
    .await
    .unwrap()
    .build()
    .unwrap();

    assert_eq!(
        request.headers()[reqwest::header::AUTHORIZATION],
        "Bearer gho_test_access"
    );
}

#[tokio::test]
async fn oauth_model_discovery_rejects_malformed_material_before_sending_credentials() {
    let mut provider = custom_openai_provider("https://api.githubcopilot.com".into());
    provider.auth.source = CredentialSource::AuthProfile;
    provider.auth.kind = CredentialKind::OAuth;
    provider.auth.profile = Some("github-copilot".into());
    for material in [
        r#"{"tokens":"refresh-secret"}"#,
        r#"{"tokens":"{\"access_token\":\"access-secret\",\"refresh_token\":\"refresh-secret\",\"id_token\":\"id-secret\"}"}"#,
        r#"{"tokens":{"access_token":"access-secret","refresh_token":"refresh-secret","id_token":"id-secret"},"expires_at":"not-a-date"}"#,
        r#"{"tokens":{"refresh_token":"refresh-secret","id_token":"id-secret"}}"#,
        r#"{"tokens":{"access_token":"access-secret","refresh_token":"refresh-secret"}"#,
        "[]",
    ] {
        provider.credential = Some(material.into());
        let error = provider_models_request(
            &provider,
            &EffectiveDiscovery::builtin(provider_discovery_definition(&provider).unwrap()),
            &provider_models_url(&provider, None).unwrap(),
        )
        .await
        .err()
        .expect("malformed OAuth material must fail before request construction");
        let rendered = format!("{error:#}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
        assert!(!rendered.contains("id-secret"));
    }

    provider.credential = Some("legacy-opaque-token".into());
    let request = provider_models_request(
        &provider,
        &EffectiveDiscovery::builtin(provider_discovery_definition(&provider).unwrap()),
        &provider_models_url(&provider, None).unwrap(),
    )
    .await
    .unwrap()
    .build()
    .unwrap();
    assert_eq!(
        request.headers()[reqwest::header::AUTHORIZATION],
        "Bearer legacy-opaque-token"
    );
}

#[tokio::test]
async fn model_discovery_request_sends_the_configured_provider_headers() {
    let mut provider = custom_openai_provider("https://api.githubcopilot.com".into());
    provider.headers = crate::config::ProviderHeaders::new([(
        "Copilot-Integration-Id".to_string(),
        "holon".to_string(),
    )])
    .unwrap();
    let discovery = provider_discovery_definition(&provider).unwrap();
    let request = provider_models_request(
        &provider,
        &EffectiveDiscovery::builtin(discovery),
        &provider_models_url(&provider, None).unwrap(),
    )
    .await
    .unwrap()
    .build()
    .unwrap();

    assert_eq!(request.headers()["copilot-integration-id"], "holon");
}

#[tokio::test]
async fn optional_model_discovery_request_allows_missing_credentials() {
    let provider = custom_openai_provider("https://gateway.example.invalid/v1".into());
    let discovery = provider_discovery_definition(&provider).unwrap();
    let request = provider_models_request(
        &provider,
        &EffectiveDiscovery::builtin(discovery),
        &provider_models_url(&provider, None).unwrap(),
    )
    .await
    .unwrap()
    .build()
    .unwrap();

    assert!(!request
        .headers()
        .contains_key(reqwest::header::AUTHORIZATION));
}

#[test]
fn tencent_tokenhub_discovery_requires_a_credential_and_uses_its_models_endpoint() {
    let provider = tencent_tokenhub_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Unauthenticated);
    assert!(!discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://tokenhub.tencentmaas.com/v1/models"
    );
}

#[test]
fn venice_discovery_is_public_and_filters_to_text_models() {
    let provider = venice_provider();
    let cache = ModelDiscoveryCacheFile::default();
    let status = discovery_cache_status_for_provider(
        &provider,
        None,
        &cache,
        Duration::from_secs(60),
        false,
    );

    assert!(status.supports_auto_refresh);
    assert!(!status.credential_configured);
    assert_eq!(status.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(
        &provider,
        None,
        &cache,
        Duration::from_secs(60)
    ));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "https://api.venice.ai/api/v1/models?type=text"
    );
}

#[tokio::test]
async fn custom_openai_compatible_provider_discovers_models_without_authentication() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("GET /v1/models HTTP/1.1"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));

        let body = r#"{"object":"list","data":[
                {"id":"custom-chat","object":"model"},
                {"id":"org/custom-reasoning","object":"model"},
                {"id":" ","object":"model"}
            ]}"#;
        write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
    });

    let provider = custom_openai_provider(base_url.clone());
    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("model-discovery-cache.json");
    let report = refresh_provider_models(&provider, None, &cache_path)
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(report.provider, ProviderId::parse("local-custom").unwrap());
    assert_eq!(report.model_count, 2);
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        format!("{base_url}/v1/models")
    );

    let cache = load_discovery_cache_at(&cache_path).unwrap();
    let models = cache.models();
    assert!(
        models.contains_key(&ModelRouteRef::parse_compatible("local-custom/custom-chat").unwrap())
    );
    assert!(models.contains_key(
        &ModelRouteRef::parse_compatible("local-custom/org/custom-reasoning").unwrap()
    ));
    assert_eq!(
        discovery_cache_status_for_provider(
            &provider,
            None,
            &cache,
            Duration::from_secs(60),
            false
        )
        .state,
        ModelDiscoveryCacheState::Fresh
    );
}

#[test]
fn openai_compatible_discovery_keeps_only_non_empty_server_model_ids() {
    let payload: OpenAiCompatibleModelsResponse = serde_json::from_str(
        r#"{"object":"list","data":[
                {"id":"deployment-alias","object":"model"},
                {"id":"org/model","object":"model"},
                {"id":" ","object":"model"}
            ]}"#,
    )
    .unwrap();

    let models = payload.into_model_metadata(&ProviderId::parse("vllm").unwrap());
    assert_eq!(
        models
            .iter()
            .map(|model| model.model_ref.model.as_str())
            .collect::<Vec<_>>(),
        vec!["deployment-alias", "org/model"]
    );
    assert!(models
        .iter()
        .all(|model| model.source == ModelMetadataSource::RemoteDiscovered));
    assert!(models
        .iter()
        .all(|model| model.capabilities == ModelCapabilityFlags::default()));
}

#[test]
fn magpie_models_decoder_maps_metadata_and_degrades_invalid_fields() {
    let payload: MagpieModelsResponse =
        serde_json::from_str(include_str!("fixtures/magpie_models.json")).unwrap();
    let models = payload.into_model_metadata(&ProviderId::parse("magpie").unwrap());

    assert_eq!(
        models
            .iter()
            .map(|model| model.model_ref.model.as_str())
            .collect::<Vec<_>>(),
        vec!["invalid-fields", "legacy-chat", "openai/gpt-5"]
    );

    let complete = &models[2];
    assert_eq!(complete.model_ref.as_string(), "magpie/openai/gpt-5");
    assert_eq!(
        ModelRef::parse(&complete.model_ref.as_string()).unwrap(),
        complete.model_ref
    );
    assert_eq!(complete.display_name, "GPT-5");
    assert_eq!(complete.description, "Magpie catalog (provider: openai)");
    assert_eq!(complete.context_window_tokens, Some(128_000));
    assert_eq!(complete.default_max_output_tokens, None);
    assert_eq!(complete.max_output_tokens_upper_limit, Some(8_192));
    assert!(complete.capabilities.image_input);
    assert!(complete.capabilities.image_generation);
    assert!(complete.capabilities.supports_reasoning);
    assert_eq!(complete.reasoning_effort_options, ["low", "high"]);
    assert_eq!(complete.source, ModelMetadataSource::RemoteDiscovered);

    let fallback = &models[1];
    assert_eq!(fallback.display_name, "legacy-chat");
    assert_eq!(fallback.description, "Magpie catalog.");
    assert_eq!(fallback.context_window_tokens, Some(4_096));
    assert!(!fallback.capabilities.image_input);
    assert!(!fallback.capabilities.image_generation);
    assert!(fallback.capabilities.supports_reasoning);
    assert_eq!(fallback.reasoning_effort_options, ["minimal", "medium"]);

    let invalid = &models[0];
    assert_eq!(invalid.display_name, "invalid-fields");
    assert_eq!(invalid.description, "Magpie catalog.");
    assert_eq!(invalid.context_window_tokens, None);
    assert_eq!(invalid.max_output_tokens_upper_limit, None);
    assert!(!invalid.capabilities.image_input);
    assert!(!invalid.capabilities.image_generation);
    assert!(!invalid.capabilities.supports_reasoning);
    assert!(invalid.reasoning_effort_options.is_empty());
}

#[test]
fn custom_openai_compatible_provider_derives_v1_models_url() {
    // `OpenAiV1` de-duplicates `/v1`, so both root and `/v1` base URLs land on
    // `/v1/models` while an explicit path prefix is preserved.
    let cases = [
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080/v1/models"),
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080/v1/models"),
        (
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1:8080/v1/models",
        ),
        (
            "http://127.0.0.1:8080/v1/",
            "http://127.0.0.1:8080/v1/models",
        ),
        (
            "https://gateway.example.invalid/gw",
            "https://gateway.example.invalid/gw/v1/models",
        ),
    ];
    for (base_url, expected) in cases {
        let provider = custom_openai_provider(base_url.to_string());
        assert_eq!(
            provider_models_url(&provider, None).unwrap(),
            expected,
            "base_url {base_url:?}"
        );
    }
}

#[test]
fn discovery_cache_status_invalidates_when_source_url_changes() {
    let provider = custom_openai_provider("http://127.0.0.1:8080".into());
    let ttl = Duration::from_secs(60);
    let mut cache = ModelDiscoveryCacheFile::default();
    cache.providers.insert(
        ModelDiscoveryCacheKey::new(
            provider.id.clone(),
            crate::config::ProviderEndpointId::default_endpoint(),
        ),
        ProviderModelDiscoveryCache {
            provider: provider.id.clone(),
            fetched_at: Utc::now(),
            source_url: Some(provider_models_url(&provider, None).unwrap()),
            response_hash: None,
            models: Vec::new(),
        },
    );
    assert_eq!(
        discovery_cache_status_for_provider(&provider, None, &cache, ttl, false).state,
        ModelDiscoveryCacheState::Fresh
    );

    // Same provider id and TTL window, but the gateway moved: the cached list no
    // longer describes the current endpoint, so a refresh is required.
    let moved = custom_openai_provider("http://127.0.0.1:9090".into());
    assert_eq!(
        discovery_cache_status_for_provider(&moved, None, &cache, ttl, false).state,
        ModelDiscoveryCacheState::Stale
    );
    assert!(discovery_cache_needs_refresh(&moved, None, &cache, ttl));
}

#[test]
fn github_copilot_discovery_defaults_to_responses_and_keeps_chat_completions_fallback() {
    let payload: OpenAiCompatibleModelsResponse = serde_json::from_str(
        r#"{"object":"list","data":[
                {"id":"gpt-6.2-sol","supported_endpoints":["/responses","ws:/responses"]},
                {"id":"mai-code-2-flash","supported_endpoints":["/chat/completions","/responses"]},
                {"id":"claude-fable-5.1","supported_endpoints":["/chat/completions","/v1/messages"]},
                {"id":"copilot-search-a","supported_endpoints":["/chat/completions"]},
                {"id":"gpt-6-luna-decisions","supported_endpoints":["/v1/decisions"]},
                {"id":"websocket-only","supported_endpoints":["ws:/responses"]},
                {"id":"gpt-4o-mini-2024-07-18"}
            ]}"#,
    )
    .unwrap();
    let models = payload.into_model_metadata(&ProviderId::parse("github-copilot").unwrap());
    let endpoint = |id: &str| {
        models
            .iter()
            .find(|model| model.model_ref.model == id)
            .unwrap()
            .endpoint
            .as_ref()
            .unwrap()
            .as_str()
    };
    assert_eq!(endpoint("gpt-6.2-sol"), "default");
    assert_eq!(endpoint("mai-code-2-flash"), "default");
    for id in [
        "claude-fable-5.1",
        "copilot-search-a",
        "gpt-6-luna-decisions",
        "websocket-only",
        "gpt-4o-mini-2024-07-18",
    ] {
        assert_eq!(endpoint(id), "chat-completions", "{id}");
    }
}

#[test]
fn github_copilot_discovery_cache_load_preserves_previous_transports() {
    let provider = ProviderId::parse("github-copilot").unwrap();
    let other_provider = ProviderId::parse("vllm").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = discovery_cache_path(dir.path());
    let mut cache = ModelDiscoveryCacheFile::default();
    for id in [&provider, &other_provider] {
        let models = [
            ("old-chat", None),
            ("old-responses", Some("responses")),
            ("new-responses", Some("default")),
            ("new-chat", Some("chat-completions")),
        ]
        .into_iter()
        .map(|(name, endpoint)| {
            let mut model = OpenAiCompatibleModel {
                id: name.to_string(),
                supported_endpoints: Vec::new(),
            }
            .into_model_metadata(id)
            .unwrap();
            model.endpoint = endpoint.map(|value| ProviderEndpointId::parse(value).unwrap());
            model
        })
        .collect();
        cache.providers.insert(
            ModelDiscoveryCacheKey::new(
                id.clone(),
                crate::config::ProviderEndpointId::default_endpoint(),
            ),
            ProviderModelDiscoveryCache {
                provider: id.clone(),
                fetched_at: Utc::now(),
                source_url: None,
                response_hash: None,
                models,
            },
        );
    }
    save_discovery_cache_at(&path, &cache).unwrap();
    let loaded = load_discovery_cache_at(&path).unwrap();
    let endpoints = |id: &ProviderId| {
        loaded.providers[&ModelDiscoveryCacheKey::new(
            id.clone(),
            crate::config::ProviderEndpointId::default_endpoint(),
        )]
            .models
            .iter()
            .map(|model| model.endpoint.as_ref().map(ProviderEndpointId::as_str))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        endpoints(&provider),
        [
            Some("chat-completions"),
            Some("default"),
            Some("default"),
            Some("chat-completions"),
        ]
    );
    assert_eq!(
        endpoints(&other_provider),
        [
            None,
            Some("responses"),
            Some("default"),
            Some("chat-completions")
        ]
    );
}

#[test]
fn openai_compatible_discovery_ignores_supported_endpoints_for_other_providers() {
    let payload: OpenAiCompatibleModelsResponse = serde_json::from_str(
        r#"{"object":"list","data":[
                {"id":"some-model","supported_endpoints":["/responses"]}
            ]}"#,
    )
    .unwrap();
    let models = payload.into_model_metadata(&ProviderId::parse("vllm").unwrap());
    assert_eq!(models.len(), 1);
    assert!(models[0].endpoint.is_none());
}

#[test]
fn only_ollama_discovery_currently_supplies_model_capabilities() {
    assert!(provider_discovers_model_capabilities(
        &ollama_provider("http://127.0.0.1:11434".into()),
        None
    ));
    assert!(!provider_discovers_model_capabilities(
        &custom_openai_provider("http://127.0.0.1:8000".into()),
        None
    ));
}

#[test]
fn discovery_cache_status_reports_missing_stale_and_fresh() {
    let provider = openrouter_provider();
    let ttl = Duration::from_secs(60);
    let mut cache = ModelDiscoveryCacheFile::default();

    let missing = discovery_cache_status_for_provider(&provider, None, &cache, ttl, false);
    assert_eq!(missing.state, ModelDiscoveryCacheState::Missing);
    assert!(discovery_cache_needs_refresh(&provider, None, &cache, ttl));

    cache.providers.insert(
        ModelDiscoveryCacheKey::new(
            provider.id.clone(),
            crate::config::ProviderEndpointId::default_endpoint(),
        ),
        ProviderModelDiscoveryCache {
            provider: provider.id.clone(),
            fetched_at: Utc::now() - chrono::TimeDelta::seconds(120),
            source_url: Some(provider_models_url(&provider, None).unwrap()),
            response_hash: None,
            models: Vec::new(),
        },
    );
    let stale = discovery_cache_status_for_provider(&provider, None, &cache, ttl, true);
    assert_eq!(stale.state, ModelDiscoveryCacheState::Stale);
    assert!(stale.refresh_in_flight);
    assert!(discovery_cache_needs_refresh(&provider, None, &cache, ttl));

    cache
        .providers
        .get_mut(&ModelDiscoveryCacheKey::new(
            provider.id.clone(),
            crate::config::ProviderEndpointId::default_endpoint(),
        ))
        .unwrap()
        .fetched_at = Utc::now();
    let fresh = discovery_cache_status_for_provider(&provider, None, &cache, ttl, false);
    assert_eq!(fresh.state, ModelDiscoveryCacheState::Fresh);
    assert!(!discovery_cache_needs_refresh(&provider, None, &cache, ttl));
}

#[test]
fn ollama_discovery_urls_replace_openai_v1_suffix_and_preserve_prefix() {
    assert_eq!(
        ollama_tags_url("http://127.0.0.1:11434/v1").unwrap(),
        "http://127.0.0.1:11434/api/tags"
    );
    assert_eq!(
        ollama_show_url("https://example.test/ollama/v1/").unwrap(),
        "https://example.test/ollama/api/show"
    );
}

#[test]
fn ollama_show_maps_context_vision_and_thinking_without_parallel_tools() {
    let show: OllamaShowResponse = serde_json::from_str(
        r#"{
                "capabilities":["completion","vision","tools","thinking"],
                "model_info":{
                    "general.architecture":"qwen3",
                    "qwen3.context_length":131072,
                    "other.context_length":"65536"
                }
            }"#,
    )
    .unwrap();
    let model = show.into_model_metadata(&ProviderId::parse("ollama").unwrap(), "qwen3-vl:latest");

    assert_eq!(model.context_window_tokens, Some(131_072));
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert!(!model.capabilities.parallel_tool_calls);
    assert_eq!(model.source, ModelMetadataSource::RemoteDiscovered);
}

#[tokio::test]
async fn ollama_discovery_uses_tags_then_show_without_authentication() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut tags_stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let read = tags_stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("GET /api/tags HTTP/1.1"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        let body = r#"{"models":[{"name":"qwen3-vl:latest"}]}"#;
        write!(
                tags_stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();

        let (mut show_stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = show_stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("POST /api/show HTTP/1.1"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(request.contains(r#""model":"qwen3-vl:latest""#));
        let body = r#"{
                "capabilities":["vision","tools","thinking"],
                "model_info":{"qwen3.context_length":131072}
            }"#;
        write!(
                show_stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
    });

    let provider = ollama_provider(base_url);
    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("model-discovery-cache.json");
    let report = refresh_provider_models(&provider, None, &cache_path)
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(report.model_count, 1);
    let cache = load_discovery_cache_at(&cache_path).unwrap();
    let entry = cache
        .providers
        .get(&ModelDiscoveryCacheKey::new(
            provider.id.clone(),
            crate::config::ProviderEndpointId::default_endpoint(),
        ))
        .unwrap();
    assert_eq!(
        entry.source_url.as_deref(),
        Some(provider_models_url(&provider, None).unwrap().as_str())
    );
    assert!(entry
        .response_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    let model = &entry.models[0];
    assert_eq!(model.context_window_tokens, Some(131_072));
    assert!(model.capabilities.image_input);
    assert!(model.capabilities.supports_reasoning);
    assert!(!model.capabilities.parallel_tool_calls);
}

#[tokio::test]
async fn ollama_discovery_skips_failed_show_requests_and_caches_healthy_models() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut tags_stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let read = tags_stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /api/tags HTTP/1.1"));
        let body = r#"{"models":[
                {"name":"healthy:latest"},
                {"name":"bad-status:latest"},
                {"name":"bad-json:latest"}
            ]}"#;
        write!(
                tags_stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();

        for (expected_model, status, body) in [
            ("bad-json:latest", "200 OK", "{"),
            (
                "bad-status:latest",
                "500 Internal Server Error",
                r#"{"error":"invalid file magic"}"#,
            ),
            (
                "healthy:latest",
                "200 OK",
                r#"{"capabilities":["tools"],"model_info":{"test.context_length":32768}}"#,
            ),
        ] {
            let (mut show_stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = show_stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /api/show HTTP/1.1"));
            assert!(request.contains(&format!(r#""model":"{expected_model}""#)));
            write!(
                    show_stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
        }
    });

    let provider = ollama_provider(base_url);
    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("model-discovery-cache.json");
    let report = refresh_provider_models(&provider, None, &cache_path)
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(report.model_count, 1);
    let cache = load_discovery_cache_at(&cache_path).unwrap();
    let entry = cache
        .providers
        .get(&ModelDiscoveryCacheKey::new(
            provider.id.clone(),
            crate::config::ProviderEndpointId::default_endpoint(),
        ))
        .unwrap();
    assert_eq!(entry.models.len(), 1);
    assert_eq!(entry.models[0].model_ref.model, "healthy:latest");
}

#[tokio::test]
async fn ollama_discovery_preserves_cache_when_all_show_requests_fail() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut tags_stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let read = tags_stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /api/tags HTTP/1.1"));
        let body = r#"{"models":[{"name":"broken:latest"}]}"#;
        write!(
                tags_stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();

        let (mut show_stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = show_stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("POST /api/show HTTP/1.1"));
        assert!(request.contains(r#""model":"broken:latest""#));
        let body = r#"{"error":"invalid file magic"}"#;
        write!(
                show_stream,
                "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
    });

    let provider = ollama_provider(base_url);
    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("model-discovery-cache.json");
    let old_model = serde_json::from_str::<OllamaShowResponse>("{}")
        .unwrap()
        .into_model_metadata(&provider.id, "cached:latest");
    let mut cache = ModelDiscoveryCacheFile::default();
    cache.providers.insert(
        ModelDiscoveryCacheKey::new(
            provider.id.clone(),
            crate::config::ProviderEndpointId::default_endpoint(),
        ),
        ProviderModelDiscoveryCache {
            provider: provider.id.clone(),
            fetched_at: Utc::now() - chrono::TimeDelta::hours(1),
            source_url: Some("http://cached.example/api/tags".into()),
            response_hash: Some("sha256:cached".into()),
            models: vec![old_model],
        },
    );
    save_discovery_cache_at(&cache_path, &cache).unwrap();
    let old_cache = fs::read(&cache_path).unwrap();

    let error = refresh_provider_models(&provider, None, &cache_path)
        .await
        .unwrap_err();
    server.join().unwrap();

    assert!(
        error
            .to_string()
            .contains("all 1 Ollama model show requests failed"),
        "unexpected error: {error:#}"
    );
    assert_eq!(fs::read(&cache_path).unwrap(), old_cache);
}

fn discovery_path(path: &str) -> ProviderDiscoveryConfig {
    ProviderDiscoveryConfig::openai_models(path)
}

#[test]
fn configured_discovery_joins_path_onto_endpoint_base() {
    let provider = custom_openai_provider("https://gateway.example/v1".into());
    assert_eq!(
        provider_models_url(&provider, Some(&discovery_path("/models"))).unwrap(),
        "https://gateway.example/v1/models"
    );

    // A gateway that keeps the OpenAI base at the host root needs the `/v1`
    // segment declared explicitly, which is exactly the Magpie shape.
    let root = custom_openai_provider("https://gateway.example".into());
    assert_eq!(
        provider_models_url(&root, Some(&discovery_path("/v1/models"))).unwrap(),
        "https://gateway.example/v1/models"
    );
    assert_eq!(
        provider_models_url(&root, Some(&discovery_path("/v1/models")))
            .unwrap()
            .split('/')
            .nth(2),
        Some("gateway.example")
    );
}

#[test]
fn configured_discovery_overrides_and_disables_transport_default() {
    let provider = custom_openai_provider("http://127.0.0.1:8000/v1".into());
    assert!(provider_supports_model_discovery(&provider, None));
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "http://127.0.0.1:8000/v1/models"
    );

    let disabled = ProviderDiscoveryConfig::disabled();
    assert!(!provider_supports_model_discovery(
        &provider,
        Some(&disabled)
    ));
    assert!(provider_models_url(&provider, Some(&disabled)).is_err());
    let cache = ModelDiscoveryCacheFile::default();
    assert_eq!(
        discovery_cache_status_for_provider(
            &provider,
            Some(&disabled),
            &cache,
            Duration::from_secs(60),
            false
        )
        .state,
        ModelDiscoveryCacheState::Unsupported
    );
}

#[test]
fn configured_discovery_path_rejects_escape_attempts() {
    for path in [
        "models",
        "http://evil.example/models",
        "//evil.example/models",
        "/a/../b",
        "/./models",
        "/models?format=text",
        "/models#frag",
        "/%2e%2e/models",
        "/models\\..\\.\\.\\evil",
        "/mo del",
    ] {
        assert!(
            ProviderDiscoveryConfig::openai_models(path)
                .validate("providers.gateway.discovery")
                .is_err(),
            "path {path:?} must be rejected during config parsing"
        );
    }
    assert!(ProviderDiscoveryConfig::openai_models("/v1/models")
        .validate("providers.gateway.discovery")
        .is_ok());
    assert!(ProviderDiscoveryConfig::disabled()
        .validate("providers.gateway.discovery")
        .is_ok());
    assert!(ProviderDiscoveryConfig {
        kind: ProviderDiscoveryKind::Disabled,
        path: Some("/models".to_string()),
        decoder: None,
    }
    .validate("providers.gateway.discovery")
    .is_err());
    assert!(ProviderDiscoveryConfig {
        kind: ProviderDiscoveryKind::OpenAiModels,
        path: None,
        decoder: None,
    }
    .validate("providers.gateway.discovery")
    .is_err());
}

#[test]
fn discovery_cache_key_separates_sibling_endpoints() {
    let first = custom_openai_provider("https://gateway.example/v1".into());
    let mut second = first.clone();
    second.route_endpoint = ProviderEndpointId::parse("anthropic").unwrap();
    second.base_url = "https://gateway.example".into();

    let mut cache = ModelDiscoveryCacheFile::default();
    for provider in [&first, &second] {
        cache.providers.insert(
            ModelDiscoveryCacheKey::for_provider(provider),
            ProviderModelDiscoveryCache {
                provider: provider.route_provider.clone(),
                fetched_at: Utc::now(),
                source_url: Some(
                    provider_models_url(provider, Some(&discovery_path("/models"))).unwrap(),
                ),
                response_hash: None,
                models: vec![BuiltInModelMetadata {
                    model_ref: ModelRef::new(
                        ProviderId::parse("local-custom").unwrap(),
                        "shared-model",
                    ),
                    display_name: "shared-model".into(),
                    description: "remote".into(),
                    context_window_tokens: None,
                    effective_context_window_percent: 95,
                    auto_compact_token_limit: None,
                    default_max_output_tokens: None,
                    max_output_tokens_upper_limit: None,
                    default_verbosity: None,
                    tool_output_truncation_estimated_tokens: None,
                    capabilities: ModelCapabilityFlags::default(),
                    reasoning_effort_options: Vec::new(),
                    source: ModelMetadataSource::RemoteDiscovered,
                    endpoint: (provider.route_endpoint.as_str() != ProviderEndpointId::DEFAULT)
                        .then(|| provider.route_endpoint.clone()),
                }],
            },
        );
    }

    let models = cache.models();
    assert_eq!(models.len(), 2, "one entry per endpoint");
    assert!(models.contains_key(&ModelRouteRef::new(
        ProviderId::parse("local-custom").unwrap(),
        ProviderEndpointId::default_endpoint(),
        "shared-model"
    )));
    assert!(models.contains_key(&ModelRouteRef::new(
        ProviderId::parse("local-custom").unwrap(),
        ProviderEndpointId::parse("anthropic").unwrap(),
        "shared-model"
    )));

    let ttl = Duration::from_secs(60);
    assert_eq!(
        discovery_cache_status_for_provider(&first, None, &cache, ttl, false).state,
        ModelDiscoveryCacheState::Fresh
    );
    assert_eq!(
        discovery_cache_status_for_provider(
            &second,
            Some(&discovery_path("/models")),
            &cache,
            ttl,
            false
        )
        .state,
        ModelDiscoveryCacheState::Fresh
    );
    // The endpoint-scoped entry describes only `/models` on the root base, so
    // the transport default `/v1/models` on the same endpoint is still stale.
    assert_eq!(
        discovery_cache_status_for_provider(&second, None, &cache, ttl, false).state,
        ModelDiscoveryCacheState::Stale
    );
}

#[test]
fn discovery_cache_key_reads_legacy_provider_keys() {
    let legacy = r#"{
        "providers": {
            "local-custom": {
                "provider": "local-custom",
                "fetched_at": "2026-01-01T00:00:00Z",
                "source_url": "http://127.0.0.1:8000/v1/models",
                "models": []
            }
        }
    }"#;
    let cache: ModelDiscoveryCacheFile = serde_json::from_str(legacy).unwrap();
    let provider = custom_openai_provider("http://127.0.0.1:8000".into());
    assert_eq!(
        discovery_cache_status_for_provider(
            &provider,
            None,
            &cache,
            Duration::from_secs(60 * 60 * 24 * 365),
            false
        )
        .state,
        ModelDiscoveryCacheState::Fresh,
        "a bare provider key keeps the default-endpoint cache entry readable"
    );
}

#[tokio::test]
async fn configured_discovery_attributes_models_to_provider_and_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(
            request.starts_with("GET /models HTTP/1.1"),
            "the configured path is relative to the endpoint base: {request}"
        );
        let body = r#"{"object":"list","data":[{"id":"vendor/chat-model"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });

    let mut provider = custom_openai_provider(base_url);
    provider.transport = ProviderTransportKind::AnthropicMessages;
    provider.route_endpoint = ProviderEndpointId::parse("anthropic").unwrap();
    provider.id = ProviderId::parse("gateway-anthropic").unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("model-discovery-cache.json");
    let discovery = discovery_path("/models");
    let report = refresh_provider_models(&provider, Some(&discovery), &cache_path)
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(
        report.endpoint,
        ProviderEndpointId::parse("anthropic").unwrap()
    );
    assert_eq!(report.provider, ProviderId::parse("local-custom").unwrap());
    let cache = load_discovery_cache_at(&cache_path).unwrap();
    let models = cache.models();
    let route = ModelRouteRef::new(
        ProviderId::parse("local-custom").unwrap(),
        ProviderEndpointId::parse("anthropic").unwrap(),
        "vendor/chat-model",
    );
    let model = models.get(&route).expect("model keeps its endpoint owner");
    assert_eq!(
        model.endpoint,
        Some(ProviderEndpointId::parse("anthropic").unwrap())
    );
    assert_eq!(
        model.model_ref,
        ModelRef::parse("local-custom/vendor/chat-model").unwrap()
    );
}

#[tokio::test]
async fn configured_discovery_bounds_same_origin_redirect_loops() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let location = format!("{base_url}/models");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_server = stop.clone();
    let server = thread::spawn(move || {
        let mut served = 0_usize;
        while !stop_server.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let mut request = [0_u8; 2048];
                    let _ = stream.read(&mut request).unwrap();
                    served += 1;
                    write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    )
                    .unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        served
    });

    let provider = custom_openai_provider(base_url);
    let discovery = discovery_path("/models");
    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("model-discovery-cache.json");
    let error = refresh_provider_models(&provider, Some(&discovery), &cache_path)
        .await
        .expect_err("a same-origin redirect loop must fail instead of looping forever");
    stop.store(true, Ordering::Relaxed);
    let served = server.join().unwrap();

    let message = format!("{error:#}");
    assert!(
        message.contains("exceeded 10 redirects"),
        "loop must stop at the hop cap: {message}"
    );
    assert!(
        served <= DISCOVERY_REDIRECT_LIMIT + 2,
        "redirect loop issued {served} discovery requests"
    );
}

#[tokio::test]
async fn configured_discovery_does_not_follow_cross_origin_redirects() {
    let target_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    target_listener.set_nonblocking(true).unwrap();
    let target_location = format!("http://{}/models", target_listener.local_addr().unwrap());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nlocation: {target_location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
        .unwrap();
    });

    let provider = custom_openai_provider(base_url);
    let discovery = discovery_path("/models");
    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().join("model-discovery-cache.json");
    let error = refresh_provider_models(&provider, Some(&discovery), &cache_path)
        .await
        .expect_err("a cross-origin redirect must not be followed");
    server.join().unwrap();

    let message = format!("{error:#}");
    assert!(
        message.contains("cross-origin redirects are not allowed")
            || message.contains("redirect was not followed"),
        "unexpected error: {message}"
    );
    assert!(
        matches!(target_listener.accept(), Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the cross-origin redirect target must never be contacted"
    );
}

#[test]
fn configured_decoder_drives_magpie_payload_via_effective_discovery() {
    let provider = custom_openai_provider("http://magpie.internal:8080".into());
    let configured = ProviderDiscoveryConfig {
        kind: ProviderDiscoveryKind::OpenAiModels,
        path: Some("/v1/models".into()),
        decoder: Some(ModelDiscoveryDecoder::Magpie),
    };

    // A configured decoder flows through effective_discovery rather than being
    // constructed directly, proving the config -> effective wiring.
    let discovery = effective_discovery(&provider, Some(&configured)).expect("discovery enabled");
    assert_eq!(discovery.decoder, ModelDiscoveryDecoder::Magpie);
    assert_eq!(
        discovery.route,
        EffectiveDiscoveryRoute::EndpointPath("/v1/models".to_string())
    );
    assert_eq!(
        discovery_models_url(&provider, &discovery).unwrap(),
        "http://magpie.internal:8080/v1/models"
    );

    let payload = br#"{"data":[{"id":"magpie-chat","display_name":"Magpie Chat","context_window":131072,"owned_by":"magpie","kind":"chat"}]}"#;
    let models =
        decode_discovered_models(&provider, discovery.decoder, payload, Utc::now()).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model_ref.model, "magpie-chat");
    assert_eq!(models[0].display_name, "Magpie Chat");
    assert_eq!(models[0].context_window_tokens, Some(131072));
}

#[test]
fn built_in_magpie_discovers_models_through_openai_v1_and_magpie_decoder() {
    let provider = magpie_provider("http://127.0.0.1:3425".into());
    let discovery = effective_discovery(&provider, None).expect("built-in discovery");
    assert_eq!(discovery.auth, ModelDiscoveryAuth::Optional);
    assert_eq!(
        discovery.route,
        EffectiveDiscoveryRoute::BuiltIn(ModelDiscoveryRoute::OpenAiV1)
    );
    assert_eq!(discovery.decoder, ModelDiscoveryDecoder::Magpie);
    assert_eq!(
        provider_models_url(&provider, None).unwrap(),
        "http://127.0.0.1:3425/v1/models"
    );

    // A root base_url and an explicit `/v1` base both resolve to the same
    // discovery GET, so users only need to change the port at most.
    let with_v1 = magpie_provider("http://127.0.0.1:3425/v1".into());
    assert_eq!(
        provider_models_url(&with_v1, None).unwrap(),
        "http://127.0.0.1:3425/v1/models"
    );
}
