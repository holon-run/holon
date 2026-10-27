use super::super::*;
use super::common::catalog_model;

pub(super) fn stepfun_model(
    provider: &str,
    model: &str,
    display_name: &str,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id(provider), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!("Holon built-in runtime metadata for the StepFun {model} model."),
        context_window_tokens: Some(262_144),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::BuiltInCatalog,
        endpoint: None,
    }
}

fn route_definition(
    legacy_provider: &str,
    canonical_provider: &str,
    endpoint: &str,
    model: &str,
    context_window_tokens: Option<usize>,
    max_output_tokens: Option<u32>,
) -> BuiltInModelRouteDefinition {
    BuiltInModelRouteDefinition {
        legacy_provider: provider_id(legacy_provider),
        model_ref: ModelRef::new(provider_id(canonical_provider), model),
        endpoint: ProviderEndpointId::parse(endpoint).expect("valid endpoint"),
        policy: BuiltInModelRoutePolicy {
            context_window_tokens: context_window_tokens.map(Some),
            default_max_output_tokens: max_output_tokens.map(Some),
            max_output_tokens_upper_limit: max_output_tokens.map(Some),
            ..BuiltInModelRoutePolicy::default()
        },
    }
}

fn volcengine_route_definition(
    legacy_provider: &str,
    endpoint: &str,
    model: &str,
    display_name: Option<&str>,
) -> BuiltInModelRouteDefinition {
    BuiltInModelRouteDefinition {
        legacy_provider: provider_id(legacy_provider),
        model_ref: ModelRef::new(provider_id("volcengine"), model),
        endpoint: ProviderEndpointId::parse(endpoint).expect("valid endpoint"),
        policy: BuiltInModelRoutePolicy {
            display_name: display_name.map(str::to_owned),
            description: (model == "glm-5.2").then(|| {
                "Legacy GLM-5.2 subscription ID forwards to GLM-5.3; not original GLM-5.2 service."
                    .into()
            }),
            capabilities: ModelCapabilityOverride {
                // Coding Plan explicitly documents K2.7 images; Agent Plan
                // eligibility alone is not evidence of the same capability.
                image_input: (endpoint == "plan" && model == "kimi-k2.7-code").then_some(false),
                ..ModelCapabilityOverride::default()
            },
            ..BuiltInModelRoutePolicy::default()
        },
    }
}

// Eligibility comes from the separate Coding/Agent Plan lists, not API models.
// Public limits use "k" without an exact numeric definition; leave hard token
// limits unset rather than inherit conflicting API or third-party values.
fn reviewed_subscription_model(
    provider: &str,
    endpoint: &str,
    model: &str,
    display_name: &str,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let tier_note = if provider == "volcengine" && model == "kimi-k3" {
        " Kimi K3 requires Medium/Large/Max on Agent Plan."
    } else {
        ""
    };
    BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id(provider), model),
        display_name: display_name.into(),
        description: format!(
            "{provider} {endpoint} subscription {display_name}; exact token limits unconfirmed.{tier_note}"
        ),
        context_window_tokens: None,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        default_verbosity: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            supports_reasoning,
            image_input,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: Some(ProviderEndpointId::parse(endpoint).expect("valid endpoint")),
    }
}

pub(super) fn route_definitions() -> Vec<BuiltInModelRouteDefinition> {
    let mut definitions = ["step-3.7-flash", "step-3.5-flash-2603", "step-3.5-flash"]
        .into_iter()
        .map(|model| BuiltInModelRouteDefinition {
            legacy_provider: provider_id("stepfun-plan"),
            model_ref: ModelRef::new(provider_id("stepfun"), model),
            endpoint: ProviderEndpointId::parse("plan").expect("valid endpoint"),
            policy: BuiltInModelRoutePolicy::default(),
        })
        .collect::<Vec<_>>();
    definitions.push(route_definition(
        "xiaomi-token-plan",
        "xiaomi",
        "token-plan",
        "mimo-v2.6-pro",
        None,
        None,
    ));
    definitions.extend(
        [
            ("qwen3.7-max", None, None),
            ("qwen3.7-plus", None, None),
            ("qwen3.8-max", None, None),
            ("qwen3.8-max-preview", None, None),
            ("qwen3.8-flash", None, None),
            ("qwen3.6-plus", None, None),
            ("qwen3.6-flash", None, None),
            ("deepseek-v4-pro", None, Some(65_536)),
            ("deepseek-v4-pro-0813", None, None),
            ("deepseek-v4.1-flash", None, None),
            ("deepseek-v4.1-flash-1017", None, None),
            ("deepseek-v4-flash", None, Some(65_536)),
            ("deepseek-v4-flash-0731", None, Some(65_536)),
            ("deepseek-v3.2", None, None),
            ("kimi-k2.7-code", None, Some(65_536)),
            ("kimi-k2.6", None, Some(65_536)),
            ("kimi-k2.5", None, None),
            ("glm-5.2", Some(1_000_000), Some(65_536)),
            ("glm-5.3", None, None),
            ("glm-5.1", None, Some(65_536)),
            ("glm-5", None, None),
            ("MiniMax-M2.5", None, None),
        ]
        .into_iter()
        .map(|(model, context, output)| {
            let mut definition = route_definition(
                "dashscope-token-plan",
                "dashscope",
                "token-plan",
                model,
                context,
                output,
            );
            // GLM-5.3 documents implicit caching only; Token Plan does not
            // separately guarantee explicit breakpoints. Keep this route conservative.
            // The two reviewed DeepSeek routes also use implicit caching (#3698).
            // Do not generalize this policy to unreviewed models or endpoints.
            if matches!(
                model,
                "glm-5.3" | "deepseek-v4.1-flash" | "deepseek-v4-flash-0731"
            ) {
                definition.policy.cache_capabilities = Some(ModelRouteCacheCapabilities {
                    cache_control: Some(false),
                });
            }
            definition
        }),
    );
    definitions.extend(
        [
            "qwen3.7-plus",
            "qwen3.6-plus",
            "qwen3.5-plus",
            "qwen3-max-2026-01-23",
            "qwen3-coder-next",
            "qwen3-coder-plus",
            "MiniMax-M2.5",
            "glm-5",
            "glm-4.7",
            "kimi-k2.5",
        ]
        .into_iter()
        .map(|model| {
            route_definition(
                "dashscope-coding-plan",
                "dashscope",
                "coding-plan",
                model,
                None,
                None,
            )
        }),
    );
    // International Coding Plan has its own six-model allowlist and key.
    // Its overview does not publish exact token limits or effort controls.
    definitions.extend(
        [
            ("qwen3.7-plus", true),
            ("qwen3.6-plus", true),
            ("qwen3.5-plus", true),
            ("kimi-k2.5", true),
            ("glm-5", false),
            ("MiniMax-M2.5", false),
        ]
        .into_iter()
        .map(|(model, image_input)| {
            BuiltInModelRouteDefinition {
            legacy_provider: provider_id("dashscope-coding-plan-intl"),
            model_ref: ModelRef::new(provider_id("dashscope"), model),
            endpoint: ProviderEndpointId::parse("coding-plan-intl").expect("valid endpoint"),
            policy: BuiltInModelRoutePolicy {
                description: Some(
                    "International Coding Plan; exact token limits and effort controls unconfirmed."
                        .into(),
                ),
                context_window_tokens: Some(None),
                auto_compact_token_limit: Some(None),
                default_max_output_tokens: Some(None),
                max_output_tokens_upper_limit: Some(None),
                capabilities: ModelCapabilityOverride {
                    image_input: Some(image_input),
                    ..ModelCapabilityOverride::default()
                },
                reasoning_effort_options: Some(Vec::new()),
                ..BuiltInModelRoutePolicy::default()
            },
        }
        }),
    );
    definitions.extend(
        [
            (
                "doubao-seed-2-0-code-preview-260215",
                Some("Doubao Seed 2.0 Code"),
            ),
            ("doubao-seed-2-0-pro-260215", None),
            ("doubao-seed-2-0-lite-260215", None),
            ("deepseek-v3-2-251201", None),
            ("glm-5.2", None),
        ]
        .into_iter()
        .map(|(model, display_name)| {
            volcengine_route_definition("volcengine-coding", "coding", model, display_name)
        }),
    );
    definitions.extend(
        [
            (
                "doubao-seed-2-0-code-preview-260215",
                Some("Doubao Seed 2.0 Code"),
            ),
            ("doubao-seed-2-0-pro-260215", None),
            ("doubao-seed-2-0-lite-260215", None),
            ("ark-code-latest", None),
            ("deepseek-v4-pro", None),
            ("deepseek-v4-flash", None),
            ("kimi-k2.7-code", None),
            ("glm-5.3", None),
            ("glm-5.2", None),
        ]
        .into_iter()
        .map(|(model, display_name)| {
            volcengine_route_definition("volcengine-agent", "plan", model, display_name)
        }),
    );
    definitions.extend(
        [
            "doubao-seed-evolving",
            "doubao-seed-2.1-pro",
            "doubao-seed-2.1-lite",
            "doubao-seed-2.0-mini",
            "minimax-m3",
            "glm-5.3-flash",
            "deepseek-v4.1-flash",
            "deepseek-v4.1-flash-economy",
            "kimi-k2.8-preview",
            "kimi-k3",
            "kimi-k3.1-pro",
            "glm-latest",
            "minimax-latest",
            "kimi-latest",
            "deepseek-latest",
        ]
        .into_iter()
        .map(|model| volcengine_route_definition("volcengine-agent", "plan", model, None)),
    );
    definitions
}

