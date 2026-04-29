use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::{ModelRef, ProviderId};
use crate::context::ContextConfig;

const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u8 = 95;
const DEFAULT_COMPACTION_TRIGGER_PERCENT: u8 = 90;
const DEFAULT_KEEP_RECENT_PERCENT: u8 = 38;
const DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS: usize = 2_500;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataSource {
    BuiltInCatalog,
    ConservativeBuiltin,
    ConfigOverride,
    UnknownFallback,
}

impl Default for ModelMetadataSource {
    fn default() -> Self {
        Self::UnknownFallback
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelCapabilityFlags {
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub reasoning_summaries: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub interactive_exec: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuiltInModelMetadata {
    pub model_ref: ModelRef,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub context_window_tokens: Option<usize>,
    #[serde(default = "default_effective_context_window_percent")]
    pub effective_context_window_percent: u8,
    #[serde(default)]
    pub auto_compact_token_limit: Option<usize>,
    #[serde(default)]
    pub default_max_output_tokens: Option<u32>,
    #[serde(default)]
    pub max_output_tokens_upper_limit: Option<u32>,
    #[serde(default)]
    pub tool_output_truncation_estimated_tokens: Option<usize>,
    #[serde(default)]
    pub capabilities: ModelCapabilityFlags,
    pub source: ModelMetadataSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelRuntimeOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_context_window_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_budget_estimated_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_trigger_estimated_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_keep_recent_estimated_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output_truncation_estimated_tokens: Option<usize>,
}

impl ModelRuntimeOverride {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.description.is_none()
            && self.context_window_tokens.is_none()
            && self.effective_context_window_percent.is_none()
            && self.auto_compact_token_limit.is_none()
            && self.prompt_budget_estimated_tokens.is_none()
            && self.compaction_trigger_estimated_tokens.is_none()
            && self.compaction_keep_recent_estimated_tokens.is_none()
            && self.runtime_max_output_tokens.is_none()
            && self.tool_output_truncation_estimated_tokens.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedRuntimeModelPolicy {
    pub model_ref: ModelRef,
    pub display_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,
    pub effective_context_window_percent: u8,
    pub prompt_budget_estimated_tokens: usize,
    pub compaction_trigger_estimated_tokens: usize,
    pub compaction_keep_recent_estimated_tokens: usize,
    pub runtime_max_output_tokens: u32,
    pub tool_output_truncation_estimated_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens_upper_limit: Option<u32>,
    #[serde(default)]
    pub capabilities: ModelCapabilityFlags,
    pub source: ModelMetadataSource,
}

impl Default for ResolvedRuntimeModelPolicy {
    fn default() -> Self {
        Self {
            model_ref: ModelRef::new(ProviderId::openai(), "unknown"),
            display_name: "Unknown model".into(),
            description: "Legacy model state without resolved runtime policy.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            runtime_max_output_tokens: 8192,
            tool_output_truncation_estimated_tokens:
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            max_output_tokens_upper_limit: None,
            capabilities: ModelCapabilityFlags::default(),
            source: ModelMetadataSource::UnknownFallback,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInModelCatalog {
    entries: HashMap<ModelRef, BuiltInModelMetadata>,
}

impl BuiltInModelCatalog {
    pub fn new() -> Self {
        let entries = built_in_entries()
            .into_iter()
            .map(|entry| (entry.model_ref.clone(), entry))
            .collect();
        Self { entries }
    }

    pub fn get(&self, model_ref: &ModelRef) -> Option<&BuiltInModelMetadata> {
        self.entries.get(model_ref)
    }

    pub fn list(&self) -> Vec<BuiltInModelMetadata> {
        let mut entries = self.entries.values().cloned().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        entries
    }

    pub fn resolve_policy(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
    ) -> ResolvedRuntimeModelPolicy {
        let built_in = self.get(model_ref);
        let override_config = overrides.get(model_ref);
        let fallback_override = if built_in.is_none() {
            unknown_fallback
        } else {
            None
        };
        let source = if override_config.is_some() || fallback_override.is_some() {
            if built_in.is_some() {
                ModelMetadataSource::ConfigOverride
            } else {
                ModelMetadataSource::UnknownFallback
            }
        } else {
            built_in
                .map(|entry| entry.source)
                .unwrap_or(ModelMetadataSource::UnknownFallback)
        };
        let display_name = override_config
            .and_then(|value| value.display_name.clone())
            .or_else(|| built_in.map(|entry| entry.display_name.clone()))
            .or_else(|| fallback_override.and_then(|value| value.display_name.clone()))
            .unwrap_or_else(|| model_ref.as_string());
        let description = override_config
            .and_then(|value| value.description.clone())
            .or_else(|| built_in.map(|entry| entry.description.clone()))
            .or_else(|| fallback_override.and_then(|value| value.description.clone()))
            .unwrap_or_else(|| "Explicit unknown-model fallback policy".to_string());
        let context_window_tokens = override_config
            .and_then(|value| value.context_window_tokens)
            .or_else(|| built_in.and_then(|entry| entry.context_window_tokens))
            .or_else(|| fallback_override.and_then(|value| value.context_window_tokens));
        let effective_context_window_percent = validated_percent(
            override_config
                .and_then(|value| value.effective_context_window_percent)
                .or_else(|| built_in.map(|entry| entry.effective_context_window_percent))
                .or_else(|| {
                    fallback_override.and_then(|value| value.effective_context_window_percent)
                })
                .unwrap_or(DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT),
        );
        let prompt_budget_estimated_tokens = override_config
            .and_then(|value| value.prompt_budget_estimated_tokens)
            .or_else(|| {
                context_window_tokens
                    .map(|window| percent_of(window, usize::from(effective_context_window_percent)))
            })
            .or_else(|| fallback_override.and_then(|value| value.prompt_budget_estimated_tokens))
            .unwrap_or(base_context_config.prompt_budget_estimated_tokens);
        let auto_compact_token_limit = override_config
            .and_then(|value| value.auto_compact_token_limit)
            .or_else(|| built_in.and_then(|entry| entry.auto_compact_token_limit))
            .or_else(|| fallback_override.and_then(|value| value.auto_compact_token_limit));
        let compaction_trigger_estimated_tokens = override_config
            .and_then(|value| value.compaction_trigger_estimated_tokens)
            .or_else(|| {
                fallback_override.and_then(|value| value.compaction_trigger_estimated_tokens)
            })
            .or(auto_compact_token_limit)
            .or_else(|| {
                Some(percent_of(
                    prompt_budget_estimated_tokens,
                    usize::from(DEFAULT_COMPACTION_TRIGGER_PERCENT),
                ))
            })
            .unwrap_or(base_context_config.compaction_trigger_estimated_tokens);
        let compaction_keep_recent_estimated_tokens = override_config
            .and_then(|value| value.compaction_keep_recent_estimated_tokens)
            .or_else(|| {
                fallback_override.and_then(|value| value.compaction_keep_recent_estimated_tokens)
            })
            .or_else(|| {
                Some(percent_of(
                    compaction_trigger_estimated_tokens,
                    usize::from(DEFAULT_KEEP_RECENT_PERCENT),
                ))
            })
            .unwrap_or(base_context_config.compaction_keep_recent_estimated_tokens);
        let runtime_max_output_tokens = override_config
            .and_then(|value| value.runtime_max_output_tokens)
            .or_else(|| built_in.and_then(|entry| entry.default_max_output_tokens))
            .or_else(|| fallback_override.and_then(|value| value.runtime_max_output_tokens))
            .unwrap_or(configured_runtime_max_output_tokens);
        let tool_output_truncation_estimated_tokens = override_config
            .and_then(|value| value.tool_output_truncation_estimated_tokens)
            .or_else(|| built_in.and_then(|entry| entry.tool_output_truncation_estimated_tokens))
            .or_else(|| {
                fallback_override.and_then(|value| value.tool_output_truncation_estimated_tokens)
            })
            .unwrap_or(DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS);
        let max_output_tokens_upper_limit =
            built_in.and_then(|entry| entry.max_output_tokens_upper_limit);
        let capabilities = built_in
            .map(|entry| entry.capabilities.clone())
            .unwrap_or_default();

        ResolvedRuntimeModelPolicy {
            model_ref: model_ref.clone(),
            display_name,
            description,
            context_window_tokens,
            effective_context_window_percent,
            prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens,
            runtime_max_output_tokens,
            tool_output_truncation_estimated_tokens,
            max_output_tokens_upper_limit,
            capabilities,
            source,
        }
    }

    pub fn apply_policy(
        &self,
        model_ref: &ModelRef,
        overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
        unknown_fallback: Option<&ModelRuntimeOverride>,
        base_context_config: &ContextConfig,
        configured_runtime_max_output_tokens: u32,
    ) -> (ContextConfig, ResolvedRuntimeModelPolicy) {
        let policy = self.resolve_policy(
            model_ref,
            overrides,
            unknown_fallback,
            base_context_config,
            configured_runtime_max_output_tokens,
        );
        let context_config = ContextConfig {
            recent_messages: base_context_config.recent_messages,
            recent_briefs: base_context_config.recent_briefs,
            compaction_trigger_messages: base_context_config.compaction_trigger_messages,
            compaction_keep_recent_messages: base_context_config.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: policy.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: policy.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: policy.compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: base_context_config.recent_episode_candidates,
            max_relevant_episodes: base_context_config.max_relevant_episodes,
        };
        (context_config, policy)
    }
}

impl Default for BuiltInModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}

fn default_effective_context_window_percent() -> u8 {
    DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT
}

fn validated_percent(percent: u8) -> u8 {
    percent.clamp(1, 100)
}

fn percent_of(total: usize, percent: usize) -> usize {
    total.saturating_mul(percent) / 100
}

fn provider_id(provider: &str) -> ProviderId {
    ProviderId::parse(provider).expect("valid built-in provider id")
}

fn catalog_model(
    provider: &str,
    model: &str,
    display_name: &str,
    context_window_tokens: usize,
    max_output_tokens: u32,
    reasoning_summaries: bool,
    image_input: bool,
) -> BuiltInModelMetadata {
    BuiltInModelMetadata {
        model_ref: ModelRef::new(provider_id(provider), model),
        display_name: display_name.into(),
        description: format!(
            "Holon built-in runtime metadata for the {provider}/{model} compatible provider model."
        ),
        context_window_tokens: Some(context_window_tokens),
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        auto_compact_token_limit: None,
        default_max_output_tokens: Some(max_output_tokens),
        max_output_tokens_upper_limit: Some(max_output_tokens),
        tool_output_truncation_estimated_tokens: Some(
            DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
        ),
        capabilities: ModelCapabilityFlags {
            image_input,
            reasoning_summaries,
            ..ModelCapabilityFlags::default()
        },
        source: ModelMetadataSource::BuiltInCatalog,
    }
}

fn built_in_entries() -> Vec<BuiltInModelMetadata> {
    let mut entries = vec![
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-sonnet-4-6"),
            display_name: "Claude Sonnet 4.6".into(),
            description: "Anthropic Sonnet 4.6 runtime defaults mirrored from local Claude Code rules.".into(),
            context_window_tokens: Some(200_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(180_000),
            default_max_output_tokens: Some(32_000),
            max_output_tokens_upper_limit: Some(128_000),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::BuiltInCatalog,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::anthropic(), "claude-haiku-4-5"),
            display_name: "Claude Haiku 4.5".into(),
            description: "Anthropic Haiku 4.5 runtime defaults mirrored from local Claude Code rules.".into(),
            context_window_tokens: Some(200_000),
            effective_context_window_percent: 90,
            auto_compact_token_limit: Some(180_000),
            default_max_output_tokens: Some(32_000),
            max_output_tokens_upper_limit: Some(64_000),
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::BuiltInCatalog,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.4"),
            display_name: "GPT-5.4 (Codex)".into(),
            description: "Codex runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                reasoning_summaries: true,
                interactive_exec: true,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::BuiltInCatalog,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai_codex(), "gpt-5.3-codex-spark"),
            display_name: "GPT-5.3 Codex Spark (Codex)".into(),
            description: "Codex Spark runtime defaults mirrored from the local OpenAI Codex model metadata contract.".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                reasoning_summaries: true,
                interactive_exec: true,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::BuiltInCatalog,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.4"),
            display_name: "GPT-5.4".into(),
            description: "Conservative GPT-5.4 runtime defaults aligned with local Codex model behavior.".into(),
            context_window_tokens: Some(272_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                reasoning_summaries: true,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::ConservativeBuiltin,
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
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                reasoning_summaries: true,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::ConservativeBuiltin,
        },
        BuiltInModelMetadata {
            model_ref: ModelRef::new(ProviderId::openai(), "gpt-5.4-mini"),
            display_name: "GPT-5.4 Mini".into(),
            description: "Conservative GPT-5.4 Mini runtime defaults used when explicit model metadata is not available locally.".into(),
            context_window_tokens: Some(128_000),
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: ModelCapabilityFlags {
                image_input: true,
                reasoning_summaries: true,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::ConservativeBuiltin,
        },
    ];
    entries.extend(compatible_provider_model_entries());
    entries
}

fn compatible_provider_model_entries() -> Vec<BuiltInModelMetadata> {
    vec![
        catalog_model(
            "anthropic",
            "claude-opus-4-7",
            "Claude Opus 4.7",
            1_000_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "anthropic",
            "claude-opus-4-6",
            "Claude Opus 4.6",
            1_000_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "anthropic",
            "claude-opus-4-5",
            "Claude Opus 4.5",
            200_000,
            64_000,
            true,
            true,
        ),
        catalog_model(
            "anthropic",
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            200_000,
            64_000,
            true,
            true,
        ),
        catalog_model(
            "openai-codex",
            "gpt-5.5",
            "GPT-5.5 (Codex)",
            272_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "openai-codex",
            "gpt-5.4-mini",
            "GPT-5.4 Mini (Codex)",
            272_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "openai-codex",
            "gpt-5.2",
            "GPT-5.2 (Codex)",
            272_000,
            128_000,
            true,
            true,
        ),
        catalog_model("openai", "gpt-5.5", "GPT-5.5", 272_000, 128_000, true, true),
        catalog_model("openai", "gpt-5.2", "GPT-5.2", 272_000, 128_000, true, true),
        catalog_model(
            "arcee",
            "trinity-mini",
            "Trinity Mini 26B",
            131_072,
            80_000,
            false,
            false,
        ),
        catalog_model(
            "arcee",
            "trinity-large-preview",
            "Trinity Large Preview",
            131_072,
            16_384,
            false,
            false,
        ),
        catalog_model(
            "arcee",
            "trinity-large-thinking",
            "Trinity Large Thinking",
            262_144,
            80_000,
            true,
            false,
        ),
        catalog_model(
            "byteplus",
            "seed-1-8-251228",
            "Seed 1.8",
            256_000,
            4_096,
            false,
            true,
        ),
        catalog_model(
            "byteplus",
            "moonshotai/kimi-k2.5",
            "Kimi K2.5",
            262_144,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "byteplus",
            "zai-org/glm-4.7",
            "GLM-4.7",
            204_800,
            131_072,
            true,
            false,
        ),
        catalog_model(
            "byteplus-coding",
            "ark-code-latest",
            "Ark Code Latest",
            256_000,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "chutes",
            "zai-org/GLM-4.7-TEE",
            "zai-org/GLM-4.7-TEE",
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
            "moonshotai/Kimi-K2.5-TEE",
            "moonshotai/Kimi-K2.5-TEE",
            262_144,
            65_535,
            true,
            true,
        ),
        catalog_model(
            "chutes",
            "openai/gpt-oss-120b-TEE",
            "openai/gpt-oss-120b-TEE",
            131_072,
            65_536,
            true,
            false,
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
        catalog_model(
            "deepseek",
            "deepseek-chat",
            "DeepSeek Chat",
            131_072,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "deepseek",
            "deepseek-reasoner",
            "DeepSeek Reasoner",
            131_072,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "fireworks",
            "accounts/fireworks/models/kimi-k2p6",
            "Kimi K2.6",
            262_144,
            262_144,
            false,
            true,
        ),
        catalog_model(
            "fireworks",
            "accounts/fireworks/routers/kimi-k2p5-turbo",
            "Kimi K2.5 Turbo (Fire Pass)",
            256_000,
            256_000,
            false,
            true,
        ),
        catalog_model(
            "huggingface",
            "moonshotai/Kimi-K2-Instruct",
            "MoonshotAI Kimi K2 Instruct",
            262_144,
            32_768,
            false,
            false,
        ),
        catalog_model(
            "kilocode",
            "kilo/auto",
            "Kilo Auto",
            1_000_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "litellm",
            "claude-opus-4-6",
            "Claude Opus 4.6",
            200_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.7",
            "MiniMax M2.7",
            200_000,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax",
            "MiniMax-M2.7-highspeed",
            "MiniMax M2.7 Highspeed",
            200_000,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "minimax-portal",
            "MiniMax-M2.7",
            "MiniMax M2.7",
            200_000,
            128_000,
            true,
            false,
        ),
        catalog_model(
            "mistral",
            "codestral-latest",
            "Codestral (latest)",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "mistral",
            "devstral-medium-latest",
            "Devstral 2 (latest)",
            262_144,
            32_768,
            false,
            false,
        ),
        catalog_model(
            "mistral",
            "magistral-small",
            "Magistral Small",
            128_000,
            40_000,
            true,
            false,
        ),
        catalog_model(
            "mistral",
            "mistral-large-latest",
            "Mistral Large (latest)",
            262_144,
            16_384,
            false,
            true,
        ),
        catalog_model(
            "mistral",
            "mistral-medium-2508",
            "Mistral Medium 3.1",
            262_144,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "mistral",
            "mistral-small-latest",
            "Mistral Small (latest)",
            128_000,
            16_384,
            true,
            true,
        ),
        catalog_model(
            "mistral",
            "pixtral-large-latest",
            "Pixtral Large (latest)",
            128_000,
            32_768,
            false,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.6",
            "Kimi K2.6",
            262_144,
            262_144,
            false,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2.5",
            "Kimi K2.5",
            262_144,
            262_144,
            false,
            true,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2-thinking",
            "Kimi K2 Thinking",
            262_144,
            262_144,
            true,
            false,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2-thinking-turbo",
            "Kimi K2 Thinking Turbo",
            262_144,
            262_144,
            true,
            false,
        ),
        catalog_model(
            "moonshot",
            "kimi-k2-turbo",
            "Kimi K2 Turbo",
            256_000,
            16_384,
            false,
            false,
        ),
        catalog_model(
            "nvidia",
            "nvidia/nemotron-3-super-120b-a12b",
            "NVIDIA Nemotron 3 Super 120B",
            262_144,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "nvidia",
            "moonshotai/kimi-k2.5",
            "Kimi K2.5",
            262_144,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "nvidia",
            "minimaxai/minimax-m2.5",
            "MiniMax M2.5",
            196_608,
            8_192,
            false,
            false,
        ),
        catalog_model("nvidia", "z-ai/glm5", "GLM-5", 202_752, 8_192, false, false),
        catalog_model(
            "opencode-go",
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "opencode-go",
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            1_000_000,
            384_000,
            true,
            false,
        ),
        catalog_model(
            "openrouter",
            "auto",
            "OpenRouter Auto",
            200_000,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "openrouter",
            "openrouter/hunter-alpha",
            "Hunter Alpha",
            1_048_576,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "openrouter",
            "openrouter/healer-alpha",
            "Healer Alpha",
            262_144,
            65_536,
            true,
            true,
        ),
        catalog_model(
            "openrouter",
            "moonshotai/kimi-k2.6",
            "MoonshotAI: Kimi K2.6",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "qianfan",
            "deepseek-v3.2",
            "DEEPSEEK V3.2",
            98_304,
            32_768,
            true,
            false,
        ),
        catalog_model(
            "qianfan",
            "ernie-5.0-thinking-preview",
            "ERNIE-5.0-Thinking-Preview",
            119_000,
            64_000,
            true,
            true,
        ),
        catalog_model(
            "qwen",
            "qwen3.5-plus",
            "qwen3.5-plus",
            1_000_000,
            65_536,
            false,
            true,
        ),
        catalog_model(
            "qwen",
            "qwen3.6-plus",
            "qwen3.6-plus",
            1_000_000,
            65_536,
            false,
            true,
        ),
        catalog_model(
            "qwen",
            "qwen3-max-2026-01-23",
            "qwen3-max-2026-01-23",
            262_144,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "qwen",
            "qwen3-coder-next",
            "qwen3-coder-next",
            262_144,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "qwen",
            "qwen3-coder-plus",
            "qwen3-coder-plus",
            1_000_000,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "qwen",
            "MiniMax-M2.5",
            "MiniMax-M2.5",
            1_000_000,
            65_536,
            true,
            false,
        ),
        catalog_model("qwen", "glm-5", "glm-5", 202_752, 16_384, false, false),
        catalog_model("qwen", "glm-4.7", "glm-4.7", 202_752, 16_384, false, false),
        catalog_model(
            "qwen",
            "kimi-k2.5",
            "kimi-k2.5",
            262_144,
            32_768,
            false,
            true,
        ),
        catalog_model(
            "stepfun",
            "step-3.5-flash",
            "Step 3.5 Flash",
            262_144,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "stepfun-plan",
            "step-3.5-flash",
            "Step 3.5 Flash",
            262_144,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "stepfun-plan",
            "step-3.5-flash-2603",
            "Step 3.5 Flash 2603",
            262_144,
            65_536,
            true,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:MiniMaxAI/MiniMax-M2.5",
            "MiniMax M2.5",
            192_000,
            65_536,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:moonshotai/Kimi-K2-Thinking",
            "Kimi K2 Thinking",
            256_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:zai-org/GLM-4.7",
            "GLM-4.7",
            198_000,
            128_000,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:deepseek-ai/DeepSeek-R1-0528",
            "DeepSeek R1 0528",
            128_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:deepseek-ai/DeepSeek-V3-0324",
            "DeepSeek V3 0324",
            128_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:deepseek-ai/DeepSeek-V3.1",
            "DeepSeek V3.1",
            128_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:deepseek-ai/DeepSeek-V3.1-Terminus",
            "DeepSeek V3.1 Terminus",
            128_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:deepseek-ai/DeepSeek-V3.2",
            "DeepSeek V3.2",
            159_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:meta-llama/Llama-3.3-70B-Instruct",
            "Llama 3.3 70B Instruct",
            128_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8",
            "Llama 4 Maverick 17B 128E Instruct FP8",
            524_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:moonshotai/Kimi-K2-Instruct-0905",
            "Kimi K2 Instruct 0905",
            256_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:moonshotai/Kimi-K2.5",
            "Kimi K2.5",
            256_000,
            8_192,
            true,
            true,
        ),
        catalog_model(
            "synthetic",
            "hf:openai/gpt-oss-120b",
            "GPT OSS 120B",
            128_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:Qwen/Qwen3-235B-A22B-Instruct-2507",
            "Qwen3 235B A22B Instruct 2507",
            256_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:Qwen/Qwen3-Coder-480B-A35B-Instruct",
            "Qwen3 Coder 480B A35B Instruct",
            256_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:Qwen/Qwen3-VL-235B-A22B-Instruct",
            "Qwen3 VL 235B A22B Instruct",
            250_000,
            8_192,
            false,
            true,
        ),
        catalog_model(
            "synthetic",
            "hf:zai-org/GLM-4.5",
            "GLM-4.5",
            128_000,
            128_000,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:zai-org/GLM-4.6",
            "GLM-4.6",
            198_000,
            128_000,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:zai-org/GLM-5",
            "GLM-5",
            256_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "synthetic",
            "hf:deepseek-ai/DeepSeek-V3",
            "DeepSeek V3",
            128_000,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "synthetic",
            "hf:Qwen/Qwen3-235B-A22B-Thinking-2507",
            "Qwen3 235B A22B Thinking 2507",
            256_000,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "tencent-tokenhub",
            "hy3-preview",
            "Hy3 preview (TokenHub)",
            256_000,
            64_000,
            true,
            false,
        ),
        catalog_model(
            "together",
            "zai-org/GLM-4.7",
            "GLM 4.7 Fp8",
            202_752,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "together",
            "moonshotai/Kimi-K2.5",
            "Kimi K2.5",
            262_144,
            32_768,
            true,
            true,
        ),
        catalog_model(
            "together",
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "Llama 3.3 70B Instruct Turbo",
            131_072,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "together",
            "meta-llama/Llama-4-Scout-17B-16E-Instruct",
            "Llama 4 Scout 17B 16E Instruct",
            10_000_000,
            32_768,
            false,
            true,
        ),
        catalog_model(
            "together",
            "meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8",
            "Llama 4 Maverick 17B 128E Instruct FP8",
            20_000_000,
            32_768,
            false,
            true,
        ),
        catalog_model(
            "together",
            "deepseek-ai/DeepSeek-V3.1",
            "DeepSeek V3.1",
            131_072,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "together",
            "deepseek-ai/DeepSeek-R1",
            "DeepSeek R1",
            131_072,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "together",
            "moonshotai/Kimi-K2-Instruct-0905",
            "Kimi K2-Instruct 0905",
            262_144,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "venice",
            "claude-opus-4-6",
            "Claude Opus 4.6 (via Venice)",
            1_000_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "venice",
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6 (via Venice)",
            1_000_000,
            128_000,
            true,
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
            200_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "openai/gpt-5.4-pro",
            "GPT 5.4 Pro",
            200_000,
            128_000,
            true,
            true,
        ),
        catalog_model(
            "vercel-ai-gateway",
            "moonshotai/kimi-k2.6",
            "Kimi K2.6",
            262_144,
            262_144,
            true,
            true,
        ),
        catalog_model(
            "vllm",
            "meta-llama/Meta-Llama-3-8B-Instruct",
            "Meta Llama 3 8B Instruct",
            131_072,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "volcengine",
            "doubao-seed-code-preview-251028",
            "doubao-seed-code-preview-251028",
            256_000,
            4_096,
            false,
            true,
        ),
        catalog_model(
            "volcengine",
            "doubao-seed-1-8-251228",
            "Doubao Seed 1.8",
            256_000,
            4_096,
            false,
            true,
        ),
        catalog_model(
            "volcengine",
            "deepseek-v3-2-251201",
            "DeepSeek V3.2",
            128_000,
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
            "doubao-seed-code-preview-251028",
            "Doubao Seed Code Preview",
            256_000,
            4_096,
            false,
            false,
        ),
        catalog_model(
            "xiaomi",
            "mimo-v2-flash",
            "Xiaomi MiMo V2 Flash",
            262_144,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "xiaomi",
            "mimo-v2-pro",
            "Xiaomi MiMo V2 Pro",
            1_048_576,
            32_000,
            true,
            false,
        ),
        catalog_model(
            "xiaomi",
            "mimo-v2-omni",
            "Xiaomi MiMo V2 Omni",
            262_144,
            32_000,
            true,
            true,
        ),
        catalog_model("xai", "grok-3", "Grok 3", 131_072, 8_192, false, false),
        catalog_model(
            "xai",
            "grok-3-fast",
            "Grok 3 Fast",
            131_072,
            8_192,
            false,
            false,
        ),
        catalog_model(
            "xai",
            "grok-3-mini",
            "Grok 3 Mini",
            131_072,
            8_192,
            true,
            false,
        ),
        catalog_model(
            "xai",
            "grok-3-mini-fast",
            "Grok 3 Mini Fast",
            131_072,
            8_192,
            true,
            false,
        ),
        catalog_model("xai", "grok-4", "Grok 4", 256_000, 64_000, true, false),
        catalog_model(
            "xai",
            "grok-4-fast",
            "Grok 4 Fast",
            2_000_000,
            30_000,
            true,
            true,
        ),
        catalog_model(
            "xai",
            "grok-4-fast-non-reasoning",
            "Grok 4 Fast (Non-Reasoning)",
            2_000_000,
            30_000,
            false,
            true,
        ),
        catalog_model(
            "xai",
            "grok-4-1-fast",
            "Grok 4.1 Fast",
            2_000_000,
            30_000,
            true,
            true,
        ),
        catalog_model(
            "xai",
            "grok-code-fast-1",
            "Grok Code Fast 1",
            256_000,
            10_000,
            true,
            false,
        ),
        catalog_model("zai", "glm-5.1", "GLM-5.1", 202_800, 131_100, true, false),
        catalog_model("zai", "glm-5", "GLM-5", 202_800, 131_100, true, false),
        catalog_model(
            "zai",
            "glm-5-turbo",
            "GLM-5 Turbo",
            202_800,
            131_100,
            true,
            false,
        ),
        catalog_model(
            "zai",
            "glm-5v-turbo",
            "GLM-5V Turbo",
            202_800,
            131_100,
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
            "glm-4.5-flash",
            "GLM-4.5 Flash",
            131_072,
            98_304,
            true,
            false,
        ),
        catalog_model("zai", "glm-4.5v", "GLM-4.5V", 64_000, 16_384, true, true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_context() -> ContextConfig {
        ContextConfig {
            recent_messages: 12,
            recent_briefs: 8,
            compaction_trigger_messages: 20,
            compaction_keep_recent_messages: 8,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
        }
    }

    #[test]
    fn resolves_built_in_policy_from_known_model() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::openai_codex(), "gpt-5.4"),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.context_window_tokens, Some(272_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, 258_400);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 232_560);
        assert_eq!(policy.source, ModelMetadataSource::BuiltInCatalog);
    }

    #[test]
    fn resolves_built_in_policy_for_codex_spark_benchmark_model() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::openai_codex(), "gpt-5.3-codex-spark"),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.context_window_tokens, Some(128_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, 121_600);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 109_440);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 41_587);
        assert_eq!(policy.source, ModelMetadataSource::BuiltInCatalog);
    }

    #[test]
    fn resolves_compatible_provider_model_policy() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::parse("deepseek/deepseek-v4-flash").unwrap(),
            &HashMap::new(),
            None,
            &base_context(),
            8192,
        );

        assert_eq!(policy.display_name, "DeepSeek V4 Flash");
        assert_eq!(policy.context_window_tokens, Some(1_000_000));
        assert_eq!(policy.prompt_budget_estimated_tokens, 950_000);
        assert_eq!(policy.runtime_max_output_tokens, 384_000);
        assert!(policy.capabilities.reasoning_summaries);
        assert_eq!(policy.source, ModelMetadataSource::BuiltInCatalog);
    }

