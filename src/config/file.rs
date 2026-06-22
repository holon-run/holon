//! TOML/JSON config file struct definitions.

use std::collections::BTreeMap;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use anyhow::{anyhow, Result};

use super::providers::ProvidersConfigFile;
pub use super::web::WebConfigFile;
use crate::model_catalog::ModelRuntimeOverride;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HolonConfigFile {
    #[serde(default, skip_serializing_if = "ApiConfigFile::is_empty")]
    pub api: ApiConfigFile,
    #[serde(default, skip_serializing_if = "ModelConfigFile::is_empty")]
    pub model: ModelConfigFile,
    #[serde(default, skip_serializing_if = "ModelsConfigFile::is_empty")]
    pub models: ModelsConfigFile,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: ProvidersConfigFile,
    #[serde(default, skip_serializing_if = "RuntimeConfigFile::is_empty")]
    pub runtime: RuntimeConfigFile,
    #[serde(default, skip_serializing_if = "VisionConfigFile::is_empty")]
    pub vision: VisionConfigFile,
    #[serde(default, skip_serializing_if = "TuiConfigFile::is_empty")]
    pub tui: TuiConfigFile,
    #[serde(default, skip_serializing_if = "WebConfigFile::is_empty")]
    pub web: WebConfigFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ApiConfigFile {
    #[serde(default, skip_serializing_if = "ApiCorsConfigFile::is_empty")]
    pub cors: ApiCorsConfigFile,
}

impl ApiConfigFile {
    pub fn is_empty(&self) -> bool {
        self.cors.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiCorsConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_headers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_credentials: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_seconds: Option<u64>,
}

impl Default for ApiCorsConfigFile {
    fn default() -> Self {
        Self {
            enabled: None,
            allowed_origins: vec![],
            allowed_methods: default_api_cors_allowed_methods(),
            allowed_headers: default_api_cors_allowed_headers(),
            allow_credentials: None,
            max_age_seconds: Some(600),
        }
    }
}

impl ApiCorsConfigFile {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.allowed_origins.is_empty()
            && self.allowed_methods == default_api_cors_allowed_methods()
            && self.allowed_headers == default_api_cors_allowed_headers()
            && self.allow_credentials.is_none()
            && self.max_age_seconds == Some(600)
    }

    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn allow_credentials(&self) -> bool {
        self.allow_credentials.unwrap_or(false)
    }

    pub fn max_age_seconds(&self) -> u64 {
        self.max_age_seconds.unwrap_or(600)
    }
}

pub fn default_api_cors_allowed_methods() -> Vec<String> {
    ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn default_api_cors_allowed_headers() -> Vec<String> {
    ["content-type", "authorization"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_fallback: Option<ModelRuntimeOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelsConfigFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub catalog: BTreeMap<String, ModelRuntimeOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VisionConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_tool_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_provider_fallback: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TuiConfigFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_screen: Option<AltScreenMode>,
}

#[derive(Debug, Deserialize)]
pub struct ClaudeSettings {
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigSchemaEntry {
    pub key: &'static str,
    pub kind: &'static str,
    pub description: &'static str,
    pub default: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AltScreenMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl AltScreenMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(anyhow!(
                "invalid alternate screen mode {other}; expected auto|always|never"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

impl RuntimeConfigFile {
    fn is_empty(&self) -> bool {
        self.max_output_tokens.is_none() && self.disable_provider_fallback.is_none()
    }
}

impl TuiConfigFile {
    fn is_empty(&self) -> bool {
        self.alternate_screen.is_none()
    }
}
