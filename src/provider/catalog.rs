use std::{path::Path, sync::Arc};

use anyhow::{anyhow, Result};

use crate::{
    config::{
        AppConfig, ModelRouteCapability, ModelRouteRef, ResolvedModelRoute, RuntimeModelCatalog,
    },
    context::ContextConfig,
    provider::fallback::FallbackProvider,
};

use super::{build_provider_for_route, AgentProvider};

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
    provider_chain: &[ModelRouteRef],
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
    route_ref: &ModelRouteRef,
) -> Result<ProviderCandidate> {
    let route =
        resolve_explicit_model_route_for_candidate(config, route_ref, ModelRouteCapability::Turn)?;
    build_candidate_from_model_route(&config.home_dir, &route)
}

pub(crate) fn build_candidate_from_model_route(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<ProviderCandidate> {
    let provider_config = route.provider_config();
    if let Some(reasoning_effort) = provider_config.reasoning_effort.as_deref() {
        route.validate_reasoning_effort(reasoning_effort)?;
    }
    let provider = build_provider_for_route(home_dir, route)?;
    Ok(ProviderCandidate {
        model_ref: route.route_ref.as_string(),
        provider_name: route.provider_name().to_string(),
        provider,
    })
}

pub(crate) fn resolve_explicit_model_route_for_candidate(
    config: &AppConfig,
    route_ref: &ModelRouteRef,
    requested_capability: ModelRouteCapability,
) -> Result<ResolvedModelRoute> {
    let base_context_config = base_context_config_for_candidate(config);
    RuntimeModelCatalog::from_config(config)
        .resolve_explicit_model_route(&base_context_config, route_ref, requested_capability)
        .ok_or_else(|| {
            anyhow!(
                "provider endpoint {}@{} cannot route model {} for requested route capability {:?}",
                route_ref.provider.as_str(),
                route_ref.endpoint.as_str(),
                route_ref.as_string(),
                requested_capability
            )
        })
}

fn base_context_config_for_candidate(config: &AppConfig) -> ContextConfig {
    ContextConfig {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{provider_registry_for_tests, ControlAuthMode};
    use tempfile::tempdir;

    fn codex_config(model: &str, reasoning_effort: &str) -> AppConfig {
        let home = tempdir().unwrap().keep();
        let workspace = tempdir().unwrap().keep();
        let route = ModelRouteRef::parse_compatible(&format!("openai-codex/{model}")).unwrap();
        let mut providers =
            provider_registry_for_tests(None, None, home.join("missing-codex-home"));
        let provider = providers
            .get_mut(&crate::config::ProviderId::openai_codex())
            .unwrap();
        provider.reasoning_effort = Some(reasoning_effort.into());
        provider.credential = Some(
            r#"{"tokens":{"access_token":"test-token","refresh_token":"test-refresh","account_id":"test-account"}}"#
                .into(),
        );
        AppConfig {
            default_agent_id: "default".into(),
            http_addr: "127.0.0.1:0".into(),
            callback_base_url: "http://127.0.0.1:0".into(),
            home_dir: home.clone(),
            data_dir: home.clone(),
            socket_path: home.join("holon.sock"),
            workspace_dir: workspace,
            context_window_messages: 8,
            context_window_briefs: 8,
            compaction_trigger_messages: 10,
            compaction_keep_recent_messages: 4,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            control_token: Some("secret".into()),
            control_auth_mode: ControlAuthMode::Auto,
            api_cors: Default::default(),
            config_file_path: home.join("config.json"),
            stored_config: Default::default(),
            default_model: route,
            fallback_models: Vec::new(),
            vision_model: None,
            image_generation_model: None,
            vision_candidate_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: Default::default(),
            validated_unknown_model_fallback: None,
            model_discovery_cache: Default::default(),
            providers,
            web_config: Default::default(),
        }
    }

    #[test]
    fn codex_provider_build_validates_effort_against_model_policy() {
        let supported = codex_config("gpt-5.6-luna", "max");
        if let Err(error) = build_provider_from_model_chain(&supported, &supported.provider_chain())
        {
            panic!("supported effort should build provider: {error:#}");
        }

        let unsupported = codex_config("gpt-5.5", "max");
        let error = build_provider_from_model_chain(&unsupported, &unsupported.provider_chain())
            .err()
            .expect("unsupported effort should fail provider construction");
        assert!(error.to_string().contains("openai-codex/gpt-5.5"));
        assert!(error.to_string().contains("low, medium, high, xhigh"));
    }
}
