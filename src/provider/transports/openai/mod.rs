use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use reqwest::{header::HeaderMap, Client, RequestBuilder, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use crate::{
    auth::{
        load_codex_cli_credential, load_codex_oauth_profile_credential,
        load_oauth_profile_credential, oauth_provider_config, refresh_codex_oauth_profile_material,
        refresh_oauth_profile_material, CodexCliCredential, CodexOAuthRefreshFailure,
        OAuthCredential,
    },
    config::{
        load_credential_store_at, save_credential_store_at, AppConfig, CredentialKind,
        CredentialProfileFile, CredentialSource, ModelRef, ModelRouteRef,
        ProviderBuiltinWebSearchConfig, ProviderId, ProviderRuntimeConfig, RuntimeModelCatalog,
    },
    context::ContextConfig,
    model_catalog::{ModelRuntimeOverride, ModelVerbosity},
    provider::{
        builtin_web_search_probe_turn_request, emitted_tool_json_schema,
        http_trace::{ProviderHttpTrace, ProviderHttpTraceRequest},
        AgentProvider, ContinuationScopeId, ConversationMessage, ModelBlock, ModelToolCallKind,
        ProviderBuiltinWebSearchCapability, ProviderCacheUsage, ProviderGenerateImageRequest,
        ProviderGenerateImageResponse, ProviderGeneratedImage,
        ProviderIncrementalContinuationDiagnostics, ProviderNativeWebSearchDiagnostics,
        ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest,
        ProviderOpenAiRemoteCompactionDiagnostics, ProviderOpenAiRequestControlsDiagnostics,
        ProviderPromptFrame, ProviderRequestDiagnostics, ProviderResponseFormatDiagnostics,
        ProviderResponseFormatRequest, ProviderTransportDiagnostics, ProviderTurnRequest,
        ProviderTurnResponse, ToolSchemaContract,
    },
    token_estimate::estimate_json_tokens,
};

use super::{build_http_client, request_send_timeout, response_body_timeout, stream_idle_timeout};
use crate::provider::retry::{
    classify_reqwest_transport_error_with_trace, classify_status_error_with_trace,
    invalid_response_error, provider_transport_error, timeout_transport_error_with_trace,
    ProviderFailureClassification, ProviderFailureKind, ProviderTransportError, RetryDisposition,
};

mod auth;
mod chat;
mod continuation;
mod http;
mod images;
mod responses;

#[cfg(test)]
use auth::{
    choose_openai_codex_credential, resolve_openai_codex_credential, CredentialStoreRefreshLock,
};
use auth::{
    is_openai_codex_auth_status_error, openai_codex_headers,
    openai_model_policy_for_runtime_config, openai_model_policy_from_config,
};

#[cfg(test)]
use images::parse_openai_codex_image_generation_response_items;
use images::{
    build_openai_codex_image_generation_request, build_openai_images_request,
    openai_images_generations_url, send_openai_codex_image_generation_request,
    send_openai_images_request,
};

#[cfg(test)]
pub(crate) use chat::{
    accumulate_chat_completion_stream_events, build_chat_completion_messages,
    build_chat_completion_request, classify_openai_chat_completion_error,
    parse_chat_completion_response,
};
use chat::{plan_chat_completion_request, send_chat_completion_request};
#[cfg(test)]
use continuation::{incremental_diagnostics, OpenAiProviderWindow};
use continuation::{
    invalidate_openai_continuation, OpenAiContinuationMismatchDiagnostics, OpenAiContinuationState,
    OpenAiRequestPlan, OpenAiRequestShape,
};

pub(crate) use responses::build_openai_responses_request;
#[cfg(test)]
pub(crate) use responses::{build_openai_input, parse_openai_response};
#[cfg(test)]
use responses::{
    consume_openai_sse_event, latest_openai_compaction_index, native_web_search_diagnostics,
    openai_compaction_trigger_for_request_plan, openai_compaction_trigger_for_window,
    openai_provider_window_compaction_candidate,
};
use responses::{
    maybe_compact_openai_provider_window, maybe_compact_openai_request_plan,
    plan_openai_responses_request, retry_openai_responses_with_lossless_replay,
    send_openai_responses_request, send_openai_responses_streaming_request,
    update_openai_continuation,
};

use http::{
    provider_model_ref, provider_request_id_from_headers, send_openai_request, trace_response_body,
    trace_response_headers, trace_stream_chunk, trace_stream_terminal,
};

#[derive(Clone)]
pub struct OpenAiProvider {
    client: Client,
    provider_id: String,
    base_url: String,
    auth: OpenAiBearerAuth,
    model: String,
    max_output_tokens: u32,
    reasoning_effort: Option<String>,
    continuation_contract: OpenAiResponsesContinuationContract,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
    compaction_policy: OpenAiCompactionPolicy,
    trace_home_dir: PathBuf,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone)]
