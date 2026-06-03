use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::{
    auth::{load_codex_cli_credential, load_codex_oauth_profile_credential},
    config::{AppConfig, CredentialSource, ModelRef, ProviderId, RuntimeModelCatalog},
    context::ContextConfig,
    onboarding::{onboarding_report, search_diagnostics},
    types::{
        ModelAvailability, ModelProviderAvailability, ModelProviderEntry, ProviderModelEntry,
        ResolvedModelAvailability,
    },
};

use super::{build_candidate, classify_provider_error, retry::provider_retry_policy_json};

pub fn provider_doctor(config: &AppConfig) -> Value {
    let catalog = RuntimeModelCatalog::from_config(config);
    let mut providers = Vec::new();
    let mut model_availability = Vec::new();
    for model_ref in config.provider_chain() {
        let availability = provider_availability(config, &model_ref);
        let provider_cfg = config
            .providers
            .get(&model_ref.provider)
            .map(|provider| {
                json!({
                    "base_url": provider.base_url,
                    "transport": provider.transport.as_str(),
                    "auth": {
                        "source": provider.auth.source.as_str(),
                        "kind": provider.auth.kind.as_str(),
                        "env": provider.auth.env,
                        "profile": provider.auth.profile,
                        "external": provider.auth.external,
                        "credential_configured": provider.has_configured_credential(),
                    },
                })
            })
            .unwrap_or_else(|| json!({"error": "provider_not_configured"}));
        providers.push(json!({
            "model": model_ref.as_string(),
            "provider": model_ref.provider.as_str(),
            "settings": provider_cfg,
            "availability": availability,
        }));
        model_availability.push(resolved_model_availability_entry(
            config,
            &catalog,
            &model_ref,
            &availability,
        ));
    }
    let provider_model_availability = resolved_model_availability(config);
    let model_providers =
        resolved_model_providers_from_availability(config, &provider_model_availability);
    let models_by_provider = model_providers
        .iter()
        .map(|provider| {
            (
                provider.id.clone(),
                provider_models_from_availability(&provider_model_availability, &provider.id),
            )
        })
        .collect::<BTreeMap<_, _>>();

    json!({
        "default_model": config.default_model.as_string(),
        "fallback_models": config.fallback_models.iter().map(ModelRef::as_string).collect::<Vec<_>>(),
        "disable_provider_fallback": config.provider_fallback_disabled(),
        "runtime_max_output_tokens": config.runtime_max_output_tokens,
        "retry_policy": provider_retry_policy_json(),
        "onboarding": onboarding_report(config),
        "search": search_diagnostics(config),
        "model_availability": model_availability,
        "model_providers": model_providers,
        "models_by_provider": models_by_provider,
        "providers": providers,
    })
}

pub fn resolved_model_availability(config: &AppConfig) -> Vec<ResolvedModelAvailability> {
    let catalog = RuntimeModelCatalog::from_config(config);
    let mut models = BTreeMap::new();
    for entry in catalog.available_models() {
        models.insert(entry.model_ref.as_string(), entry.model_ref);
    }
    models.insert(
        config.default_model.as_string(),
        config.default_model.clone(),
    );
    for model_ref in &config.fallback_models {
        models.insert(model_ref.as_string(), model_ref.clone());
    }
    for model_ref in config.validated_model_overrides.keys() {
        models.insert(model_ref.as_string(), model_ref.clone());
    }

    models
        .into_values()
        .map(|model_ref| {
            let availability = provider_availability(config, &model_ref);
            resolved_model_availability_entry(config, &catalog, &model_ref, &availability)
        })
        .collect()
}

pub fn resolved_model_providers(config: &AppConfig) -> Vec<ModelProviderEntry> {
    let models = resolved_model_availability(config);
    resolved_model_providers_from_availability(config, &models)
}

