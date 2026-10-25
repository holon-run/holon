#[cfg(test)]
use super::super::*;

#[cfg(test)]
fn tencent_tokenhub_model(
    model: &str,
    display_name: &str,
    context_window_tokens: Option<usize>,
    max_output_tokens: Option<u32>,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("tencent-tokenhub"), model);
    let (chat_reasoning, chat_image) = reviewed_chat_capabilities(model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Tencent TokenHub {model} model."
        ),
        context_window_tokens,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: max_output_tokens,
        max_output_tokens_upper_limit: max_output_tokens,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input: image_input || chat_image,
            supports_reasoning: supports_reasoning || chat_reasoning,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

type TencentTokenHubModelSpec = (
    &'static str,
    &'static str,
    Option<usize>,
    Option<u32>,
    bool,
    bool,
);

const TENCENT_TOKENHUB_MODELS: &[TencentTokenHubModelSpec] = &[
    ("hy3", "Hy3", Some(256_000), Some(128_000), true, false),
    (
        "hy3-preview",
        "Hy3 Preview",
        Some(256_000),
        Some(128_000),
        true,
        false,
    ),
    ("hy-mt2-pro", "Hy-MT2-Pro", None, None, false, false),
    ("hy-mt2-plus", "Hy-MT2-Plus", None, None, false, false),
    ("hy-mt2-lite", "Hy-MT2-Lite", None, None, false, false),
    (
        "hunyuan-role-latest",
        "Hy-Role-Latest",
        None,
        None,
        false,
        false,
    ),
    ("hy-role", "Hy-Role", None, None, false, false),
    (
        "deepseek-v4-flash",
        "DeepSeek V4 Flash",
        Some(1_000_000),
        Some(384_000),
        true,
        false,
    ),
    (
        "deepseek-v4-pro",
        "DeepSeek V4 Pro",
        Some(1_000_000),
        Some(384_000),
        true,
        false,
    ),
    (
        "deepseek-v3.2",
        "DeepSeek V3.2",
        Some(128_000),
        Some(32_768),
        true,
        false,
    ),
    (
        "glm-5.2",
        "GLM-5.2",
        Some(1_000_000),
        Some(131_072),
        true,
        false,
    ),
    (
        "glm-5.1",
        "GLM-5.1",
        Some(202_800),
        Some(131_072),
        true,
        false,
    ),
    (
        "glm-5v-turbo",
        "GLM-5V Turbo",
        Some(202_800),
        Some(131_072),
        true,
        true,
    ),
    (
        "glm-5-turbo",
        "GLM-5 Turbo",
        Some(202_800),
        Some(131_072),
        true,
        false,
    ),
    ("glm-5", "GLM-5", Some(202_800), Some(131_072), true, false),
    (
        "kimi-k2.7-code-highspeed",
        "Kimi K2.7 Code HighSpeed",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.7-code",
        "Kimi K2.7 Code",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.6",
        "Kimi K2.6",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.5",
        "Kimi K2.5",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "minimax-m3",
        "MiniMax M3",
        Some(1_000_000),
        None,
        true,
        true,
    ),
    (
        "minimax-m2.7",
        "MiniMax M2.7",
        Some(204_800),
        None,
        true,
        false,
    ),
    (
        "minimax-m2.5",
        "MiniMax M2.5",
        Some(196_608),
        Some(32_768),
        true,
        false,
    ),
    (
        "qwen3.5-flash",
        "Qwen3.5 Flash",
        Some(1_000_000),
        Some(65_536),
        true,
        true,
    ),
    (
        "qwen3.5-plus",
        "Qwen3.5 Plus",
        Some(1_000_000),
        Some(65_536),
        true,
        true,
    ),
    ("youtu-vita", "YT-VITA", None, None, false, true),
    (
        "hy-vision-2.0-instruct",
        "HY-Vision-2.0-Instruct",
        None,
        None,
        false,
        true,
    ),
    (
        "hunyuan-t1-vision-20250916",
        "HY-Vision-1.5-Thinking",
        None,
        None,
        true,
        true,
    ),
];