pub(crate) struct OpenAiBearerAuth {
    client: Client,
    provider_id: String,
    api_key: Option<String>,
    credential_profile: Option<String>,
    credential_material: Option<String>,
    credential_store_path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct OpenAiCodexProvider {
    client: Client,
    provider_id: String,
    base_url: String,
    credential_profile: Option<String>,
    credential_material: Option<String>,
    credential_external: Option<String>,
    credential_store_path: Option<PathBuf>,
    codex_home: std::path::PathBuf,
    originator: String,
    model: String,
    max_output_tokens: u32,
    reasoning_effort: Option<String>,
    supports_reasoning: bool,
    verbosity: Option<ModelVerbosity>,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
    compaction_policy: OpenAiCompactionPolicy,
    trace_home_dir: PathBuf,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone)]
pub struct OpenAiChatCompletionsProvider {
    client: Client,
    provider_id: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_output_tokens: u32,
    trace_home_dir: PathBuf,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenAiResponsesTransportContract {
    StandardJson,
    CodexStreaming,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpenAiResponsesContinuationContract {
    Standard,
    StoreResponsesAndOmitInstructionsWithPreviousResponseId,
}

const OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND: &str = "responses_compact";

#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenAiCompactionPolicy {
    pub(crate) trigger_input_tokens: u64,
}

fn request_agent_id(request: &ProviderTurnRequest) -> Option<&str> {
    request
        .prompt_frame
        .cache
        .as_ref()
        .map(|cache| cache.agent_id.as_str())
}

fn openai_responses_url(base_url: &str) -> String {
    format!("{}/responses", base_url.trim_end_matches('/'))
}

fn openai_responses_compact_url(base_url: &str) -> String {
    format!("{}/responses/compact", base_url.trim_end_matches('/'))
}

fn openai_codex_api_base_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/codex") {
        base_url.to_string()
    } else {
        format!("{base_url}/codex")
    }
}

fn openai_codex_responses_url(base_url: &str) -> String {
    format!("{}/responses", openai_codex_api_base_url(base_url))
}

fn openai_codex_responses_compact_url(base_url: &str) -> String {
    format!("{}/responses/compact", openai_codex_api_base_url(base_url))
}

#[derive(Debug)]
pub(crate) struct ParsedOpenAiResponse {
    pub(crate) response: ProviderTurnResponse,
    pub(crate) response_id: Option<String>,
    pub(crate) output_items: Vec<Value>,
}

impl OpenAiProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        let policy = openai_model_policy_from_config(config, ProviderId::openai(), model);
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            &config.home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        let policy =
            openai_model_policy_for_runtime_config(provider_config, model, max_output_tokens);
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            trace_home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
        )
    }

    pub(crate) fn from_runtime_config_with_compaction_policy(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        compaction_policy: OpenAiCompactionPolicy,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let auth = OpenAiBearerAuth::from_runtime_config(provider_config, client.clone())?;
        Ok(Self {
            client,
            provider_id: provider_config.id.as_str().to_string(),
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            auth,
            model: model.to_string(),
            max_output_tokens,
            reasoning_effort: provider_config.reasoning_effort.clone(),
            continuation_contract: if provider_config.id.as_str() == "xai" {
                OpenAiResponsesContinuationContract::StoreResponsesAndOmitInstructionsWithPreviousResponseId
            } else {
                OpenAiResponsesContinuationContract::Standard
            },
            builtin_web_search: provider_config.builtin_web_search.clone(),
            compaction_policy,
            trace_home_dir: trace_home_dir.to_path_buf(),
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }

    async fn resolve_auth_headers(&self) -> Result<Vec<(&'static str, String)>> {
        self.auth.resolve_auth_headers().await
    }

    async fn refresh_auth_headers_after_failure(
        &self,
        error: &anyhow::Error,
    ) -> Result<Option<Vec<(&'static str, String)>>> {
        if !is_openai_codex_auth_status_error(error) {
            return Ok(None);
        }
        self.auth.refresh_auth_headers().await
    }
}

impl OpenAiChatCompletionsProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        let policy = openai_model_policy_from_config(config, ProviderId::openai(), model);
        Self::from_resolved_runtime_config(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            &config.home_dir,
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        let policy =
            openai_model_policy_for_runtime_config(provider_config, model, max_output_tokens);
        Self::from_resolved_runtime_config(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            trace_home_dir,
        )
    }

    pub(crate) fn from_resolved_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        resolved_max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let api_key = match (provider_config.auth.source, provider_config.auth.kind) {
            (CredentialSource::None, CredentialKind::None) => None,
            _ => Some(
                provider_config
                    .credential
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        let credential_name = provider_config
                            .auth
                            .env
                            .as_deref()
                            .or(provider_config.auth.profile.as_deref())
                            .or(provider_config.auth.external.as_deref())
                            .unwrap_or("configured credential");
                        anyhow::anyhow!("missing {credential_name}")
                    })?,
            ),
        };
        Ok(Self {
            client,
            provider_id: provider_config.id.as_str().to_string(),
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            api_key,
            model: model.to_string(),
            max_output_tokens: resolved_max_output_tokens,
            trace_home_dir: trace_home_dir.to_path_buf(),
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }
}

