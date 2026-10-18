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
            image_input,
            supports_reasoning,
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
// does not establish exact limits or capabilities.
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
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-flashx",
    "kimi-k3",
    "kimi-k2.8-preview",
    "mimo-v2.6-pro",
    "mimo-v2.6-flash",
    "mimo-v2.5-pro",
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
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro-0813",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-flashx",
    "glm-5.2",
    "kimi-k3",
    "kimi-k2.6",
    "minimax-m3",
    "minimax-m2.7",
    "qwen3.5-flash",
    "qwen3.5-plus",
    "mimo-v2.6-pro",
    "mimo-v2.6-flash",
    "mimo-v2.5-pro",
];

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
                                 support; exact endpoint limits and capabilities unconfirmed."
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
            policy: BuiltInModelRoutePolicy::default(),
        })
        .chain(
            TENCENT_TOKENHUB_RESPONSES_MODEL_IDS
                .iter()
                .map(|id| BuiltInModelRouteDefinition {
                    legacy_provider: provider_id("tencent-tokenhub-responses"),
                    model_ref: ModelRef::new(provider_id("tencent-tokenhub"), *id),
                    endpoint: ProviderEndpointId::parse("responses").expect("valid endpoint"),
                    policy: BuiltInModelRoutePolicy::default(),
                }),
        )
        .collect()
}

#[test]
fn tokenhub_responses_routes_follow_the_independent_protocol_matrix() {
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    assert_eq!(TENCENT_TOKENHUB_RESPONSES_MODEL_IDS.len(), 28);
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
        assert_eq!(definition, &BuiltInModelRoutePolicy::default());
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
        assert_eq!(metadata.capabilities, ModelCapabilityFlags::default());
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
