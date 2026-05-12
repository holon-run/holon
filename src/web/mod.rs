use crate::config::CredentialStoreFile;

pub mod fetch;
pub mod policy;
pub mod search;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebConfig {
    pub fetch: WebFetchConfig,
    pub search: WebSearchConfig,
    pub providers: BTreeMap<String, WebProviderConfig>,
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

impl From<&crate::config::WebConfigFile> for WebConfig {
    fn from(value: &crate::config::WebConfigFile) -> Self {
        Self {
            fetch: WebFetchConfig::from(&value.fetch),
            search: WebSearchConfig::from(&value.search),
            providers: value
                .providers
                .iter()
                .map(|(id, provider)| (id.clone(), WebProviderConfig::from(provider)))
                .collect(),
        }
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
    pub provider: String,
    pub max_results: usize,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "auto".into(),
            max_results: 5,
        }
    }
}

impl From<&crate::config::WebSearchConfigFile> for WebSearchConfig {
    fn from(value: &crate::config::WebSearchConfigFile) -> Self {
        let fallback = Self::default();
        Self {
            enabled: value.enabled.unwrap_or(fallback.enabled),
            provider: value
                .provider
                .clone()
                .filter(|provider| !provider.trim().is_empty())
                .unwrap_or(fallback.provider),
            max_results: value.max_results.unwrap_or(fallback.max_results),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebProviderConfig {
    pub kind: WebProviderKind,
    pub base_url: Option<String>,
    /// Resolved API key from a credential profile (for API-backed providers).
    /// Empty when no credential profile is configured.
    #[serde(skip)]
    pub api_key: String,
}

impl From<&crate::config::WebProviderConfigFile> for WebProviderConfig {
    fn from(value: &crate::config::WebProviderConfigFile) -> Self {
        Self {
            kind: value.kind,
            base_url: value.base_url.clone(),
            api_key: String::new(),
        }
    }
}

/// Materialize a resolved WebConfig from the file config and credential store.
pub fn materialize_web_config(
    file: &crate::config::WebConfigFile,
    credential_store: &CredentialStoreFile,
) -> WebConfig {
    WebConfig {
        fetch: WebFetchConfig::from(&file.fetch),
        search: WebSearchConfig::from(&file.search),
        providers: file
            .providers
            .iter()
            .map(|(id, provider)| {
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
                (
                    id.clone(),
                    WebProviderConfig {
                        kind: provider.kind,
                        base_url: provider.base_url.clone(),
                        api_key,
                    },
                )
            })
            .collect(),
    }
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
}

impl WebProviderKind {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CredentialProfileFile, CredentialStoreFile, WebProviderConfigFile};
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

    #[test]
    fn materialize_resolves_api_key_from_credential_profile() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "my_brave".to_string(),
            WebProviderConfigFile {
                kind: WebProviderKind::Brave,
                base_url: None,
                credential_profile: Some("brave_key".to_string()),
            },
        );
        let file = crate::config::WebConfigFile {
            fetch: Default::default(),
            search: Default::default(),
            providers,
        };
        let store = credential_store_with("brave_key", "test-api-key-123");
        let config = materialize_web_config(&file, &store);
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
            },
        );
        let file = crate::config::WebConfigFile {
            fetch: Default::default(),
            search: Default::default(),
            providers,
        };
        let store = CredentialStoreFile::default();
        let config = materialize_web_config(&file, &store);
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
            },
        );
        let file = crate::config::WebConfigFile {
            fetch: Default::default(),
            search: Default::default(),
            providers,
        };
        // Credential store does not contain "missing_profile"
        let store = credential_store_with("other", "irrelevant");
        let config = materialize_web_config(&file, &store);
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
        let config = materialize_web_config(&file, &store);
        let provider = config.providers.get("my_brave").unwrap();
        assert!(provider.api_key.is_empty());
    }
}