// Qianfan publishes exact output ceilings but abbreviated context lengths
// ("128k", "1M", etc.). A ceiling does not establish an output default.
// https://cloud.baidu.com/doc/qianfan/s/rmh4stp0j
fn reviewed_qianfan_model(
    model: &str,
    display_name: &str,
    output_ceiling: u32,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("qianfan"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Qianfan Chat {display_name}; exact context and output default unconfirmed."
        ),
        context_window_tokens: None,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: Some(output_ceiling),
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            supports_reasoning,
            image_input,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: Some(ProviderEndpointId::default_endpoint()),
    }
}

const REVIEWED_QIANFAN_CHAT_MODELS: &[(&str, u32, bool, bool)] = &[
    ("deepseek-v4.1-flash-1017", 393_216, true, false),
    ("deepseek-v4.1-flash", 393_216, true, false),
    ("deepseek-v4-pro-0813", 393_216, true, false),
    ("deepseek-v4-pro", 393_216, true, false),
    ("deepseek-v4-flash-0731", 393_216, true, false),
    ("ernie-5.0-thinking-latest", 65_536, true, true),
    // Listing and output ceiling alone do not confirm optional capabilities.
    ("ernie-5.0-thinking-exp", 65_536, false, false),
    ("glm-5.3-flash", 131_072, false, false),
    ("glm-5.3", 131_072, false, false),
    ("glm-5.2", 131_072, false, false),
    ("glm-5.1", 131_072, false, false),
    ("qwen3.5-397b-a17b", 65_536, true, false),
    ("qwen3.5-122b-a10b", 65_536, true, false),
    ("qwen3.5-27b", 65_536, true, false),
    ("qwen3.5-35b-a3b", 65_536, true, false),
];

pub(super) fn early_entries() -> Vec<BuiltInModelMetadata> {
    let mut entries = vec![
        catalog_model(
            "qianfan",
            "deepseek-v3.2",
            "DEEPSEEK V3.2",
            131_072,
            32_768,
            false,
            false,
        ),
        catalog_model(
            "qianfan",
            "deepseek-v3.2-think",
            "DEEPSEEK V3.2 Think",
            163_840,
            65_536,
            true,
            false,
        ),
        reviewed_qianfan_model("ernie-5.0", "ERNIE 5.0", 65_536, true, true),
        reviewed_qianfan_model(
            "ernie-5.0-thinking-preview",
            "ERNIE 5.0 Thinking Preview",
            65_536,
            true,
            true,
        ),
        reviewed_qianfan_model("ernie-5.1", "ERNIE 5.1", 65_536, false, false),
        reviewed_qianfan_model("ernie-x1.1", "ERNIE X1.1", 65_536, true, false),
        catalog_model(
            "dashscope",
            "qwen3.7-plus",
            "qwen3.7-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-plus-2026-05-26",
            "qwen3.7-plus-2026-05-26",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-max",
            "qwen3.7-max",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3.8-max",
            "qwen3.8-max",
            1_000_000,
            131_072,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.8-max-preview",
            "qwen3.8-max-preview",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.8-flash",
            "qwen3.8-flash",
            1_000_000,
            131_072,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-max-2026-06-08",
            "qwen3.7-max-2026-06-08",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3.7-max-2026-05-20",
            "qwen3.7-max-2026-05-20",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3.5-plus",
            "qwen3.5-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.6-plus",
            "qwen3.6-plus",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.6-flash",
            "qwen3.6-flash",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3.5-flash",
            "qwen3.5-flash",
            1_000_000,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "qwen3-max-2026-01-23",
            "qwen3-max-2026-01-23",
            262_144,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3-coder-next",
            "qwen3-coder-next",
            262_144,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3-coder-plus",
            "qwen3-coder-plus",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "qwen3-coder-flash",
            "qwen3-coder-flash",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "deepseek-v3.2",
            "DeepSeek V3.2",
            128_000,
            32_768,
            true,
            false,
        ),
        stepfun_model("stepfun", "step-3.7-flash", "Step 3.7 Flash", true),
        stepfun_model(
            "stepfun",
            "step-3.5-flash-2603",
            "Step 3.5 Flash 2603",
            false,
        ),
        stepfun_model("stepfun", "step-3.5-flash", "Step 3.5 Flash", false),
    ];
    entries.extend(
        REVIEWED_QIANFAN_CHAT_MODELS
            .iter()
            .map(|&(id, ceiling, reasoning, image)| {
                reviewed_qianfan_model(id, id, ceiling, reasoning, image)
            }),
    );
    entries
}

