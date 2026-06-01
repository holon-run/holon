use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    config::{AppConfig, CredentialSource, ProviderId},
    web::{WebProviderAuthClass, WebProviderSupportStatus},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingReport {
    pub schema_version: u32,
    pub status: OnboardingStatus,
    pub sections: Vec<OnboardingSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<OnboardingAction>,
}

pub fn credential_repair_plan(config: &AppConfig) -> Option<OnboardingCredentialRepair> {
    let provider_id = &config.default_model.provider;
    let provider = config.providers.get(provider_id);
    let provider_configured = provider.is_some();
    let credential_configured = provider
        .map(|provider| {
            provider.has_configured_credential() || provider.auth.source == CredentialSource::None
        })
        .unwrap_or(false);

    if credential_configured {
        return None;
    }

    Some(OnboardingCredentialRepair {
        provider: provider_id.as_str().to_string(),
        credential_profile: default_credential_profile(provider_id),
        credential_kind: "api_key".into(),
        provider_configured,
        credential_configured,
        requires_confirmation: provider_configured,
        summary: if provider_configured {
            "Update the default model provider to use a stored credential profile.".into()
        } else {
            "Configure the default model provider with a stored credential profile.".into()
        },
    })
}

fn default_credential_profile(provider_id: &ProviderId) -> String {
    provider_id.as_str().to_string()
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingCredentialRepair {
    pub provider: String,
    pub credential_profile: String,
    pub credential_kind: String,
    pub provider_configured: bool,
    pub credential_configured: bool,
    pub requires_confirmation: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchDiagnostics {
    pub status: OnboardingStatus,
    pub summary: String,
    pub enabled: bool,
    pub builtin_provider_enabled: bool,
    pub provider: String,
    pub providers: Vec<String>,
    pub mode: String,
    pub max_results: usize,
    pub managed_providers: Vec<SearchManagedProviderDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_provider: Option<SearchBuiltinProviderDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<OnboardingAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchManagedProviderDiagnostic {
    pub id: String,
    pub kind: String,
    pub configured: bool,
    pub available: bool,
    pub status: String,
    pub auth: String,
    pub credential_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchBuiltinProviderDiagnostic {
    pub provider: String,
    pub configured: bool,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised_tool_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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

pub fn search_diagnostics(config: &AppConfig) -> SearchDiagnostics {
    let search = &config.web_config.search;
    let providers = configured_search_providers(config);
    let managed_providers = providers
        .iter()
        .map(|provider| managed_search_provider_diagnostic(config, provider))
        .collect::<Vec<_>>();
    let builtin_provider = search
        .builtin_provider_enabled
        .then(|| builtin_search_provider_diagnostic(config));

    if !search.enabled {
        return SearchDiagnostics {
            status: OnboardingStatus::Skipped,
            summary: "Search tools are disabled.".into(),
            enabled: false,
            builtin_provider_enabled: search.builtin_provider_enabled,
            provider: search.provider.clone(),
            providers,
            mode: search.mode.as_str().into(),
            max_results: search.max_results,
            managed_providers,
            builtin_provider,
            actions: Vec::new(),
        };
    }

    let has_available_managed_provider =
        managed_providers.iter().any(|provider| provider.available);
    let has_available_builtin_provider = builtin_provider
        .as_ref()
        .map(|provider| provider.available)
        .unwrap_or(false);
    let has_configured_path = has_available_managed_provider || has_available_builtin_provider;
    let has_unavailable_path = managed_providers
        .iter()
        .any(|provider| provider.configured && !provider.available)
        || builtin_provider
            .as_ref()
            .map(|provider| provider.configured && !provider.available)
            .unwrap_or(false);
    let status = if has_configured_path {
        OnboardingStatus::Configured
    } else if has_unavailable_path {
        OnboardingStatus::Unavailable
    } else {
        OnboardingStatus::Missing
    };
    let actions = if status == OnboardingStatus::Configured {
        Vec::new()
    } else {
        vec![OnboardingAction {
            id: "configure_search_provider".into(),
            title: "Configure a search provider or enable a provider-declared built-in search capability.".into(),
            command: Some(vec![
                "holon".into(),
                "config".into(),
                "set".into(),
                "web.search.provider".into(),
                "duck_duck_go".into(),
            ]),
        }]
    };

    let summary = match status {
        OnboardingStatus::Configured => "Search tools have at least one available provider path.",
        OnboardingStatus::Unavailable => {
            "Search tools are enabled, but the configured provider path is unavailable."
        }
        OnboardingStatus::Missing => "Search tools are enabled but no provider path is configured.",
        _ => "Search tools require attention.",
    };

    SearchDiagnostics {
        status,
        summary: summary.into(),
        enabled: true,
        builtin_provider_enabled: search.builtin_provider_enabled,
        provider: search.provider.clone(),
        providers,
        mode: search.mode.as_str().into(),
        max_results: search.max_results,
        managed_providers,
        builtin_provider,
        actions,
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
    let diagnostics = search_diagnostics(config);

    section(
        "search",
        "Search tools",
        diagnostics.status,
        &diagnostics.summary,
        [
            ("enabled", json!(diagnostics.enabled)),
            (
                "builtin_provider_enabled",
                json!(diagnostics.builtin_provider_enabled),
            ),
            ("provider", json!(diagnostics.provider)),
            ("providers", json!(diagnostics.providers)),
            ("mode", json!(diagnostics.mode)),
            ("max_results", json!(diagnostics.max_results)),
            ("managed_providers", json!(diagnostics.managed_providers)),
            ("builtin_provider", json!(diagnostics.builtin_provider)),
        ],
        diagnostics.actions,
    )
}

fn configured_search_providers(config: &AppConfig) -> Vec<String> {
    let search = &config.web_config.search;
    let mut providers = search.providers.clone();
    if !search.provider.trim().is_empty() && search.provider != "auto" {
        providers.push(search.provider.clone());
    }
    providers.sort();
    providers.dedup();
    providers
}

fn managed_search_provider_diagnostic(
    config: &AppConfig,
    provider_id: &str,
) -> SearchManagedProviderDiagnostic {
    if provider_id == "duck_duck_go" {
        let capabilities = crate::web::WebProviderKind::DuckDuckGo.capabilities();
        return SearchManagedProviderDiagnostic {
            id: provider_id.into(),
            kind: crate::web::WebProviderKind::DuckDuckGo.as_str().into(),
            configured: true,
            available: true,
            status: web_provider_support_status(capabilities.status).into(),
            auth: web_provider_auth(capabilities.auth).into(),
            credential_configured: true,
            reason: None,
        };
    }

    let Some(provider) = config.web_config.providers.get(provider_id) else {
        return SearchManagedProviderDiagnostic {
            id: provider_id.into(),
            kind: "unknown".into(),
            configured: false,
            available: false,
            status: "unknown_provider".into(),
            auth: "unknown".into(),
            credential_configured: false,
            reason: Some("configured search provider id is not defined in web.providers".into()),
        };
    };
    let capabilities = provider.kind.capabilities();
    let credential_configured =
        capabilities.auth != WebProviderAuthClass::ApiKey || !provider.api_key.trim().is_empty();
    let supported = capabilities.status == WebProviderSupportStatus::Supported;
    let available = supported && credential_configured;
    let reason = if !supported {
        Some(format!(
            "provider kind {} is {}",
            provider.kind.as_str(),
            web_provider_support_status(capabilities.status)
        ))
    } else if !credential_configured {
        Some("provider requires an API key credential profile".into())
    } else {
        None
    };

    SearchManagedProviderDiagnostic {
        id: provider_id.into(),
        kind: provider.kind.as_str().into(),
        configured: true,
        available,
        status: web_provider_support_status(capabilities.status).into(),
        auth: web_provider_auth(capabilities.auth).into(),
        credential_configured,
        reason,
    }
}

fn builtin_search_provider_diagnostic(config: &AppConfig) -> SearchBuiltinProviderDiagnostic {
    let provider_id = config.default_model.provider.as_str().to_string();
    let provider = config.providers.get(&config.default_model.provider);
    let capability = provider.and_then(|provider| provider.builtin_web_search.as_ref());
    let Some(capability) = capability else {
        return SearchBuiltinProviderDiagnostic {
            provider: provider_id,
            configured: false,
            available: false,
            provider_model_ref: None,
            transport: provider.map(|provider| provider.transport.as_str().into()),
            advertised_tool_type: None,
            backend_kind: None,
            reason: Some("default model provider does not declare built-in web search".into()),
        };
    };

    SearchBuiltinProviderDiagnostic {
        provider: provider_id.clone(),
        configured: capability.enabled,
        available: capability.enabled,
        provider_model_ref: Some(config.default_model.as_string()),
        transport: provider.map(|provider| provider.transport.as_str().into()),
        advertised_tool_type: Some(capability.advertised_tool_type.clone()),
        backend_kind: Some(capability.backend_kind.clone()),
        reason: (!capability.enabled).then(|| {
            format!("provider {provider_id} declares built-in web search but it is disabled")
        }),
    }
}

fn web_provider_support_status(status: WebProviderSupportStatus) -> &'static str {
    match status {
        WebProviderSupportStatus::Supported => "supported",
        WebProviderSupportStatus::Unsupported => "unsupported",
        WebProviderSupportStatus::NativeOnly => "native_only",
    }
}

fn web_provider_auth(auth: WebProviderAuthClass) -> &'static str {
    match auth {
        WebProviderAuthClass::None => "none",
        WebProviderAuthClass::ApiKey => "api_key",
        WebProviderAuthClass::NativeProvider => "native_provider",
        WebProviderAuthClass::SelfHosted => "self_hosted",
    }
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
        onboarding::{
            credential_repair_plan, onboarding_report, search_diagnostics, OnboardingStatus,
        },
        web::{WebProviderConfig, WebProviderKind, WebProviderLimitsConfig, WebSearchMode},
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

    #[test]
    fn credential_repair_plan_targets_default_provider_without_secret_material() {
        let config = test_config(None);
        let plan = credential_repair_plan(&config).expect("repair plan");
        let json = serde_json::to_string(&plan).unwrap();

        assert_eq!(plan.provider, "openai");
        assert_eq!(plan.credential_profile, "openai");
        assert_eq!(plan.credential_kind, "api_key");
        assert!(plan.provider_configured);
        assert!(plan.requires_confirmation);
        assert!(!json.contains("openai-key"));
    }

    #[test]
    fn credential_repair_plan_is_absent_when_default_provider_has_credential() {
        let config = test_config(Some("openai-key"));

        assert_eq!(credential_repair_plan(&config), None);
    }

    #[test]
    fn search_diagnostics_reports_default_builtin_provider_without_secrets() {
        let config = test_config(Some("openai-key"));
        let diagnostics = search_diagnostics(&config);
        let json = serde_json::to_string(&diagnostics).unwrap();

        assert_eq!(diagnostics.status, OnboardingStatus::Configured);
        assert_eq!(
            diagnostics
                .builtin_provider
                .as_ref()
                .and_then(|provider| provider.advertised_tool_type.as_deref()),
            Some("web_search_preview")
        );
        assert!(!json.contains("openai-key"));
    }

    #[test]
    fn search_diagnostics_reports_missing_provider_path() {
        let mut config = test_config(Some("openai-key"));
        config.web_config.search.builtin_provider_enabled = false;

        let diagnostics = search_diagnostics(&config);

        assert_eq!(diagnostics.status, OnboardingStatus::Missing);
        assert_eq!(diagnostics.actions.len(), 1);
    }

    #[test]
    fn search_diagnostics_reports_managed_provider_credential_state() {
        let mut config = test_config(Some("openai-key"));
        config.web_config.search.builtin_provider_enabled = false;
        config.web_config.search.provider = "brave_search".into();
        config.web_config.providers.insert(
            "brave_search".into(),
            WebProviderConfig {
                kind: WebProviderKind::Brave,
                base_url: None,
                api_key: String::new(),
                command: None,
                output: None,
                limits: WebProviderLimitsConfig::default(),
            },
        );

        let diagnostics = search_diagnostics(&config);
        let provider = diagnostics.managed_providers.first().unwrap();

        assert_eq!(diagnostics.status, OnboardingStatus::Unavailable);
        assert_eq!(provider.id, "brave_search");
        assert_eq!(provider.auth, "api_key");
        assert!(!provider.credential_configured);
        assert!(!provider.available);
    }

    #[test]
    fn onboarding_report_reuses_search_diagnostics_section() {
        let mut config = test_config(Some("openai-key"));
        config.web_config.search.enabled = true;
        config.web_config.search.builtin_provider_enabled = false;
        config.web_config.search.provider = "duck_duck_go".into();
        config.web_config.search.mode = WebSearchMode::Single;

        let report = onboarding_report(&config);
        let search = report
            .sections
            .iter()
            .find(|section| section.id == "search")
            .expect("search section");

        assert_eq!(search.status, OnboardingStatus::Configured);
        assert_eq!(search.details["provider"], "duck_duck_go");
        assert_eq!(search.details["mode"], "single");
        assert_eq!(
            search.details["managed_providers"][0]["kind"],
            "duck_duck_go"
        );
    }
}
