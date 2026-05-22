use crate::config::CredentialStoreFile;

pub mod fetch;
pub mod policy;
pub mod search;

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebConfig {
    pub fetch: WebFetchConfig,
    pub search: WebSearchConfig,
    pub providers: BTreeMap<String, WebProviderConfig>,
}

impl WebConfig {
    pub fn native_search_provider(&self) -> Option<(String, WebProviderKind)> {
        let provider_id = self.search.provider.trim();
        if !self.search.enabled || provider_id.is_empty() || provider_id == "auto" {
            return None;
        }
        self.providers.get(provider_id).and_then(|provider| {
            provider
                .kind
                .is_native_search()
                .then(|| (provider_id.to_string(), provider.kind))
        })
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            fetch: WebFetchConfig::default(),
            search: WebSearchConfig::default(),
            providers: BTreeMap::new(),
        }
    }
}

impl TryFrom<&crate::config::WebConfigFile> for WebConfig {
    type Error = anyhow::Error;

    fn try_from(value: &crate::config::WebConfigFile) -> Result<Self> {
        Ok(Self {
            fetch: WebFetchConfig::from(&value.fetch),
            search: WebSearchConfig::from(&value.search),
            providers: value
                .providers
                .iter()
                .map(|(id, provider)| {
                    WebProviderConfig::from_file(id, provider)
                        .map(|provider| (id.clone(), provider))
                })
                .collect::<Result<BTreeMap<_, _>>>()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebFetchConfig {
    pub enabled: bool,
    pub max_chars: usize,
    pub max_response_bytes: usize,
    pub timeout_seconds: u64,
    pub max_redirects: usize,
    pub allowed_hosts: Vec<String>,
    pub denied_hosts: Vec<String>,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_chars: 20_000,
            max_response_bytes: 750_000,
            timeout_seconds: 20,
            max_redirects: 5,
            allowed_hosts: Vec::new(),
            denied_hosts: Vec::new(),
        }
    }
}

impl From<&crate::config::WebFetchConfigFile> for WebFetchConfig {
    fn from(value: &crate::config::WebFetchConfigFile) -> Self {
        let fallback = Self::default();
        Self {
            enabled: value.enabled.unwrap_or(fallback.enabled),
            max_chars: value.max_chars.unwrap_or(fallback.max_chars),
            max_response_bytes: value
                .max_response_bytes
                .unwrap_or(fallback.max_response_bytes),
            timeout_seconds: value.timeout_seconds.unwrap_or(fallback.timeout_seconds),
            max_redirects: value.max_redirects.unwrap_or(fallback.max_redirects),
            allowed_hosts: value.allowed_hosts.clone(),
            denied_hosts: value.denied_hosts.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchConfig {
    pub enabled: bool,
    pub builtin_provider_enabled: bool,
    pub provider: String,
    pub mode: WebSearchMode,
    pub providers: Vec<String>,
    pub max_results: usize,
    pub max_provider_attempts: usize,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtin_provider_enabled: true,
            provider: "auto".into(),
            mode: WebSearchMode::Fallback,
            providers: Vec::new(),
            max_results: 5,
            max_provider_attempts: 3,
        }
    }
}

impl From<&crate::config::WebSearchConfigFile> for WebSearchConfig {
    fn from(value: &crate::config::WebSearchConfigFile) -> Self {
        let fallback = Self::default();
        Self {
            enabled: value.enabled.unwrap_or(fallback.enabled),
            builtin_provider_enabled: value
                .builtin_provider
                .enabled
                .unwrap_or(fallback.builtin_provider_enabled),
            provider: value
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|provider| !provider.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or(fallback.provider),
            mode: value.mode.unwrap_or(fallback.mode),
            providers: value
                .providers
                .iter()
                .map(|provider| provider.trim().to_string())
                .filter(|provider| !provider.is_empty())
                .collect(),
            max_results: value.max_results.unwrap_or(fallback.max_results),
            max_provider_attempts: value
                .max_provider_attempts
                .unwrap_or(fallback.max_provider_attempts)
                .max(1),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchMode {
    Single,
    Fallback,
    Aggregate,
}

impl WebSearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Fallback => "fallback",
            Self::Aggregate => "aggregate",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderAuthClass {
    None,
    ApiKey,
    NativeProvider,
    SelfHosted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderCostClass {
    Free,
    SelfHosted,
    Paid,
    ProviderMetered,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderQualityHint {
    HtmlFallback,
    Keyword,
    Semantic,
    Research,
    Native,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderSupportStatus {
    Supported,
    Unsupported,
    NativeOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebProviderCapabilityMetadata {
    pub auth: WebProviderAuthClass,
    pub cost_class: WebProviderCostClass,
    pub quality_hint: WebProviderQualityHint,
    pub supports_domain_filter: bool,
    pub supports_freshness: bool,
    pub supports_region_or_language: bool,
    pub supports_full_content: bool,
    pub supports_native_citations: bool,
    pub default_priority: u16,
    pub status: WebProviderSupportStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebProviderConfig {
    pub kind: WebProviderKind,
    pub base_url: Option<String>,
    /// Resolved API key from a credential profile (for API-backed providers).
    /// Empty when no credential profile is configured.
    #[serde(skip)]
    pub api_key: String,
    pub command: Option<WebCommandProviderConfig>,
    pub output: Option<WebCommandOutputConfig>,
    pub limits: WebProviderLimitsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCommandProviderConfig {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCommandOutputConfig {
    pub format: WebCommandOutputFormat,
    pub mapping: WebCommandResultMapping,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebCommandOutputFormat {
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCommandResultMapping {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebProviderLimitsConfig {
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl Default for WebProviderLimitsConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            max_output_bytes: 200_000,
        }
    }
}

impl WebProviderConfig {
    fn from_file(id: &str, value: &crate::config::WebProviderConfigFile) -> Result<Self> {
        validate_provider_config(id, value)?;
        Ok(Self {
            kind: value.kind,
            base_url: value.base_url.clone(),
            api_key: String::new(),
            command: value
                .command
                .as_ref()
                .map(|command| WebCommandProviderConfig {
                    argv: command.argv.clone(),
                }),
            output: value.output.as_ref().map(|output| WebCommandOutputConfig {
                format: output.format.into(),
                mapping: WebCommandResultMapping {
                    title: output.mapping.title.clone(),
                    url: output.mapping.url.clone(),
                    snippet: output.mapping.snippet.clone(),
                    published_at: output.mapping.published_at.clone(),
                },
            }),
            limits: WebProviderLimitsConfig {
                timeout_ms: value.limits.timeout_ms.unwrap_or(10_000).max(1),
                max_output_bytes: value.limits.max_output_bytes.unwrap_or(200_000).max(1),
            },
        })
    }
}

impl From<crate::config::WebCommandOutputFormatFile> for WebCommandOutputFormat {
    fn from(value: crate::config::WebCommandOutputFormatFile) -> Self {
        match value {
            crate::config::WebCommandOutputFormatFile::Json => Self::Json,
        }
    }
}

/// Materialize a resolved WebConfig from the file config and credential store.
pub fn materialize_web_config(
    file: &crate::config::WebConfigFile,
    credential_store: &CredentialStoreFile,
) -> Result<WebConfig> {
    let providers = file
        .providers
        .iter()
        .map(|(id, provider)| -> Result<(String, WebProviderConfig)> {
            let api_key = provider
                .credential_profile
                .as_deref()
                .and_then(|profile| {
                    credential_store
                        .profiles
                        .get(profile)
                        .filter(|entry| entry.kind == crate::config::CredentialKind::ApiKey)
                })
                .map(|entry| entry.material.clone())
                .unwrap_or_default();
            let mut provider_config = WebProviderConfig::from_file(id, provider)?;
            provider_config.api_key = api_key;
            Ok((id.clone(), provider_config))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    Ok(WebConfig {
        fetch: WebFetchConfig::from(&file.fetch),
        search: WebSearchConfig::from(&file.search),
        providers,
    })
}

fn validate_provider_config(
    id: &str,
    provider: &crate::config::WebProviderConfigFile,
) -> Result<()> {
    if provider.kind != WebProviderKind::Command {
        if provider.command.is_some() {
            return Err(anyhow!(
                "web provider `{id}` kind={} must not configure command.argv; command is only supported for kind=command",
                provider.kind.as_str()
            ));
        }
        if provider.output.is_some() {
            return Err(anyhow!(
                "web provider `{id}` kind={} must not configure output.mapping; command output is only supported for kind=command",
                provider.kind.as_str()
            ));
        }
        if provider.limits.timeout_ms.is_some() || provider.limits.max_output_bytes.is_some() {
            return Err(anyhow!(
                "web provider `{id}` kind={} must not configure command limits; limits are only supported for kind=command",
                provider.kind.as_str()
            ));
        }
        return Ok(());
    }
    let command = provider
        .command
        .as_ref()
        .ok_or_else(|| anyhow!("web provider `{id}` kind=command requires command.argv"))?;
    provider
        .output
        .as_ref()
        .ok_or_else(|| anyhow!("web provider `{id}` kind=command requires output.mapping"))?;
    let binary = command.argv.first().map(|arg| arg.trim()).unwrap_or("");
    if binary.is_empty() {
        return Err(anyhow!(
            "web provider `{id}` command.argv must not be empty"
        ));
    }
    let basename = binary.rsplit(['/', '\\']).next().unwrap_or(binary);
    if matches!(
        basename,
        "sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh"
    ) {
        return Err(anyhow!(
            "web provider `{id}` command binary `{binary}` is unsafe"
        ));
    }
    if command.argv.iter().any(|arg| arg.contains('\0')) {
        return Err(anyhow!(
            "web provider `{id}` command argv contains a NUL byte"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderKind {
    DuckDuckGo,
    Searxng,
    Brave,
    Tavily,
    Exa,
    Perplexity,
    Firecrawl,
    OpenAiNative,
    AnthropicNative,
    GeminiNative,
    Command,
}

impl WebProviderKind {
    pub fn is_native_search(self) -> bool {
        matches!(
            self,
            WebProviderKind::OpenAiNative
                | WebProviderKind::AnthropicNative
                | WebProviderKind::GeminiNative
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            WebProviderKind::DuckDuckGo => "duck_duck_go",
            WebProviderKind::Searxng => "searxng",
            WebProviderKind::Brave => "brave",
            WebProviderKind::Tavily => "tavily",
            WebProviderKind::Exa => "exa",
            WebProviderKind::Perplexity => "perplexity",
            WebProviderKind::Firecrawl => "firecrawl",
            WebProviderKind::OpenAiNative => "open_ai_native",
            WebProviderKind::AnthropicNative => "anthropic_native",
            WebProviderKind::GeminiNative => "gemini_native",
            WebProviderKind::Command => "command",
        }
    }

    pub fn capabilities(&self) -> WebProviderCapabilityMetadata {
        match self {
            WebProviderKind::DuckDuckGo => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::None,
                cost_class: WebProviderCostClass::Free,
                quality_hint: WebProviderQualityHint::HtmlFallback,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 10,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Searxng => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::SelfHosted,
                cost_class: WebProviderCostClass::SelfHosted,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: true,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 50,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Brave => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: true,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 80,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Tavily => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: true,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: true,
                supports_native_citations: false,
                default_priority: 75,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Exa => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Semantic,
                supports_domain_filter: true,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: true,
                supports_native_citations: false,
                default_priority: 70,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Perplexity => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: false,
                supports_freshness: true,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: true,
                default_priority: 60,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::Firecrawl => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::ApiKey,
                cost_class: WebProviderCostClass::Paid,
                quality_hint: WebProviderQualityHint::Research,
                supports_domain_filter: true,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: true,
                supports_native_citations: false,
                default_priority: 55,
                status: WebProviderSupportStatus::Supported,
            },
            WebProviderKind::OpenAiNative
            | WebProviderKind::AnthropicNative
            | WebProviderKind::GeminiNative => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::NativeProvider,
                cost_class: WebProviderCostClass::ProviderMetered,
                quality_hint: WebProviderQualityHint::Native,
                supports_domain_filter: false,
                supports_freshness: true,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: true,
                default_priority: 65,
                status: WebProviderSupportStatus::NativeOnly,
            },
            WebProviderKind::Command => WebProviderCapabilityMetadata {
                auth: WebProviderAuthClass::None,
                cost_class: WebProviderCostClass::Free,
                quality_hint: WebProviderQualityHint::Keyword,
                supports_domain_filter: false,
                supports_freshness: false,
                supports_region_or_language: false,
                supports_full_content: false,
                supports_native_citations: false,
                default_priority: 40,
                status: WebProviderSupportStatus::Supported,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        CredentialProfileFile, CredentialStoreFile, WebCommandProviderConfigFile,
        WebProviderConfigFile,
    };
    use std::collections::BTreeMap;

    fn credential_store_with(profile: &str, material: &str) -> CredentialStoreFile {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            profile.to_string(),
            CredentialProfileFile {
                kind: crate::config::CredentialKind::ApiKey,
                material: material.to_string(),
            },
        );
        CredentialStoreFile { profiles }
    }

    fn provider_file(kind: WebProviderKind) -> WebProviderConfigFile {
        WebProviderConfigFile {
            kind,
            base_url: None,
            credential_profile: None,
            command: None,
            output: None,
            limits: Default::default(),
        }
    }

    #[test]
    fn materialize_resolves_api_key_from_credential_profile() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "my_brave".to_string(),
            WebProviderConfigFile {
                kind: WebProviderKind::Brave,
                base_url: None,
                credential_profile: Some("brave_key".to_string()),
                command: None,
                output: None,
                limits: Default::default(),
            },
        );
        let file = crate::config::WebConfigFile {
            fetch: Default::default(),
            search: Default::default(),
            providers,
        };
        let store = credential_store_with("brave_key", "test-api-key-123");
        let config = materialize_web_config(&file, &store).unwrap();
        let provider = config.providers.get("my_brave").unwrap();
        assert_eq!(provider.api_key, "test-api-key-123");
        assert_eq!(provider.kind, WebProviderKind::Brave);
    }

    #[test]
    fn materialize_empty_api_key_without_credential_profile() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "my_tavily".to_string(),
            WebProviderConfigFile {
                kind: WebProviderKind::Tavily,
                base_url: None,
                credential_profile: None,
                command: None,
                output: None,
                limits: Default::default(),
            },
        );
        let file = crate::config::WebConfigFile {
            fetch: Default::default(),
            search: Default::default(),
            providers,
        };
        let store = CredentialStoreFile::default();
        let config = materialize_web_config(&file, &store).unwrap();
        let provider = config.providers.get("my_tavily").unwrap();
        assert!(provider.api_key.is_empty());
    }

    #[test]
    fn materialize_missing_credential_profile_yields_empty_key() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "my_brave".to_string(),
            WebProviderConfigFile {
                kind: WebProviderKind::Brave,
                base_url: None,
                credential_profile: Some("missing_profile".to_string()),
                command: None,
                output: None,
                limits: Default::default(),
            },
        );
        let file = crate::config::WebConfigFile {
            fetch: Default::default(),
            search: Default::default(),
            providers,
        };
        // Credential store does not contain "missing_profile"
        let store = credential_store_with("other", "irrelevant");
        let config = materialize_web_config(&file, &store).unwrap();
        let provider = config.providers.get("my_brave").unwrap();
        assert!(provider.api_key.is_empty());
    }

    #[test]
    fn materialize_non_api_key_credential_kind_yields_empty_key() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "my_brave".to_string(),
            WebProviderConfigFile {
                kind: WebProviderKind::Brave,
                base_url: None,
                credential_profile: Some("session_token".to_string()),
                command: None,
                output: None,
                limits: Default::default(),
            },
        );
        let file = crate::config::WebConfigFile {
            fetch: Default::default(),
            search: Default::default(),
            providers,
        };
        // Profile exists but kind is SessionToken, not ApiKey
        let mut store = CredentialStoreFile::default();
        store.profiles.insert(
            "session_token".to_string(),
            crate::config::CredentialProfileFile {
                kind: crate::config::CredentialKind::SessionToken,
                material: "some-session-hash".to_string(),
            },
        );
        let config = materialize_web_config(&file, &store).unwrap();
        let provider = config.providers.get("my_brave").unwrap();
        assert!(provider.api_key.is_empty());
    }

    #[test]
    fn web_search_config_trims_primary_provider() {
        let file = crate::config::WebSearchConfigFile {
            provider: Some("  openai-native  ".to_string()),
            ..Default::default()
        };

        let config = WebSearchConfig::from(&file);

        assert_eq!(config.provider, "openai-native");
    }

    #[test]
    fn native_search_provider_uses_normalized_provider_id() {
        let mut config = WebConfig::default();
        config.search.provider = " openai-native ".to_string();
        config.providers.insert(
            "openai-native".to_string(),
            WebProviderConfig {
                kind: WebProviderKind::OpenAiNative,
                base_url: None,
                api_key: String::new(),
                command: None,
                output: None,
                limits: WebProviderLimitsConfig::default(),
            },
        );

        assert_eq!(
            config.native_search_provider(),
            Some(("openai-native".to_string(), WebProviderKind::OpenAiNative))
        );
    }

    #[test]
    fn try_from_web_config_file_propagates_provider_errors() {
        let mut file = crate::config::WebConfigFile::default();
        let mut provider = provider_file(WebProviderKind::Command);
        provider.command = Some(WebCommandProviderConfigFile {
            argv: vec!["search".to_string()],
        });
        file.providers.insert("cmd".to_string(), provider);

        let error = WebConfig::try_from(&file).unwrap_err().to_string();

        assert!(error.contains("requires output.mapping"));
    }

    #[test]
    fn materialize_rejects_command_fields_on_non_command_provider() {
        let mut file = crate::config::WebConfigFile::default();
        let mut provider = provider_file(WebProviderKind::Brave);
        provider.command = Some(WebCommandProviderConfigFile {
            argv: vec!["search".to_string()],
        });
        file.providers.insert("brave".to_string(), provider);

        let error = materialize_web_config(&file, &CredentialStoreFile::default())
            .unwrap_err()
            .to_string();

        assert!(error.contains("must not configure command.argv"));
    }

    #[test]
    fn materialize_rejects_command_limits_on_non_command_provider() {
        let mut file = crate::config::WebConfigFile::default();
        let mut provider = provider_file(WebProviderKind::Brave);
        provider.limits.timeout_ms = Some(1000);
        file.providers.insert("brave".to_string(), provider);

        let error = materialize_web_config(&file, &CredentialStoreFile::default())
            .unwrap_err()
            .to_string();

        assert!(error.contains("must not configure command limits"));
    }

    #[test]
    fn provider_capabilities_mark_reserved_kinds() {
        assert_eq!(
            WebProviderKind::OpenAiNative.capabilities().status,
            WebProviderSupportStatus::NativeOnly
        );
        assert_eq!(WebProviderKind::Brave.capabilities().default_priority, 80);
        assert_eq!(
            WebProviderKind::Command.capabilities().status,
            WebProviderSupportStatus::Supported
        );
    }
}