fn resolved_model_providers_from_availability(
    config: &AppConfig,
    models: &[ResolvedModelAvailability],
) -> Vec<ModelProviderEntry> {
    let mut providers = BTreeMap::<String, Vec<&ResolvedModelAvailability>>::new();
    for model in models {
        providers
            .entry(model.provider.clone())
            .or_default()
            .push(model);
    }
    for provider_id in config.providers.keys() {
        providers
            .entry(provider_id.as_str().to_string())
            .or_default();
    }

    providers
        .into_iter()
        .map(|(provider_id, models)| {
            let parsed_provider_id = ProviderId::parse(&provider_id).ok();
            let provider = parsed_provider_id
                .as_ref()
                .and_then(|provider_id| config.providers.get(provider_id));
            let first_model = models.first().copied();
            let available_count = models.iter().filter(|model| model.available).count();
            let model_count = models.len();
            let availability = if model_count == 0 || available_count == 0 {
                ModelProviderAvailability::Unavailable
            } else if available_count == model_count {
                ModelProviderAvailability::Available
            } else {
                ModelProviderAvailability::Degraded
            };
            let provider_configured = provider.is_some()
                || first_model
                    .map(|model| model.provider_configured)
                    .unwrap_or(false);
            let provider_source = first_model
                .and_then(|model| model.provider_source.clone())
                .or_else(|| {
                    if provider.is_some() {
                        parsed_provider_id
                            .as_ref()
                            .map(|provider_id| provider_source_for_config(config, provider_id))
                    } else {
                        None
                    }
                });
            let credential_configured = models.iter().any(|model| model.credential_configured)
                || provider
                    .map(provider_static_credential_configured)
                    .unwrap_or(false);

            ModelProviderEntry {
                id: provider_id.clone(),
                display_name: Some(provider_id.clone()),
                availability,
                provider_configured,
                provider_source,
                transport: first_model
                    .and_then(|model| model.transport.clone())
                    .or_else(|| provider.map(|provider| provider.transport.as_str().to_string())),
                credential_source: first_model
                    .and_then(|model| model.credential_source.clone())
                    .or_else(|| provider.map(|provider| provider.auth.source.as_str().to_string())),
                credential_kind: first_model
                    .and_then(|model| model.credential_kind.clone())
                    .or_else(|| provider.map(|provider| provider.auth.kind.as_str().to_string())),
                credential_configured,
                default_model: default_model_for_provider(config, &provider_id),
                model_count,
                discovered_model_count: models
                    .iter()
                    .filter(|model| model.metadata_source == "remote_discovered")
                    .count(),
                policy_notes: Vec::new(),
            }
        })
        .collect()
}

pub fn resolved_provider_models(config: &AppConfig, provider: &str) -> Vec<ProviderModelEntry> {
    let models = resolved_model_availability(config);
    provider_models_from_availability(&models, provider)
}

fn provider_models_from_availability(
    models: &[ResolvedModelAvailability],
    provider: &str,
) -> Vec<ProviderModelEntry> {
    models
        .iter()
        .filter(|model| model.provider == provider)
        .cloned()
        .into_iter()
        .map(|model| {
            let model_id = model.policy.model_ref.model.clone();
            ProviderModelEntry {
                provider: model.provider,
                id: model_id,
                model_ref: model.model,
                display_name: model.display_name,
                availability: if model.available {
                    ModelAvailability::Available
                } else {
                    ModelAvailability::Unavailable
                },
                selectable: model.available,
                unavailable_reason: model.unavailable_reason,
                metadata_source: model.metadata_source,
                policy: model.policy,
                policy_notes: Vec::new(),
            }
        })
        .collect()
}