pub(super) fn late_entries() -> Vec<BuiltInModelMetadata> {
    let mut entries = vec![
        catalog_model(
            "volcengine",
            "doubao-seed-2-0-pro-260215",
            "Doubao Seed 2.0 Pro",
            256_000,
            4_096,
            true,
            true,
        ),
        catalog_model(
            "volcengine",
            "doubao-seed-2-0-code-preview-260215",
            "Doubao Seed 2.0 Code Preview",
            256_000,
            4_096,
            false,
            true,
        ),
        catalog_model(
            "volcengine-coding",
            "ark-code-latest",
            "Ark Code Latest",
            256_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            262_144,
            // Volcengine API rejects max_tokens > 32768 for kimi-k2.7-code
            32_768,
            true,
            true,
        ),
        catalog_model(
            "volcengine-coding",
            "glm-5.3",
            "GLM-5.3",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "volcengine-coding",
            "glm-5.2",
            "GLM-5.2",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "doubao-seed-2-0-mini-260215",
            "Doubao Seed 2.0 Mini",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "volcengine-agent",
            "MiniMax-M3",
            "MiniMax M3",
            1_000_000,
            8_192,
            true,
            false,
        ),
        BuiltInModelMetadata {
            model_ref: ModelRef::new(
                provider_id("volcengine"),
                "doubao-seedream-5.0-lite",
            ),
            display_name: "Doubao Seedream 5.0 Lite".into(),
            description: "Volcengine Ark Seedream image generation model for the OpenAI Images-compatible API.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                image_generation: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xiaomi"), "mimo-v2.6-pro"),
            display_name: "MiMo-V2.6-Pro".into(),
            description: "Xiaomi MiMo V2.6 Pro; token defaults follow official client configuration examples, not independently verified service hard limits.".into(),
            context_window_tokens: Some(1_048_576),
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: Some(996_147),
            default_max_output_tokens: Some(131_072),
            max_output_tokens_upper_limit: Some(131_072),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xai"), "grok-4.3"),
            display_name: "Grok 4.3".into(),
            description:
                "xAI Grok 4.3 runtime defaults aligned with the official model page.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xai"), "grok-4.5"),
            display_name: "Grok 4.5".into(),
            description:
                "xAI Grok 4.5 runtime defaults aligned with the official model page.".into(),
            context_window_tokens: Some(500_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(450_000),
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xai"), "grok-imagine-image-2.0"),
            display_name: "Grok Imagine Image 2.0".into(),
            description: "xAI Grok Imagine image generation model exposed through the OpenAI Images-compatible endpoint.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                image_generation: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xai"), "grok-imagine-image"),
            display_name: "Grok Imagine Image".into(),
            description: "xAI Grok Imagine image generation model exposed through the OpenAI Images-compatible endpoint.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                image_generation: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("xai"), "grok-imagine-image-quality"),
            display_name: "Grok Imagine Image Quality".into(),
            description: "xAI Grok Imagine image generation model exposed through the OpenAI Images-compatible endpoint.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                image_generation: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        },
        catalog_model("zai", "glm-5.3", "GLM-5.3", 1_000_000, 131_072, true, false),
        catalog_model(
            "zai",
            "glm-5.3-flash",
            "GLM-5.3 Flash",
            1_000_000,
            131_072,
            true,
            true,
        ),
        catalog_model("zai", "glm-5.2", "GLM-5.2", 1_000_000, 131_072, true, false),
        catalog_model("zai", "glm-5.1", "GLM-5.1", 202_800, 131_072, true, false),
        catalog_model("zai", "glm-5", "GLM-5", 202_800, 131_072, true, false),
        catalog_model(
            "zai",
            "glm-5-turbo",
            "GLM-5 Turbo",
            202_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-5v-turbo",
            "GLM-5V Turbo",
            202_800,
            131_072,
            true,
            true,
        ),
        catalog_model("zai", "glm-4.7", "GLM-4.7", 204_800, 131_072, true, false),
        catalog_model(
            "zai",
            "glm-4.7-flash",
            "GLM-4.7 Flash",
            200_000,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.7-flashx",
            "GLM-4.7 FlashX",
            200_000,
            128_000,
            true,
            false,
        ),
        catalog_model("zai", "glm-4.6", "GLM-4.6", 204_800, 131_072, true, false),
        catalog_model("zai", "glm-4.6v", "GLM-4.6V", 128_000, 32_768, true, true),
        catalog_model("zai", "glm-4.5", "GLM-4.5", 131_072, 98_304, true, false),
        catalog_model(
            "zai",
            "glm-4.5-air",
            "GLM-4.5 Air",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.5-x",
            "GLM-4.5 X",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.5-airx",
            "GLM-4.5 AirX",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-4.5-flash",
            "GLM-4.5 Flash",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model("zai", "glm-4.5v", "GLM-4.5V", 64_000, 16_384, true, true),
        catalog_model(
            "zai",
            "glm-4.6v-flashx",
            "GLM-4.6V FlashX",
            128_000,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "zai",
            "glm-4.6v-flash",
            "GLM-4.6V Flash",
            128_000,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "zai",
            "glm-4-32b-0414-128k",
            "GLM-4 32B 0414 128K",
            131_072,
            16_384,
            false,
            false,
        ),
    ];
    entries.extend([
        catalog_model(
            "bigmodel", "glm-5.3", "GLM-5.3", 1_000_000, 131_072, true, false,
        ),
        catalog_model(
            "bigmodel",
            "glm-5.3-flash",
            "GLM-5.3 Flash",
            1_000_000,
            131_072,
            true,
            true,
        ),
        catalog_model(
            "bigmodel", "glm-5.2", "GLM-5.2", 1_000_000, 131_072, true, false,
        ),
        catalog_model(
            "bigmodel", "glm-5.1", "GLM-5.1", 204_800, 131_072, true, false,
        ),
        catalog_model("bigmodel", "glm-5", "GLM-5", 204_800, 131_072, true, false),
        catalog_model(
            "bigmodel",
            "glm-5-turbo",
            "GLM-5 Turbo",
            204_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-5v-turbo",
            "GLM-5V Turbo",
            204_800,
            131_072,
            true,
            true,
        ),
        catalog_model(
            "bigmodel", "glm-4.7", "GLM-4.7", 204_800, 131_072, true, false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.7-flashx",
            "GLM-4.7 FlashX",
            204_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.7-flash",
            "GLM-4.7 Flash",
            204_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "bigmodel", "glm-4.6", "GLM-4.6", 204_800, 131_072, true, false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.5-air",
            "GLM-4.5 Air",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.5-airx",
            "GLM-4.5 AirX",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.5-flash",
            "GLM-4.5 Flash",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4-long",
            "GLM-4 Long",
            1_000_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4-flashx-250414",
            "GLM-4 FlashX 250414",
            131_072,
            16_384,
            false,
            false,
        ),
        catalog_model(
            "bigmodel",
            "glm-4-flash-250414",
            "GLM-4 Flash 250414",
            131_072,
            16_384,
            false,
            false,
        ),
        catalog_model(
            "bigmodel", "glm-4.6v", "GLM-4.6V", 131_072, 32_768, true, true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.6v-flash",
            "GLM-4.6V Flash",
            131_072,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.1v-thinking-flashx",
            "GLM-4.1V Thinking FlashX",
            65_536,
            16_384,
            true,
            true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4.1v-thinking-flash",
            "GLM-4.1V Thinking Flash",
            65_536,
            16_384,
            true,
            true,
        ),
        catalog_model(
            "bigmodel",
            "glm-4v-flash",
            "GLM-4V Flash",
            16_384,
            1_024,
            false,
            true,
        ),
        catalog_model(
            "dashscope",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "deepseek-v4-flash-0731",
            "DeepSeek V4 Flash 0731",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "ZHIPU/GLM-5.3",
            "ZHIPU/GLM-5.3",
            1_000_000,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "ZHIPU/GLM-5.2",
            "ZHIPU/GLM-5.2",
            1_000_000,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "glm-5.2",
            "glm-5.2",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "glm-5.1",
            "glm-5.1",
            202_752,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "kimi-k2.7-code",
            "kimi-k2.7-code",
            262_144,
            98_304,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "kimi-k2.6",
            "kimi-k2.6",
            262_144,
            98_304,
            true,
            true,
        ),
        catalog_model(
            "dashscope",
            "MiniMax/MiniMax-M3",
            "MiniMax/MiniMax-M3",
            196_608,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "MiniMax-M2.5",
            "MiniMax-M2.5",
            196_608,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "mimo-v2.5-pro",
            "MiMo V2.5 Pro",
            1_000_000,
            131_072,
            true,
            false,
        ),
        catalog_model("dashscope", "glm-5", "glm-5", 202_752, 16_384, true, false),
        catalog_model(
            "dashscope",
            "glm-4.7",
            "glm-4.7",
            202_752,
            16_384,
            true,
            false,
        ),
        catalog_model(
            "dashscope",
            "kimi-k2.5",
            "kimi-k2.5",
            262_144,
            32_768,
            true,
            true,
        ),
    ]);
    entries.extend(
        [
            ("doubao-seed-evolving", "Doubao Seed Evolving", false, false),
            ("doubao-seed-2.1-pro", "Doubao Seed 2.1 Pro", false, false),
            ("doubao-seed-2.1-lite", "Doubao Seed 2.1 Lite", false, false),
            ("doubao-seed-2.0-mini", "Doubao Seed 2.0 Mini", false, false),
            ("minimax-m3", "MiniMax M3", true, true),
            ("glm-5.3-flash", "GLM-5.3 Flash", false, true),
            ("deepseek-v4.1-flash", "DeepSeek V4.1 Flash", true, true),
            (
                "deepseek-v4.1-flash-economy",
                "DeepSeek V4.1 Flash Economy",
                true,
                true,
            ),
            ("kimi-k2.8-preview", "Kimi K2.8 Preview", true, true),
            ("kimi-k3", "Kimi K3", true, true),
            ("kimi-k3.1-pro", "Kimi K3.1 Pro", true, false),
        ]
        .into_iter()
        .map(|(model, name, reasoning, image)| {
            reviewed_subscription_model("volcengine", "coding", model, name, reasoning, image)
        }),
    );
    // Reviewed independently against BytePlus Coding Plan, never copied from
    // the mainland Volcengine catalog. API-key availability is a separate fact.
    entries.extend(
        [
            ("ark-code-latest", "Ark Code Latest", false, false),
            ("dola-seed-2.0-pro", "Dola Seed 2.0 Pro", false, false),
            ("dola-seed-2.0-lite", "Dola Seed 2.0 Lite", false, false),
            ("dola-seed-2.0-code", "Dola Seed 2.0 Code", false, true),
            ("bytedance-seed-code", "ByteDance Seed Code", false, true),
            ("glm-5.3-flash", "GLM-5.3 Flash", false, true),
            ("glm-5.2", "GLM-5.2", false, false),
            ("glm-5.1", "GLM-5.1", true, false),
            ("kimi-k2.5", "Kimi K2.5", true, true),
            ("gpt-oss-120b", "GPT OSS 120B", true, false),
            ("deepseek-v4.1-flash", "DeepSeek V4.1 Flash", true, true),
            (
                "deepseek-v4-flash",
                "DeepSeek V4 Flash Preview",
                true,
                false,
            ),
            ("deepseek-v4-pro", "DeepSeek V4 Pro Preview", true, false),
        ]
        .into_iter()
        .map(|(model, name, reasoning, image)| {
            reviewed_subscription_model("byteplus", "coding", model, name, reasoning, image)
        }),
    );
    // Both Coding and Agent Plan independently document these aliases.
    // The subscription routers below do not inherit underlying capabilities.
    entries.extend(
        [
            ("minimax-latest", "MiniMax Latest"),
            ("kimi-latest", "Kimi Latest"),
            ("deepseek-latest", "DeepSeek Latest"),
        ]
        .into_iter()
        .map(|(model, name)| {
            reviewed_subscription_model("volcengine", "coding", model, name, false, false)
        }),
    );
    entries.extend([
        reviewed_subscription_model(
            "volcengine",
            "coding",
            "glm-latest",
            "GLM-5.3 Latest",
            true,
            false,
        ),
        reviewed_subscription_model(
            "dashscope",
            "token-plan",
            "auto",
            "Token Plan Auto",
            false,
            false,
        ),
        BuiltInModelMetadata {
            context_window_tokens: Some(1_000_000),
            max_output_tokens_upper_limit: Some(384_000),
            description: "DashScope Beijing Token Plan DeepSeek V4.1 Flash 1017; \
                reviewed advisory limits from models.dev alibaba-token-plan-cn, \
                not a default output policy or other region's guarantee."
                .into(),
            ..reviewed_subscription_model(
                "dashscope",
                "token-plan",
                "deepseek-v4.1-flash-1017",
                "DeepSeek V4.1 Flash 1017",
                true,
                true,
            )
        },
        reviewed_subscription_model(
            "stepfun",
            "plan",
            "step-router-v1",
            "Step Router V1",
            false,
            false,
        ),
    ]);
    // Messages eligibility and the three model cards independently confirm
    // these limits and modalities, but not subscription eligibility or effort.
    entries.extend(
        [
            ("qwen3.8-2.4t-a95b", false),
            ("qwen3.8-27b", true),
            ("qwen3.8-max-0902", true),
        ]
        .into_iter()
        .map(|(model, image)| {
            let mut entry =
                catalog_model("dashscope", model, model, 1_000_000, 131_072, true, image);
            entry.endpoint = Some(ProviderEndpointId::parse("default").unwrap());
            entry
        }),
    );
    // Beijing model cards reviewed on 2026-10-25; capacity is not a default
    // generation policy, capability assertion, or subscription entitlement.
    entries.extend(
        [
            ("qwen-plus-latest", 32_768),
            ("qwen-flash-2025-07-28", 32_768),
            ("qwen3.5-flash-2026-02-23", 65_536),
            ("qwen3.5-plus-2026-02-15", 65_536),
            ("qwen3.5-plus-2026-04-20", 65_536),
            ("qwen3.6-flash-2026-04-16", 65_536),
            ("qwen3.6-plus-2026-04-02", 65_536),
            ("qwen3.7-flash-2026-07-15", 131_072),
        ]
        .into_iter()
        .map(|(model, output)| {
            let mut entry =
                reviewed_subscription_model("dashscope", "default", model, model, false, false);
            entry.context_window_tokens = Some(1_000_000);
            entry.max_output_tokens_upper_limit = Some(output);
            entry.description = format!(
                "DashScope Beijing default Anthropic-compatible API {model}; \
                 token capacity reviewed 2026-10-25, model capabilities unconfirmed."
            );
            entry
        }),
    );
    // Endpoint eligibility is reviewed against the official Token Plan list.
    // Limits and tool support come from models.dev's alibaba-token-plan-cn,
    // not from the underlying vendors' API catalogs or official hard limits.
    entries.extend(
        [
            ("glm-5.3", "GLM-5.3", 131_072, false),
            ("deepseek-v4.1-flash", "DeepSeek V4.1 Flash", 384_000, true),
            (
                "deepseek-v4-pro-0813",
                "DeepSeek V4 Pro 0813",
                384_000,
                false,
            ),
        ]
        .into_iter()
        .map(|(model, name, output, image)| {
            let mut entry = catalog_model("dashscope", model, name, 1_000_000, output, true, image);
            entry.endpoint = Some(ProviderEndpointId::parse("token-plan").expect("valid endpoint"));
            entry.description = format!(
                "DashScope Token Plan {name}; limits and tool support from models.dev \
                 alibaba-token-plan-cn, not confirmed official endpoint limits."
            );
            entry
        }),
    );
    // API EOS does not revoke separately reviewed Coding/Agent Plan access.
    // Keep the shared metadata, but no longer register a default API route.
    // Append after existing candidates to preserve subscription defaults.
    entries.extend(
        [
            ("deepseek-v3-2-251201", "DeepSeek V3.2", 128_000),
            (
                "doubao-seed-2-0-lite-260215",
                "Doubao Seed 2.0 Lite",
                256_000,
            ),
        ]
        .into_iter()
        .map(|(model, name, context)| {
            let mut entry = catalog_model("volcengine", model, name, context, 4_096, false, false);
            entry.endpoint = Some(ProviderEndpointId::parse("coding").expect("valid endpoint"));
            entry
        }),
    );
    // Native Responses IDs, independently reviewed against model-list.md and
    // responses-api-deep-thinking.md. Do not project dotted subscription IDs
    // or K3.1's response-only internal version ID into direct API eligibility.
    // Append so new API models cannot displace existing preferred routes.
    entries.extend(
        [
            ("doubao-seed-2-1-pro-260915", "Doubao Seed 2.1 Pro 260915"),
            ("doubao-seed-2-1-lite-260915", "Doubao Seed 2.1 Lite 260915"),
            ("deepseek-v4-1-flash-261017", "DeepSeek V4.1 Flash 261017"),
            ("deepseek-v4-1-flash-260910", "DeepSeek V4.1 Flash 260910"),
            (
                "deepseek-v4-1-flash-economy-261017",
                "DeepSeek V4.1 Flash Economy 261017",
            ),
            ("kimi-k3-1-pro", "Kimi K3.1 Pro"),
        ]
        .into_iter()
        .map(|(model, name)| BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("volcengine"), model),
            display_name: name.into(),
            description: format!(
                "Volcengine native Responses API {name}; abbreviated token limits \
                 remain unknown; K3.1 output limits conflict across official pages."
            ),
            // Only K3.1 has an exact numeric context specification. A "k"
            // abbreviation is not evidence of an exact integer token limit.
            context_window_tokens: (model == "kimi-k3-1-pro").then_some(1_048_576),
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            ),
            capabilities: ModelCapabilityFlags {
                supports_reasoning: true,
                image_input: true,
                ..ModelCapabilityFlags::default()
            },
            // Advertise distinct native levels, not the compatibility aliases
            // that Responses maps onto them.
            reasoning_effort_options: match model {
                "doubao-seed-2-1-pro-260915" | "doubao-seed-2-1-lite-260915" => {
                    &["minimal", "low", "medium", "high"][..]
                }
                "kimi-k3-1-pro" => &["low", "high", "max"][..],
                _ => &["none", "low", "high", "max"][..],
            }
            .iter()
            .map(|level| (*level).to_owned())
            .collect(),
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        }),
    );
    entries
}

