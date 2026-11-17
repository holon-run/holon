use super::super::*;

fn codex_daybreak_model(model: &str, name: &str, context: usize) -> BuiltInModelMetadata {
    BuiltInModelMetadata {
        model_ref: ModelRef::new(ProviderId::openai_codex(), model),
        display_name: name.into(),
        description: "Codex default context reviewed independently of the API and experimental maximum context.".into(),
        context_window_tokens: Some(context),
        effective_context_window_percent: 95,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        default_verbosity: None,
        tool_output_truncation_estimated_tokens: Some(2_500),
        capabilities: ModelCapabilityFlags {
            parallel_tool_calls: true,
            image_input: true,
            supports_reasoning: true,
            ..ModelCapabilityFlags::default()
        },
        // Keep Codex's delegation-enabled ultra mode outside these reviewed levels.
        reasoning_effort_options: ["low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        source: ModelMetadataSource::BuiltInCatalog,
        endpoint: None,
    }
}

#[test]
fn codex_daybreak_defaults_are_not_general_api_or_experimental_limits() {
    for catalog in [
        legacy_builtin_catalog().unwrap(),
        BuiltInModelCatalog::new(),
    ] {
        for (model, context) in [
            ("gpt-daybreak-blue-latest", 272_000),
            ("gpt-daybreak-red-latest", 372_000),
        ] {
            let model_ref = ModelRef::new(ProviderId::openai_codex(), model);
            let metadata = catalog.get(&model_ref).unwrap();
            assert_eq!(metadata.context_window_tokens, Some(context));
            assert_eq!(metadata.auto_compact_token_limit, None);
            assert_eq!(metadata.default_max_output_tokens, None);
            assert_eq!(metadata.max_output_tokens_upper_limit, None);
            assert!(metadata.capabilities.parallel_tool_calls);
            assert!(metadata.capabilities.image_input);
            assert_eq!(
                metadata.reasoning_effort_options,
                ["low", "medium", "high", "xhigh", "max"]
            );
        }
    }
    let catalog = BuiltInModelCatalog::new();
    for (model, context) in [
        ("gpt-daybreak-blue-latest", 1_050_000),
        ("gpt-daybreak-red-latest", 400_000),
    ] {
        let api = catalog
            .get(&ModelRef::new(ProviderId::openai(), model))
            .unwrap();
        assert_eq!(api.context_window_tokens, Some(context));
        assert_eq!(api.max_output_tokens_upper_limit, Some(128_000));
    }
}

pub(super) fn entries() -> Vec<BuiltInModelMetadata> {
    vec![
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-fable-5"),
            display_name: "Claude Fable 5".into(),
            description: "Anthropic Claude Fable 5 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: Some(128_000),
            max_output_tokens_upper_limit: Some(128_000),
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
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-opus-4-8"),
            display_name: "Claude Opus 4.8".into(),
            description: "Anthropic Claude Opus 4.8 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: Some(128_000),
            max_output_tokens_upper_limit: Some(128_000),
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
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-sonnet-5"),
            display_name: "Claude Sonnet 5".into(),
            description: "Anthropic Claude Sonnet 5 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(1_000_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(900_000),
            default_max_output_tokens: Some(128_000),
            max_output_tokens_upper_limit: Some(128_000),
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
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-haiku-4-5"),
            display_name: "Claude Haiku 4.5".into(),
            description: "Anthropic Claude Haiku 4.5 runtime defaults aligned with the official model overview.".into(),
            context_window_tokens: Some(200_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(180_000),
            default_max_output_tokens: Some(64_000),
            max_output_tokens_upper_limit: Some(64_000),
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
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-6-astra"),
            display_name: "GPT-6-Astra (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                agent_turn: true,
                parallel_tool_calls: true,
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-6.1-sol"),
            display_name: "GPT-6.1-Sol (Codex)".into(),
            description: "Codex default context from OpenAI codex-rs/models-manager/models.json; account-specific or opt-in larger windows are not global defaults.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                agent_turn: true,
                parallel_tool_calls: true,
                image_input: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-6.1-terra"),
            display_name: "GPT-6.1-Terra (Codex)".into(),
            description: "Reviewed Codex context from OpenAI codex-rs/models-manager/models.json; not the general API context or opt-in maximum.".into(),
            context_window_tokens: Some(500_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                agent_turn: true,
                parallel_tool_calls: true,
                image_input: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-6-luna"),
            display_name: "GPT-6-Luna (Codex)".into(),
            description: "Codex default context from OpenAI codex-rs/models-manager/models.json, not the general API context or output limit.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                agent_turn: true,
                parallel_tool_calls: true,
                image_input: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-6-sol"),
            display_name: "GPT-6-Sol (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                agent_turn: true,
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.6-sol"),
            display_name: "GPT-5.6-Sol (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.6-terra"),
            display_name: "GPT-5.6-Terra (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.6-luna"),
            display_name: "GPT-5.6-Luna (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: Some(ModelVerbosity::Low),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                image_generation: true,
                interactive_exec: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        codex_daybreak_model("gpt-daybreak-blue-latest", "Daybreak Blue (Codex)", 272_000),
        codex_daybreak_model("gpt-daybreak-red-latest", "Daybreak Red (Codex)", 372_000),
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-image-2"),
            display_name: "GPT Image 2".into(),
            description: "OpenAI image generation model for the Images API.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_generation: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::BuiltInCatalog,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.4"),
            display_name: "GPT-5.4".into(),
            description: "Conservative GPT-5.4 runtime defaults aligned with local Codex model behavior.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: Some(128_000),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                supports_reasoning: true,
                ..ModelCapabilityFlags::default()
            },
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.3"),
            display_name: "GPT-5.3".into(),
            description: "Conservative GPT-5.3 runtime defaults used when explicit model metadata is not available locally.".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
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
            source: ModelMetadataSource::ConservativeBuiltin,
            endpoint: None,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.4-mini"),
            display_name: "GPT-5.4 Mini".into(),
            description: "Conservative GPT-5.4 Mini runtime defaults used when explicit model metadata is not available locally.".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: Some(128_000),
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
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
