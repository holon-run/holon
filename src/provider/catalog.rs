use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::{
    config::{AppConfig, ModelRef, ProviderTransportKind},
    context::ContextConfig,
    model_catalog::BuiltInModelCatalog,
    provider::fallback::FallbackProvider,
};

use super::{
    transports::{
        GeminiProvider, OpenAiChatCompletionsProvider, OpenAiCodexProvider, OpenAiCompactionPolicy,
        OpenAiProvider,
    },
    AgentProvider, AnthropicProvider,
};

#[derive(Clone)]
pub(crate) struct ProviderCandidate {
    pub(crate) model_ref: String,
    pub(crate) provider_name: String,
    pub(crate) provider: Arc<dyn AgentProvider>,
}

pub fn build_provider_from_config(config: &AppConfig) -> Result<Arc<dyn AgentProvider>> {
    build_provider_from_model_chain(config, &config.provider_chain())
}

pub fn build_provider_from_model_chain(
    config: &AppConfig,
    provider_chain: &[ModelRef],
) -> Result<Arc<dyn AgentProvider>> {
    let mut candidates = Vec::new();
    let mut errors = Vec::new();
    let disable_fallback = config.provider_fallback_disabled();

    for model_ref in provider_chain.iter().take(if disable_fallback {
        1
    } else {
        provider_chain.len()
    }) {
        match build_candidate(config, model_ref) {
            Ok(candidate) => {
                if !candidates
                    .iter()
                    .any(|existing: &ProviderCandidate| existing.model_ref == candidate.model_ref)
                {
                    candidates.push(candidate);
                }
            }
            Err(err) => errors.push(format!("{}: {err}", model_ref.as_string())),
        }
    }

    match candidates.len() {
        0 => Err(anyhow!(
            "no available providers for configured model chain: {}",
            errors.join("; ")
        )),
        _ => Ok(Arc::new(FallbackProvider { candidates })),
    }
}

pub(crate) fn build_candidate(
    config: &AppConfig,
    model_ref: &ModelRef,
) -> Result<ProviderCandidate> {
    let provider_config = config.providers.get(&model_ref.provider).ok_or_else(|| {
        anyhow!(
            "unknown provider {}; configure providers.{}",
            model_ref.provider.as_str(),
            model_ref.provider.as_str()
        )
    })?;
    let resolved_policy = resolved_model_policy_for_candidate(config, model_ref);
    let openai_compaction_policy = OpenAiCompactionPolicy {
        trigger_input_tokens: resolved_policy.compaction_trigger_estimated_tokens as u64,
    };
    let provider: Arc<dyn AgentProvider> = match provider_config.transport {
        ProviderTransportKind::OpenAiCodexResponses => Arc::new(
            OpenAiCodexProvider::from_runtime_config_with_compaction_policy(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
                &config.home_dir,
                openai_compaction_policy,
                resolved_policy.verbosity,
            )?,
        ),
        ProviderTransportKind::OpenAiResponses => {
            Arc::new(OpenAiProvider::from_runtime_config_with_compaction_policy(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
                &config.home_dir,
                openai_compaction_policy,
            )?)
        }
        ProviderTransportKind::AnthropicMessages => {
            Arc::new(AnthropicProvider::from_runtime_config(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
                &config.home_dir,
            )?)
        }
        ProviderTransportKind::OpenAiChatCompletions => {
            Arc::new(OpenAiChatCompletionsProvider::from_runtime_config(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
                &config.home_dir,
            )?)
        }
        ProviderTransportKind::GeminiGenerateContent => {
            Arc::new(GeminiProvider::from_runtime_config(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
                &config.home_dir,
            )?)
        }
    };
    Ok(ProviderCandidate {
        model_ref: model_ref.as_string(),
        provider_name: model_ref.provider.as_str().to_string(),
        provider,
    })
}

fn resolved_model_policy_for_candidate(
    config: &AppConfig,
    model_ref: &ModelRef,
) -> crate::model_catalog::ResolvedRuntimeModelPolicy {
    let base_context_config = ContextConfig {
        recent_messages: config.context_window_messages,
        recent_briefs: config.context_window_briefs,
        compaction_trigger_messages: config.compaction_trigger_messages,
        compaction_keep_recent_messages: config.compaction_keep_recent_messages,
        prompt_budget_estimated_tokens: config.prompt_budget_estimated_tokens,
        compaction_trigger_estimated_tokens: config.compaction_trigger_estimated_tokens,
        compaction_keep_recent_estimated_tokens: config.compaction_keep_recent_estimated_tokens,
        recent_episode_candidates: config.recent_episode_candidates,
        max_relevant_episodes: config.max_relevant_episodes,
        ..ContextConfig::default()
    };
    BuiltInModelCatalog::default().resolve_policy(
        model_ref,
        &config.validated_model_overrides,
        &config.model_discovery_cache.models(),
        config.validated_unknown_model_fallback.as_ref(),
        &base_context_config,
        config.runtime_max_output_tokens,
    )
}