#[test]
fn volcengine_api_eos_keeps_independently_reviewed_subscription_routes() {
    let catalog = BuiltInModelCatalog::new();
    for model in [
        "deepseek-v3-2-251201",
        "doubao-seed-1-8-251228",
        "doubao-seed-1-6-251015",
        "doubao-seed-1-6-flash-250828",
        "doubao-seed-1-6-vision-250815",
        "doubao-seed-2-0-lite-260215",
        "doubao-seed-2-0-mini-260215",
    ] {
        let api = ModelRouteRef::parse(&format!("volcengine@default/{model}")).unwrap();
        assert!(catalog.get_route(&api).is_none(), "{}", api.as_string());
    }
    for (endpoint, model) in [
        ("coding", "deepseek-v3-2-251201"),
        ("coding", "doubao-seed-2-0-lite-260215"),
        ("plan", "doubao-seed-2-0-lite-260215"),
        ("plan", "doubao-seed-2-0-mini-260215"),
        // EOS for these API versions is not until 2026-11-24 (UTC+8).
        ("default", "doubao-seed-2-0-pro-260215"),
        ("default", "doubao-seed-2-0-code-preview-260215"),
    ] {
        let route = ModelRouteRef::parse(&format!("volcengine@{endpoint}/{model}")).unwrap();
        assert!(catalog.get_route(&route).is_some(), "{}", route.as_string());
    }
    let lite = ModelRef::new(provider_id("volcengine"), "doubao-seed-2-0-lite-260215");
    let entries = late_entries();
    let metadata = entries
        .iter()
        .find(|entry| entry.model_ref == lite)
        .unwrap();
    assert!(!metadata.capabilities.image_input);
    for endpoint in ["coding", "plan"] {
        let route = ModelRouteRef::parse(&format!("volcengine@{endpoint}/{}", lite.model)).unwrap();
        let policy = catalog.resolve_route_policy(
            &route,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &ContextConfig::default(),
            8_192,
        );
        assert!(!policy.capabilities.image_input);
    }
}

