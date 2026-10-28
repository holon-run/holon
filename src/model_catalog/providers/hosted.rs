use super::super::*;
use super::common::catalog_model;

pub(super) fn venice_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: u32,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("venice"), model);
    let capabilities = ModelCapabilityFlags {
        image_input,
        supports_reasoning,
        ..ModelCapabilityFlags::default()
    };
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref: model_ref.clone(),
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Venice {model} text model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: Some(max_output_tokens),
        max_output_tokens_upper_limit: Some(max_output_tokens),
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        reasoning_effort_options: reasoning_effort_options(&model_ref, None, &capabilities),
        capabilities,
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

pub(super) fn chutes_model_without_published_capabilities(
    model: &str,
    context_window_tokens: usize,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("chutes"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: model.into(),
        description: format!(
            "Holon conservative built-in metadata for the Chutes {model} model; the public model entry does not publish complete capability fields."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags::default(),
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

pub(super) fn arcee_model(model: &str, display_name: &str) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("arcee"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Arcee hosted {model} model."
        ),
        context_window_tokens: Some(131_072),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags::default(),
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

pub(super) fn huggingface_gpt_oss_model() -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("huggingface"), "openai/gpt-oss-120b");
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: "OpenAI GPT OSS 120B".into(),
        description:
            "Holon conservative built-in metadata for the Hugging Face Inference Providers quick-start chat model."
                .into(),
        context_window_tokens: Some(131_072),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: vec!["low".into(), "medium".into(), "high".into()],
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

pub(super) fn fireworks_model(
    model: &str,
    display_name: &str,
    context_window_tokens: Option<usize>,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("fireworks"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Fireworks serverless {model} model."
        ),
        context_window_tokens,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
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

pub(super) fn nvidia_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("nvidia"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the NVIDIA-hosted {model} model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
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

pub(super) fn together_model(
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    supports_reasoning: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    let model_ref = ModelRef::new(provider_id("together"), model);
    BuiltInModelMetadata {
        default_verbosity: default_verbosity_for_model(&model_ref),
        model_ref,
        display_name: display_name.into(),
        description: format!(
            "Holon conservative built-in metadata for the Together serverless {model} model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
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

fn reviewed_together_model(
    model: &str,
    display_name: &str,
    effort_options: &[&str],
) -> BuiltInModelMetadata {
    BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id("together"), model),
        display_name: display_name.into(),
        description: format!(
            "Together serverless {model} Chat route confirmed by its official model page; exact context and output limits remain unconfirmed."
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
            image_input: true,
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        reasoning_effort_options: effort_options.iter().map(|value| (*value).into()).collect(),
        source: ModelMetadataSource::ConservativeBuiltin,
        endpoint: None,
    }
}

fn moonshot_k3_model() -> BuiltInModelMetadata {
    let mut model = catalog_model(
        "moonshot", "kimi-k3", "Kimi K3", 1_048_576, 131_072, true, true,
    );
    model.max_output_tokens_upper_limit = Some(1_048_576);
    model
}

pub(super) fn early_entries() -> Vec<BuiltInModelMetadata> {
    vec![
        catalog_model(
            "openai",
            "gpt-5.6-sol",
            "GPT-5.6 Sol",
            372_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "openai",
            "gpt-5.6-terra",
            "GPT-5.6 Terra",
            372_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "openai",
            "gpt-5.6-luna",
            "GPT-5.6 Luna",
            372_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-3.5-flash",
            "Gemini 3.5 Flash",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-3.1-pro-preview",
            "Gemini 3.1 Pro Preview",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-3.1-flash-lite",
            "Gemini 3.1 Flash-Lite",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-2.5-pro",
            "Gemini 2.5 Pro",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-2.5-flash",
            "Gemini 2.5 Flash",
            1_048_576,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "gemini",
            "gemini-2.5-flash-lite",
            "Gemini 2.5 Flash-Lite",
            1_048_576,
            65_536,
            true,
            true,
        ),
        arcee_model("trinity-mini", "Trinity Mini 26B"),
        arcee_model("trinity-large-preview", "Trinity Large Preview"),
        catalog_model(
            "chutes",
            "moonshotai/Kimi-K2.6-TEE",
            "moonshotai/Kimi-K2.6-TEE",
            262_144,
            65_535,
            true,
            true,
        ),
        catalog_model(
            "chutes",
            "zai-org/GLM-5.2-TEE",
            "zai-org/GLM-5.2-TEE",
            1_048_576,
            65_535,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "zai-org/GLM-5.1-TEE",
            "zai-org/GLM-5.1-TEE",
            202_752,
            65_535,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "zai-org/GLM-5-TEE",
            "zai-org/GLM-5-TEE",
            202_752,
            65_535,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "deepseek-ai/DeepSeek-V3.2-TEE",
            "deepseek-ai/DeepSeek-V3.2-TEE",
            131_072,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "MiniMaxAI/MiniMax-M2.5-TEE",
            "MiniMaxAI/MiniMax-M2.5-TEE",
            196_608,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "moonshotai/Kimi-K2.5-TEE",
            "moonshotai/Kimi-K2.5-TEE",
            262_144,
            65_535,
            true,
            true,
        ),
        catalog_model(
            "chutes",
            "Qwen/Qwen3.6-27B-TEE",
            "Qwen/Qwen3.6-27B-TEE",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "chutes",
            "Qwen/Qwen3.5-397B-A17B-TEE",
            "Qwen/Qwen3.5-397B-A17B-TEE",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "chutes",
            "Qwen/Qwen3-235B-A22B-Thinking-2507-TEE",
            "Qwen/Qwen3-235B-A22B-Thinking-2507-TEE",
            262_144,
            262_144,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "Qwen/Qwen3-32B-TEE",
            "Qwen/Qwen3-32B-TEE",
            40_960,
            40_960,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "google/gemma-4-31B-turbo-TEE",
            "google/gemma-4-31B-turbo-TEE",
            131_072,
            65_536,
            true,
            true,
        ),
        chutes_model_without_published_capabilities(
            "unsloth/Mistral-Nemo-Instruct-2407-TEE",
            131_072,
        ),
        catalog_model(
            "deepseek",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "deepseek",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            384_000,
            true,
            false,
        ),
        BuiltInModelMetadata {
            model_ref: ModelRef::new(provider_id("deepseek"), "deepseek-flash"),
            display_name: "DeepSeek Flash".into(),
            description: "DeepSeek Flash with image input through the Responses API; \
                conservative reviewed ceilings, not the native API's full binary-unit limits."
                .into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: Some(384_000),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: None,
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: Some(
                ProviderEndpointId::parse("responses").expect("valid built-in endpoint"),
            ),
        },
        // The former preferred K2.6 serverless ID retired on 2026-09-25.
        // Prefer a confirmed serverless model, not a deployment-only successor.
        BuiltInModelMetadata {
            max_output_tokens_upper_limit: Some(512_000),
            ..fireworks_model(
                "accounts/fireworks/models/minimax-m3",
                "MiniMax M3",
                Some(512_000),
                true,
                true,
            )
        },
        fireworks_model(
            "accounts/fireworks/models/deepseek-v4-flash",
            "DeepSeek V4 Flash",
            Some(1_048_576),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/deepseek-v4-pro",
            "DeepSeek V4 Pro",
            Some(1_048_576),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/glm-5p1",
            "GLM 5.1",
            Some(202_752),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/gpt-oss-120b",
            "GPT-OSS 120B",
            Some(131_072),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/minimax-m2p7",
            "MiniMax M2.7",
            Some(196_608),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
            "Nemotron 3 Ultra NVFP4",
            Some(262_144),
            true,
            false,
        ),
        fireworks_model(
            "accounts/fireworks/models/qwen3p6-plus",
            "Qwen 3.6 Plus",
            None,
            true,
            true,
        ),
        fireworks_model(
            "accounts/fireworks/models/qwen3p7-plus",
            "Qwen 3.7 Plus",
            Some(262_144),
            true,
            true,
        ),
        huggingface_gpt_oss_model(),
    ]
}

fn minimax_m31_model(provider: &str) -> BuiltInModelMetadata {
    let mut entry = catalog_model(
        provider,
        "MiniMax-M3.1-Flash",
        "MiniMax M3.1 Flash",
        1_000_000,
        131_072,
        true,
        true,
    );
    entry.description = "MiniMax M3.1 Flash supports general API access; M Plan and existing Token Plan access use separate subscription credentials.".into();
    entry.max_output_tokens_upper_limit = Some(524_288);
    entry.reasoning_effort_options = vec![
        "low".into(),
        "medium".into(),
        "high".into(),
        "xhigh".into(),
        "max".into(),
    ];
    entry
}

fn minimax_m3_model(provider: &str) -> BuiltInModelMetadata {
    let mut entry = catalog_model(
        provider,
        "MiniMax-M3",
        "MiniMax M3",
        1_000_000,
        131_072,
        true,
        true,
    );
    entry.max_output_tokens_upper_limit = Some(524_288);
    entry
}

fn minimax_m_plan_model(provider: &str) -> BuiltInModelMetadata {
    BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id(provider), "MiniMax-M3.1-Flash-Preview"),
        display_name: "MiniMax M3.1 Flash Preview".into(),
        description: "Legacy MiniMax M3.1 Flash Preview subscription ID is now served by MiniMax-M3.1-Flash; requires M Plan, an existing Token Plan, or MiniMax Code, not general API access.".into(),
        context_window_tokens: Some(1_000_000),
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
        reasoning_effort_options: vec![
            "low".into(),
            "medium".into(),
            "high".into(),
            "xhigh".into(),
            "max".into(),
        ],
        source: ModelMetadataSource::BuiltInCatalog,
        endpoint: Some(ProviderEndpointId::parse("m-plan").expect("valid endpoint")),
    }
}

pub(super) fn route_definitions() -> Vec<BuiltInModelRouteDefinition> {
    let mut definitions = Vec::new();
    for (provider, subscription_provider) in [
        ("minimax", "minimax-m-plan"),
        ("minimax-cn", "minimax-cn-m-plan"),
    ] {
        for (endpoint, legacy_provider, model) in [
            ("default", provider, "MiniMax-M3"),
            ("default", provider, "MiniMax-M3.1-Flash"),
            ("m-plan", subscription_provider, "MiniMax-M3.1-Flash"),
            (
                "m-plan",
                subscription_provider,
                "MiniMax-M3.1-Flash-Preview",
            ),
        ] {
            definitions.push(BuiltInModelRouteDefinition {
                legacy_provider: provider_id(legacy_provider),
                model_ref: ModelRef::new(provider_id(provider), model),
                endpoint: ProviderEndpointId::parse(endpoint).expect("valid endpoint"),
                policy: BuiltInModelRoutePolicy {
                    // M3/M3.1 do not support explicit cache_control. The legacy
                    // Preview ID forwards to M3.1; implicit caching is separate.
                    cache_capabilities: Some(ModelRouteCacheCapabilities {
                        cache_control: Some(false),
                    }),
                    ..BuiltInModelRoutePolicy::default()
                },
            });
        }
    }
    definitions
}

pub(super) fn middle_entries() -> Vec<BuiltInModelMetadata> {
    vec![
        minimax_m31_model("minimax"),
        minimax_m31_model("minimax-cn"),
        minimax_m3_model("minimax"),
        minimax_m3_model("minimax-cn"),
        catalog_model(
            "minimax",
            "MiniMax-M2.7",
            "MiniMax M2.7",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.7-highspeed",
            "MiniMax M2.7 Highspeed",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.5",
            "MiniMax M2.5",
            204_800,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.5-highspeed",
            "MiniMax M2.5 Highspeed",
            204_800,
            128_000,
            true,
            false,
        ),
        minimax_m_plan_model("minimax-m-plan"),
        minimax_m_plan_model("minimax-cn-m-plan"),
        catalog_model(
            "mistral",
            "codestral-latest",
            "Codestral (latest)",
            128_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "mistral",
            "mistral-large-latest",
            "Mistral Large 3 (latest)",
            256_000,
            16_384,
            false,
            true,
        ),
        catalog_model(
            "mistral",
            "mistral-medium-latest",
            "Mistral Medium 3.5 (latest)",
            256_000,
            8_192,
            true,
            true,
        ),
        catalog_model(
            "mistral",
            "mistral-small-latest",
            "Mistral Small 4 (latest)",
            256_000,
            16_384,
            false,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.7-code-highspeed",
            "Kimi K2.7 Code HighSpeed",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.6",
            "Kimi K2.6",
            262_144,
            262_144,
            true,
            true,
        ),
        moonshot_k3_model(),
        catalog_model(
            "nearai",
            "zai-org/GLM-5.1-FP8",
            "GLM 5.1 (NEAR AI Cloud TEE)",
            202_752,
            16_384,
            true,
            false,
        ),
        catalog_model(
            "nearai",
            "Qwen/Qwen3.6-35B-A3B-FP8",
            "Qwen 3.6 35B A3B FP8 (NEAR AI Cloud TEE)",
            262_144,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "nearai",
            "Qwen/Qwen3.5-122B-A10B",
            "Qwen 3.5 122B A10B (NEAR AI Cloud TEE)",
            262_144,
            16_384,
            true,
            true,
        ),
        catalog_model(
            "nearai",
            "Qwen/Qwen3-VL-30B-A3B-Instruct",
            "Qwen3 VL 30B A3B Instruct (NEAR AI Cloud TEE)",
            16_384,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "nearai",
            "google/gemma-4-31B-it",
            "Gemma 4 31B Instruct (NEAR AI Cloud TEE)",
            262_144,
            8_192,
            true,
            true,
        ),
        nvidia_model(
            "nvidia/nemotron-3-super-120b-a12b",
            "NVIDIA Nemotron 3 Super 120B",
            1_000_000,
            true,
            false,
        ),
        nvidia_model("moonshotai/kimi-k2.6", "Kimi K2.6", 262_144, true, true),
        nvidia_model(
            "minimaxai/minimax-m2.7",
            "MiniMax M2.7",
            204_800,
            true,
            false,
        ),
        nvidia_model("minimaxai/minimax-m3", "MiniMax M3", 1_000_000, true, true),
        nvidia_model("z-ai/glm-5.2", "GLM-5.2", 1_000_000, true, false),
    ]
}

pub(super) fn late_entries() -> Vec<BuiltInModelMetadata> {
    vec![
        catalog_model(
            "synthetic",
            "syn:large:text",
            "Synthetic Large Text",
            524_288,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "synthetic",
            "syn:small:text",
            "Synthetic Small Text",
            196_608,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "synthetic",
            "syn:large:vision",
            "Synthetic Large Vision",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "synthetic",
            "syn:small:vision",
            "Synthetic Small Vision",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "synthetic",
            "hf:openai/gpt-oss-120b",
            "OpenAI GPT OSS 120B",
            131_072,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:zai-org/GLM-5.2",
            "Z.ai GLM-5.2",
            524_288,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:moonshotai/Kimi-K2.7-Code",
            "Moonshot AI Kimi K2.7 Code",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "synthetic",
            "hf:Qwen/Qwen3.6-27B",
            "Qwen3.6 27B",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "synthetic",
            "hf:MiniMaxAI/MiniMax-M3",
            "MiniMax M3",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "synthetic",
            "hf:zai-org/GLM-4.7-Flash",
            "Z.ai GLM-4.7 Flash",
            196_608,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-NVFP4",
            "NVIDIA Nemotron 3 Super 120B A12B NVFP4",
            262_144,
            65_536,
            true,
            false,
        ),
        together_model("MiniMaxAI/MiniMax-M3", "MiniMax M3", 524_288, true, true),
        reviewed_together_model(
            "MiniMaxAI/MiniMax-M3.1-Flash",
            "MiniMax M3.1 Flash",
            &["low", "medium", "high", "xhigh", "max"],
        ),
        reviewed_together_model(
            "deepseek-ai/DeepSeek-V4.1-Flash-1017",
            "DeepSeek V4.1 Flash 1017",
            &[],
        ),
        together_model(
            "MiniMaxAI/MiniMax-M2.7",
            "MiniMax M2.7",
            202_752,
            true,
            false,
        ),
        together_model("Qwen/Qwen3.5-9B", "Qwen3.5 9B", 262_144, true, true),
        together_model(
            "moonshotai/Kimi-K2.7-Code",
            "Kimi K2.7 Code",
            262_144,
            false,
            true,
        ),
        together_model("moonshotai/Kimi-K2.6", "Kimi K2.6", 262_144, true, true),
        together_model("zai-org/GLM-5.2", "GLM-5.2", 262_144, false, false),
        together_model("openai/gpt-oss-120b", "GPT-OSS 120B", 128_000, true, false),
        together_model(
            "deepseek-ai/DeepSeek-V4-Pro",
            "DeepSeek V4 Pro",
            512_000,
            true,
            false,
        ),
        together_model(
            "nvidia/nemotron-3-ultra-550b-a55b",
            "NVIDIA Nemotron 3 Ultra 550B",
            512_300,
            true,
            false,
        ),
        together_model(
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "Llama 3.3 70B Instruct Turbo",
            131_072,
            false,
            false,
        ),
        venice_model(
            "zai-org-glm-4.7",
            "GLM 4.7 (via Venice)",
            198_000,
            16_384,
            true,
            false,
        ),
        venice_model(
            "qwen3-235b-a22b-thinking-2507",
            "Qwen3 235B A22B Thinking 2507 (via Venice)",
            128_000,
            16_384,
            true,
            false,
        ),
        venice_model(
            "qwen3-vl-235b-a22b",
            "Qwen3 VL 235B (via Venice)",
            128_000,
            16_384,
            false,
            true,
        ),
        venice_model(
            "qwen3-coder-480b-a35b-instruct-turbo",
            "Qwen3 Coder 480B Turbo (via Venice)",
            256_000,
            65_536,
            false,
            false,
        ),
        venice_model(
            "venice-uncensored-1-2",
            "Venice Uncensored 1.2",
            128_000,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "anthropic/claude-opus-4.6",
            "Claude Opus 4.6",
            1_000_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "openai/gpt-5.4",
            "GPT 5.4",
            1_050_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "openai/gpt-5.4-pro",
            "GPT 5.4 Pro",
            1_050_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "moonshotai/kimi-k2.6",
            "Kimi K2.6",
            262_000,
            262_000,
            true,
            true,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fireworks_retired_serverless_ids_preserve_custom_deployments_and_peers() {
        for catalog in [
            legacy_builtin_catalog().unwrap(),
            BuiltInModelCatalog::new(),
        ] {
            assert_eq!(
                catalog
                    .preferred_route_for_provider(&provider_id("fireworks"))
                    .unwrap()
                    .as_string(),
                "fireworks@default/accounts/fireworks/models/minimax-m3"
            );
            for model in ["kimi-k2p6", "kimi-k2p7-code", "glm-5p2"] {
                let route = ModelRouteRef::parse(&format!(
                    "fireworks@default/accounts/fireworks/models/{model}"
                ))
                .unwrap();
                assert!(catalog.get_route(&route).is_none(), "{}", route.as_string());
            }
            for route in [
                "fireworks@default/accounts/fireworks/models/minimax-m3",
                "fireworks@default/accounts/fireworks/models/deepseek-v4-flash",
                "tencent-tokenhub@default/kimi-k2.7-code",
                "dashscope@token-plan/glm-5.2",
            ] {
                assert!(
                    catalog
                        .get_route(&ModelRouteRef::parse(route).unwrap())
                        .is_some(),
                    "{route}"
                );
            }
            let deployment = ModelRouteRef::parse(
                "fireworks@default/accounts/example/deployments/reviewed-kimi",
            )
            .unwrap();
            let policy = catalog.resolve_route_policy(
                &deployment,
                &HashMap::new(),
                &HashMap::new(),
                Some(&ModelRuntimeOverride {
                    prompt_budget_estimated_tokens: Some(64_000),
                    ..ModelRuntimeOverride::default()
                }),
                &ContextConfig::default(),
                8_192,
            );
            assert_eq!(policy.source, ModelMetadataSource::UnknownFallback);
            assert_eq!(policy.prompt_budget_estimated_tokens, 64_000);
        }
    }

    #[test]
    fn together_minimax_m3_supports_reasoning_without_inventing_limits_or_effort() {
        let model = late_entries()
            .into_iter()
            .find(|model| {
                model.model_ref == ModelRef::new(provider_id("together"), "MiniMaxAI/MiniMax-M3")
            })
            .expect("Together MiniMax M3 builtin");

        assert!(model.capabilities.supports_reasoning);
        assert!(model.capabilities.image_input);
        assert!(model.reasoning_effort_options.is_empty());
        assert_eq!(model.context_window_tokens, Some(524_288));
        assert_eq!(model.default_max_output_tokens, None);
        assert_eq!(model.max_output_tokens_upper_limit, None);
    }

    #[test]
    fn fireworks_minimax_m3_output_ceiling_preserves_default_and_other_models() {
        let model = early_entries()
            .into_iter()
            .find(|model| {
                model.model_ref
                    == ModelRef::new(
                        provider_id("fireworks"),
                        "accounts/fireworks/models/minimax-m3",
                    )
            })
            .expect("Fireworks MiniMax M3 builtin");

        assert_eq!(model.max_output_tokens_upper_limit, Some(512_000));
        assert_eq!(model.default_max_output_tokens, None);
        assert_eq!(model.context_window_tokens, Some(512_000));
        assert!(model.capabilities.image_input);
        assert!(model.capabilities.supports_reasoning);
        assert!(model.reasoning_effort_options.is_empty());

        for id in [
            "accounts/fireworks/models/gpt-oss-120b",
            "accounts/fireworks/models/minimax-m2p7",
        ] {
            let other = early_entries()
                .into_iter()
                .find(|model| model.model_ref == ModelRef::new(provider_id("fireworks"), id))
                .expect("existing Fireworks builtin");
            assert_eq!(other.default_max_output_tokens, None, "{id}");
            assert_eq!(other.max_output_tokens_upper_limit, None, "{id}");
        }
    }
}
