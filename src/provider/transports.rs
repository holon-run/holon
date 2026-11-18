//! App configuration and compatibility constructors for the provider adapters.
use std::{collections::HashMap, path::Path};

use anyhow::Result;
use async_trait::async_trait;
use holon_provider::provider::transports as adapters;

use crate::{
    config::{
        AppConfig, ModelRef, ModelRouteRef, ProviderId, ProviderRuntimeConfig, RuntimeModelCatalog,
        ServiceTier,
    },
    context::ContextConfig,
    model_catalog::{ModelRuntimeOverride, ModelVerbosity},
};

use holon_domain::ToolSpec;

use super::*;

pub(crate) use adapters::OpenAiCompactionPolicy;
#[cfg(test)]
pub(crate) use adapters::OpenAiResponsesTransportContract;

const HOLON_VERSION: &str = env!("CARGO_PKG_VERSION");

macro_rules! delegate_provider {
    ($name:ident) => {
        #[derive(Clone)]
        pub struct $name(adapters::$name);

        #[async_trait]
        impl AgentProvider for $name {
            async fn complete_turn(
                &self,
                request: ProviderTurnRequest,
            ) -> Result<ProviderTurnResponse> {
                self.0.complete_turn(request).await
            }

            fn quota_identity(&self) -> Option<ProviderQuotaIdentity> {
                self.0.quota_identity()
            }

            fn select_model_lineage(
                &self,
                model_ref: &ModelRouteRef,
            ) -> Option<std::sync::Arc<dyn AgentProvider>> {
                self.0.select_model_lineage(model_ref)
            }

            async fn generate_image(
                &self,
                request: ProviderGenerateImageRequest,
            ) -> Result<ProviderGenerateImageResponse> {
                self.0.generate_image(request).await
            }

            fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
                self.0.prompt_capabilities()
            }

            fn supports_freeform_grammar_tools(&self) -> bool {
                self.0.supports_freeform_grammar_tools()
            }

            fn prompt_tool_specs(&self, tools: &[ToolSpec]) -> Vec<ToolSpec> {
                self.0.prompt_tool_specs(tools)
            }

            fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
                self.0.builtin_web_search()
            }

            async fn probe_builtin_web_search(
                &self,
                request: ProviderNativeWebSearchRequest,
            ) -> Result<()> {
                self.0.probe_builtin_web_search(request).await
            }

            fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
                self.0.context_management_policy()
            }

            fn resolved_image_input_support(&self) -> Option<bool> {
                self.0.resolved_image_input_support()
            }

            async fn complete_turn_with_diagnostics(
                &self,
                request: ProviderTurnRequest,
            ) -> Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)> {
                self.0.complete_turn_with_diagnostics(request).await
            }

            #[cfg(test)]
            fn configured_model_refs(&self) -> Vec<String> {
                self.0.configured_model_refs()
            }
        }
    };
}

delegate_provider!(OpenAiProvider);

impl OpenAiProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        let policy = openai_model_policy_from_config(config, ProviderId::openai(), model);
        Self::from_runtime_config_with_compaction_policy_and_context_window(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            policy.context_window_tokens,
            &config.home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        let policy =
            openai_model_policy_for_runtime_config(provider_config, model, max_output_tokens);
        Self::from_runtime_config_with_compaction_policy_and_context_window(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            policy.context_window_tokens,
            trace_home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
        )
    }

    pub(crate) fn from_runtime_config_with_compaction_policy_and_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
        compaction_policy: OpenAiCompactionPolicy,
    ) -> Result<Self> {
        adapters::OpenAiProvider::from_runtime_config_with_compaction_policy_and_context_window(
            provider_config,
            model,
            max_output_tokens,
            context_window_tokens,
            trace_home_dir,
            compaction_policy,
            HOLON_VERSION,
        )
        .map(Self)
    }

    pub(crate) fn with_service_tier(self, tier: Option<ServiceTier>) -> Self {
        Self(self.0.with_service_tier(tier))
    }
}

delegate_provider!(OpenAiChatCompletionsProvider);

impl OpenAiChatCompletionsProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        let policy = openai_model_policy_from_config(config, ProviderId::openai(), model);
        Self::from_resolved_runtime_config_with_context_window(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            policy.context_window_tokens,
            &config.home_dir,
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        let policy =
            openai_model_policy_for_runtime_config(provider_config, model, max_output_tokens);
        Self::from_resolved_runtime_config_with_context_window(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            policy.context_window_tokens,
            trace_home_dir,
        )
    }

    pub(crate) fn from_resolved_runtime_config_with_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        resolved_max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        adapters::OpenAiChatCompletionsProvider::from_resolved_runtime_config_with_context_window(
            provider_config,
            model,
            resolved_max_output_tokens,
            context_window_tokens,
            trace_home_dir,
            HOLON_VERSION,
        )
        .map(Self)
    }

    pub(crate) fn with_service_tier(self, tier: Option<ServiceTier>) -> Self {
        Self(self.0.with_service_tier(tier))
    }
}

delegate_provider!(OpenAiCodexProvider);