#[test]
fn volcengine_native_responses_ids_keep_protocol_and_unknown_limits_scoped() {
    let entries = late_entries();
    let catalog = BuiltInModelCatalog::new();
    for model in [
        "doubao-seed-2-1-pro-260915",
        "doubao-seed-2-1-lite-260915",
        "deepseek-v4-1-flash-261017",
        "deepseek-v4-1-flash-260910",
        "deepseek-v4-1-flash-economy-261017",
        "kimi-k3-1-pro",
    ] {
        let metadata = entries
            .iter()
            .find(|entry| entry.model_ref == ModelRef::new(provider_id("volcengine"), model))
            .unwrap();
        assert_eq!(metadata.endpoint.as_ref().unwrap().as_str(), "default");
        assert_eq!(
            metadata.context_window_tokens,
            (model == "kimi-k3-1-pro").then_some(1_048_576)
        );
        assert_eq!(metadata.auto_compact_token_limit, None);
        assert_eq!(metadata.default_max_output_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, None);
        assert!(metadata.capabilities.supports_reasoning);
        assert!(metadata.capabilities.image_input);
        let efforts: &[&str] = if model.starts_with("doubao-") {
            &["minimal", "low", "medium", "high"]
        } else if model == "kimi-k3-1-pro" {
            &["low", "high", "max"]
        } else {
            &["none", "low", "high", "max"]
        };
        assert_eq!(metadata.reasoning_effort_options, efforts);
        let api = ModelRouteRef::parse(&format!("volcengine@default/{model}")).unwrap();
        // Responses caching is not evidence for Anthropic cache_control.
        assert!(catalog
            .get_route(&api)
            .unwrap()
            .cache_capabilities
            .is_none());
        for endpoint in ["coding", "plan", "messages", "responses"] {
            let other = ModelRouteRef::parse(&format!("volcengine@{endpoint}/{model}")).unwrap();
            assert!(catalog.get_route(&other).is_none(), "{}", other.as_string());
        }
    }
    // The dotted spelling is a separately reviewed subscription ID. The
    // dated K3.1 version is response-only, not a direct invocation model ID.
    for model in ["kimi-k3.1-pro", "kimi-k3-1-pro-261024"] {
        let api = ModelRouteRef::parse(&format!("volcengine@default/{model}")).unwrap();
        assert!(catalog.get_route(&api).is_none());
    }
}