// The TokenHub protocol matrix confirms Chat and Messages for these IDs, but
// does not itself establish exact limits or capabilities. Separate Chat
// guides below refine only the reviewed Chat metadata.
// https://cloud.tencent.com/document/product/1823/130079
const TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS: &[&str] = &[
    "hy4-preview",
    "deepseek/deepseek-flash",
    "deepseek-v4-flash-202605",
    "deepseek/deepseek-v4-flash-0731",
    "deepseek/deepseek-v4-flash",
    "deepseek-v4-pro-202606",
    "deepseek/deepseek-v4-pro-0813",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-flash-vision-exp",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro-0813",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-1017",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-flashx",
    "kimi-k3",
    "kimi-k3.1-pro",
    "kimi-k3.1-pro-highspeed",
    "kimi-k2.8-preview",
    "mimo-v2.6-pro",
    "mimo-v2.6-flash",
    "mimo-v2.5-pro",
    "minimax-m3.1-flash",
    "step-5-preview",
];

#[cfg(test)]
const TENCENT_TOKENHUB_CHAT_ONLY_MODEL_IDS: &[&str] = &["hy-mt2-pro", "hy-mt2-plus", "hy-mt2-lite"];

// Reviewed independently of Chat/Messages: the protocol matrix excludes
// several models and marks GLM-5.2 as compatibility mode.
#[cfg(test)]
const TENCENT_TOKENHUB_RESPONSES_MODEL_IDS: &[&str] = &[
    "hy4-preview",
    "hy3",
    "deepseek/deepseek-flash",
    "deepseek-v4-flash-202605",
    "deepseek/deepseek-v4-flash-0731",
    "deepseek/deepseek-v4-flash",
    "deepseek-v4-pro-202606",
    "deepseek/deepseek-v4-pro-0813",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-flash-vision-exp",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-1017",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro-0813",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-flashx",
    "glm-5.2",
    "kimi-k3",
    "kimi-k3.1-pro",
    "kimi-k3.1-pro-highspeed",
    "kimi-k2.6",
    "minimax-m3",
    "minimax-m3.1-flash",
    "minimax-m2.7",
    "qwen3.5-flash",
    "qwen3.5-plus",
    "mimo-v2.6-pro",
    "mimo-v2.6-flash",
    "mimo-v2.5-pro",
];

// Chat guides establish these capabilities independently of protocol eligibility.
// New intrinsic capabilities must be narrowed back on unreviewed protocols.
// https://cloud.tencent.com/document/product/1823/131208
// https://cloud.tencent.com/document/product/1823/132248
// https://cloud.tencent.com/document/product/1823/132061
// https://cloud.tencent.com/document/product/1823/132232
#[cfg(test)]
fn reviewed_chat_capabilities(id: &str) -> (bool, bool) {
    let reasoning = matches!(
        id,
        "hy4-preview"
            | "deepseek/deepseek-flash"
            | "deepseek-v4-flash-202605"
            | "deepseek/deepseek-v4-flash-0731"
            | "deepseek/deepseek-v4-flash"
            | "deepseek-v4-pro-202606"
            | "deepseek/deepseek-v4-pro-0813"
            | "deepseek/deepseek-v4-pro"
            | "deepseek-v4-flash-0731"
            | "deepseek-v4-pro-0813"
            | "deepseek/deepseek-v4-flash-vision-exp"
            | "glm-5.3"
            | "glm-5.3-flash"
            | "glm-5.3-flashx"
            | "kimi-k3"
            | "kimi-k3.1-pro"
            | "kimi-k3.1-pro-highspeed"
            | "kimi-k2.8-preview"
    );
    let image = matches!(
        id,
        "glm-5.3-flash"
            | "glm-5.3-flashx"
            | "kimi-k3"
            | "kimi-k3.1-pro"
            | "kimi-k3.1-pro-highspeed"
            | "kimi-k2.8-preview"
    );
    (reasoning, image)
}