impl OpenAiCodexProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai_codex())
            .ok_or_else(|| anyhow::anyhow!("missing openai-codex provider config"))?;
        let policy = openai_model_policy_from_config(config, ProviderId::openai_codex(), model);
        Self::from_runtime_config_with_compaction_policy_and_context_window(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            policy.context_window_tokens,
            &config.home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
            policy.verbosity,
            policy.capabilities.supports_reasoning,
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        supports_reasoning: bool,
    ) -> Result<Self> {
        let policy =
            openai_model_policy_for_runtime_config(provider_config, model, max_output_tokens);
        Self::from_runtime_config_with_compaction_policy_and_context_window(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            policy.context_window_tokens,
            trace_home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
            policy.verbosity,
            supports_reasoning,
        )
    }

    pub(crate) fn from_runtime_config_with_compaction_policy_and_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
        compaction_policy: OpenAiCompactionPolicy,
        verbosity: Option<ModelVerbosity>,
        supports_reasoning: bool,
    ) -> Result<Self> {
        adapters::OpenAiCodexProvider::from_runtime_config_with_compaction_policy_and_context_window(provider_config, model, max_output_tokens, context_window_tokens, trace_home_dir, compaction_policy, verbosity, supports_reasoning, HOLON_VERSION).map(Self)
    }

    pub(crate) fn with_service_tier(self, tier: Option<ServiceTier>) -> Self {
        Self(self.0.with_service_tier(tier))
    }
}

delegate_provider!(AnthropicProvider);

impl AnthropicProvider {
    pub fn from_config(config: &AppConfig) -> Result<Self> {
        Self::from_config_with_model(config, "claude-sonnet-4-6")
    }

    pub fn from_config_with_model(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::anthropic())
            .ok_or_else(|| anyhow::anyhow!("missing anthropic provider config"))?;
        Self::from_runtime_config(
            provider_config,
            model,
            config.runtime_max_output_tokens,
            &config.home_dir,
            true,
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        supports_reasoning: bool,
    ) -> Result<Self> {
        adapters::AnthropicProvider::from_runtime_config(
            provider_config,
            model,
            max_output_tokens,
            trace_home_dir,
            supports_reasoning,
            HOLON_VERSION,
        )
        .map(Self)
    }

    pub fn from_runtime_config_with_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
        supports_reasoning: bool,
    ) -> Result<Self> {
        adapters::AnthropicProvider::from_runtime_config_with_context_window(
            provider_config,
            model,
            max_output_tokens,
            context_window_tokens,
            trace_home_dir,
            supports_reasoning,
            HOLON_VERSION,
        )
        .map(Self)
    }

    pub(crate) fn with_cache_policy(
        self,
        policy: crate::model_catalog::ResolvedAnthropicCachePolicy,
        model_route: String,
    ) -> Self {
        Self(self.0.with_cache_policy(policy, model_route))
    }
}

delegate_provider!(GeminiProvider);

impl GeminiProvider {
    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        adapters::GeminiProvider::from_runtime_config(
            provider_config,
            model,
            max_output_tokens,
            trace_home_dir,
            HOLON_VERSION,
        )
        .map(Self)
    }

    pub fn from_runtime_config_with_context_window(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        context_window_tokens: Option<usize>,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        adapters::GeminiProvider::from_runtime_config_with_context_window(
            provider_config,
            model,
            max_output_tokens,
            context_window_tokens,
            trace_home_dir,
            HOLON_VERSION,
        )
        .map(Self)
    }
}

fn openai_model_policy_from_config(
    config: &AppConfig,
    provider: ProviderId,
    model: &str,
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
    let route_ref = config
        .providers
        .get(&provider)
        .map(|provider_config| {
            ModelRouteRef::new(
                provider_config.route_provider.clone(),
                provider_config.route_endpoint.clone(),
                model,
            )
        })
        .unwrap_or_else(|| ModelRouteRef::from_legacy_model_ref(&ModelRef::new(provider, model)));
    RuntimeModelCatalog::from_config(config)
        .resolved_model_policy(&base_context_config, Some(&route_ref))
}

fn openai_model_policy_for_runtime_config(
    provider_config: &ProviderRuntimeConfig,
    model: &str,
    max_output_tokens: u32,
) -> crate::model_catalog::ResolvedRuntimeModelPolicy {
    let model_ref = ModelRef::new(provider_config.route_provider.clone(), model);
    let route_ref = ModelRouteRef::new(
        provider_config.route_provider.clone(),
        provider_config.route_endpoint.clone(),
        model,
    );
    let mut model_overrides = HashMap::new();
    model_overrides.insert(
        model_ref,
        ModelRuntimeOverride {
            runtime_max_output_tokens: Some(max_output_tokens),
            ..ModelRuntimeOverride::default()
        },
    );
    RuntimeModelCatalog {
        model_overrides,
        configured_runtime_max_output_tokens: max_output_tokens,
        ..RuntimeModelCatalog::default()
    }
    .resolved_model_policy(&ContextConfig::default(), Some(&route_ref))
}

#[cfg(all(test, holon_lib_test_shard = "c"))]
mod tests {
    use super::*;
    use crate::config::{ProviderEndpointId, ProviderTransportKind};
    #[test]
    fn runtime_config_policy_preserves_exact_route_and_explicit_output_limit() {
        let mut config = ProviderRuntimeConfig {
            id: ProviderId::openai_codex(),
            route_provider: ProviderId::openai_codex(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://example.invalid".into(),
            auth: Default::default(),
            headers: Default::default(),
            credential: Some("credential".into()),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        };
        config.route_provider = ProviderId::parse("volcengine").unwrap();
        config.route_endpoint = ProviderEndpointId::parse("plan").unwrap();

        let clamped = openai_model_policy_for_runtime_config(&config, "glm-5.2", 200_000);
        assert_eq!(clamped.runtime_max_output_tokens, 128_000);
        assert_eq!(clamped.reasoning_effort_options, ["low", "medium", "high"]);

        let explicit = openai_model_policy_for_runtime_config(&config, "glm-5.2", 1_024);
        assert_eq!(explicit.runtime_max_output_tokens, 1_024);
    }
}

#[cfg(all(test, holon_lib_test_shard = "c"))]
#[path = "transports/openai/oauth_material_tests.rs"]
mod oauth_material_tests;
