//! Web fetch/search provider config file types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::web::{WebProviderKind, WebSearchMode};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebConfigFile {
    #[serde(default, skip_serializing_if = "WebFetchConfigFile::is_empty")]
    pub fetch: WebFetchConfigFile,
    #[serde(default, skip_serializing_if = "WebSearchConfigFile::is_empty")]
    pub search: WebSearchConfigFile,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, WebProviderConfigFile>,
}

impl WebConfigFile {
    pub fn is_empty(&self) -> bool {
        self.fetch.is_empty() && self.search.is_empty() && self.providers.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebFetchConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_response_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_hosts: Vec<String>,
}

impl WebFetchConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.max_chars.is_none()
            && self.max_response_bytes.is_none()
            && self.timeout_seconds.is_none()
            && self.max_redirects.is_none()
            && self.allowed_hosts.is_empty()
            && self.denied_hosts.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebSearchConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "WebSearchBuiltinProviderConfigFile::is_empty"
    )]
    pub builtin_provider: WebSearchBuiltinProviderConfigFile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<WebSearchMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_provider_attempts: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebSearchBuiltinProviderConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl WebSearchBuiltinProviderConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

impl WebSearchConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.builtin_provider.is_empty()
            && self.provider.is_none()
            && self.mode.is_none()
            && self.providers.is_empty()
            && self.max_results.is_none()
            && self.max_provider_attempts.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebProviderConfigFile {
    pub kind: WebProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Named credential profile to load the API key from.
    /// When set, the profile must exist in the credential store
    /// and must be of kind `api_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<WebCommandProviderConfigFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<WebCommandOutputConfigFile>,
    #[serde(
        default,
        skip_serializing_if = "WebProviderLimitsConfigFile::is_default"
    )]
    pub limits: WebProviderLimitsConfigFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCommandProviderConfigFile {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCommandOutputConfigFile {
    #[serde(default)]
    pub format: WebCommandOutputFormatFile,
    pub mapping: WebCommandResultMappingFile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebCommandOutputFormatFile {
    #[default]
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCommandResultMappingFile {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct WebProviderLimitsConfigFile {
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
}

impl WebProviderLimitsConfigFile {
    pub fn is_default(value: &Self) -> bool {
        value == &Self::default()
    }
}
