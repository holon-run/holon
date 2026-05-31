use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{AppConfig, CredentialSource};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingReport {
    pub schema_version: u32,
    pub status: OnboardingStatus,
    pub sections: Vec<OnboardingSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<OnboardingAction>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingStatus {
    Configured,
    Missing,
    Unavailable,
    Restricted,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingSection {
    pub id: String,
    pub title: String,
    pub status: OnboardingStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<OnboardingAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingAction {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

pub fn onboarding_report(config: &AppConfig) -> OnboardingReport {
    let sections = vec![
        home_section(config),
        agent_section(config),
        model_provider_section(config),
        search_section(config),
        credentials_section(config),
    ];
    let status = overall_status(&sections);
    let next_actions = sections
        .iter()
        .flat_map(|section| section.actions.iter().cloned())
        .collect();

    OnboardingReport {
        schema_version: 1,
        status,
        sections,
        next_actions,
    }
}

fn home_section(config: &AppConfig) -> OnboardingSection {
    section(
        "home",
        "Holon home",
        OnboardingStatus::Configured,
        "Holon home and config paths are resolved.",
        [
            ("home_dir", json!(config.home_dir)),
            ("data_dir", json!(config.data_dir)),
            ("config_file", json!(config.config_file_path)),
            ("socket_path", json!(config.socket_path)),
        ],
        [],
    )
}

fn agent_section(config: &AppConfig) -> OnboardingSection {
    section(
        "agent",
        "Default agent",
        OnboardingStatus::Configured,
        "A default agent id is configured.",
        [("default_agent_id", json!(config.default_agent_id))],
        [],
    )
}

fn model_provider_section(config: &AppConfig) -> OnboardingSection {
    let model_ref = &config.default_model;
    let Some(provider) = config.providers.get(&model_ref.provider) else {
        return section(
            "model_provider",
            "Model provider",
            OnboardingStatus::Missing,
            "The default model provider is not configured.",
            [
                ("default_model", json!(model_ref.as_string())),
                ("provider", json!(model_ref.provider.as_str())),
            ],
            [OnboardingAction {
                id: "configure_model_provider".into(),
                title: "Configure a model provider credential.".into(),
                command: Some(vec![
                    "holon".into(),
                    "config".into(),
                    "providers".into(),
                    "set".into(),
                    model_ref.provider.as_str().into(),
                ]),
            }],
        );
    };

    let credential_configured =
        provider.has_configured_credential() || provider.auth.source == CredentialSource::None;
    let status = if credential_configured {
        OnboardingStatus::Configured
    } else {
        OnboardingStatus::Missing
    };
    let actions = if credential_configured {
        Vec::new()
    } else {
        vec![OnboardingAction {
            id: "configure_model_credential".into(),
            title: "Configure a credential for the default model provider.".into(),
            command: Some(vec![
                "holon".into(),
                "config".into(),
                "providers".into(),
                "set".into(),
                model_ref.provider.as_str().into(),
            ]),
        }]
    };

    section(
        "model_provider",
        "Model provider",
        status,
        if credential_configured {
            "The default model provider has a configured credential path."
        } else {
            "The default model provider is missing a configured credential path."
        },
        [
            ("default_model", json!(model_ref.as_string())),
            ("provider", json!(model_ref.provider.as_str())),
            ("transport", json!(provider.transport.as_str())),
            ("credential_source", json!(provider.auth.source.as_str())),
            ("credential_kind", json!(provider.auth.kind.as_str())),
            ("credential_configured", json!(credential_configured)),
        ],
        actions,
    )
}

fn search_section(config: &AppConfig) -> OnboardingSection {
    let search = &config.web_config.search;
    let mut configured_providers = search.providers.clone();
    if !search.provider.trim().is_empty() && search.provider != "auto" {
        configured_providers.push(search.provider.clone());
    }
    configured_providers.sort();
    configured_providers.dedup();

    if !search.enabled {
        return section(
            "search",
            "Search tools",
            OnboardingStatus::Skipped,
            "Search tools are disabled.",
            [
                ("enabled", json!(false)),
                ("provider", json!(search.provider)),
                ("mode", json!(search.mode.as_str())),
            ],
            [],
        );
    }

    let configured = search.builtin_provider_enabled || !configured_providers.is_empty();
    let status = if configured {
        OnboardingStatus::Configured
    } else {
        OnboardingStatus::Missing
    };
    let actions = if configured {
        Vec::new()
    } else {
        vec![OnboardingAction {
            id: "configure_search_provider".into(),
            title: "Configure a search provider or enable the built-in provider.".into(),
            command: Some(vec![
                "holon".into(),
                "config".into(),
                "set".into(),
                "web.search.provider".into(),
                "duck_duck_go".into(),
            ]),
        }]
    };

    section(
        "search",
        "Search tools",
        status,
        if configured {
            "Search tools have at least one configured provider path."
        } else {
            "Search tools are enabled but no provider path is configured."
        },
        [
            ("enabled", json!(true)),
            (
                "builtin_provider_enabled",
                json!(search.builtin_provider_enabled),
            ),
            ("provider", json!(search.provider)),
            ("providers", json!(configured_providers)),
            ("mode", json!(search.mode.as_str())),
            ("max_results", json!(search.max_results)),
        ],
        actions,
    )
}

fn credentials_section(config: &AppConfig) -> OnboardingSection {
    let mut provider_credentials = Vec::new();
    for (id, provider) in &config.providers {
        provider_credentials.push(json!({
            "provider": id.as_str(),
            "source": provider.auth.source.as_str(),
            "kind": provider.auth.kind.as_str(),
            "configured": provider.has_configured_credential()
                || provider.auth.source == CredentialSource::None,
        }));
    }

    section(
        "credentials",
        "Credential safety",
        OnboardingStatus::Configured,
        "Credential report is secret-safe and only exposes source metadata.",
        [
            ("provider_credentials", json!(provider_credentials)),
            ("redaction", json!("material_never_included")),
        ],
        [],
    )
}

fn section<D, A>(
    id: &str,
    title: &str,
    status: OnboardingStatus,
    summary: &str,
    details: D,
    actions: A,
) -> OnboardingSection
where
    D: IntoIterator<Item = (&'static str, Value)>,
    A: IntoIterator<Item = OnboardingAction>,
{
    OnboardingSection {
        id: id.into(),
        title: title.into(),
        status,
        summary: summary.into(),
        details: details
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        actions: actions.into_iter().collect(),
    }
}

fn overall_status(sections: &[OnboardingSection]) -> OnboardingStatus {
    if sections
        .iter()
        .any(|section| section.status == OnboardingStatus::Failed)
    {
        OnboardingStatus::Failed
    } else if sections
        .iter()
        .any(|section| section.status == OnboardingStatus::Unavailable)
    {
        OnboardingStatus::Unavailable
    } else if sections
        .iter()
        .any(|section| section.status == OnboardingStatus::Missing)
    {
        OnboardingStatus::Missing
    } else if sections
        .iter()
        .any(|section| section.status == OnboardingStatus::Restricted)
    {
        OnboardingStatus::Restricted
    } else {
        OnboardingStatus::Configured
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use tempfile::tempdir;

    use crate::{
        config::{provider_registry_for_tests, AppConfig, ControlAuthMode, ModelRef},
        onboarding::{onboarding_report, OnboardingStatus},
    };

    fn test_config(openai_key: Option<&str>) -> AppConfig {
        let home_dir = tempdir().unwrap().keep();
        let workspace_dir = tempdir().unwrap().keep();
        AppConfig {
            default_agent_id: "default".into(),
            http_addr: "127.0.0.1:0".into(),
            callback_base_url: "http://127.0.0.1:0".into(),
            home_dir: home_dir.clone(),
            data_dir: home_dir.clone(),
            socket_path: home_dir.join("run").join("holon.sock"),
            workspace_dir,
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
            config_file_path: home_dir.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse("openai/gpt-5.4").unwrap(),
            fallback_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: HashMap::new(),
            validated_unknown_model_fallback: None,
            providers: provider_registry_for_tests(openai_key, None, PathBuf::new()),
            web_config: crate::web::WebConfig::default(),
        }
    }

    #[test]
    fn onboarding_report_is_configured_without_secret_material() {
        let config = test_config(Some("openai-key"));
        let report = onboarding_report(&config);
        let json = serde_json::to_string(&report).unwrap();

        assert_eq!(report.status, OnboardingStatus::Configured);
        assert!(json.contains("credential_configured"));
        assert!(!json.contains("openai-key"));
        assert!(!json.contains("control-value"));
    }

    #[test]
    fn onboarding_report_marks_missing_model_credential() {
        let config = test_config(None);
        let report = onboarding_report(&config);
        let model = report
            .sections
            .iter()
            .find(|section| section.id == "model_provider")
            .expect("model provider section");

        assert_eq!(report.status, OnboardingStatus::Missing);
        assert_eq!(model.status, OnboardingStatus::Missing);
        assert_eq!(report.next_actions.len(), 1);
    }
}