    #[test]
    fn resolves_unknown_model_from_explicit_fallback() {
        let catalog = BuiltInModelCatalog::new();
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::openai(), "custom-model"),
            &HashMap::new(),
            Some(&ModelRuntimeOverride {
                prompt_budget_estimated_tokens: Some(64_000),
                compaction_trigger_estimated_tokens: Some(48_000),
                compaction_keep_recent_estimated_tokens: Some(24_000),
                ..ModelRuntimeOverride::default()
            }),
            &base_context(),
            8192,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, 64_000);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 48_000);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 24_000);
        assert_eq!(policy.source, ModelMetadataSource::UnknownFallback);
    }

    #[test]
    fn model_override_can_replace_known_context_budget_fields() {
        let catalog = BuiltInModelCatalog::new();
        let mut overrides = HashMap::new();
        overrides.insert(
            ModelRef::new(ProviderId::anthropic(), "claude-sonnet-4-6"),
            ModelRuntimeOverride {
                prompt_budget_estimated_tokens: Some(32_000),
                runtime_max_output_tokens: Some(4_096),
                ..ModelRuntimeOverride::default()
            },
        );
        let policy = catalog.resolve_policy(
            &ModelRef::new(ProviderId::anthropic(), "claude-sonnet-4-6"),
            &overrides,
            None,
            &base_context(),
            8192,
        );
        assert_eq!(policy.prompt_budget_estimated_tokens, 32_000);
        assert_eq!(policy.runtime_max_output_tokens, 4_096);
        assert_eq!(policy.source, ModelMetadataSource::ConfigOverride);
    }
}