#[cfg(test)]
fn reviewed_chat_policy(id: &str) -> BuiltInModelRoutePolicy {
    let (default, ceiling) = match id {
        "kimi-k3" | "kimi-k3.1-pro" | "kimi-k3.1-pro-highspeed" => {
            (Some(Some(131_072)), Some(Some(1_048_576)))
        }
        "kimi-k2.7-code" | "kimi-k2.7-code-highspeed" | "kimi-k2.8-preview" => {
            (Some(Some(32_768)), None)
        }
        _ => (None, None),
    };
    let effort: &[&str] = match id {
        "hy4-preview" => &["high", "none"],
        "hy3" => &["low", "high", "none"],
        "deepseek-v4-flash"
        | "deepseek-v4-pro"
        | "glm-5.3"
        | "glm-5.3-flash"
        | "glm-5.3-flashx"
        | "kimi-k3"
        | "kimi-k3.1-pro"
        | "kimi-k3.1-pro-highspeed"
        | "kimi-k2.8-preview" => &["low", "high", "max"],
        // Accepted aliases, not seven independent strengths:
        // xhigh=max; medium=low=high; minimal=none=thinking disabled.
        "glm-5.2" => &["max", "xhigh", "high", "medium", "low", "minimal", "none"],
        _ => &[],
    };
    BuiltInModelRoutePolicy {
        default_max_output_tokens: default,
        max_output_tokens_upper_limit: ceiling,
        reasoning_effort_options: (!effort.is_empty())
            .then(|| effort.iter().map(|value| (*value).to_owned()).collect()),
        ..BuiltInModelRoutePolicy::default()
    }
}

#[cfg(test)]
fn unreviewed_protocol_policy(id: &str) -> BuiltInModelRoutePolicy {
    let (reasoning, image) = reviewed_chat_capabilities(id);
    BuiltInModelRoutePolicy {
        capabilities: ModelCapabilityOverride {
            supports_reasoning: reasoning.then_some(false),
            image_input: image.then_some(false),
            ..ModelCapabilityOverride::default()
        },
        ..BuiltInModelRoutePolicy::default()
    }
}

pub(crate) fn is_tencent_tokenhub_model_id(id: &str) -> bool {
    TENCENT_TOKENHUB_MODELS.iter().any(|model| model.0 == id)
        || TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS.contains(&id)
}

#[cfg(test)]
pub(super) fn entries() -> Vec<BuiltInModelMetadata> {
    TENCENT_TOKENHUB_MODELS
        .iter()
        .map(|model| tencent_tokenhub_model(model.0, model.1, model.2, model.3, model.4, model.5))
        .chain(TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS.iter().map(|id| {
            let mut entry = tencent_tokenhub_model(id, id, None, None, false, false);
            entry.description = "Official TokenHub language-model ID with Chat and Messages \
                                 support; only independently reviewed Chat fields are populated."
                .into();
            entry
        }))
        .collect()
}

#[cfg(test)]
pub(super) fn route_definitions() -> Vec<BuiltInModelRouteDefinition> {
    TENCENT_TOKENHUB_MODELS
        .iter()
        .map(|model| model.0)
        .filter(|id| !TENCENT_TOKENHUB_CHAT_ONLY_MODEL_IDS.contains(id))
        .chain(TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS.iter().copied())
        .map(|id| BuiltInModelRouteDefinition {
            legacy_provider: provider_id("tencent-tokenhub-messages"),
            model_ref: ModelRef::new(provider_id("tencent-tokenhub"), id),
            endpoint: ProviderEndpointId::parse("messages").expect("valid endpoint"),
            policy: unreviewed_protocol_policy(id),
        })
        .chain(
            TENCENT_TOKENHUB_RESPONSES_MODEL_IDS
                .iter()
                .map(|id| BuiltInModelRouteDefinition {
                    legacy_provider: provider_id("tencent-tokenhub-responses"),
                    model_ref: ModelRef::new(provider_id("tencent-tokenhub"), *id),
                    endpoint: ProviderEndpointId::parse("responses").expect("valid endpoint"),
                    policy: unreviewed_protocol_policy(id),
                }),
        )
        .chain(
            TENCENT_TOKENHUB_MODELS
                .iter()
                .map(|model| model.0)
                .chain(TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS.iter().copied())
                .filter_map(|id| {
                    let policy = reviewed_chat_policy(id);
                    (policy != BuiltInModelRoutePolicy::default()).then(|| {
                        BuiltInModelRouteDefinition {
                            legacy_provider: provider_id("tencent-tokenhub"),
                            model_ref: ModelRef::new(provider_id("tencent-tokenhub"), id),
                            endpoint: ProviderEndpointId::default_endpoint(),
                            policy,
                        }
                    })
                }),
        )
        .collect()
}

