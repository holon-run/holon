use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::load_codex_cli_credential,
    config::{
        built_in_provider_default_config, credential_store_path, load_persisted_config_at,
        save_persisted_config_at, set_credential_profile_at, validate_provider_config, AppConfig,
        CredentialKind, CredentialSource, ModelRef, ProviderAuthConfig, ProviderConfigFile,
        ProviderId, ProviderRuntimeConfig,
    },
    model_catalog::BuiltInModelCatalog,
    web::{WebProviderAuthClass, WebProviderSupportStatus},
};

const DUCKDUCKGO_SEARCH_PROVIDER_ID: &str = "duckduckgo";

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
    let persisted_provider_configured = config.stored_config.providers.contains_key(provider_id);
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
        credential_profile: provider
            .and_then(|provider| provider.auth.profile.clone())
            .unwrap_or_else(|| default_credential_profile(provider_id)),
        credential_kind: provider
            .map(|provider| provider.auth.kind)
            .unwrap_or(CredentialKind::ApiKey)
            .as_str()
            .into(),
        provider_configured,
        credential_configured,
        requires_confirmation: persisted_provider_configured,
        summary: if persisted_provider_configured {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingWizardDraft {
    pub provider: ProviderId,
    pub credential_profile: String,
    pub credential_kind: CredentialKind,
    pub default_model: ModelRef,
    pub search: OnboardingSearchSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingSearchSelection {
    Disabled,
    Auto,
    ManagedDuckDuckGo,
}

impl OnboardingSearchSelection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disable WebSearch",
            Self::Auto => "Auto: prefer native search, fallback to managed WebSearch",
            Self::ManagedDuckDuckGo => "Managed WebSearch: DuckDuckGo",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingProviderChoice {
    pub id: ProviderId,
    pub title: String,
    pub detail: String,
    pub credential_kind: CredentialKind,
    pub credential_profile: String,
    pub credential_configured: bool,
    pub codex_home: Option<PathBuf>,
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingModelChoice {
    pub model: ModelRef,
    pub title: String,
    pub detail: String,
    pub custom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingSearchChoice {
    pub selection: OnboardingSearchSelection,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingApplySummary {
    pub applied_via: String,
    pub provider: String,
    pub credential_profile: String,
    pub credential_kind: String,
    pub default_model: String,
    pub search: String,
    pub credential_written: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OnboardingWizardSubmission {
    pub draft: OnboardingWizardDraft,
    pub credential_material: Option<String>,
}

pub fn onboarding_provider_choices(config: &AppConfig) -> Vec<OnboardingProviderChoice> {
    config
        .providers
        .values()
        .filter(|provider| should_show_onboarding_provider(config, &provider.id))
        .map(|provider| {
            let auth = match provider.auth.kind {
                CredentialKind::OAuth => "OAuth login/profile",
                CredentialKind::ApiKey => "API key",
                CredentialKind::None => "no credential",
                _ => provider.auth.kind.as_str(),
            };
            OnboardingProviderChoice {
                id: provider.id.clone(),
                title: provider_title(&provider.id),
                detail: format!("{} · {}", provider.transport.as_str(), auth),
                credential_kind: provider.auth.kind,
                credential_profile: provider
                    .auth
                    .profile
                    .clone()
                    .unwrap_or_else(|| provider.id.as_str().to_string()),
                credential_configured: provider_credential_configured(provider),
                codex_home: provider.codex_home.clone(),
                configured: config.stored_config.providers.contains_key(&provider.id),
            }
        })
        .collect()
}

fn should_show_onboarding_provider(config: &AppConfig, provider_id: &ProviderId) -> bool {
    canonical_onboarding_provider_id(provider_id)
        .map(|canonical_id| {
            canonical_id == *provider_id || !config.providers.contains_key(&canonical_id)
        })
        .unwrap_or(true)
}

fn canonical_onboarding_provider_id(provider_id: &ProviderId) -> Option<ProviderId> {
    let id = provider_id.as_str();
    for suffix in ["-openai", "-anthropic"] {
        if let Some(base) = id.strip_suffix(suffix) {
            return ProviderId::parse(base).ok();
        }
    }
    None
}

fn provider_title(provider_id: &ProviderId) -> String {
    match provider_id.as_str() {
        ProviderId::OPENAI_CODEX => "OpenAI Codex".to_string(),
        ProviderId::OPENAI => "OpenAI".to_string(),
        ProviderId::ANTHROPIC => "Anthropic".to_string(),
        ProviderId::GEMINI => "Gemini".to_string(),
        other => other
            .split(['-', '_'])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn provider_credential_configured(provider: &ProviderRuntimeConfig) -> bool {
    provider.has_configured_credential()
        || provider.auth.source == CredentialSource::None
        || (provider.id.is_openai_codex()
            && provider.auth.external.as_deref() == Some("codex_cli")
            && provider
                .codex_home
                .as_deref()
                .map(|home| load_codex_cli_credential(home).is_ok())
                .unwrap_or(false))
}

pub fn onboarding_model_choices(
    config: &AppConfig,
    provider: &ProviderId,
) -> Vec<OnboardingModelChoice> {
    let mut models = configured_model_choices(config, provider);
    for choice in RuntimeModelCatalogShim::choices(config, provider) {
        if !models.iter().any(|existing| existing.model == choice.model) {
            models.push(choice);
        }
    }
    if models.is_empty() {
        models.push(OnboardingModelChoice {
            model: ModelRef::new(provider.clone(), "unknown"),
            title: format!("{}/unknown", provider.as_str()),
            detail: "custom provider model placeholder".into(),
            custom: false,
        });
    }
    models.push(OnboardingModelChoice {
        model: ModelRef::new(provider.clone(), "__custom__"),
        title: "Custom model…".into(),
        detail: format!("enter any {} model id", provider.as_str()),
        custom: true,
    });
    models
}

fn configured_model_choices(
    config: &AppConfig,
    provider: &ProviderId,
) -> Vec<OnboardingModelChoice> {
    let mut models = Vec::new();
    push_configured_model_choice(
        &mut models,
        &config.default_model,
        provider,
        "current default model",
    );
    for model in &config.fallback_models {
        push_configured_model_choice(&mut models, model, provider, "configured fallback model");
    }
    let mut override_models = config
        .validated_model_overrides
        .keys()
        .filter(|model| &model.provider == provider)
        .cloned()
        .collect::<Vec<_>>();
    override_models.sort_by_key(ModelRef::as_string);
    for model in override_models {
        push_configured_model_choice(&mut models, &model, provider, "configured model override");
    }
    models
}

fn push_configured_model_choice(
    models: &mut Vec<OnboardingModelChoice>,
    model: &ModelRef,
    provider: &ProviderId,
    detail: &str,
) {
    if &model.provider != provider || models.iter().any(|choice| choice.model == *model) {
        return;
    }
    models.push(OnboardingModelChoice {
        model: model.clone(),
        title: model.as_string(),
        detail: detail.into(),
        custom: false,
    });
}

struct RuntimeModelCatalogShim;

impl RuntimeModelCatalogShim {
    fn choices(config: &AppConfig, provider: &ProviderId) -> Vec<OnboardingModelChoice> {
        let preferred = BuiltInModelCatalog::default().preferred_model_for_provider(provider);
        let mut entries = crate::config::RuntimeModelCatalog::from_config(config)
            .available_models()
            .into_iter()
            .filter(|entry| &entry.model_ref.provider == provider)
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            let left_preferred = preferred.as_ref() == Some(&left.model_ref);
            let right_preferred = preferred.as_ref() == Some(&right.model_ref);
            right_preferred
                .cmp(&left_preferred)
                .then_with(|| left.display_name.cmp(&right.display_name))
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        entries
            .into_iter()
            .map(|entry| {
                let preferred_suffix = if preferred.as_ref() == Some(&entry.model_ref) {
                    " · recommended"
                } else {
                    ""
                };
                OnboardingModelChoice {
                    title: format!("{}{}", entry.display_name, preferred_suffix),
                    detail: entry.model_ref.as_string(),
                    model: entry.model_ref,
                    custom: false,
                }
            })
            .collect()
    }
}

pub fn onboarding_search_choices(config: &AppConfig) -> Vec<OnboardingSearchChoice> {
    let native_detail = if config
        .providers
        .get(&config.default_model.provider)
        .and_then(|provider| provider.builtin_web_search.as_ref())
        .is_some()
    {
        "current provider declares native search; runtime still probes the active model"
    } else {
        "current provider has no native search declaration; managed WebSearch remains available"
    };
    vec![
        OnboardingSearchChoice {
            selection: OnboardingSearchSelection::Auto,
            title: OnboardingSearchSelection::Auto.label().into(),
            detail: format!("{native_detail}; DuckDuckGo is used as the default managed fallback"),
        },
        OnboardingSearchChoice {
            selection: OnboardingSearchSelection::ManagedDuckDuckGo,
            title: OnboardingSearchSelection::ManagedDuckDuckGo.label().into(),
            detail: "global agent WebSearch provider; no per-model native search".into(),
        },
        OnboardingSearchChoice {
            selection: OnboardingSearchSelection::Disabled,
            title: OnboardingSearchSelection::Disabled.label().into(),
            detail: "keep WebSearch off; WebFetch can still fetch explicit URLs".into(),
        },
    ]
}

pub fn apply_onboarding_wizard_draft(
    config: &AppConfig,
    draft: &OnboardingWizardDraft,
    credential_material: Option<String>,
) -> Result<OnboardingApplySummary> {
    let mut persisted = load_persisted_config_at(&config.config_file_path)?;
    let mut provider_config = persisted
        .providers
        .remove(&draft.provider)
        .or_else(|| {
            config
                .providers
                .get(&draft.provider)
                .map(provider_config_file_from_runtime)
        })
        .or_else(|| {
            built_in_provider_default_config(&draft.provider)
                .ok()
                .flatten()
        })
        .with_context(|| {
            format!(
                "provider {} is not configured and has no built-in default",
                draft.provider.as_str()
            )
        })?;
    let credential_written = credential_material
        .map(|mut material| {
            trim_trailing_newlines(&mut material);
            provider_config.auth = ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: draft.credential_kind,
                env: None,
                profile: Some(draft.credential_profile.clone()),
                external: provider_config.auth.external.clone(),
            };
            set_credential_profile_at(
                &credential_store_path(&config.home_dir),
                &draft.credential_profile,
                draft.credential_kind,
                material,
            )
            .map(|_| true)
        })
        .transpose()?
        .unwrap_or(false);
    validate_provider_config(&draft.provider, &provider_config)?;

    persisted.model.default = Some(draft.default_model.as_string());
    persisted
        .providers
        .insert(draft.provider.clone(), provider_config);
    match draft.search {
        OnboardingSearchSelection::Disabled => {
            persisted.web.search.enabled = Some(false);
        }
        OnboardingSearchSelection::Auto => {
            persisted.web.search.enabled = Some(true);
            persisted.web.search.builtin_provider.enabled = Some(true);
            persisted.web.search.provider = Some("auto".into());
            persisted.web.search.providers.clear();
        }
        OnboardingSearchSelection::ManagedDuckDuckGo => {
            persisted.web.search.enabled = Some(true);
            persisted.web.search.builtin_provider.enabled = Some(false);
            persisted.web.search.provider = Some(DUCKDUCKGO_SEARCH_PROVIDER_ID.into());
            persisted.web.search.providers = vec![DUCKDUCKGO_SEARCH_PROVIDER_ID.into()];
        }
    }
    save_persisted_config_at(&config.config_file_path, &persisted)?;

    Ok(OnboardingApplySummary {
        applied_via: "offline_store".into(),
        provider: draft.provider.as_str().into(),
        credential_profile: draft.credential_profile.clone(),
        credential_kind: draft.credential_kind.as_str().into(),
        default_model: draft.default_model.as_string(),
        search: draft.search.label().into(),
        credential_written,
    })
}

fn provider_config_file_from_runtime(provider: &ProviderRuntimeConfig) -> ProviderConfigFile {
    ProviderConfigFile {
        transport: provider.transport,
        base_url: provider.base_url.clone(),
        auth: provider.auth.clone(),
        reasoning_effort: provider.reasoning_effort.clone(),
        builtin_web_search: provider.builtin_web_search.clone(),
    }
}

fn trim_trailing_newlines(value: &mut String) {
    while value.ends_with('\n') || value.ends_with('\r') {
        value.pop();
    }
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
                DUCKDUCKGO_SEARCH_PROVIDER_ID.into(),
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
    if provider_id == DUCKDUCKGO_SEARCH_PROVIDER_ID {
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
        config::{
            credential_store_path, load_credential_store_at, load_persisted_config_at,
            provider_registry_for_tests, AppConfig, ControlAuthMode, CredentialKind,
            CredentialSource, ModelRef, ProviderAuthConfig, ProviderConfigFile, ProviderId,
            ProviderRuntimeConfig, ProviderTransportKind,
        },
        model_catalog::ModelRuntimeOverride,
        onboarding::{
            apply_onboarding_wizard_draft, credential_repair_plan, onboarding_model_choices,
            onboarding_provider_choices, onboarding_report, search_diagnostics,
            OnboardingSearchSelection, OnboardingStatus, OnboardingWizardDraft,
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
            api_cors: Default::default(),
            config_file_path: home_dir.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse("openai/gpt-5.4").unwrap(),
            fallback_models: Vec::new(),
            vision_model: None,
            vision_candidate_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: HashMap::new(),
            validated_unknown_model_fallback: None,
            model_discovery_cache: Default::default(),
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
        assert!(!plan.requires_confirmation);
        assert!(!json.contains("openai-key"));
    }

    #[test]
    fn credential_repair_plan_requires_confirmation_for_persisted_provider_config() {
        let mut config = test_config(None);
        config.stored_config.providers.insert(
            ProviderId::openai(),
            ProviderConfigFile {
                transport: ProviderTransportKind::OpenAiResponses,
                base_url: "https://api.openai.com/v1".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::Env,
                    kind: CredentialKind::ApiKey,
                    env: Some("OPENAI_API_KEY".into()),
                    profile: None,
                    external: None,
                },
                reasoning_effort: None,
                builtin_web_search: None,
            },
        );

        let plan = credential_repair_plan(&config).expect("repair plan");

        assert!(plan.provider_configured);
        assert!(plan.requires_confirmation);
        assert_eq!(
            plan.summary,
            "Update the default model provider to use a stored credential profile."
        );
    }

    #[test]
    fn credential_repair_plan_preserves_provider_auth_contract() {
        let mut config = test_config(None);
        config.default_model = ModelRef::parse("openai-codex/gpt-5.4").unwrap();

        let plan = credential_repair_plan(&config).expect("repair plan");

        assert_eq!(plan.provider, "openai-codex");
        assert_eq!(plan.credential_profile, "openai-codex");
        assert_eq!(plan.credential_kind, "oauth");
        assert!(plan.provider_configured);
        assert!(!plan.requires_confirmation);
    }

    #[test]
    fn credential_repair_plan_marks_unknown_default_provider_unconfigured() {
        let mut config = test_config(None);
        config.default_model = ModelRef::parse("custom/gpt-5.4").unwrap();

        let plan = credential_repair_plan(&config).expect("repair plan");

        assert_eq!(plan.provider, "custom");
        assert_eq!(plan.credential_profile, "custom");
        assert_eq!(plan.credential_kind, "api_key");
        assert!(!plan.provider_configured);
        assert!(!plan.requires_confirmation);
    }

    #[test]
    fn credential_repair_plan_is_absent_when_default_provider_has_credential() {
        let config = test_config(Some("openai-key"));

        assert_eq!(credential_repair_plan(&config), None);
    }

    #[test]
    fn onboarding_wizard_choices_include_provider_auth_and_models() {
        let mut config = test_config(None);
        let custom_provider = ProviderId::parse("custom-openai").unwrap();
        let custom_provider_config = ProviderRuntimeConfig {
            id: custom_provider.clone(),
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://custom.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("CUSTOM_OPENAI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        };
        config
            .providers
            .insert(custom_provider.clone(), custom_provider_config.clone());
        config.stored_config.providers.insert(
            custom_provider.clone(),
            ProviderConfigFile {
                transport: custom_provider_config.transport,
                base_url: custom_provider_config.base_url.clone(),
                auth: custom_provider_config.auth.clone(),
                reasoning_effort: None,
                builtin_web_search: None,
            },
        );
        let deepseek = ProviderId::parse("deepseek").unwrap();
        let deepseek_anthropic = ProviderId::parse("deepseek-anthropic").unwrap();
        let deepseek_openai = ProviderId::parse("deepseek-openai").unwrap();
        let deepseek_config = ProviderRuntimeConfig {
            id: deepseek.clone(),
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: "https://api.deepseek.com/anthropic".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("DEEPSEEK_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        };
        config
            .providers
            .insert(deepseek.clone(), deepseek_config.clone());
        config.providers.insert(
            deepseek_anthropic.clone(),
            ProviderRuntimeConfig {
                id: deepseek_anthropic.clone(),
                ..deepseek_config.clone()
            },
        );
        config.providers.insert(
            deepseek_openai.clone(),
            ProviderRuntimeConfig {
                id: deepseek_openai.clone(),
                transport: ProviderTransportKind::OpenAiChatCompletions,
                base_url: "https://api.deepseek.com/v1".into(),
                ..deepseek_config
            },
        );
        config.default_model = ModelRef::parse("openai/custom-default").unwrap();
        config.fallback_models = vec![ModelRef::parse("openai/custom-fallback").unwrap()];
        config.validated_model_overrides.insert(
            ModelRef::parse("openai/custom-override").unwrap(),
            ModelRuntimeOverride::default(),
        );

        let providers = onboarding_provider_choices(&config);
        let openai = providers
            .iter()
            .find(|provider| provider.id == ProviderId::openai())
            .expect("openai provider choice");
        let codex = providers
            .iter()
            .find(|provider| provider.id == ProviderId::openai_codex())
            .expect("openai-codex provider choice");
        let custom = providers
            .iter()
            .find(|provider| provider.id == custom_provider)
            .expect("custom provider choice");

        assert_eq!(openai.credential_kind, CredentialKind::ApiKey);
        assert_eq!(openai.credential_profile, "openai");
        assert_eq!(codex.credential_kind, CredentialKind::OAuth);
        assert_eq!(codex.credential_profile, "openai-codex");
        assert_eq!(custom.credential_kind, CredentialKind::ApiKey);
        assert_eq!(custom.credential_profile, "custom-openai");
        assert!(custom.configured);
        assert!(providers.iter().any(|provider| provider.id == deepseek));
        assert!(!providers
            .iter()
            .any(|provider| provider.id == deepseek_anthropic));
        assert!(!providers
            .iter()
            .any(|provider| provider.id == deepseek_openai));

        let models = onboarding_model_choices(&config, &ProviderId::openai());
        assert_eq!(models[0].model.as_string(), "openai/custom-default");
        assert_eq!(models[1].model.as_string(), "openai/custom-fallback");
        assert_eq!(models[2].model.as_string(), "openai/custom-override");
        assert!(models
            .iter()
            .any(|choice| choice.model.as_string() == "openai/gpt-5.4"));
        assert!(models
            .iter()
            .any(|choice| choice.custom && choice.title == "Custom model…"));
    }

    #[test]
    fn onboarding_wizard_apply_writes_config_without_printing_secret() {
        let config = test_config(None);
        let draft = OnboardingWizardDraft {
            provider: ProviderId::openai(),
            credential_profile: "openai".into(),
            credential_kind: CredentialKind::ApiKey,
            default_model: ModelRef::parse("openai/gpt-5.4").unwrap(),
            search: OnboardingSearchSelection::ManagedDuckDuckGo,
        };

        let summary =
            apply_onboarding_wizard_draft(&config, &draft, Some("test-secret\n".into())).unwrap();
        let persisted = load_persisted_config_at(&config.config_file_path).unwrap();
        let store = load_credential_store_at(&credential_store_path(&config.home_dir)).unwrap();

        assert_eq!(summary.provider, "openai");
        assert_eq!(summary.applied_via, "offline_store");
        assert_eq!(summary.default_model, "openai/gpt-5.4");
        assert_eq!(summary.search, "Managed WebSearch: DuckDuckGo");
        assert!(summary.credential_written);
        assert!(!format!("{summary:?}").contains("test-secret"));

        assert_eq!(persisted.model.default.as_deref(), Some("openai/gpt-5.4"));
        assert_eq!(persisted.web.search.enabled, Some(true));
        assert_eq!(persisted.web.search.builtin_provider.enabled, Some(false));
        assert_eq!(persisted.web.search.provider.as_deref(), Some("duckduckgo"));
        assert_eq!(persisted.web.search.providers, vec!["duckduckgo"]);
        assert_eq!(
            persisted
                .providers
                .get(&ProviderId::openai())
                .and_then(|provider| provider.auth.profile.as_deref()),
            Some("openai")
        );
        assert_eq!(
            store.profiles.get("openai").map(|profile| profile.kind),
            Some(CredentialKind::ApiKey)
        );
        assert_eq!(
            store
                .profiles
                .get("openai")
                .map(|profile| profile.material.as_str()),
            Some("test-secret")
        );
    }

    #[test]
    fn onboarding_wizard_apply_preserves_existing_auth_without_new_secret() {
        let config = test_config(Some("openai-key"));
        let draft = OnboardingWizardDraft {
            provider: ProviderId::openai(),
            credential_profile: "openai".into(),
            credential_kind: CredentialKind::ApiKey,
            default_model: ModelRef::parse("openai/gpt-5.4").unwrap(),
            search: OnboardingSearchSelection::Auto,
        };

        let summary = apply_onboarding_wizard_draft(&config, &draft, None).unwrap();
        let persisted = load_persisted_config_at(&config.config_file_path).unwrap();
        let openai = persisted.providers.get(&ProviderId::openai()).unwrap();
        let store = load_credential_store_at(&credential_store_path(&config.home_dir)).unwrap();

        assert!(!summary.credential_written);
        assert_eq!(openai.auth.source, CredentialSource::Env);
        assert_eq!(openai.auth.env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(openai.auth.profile, None);
        assert!(store.profiles.is_empty());
        assert_eq!(persisted.web.search.provider.as_deref(), Some("auto"));
        assert!(persisted.web.search.providers.is_empty());
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
        config.web_config.search.provider = "duckduckgo".into();
        config.web_config.search.mode = WebSearchMode::Single;

        let report = onboarding_report(&config);
        let search = report
            .sections
            .iter()
            .find(|section| section.id == "search")
            .expect("search section");

        assert_eq!(search.status, OnboardingStatus::Configured);
        assert_eq!(search.details["provider"], "duckduckgo");
        assert_eq!(search.details["mode"], "single");
        assert_eq!(
            search.details["managed_providers"][0]["kind"],
            "duck_duck_go"
        );
    }
}
