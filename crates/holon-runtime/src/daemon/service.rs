use anyhow::Result;
use holon_daemon::service::*;

use crate::{
    config::AppConfig, host::RuntimeHost, provider::resolved_route_availability,
    types::AgentStatus, web::WebProviderKind,
};

pub fn activity_from_snapshots(
    agents: &[crate::host::PublicAgentActivitySnapshot],
) -> RuntimeActivitySummary {
    let active_task_count = agents
        .iter()
        .map(|agent| agent.active_task_count)
        .sum::<usize>();
    let processing_agent_count = agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status,
                AgentStatus::Booting | AgentStatus::AwakeRunning
            )
        })
        .count();
    let waiting_agent_count = agents
        .iter()
        .filter(|agent| {
            !matches!(
                agent.status,
                AgentStatus::Booting | AgentStatus::AwakeRunning
            ) && (agent.active_task_count > 0 || agent.status == AgentStatus::AwaitingTask)
        })
        .count();
    let state = if processing_agent_count > 0 {
        RuntimeActivityState::Processing
    } else if waiting_agent_count > 0 || active_task_count > 0 {
        RuntimeActivityState::Waiting
    } else {
        RuntimeActivityState::Idle
    };
    RuntimeActivitySummary {
        state,
        active_agent_count: agents.len(),
        active_task_count,
        processing_agent_count,
        waiting_agent_count,
    }
}

impl holon_client::wire::RuntimeConfigSource for AppConfig {
    fn decision_surface(&self) -> RuntimeDecisionSurface {
        let config = self;
        let decision = &config.stored_config.decision;
        let local_onnx = decision.local_onnx.as_ref();
        let tools = decision.tools.as_ref();
        let selected_availability = decision
            .model
            .as_deref()
            .and_then(|model| crate::config::ModelRouteRef::parse_compatible(model).ok())
            .and_then(|route| resolved_route_availability(config, &route));
        RuntimeDecisionSurface {
            enabled: decision.enabled.unwrap_or(false),
            model: decision.model.clone(),
            protocol: selected_availability
                .as_ref()
                .and_then(|entry| entry.policy.decision_protocol)
                .map(|protocol| match protocol {
                    crate::model_catalog::DecisionProtocol::OpenAiCompatible => {
                        "openai_compatible".to_string()
                    }
                    crate::model_catalog::DecisionProtocol::OpenAiDecisions => {
                        "openai_decisions".to_string()
                    }
                    crate::model_catalog::DecisionProtocol::CloudflareClef => {
                        "cloudflare_clef".to_string()
                    }
                    crate::model_catalog::DecisionProtocol::Jev => "jev".to_string(),
                }),
            provider: selected_availability
                .as_ref()
                .map(|entry| entry.provider.clone()),
            endpoint: selected_availability
                .as_ref()
                .map(|entry| entry.endpoint.clone()),
            route_provider: selected_availability
                .as_ref()
                .map(|entry| entry.route_provider.clone()),
            transport: selected_availability
                .as_ref()
                .and_then(|entry| entry.transport.clone()),
            decision_capable: selected_availability
                .as_ref()
                .and_then(|entry| entry.resolved_capabilities.as_ref())
                .map(|capabilities| capabilities.decision_capable)
                .unwrap_or(false),
            credential_configured: selected_availability
                .as_ref()
                .map(|entry| entry.credential_configured)
                .unwrap_or(false),
            available: selected_availability
                .as_ref()
                .map(|entry| entry.available)
                .unwrap_or(false),
            unavailable_reason: selected_availability
                .as_ref()
                .and_then(|entry| entry.unavailable_reason.clone()),
            local_onnx: RuntimeDecisionLocalOnnxSurface {
                enabled: local_onnx
                    .and_then(|config| config.enabled)
                    .unwrap_or(false),
                preset: local_onnx.and_then(|config| config.preset.clone()),
                model_dir: local_onnx.and_then(|config| config.model_dir.clone()),
                variant: local_onnx.and_then(|config| config.variant.clone()),
                num_threads: local_onnx.and_then(|config| config.num_threads),
                checksum: local_onnx.and_then(|config| config.checksum.clone()),
            },
            tools: RuntimeDecisionToolsSurface {
                enabled: tools.and_then(|tools| tools.enabled).unwrap_or(false),
                max_calls_per_turn: tools.and_then(|tools| tools.max_calls_per_turn),
                timeout_ms: tools.and_then(|tools| tools.timeout_ms).unwrap_or(1500),
                min_confidence_percent: tools
                    .and_then(|tools| tools.min_confidence)
                    .unwrap_or(0.0)
                    .mul_add(100.0, 0.5) as u8,
            },
        }
    }

