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
        kilocode_auto_model("kilo-auto/free", "Kilo Auto Free", 256_000, 32_768, false),
        kilocode_auto_model("kilo-auto/small", "Kilo Auto Small", 262_144, 32_768, true),
    ]
}

fn opencode_go_models_dev_entries() -> Vec<BuiltInModelMetadata> {
    // Preferences follow the official native Go table; specifications are from
    // models.dev's opencode-go entry only, never inherited from upstream vendors.
    // Grok 4.6 lacks Go-specific specifications; translated routes are separate.
    [
        ("grok-4.7", "Grok 4.7", 500_000, 500_000, true, Some("responses")),
        ("gpt-6-luna", "GPT-6 Luna", 1_050_000, 128_000, true, Some("responses")),
        ("gpt-5.6-luna", "GPT-5.6 Luna", 1_050_000, 128_000, true, Some("responses")),
        ("claude-haiku-5-5", "Claude Haiku 5.5", 1_000_000, 128_000, true, Some("messages")),
        ("glm-5.3-flash", "GLM-5.3-Flash", 1_000_000, 131_072, true, None),
        ("glm-5.3", "GLM-5.3", 1_000_000, 131_072, false, None),
        ("kimi-k3", "Kimi K3", 1_048_576, 131_072, true, None),
        ("longcat-2.0", "LongCat-2.0", 1_000_000, 131_072, false, None),
        ("deepseek-v4.1-flash", "DeepSeek V4.1 Flash", 1_000_000, 384_000, true, None),
        ("mimo-v2.6-flash", "MiMo-V2.6-Flash", 1_048_576, 131_072, true, None),
        ("mimo-v2.6-pro", "MiMo-V2.6-Pro", 1_048_576, 131_072, true, None),
        ("muse-spark-1.3-contributor", "Muse Spark 1.3 Contributor", 1_048_576, 131_072, true, Some("responses")),
        ("muse-spark-1.2-contributor", "Muse Spark 1.2 Contributor", 1_048_576, 131_072, true, Some("responses")),
        ("qwen3.8-max", "Qwen3.8 Max", 1_000_000, 131_072, true, Some("messages")),
        ("qwen3.8-flash", "Qwen3.8 Flash", 1_000_000, 131_072, true, Some("messages")),
        ("hy4-preview", "Hy4 preview", 1_024_000, 64_000, false, None),
        ("hy3", "Hy3", 256_000, 128_000, false, None),
        ("space-bunny", "Space Bunny", 1_048_576, 524_288, true, None),
        ("longcat-2.5-preview-free", "LongCat 2.5 Preview Free", 1_000_000, 131_072, true, None),
        ("step-5-preview-free", "Step 5 Preview Free", 1_000_000, 65_536, true, None),
        ("cat-jam-free", "Cat Jam Free", 262_144, 131_072, false, None),
        ("step-5-preview", "Step 5 Preview", 1_000_000, 65_536, true, None),
        ("beam-free", "Beam Free", 1_048_576, 131_072, false, None),
        ("minimax-m3.1-flash", "MiniMax M3.1 Flash", 1_000_000, 512_000, true, None),
        ("minimax-m3.1-flash-free", "MiniMax M3.1 Flash Free", 1_000_000, 512_000, true, None),
    ]
    .into_iter()
    .map(|(id, name, context, output, image_input, endpoint)| {
        let mut model = opencode_go_model(
            id, name, context, Some(output), true, image_input, endpoint,
        );
        model.description = format!(
            "OpenCode Go {id} route from the official endpoint table. Advisory capabilities and token limits from models.dev's opencode-go entry; not independently verified service hard limits."
        );
        model
    })
    .collect()
}