#[async_trait]
impl AgentProvider for OpenAiProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let body = build_openai_responses_request(
            &self.model,
            self.max_output_tokens,
            &request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Relaxed,
            self.reasoning_effort.as_deref(),
            None,
        )?;
        let mut plan = plan_openai_responses_request(
            body,
            &request,
            &self.continuation,
            true,
            self.continuation_contract,
        )?;
        let mut sent_diagnostics = plan.diagnostics.clone();
        let plan_scope = plan.scope.clone();
        let plan_request_shape = plan.request_shape.clone();
        let mut headers = self.resolve_auth_headers().await?;
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        if let Some(remote_compaction) = maybe_compact_openai_request_plan(
            &self.continuation,
            &mut plan,
            self.compaction_policy,
            &self.client,
            openai_responses_compact_url(&self.base_url),
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            sent_diagnostics.openai_remote_compaction = Some(remote_compaction);
            sent_diagnostics.request_lowering_mode = plan.diagnostics.request_lowering_mode.clone();
        }
        let mut final_provider_input = plan.provider_input.clone();
        let mut final_replay_loss_reason = plan.replay_loss_reason.clone();
        let parsed = match send_openai_responses_request(
            &self.client,
            openai_responses_url(&self.base_url),
            plan.body.clone(),
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                let retried = if let Some(refreshed_headers) =
                    self.refresh_auth_headers_after_failure(&error).await?
                {
                    headers = refreshed_headers;
                    send_openai_responses_request(
                        &self.client,
                        openai_responses_url(&self.base_url),
                        plan.body.clone(),
                        headers.clone(),
                        trace.as_ref(),
                        request_agent_id(&request),
                    )
                    .await
                } else {
                    Err(error)
                };
                match retried {
                    Ok(parsed) => parsed,
                    Err(error) => match retry_openai_responses_with_lossless_replay(
                        &self.client,
                        openai_responses_url(&self.base_url),
                        &plan,
                        headers.clone(),
                        trace.as_ref(),
                        request_agent_id(&request),
                        error,
                        &mut sent_diagnostics,
                        &mut final_provider_input,
                        &mut final_replay_loss_reason,
                    )
                    .await
                    {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                            return Err(error);
                        }
                    },
                }
            }
        };
        update_openai_continuation(
            &self.continuation,
            plan_scope.clone(),
            plan_request_shape.clone(),
            plan.append_match_input,
            final_provider_input,
            final_replay_loss_reason,
            &parsed,
        );
        if sent_diagnostics.openai_remote_compaction.is_none() {
            sent_diagnostics.openai_remote_compaction = maybe_compact_openai_provider_window(
                &self.continuation,
                plan_scope.as_ref(),
                &plan_request_shape,
                self.compaction_policy,
                &self.client,
                openai_responses_compact_url(&self.base_url),
                headers,
                trace.as_ref(),
                request_agent_id(&request),
            )
            .await;
        }
        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        let body = build_openai_images_request(&self.model, &request);
        let mut headers = self.resolve_auth_headers().await?;
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        let images = match send_openai_images_request(
            &self.client,
            openai_images_generations_url(&self.base_url),
            body.clone(),
            headers.clone(),
            trace.as_ref(),
            None,
        )
        .await
        {
            Ok(images) => images,
            Err(error) => {
                let Some(refreshed_headers) =
                    self.refresh_auth_headers_after_failure(&error).await?
                else {
                    return Err(error);
                };
                headers = refreshed_headers;
                send_openai_images_request(
                    &self.client,
                    openai_images_generations_url(&self.base_url),
                    body,
                    headers,
                    trace.as_ref(),
                    None,
                )
                .await?
            }
        };
        Ok(ProviderGenerateImageResponse {
            provider: self.provider_id.clone(),
            model: self.model.clone(),
            images,
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
            crate::provider::ProviderPromptCapability::IncrementalResponses,
        ]
    }

    fn supports_freeform_grammar_tools(&self) -> bool {
        true
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        let config = self.builtin_web_search.as_ref()?;
        Some(ProviderBuiltinWebSearchCapability {
            kind: config.kind,
            provider_id: self.provider_id.clone(),
            provider_model_ref: format!("{}/{}", self.provider_id, self.model),
            provider_transport: "openai_responses".into(),
            provider_base_url: self.base_url.clone(),
            advertised_tool_type: config.advertised_tool_type.clone(),
            backend_kind: config.backend_kind.clone(),
        })
    }

    async fn probe_builtin_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        let probe_request = builtin_web_search_probe_turn_request(request);
        let body = build_openai_responses_request(
            &self.model,
            16,
            &probe_request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Relaxed,
            self.reasoning_effort.as_deref(),
            None,
        )?;
        let mut headers = self.resolve_auth_headers().await?;
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        match send_openai_responses_request(
            &self.client,
            openai_responses_url(&self.base_url),
            body.clone(),
            headers.clone(),
            trace.as_ref(),
            None,
        )
        .await
        {
            Ok(_) => {}
            Err(error) => {
                let Some(refreshed_headers) =
                    self.refresh_auth_headers_after_failure(&error).await?
                else {
                    return Err(error);
                };
                headers = refreshed_headers;
                send_openai_responses_request(
                    &self.client,
                    openai_responses_url(&self.base_url),
                    body,
                    headers,
                    trace.as_ref(),
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl AgentProvider for OpenAiCodexProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let model_ref = format!("{}/{}", self.provider_id, self.model);
        let credential = self.resolve_fresh_credential().await.map_err(|error| {
            openai_codex_auth_error(
                "credential_resolution",
                &model_ref,
                vec![
                    error.to_string(),
                    "OpenAI Codex can use a Holon-managed openai-codex credential profile or the external Codex CLI credential store. Holon may refresh its own profile, but only reads Codex CLI credentials and never overwrites them."
                        .into(),
                ],
                "OpenAI Codex authentication failed: no Holon openai-codex credential profile or usable Codex CLI credentials are available.",
            )
        })?;
        if let Some(expires_at) = credential.expires_at {
            if expires_at <= Utc::now() + chrono::Duration::seconds(60) {
                return Err(openai_codex_auth_error(
                    "credential_expired",
                    &model_ref,
                    vec![
                        format!(
                            "OpenAI Codex OAuth access token is expired: credential source {} expired at {}",
                            credential.source,
                            expires_at.to_rfc3339()
                        ),
                        "Run Holon onboarding login for openai-codex to configure or refresh Holon-managed OAuth, or run `codex login` if intentionally using external CLI credentials.".into(),
                    ],
                    "OpenAI Codex authentication failed: OAuth access token is expired. Run Holon onboarding login for openai-codex again, or run `codex login` if intentionally using external CLI credentials.",
                ));
            }
        }
        let body = build_openai_responses_request(
            &self.model,
            self.max_output_tokens,
            &request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            if self.supports_reasoning {
                self.reasoning_effort.as_deref()
            } else {
                None
            },
            self.verbosity,
        )?;
        let mut plan = plan_openai_responses_request(
            body,
            &request,
            &self.continuation,
            false,
            OpenAiResponsesContinuationContract::Standard,
        )?;
        let mut sent_diagnostics = plan.diagnostics.clone();
        let plan_scope = plan.scope.clone();
        let plan_request_shape = plan.request_shape.clone();
        let mut credential = credential;
        let mut headers = openai_codex_headers(&credential, &self.originator);
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        if let Some(remote_compaction) = maybe_compact_openai_request_plan(
            &self.continuation,
            &mut plan,
            self.compaction_policy,
            &self.client,
            openai_codex_responses_compact_url(&self.base_url),
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            sent_diagnostics.openai_remote_compaction = Some(remote_compaction);
            sent_diagnostics.request_lowering_mode = plan.diagnostics.request_lowering_mode.clone();
        }
        let parsed = match send_openai_responses_streaming_request(
            &self.client,
            openai_codex_responses_url(&self.base_url),
            plan.body.clone(),
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                if is_openai_codex_auth_status_error(&error) {
                    if let Some(refreshed) =
                        self.refresh_after_auth_failure(&credential).await.map_err(|refresh_error| {
                            invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                            openai_codex_auth_error(
                                "credential_refresh_after_auth_failure",
                                &model_ref,
                                vec![
                                    error.to_string(),
                                    refresh_error.to_string(),
                                    "OpenAI Codex returned an auth error; Holon attempted to refresh the Holon-managed profile before retrying.".into(),
                                ],
                                "OpenAI Codex authentication failed and Holon-managed OAuth refresh did not recover it.",
                            )
                        })?
                    {
                        credential = refreshed;
                        headers = openai_codex_headers(&credential, &self.originator);
                        match send_openai_responses_streaming_request(
                            &self.client,
                            openai_codex_responses_url(&self.base_url),
                            plan.body.clone(),
                            headers.clone(),
                            trace.as_ref(),
                            request_agent_id(&request),
                        )
                        .await
                        {
                            Ok(parsed) => parsed,
                            Err(retry_error) => {
                                invalidate_openai_continuation(
                                    &self.continuation,
                                    plan.scope.as_ref(),
                                );
                                return Err(retry_error);
                            }
                        }
                    } else {
                        invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                        return Err(error);
                    }
                } else {
                    invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                    return Err(error);
                }
            }
        };
        update_openai_continuation(
            &self.continuation,
            plan_scope.clone(),
            plan_request_shape.clone(),
            plan.append_match_input,
            plan.provider_input,
            plan.replay_loss_reason,
            &parsed,
        );
        if sent_diagnostics.openai_remote_compaction.is_none() {
            sent_diagnostics.openai_remote_compaction = maybe_compact_openai_provider_window(
                &self.continuation,
                plan_scope.as_ref(),
                &plan_request_shape,
                self.compaction_policy,
                &self.client,
                openai_codex_responses_compact_url(&self.base_url),
                headers,
                trace.as_ref(),
                request_agent_id(&request),
            )
            .await;
        }
        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        let model_ref = format!("{}/{}", self.provider_id, self.model);
        let credential = self.resolve_fresh_credential().await.map_err(|error| {
            openai_codex_auth_error(
                "credential_resolution",
                &model_ref,
                vec![
                    error.to_string(),
                    "OpenAI Codex image generation uses Codex OAuth credentials through the Responses image_generation tool.".into(),
                ],
                "OpenAI Codex image generation authentication failed: no usable OAuth credentials are available.",
            )
        })?;
        if let Some(expires_at) = credential.expires_at {
            if expires_at <= Utc::now() + chrono::Duration::seconds(60) {
                return Err(openai_codex_auth_error(
                    "credential_expired",
                    &model_ref,
                    vec![
                        format!(
                            "OpenAI Codex OAuth access token is expired: credential source {} expired at {}",
                            credential.source,
                            expires_at.to_rfc3339()
                        ),
                        "Run Holon onboarding login for openai-codex to configure or refresh Holon-managed OAuth, or run `codex login` if intentionally using external CLI credentials.".into(),
                    ],
                    "OpenAI Codex image generation authentication failed: OAuth access token is expired.",
                ));
            }
        }

        let body = build_openai_codex_image_generation_request(&self.model, &request);
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        let mut credential = credential;
        let mut headers = openai_codex_headers(&credential, &self.originator);
        let images = match send_openai_codex_image_generation_request(
            &self.client,
            openai_codex_responses_url(&self.base_url),
            body.clone(),
            headers.clone(),
            trace.as_ref(),
            None,
        )
        .await
        {
            Ok(images) => images,
            Err(error) => {
                if is_openai_codex_auth_status_error(&error) {
                    if let Some(refreshed) = self.refresh_after_auth_failure(&credential).await.map_err(|refresh_error| {
                        openai_codex_auth_error(
                            "credential_refresh_after_auth_failure",
                            &model_ref,
                            vec![
                                error.to_string(),
                                refresh_error.to_string(),
                                "OpenAI Codex returned an auth error for image generation; Holon attempted to refresh the Holon-managed profile before retrying.".into(),
                            ],
                            "OpenAI Codex image generation authentication failed and Holon-managed OAuth refresh did not recover it.",
                        )
                    })? {
                        credential = refreshed;
                        headers = openai_codex_headers(&credential, &self.originator);
                        send_openai_codex_image_generation_request(
                            &self.client,
                            openai_codex_responses_url(&self.base_url),
                            body,
                            headers,
                            trace.as_ref(),
                            None,
                        )
                        .await?
                    } else {
                        return Err(error);
                    }
                } else {
                    return Err(error);
                }
            }
        };

        Ok(ProviderGenerateImageResponse {
            provider: self.provider_id.clone(),
            model: self.model.clone(),
            images,
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai-codex/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
            crate::provider::ProviderPromptCapability::IncrementalResponses,
        ]
    }

    fn supports_freeform_grammar_tools(&self) -> bool {
        true
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        let config = self.builtin_web_search.as_ref()?;
        Some(ProviderBuiltinWebSearchCapability {
            kind: config.kind,
            provider_id: self.provider_id.clone(),
            provider_model_ref: format!("{}/{}", self.provider_id, self.model),
            provider_transport: "openai_codex_responses".into(),
            provider_base_url: self.base_url.clone(),
            advertised_tool_type: config.advertised_tool_type.clone(),
            backend_kind: config.backend_kind.clone(),
        })
    }

    async fn probe_builtin_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        let credential = self.resolve_fresh_credential().await?;
        let probe_request = builtin_web_search_probe_turn_request(request);
        let body = build_openai_responses_request(
            &self.model,
            16,
            &probe_request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            if self.supports_reasoning {
                self.reasoning_effort.as_deref()
            } else {
                None
            },
            self.verbosity,
        )?;
        let headers = openai_codex_headers(&credential, &self.originator);
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        send_openai_responses_streaming_request(
            &self.client,
            openai_codex_responses_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            None,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl AgentProvider for OpenAiChatCompletionsProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        // Build Chat Completions request
        let (body, plan) = plan_chat_completion_request(
            &self.model,
            self.max_output_tokens,
            &request,
            ToolSchemaContract::Relaxed,
            false, // Streaming infrastructure exists but is not currently enabled; requests are non-streaming
            &self.continuation,
        )?;
        let sent_diagnostics = plan.diagnostics.clone();
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());

        // Send to /v1/chat/completions endpoint
        let parsed = match send_chat_completion_request(
            &self.client,
            chat_completions_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                return Err(error);
            }
        };

        update_openai_continuation(
            &self.continuation,
            plan.scope,
            plan.request_shape,
            plan.append_match_input,
            plan.provider_input,
            plan.replay_loss_reason,
            &parsed,
        );

        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        let body = build_openai_images_request(&self.model, &request);
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        let images = send_openai_images_request(
            &self.client,
            openai_images_generations_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            None,
        )
        .await?;
        Ok(ProviderGenerateImageResponse {
            provider: self.provider_id.clone(),
            model: self.model.clone(),
            images,
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
        ]
    }
}

fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if has_trailing_version_segment(trimmed) {
        format!("{trimmed}/chat/completions")
    } else {
        format!("{trimmed}/v1/chat/completions")
    }
}

fn has_trailing_version_segment(url: &str) -> bool {
    let Some(last_segment) = url.rsplit('/').next() else {
        return false;
    };
    let Some(version_digits) = last_segment.strip_prefix('v') else {
        return false;
    };
    !version_digits.is_empty() && version_digits.chars().all(|ch| ch.is_ascii_digit())
}

impl ProviderTurnResponse {
    fn with_request_diagnostics(mut self, diagnostics: ProviderRequestDiagnostics) -> Self {
        self.request_diagnostics = Some(diagnostics);
        self
    }
}

impl ParsedOpenAiResponse {
    fn with_provider_request_id(mut self, provider_request_id: Option<String>) -> Self {
        self.response.provider_request_id = provider_request_id;
        self
    }
}

fn openai_codex_auth_error(
    stage: &str,
    model_ref: &str,
    source_chain: Vec<String>,
    message: &str,
) -> anyhow::Error {
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        None,
        Some(ProviderTransportDiagnostics {
            stage: stage.into(),
            provider: Some("openai-codex".into()),
            model_ref: Some(model_ref.into()),
            url: None,
            status: None,
            reqwest: None,
            http_trace: None,
            source_chain,
        }),
        message,
    )
}

fn openai_codex_status_error_context(status: reqwest::StatusCode) -> &'static str {
    if matches!(
        status,
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        "OpenAI Codex authentication failed; run Holon onboarding login for openai-codex again, or run `codex login` if intentionally using external CLI credentials."
    } else {
        "OpenAI-style streaming request failed"
    }
}

#[cfg(test)]
mod tests;