#[test]
fn volcengine_native_additions_do_not_displace_surviving_preferences() {
    let catalog = BuiltInModelCatalog::new();
    for (provider, route) in [
        // The former default (Seed 1.8) is retired; retain the next existing
        // candidate, never prefer one of the newly appended API models.
        (
            "volcengine",
            "volcengine@default/doubao-seed-2-0-pro-260215",
        ),
        ("volcengine-coding", "volcengine@coding/ark-code-latest"),
        (
            "volcengine-agent",
            "volcengine@plan/doubao-seed-2-0-mini-260215",
        ),
    ] {
        assert_eq!(
            catalog.preferred_route_for_provider(&provider_id(provider)),
            Some(ModelRouteRef::parse(route).unwrap())
        );
    }
    for (model, endpoint) in [
        ("ark-code-latest", "coding"),
        ("deepseek-v4-pro", "coding"),
        ("kimi-k3.1-pro", "coding"),
        ("doubao-seed-2-0-code-preview-260215", "default"),
    ] {
        assert_eq!(
            catalog.preferred_route_for_model(&ModelRef::new(provider_id("volcengine"), model)),
            Some(ModelRouteRef::parse(&format!("volcengine@{endpoint}/{model}")).unwrap())
        );
    }
}

#[test]
fn volcengine_reviewed_subscription_models_do_not_imply_default_api_routes() {
    let catalog = BuiltInModelCatalog::new();
    for model in [
        "doubao-seed-evolving",
        "doubao-seed-2.1-pro",
        "doubao-seed-2.1-lite",
        "doubao-seed-2.0-mini",
        "minimax-m3",
        "glm-5.3-flash",
        "deepseek-v4.1-flash",
        "kimi-k2.8-preview",
        "kimi-k3",
        "kimi-k3.1-pro",
    ] {
        for endpoint in ["coding", "plan"] {
            let route = ModelRouteRef::parse(&format!("volcengine@{endpoint}/{model}")).unwrap();
            assert!(catalog.get_route(&route).is_some(), "{}", route.as_string());
        }
        let api = ModelRouteRef::parse(&format!("volcengine@default/{model}")).unwrap();
        assert!(catalog.get_route(&api).is_none(), "{}", api.as_string());
        let metadata = catalog
            .get(&ModelRef::parse(&format!("volcengine/{model}")).unwrap())
            .unwrap();
        assert_eq!(metadata.context_window_tokens, None);
        assert_eq!(metadata.max_output_tokens_upper_limit, None);
    }
    for endpoint in ["coding", "plan"] {
        let auto = ModelRouteRef::parse(&format!("volcengine@{endpoint}/Auto")).unwrap();
        assert!(catalog.get_route(&auto).is_none());
    }
}

