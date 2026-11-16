mod catalog;
#[path = "material.rs"]
mod runtime_material;
pub(crate) use runtime_material::{encode_image, material_matches_runtime};
mod diagnostics;
pub mod image_generation;
pub mod media;
mod registry;
mod transports;

pub(crate) use catalog::{
    build_candidate_from_model_route, build_candidate_with_catalog,
    build_provider_from_model_chain_with_override, ModelRouteParameterOverride,
};
pub use catalog::{build_provider_from_config, build_provider_from_model_chain};
pub use diagnostics::{
    provider_doctor, resolved_model_availability, resolved_model_providers,
    resolved_provider_models, resolved_route_availability,
};
pub(crate) use diagnostics::{
    provider_models_from_availability_for_runtime,
    resolved_model_providers_from_availability_for_runtime,
};
pub(crate) use registry::{
    build_provider_for_route, provider_definition, provider_definitions, ProviderCatalogPolicy,
    ProviderContextManagement, ProviderDefinition, ProviderMaterializer, ProviderWebSearch,
};
pub use transports::{
    AnthropicProvider, GeminiProvider, OpenAiChatCompletionsProvider, OpenAiCodexProvider,
    OpenAiProvider,
};

pub(crate) use holon_provider::provider::{
    aggregate_attempt_token_usage, classify_provider_error, fallback, is_dispatch_guard_rejected,
    provider_transport_error_with_code, provider_turn_error, retry, sanitize_transport_url,
    sanitized_provider_error, BootstrapProvider, OpenAiBearerAuth, ProviderFailureClassification,
    ProviderFailureKind, ProviderTransportError, RetryDisposition,
    PROVIDER_RECOVERY_BASE_BACKOFF_MS, PROVIDER_RECOVERY_MAX_BACKOFF_MS,
    PROVIDER_RECOVERY_MAX_FALLBACKS,
};
#[cfg(test)]
pub(crate) use holon_provider::provider::{
    budget, build_openai_input, build_openai_responses_request, emitted_tool_json_schema,
    http_trace, parse_openai_response, provider_error_token_usage, provider_max_attempts,
    validate_emitted_tool_schema, ToolSchemaContract,
};
pub use holon_provider::provider::{
    image_delivery, provider_attempt_timeline, provider_error_code,
    provider_error_is_context_length_exceeded, provider_transport_diagnostics, test_support,
    AgentProvider, AnthropicPromptCacheDiagnostics, CacheBreakpointInfo, ContinuationScopeId,
    ConversationMessage, ModelBlock, ModelToolCallKind, PromptContentBlock, ProviderAttemptOutcome,
    ProviderAttemptRecord, ProviderAttemptTimeline, ProviderBlockData,
    ProviderBuiltinWebSearchCapability, ProviderCacheUsage, ProviderContextBudgetDiagnostics,
    ProviderContextManagementPolicy, ProviderDispatchGuard, ProviderFallbackDisposition,
    ProviderGenerateImageRequest, ProviderGenerateImageResponse, ProviderGeneratedImage,
    ProviderHttpTraceDiagnostics, ProviderImageData, ProviderIncrementalContinuationDiagnostics,
    ProviderJsonSchemaResponseFormat, ProviderNativeWebSearchDiagnostics,
    ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest,
    ProviderOpenAiRemoteCompactionDiagnostics, ProviderOpenAiRequestControlsDiagnostics,
    ProviderPromptCache, ProviderPromptCapability, ProviderPromptFrame, ProviderQuotaIdentity,
    ProviderQuotaIdentityConfidence, ProviderRequestDiagnostics, ProviderRequestMaterial,
    ProviderResponseFormatDiagnostics, ProviderResponseFormatRequest, ProviderSessionId,
    ProviderStablePrefixComponentDiagnostics, ProviderStablePrefixDiagnostics,
    ProviderThrottleHintSource, ProviderThrottleKind, ProviderThrottleMetadata,
    ProviderThrottleReason, ProviderTransportDiagnostics, ProviderTransportTimeline,
    ProviderTurnRequest, ProviderTurnResponse, ReqwestTransportDiagnostics, StubProvider,
    ToolResultBlock, INCOMPLETE_TOOL_RESULT_SENTINEL,
};

pub(crate) use catalog::build_candidate;

#[cfg(test)]
mod tests;

#[cfg(all(test, holon_lib_test_shard = "c"))]
#[path = "transports/anthropic_material_tests.rs"]
mod anthropic_material_tests;
#[cfg(all(test, holon_lib_test_shard = "c"))]
#[path = "transports/openai/responses/compaction_material_tests.rs"]
mod compaction_material_tests;
#[cfg(all(test, holon_lib_test_shard = "c"))]
mod http_trace_material_tests;
#[cfg(all(test, holon_lib_test_shard = "c"))]
mod image_data_tests;
#[cfg(all(test, holon_lib_test_shard = "c"))]
#[path = "transports/openai/image_turn_material_tests.rs"]
mod image_turn_material_tests;
#[cfg(all(test, holon_lib_test_shard = "c"))]
#[path = "transports/openai/images_material_tests.rs"]
mod images_material_tests;