    fn config_surface(&self) -> RuntimeConfigSurface {
        let config = self;
        let mut model_catalog = config
            .validated_model_overrides
            .keys()
            .map(|value| value.as_string())
            .collect::<Vec<_>>();
        model_catalog.sort();
        RuntimeConfigSurface {
            model_default: config.default_model.as_string(),
            model_fallbacks: config
                .fallback_models
                .iter()
                .map(|value| value.as_string())
                .collect(),
            vision_default: config.vision_model.as_ref().map(|value| value.as_string()),
            image_generation_default: config
                .image_generation_model
                .as_ref()
                .map(|value| value.as_string()),
            decision: self.decision_surface(),
            model_catalog,
            unknown_model_fallback_configured: config.validated_unknown_model_fallback.is_some(),
            runtime_max_output_tokens: config.runtime_max_output_tokens,
            default_tool_output_tokens: config.default_tool_output_tokens,
            max_tool_output_tokens: config.max_tool_output_tokens,
            command_task_output_retention_bytes: config.command_task_output_retention_bytes,
            command_task_output_quota_bytes: config.command_task_output_quota_bytes,
            command_task_min_free_disk_bytes: config.command_task_min_free_disk_bytes,
            command_task_min_free_disk_percent: config.command_task_min_free_disk_percent,
            disable_provider_fallback: config.disable_provider_fallback,
            runtime_db_retention: config
                .runtime_db_retention_policy()
                .expect("runtime database retention policy was validated during config load"),
            providers: config
                .providers
                .values()
                .map(|provider| RuntimeProviderSummary {
                    id: provider.id.as_str().to_string(),
                    transport: provider.transport.as_str().to_string(),
                    base_url: provider.base_url.clone(),
                    oauth_supported: crate::auth::provider_supports_oauth_device_login(
                        provider.id.as_str(),
                    ),
                    api_key_supported: provider.auth.kind != crate::config::CredentialKind::None
                        && provider.transport.as_str() != "openai_codex_responses",
                    credential_source: provider.auth.source.as_str().to_string(),
                    credential_kind: provider.auth.kind.as_str().to_string(),
                    credential_env: provider.auth.env.clone(),
                    credential_profile: provider.auth.profile.clone(),
                    credential_external: provider.auth.external.clone(),
                    credential_configured: provider.auth.kind
                        == crate::config::CredentialKind::None
                        || provider.has_configured_credential(),
                    configured_in_config: config.stored_config.providers.contains_key(&provider.id),
                })
                .collect(),
            web_search: RuntimeWebSearchSummary {
                enabled: config.web_config.search.enabled,
                builtin_provider_enabled: config.web_config.search.builtin_provider_enabled,
                provider: config.web_config.search.provider.clone(),
                mode: config.web_config.search.mode.as_str().to_string(),
                providers: config.web_config.search.providers.clone(),
                max_results: config.web_config.search.max_results,
                max_provider_attempts: config.web_config.search.max_provider_attempts,
            },
            available_search_provider_kinds: WebProviderKind::ALL
                .iter()
                .map(|kind| RuntimeWebSearchProviderKindSummary {
                    kind: kind.as_str().to_string(),
                    capabilities: kind.capabilities(),
                })
                .collect(),
            web_search_providers: config
                .stored_config
                .web
                .providers
                .iter()
                .map(|(id, provider)| RuntimeWebSearchProviderSummary {
                    id: id.clone(),
                    kind: provider.kind.as_str().to_string(),
                    base_url: provider.base_url.clone(),
                    engine_id: provider.engine_id.clone(),
                    credential_profile: provider.credential_profile.clone(),
                    credential_configured: config
                        .web_config
                        .providers
                        .get(id)
                        .map(|provider| !provider.api_key.is_empty())
                        .unwrap_or(false),
                })
                .collect(),
        }
    }
}

pub async fn runtime_activity_summary(host: &RuntimeHost) -> Result<RuntimeActivitySummary> {
    let agents = host.public_agent_activity_snapshots().await?;
    Ok(activity_from_snapshots(&agents))
}