#[test]
fn reviewed_byteplus_vision_and_volcengine_alias_are_endpoint_scoped() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        for model in ["dola-seed-2.0-code", "bytedance-seed-code"] {
            let route = ModelRouteRef::parse(&format!("byteplus@coding/{model}")).unwrap();
            assert!(catalog.get_route(&route).is_some());
            let metadata = catalog.get(&route.model_ref()).unwrap();
            assert!(metadata.capabilities.image_input);
            assert_eq!(metadata.context_window_tokens, None);
            assert_eq!(metadata.max_output_tokens_upper_limit, None);
        }
        for endpoint in ["coding", "plan"] {
            let alias = ModelRouteRef::parse(&format!("volcengine@{endpoint}/glm-latest")).unwrap();
            assert!(catalog.get_route(&alias).is_some());
            let metadata = catalog.get(&alias.model_ref()).unwrap();
            assert!(metadata.capabilities.supports_reasoning);
            assert_eq!(metadata.context_window_tokens, None);
            assert_eq!(metadata.max_output_tokens_upper_limit, None);
        }
        for unsupported in [
            "volcengine@default/glm-latest",
            "byteplus@coding/glm-latest",
        ] {
            let route = ModelRouteRef::parse(unsupported).unwrap();
            assert!(catalog.get_route(&route).is_none(), "{unsupported}");
        }
    }
}

#[test]
fn subscription_routers_do_not_inherit_underlying_model_capabilities() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        for (provider, endpoint, model) in [
            ("dashscope", "token-plan", "auto"),
            ("stepfun", "plan", "step-router-v1"),
            ("volcengine", "coding", "minimax-latest"),
            ("volcengine", "plan", "minimax-latest"),
            ("volcengine", "coding", "kimi-latest"),
            ("volcengine", "plan", "kimi-latest"),
            ("volcengine", "coding", "deepseek-latest"),
            ("volcengine", "plan", "deepseek-latest"),
        ] {
            let route = ModelRouteRef::parse(&format!("{provider}@{endpoint}/{model}")).unwrap();
            assert!(catalog.get_route(&route).is_some(), "{}", route.as_string());
            let metadata = catalog.get(&route.model_ref()).unwrap();
            assert_eq!(metadata.context_window_tokens, None);
            assert_eq!(metadata.default_max_output_tokens, None);
            assert_eq!(metadata.max_output_tokens_upper_limit, None);
            assert!(!metadata.capabilities.image_input);
            assert!(!metadata.capabilities.supports_reasoning);
            assert!(metadata.reasoning_effort_options.is_empty());
            let api = ModelRouteRef::parse(&format!("{provider}@default/{model}")).unwrap();
            assert!(catalog.get_route(&api).is_none(), "{}", api.as_string());
        }
    }
}

#[test]
fn volcengine_retired_k26_does_not_remove_compatibility_or_future_retirements() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        for endpoint in ["coding", "plan"] {
            let retired =
                ModelRouteRef::parse(&format!("volcengine@{endpoint}/kimi-k2.6")).unwrap();
            assert!(catalog.get_route(&retired).is_none());
            for model in ["glm-5.2", "kimi-k2.7-code", "minimax-m3"] {
                let route =
                    ModelRouteRef::parse(&format!("volcengine@{endpoint}/{model}")).unwrap();
                assert!(catalog.get_route(&route).is_some(), "{}", route.as_string());
            }
            let compatibility =
                ModelRouteRef::parse(&format!("volcengine@{endpoint}/glm-5.2")).unwrap();
            assert!(catalog
                .get_route(&compatibility)
                .unwrap()
                .description
                .as_deref()
                .unwrap()
                .contains("forwards to GLM-5.3"));
        }
        for peer in [
            "dashscope@token-plan/kimi-k2.6",
            "opencode-go@default/kimi-k2.6",
        ] {
            assert!(catalog
                .get_route(&ModelRouteRef::parse(peer).unwrap())
                .is_some());
        }
    }
}

#[test]
fn international_coding_plan_has_its_own_allowlist_and_unknown_limits() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        let routes = catalog
            .list_routes()
            .into_iter()
            .filter(|route| {
                route.provider.as_str() == "dashscope"
                    && route.endpoint.as_str() == "coding-plan-intl"
            })
            .collect::<Vec<_>>();
        assert_eq!(routes.len(), 6);
        for (model, image_input) in [
            ("qwen3.7-plus", true),
            ("qwen3.6-plus", true),
            ("qwen3.5-plus", true),
            ("kimi-k2.5", true),
            ("glm-5", false),
            ("MiniMax-M2.5", false),
        ] {
            let route =
                ModelRouteRef::parse(&format!("dashscope@coding-plan-intl/{model}")).unwrap();
            let overrides = catalog.get_route(&route).unwrap();
            assert_eq!(overrides.context_window_tokens, Some(None));
            assert_eq!(overrides.default_max_output_tokens, Some(None));
            assert_eq!(overrides.max_output_tokens_upper_limit, Some(None));
            let policy = catalog.resolve_route_policy(
                &route,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert!(policy.context_window_tokens.is_none());
            assert!(policy.max_output_tokens_upper_limit.is_none());
            assert_eq!(policy.capabilities.image_input, image_input);
            assert!(policy.reasoning_effort_options.is_empty());
        }
        for unsupported in ["qwen3-coder-plus", "qwen3.8-max", "auto", "glm-5.3"] {
            let route =
                ModelRouteRef::parse(&format!("dashscope@coding-plan-intl/{unsupported}")).unwrap();
            assert!(catalog.get_route(&route).is_none());
        }
    }
}

#[test]
fn reviewed_subscription_capabilities_do_not_leak_between_endpoints() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        for model in ["glm-5.1", "kimi-k2.5", "gpt-oss-120b"] {
            let route = ModelRouteRef::parse(&format!("byteplus@coding/{model}")).unwrap();
            assert!(catalog.get_route(&route).is_some());
            assert!(
                catalog
                    .get(&route.model_ref())
                    .unwrap()
                    .capabilities
                    .supports_reasoning
            );
            let api = ModelRouteRef::parse(&format!("byteplus@default/{model}")).unwrap();
            assert!(catalog.get_route(&api).is_none());
        }
        for (endpoint, image_input) in [("coding", true), ("plan", false)] {
            let policy = catalog.resolve_route_policy(
                &ModelRouteRef::parse(&format!("volcengine@{endpoint}/kimi-k2.7-code")).unwrap(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(policy.capabilities.image_input, image_input);
        }
    }
}

#[test]
fn reviewed_kimi_k31_subscription_metadata_does_not_inherit_k3_tier_or_api_limits() {
    let entries = late_entries();
    let metadata = entries
        .iter()
        .find(|entry| entry.model_ref.as_string() == "volcengine/kimi-k3.1-pro")
        .unwrap();
    assert_eq!(metadata.endpoint.as_ref().unwrap().as_str(), "coding");
    assert!(metadata.capabilities.supports_reasoning);
    assert!(!metadata.capabilities.image_input);
    assert_eq!(metadata.context_window_tokens, None);
    assert_eq!(metadata.default_max_output_tokens, None);
    assert_eq!(metadata.max_output_tokens_upper_limit, None);
    assert!(metadata.reasoning_effort_options.is_empty());
    assert!(!metadata.description.contains("Medium/Large/Max"));
    let definitions = route_definitions();
    let plan = definitions
        .iter()
        .find(|definition| {
            definition.model_ref == metadata.model_ref && definition.endpoint.as_str() == "plan"
        })
        .unwrap();
    assert!(plan.policy.description.is_none());
}