pub(super) fn opencode_entries() -> Vec<BuiltInModelMetadata> {
    let mut entries = vec![
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
    ];
    entries.push(BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id("opencode-go"), "kimi-k3.1-pro"),
        display_name: "Kimi K3.1 Pro".into(),
        description: "OpenCode Go and Go Plus native Chat route; capacity, modalities, and reasoning controls follow the official Go model listing.".into(),
        context_window_tokens: Some(1_048_576),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
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
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    });
    entries.push(BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id("opencode-go"), "iceman-contributor-free"),
        display_name: "Iceman Contributor Free".into(),
        description: "OpenCode Go native Chat limited-time free route; prompts and completions may be used for training (not ZDR). Capacity and reasoning support follow the official Go model listing; effort controls remain unconfirmed.".into(),
        context_window_tokens: Some(262_144),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: Some(131_072),
        default_verbosity: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    });
    entries.push(BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id("opencode-go"), "gemini-4-argon"),
        display_name: "Gemini 4 Argon".into(),
        description: "OpenCode Go translated text route; the native Google API is not registered. Token limits, vision, and reasoning controls remain unconfirmed.".into(),
        context_window_tokens: None,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        default_verbosity: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags::default(),
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    });
    entries.extend(opencode_go_models_dev_entries());
    for entry in &mut entries {
        if entry.model_ref.provider != provider_id("opencode-go") {
            continue;
        }
        let id = entry.model_ref.model.as_str();
        if id == "minimax-m3" {
            // The official ceiling is not a service default.
            entry.max_output_tokens_upper_limit = Some(131_072);
        }
        let efforts: &[&str] = match id {
            "deepseek-v4.1-flash" | "glm-5.3" | "glm-5.3-flash" | "kimi-k3.1-pro" => {
                &["low", "high", "max"]
            }
            "deepseek-v4-pro" | "glm-5.2" => &["high", "max"],
            "hy3" => &["none", "low", "high"],
            "hy4-preview" => &["none", "high"],
            "kimi-k3" => &["max"],
            "minimax-m3.1-flash" | "beam-free" | "claude-haiku-5-5" => {
                &["low", "medium", "high", "xhigh", "max"]
            }
            "step-5-preview" => &["low", "medium", "high"],
            "qwen3.8-max" | "qwen3.8-flash" => &["low", "medium", "xhigh"],
            "grok-4.7" => &["low", "medium", "high", "xhigh"],
            "muse-spark-1.2-contributor" | "muse-spark-1.3-contributor" => {
                &["minimal", "low", "medium", "high", "xhigh"]
            }
            "gpt-5.6-luna" | "gpt-6-luna" => &["none", "low", "medium", "high", "xhigh", "max"],
            _ => &[],
        };
        entry.reasoning_effort_options = efforts.iter().map(|effort| (*effort).into()).collect();
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_go_native_metadata_preserves_defaults_and_exact_efforts() {
        let entries = opencode_entries();
        let find = |id: &str| {
            entries
                .iter()
                .find(|entry| entry.model_ref.model == id)
                .unwrap()
        };
        for (id, context, output, image) in [
            ("kimi-k3.1-pro", 1_048_576, 131_072, true),
            ("iceman-contributor-free", 262_144, 131_072, false),
            ("minimax-m3", 1_000_000, 131_072, true),
        ] {
            let model = find(id);
            assert_eq!(model.context_window_tokens, Some(context), "{id}");
            assert_eq!(model.max_output_tokens_upper_limit, Some(output), "{id}");
            assert_eq!(model.default_max_output_tokens, None, "{id}");
            assert_eq!(model.capabilities.image_input, image, "{id}");
            assert!(model.capabilities.supports_reasoning, "{id}");
        }
        for (id, endpoint, expected) in [
            (
                "deepseek-v4.1-flash",
                "default",
                &["low", "high", "max"][..],
            ),
            ("glm-5.3", "default", &["low", "high", "max"][..]),
            ("glm-5.3-flash", "default", &["low", "high", "max"][..]),
            ("kimi-k3.1-pro", "default", &["low", "high", "max"][..]),
            ("deepseek-v4-pro", "default", &["high", "max"][..]),
            ("glm-5.2", "default", &["high", "max"][..]),
            ("hy3", "default", &["none", "low", "high"][..]),
            ("hy4-preview", "default", &["none", "high"][..]),
            ("kimi-k3", "default", &["max"][..]),
            (
                "minimax-m3.1-flash",
                "default",
                &["low", "medium", "high", "xhigh", "max"][..],
            ),
            (
                "beam-free",
                "default",
                &["low", "medium", "high", "xhigh", "max"][..],
            ),
            ("step-5-preview", "default", &["low", "medium", "high"][..]),
            (
                "claude-haiku-5-5",
                "messages",
                &["low", "medium", "high", "xhigh", "max"][..],
            ),
            ("qwen3.8-max", "messages", &["low", "medium", "xhigh"][..]),
            ("qwen3.8-flash", "messages", &["low", "medium", "xhigh"][..]),
            (
                "grok-4.7",
                "responses",
                &["low", "medium", "high", "xhigh"][..],
            ),
            (
                "muse-spark-1.2-contributor",
                "responses",
                &["minimal", "low", "medium", "high", "xhigh"][..],
            ),
            (
                "muse-spark-1.3-contributor",
                "responses",
                &["minimal", "low", "medium", "high", "xhigh"][..],
            ),
            (
                "gpt-5.6-luna",
                "responses",
                &["none", "low", "medium", "high", "xhigh", "max"][..],
            ),
            (
                "gpt-6-luna",
                "responses",
                &["none", "low", "medium", "high", "xhigh", "max"][..],
            ),
        ] {
            let model = find(id);
            assert_eq!(
                model
                    .endpoint
                    .as_ref()
                    .map_or("default", ProviderEndpointId::as_str),
                endpoint
            );
            assert_eq!(model.reasoning_effort_options, expected, "{id}");
        }
        for id in [
            "iceman-contributor-free",
            "minimax-m3",
            "gemini-4-argon",
            "minimax-m3.1-flash-free",
        ] {
            assert!(find(id).reasoning_effort_options.is_empty(), "{id}");
        }
    }

    #[test]
    fn opencode_go_gemini_uses_exact_id_without_inventing_capabilities() {
        let entries = opencode_entries();
        let gemini = entries
            .iter()
            .filter(|entry| entry.model_ref.provider == provider_id("opencode-go"))
            .filter(|entry| entry.model_ref.model.starts_with("gemini-"))
            .collect::<Vec<_>>();
        assert_eq!(gemini.len(), 1);
        let model = gemini[0];
        assert_eq!(model.model_ref.model, "gemini-4-argon");
        assert_eq!(model.display_name, "Gemini 4 Argon");
        assert_eq!(model.endpoint, None);
        assert_eq!(model.source, ModelMetadataSource::ConservativeBuiltin);
        assert_eq!(model.context_window_tokens, None);
        assert_eq!(model.auto_compact_token_limit, None);
        assert_eq!(model.default_max_output_tokens, None);
        assert_eq!(model.max_output_tokens_upper_limit, None);
        assert_eq!(model.default_verbosity, None);
        assert_eq!(model.capabilities, ModelCapabilityFlags::default());
        assert!(model.reasoning_effort_options.is_empty());
        assert!(crate::model_discovery::OPENCODE_GO_TEXT_MODELS
            .contains(&("gemini-4-argon", "default")));
    }
}