fn resolved_model_availability_entry(
    config: &AppConfig,
    catalog: &RuntimeModelCatalog,
    model_ref: &ModelRef,
    availability: &Value,
) -> ResolvedModelAvailability {
    let base_context = base_context_config(config);
    let policy = catalog.resolved_model_policy(&base_context, Some(model_ref));
    let metadata_source = if config.validated_model_overrides.contains_key(model_ref) {
        "config_override".to_string()
    } else {
        serde_json::to_value(policy.source)
            .ok()
            .and_then(|value| value.as_str().map(ToString::to_string))
            .unwrap_or_else(|| "unknown_fallback".to_string())
    };
    let provider = config.providers.get(&model_ref.provider);
    let provider_configured = provider.is_some();
    let provider_source = provider.map(|_| provider_source_for_config(config, &model_ref.provider));
    let credential_configured = provider
        .map(provider_static_credential_configured)
        .unwrap_or(false);
    let available = availability["available"].as_bool().unwrap_or(false);
    let availability_failure_reason = availability["error"]
        .as_str()
        .or_else(|| availability["failure_kind"].as_str())
        .map(ToString::to_string);
    let unavailable_reason = if available {
        None
    } else if !provider_configured {
        Some("provider_not_configured".to_string())
    } else if provider
        .map(credential_missing_should_be_static_reason)
        .unwrap_or(false)
        && !credential_configured
    {
        Some("credential_missing".to_string())
    } else {
        availability_failure_reason
    };

    ResolvedModelAvailability {
        model: model_ref.as_string(),
        provider: model_ref.provider.as_str().to_string(),
        display_name: policy.display_name.clone(),
        metadata_source,
        provider_configured,
        provider_source,
        transport: provider.map(|provider| provider.transport.as_str().to_string()),
        credential_source: provider.map(|provider| provider.auth.source.as_str().to_string()),
        credential_kind: provider.map(|provider| provider.auth.kind.as_str().to_string()),
        credential_configured: credential_configured || available,
        available,
        unavailable_reason,
        policy,
    }
}

fn provider_source_for_config(config: &AppConfig, provider_id: &ProviderId) -> String {
    if config.stored_config.providers.contains_key(provider_id) {
        "config".to_string()
    } else {
        "built_in".to_string()
    }
}

fn default_model_for_provider(config: &AppConfig, provider_id: &str) -> Option<String> {
    if config.default_model.provider.as_str() == provider_id {
        return Some(config.default_model.model.clone());
    }
    config
        .fallback_models
        .iter()
        .find(|model| model.provider.as_str() == provider_id)
        .map(|model| model.model.clone())
}

fn provider_static_credential_configured(provider: &crate::config::ProviderRuntimeConfig) -> bool {
    provider.has_configured_credential() || matches!(provider.auth.source, CredentialSource::None)
}

fn credential_missing_should_be_static_reason(
    provider: &crate::config::ProviderRuntimeConfig,
) -> bool {
    if provider.id.is_openai_codex() && provider.auth.source == CredentialSource::AuthProfile {
        return false;
    }
    matches!(
        provider.auth.source,
        CredentialSource::Env | CredentialSource::AuthProfile
    )
}

