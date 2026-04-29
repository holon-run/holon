use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::{
    config::{AppConfig, ModelRef, ProviderTransportKind},
    provider::fallback::FallbackProvider,
};

use super::{
    transports::{OpenAiChatCompletionsProvider, OpenAiCodexProvider, OpenAiProvider},
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
    let provider: Arc<dyn AgentProvider> = match provider_config.transport {
        ProviderTransportKind::OpenAiCodexResponses => {
            Arc::new(OpenAiCodexProvider::from_runtime_config(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
            )?)
        }
        ProviderTransportKind::OpenAiResponses => Arc::new(OpenAiProvider::from_runtime_config(
            provider_config,
            &model_ref.model,
            config.runtime_max_output_tokens,
        )?),
        ProviderTransportKind::AnthropicMessages => {
            Arc::new(AnthropicProvider::from_runtime_config(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
            )?)
        }
        ProviderTransportKind::OpenAiChatCompletions => {
            Arc::new(OpenAiChatCompletionsProvider::from_runtime_config(
                provider_config,
                &model_ref.model,
                config.runtime_max_output_tokens,
            )?)
        }
    };
    Ok(ProviderCandidate {
        model_ref: model_ref.as_string(),
        provider_name: model_ref.provider.as_str().to_string(),
        provider,
    })
}
