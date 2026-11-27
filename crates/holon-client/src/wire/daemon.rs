use std::path::PathBuf;

use chrono::{DateTime, Utc};
use holon_config::web_config::WebProviderCapabilityMetadata;
use holon_domain::RuntimeFailureSummary;
use holon_storage::RuntimeDbProtectionStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// The application resolves the configuration; the wire crate owns its projected shape.
pub trait RuntimeConfigSource {
    fn config_surface(&self) -> RuntimeConfigSurface;
    fn decision_surface(&self) -> RuntimeDecisionSurface;
}

impl<T: RuntimeConfigSource> RuntimeConfigSource for std::sync::Arc<T> {
    fn config_surface(&self) -> RuntimeConfigSurface {
        self.as_ref().config_surface()
    }

    fn decision_surface(&self) -> RuntimeDecisionSurface {
        self.as_ref().decision_surface()
    }
}

impl RuntimeConfigSurface {
    pub fn new(config: &impl RuntimeConfigSource) -> Self {
        config.config_surface()
    }
}

impl RuntimeDecisionSurface {
    pub fn from_app_config(config: &impl RuntimeConfigSource) -> Self {
        config.decision_surface()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStatusResponse {
    pub ok: bool,
    pub healthy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<TunnelStatus>,
    pub pid: u32,
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub http_addr: String,
    pub started_at: DateTime<Utc>,
    pub config_fingerprint: String,
    #[serde(default)]
    pub product_version: String,
    #[serde(default)]
    pub control_protocol_version: u32,
    #[serde(default)]
    pub lifecycle_owner: DaemonLifecycleOwner,
    #[serde(default)]
    pub executable_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<RuntimeActivitySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_surface: Option<RuntimeStartupSurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_surface: Option<RuntimeConfigSurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<RuntimeFailureSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_db_protection: Option<RuntimeDbProtectionStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStartupSurface {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_home_dir: Option<PathBuf>,
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub workspace_dir: PathBuf,
    pub default_agent_id: String,
    #[serde(default)]
    pub callback_base_url: String,
    pub control_token_configured: bool,
    pub control_auth_mode: RuntimeControlAuthMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlAuthMode {
    Auto,
    Required,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigSurface {
    pub model_default: String,
    pub model_fallbacks: Vec<String>,
    pub vision_default: Option<String>,
    pub image_generation_default: Option<String>,
    #[serde(default)]
    pub decision: RuntimeDecisionSurface,
    pub model_catalog: Vec<String>,
    pub unknown_model_fallback_configured: bool,
    pub runtime_max_output_tokens: u32,
    pub default_tool_output_tokens: u32,
    pub max_tool_output_tokens: u32,
    pub command_task_output_retention_bytes: u64,
    pub command_task_output_quota_bytes: u64,
    pub command_task_min_free_disk_bytes: u64,
    pub command_task_min_free_disk_percent: u8,
    pub disable_provider_fallback: bool,
    pub runtime_db_retention: holon_storage::retention_config::RuntimeDbRetentionPolicy,
    pub providers: Vec<RuntimeProviderSummary>,
    pub web_search: RuntimeWebSearchSummary,
    #[serde(default)]
    pub available_search_provider_kinds: Vec<RuntimeWebSearchProviderKindSummary>,
    pub web_search_providers: Vec<RuntimeWebSearchProviderSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeDecisionSurface {
    pub enabled: bool,
    pub model: Option<String>,
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default)]
    pub decision_capable: bool,
    #[serde(default)]
    pub credential_configured: bool,
    #[serde(default)]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub local_onnx: RuntimeDecisionLocalOnnxSurface,
    #[serde(default)]
    pub tools: RuntimeDecisionToolsSurface,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeDecisionLocalOnnxSurface {
    pub enabled: bool,
    pub preset: Option<String>,
    pub model_dir: Option<String>,
    pub variant: Option<String>,
    pub num_threads: Option<usize>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeDecisionToolsSurface {
    pub enabled: bool,
    pub max_calls_per_turn: Option<usize>,
    pub timeout_ms: u64,
    pub min_confidence_percent: u8,
}

impl Default for RuntimeDecisionToolsSurface {
    fn default() -> Self {
        Self {
            enabled: false,
            max_calls_per_turn: None,
            timeout_ms: 1500,
            min_confidence_percent: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeProviderSummary {
    pub id: String,
    /// Endpoint this entry routes to; `default` is the provider entry point.
    #[serde(default)]
    pub endpoint: String,
    /// Route provider this entry belongs to.
    #[serde(default)]
    pub route_provider: String,
    /// `primary` for first-class providers, `advanced` for explicitly
    /// configured internal protocol sub-endpoints.
    #[serde(default)]
    pub exposure: String,
    pub transport: String,
    pub base_url: String,
    pub oauth_supported: bool,
    pub api_key_supported: bool,
    pub credential_source: String,
    pub credential_kind: String,
    pub credential_env: Option<String>,
    pub credential_profile: Option<String>,
    pub credential_external: Option<String>,
    pub credential_configured: bool,
    pub configured_in_config: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<RuntimeProviderDiscoverySummary>,
}

/// Effective model-discovery settings for a provider as resolved at runtime.
/// `None` on [`RuntimeProviderSummary`] means nothing is configured and the
/// built-in/transport default still applies.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeProviderDiscoverySummary {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Resolved decoder name (`openai_compatible`, `magpie`, ...). Kept as a
    /// string so the wire surface stays stable if more decoders are added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeWebSearchSummary {
    pub enabled: bool,
    pub builtin_provider_enabled: bool,
    pub provider: String,
    pub mode: String,
    pub providers: Vec<String>,
    pub max_results: usize,
    pub max_provider_attempts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeWebSearchProviderSummary {
    pub id: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub engine_id: Option<String>,
    pub credential_profile: Option<String>,
    pub credential_configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeWebSearchProviderKindSummary {
    pub kind: String,
    pub capabilities: WebProviderCapabilityMetadata,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeActivityState {
    Idle,
    Waiting,
    Processing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeActivitySummary {
    pub state: RuntimeActivityState,
    pub active_agent_count: usize,
    pub active_task_count: usize,
    pub processing_agent_count: usize,
    pub waiting_agent_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeShutdownResponse {
    pub ok: bool,
    pub pid: u32,
    pub home_dir: PathBuf,
    pub shutdown_requested: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonLifecycleOwner {
    #[default]
    Standalone,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TunnelPhase {
    Starting,
    Connected,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelStatus {
    pub phase: TunnelPhase,
    pub public_url: String,
    pub message: String,
}

impl From<holon_config::ControlAuthMode> for RuntimeControlAuthMode {
    fn from(value: holon_config::ControlAuthMode) -> Self {
        use holon_config::ControlAuthMode;
        match value {
            ControlAuthMode::Auto => Self::Auto,
            ControlAuthMode::Required => Self::Required,
            ControlAuthMode::Disabled => Self::Disabled,
        }
    }
}