fn base_context_config(config: &AppConfig) -> ContextConfig {
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

fn provider_availability(config: &AppConfig, model_ref: &ModelRef) -> Value {
    let mut availability = match build_candidate(config, model_ref) {
        Ok(candidate) => json!({
            "available": true,
            "prompt_capabilities": candidate.provider.prompt_capabilities(),
        }),
        Err(error) => {
            let classification = classify_provider_error(&error);
            let mut availability = json!({
                "available": false,
                "error": error.to_string(),
                "failure_kind": classification.kind.as_str(),
                "disposition": classification.disposition.as_str(),
            });
            if classification.kind == super::retry::ProviderFailureKind::UnsupportedTransport {
                availability["transport_contract"] = json!("streaming_required");
            }
            availability
        }
    };

    if let Some(provider) = config.providers.get(&model_ref.provider) {
        if provider.auth.source != CredentialSource::ExternalCli
            && !(provider.id.is_openai_codex()
                && provider.auth.source == CredentialSource::AuthProfile)
        {
            return availability;
        }
        let credential_result = provider
            .credential
            .as_deref()
            .filter(|material| !material.trim().is_empty())
            .map(|material| {
                load_codex_oauth_profile_credential(
                    material,
                    provider.auth.profile.as_deref().unwrap_or("openai-codex"),
                )
            })
            .unwrap_or_else(|| {
                provider
                    .codex_home
                    .as_ref()
                    .map(|codex_home| load_codex_cli_credential(codex_home))
                    .unwrap_or_else(|| Err(anyhow::anyhow!("missing codex_home")))
            });
        if let Ok(credential) = credential_result.as_ref() {
            availability["credential"] = json!({
                "source": credential.source,
                "account_id": credential.account_id,
                "expires_at": credential.expires_at,
            });
        }
        if let Err(error) = credential_result {
            availability["credential_error"] = json!(error.to_string());
        }
    }

    availability
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use tempfile::tempdir;

    use crate::{
        config::{provider_registry_for_tests, AppConfig, ControlAuthMode, ModelRef, ProviderId},
        model_catalog::{ModelMetadataSource, ModelRuntimeOverride},
    };

    use super::{
        provider_doctor, resolved_model_availability, resolved_model_providers,
        resolved_provider_models,
    };

    struct TestConfigFixture {
        _home_dir: tempfile::TempDir,
        _workspace_dir: tempfile::TempDir,
        config: AppConfig,
    }

    fn test_config(openai_key: Option<&str>) -> TestConfigFixture {
        let home_dir = tempdir().unwrap();
        let workspace_dir = tempdir().unwrap();
        let home_path = home_dir.path().to_path_buf();
        let workspace_path = workspace_dir.path().to_path_buf();
        let config = AppConfig {
            default_agent_id: "default".into(),
            http_addr: "127.0.0.1:0".into(),
            callback_base_url: "http://127.0.0.1:0".into(),
            home_dir: home_path.clone(),
            data_dir: home_path.clone(),
            socket_path: home_path.join("run").join("holon.sock"),
            workspace_dir: workspace_path,
            context_window_messages: 8,
            context_window_briefs: 8,
            compaction_trigger_messages: 10,
            compaction_keep_recent_messages: 4,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            control_token: Some("control-value".into()),
            control_auth_mode: ControlAuthMode::Auto,
            config_file_path: home_path.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse("openai/gpt-5.4").unwrap(),
            fallback_models: vec![ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap()],
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: HashMap::new(),
            validated_unknown_model_fallback: None,
            model_discovery_cache: Default::default(),
            providers: provider_registry_for_tests(
                openai_key,
                Some("anthropic-token"),
                PathBuf::new(),
            ),
            web_config: crate::web::WebConfig::default(),
        };
        TestConfigFixture {
            _home_dir: home_dir,
            _workspace_dir: workspace_dir,
            config,
        }
    }

    #[test]
    fn resolved_model_availability_marks_configured_model_ready() {
        let fixture = test_config(Some("openai-key"));
        let models = resolved_model_availability(&fixture.config);
        let openai = models
            .iter()
            .find(|entry| entry.model == "openai/gpt-5.4")
            .expect("openai model entry");

        assert!(openai.provider_configured);
        assert!(openai.credential_configured);
        assert!(openai.available);
        assert_eq!(openai.provider_source.as_deref(), Some("built_in"));
        assert_eq!(openai.metadata_source, "conservative_builtin");
        assert_eq!(
            openai.policy.source,
            ModelMetadataSource::ConservativeBuiltin
        );
    }

    #[test]
    fn resolved_model_availability_reports_missing_credential() {
        let fixture = test_config(None);
        let models = resolved_model_availability(&fixture.config);
        let openai = models
            .iter()
            .find(|entry| entry.model == "openai/gpt-5.4")
            .expect("openai model entry");

        assert!(openai.provider_configured);
        assert!(!openai.credential_configured);
        assert!(!openai.available);
        assert_eq!(
            openai.unavailable_reason.as_deref(),
            Some("credential_missing")
        );
    }

    #[test]
    fn resolved_model_availability_preserves_external_cli_config_errors() {
        let mut fixture = test_config(Some("openai-key"));
        fixture
            .config
            .providers
            .get_mut(&ProviderId::openai_codex())
            .expect("openai codex provider")
            .codex_home = None;

        let models = resolved_model_availability(&fixture.config);
        let codex = models
            .iter()
            .find(|entry| entry.model == "openai-codex/gpt-5.4")
            .expect("openai codex model entry");

        assert!(codex.provider_configured);
        assert!(!codex.available);
        assert_ne!(
            codex.unavailable_reason.as_deref(),
            Some("credential_missing")
        );
        assert!(codex
            .unavailable_reason
            .as_deref()
            .unwrap_or_default()
            .contains("codex_home"));
    }

    #[test]
    fn resolved_model_availability_includes_config_catalog_models() {
        let mut fixture = test_config(Some("openai-key"));
        let config = &mut fixture.config;
        config.validated_model_overrides.insert(
            ModelRef::new(ProviderId::openai(), "custom-model"),
            ModelRuntimeOverride {
                display_name: Some("Custom Model".into()),
                runtime_max_output_tokens: Some(1024),
                ..Default::default()
            },
        );

        let models = resolved_model_availability(&config);
        let custom = models
            .iter()
            .find(|entry| entry.model == "openai/custom-model")
            .expect("custom model entry");

        assert_eq!(custom.display_name, "Custom Model");
        assert_eq!(custom.metadata_source, "config_override");
        assert!(custom.available);
        assert_eq!(custom.policy.runtime_max_output_tokens, 1024);
    }

    #[test]
    fn resolved_model_providers_groups_models_by_provider() {
        let fixture = test_config(Some("openai-key"));
        let providers = resolved_model_providers(&fixture.config);
        let openai = providers
            .iter()
            .find(|entry| entry.id == "openai")
            .expect("openai provider entry");

        assert!(openai.provider_configured);
        assert!(openai.credential_configured);
        assert_eq!(openai.default_model.as_deref(), Some("gpt-5.4"));
        assert!(openai.model_count > 0);
        assert_eq!(
            openai.availability,
            crate::types::ModelProviderAvailability::Available
        );
    }

    #[test]
    fn resolved_provider_models_returns_models_for_one_provider() {
        let fixture = test_config(Some("openai-key"));
        let models = resolved_provider_models(&fixture.config, "openai");
        let openai = models
            .iter()
            .find(|entry| entry.model_ref == "openai/gpt-5.4")
            .expect("openai model entry");

        assert_eq!(openai.provider, "openai");
        assert_eq!(openai.id, "gpt-5.4");
        assert!(openai.selectable);
        assert_eq!(
            openai.availability,
            crate::types::ModelAvailability::Available
        );
    }

    #[test]
    fn provider_doctor_includes_chain_model_availability() {
        let fixture = test_config(Some("openai-key"));
        let doctor = provider_doctor(&fixture.config);
        let models = doctor["model_availability"]
            .as_array()
            .expect("model availability array");

        assert!(models
            .iter()
            .any(|entry| entry["model"].as_str() == Some("openai/gpt-5.4")
                && entry["available"].as_bool() == Some(true)));
    }

    #[test]
    fn provider_doctor_includes_onboarding_report_contract() {
        let fixture = test_config(Some("openai-key"));
        let doctor = provider_doctor(&fixture.config);

        assert_eq!(doctor["onboarding"]["schema_version"].as_u64(), Some(1));
        assert_eq!(doctor["onboarding"]["status"].as_str(), Some("configured"));
        assert_eq!(doctor["search"]["status"].as_str(), Some("configured"));
        assert!(doctor["onboarding"]["sections"]
            .as_array()
            .expect("onboarding sections")
            .iter()
            .any(|section| section["id"].as_str() == Some("model_provider")));
    }
}