#[test]
fn tokenhub_responses_routes_follow_the_independent_protocol_matrix() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    assert_eq!(TENCENT_TOKENHUB_RESPONSES_MODEL_IDS.len(), 32);
    for id in TENCENT_TOKENHUB_MODELS
        .iter()
        .map(|model| model.0)
        .chain(TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS.iter().copied())
    {
        let route = ModelRouteRef::new(
            provider_id("tencent-tokenhub"),
            ProviderEndpointId::parse("responses").unwrap(),
            id,
        );
        assert_eq!(
            catalog.get_route(&route).is_some(),
            TENCENT_TOKENHUB_RESPONSES_MODEL_IDS.contains(&id),
            "{id}"
        );
    }
    for id in TENCENT_TOKENHUB_RESPONSES_MODEL_IDS {
        let route = ModelRouteRef::new(
            provider_id("tencent-tokenhub"),
            ProviderEndpointId::parse("responses").unwrap(),
            *id,
        );
        let definition = catalog.get_route(&route).expect("reviewed Responses ID");
        assert_eq!(definition, &unreviewed_protocol_policy(id));
    }
}

#[test]
fn tokenhub_preferred_routes_do_not_inherit_unreviewed_chat_capabilities() {
    for catalog in [
        BuiltInModelCatalog::from_legacy_definitions(),
        BuiltInModelCatalog::new(),
    ] {
        let legacy = provider_id("tencent-tokenhub-responses");
        assert_eq!(
            catalog.preferred_model_for_provider(&legacy).unwrap().model,
            "hy3"
        );
        assert_eq!(
            catalog
                .preferred_route_for_provider(&legacy)
                .unwrap()
                .as_string(),
            "tencent-tokenhub@responses/hy3"
        );
    }
}

#[test]
fn tokenhub_protocol_matrix_routes_do_not_inherit_vendor_limits() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    for id in TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS {
        assert!(is_tencent_tokenhub_model_id(id));
        let model = ModelRef::new(provider_id("tencent-tokenhub"), *id);
        let metadata = catalog.get(&model).unwrap();
        assert_eq!(metadata.context_window_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, None);
        let (reasoning, image) = reviewed_chat_capabilities(id);
        assert_eq!(metadata.capabilities.supports_reasoning, reasoning);
        assert_eq!(metadata.capabilities.image_input, image);
        let chat = ModelRouteRef::new(
            model.provider.clone(),
            ProviderEndpointId::default_endpoint(),
            model.model.clone(),
        );
        assert!(catalog.get_route(&chat).is_some());
        let messages = ModelRouteRef::new(
            model.provider,
            ProviderEndpointId::parse("messages").unwrap(),
            model.model,
        );
        assert!(catalog.get_route(&messages).is_some(), "{id}");
    }
    for model in TENCENT_TOKENHUB_MODELS {
        let messages = ModelRouteRef::new(
            provider_id("tencent-tokenhub"),
            ProviderEndpointId::parse("messages").unwrap(),
            model.0,
        );
        assert_eq!(
            catalog.get_route(&messages).is_some(),
            !TENCENT_TOKENHUB_CHAT_ONLY_MODEL_IDS.contains(&model.0),
            "{}",
            model.0
        );
    }
}

#[test]
fn tokenhub_kimi_k3_1_routes_keep_endpoint_metadata_conservative() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    for id in ["kimi-k3.1-pro", "kimi-k3.1-pro-highspeed"] {
        assert!(is_tencent_tokenhub_model_id(id));
        let model = ModelRef::new(provider_id("tencent-tokenhub"), id);
        let metadata = catalog.get(&model).expect("reviewed TokenHub model ID");
        assert_eq!(metadata.context_window_tokens, None);
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, None);
        assert!(metadata.capabilities.supports_reasoning);
        assert!(metadata.capabilities.image_input);
        assert!(metadata.reasoning_effort_options.is_empty());
        for endpoint in ["default", "messages", "responses"] {
            let route = ModelRouteRef::new(
                provider_id("tencent-tokenhub"),
                ProviderEndpointId::parse(endpoint).unwrap(),
                id,
            );
            let expected = if endpoint == "default" {
                reviewed_chat_policy(id)
            } else {
                unreviewed_protocol_policy(id)
            };
            assert_eq!(
                catalog.get_route(&route),
                Some(&expected),
                "{id}@{endpoint}"
            );
        }
    }
    for id in ["kimi-k3-pro", "kimi-k3-pro-highspeed"] {
        assert!(!is_tencent_tokenhub_model_id(id));
        assert!(catalog
            .get(&ModelRef::new(provider_id("tencent-tokenhub"), id))
            .is_none());
        for endpoint in ["default", "messages", "responses"] {
            let route = ModelRouteRef::new(
                provider_id("tencent-tokenhub"),
                ProviderEndpointId::parse(endpoint).unwrap(),
                id,
            );
            assert!(catalog.get_route(&route).is_none(), "{id}@{endpoint}");
        }
    }
}

