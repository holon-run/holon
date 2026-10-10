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
    pub(crate) resolved_image_input: bool,
    pub(crate) provider: Arc<dyn AgentProvider>,
}

pub fn build_provider_from_config(config: &AppConfig) -> Result<Arc<dyn AgentProvider>> {
    build_provider_from_model_chain(config, &config.provider_chain())
}

pub fn build_provider_from_model_chain(
    config: &AppConfig,
    provider_chain: &[ModelRouteRef],
) -> Result<Arc<dyn AgentProvider>> {
    build_provider_from_model_chain_with_override(config, provider_chain, None)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelRouteParameterOverride<'a> {
    pub(crate) route_ref: &'a ModelRouteRef,
    pub(crate) reasoning_effort: Option<&'a str>,
    pub(crate) service_tier: Option<crate::config::ServiceTier>,
}

pub(crate) fn build_provider_from_model_chain_with_override(
    config: &AppConfig,
    provider_chain: &[ModelRouteRef],
    parameter_override: Option<ModelRouteParameterOverride<'_>>,
) -> Result<Arc<dyn AgentProvider>> {
    let mut candidates = Vec::new();
    let mut errors = Vec::new();
    let disable_fallback = config.provider_fallback_disabled();

    for model_ref in provider_chain.iter().take(if disable_fallback {
        1
    } else {
        provider_chain.len()
    }) {
        match build_candidate_with_override(config, model_ref, parameter_override) {
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
    build_candidate_with_override(config, route_ref, None)
}

fn build_candidate_with_override(
    config: &AppConfig,
    route_ref: &ModelRouteRef,
    parameter_override: Option<ModelRouteParameterOverride<'_>>,
) -> Result<ProviderCandidate> {
    let mut route =
        resolve_explicit_model_route_for_candidate(config, route_ref, ModelRouteCapability::Turn)?;
    apply_parameter_override(config, &mut route, parameter_override);
    build_candidate_from_model_route(&config.home_dir, &route)
}

fn apply_parameter_override(
    config: &AppConfig,
    route: &mut ResolvedModelRoute,
    parameter_override: Option<ModelRouteParameterOverride<'_>>,
) {
    if let Some(options) = parameter_override.filter(|options| {
        RuntimeModelCatalog::from_config(config).canonicalize_model_route_ref(options.route_ref)
            == route.route_ref
    }) {
        if let Some(effort) = options.reasoning_effort {
            route.endpoint.runtime_config.reasoning_effort = Some(effort.to_string());
        }
        if let Some(tier) = options.service_tier {
            route.service_tier = Some(tier);
        }
    }
}

pub(crate) fn build_candidate_from_model_route(
    home_dir: &Path,
    route: &ResolvedModelRoute,
) -> Result<ProviderCandidate> {
    if let Some(tier) = route.service_tier {
        route.validate_service_tier(tier)?;
    }
    let provider_config = route.provider_config();
    if let Some(reasoning_effort) = provider_config.reasoning_effort.as_deref() {
        route.validate_reasoning_effort(reasoning_effort)?;
    }
    let provider = build_provider_for_route(home_dir, route)?;
    Ok(ProviderCandidate {
        model_ref: route.route_ref.as_string(),
        provider_name: route.provider_name().to_string(),
        resolved_image_input: route.capabilities.image_input,
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
            user_home_dir: None,
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
            auth: Default::default(),
            api_cors: Default::default(),
            api_projection: Default::default(),
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
            command_task_output_retention_bytes: 8 * 1024 * 1024,
            command_task_output_quota_bytes: 64 * 1024 * 1024,
            command_task_min_free_disk_bytes: 512 * 1024 * 1024,
            command_task_min_free_disk_percent: 5,
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

    #[test]
    fn model_route_effort_override_does_not_filter_same_provider_fallback() {
        let mut config = codex_config("gpt-5.6-luna", "medium");
        let primary = config.default_model.clone();
        let fallback = ModelRouteRef::parse_compatible("openai-codex/gpt-5.5").unwrap();
        config.fallback_models.push(fallback.clone());

        let provider = build_provider_from_model_chain_with_override(
            &config,
            &config.provider_chain(),
            Some(ModelRouteParameterOverride {
                route_ref: &primary,
                reasoning_effort: Some("max"),
                service_tier: None,
            }),
        )
        .expect("route-scoped effort should not affect the fallback model");

        assert_eq!(
            provider.configured_model_refs(),
            vec![primary.as_string(), fallback.as_string()]
        );
    }

    #[test]
    fn provider_effort_still_filters_each_model_candidate() {
        let mut config = codex_config("gpt-5.6-luna", "max");
        let primary = config.default_model.clone();
        config.fallback_models =
            vec![ModelRouteRef::parse_compatible("openai-codex/gpt-5.5").unwrap()];

        let provider = build_provider_from_model_chain(&config, &config.provider_chain())
            .expect("the compatible primary should remain available");
        assert_eq!(provider.configured_model_refs(), vec![primary.as_string()]);

        config.default_model = ModelRouteRef::parse_compatible("openai-codex/gpt-5.5").unwrap();
        config.fallback_models.clear();
        let error = build_provider_from_model_chain(&config, &config.provider_chain())
            .err()
            .expect("an incompatible provider effort should reject every candidate");
        assert!(error
            .to_string()
            .contains("no available providers for configured model chain"));
    }

    #[test]
    fn model_route_effort_override_matches_legacy_model_alias() {
        let mut config = codex_config("gpt-5.6-luna", "medium");
        let mistral = crate::config::ProviderId::parse("mistral").unwrap();
        let mut provider = config
            .providers
            .get(&crate::config::ProviderId::openai())
            .unwrap()
            .clone();
        provider.id = mistral.clone();
        provider.route_provider = mistral.clone();
        config.providers.insert(mistral.clone(), provider);
        let alias = ModelRouteRef::new(
            mistral,
            crate::config::ProviderEndpointId::default_endpoint(),
            "devstral-medium-latest",
        );
        config.default_model = alias.clone();

        let mut route =
            resolve_explicit_model_route_for_candidate(&config, &alias, ModelRouteCapability::Turn)
                .expect("the legacy alias should resolve");
        apply_parameter_override(
            &config,
            &mut route,
            Some(ModelRouteParameterOverride {
                route_ref: &alias,
                reasoning_effort: Some("high"),
                service_tier: None,
            }),
        );

        assert_eq!(
            route.endpoint.runtime_config.reasoning_effort.as_deref(),
            Some("high")
        );
    }
    #[test]
    fn service_tier_is_scoped_to_the_exact_route_and_never_filters_fallbacks() {
        use crate::config::{ModelRouteOptions, ServiceTier};
        let mut config = codex_config("gpt-6-astra", "low");
        let primary = config.default_model.clone();
        let fallback = ModelRouteRef::parse("openai-codex@default/gpt-6-sol").unwrap();
        config.fallback_models = vec![fallback.clone()];
        config.stored_config.model.route_options.insert(
            primary.as_string(),
            ModelRouteOptions {
                service_tier: Some(ServiceTier::Fast),
            },
        );
        config.validate_model_route_options().unwrap();
        let catalog = RuntimeModelCatalog::from_config(&config);
        let options = Some(ModelRouteParameterOverride {
            route_ref: &primary,
            reasoning_effort: None,
            service_tier: Some(ServiceTier::Default),
        });
        let mut selected = resolve_explicit_model_route_for_candidate(
            &config,
            &primary,
            ModelRouteCapability::Turn,
        )
        .unwrap();
        let mut other = resolve_explicit_model_route_for_candidate(
            &config,
            &fallback,
            ModelRouteCapability::Turn,
        )
        .unwrap();
        assert_eq!(selected.service_tier, Some(ServiceTier::Fast));
        assert_eq!(catalog.route_service_tier(&fallback), None);
        apply_parameter_override(&config, &mut selected, options);
        apply_parameter_override(&config, &mut other, options);
        assert_eq!(selected.service_tier, Some(ServiceTier::Default));
        assert_eq!(other.service_tier, None);
        let mut other_endpoint = selected.clone();
        other_endpoint.route_ref.endpoint =
            crate::config::ProviderEndpointId::parse("other").unwrap();
        other_endpoint.service_tier = None;
        apply_parameter_override(&config, &mut other_endpoint, options);
        assert_eq!(other_endpoint.service_tier, None);
        assert_eq!(
            build_provider_from_model_chain_with_override(
                &config,
                &config.provider_chain(),
                options
            )
            .unwrap()
            .configured_model_refs(),
            vec![primary.as_string(), fallback.as_string()]
        );
        // A fallback owns its own explicit choice rather than borrowing the primary's.
        config.stored_config.model.route_options.insert(
            fallback.as_string(),
            ModelRouteOptions {
                service_tier: Some(ServiceTier::Fast),
            },
        );
        let mut other = resolve_explicit_model_route_for_candidate(
            &config,
            &fallback,
            ModelRouteCapability::Turn,
        )
        .unwrap();
        apply_parameter_override(&config, &mut other, options);
        assert_eq!(other.service_tier, Some(ServiceTier::Fast));
    }

    #[test]
    fn service_tier_capability_requires_a_supported_model_and_official_endpoint() {
        use crate::config::{service_tier_supported, ModelRouteOptions, ServiceTier};
        let mut config = codex_config("gpt-6-astra", "low");
        let primary = config.default_model.clone();
        let codex = config
            .providers
            .get(&crate::config::ProviderId::openai_codex())
            .unwrap()
            .clone();
        assert!(service_tier_supported(&primary, &codex));
        let mut proxy = codex.clone();
        proxy.base_url = "https://gateway.example/v1".into();
        assert!(!service_tier_supported(&primary, &proxy));
        let mut unsupported = primary.clone();
        unsupported.model = "gpt-unknown".into();
        assert!(!service_tier_supported(&unsupported, &codex));
        let openai = config
            .providers
            .get(&crate::config::ProviderId::openai())
            .unwrap();
        let api_route = ModelRouteRef::parse("openai@default/gpt-6-sol").unwrap();
        assert!(service_tier_supported(&api_route, openai));
        let mut third_party = api_route.clone();
        third_party.provider = crate::config::ProviderId::parse("compatible").unwrap();
        assert!(!service_tier_supported(&third_party, openai));
        config
            .providers
            .get_mut(&crate::config::ProviderId::openai_codex())
            .unwrap()
            .base_url = proxy.base_url;
        config.stored_config.model.route_options.insert(
            primary.as_string(),
            ModelRouteOptions {
                service_tier: Some(ServiceTier::Fast),
            },
        );
        assert!(config
            .validate_model_route_options()
            .unwrap_err()
            .to_string()
            .contains("does not support service_tier"));
    }
}
