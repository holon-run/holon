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
}

impl From<&crate::config::WebProviderConfigFile> for WebProviderConfig {
    fn from(value: &crate::config::WebProviderConfigFile) -> Self {
        Self {
            kind: value.kind,
            base_url: value.base_url.clone(),
        }
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