#[test]
fn tokenhub_chat_budgets_and_effort_do_not_leak_to_other_protocols() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    for (id, default, ceiling) in [
        ("kimi-k3", 131_072, Some(1_048_576)),
        ("kimi-k3.1-pro", 131_072, Some(1_048_576)),
        ("kimi-k3.1-pro-highspeed", 131_072, Some(1_048_576)),
        ("kimi-k2.7-code", 32_768, Some(262_144)),
        ("kimi-k2.7-code-highspeed", 32_768, Some(262_144)),
        ("kimi-k2.8-preview", 32_768, None),
    ] {
        let route = ModelRouteRef::parse(&format!("tencent-tokenhub@default/{id}")).unwrap();
        let chat = catalog.resolve_route_policy(
            &route,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &ContextConfig::default(),
            8_192,
        );
        assert_eq!(chat.runtime_max_output_tokens, default, "{id}");
        assert_eq!(chat.max_output_tokens_upper_limit, ceiling, "{id}");
        let metadata = catalog.get(&route.model_ref()).unwrap();
        for endpoint in ["messages", "responses"] {
            let route = ModelRouteRef::parse(&format!("tencent-tokenhub@{endpoint}/{id}")).unwrap();
            if catalog.get_route(&route).is_none() {
                continue;
            }
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(
                policy.runtime_max_output_tokens,
                metadata.default_max_output_tokens.unwrap_or(8_192)
            );
            assert_eq!(
                policy.max_output_tokens_upper_limit,
                metadata.max_output_tokens_upper_limit
            );
            assert!(
                policy.reasoning_effort_options.is_empty(),
                "{id}@{endpoint}"
            );
        }
    }
}

#[test]
fn tokenhub_reviewed_chat_capabilities_and_effort_preserve_unknown_fields() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    for id in TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS {
        let (reasoning, image) = reviewed_chat_capabilities(id);
        for endpoint in ["default", "messages", "responses"] {
            let route = ModelRouteRef::parse(&format!("tencent-tokenhub@{endpoint}/{id}")).unwrap();
            if catalog.get_route(&route).is_none() {
                continue;
            }
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(policy.context_window_tokens, None, "{id}@{endpoint}");
            assert_eq!(
                policy.capabilities.supports_reasoning,
                endpoint == "default" && reasoning,
                "{id}@{endpoint}"
            );
            assert_eq!(
                policy.capabilities.image_input,
                endpoint == "default" && image,
                "{id}@{endpoint}"
            );
        }
    }
    for (id, expected) in [
        ("hy4-preview", vec!["high", "none"]),
        ("hy3", vec!["low", "high", "none"]),
        ("deepseek-v4-flash", vec!["low", "high", "max"]),
        ("deepseek-v4-pro", vec!["low", "high", "max"]),
        ("glm-5.3", vec!["low", "high", "max"]),
        ("glm-5.3-flash", vec!["low", "high", "max"]),
        ("glm-5.3-flashx", vec!["low", "high", "max"]),
        ("kimi-k3", vec!["low", "high", "max"]),
        ("kimi-k3.1-pro", vec!["low", "high", "max"]),
        ("kimi-k3.1-pro-highspeed", vec!["low", "high", "max"]),
        ("kimi-k2.8-preview", vec!["low", "high", "max"]),
        (
            "glm-5.2",
            vec!["max", "xhigh", "high", "medium", "low", "minimal", "none"],
        ),
    ] {
        for endpoint in ["default", "messages", "responses"] {
            let route = ModelRouteRef::parse(&format!("tencent-tokenhub@{endpoint}/{id}")).unwrap();
            if catalog.get_route(&route).is_none() {
                continue;
            }
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            if endpoint == "default" {
                assert_eq!(policy.reasoning_effort_options, expected, "{id}");
            } else {
                assert!(
                    policy.reasoning_effort_options.is_empty(),
                    "{id}@{endpoint}"
                );
            }
        }
    }
}
