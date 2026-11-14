use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

#[path = "provider_registry.rs"]
mod provider_registry;
pub use provider_registry::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataSource {
    BuiltInCatalog,
    ConservativeBuiltin,
    ConfigOverride,
    RemoteDiscovered,
    /// Checked-in `models.dev/supplemental_catalog.json` entry, drafted by
    /// `holon models-dev refresh` for already-supported providers and
    /// admitted through PR review.
    ModelsDevSupplement,
    UnknownFallback,
}

impl Default for ModelMetadataSource {
    fn default() -> Self {
        Self::UnknownFallback
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataOrigin {
    ExplicitOverride,
    EnvironmentOverride,
    RemoteDiscovered,
    RouteBuiltin,
    ModelBuiltin,
    UnknownFallback,
    RuntimeDefault,
    Derived,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataField {
    DisplayName,
    Description,
    ContextWindowTokens,
    EffectiveContextWindowPercent,
    AutoCompactTokenLimit,
    PromptBudgetEstimatedTokens,
    CompactionTriggerEstimatedTokens,
    CompactionKeepRecentEstimatedTokens,
    RuntimeMaxOutputTokens,
    Verbosity,
    ToolOutputTruncationEstimatedTokens,
    MaxOutputTokensUpperLimit,
    AgentTurn,
    ParallelToolCalls,
    ImageInput,
    ImageGeneration,
    SupportsReasoning,
    InteractiveExec,
    ReasoningEffortOptions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataConstraintKind {
    Clamped,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataConstraintSource {
    ModelUpperLimit,
    RuntimeBudget,
    EndpointPolicy,
    TransportCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelMetadataConstraint {
    pub field: ModelMetadataField,
    pub kind: ModelMetadataConstraintKind,
    pub source: ModelMetadataConstraintSource,
    pub requested: String,
    pub effective: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModelMetadataEvidence {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<ModelMetadataField, ModelMetadataOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ModelMetadataConstraint>,
}

impl ModelMetadataEvidence {
    pub fn source_for(&self, field: ModelMetadataField) -> Option<ModelMetadataOrigin> {
        self.fields.get(&field).copied()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilityFlags {
    #[serde(default = "default_agent_turn_capability")]
    pub agent_turn: bool,
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub image_generation: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub interactive_exec: bool,
}

impl Default for ModelCapabilityFlags {
    fn default() -> Self {
        Self {
            agent_turn: default_agent_turn_capability(),
            parallel_tool_calls: false,
            image_input: false,
            image_generation: false,
            supports_reasoning: false,
            interactive_exec: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModelModality {
    Text,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelReasoningCapability {
    None,
    Fixed,
    Effort,
    Budget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelIntrinsicCapabilities {
    pub input_modalities: Vec<ModelModality>,
    pub output_modalities: Vec<ModelModality>,
    pub reasoning: ModelReasoningCapability,
    pub parallel_tool_calls: bool,
    pub interactive_exec: bool,
}

impl ModelCapabilityFlags {
    pub fn intrinsic(&self) -> ModelIntrinsicCapabilities {
        let mut input_modalities = vec![ModelModality::Text];
        if self.image_input {
            input_modalities.push(ModelModality::Image);
        }
        let mut output_modalities = vec![ModelModality::Text];
        if self.image_generation {
            output_modalities.push(ModelModality::Image);
        }
        ModelIntrinsicCapabilities {
            input_modalities,
            output_modalities,
            reasoning: if self.supports_reasoning {
                ModelReasoningCapability::Fixed
            } else {
                ModelReasoningCapability::None
            },
            parallel_tool_calls: self.parallel_tool_calls,
            interactive_exec: self.interactive_exec,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelParameterSupport {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EndpointModelPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_parameters: Vec<ModelParameterSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_cache: Option<ResolvedAnthropicCachePolicy>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicCacheControlSource {
    RouteOverride,
    ProviderOverride,
    RouteBuiltin,
    EndpointBuiltin,
    CompatibilityDefault,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedAnthropicCachePolicy {
    pub cache_control: bool,
    pub source: AnthropicCacheControlSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionProtocol {
    OpenAiCompatible,
    OpenAiDecisions,
    CloudflareClef,
    Jev,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelVerbosity {
    Low,
    Medium,
    High,
}

impl ModelVerbosity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedRuntimeModelPolicy {
    pub model_ref: ModelRef,
    pub display_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,
    pub effective_context_window_percent: u8,
    pub prompt_budget_estimated_tokens: usize,
    pub compaction_trigger_estimated_tokens: usize,
    pub compaction_keep_recent_estimated_tokens: usize,
    pub runtime_max_output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<ModelVerbosity>,
    pub tool_output_truncation_estimated_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens_upper_limit: Option<u32>,
    #[serde(default)]
    pub capabilities: ModelCapabilityFlags,
    #[serde(default = "default_agent_turn_capability")]
    pub agent_turn: bool,
    #[serde(default)]
    pub decision_capable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_protocol: Option<DecisionProtocol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_effort_options: Vec<String>,
    pub source: ModelMetadataSource,
    #[serde(default)]
    pub evidence: ModelMetadataEvidence,
}

pub fn default_agent_turn_capability() -> bool {
    true
}

/// User preference; adapters own wire values such as Codex's `priority`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    Default,
    #[serde(alias = "priority")]
    Fast,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportCapabilities {
    pub image_input: bool,
    pub image_output: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub const OPENAI_CODEX: &'static str = "openai-codex";
    pub const OPENAI: &'static str = "openai";
    pub const ANTHROPIC: &'static str = "anthropic";
    pub const GEMINI: &'static str = "gemini";
    pub const XAI: &'static str = "xai";
}

impl ProviderId {
    pub fn parse(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(anyhow!("provider id must not be empty"));
        }
        if !normalized
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(anyhow!(
                "invalid provider id {normalized}; expected lowercase ascii, digits, '-' or '_'"
            ));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn openai_codex() -> Self {
        Self(Self::OPENAI_CODEX.to_string())
    }

    pub fn openai() -> Self {
        Self(Self::OPENAI.to_string())
    }

    pub fn anthropic() -> Self {
        Self(Self::ANTHROPIC.to_string())
    }

    pub fn gemini() -> Self {
        Self(Self::GEMINI.to_string())
    }

    pub fn is_openai_codex(&self) -> bool {
        self.as_str() == Self::OPENAI_CODEX
    }

    pub fn is_openai(&self) -> bool {
        self.as_str() == Self::OPENAI
    }

    pub fn is_anthropic(&self) -> bool {
        self.as_str() == Self::ANTHROPIC
    }

    pub fn is_gemini(&self) -> bool {
        self.as_str() == Self::GEMINI
    }
}

impl Serialize for ProviderId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ProviderId::parse(&raw).map_err(D::Error::custom)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderEndpointId(String);

impl ProviderEndpointId {
    pub const DEFAULT: &'static str = "default";

    pub fn default_endpoint() -> Self {
        Self(Self::DEFAULT.to_string())
    }

    pub fn parse(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(anyhow!("provider endpoint id must not be empty"));
        }
        if !normalized
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(anyhow!(
                "invalid provider endpoint id {normalized}; expected lowercase ascii, digits, '-' or '_'"
            ));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ProviderEndpointId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProviderEndpointId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ProviderEndpointId::parse(&raw).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRouteRef {
    pub provider: ProviderId,
    pub endpoint: ProviderEndpointId,
    pub model: String,
}

impl ModelRouteRef {
    pub fn new(
        provider: ProviderId,
        endpoint: ProviderEndpointId,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            endpoint,
            model: model.into(),
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("model route ref must not be empty"));
        }
        let (route, model) = trimmed.split_once('/').ok_or_else(|| {
            anyhow!("invalid model route ref {trimmed}; expected provider@endpoint/model")
        })?;
        let (provider, endpoint) = route.split_once('@').ok_or_else(|| {
            anyhow!("invalid model route ref {trimmed}; expected provider@endpoint/model")
        })?;
        let provider = ProviderId::parse(provider)?;
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        let model = model.trim();
        if model.is_empty() {
            return Err(anyhow!(
                "invalid model route ref {trimmed}; model part must not be empty"
            ));
        }
        Ok(Self::new(provider, endpoint, model))
    }

    pub fn parse_compatible(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        let accepted_formats = || {
            format!("invalid model reference {trimmed}; expected provider@endpoint/model or legacy provider/model")
        };
        if trimmed
            .split_once('/')
            .is_some_and(|(route, _)| route.contains('@'))
        {
            return Self::parse(trimmed).with_context(accepted_formats);
        }
        let model_ref = ModelRef::parse(trimmed).with_context(accepted_formats)?;
        Ok(Self::from_legacy_model_ref(&model_ref))
    }

    pub fn from_legacy_model_ref(model_ref: &ModelRef) -> Self {
        let (route_provider, route_endpoint) = legacy_provider_endpoint(&model_ref.provider)
            .unwrap_or_else(|_| {
                (
                    model_ref.provider.clone(),
                    ProviderEndpointId::default_endpoint(),
                )
            });
        let catalog = legacy_model_routes();
        let model_ref = catalog
            .aliases
            .get(model_ref)
            .cloned()
            .unwrap_or_else(|| model_ref.clone());
        let endpoint = if route_provider != model_ref.provider
            || route_endpoint != ProviderEndpointId::default_endpoint()
        {
            route_endpoint
        } else {
            if model_ref.provider.as_str() == "opencode-go" {
                Some(ProviderEndpointId::parse("messages").expect("valid Go endpoint"))
            } else {
                catalog
                    .preferred_routes
                    .get(&model_ref)
                    .map(|route| route.endpoint.clone())
            }
            .unwrap_or(route_endpoint)
        };
        Self::new(route_provider, endpoint, model_ref.model)
    }

    pub fn model_ref(&self) -> ModelRef {
        ModelRef::new(self.provider.clone(), self.model.clone())
    }

    pub fn as_string(&self) -> String {
        format!(
            "{}@{}/{}",
            self.provider.as_str(),
            self.endpoint.as_str(),
            self.model
        )
    }

    pub fn as_compact_display(&self) -> String {
        if self.endpoint == ProviderEndpointId::default_endpoint()
            && self.provider.as_str() != "opencode-go"
        {
            return self.model_ref().as_string();
        }
        self.as_string()
    }
}

impl Serialize for ModelRouteRef {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_string())
    }
}

impl<'de> Deserialize<'de> for ModelRouteRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ModelRouteRef::parse_compatible(&raw).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRouteCapability {
    Turn,
    Decision,
    VisionObservation,
    ImageGeneration,
}

impl ModelRouteCapability {
    pub fn model_supports(self, policy: &ResolvedRuntimeModelPolicy) -> bool {
        match self {
            Self::Turn => policy.agent_turn,
            Self::Decision => policy.decision_capable,
            Self::VisionObservation => policy.capabilities.image_input,
            Self::ImageGeneration => policy.capabilities.image_generation,
        }
    }

    pub fn transport_supports(self, transport: ProviderTransportKind) -> bool {
        match self {
            Self::Turn => true,
            Self::Decision => true,
            Self::VisionObservation => transport.supports_view_image_observation_generation(),
            Self::ImageGeneration => transport.supports_image_generation(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedModelCapabilities {
    pub intrinsic: crate::model_values::ModelIntrinsicCapabilities,
    pub endpoint: crate::model_values::EndpointModelPolicy,
    pub transport: TransportCapabilities,
    pub agent_turn: bool,
    pub decision_capable: bool,
    pub image_input: bool,
    pub image_output: bool,
}

impl ResolvedModelCapabilities {
    pub fn supports(&self, capability: ModelRouteCapability) -> bool {
        match capability {
            ModelRouteCapability::Turn => self.agent_turn,
            ModelRouteCapability::Decision => self.decision_capable,
            ModelRouteCapability::VisionObservation => self.image_input,
            ModelRouteCapability::ImageGeneration => self.image_output,
        }
    }
}

impl ModelRef {
    pub fn new(provider: ProviderId, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("model ref must not be empty"));
        }
        let (provider, model) = trimmed
            .split_once('/')
            .ok_or_else(|| anyhow!("invalid model ref {trimmed}; expected provider/model"))?;
        let provider = ProviderId::parse(provider)?;
        let model = model.trim();
        if model.is_empty() {
            return Err(anyhow!(
                "invalid model ref {trimmed}; model part must not be empty"
            ));
        }
        Ok(Self {
            provider,
            model: model.to_string(),
        })
    }

    pub fn as_string(&self) -> String {
        format!("{}/{}", self.provider.as_str(), self.model)
    }
}

impl Serialize for ModelRef {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_string())
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ModelRef::parse(&raw).map_err(D::Error::custom)
    }
}

#[doc(hidden)]
pub const BUILT_IN_SNAPSHOT: &str = include_str!("builtin_snapshot_v1.json");

struct LegacyModelRoutes {
    aliases: HashMap<ModelRef, ModelRef>,
    preferred_routes: HashMap<ModelRef, ModelRouteRef>,
}

fn legacy_model_routes() -> &'static LegacyModelRoutes {
    #[derive(Deserialize)]
    struct Alias {
        alias: ModelRef,
        target: ModelRef,
    }

    #[derive(Deserialize)]
    struct PreferredRoute {
        model_ref: ModelRef,
        route_ref: String,
    }

    #[derive(Deserialize)]
    struct Snapshot {
        aliases: Vec<Alias>,
        preferred_routes_by_model: Vec<PreferredRoute>,
    }

    static ROUTES: OnceLock<LegacyModelRoutes> = OnceLock::new();
    ROUTES.get_or_init(|| {
        let snapshot: Snapshot =
            serde_json::from_str(BUILT_IN_SNAPSHOT).expect("valid built-in model routing snapshot");
        LegacyModelRoutes {
            aliases: snapshot
                .aliases
                .into_iter()
                .map(|alias| (alias.alias, alias.target))
                .collect(),
            preferred_routes: snapshot
                .preferred_routes_by_model
                .into_iter()
                .map(|preferred| {
                    let route = ModelRouteRef::parse(&preferred.route_ref)
                        .expect("valid preferred route in the built-in snapshot");
                    (preferred.model_ref, route)
                })
                .collect(),
        }
    })
}

fn legacy_provider_endpoint(provider: &ProviderId) -> Result<(ProviderId, ProviderEndpointId)> {
    let Some(definition) = provider_definition(provider.as_str()) else {
        return Ok((provider.clone(), ProviderEndpointId::default_endpoint()));
    };
    Ok((
        ProviderId::parse(definition.route_provider)?,
        ProviderEndpointId::parse(definition.route_endpoint)?,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTransportKind {
    #[serde(rename = "openai_codex_responses")]
    OpenAiCodexResponses,
    #[serde(rename = "openai_responses")]
    #[default]
    OpenAiResponses,
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    AnthropicMessages,
    GeminiGenerateContent,
    /// Typed decision/evaluation wire family used by TypeSafe System One and the
    /// Vercel AI Gateway evaluation-model endpoint. It serves Decision requests
    /// only and has no agent-turn wire implementation.
    AiEvaluationModel,
}

impl ProviderTransportKind {
    const ALL: [Self; 6] = [
        Self::OpenAiCodexResponses,
        Self::OpenAiResponses,
        Self::OpenAiChatCompletions,
        Self::AnthropicMessages,
        Self::GeminiGenerateContent,
        Self::AiEvaluationModel,
    ];
    pub fn parse(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == normalized)
            .ok_or_else(|| {
                anyhow!(
                    "invalid provider transport {normalized}; expected {}",
                    Self::ALL
                        .into_iter()
                        .map(Self::as_str)
                        .collect::<Vec<_>>()
                        .join("|")
                )
            })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCodexResponses => "openai_codex_responses",
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::AnthropicMessages => "anthropic_messages",
            Self::GeminiGenerateContent => "gemini_generate_content",
            Self::AiEvaluationModel => "ai_evaluation_model",
        }
    }

    pub fn supports_view_image_observation_generation(self) -> bool {
        self.capabilities().image_input
    }

    pub fn supports_image_generation(self) -> bool {
        self.capabilities().image_output
    }

    pub const fn capabilities(self) -> TransportCapabilities {
        TransportCapabilities {
            image_input: !matches!(self, Self::AiEvaluationModel),
            image_output: matches!(
                self,
                Self::OpenAiCodexResponses | Self::OpenAiResponses | Self::OpenAiChatCompletions
            ),
        }
    }
}

pub const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u8 = 95;
pub const DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS: usize = 2_500;

pub const DEFAULT_PROMPT_BUDGET_ESTIMATED_TOKENS: usize = 128_000;

pub fn default_compaction_trigger_estimated_tokens(prompt_budget: usize) -> usize {
    prompt_budget.saturating_mul(90) / 100
}

pub fn default_compaction_keep_recent_estimated_tokens(trigger_budget: usize) -> usize {
    trigger_budget.saturating_mul(38) / 100
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningEffortValidationError {
    message: String,
}

impl ReasoningEffortValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ReasoningEffortValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReasoningEffortValidationError {}

impl Default for ResolvedRuntimeModelPolicy {
    fn default() -> Self {
        Self {
            model_ref: ModelRef::new(ProviderId::openai(), "unknown"),
            display_name: "Unknown model".into(),
            description: "Legacy model state without resolved runtime policy.".into(),
            context_window_tokens: None,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            prompt_budget_estimated_tokens: DEFAULT_PROMPT_BUDGET_ESTIMATED_TOKENS,
            compaction_trigger_estimated_tokens: default_compaction_trigger_estimated_tokens(
                DEFAULT_PROMPT_BUDGET_ESTIMATED_TOKENS,
            ),
            compaction_keep_recent_estimated_tokens:
                default_compaction_keep_recent_estimated_tokens(
                    default_compaction_trigger_estimated_tokens(
                        DEFAULT_PROMPT_BUDGET_ESTIMATED_TOKENS,
                    ),
                ),
            runtime_max_output_tokens: 8192,
            verbosity: None,
            tool_output_truncation_estimated_tokens:
                DEFAULT_TOOL_OUTPUT_TRUNCATION_ESTIMATED_TOKENS,
            max_output_tokens_upper_limit: None,
            capabilities: ModelCapabilityFlags::default(),
            agent_turn: default_agent_turn_capability(),
            decision_capable: false,
            decision_protocol: None,
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::UnknownFallback,
            evidence: ModelMetadataEvidence::default(),
        }
    }
}

impl ResolvedRuntimeModelPolicy {
    pub fn validate_reasoning_effort(
        &self,
        value: &str,
    ) -> Result<(), ReasoningEffortValidationError> {
        if self
            .reasoning_effort_options
            .iter()
            .any(|option| option == value)
        {
            return Ok(());
        }

        let model = self.model_ref.as_string();
        if value == "ultra" && self.model_ref.provider == ProviderId::openai_codex() {
            return Err(ReasoningEffortValidationError::new(format!(
                "reasoning_effort 'ultra' is unavailable for model {model}; \
                 Holon does not yet implement the required orchestration semantics"
            )));
        }
        if self.reasoning_effort_options.is_empty() {
            return Err(ReasoningEffortValidationError::new(format!(
                "model {model} does not support configurable reasoning_effort"
            )));
        }
        Err(ReasoningEffortValidationError::new(format!(
            "reasoning_effort '{value}' is not supported by model {model}; supported values: {}",
            self.reasoning_effort_options.join(", ")
        )))
    }
}
