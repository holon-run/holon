use crate::config::ModelRouteRef;
use crate::model_catalog::ResolvedRuntimeModelPolicy;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentModelRequest {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fallback: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelResolutionStatus {
    Inherited,
    Accepted,
    Normalized,
    FallbackUsed,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentModelResolution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<AgentModelRequest>,
    pub resolved_provider: String,
    pub resolved_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_parameters: Option<serde_json::Map<String, serde_json::Value>>,
    pub resolution_status: AgentModelResolutionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelSource {
    RuntimeDefault,
    AgentOverride,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentModelState {
    pub source: AgentModelSource,
    pub runtime_default_model: ModelRouteRef,
    pub effective_model: ModelRouteRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<ModelRouteRef>,
    #[serde(default)]
    pub fallback_active: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effective_fallback_models: Vec<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_model: Option<ModelRouteRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_service_tier: Option<crate::config::ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_service_tier: Option<crate::config::ServiceTier>,
    #[serde(default)]
    pub resolved_policy: ResolvedRuntimeModelPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedModelAvailability {
    pub model: String,
    pub provider: String,
    pub provider_family: String,
    pub endpoint: String,
    pub route_provider: String,
    pub display_name: String,
    pub metadata_source: String,
    pub provider_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<String>,
    pub credential_configured: bool,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_disposition: Option<String>,
    pub policy: ResolvedRuntimeModelPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_capabilities: Option<crate::config::ResolvedModelCapabilities>,
    #[serde(default)]
    pub media: crate::provider::media::MediaModelCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderAvailability {
    Available,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProviderEntry {
    pub id: String,
    pub provider_family: String,
    pub endpoint: String,
    pub route_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub availability: ModelProviderAvailability,
    pub provider_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<String>,
    pub credential_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub model_count: usize,
    pub discovered_model_count: usize,
    #[serde(default)]
    pub media: crate::provider::media::MediaProviderCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderModelEntry {
    pub provider: String,
    pub provider_family: String,
    pub endpoint: String,
    pub route_provider: String,
    pub id: String,
    pub model_ref: String,
    pub display_name: String,
    pub availability: ModelAvailability,
    pub selectable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub metadata_source: String,
    pub policy: ResolvedRuntimeModelPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameter_contracts: Vec<crate::model_catalog::ModelParameterSupport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_parameters: Vec<String>,
    #[serde(default)]
    pub media: crate::provider::media::MediaModelCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_notes: Vec<String>,
}
