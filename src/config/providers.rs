use super::*;
pub use holon_provider::config::{
    AnthropicCacheCapabilities, AnthropicCacheStrategy, AnthropicContextManagementConfig,
    CredentialKind, CredentialSource, ProviderAuthConfig, ProviderRuntimeConfig,
    OPENAI_CODEX_CREDENTIAL_PROFILE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAuthMode {
    Auto,
    Required,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlTransportKind {
    Tcp,
    Unix,
}

pub use holon_domain::model_values::{ProviderId, ProviderTransportKind, TransportCapabilities};

pub const DEFAULT_LOCAL_AGENT_ID: &str = "main";

pub type ProviderRegistry = BTreeMap<ProviderId, ProviderRuntimeConfig>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfigView {
    pub id: String,
    pub transport: String,
    pub base_url: String,
    pub auth: ProviderAuthView,
    pub credential_configured: bool,
    pub configured_in_config: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAuthView {
    pub source: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
}

impl ControlAuthMode {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "required" => Ok(Self::Required),
            "disabled" => Ok(Self::Disabled),
            other => Err(anyhow!(
                "invalid HOLON_CONTROL_AUTH_MODE {other}; expected auto|required|disabled"
            )),
        }
    }
}