#[test]
fn qianfan_reviewed_chat_ids_keep_context_and_output_defaults_unknown() {
    let entries = early_entries();
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    let groups: &[(&[&str], u32)] = &[
        (
            &[
                "deepseek-v4.1-flash-1017",
                "deepseek-v4.1-flash",
                "deepseek-v4-pro-0813",
                "deepseek-v4-pro",
                "deepseek-v4-flash-0731",
            ],
            393_216,
        ),
        (
            &["ernie-5.0-thinking-latest", "ernie-5.0-thinking-exp"],
            65_536,
        ),
        (&["glm-5.3-flash", "glm-5.3", "glm-5.2", "glm-5.1"], 131_072),
        (
            &[
                "qwen3.5-397b-a17b",
                "qwen3.5-122b-a10b",
                "qwen3.5-27b",
                "qwen3.5-35b-a3b",
            ],
            65_536,
        ),
    ];
    assert_eq!(REVIEWED_QIANFAN_CHAT_MODELS.len(), 15);
    for &(ids, ceiling) in groups {
        for id in ids {
            let model_ref = ModelRef::new(provider_id("qianfan"), *id);
            let metadata = entries
                .iter()
                .find(|entry| entry.model_ref == model_ref)
                .expect("reviewed Qianfan Chat ID");
            assert_eq!(metadata.context_window_tokens, None, "{id}");
            assert_eq!(metadata.auto_compact_token_limit, None, "{id}");
            assert_eq!(metadata.default_max_output_tokens, None, "{id}");
            assert_eq!(
                metadata.max_output_tokens_upper_limit,
                Some(ceiling),
                "{id}"
            );
            assert!(metadata.reasoning_effort_options.is_empty(), "{id}");
            let chat = ModelRouteRef::new(
                model_ref.provider,
                ProviderEndpointId::default_endpoint(),
                model_ref.model,
            );
            assert!(catalog.get_route(&chat).is_some(), "{id}");
            let policy = catalog.resolve_route_policy(
                &chat,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(policy.context_window_tokens, None, "{id}");
            assert_eq!(policy.runtime_max_output_tokens, 8_192, "{id}");
            assert_eq!(policy.max_output_tokens_upper_limit, Some(ceiling), "{id}");
            for endpoint in ["coding", "plan", "messages", "responses"] {
                let route = ModelRouteRef::parse(&format!("qianfan@{endpoint}/{id}")).unwrap();
                assert!(catalog.get_route(&route).is_none(), "{id}@{endpoint}");
            }
        }
    }
}

#[test]
fn qianfan_ernie_abbreviated_context_does_not_keep_conflicting_exact_limits() {
    let entries = early_entries();
    let catalog = BuiltInModelCatalog::from_legacy_definitions();
    for id in [
        "ernie-5.0",
        "ernie-5.0-thinking-preview",
        "ernie-5.1",
        "ernie-x1.1",
    ] {
        let model_ref = ModelRef::new(provider_id("qianfan"), id);
        let metadata = entries
            .iter()
            .find(|entry| entry.model_ref == model_ref)
            .unwrap();
        assert_eq!(metadata.context_window_tokens, None, "{id}");
        assert_eq!(metadata.auto_compact_token_limit, None, "{id}");
        assert_eq!(metadata.default_max_output_tokens, None, "{id}");
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(65_536), "{id}");
        if id == "ernie-5.0" {
            assert!(metadata.capabilities.supports_reasoning);
        }
        let route = ModelRouteRef::new(
            model_ref.provider,
            ProviderEndpointId::default_endpoint(),
            model_ref.model,
        );
        assert!(catalog.get_route(&route).is_some(), "{id}");
    }
    for id in ["ernie-x1.1", "deepseek-v3.2", "deepseek-v3.2-think"] {
        let route = ModelRouteRef::parse(&format!("qianfan@default/{id}")).unwrap();
        assert!(catalog.get_route(&route).is_some(), "{id}");
    }
}

#[test]
fn reviewed_dashscope_anthropic_ids_are_default_only_and_conservative() {
    let entries = late_entries();
    for model in [
        "qwen3.8-2.4t-a95b",
        "qwen3.8-27b",
        "qwen3.8-max-0902",
        "qwen-plus-latest",
        "qwen-flash-2025-07-28",
        "qwen3.5-flash-2026-02-23",
        "qwen3.5-plus-2026-02-15",
        "qwen3.5-plus-2026-04-20",
        "qwen3.6-flash-2026-04-16",
        "qwen3.6-plus-2026-04-02",
        "qwen3.7-flash-2026-07-15",
    ] {
        let model_ref = ModelRef::new(provider_id("dashscope"), model);
        let metadata = entries
            .iter()
            .find(|entry| entry.model_ref == model_ref)
            .unwrap();
        assert_eq!(metadata.endpoint.as_ref().unwrap().as_str(), "default");
        let has_model_card = model.starts_with("qwen3.8-");
        assert_eq!(metadata.context_window_tokens, Some(1_000_000));
        assert_eq!(
            metadata.default_max_output_tokens,
            has_model_card.then_some(131_072)
        );
        assert_eq!(
            metadata.max_output_tokens_upper_limit,
            Some(match model {
                "qwen-plus-latest" | "qwen-flash-2025-07-28" => 32_768,
                "qwen3.7-flash-2026-07-15" => 131_072,
                _ if has_model_card => 131_072,
                _ => 65_536,
            })
        );
        assert!(metadata.reasoning_effort_options.is_empty());
        assert_eq!(metadata.capabilities.supports_reasoning, has_model_card);
        assert_eq!(
            metadata.capabilities.image_input,
            matches!(model, "qwen3.8-27b" | "qwen3.8-max-0902")
        );
        for catalog in [
            legacy_builtin_catalog().unwrap(),
            BuiltInModelCatalog::new(),
        ] {
            let route = ModelRouteRef::parse(&format!("dashscope@default/{model}")).unwrap();
            assert!(catalog.get_route(&route).is_some(), "{}", route.as_string());
            for endpoint in ["token-plan", "coding-plan", "coding-plan-intl"] {
                let route = ModelRouteRef::parse(&format!("dashscope@{endpoint}/{model}")).unwrap();
                assert!(catalog.get_route(&route).is_none(), "{}", route.as_string());
            }
        }
    }
}
