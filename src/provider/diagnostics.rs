use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::load_codex_cli_credential,
    config::{AppConfig, CredentialSource, ModelRef, RuntimeModelCatalog},
    context::ContextConfig,
    model_catalog::ResolvedRuntimeModelPolicy,
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

    json!({
        "default_model": config.default_model.as_string(),
        "fallback_models": config.fallback_models.iter().map(ModelRef::as_string).collect::<Vec<_>>(),
        "disable_provider_fallback": config.provider_fallback_disabled(),
        "runtime_max_output_tokens": config.runtime_max_output_tokens,
        "retry_policy": provider_retry_policy_json(),
        "model_availability": model_availability,
        "providers": providers,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedModelAvailability {
    pub model: String,
    pub provider: String,
    pub display_name: String,
    pub metadata_source: String,
    pub provider_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<String>,
    pub credential_configured: bool,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub policy: ResolvedRuntimeModelPolicy,
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
    let provider_source = provider.map(|_| {
        if config
            .stored_config
            .providers
            .contains_key(&model_ref.provider)
        {
            "config".to_string()
        } else {
            "built_in".to_string()
        }
    });
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
        .map(|provider| credential_missing_should_be_static_reason(provider.auth.source))
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

fn provider_static_credential_configured(provider: &crate::config::ProviderRuntimeConfig) -> bool {
    provider.has_configured_credential() || matches!(provider.auth.source, CredentialSource::None)
}

fn credential_missing_should_be_static_reason(source: CredentialSource) -> bool {
    matches!(
        source,
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
        if provider.auth.source != CredentialSource::ExternalCli {
            return availability;
        }
        let Some(codex_home) = provider.codex_home.as_ref() else {
            return availability;
        };
        let credential_result = load_codex_cli_credential(codex_home);
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

    use super::{provider_doctor, resolved_model_availability};

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
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: HashMap::new(),
            validated_unknown_model_fallback: None,
            providers: provider_registry_for_tests(
                openai_key,
                Some("anthropic-token"),
                PathBuf::new(),
            ),
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
}
