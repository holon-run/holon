use super::super::*;

pub(super) fn opencode_go_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: Option<u32>,
    supports_reasoning: bool,
    image_input: bool,
    endpoint: Option<&str>,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("opencode-go"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the OpenCode Go {model} route."
        ),
        context_window_tokens: Some(context_window_tokens),
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
        endpoint: endpoint.map(|endpoint| {
            ProviderEndpointId::parse(endpoint).expect("valid OpenCode Go endpoint id")
        }),
    }
}

pub(super) fn kilocode_auto_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: u32,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("kilocode"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Kilo Gateway {display_name} virtual model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: Some(max_output_tokens),
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

pub(super) fn kilocode_entries() -> Vec<BuiltInModelMetadata> {
    vec![
        kilocode_auto_model(
            "kilo-auto/frontier",
            "Kilo Auto Frontier",
            1_000_000,
            128_000,
            true,
        ),
        kilocode_auto_model(
            "kilo-auto/balanced",
            "Kilo Auto Balanced",
            1_000_000,
            65_536,
            true,
        ),
        kilocode_auto_model(
            "kilo-auto/efficient",
            "Kilo Auto Efficient",
            1_000_000,
            65_536,
            true,
        ),
        kilocode_auto_model("kilo-auto/free", "Kilo Auto Free", 256_000, 10_000, false),
    ]
}

pub(super) fn opencode_entries() -> Vec<BuiltInModelMetadata> {
    vec![
        opencode_go_model(
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            Some(384_000),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            Some(384_000),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "glm-5.2",
            "GLM-5.2",
            1_000_000,
            Some(131_072),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "glm-5.1",
            "GLM-5.1",
            202_800,
            Some(131_072),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            262_144,
            Some(262_144),
            true,
            true,
            None,
        ),
        opencode_go_model(
            "kimi-k2.6",
            "Kimi K2.6",
            262_144,
            Some(262_144),
            true,
            true,
            None,
        ),
        opencode_go_model(
            "mimo-v2.5-pro",
            "MiMo V2.5 Pro",
            1_048_576,
            Some(131_072),
            true,
            false,
            None,
        ),
        opencode_go_model(
            "mimo-v2.5",
            "MiMo V2.5",
            1_048_576,
            Some(131_072),
            true,
            true,
            None,
        ),
        opencode_go_model(
            "minimax-m3",
            "MiniMax M3",
            1_000_000,
            None,
            true,
            true,
            Some("messages"),
        ),
        opencode_go_model(
            "minimax-m2.7",
            "MiniMax M2.7",
            204_800,
            None,
            true,
            false,
            Some("messages"),
        ),
        opencode_go_model(
            "minimax-m2.5",
            "MiniMax M2.5",
            196_608,
            Some(32_768),
            true,
            false,
            Some("messages"),
        ),
        opencode_go_model(
            "qwen3.7-max",
            "Qwen3.7 Max",
            1_000_000,
            Some(65_536),
            true,
            false,
            Some("messages"),
        ),
        opencode_go_model(
            "qwen3.7-plus",
            "Qwen3.7 Plus",
            1_000_000,
            Some(65_536),
            true,
            true,
            Some("messages"),
        ),
        opencode_go_model(
            "qwen3.6-plus",
            "Qwen3.6 Plus",
            1_000_000,
            Some(65_536),
            true,
            true,
            Some("messages"),
        ),
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("openrouter"), "auto"),
            display_name: "OpenRouter Auto".into(),
            description: "OpenRouter Auto Router metadata aligned with the official Models API; the selected upstream model and provider are dynamic.".into(),
            context_window_tokens: Some(2_000_000),
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
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
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: None,
        },
    ]
}
