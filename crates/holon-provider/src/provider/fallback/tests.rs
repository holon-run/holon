use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::provider::transports::{
    AnthropicProvider, GeminiProvider, OpenAiChatCompletionsProvider, OpenAiProvider,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use axum::{http::StatusCode, routing::post, Json, Router};
use tokio::sync::Mutex;

use super::*;
use crate::config::{
    ProviderAuthConfig, ProviderEndpointId, ProviderId, ProviderRuntimeConfig,
    ProviderTransportKind,
};
use crate::provider::{
    provider_attempt_timeline, ModelBlock, ProviderCacheUsage, ProviderDispatchGuard,
    ProviderNativeWebSearchKind, ProviderPromptFrame, ProviderQuotaIdentity,
};

#[derive(Debug, thiserror::Error)]
#[error("local dispatch denied")]
struct GuardDenied;

#[derive(Debug)]
struct RejectAfterDispatches {
    allowed: usize,
    calls: AtomicUsize,
}

impl ProviderDispatchGuard for RejectAfterDispatches {
    fn before_dispatch(&self) -> Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= self.allowed {
            return Err(GuardDenied.into());
        }
        Ok(())
    }
}

struct GuardedProvider {
    provider: Arc<dyn AgentProvider>,
    guard: Arc<dyn ProviderDispatchGuard>,
}

#[async_trait]
impl AgentProvider for GuardedProvider {
    async fn complete_turn(
        &self,
        mut request: ProviderTurnRequest,
    ) -> Result<ProviderTurnResponse> {
        request.dispatch_guard = Some(self.guard.clone());
        self.provider.complete_turn(request).await
    }
}

fn http_candidate(
    base_url: &str,
    home_dir: &std::path::Path,
    transport: ProviderTransportKind,
) -> ProviderCandidate {
    let id = match transport {
        ProviderTransportKind::AnthropicMessages => ProviderId::anthropic(),
        ProviderTransportKind::GeminiGenerateContent => ProviderId::gemini(),
        _ => ProviderId::openai(),
    };
    let config = ProviderRuntimeConfig {
        headers: Default::default(),
        id: id.clone(),
        route_provider: id.clone(),
        route_endpoint: ProviderEndpointId::default_endpoint(),
        transport,
        base_url: base_url.into(),
        auth: ProviderAuthConfig::default(),
        credential: Some("test-key".into()),
        credential_store_path: None,
        codex_home: None,
        originator: None,
        reasoning_effort: None,
        context_management: Default::default(),
        builtin_web_search: None,
    };
    let provider: Arc<dyn AgentProvider> = match transport {
        ProviderTransportKind::OpenAiResponses => Arc::new(
            OpenAiProvider::from_runtime_config(
                &config,
                "test",
                128,
                home_dir,
                env!("CARGO_PKG_VERSION"),
            )
            .unwrap(),
        ),
        ProviderTransportKind::OpenAiChatCompletions => Arc::new(
            OpenAiChatCompletionsProvider::from_runtime_config(
                &config,
                "test",
                128,
                home_dir,
                env!("CARGO_PKG_VERSION"),
            )
            .unwrap(),
        ),
        ProviderTransportKind::AnthropicMessages => Arc::new(
            AnthropicProvider::from_runtime_config(
                &config,
                "test",
                128,
                home_dir,
                false,
                env!("CARGO_PKG_VERSION"),
            )
            .unwrap(),
        ),
        ProviderTransportKind::GeminiGenerateContent => Arc::new(
            GeminiProvider::from_runtime_config(
                &config,
                "test",
                128,
                home_dir,
                env!("CARGO_PKG_VERSION"),
            )
            .unwrap(),
        ),
        _ => panic!("unsupported test transport"),
    };
    ProviderCandidate {
        model_ref: format!("{}/test", id.as_str()),
        provider_name: id.as_str().into(),
        transport_id: None,
        resolved_image_input: false,
        provider,
    }
}

async fn http_failure_server(
    status: StatusCode,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let app = Router::new().route(
        "/responses",
        post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async move {
                (
                    status,
                    Json(serde_json::json!({"error": {"message": "remote failure"}})),
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base_url, requests, server)
}

#[tokio::test]
async fn dispatch_guard_rejection_bypasses_provider_failure_and_fallback() {
    let home = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    for transport in [
        ProviderTransportKind::OpenAiResponses,
        ProviderTransportKind::OpenAiChatCompletions,
        ProviderTransportKind::AnthropicMessages,
        ProviderTransportKind::GeminiGenerateContent,
    ] {
        let guard = Arc::new(RejectAfterDispatches {
            allowed: 0,
            calls: AtomicUsize::new(0),
        });
        let provider = FallbackProvider {
            record_retry: |_| {},
            candidates: vec![
                http_candidate(&base_url, home.path(), transport),
                scripted_candidate("openai/fallback", vec![ScriptedFailure::Succeed]),
            ],
        };
        let mut request = plain_turn_request();
        request.dispatch_guard = Some(guard.clone());
        let error = provider
            .complete_turn_with_diagnostics(request)
            .await
            .unwrap_err();
        assert!(
            is_dispatch_guard_rejected(&error),
            "{transport:?}: {error:#}"
        );
        assert!(error.is::<GuardDenied>());
        assert!(provider_attempt_timeline(&error).is_none());
        assert!(provider_transport_diagnostics(&error).is_none());
        assert_eq!(guard.calls.load(Ordering::SeqCst), 1);
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "local denial must not send HTTP"
    );
}

#[tokio::test]
async fn dispatch_guard_rejection_preserves_prior_http_retry_diagnostics() {
    let home = tempfile::tempdir().unwrap();
    let (base_url, requests, server) = http_failure_server(StatusCode::SERVICE_UNAVAILABLE).await;
    let guard = Arc::new(RejectAfterDispatches {
        allowed: 1,
        calls: AtomicUsize::new(0),
    });
    let mut candidate = http_candidate(
        &base_url,
        home.path(),
        ProviderTransportKind::OpenAiResponses,
    );
    // Inject the transport guard separately so the outer fallback exercises a same-route retry.
    candidate.provider = Arc::new(GuardedProvider {
        provider: candidate.provider.clone(),
        guard: guard.clone(),
    });
    let inner = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            candidate,
            scripted_candidate("openai/fallback", vec![ScriptedFailure::Succeed]),
        ],
    };
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![ProviderCandidate {
            model_ref: "openai/test".into(),
            provider_name: "openai".into(),
            transport_id: None,
            resolved_image_input: false,
            provider: Arc::new(inner),
        }],
    };
    let error = provider
        .complete_turn_with_diagnostics(plain_turn_request())
        .await
        .unwrap_err()
        .context("outer request");
    server.abort();
    assert!(is_dispatch_guard_rejected(&error));
    assert!(error.chain().any(|source| source.is::<GuardDenied>()));
    let timeline = provider_attempt_timeline(&error).unwrap();
    assert_eq!(timeline.attempts.len(), 1);
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::Retrying
    );
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("server_error")
    );
    assert!(!timeline.attempts[0].advanced_to_fallback);
    assert!(timeline.pending_fallback_model_ref.is_none());
    assert!(timeline.pending_fallback_disposition.is_none());
    assert_eq!(
        provider_transport_diagnostics(&error).unwrap().status,
        Some(503)
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(guard.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn genuine_http_failures_still_defer_to_fallback() {
    for (status, kind) in [
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        (StatusCode::SERVICE_UNAVAILABLE, "server_error"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (base_url, requests, server) = http_failure_server(status).await;
        let guard = Arc::new(RejectAfterDispatches {
            allowed: usize::MAX,
            calls: AtomicUsize::new(0),
        });
        let provider = FallbackProvider {
            record_retry: |_| {},
            candidates: vec![
                http_candidate(
                    &base_url,
                    home.path(),
                    ProviderTransportKind::OpenAiResponses,
                ),
                scripted_candidate("openai/fallback", vec![ScriptedFailure::Succeed]),
            ],
        };
        let mut request = plain_turn_request();
        request.dispatch_guard = Some(guard.clone());
        let error = provider
            .complete_turn_with_diagnostics(request)
            .await
            .unwrap_err();
        server.abort();
        assert!(!is_dispatch_guard_rejected(&error));
        let timeline = provider_attempt_timeline(&error).unwrap();
        assert_eq!(timeline.attempts.len(), 1);
        assert_eq!(timeline.attempts[0].failure_kind.as_deref(), Some(kind));
        assert!(timeline.attempts[0].advanced_to_fallback);
        assert_eq!(
            timeline.pending_fallback_model_ref.as_deref(),
            Some("openai/fallback")
        );
        assert!(timeline.pending_fallback_disposition.is_some());
        assert_eq!(
            provider_transport_diagnostics(&error).unwrap().status,
            Some(status.as_u16())
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(guard.calls.load(Ordering::SeqCst), 1);
    }
}

#[derive(Clone)]
struct PolicyProvider {
    policy: Option<ProviderContextManagementPolicy>,
}

#[derive(Clone)]
struct RecordingProvider {
    name: &'static str,
    fail: bool,
    prompts: Arc<Mutex<Vec<String>>>,
    system_blocks: Arc<Mutex<Vec<Vec<PromptContentBlock>>>>,
}

#[derive(Clone)]
struct SearchProvider {
    capability: Option<ProviderBuiltinWebSearchCapability>,
}

#[derive(Clone)]
struct ImageProvider;

struct ConversationRecorder {
    conversations: Arc<Mutex<Vec<Vec<ConversationMessage>>>>,
}

#[async_trait]
impl AgentProvider for ConversationRecorder {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.conversations
            .lock()
            .await
            .push(request.conversation.clone());
        Ok(ProviderTurnResponse {
            normalized_token_usage: None,
            material: Default::default(),
            blocks: vec![ModelBlock::Text { text: "ok".into() }],
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for PolicyProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            normalized_token_usage: None,
            material: Default::default(),
            blocks: vec![ModelBlock::Text { text: "ok".into() }],
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: Some(ProviderCacheUsage {
                read_input_tokens: 0,
                creation_input_tokens: 0,
            }),
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
        self.policy.clone()
    }
}

#[async_trait]
impl AgentProvider for RecordingProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.prompts.lock().await.push(format!(
            "{}:{}",
            self.name, request.prompt_frame.system_prompt
        ));
        self.system_blocks
            .lock()
            .await
            .push(request.prompt_frame.system_blocks.clone());
        if self.fail {
            return Err(anyhow!("forced provider failure"));
        }
        Ok(ProviderTurnResponse {
            normalized_token_usage: None,
            material: Default::default(),
            blocks: vec![ModelBlock::Text { text: "ok".into() }],
            stop_reason: None,
            input_tokens: 1,
            output_tokens: 1,
            cache_usage: Some(ProviderCacheUsage {
                read_input_tokens: 0,
                creation_input_tokens: 0,
            }),
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for SearchProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            normalized_token_usage: None,
            material: Default::default(),
            blocks: vec![ModelBlock::Text { text: "ok".into() }],
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: Some(ProviderCacheUsage {
                read_input_tokens: 0,
                creation_input_tokens: 0,
            }),
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        self.capability.clone()
    }
}

#[async_trait]
impl AgentProvider for ImageProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            normalized_token_usage: None,
            material: Default::default(),
            blocks: vec![ModelBlock::Text { text: "ok".into() }],
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: Some(ProviderCacheUsage {
                read_input_tokens: 0,
                creation_input_tokens: 0,
            }),
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        Ok(ProviderGenerateImageResponse {
            provider: "test".into(),
            model: request.prompt,
            images: Vec::new(),
        })
    }
}

fn policy(trigger_input_tokens: u32) -> ProviderContextManagementPolicy {
    ProviderContextManagementPolicy {
        provider: "anthropic".into(),
        strategy: "clear_tool_uses_20250919".into(),
        keep_recent_tool_uses: 3,
        trigger_input_tokens,
        clear_at_least_input_tokens: None,
    }
}

#[derive(Clone, Copy)]
enum ScriptedFailure {
    Fail {
        retry_after: Option<std::time::Duration>,
    },
    ServerError {
        retry_after: Option<std::time::Duration>,
    },
    HardQuota,
    DeterministicError,
    Succeed,
}

#[derive(Clone)]
struct ScriptedProvider {
    script: Arc<Mutex<Vec<ScriptedFailure>>>,
    quota_identity: Option<ProviderQuotaIdentity>,
}

#[async_trait]
impl AgentProvider for ScriptedProvider {
    fn quota_identity(&self) -> Option<ProviderQuotaIdentity> {
        self.quota_identity.clone()
    }

    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let next = self.script.lock().await.remove(0);
        match next {
            ScriptedFailure::Fail { retry_after } => Err(
                crate::provider::retry::provider_transport_error_with_code_and_retry_after(
                    crate::provider::retry::ProviderFailureClassification {
                        kind: crate::provider::ProviderFailureKind::RateLimited,
                        disposition: RetryDisposition::Retryable,
                    },
                    Some("rate_limit_exceeded"),
                    Some(429),
                    None,
                    retry_after,
                    "scripted rate limit",
                ),
            ),
            ScriptedFailure::ServerError { retry_after } => Err(
                crate::provider::retry::provider_transport_error_with_code_and_retry_after(
                    crate::provider::retry::ProviderFailureClassification {
                        kind: crate::provider::ProviderFailureKind::ServerError,
                        disposition: RetryDisposition::Retryable,
                    },
                    Some("server_error"),
                    Some(503),
                    None,
                    retry_after,
                    "scripted server error",
                ),
            ),
            ScriptedFailure::HardQuota => {
                Err(crate::provider::retry::classify_status_error_with_trace(
                    "BigModel request failed",
                    "response_status",
                    Some("bigmodel"),
                    Some("bigmodel@default/glm-5.2"),
                    Some("https://open.bigmodel.cn/api/anthropic/v1/messages"),
                    reqwest::StatusCode::TOO_MANY_REQUESTS,
                    // Constructed from the official api-code documentation, not a capture.
                    r#"{"error":{"code":"1310","message":"weekly/monthly usage limit"}}"#.into(),
                    None,
                    None,
                ))
            }
            ScriptedFailure::DeterministicError => Err(
                crate::provider::retry::provider_transport_error_with_code_and_retry_after(
                    crate::provider::retry::ProviderFailureClassification {
                        kind: crate::provider::ProviderFailureKind::ContractError,
                        disposition: RetryDisposition::FailFast,
                    },
                    Some("context_length_exceeded"),
                    Some(500),
                    None,
                    None,
                    "context length exceeded",
                ),
            ),
            ScriptedFailure::Succeed => Ok(ProviderTurnResponse {
                normalized_token_usage: None,
                material: Default::default(),
                blocks: vec![ModelBlock::Text { text: "ok".into() }],
                stop_reason: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: Some(ProviderCacheUsage {
                    read_input_tokens: 0,
                    creation_input_tokens: 0,
                }),
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
        }
    }
}

fn scripted_candidate(model_ref: &str, script: Vec<ScriptedFailure>) -> ProviderCandidate {
    ProviderCandidate {
        model_ref: model_ref.into(),
        provider_name: "openai".into(),
        transport_id: None,
        resolved_image_input: false,
        provider: Arc::new(ScriptedProvider {
            script: Arc::new(Mutex::new(script)),
            quota_identity: None,
        }),
    }
}

fn scripted_candidate_with_identity(
    model_ref: &str,
    script: Vec<ScriptedFailure>,
    quota_identity: ProviderQuotaIdentity,
) -> ProviderCandidate {
    ProviderCandidate {
        model_ref: model_ref.into(),
        provider_name: "openai".into(),
        transport_id: None,
        resolved_image_input: false,
        provider: Arc::new(ScriptedProvider {
            script: Arc::new(Mutex::new(script)),
            quota_identity: Some(quota_identity),
        }),
    }
}

fn plain_turn_request() -> ProviderTurnRequest {
    ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame::plain("system"),
        conversation: Vec::new(),
        tools: Vec::new(),
        native_web_search: None,
        response_format: None,
    }
}

#[tokio::test]
async fn retry_records_server_retry_after_backoff_source() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![scripted_candidate(
            "openai/gpt-5.4",
            vec![
                ScriptedFailure::Fail {
                    retry_after: Some(std::time::Duration::from_millis(1)),
                },
                ScriptedFailure::Succeed,
            ],
        )],
    };

    let (_, diagnostics) = provider
        .complete_turn_with_diagnostics(plain_turn_request())
        .await
        .expect("second attempt should succeed");
    let timeline = diagnostics.expect("timeline");
    assert_eq!(timeline.attempts.len(), 2);
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::Retrying
    );
    assert_eq!(timeline.attempts[0].backoff_ms, Some(200));
    assert_eq!(
        timeline.attempts[0].backoff_source.as_deref(),
        Some("server_retry_after")
    );
    assert!(timeline.attempts[0].started_at.is_some());
    assert!(timeline.attempts[0].completed_at.is_some());
    assert!(timeline.attempts[0].completed_at.unwrap() >= timeline.attempts[0].started_at.unwrap());
    assert!(timeline.attempts[0].duration_ms.is_some());
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Succeeded
    );
}

#[tokio::test(start_paused = true)]
async fn oversized_rate_limit_hint_skips_remaining_same_route_retries() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![scripted_candidate(
            "openai/gpt-5.4",
            vec![
                ScriptedFailure::Fail {
                    retry_after: Some(std::time::Duration::from_secs(45)),
                },
                ScriptedFailure::Succeed,
            ],
        )],
    };

    let error = provider
        .complete_turn_with_diagnostics(plain_turn_request())
        .await
        .expect_err("long rate-limit hint must stop the current request");
    let timeline = crate::provider::provider_attempt_timeline(&error).expect("timeline");
    assert_eq!(timeline.attempts.len(), 1);
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("rate_limited")
    );
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::RetriesExhausted
    );
    assert_eq!(timeline.attempts[0].backoff_ms, None);
    assert_eq!(timeline.attempts[0].backoff_source, None);
}

#[tokio::test(start_paused = true)]
async fn persistent_rate_limit_retries_are_bounded_before_fallback() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            scripted_candidate(
                "openai/gpt-5.4",
                vec![
                    ScriptedFailure::Fail {
                        retry_after: Some(std::time::Duration::from_secs(5)),
                    },
                    ScriptedFailure::Fail {
                        retry_after: Some(std::time::Duration::from_secs(5)),
                    },
                    ScriptedFailure::Fail {
                        retry_after: Some(std::time::Duration::from_secs(5)),
                    },
                    ScriptedFailure::Fail {
                        retry_after: Some(std::time::Duration::from_secs(5)),
                    },
                ],
            ),
            scripted_candidate("openai/gpt-5.4-mini", vec![ScriptedFailure::Succeed]),
        ],
    };

    let task = tokio::spawn(async move {
        provider
            .complete_turn_with_diagnostics(plain_turn_request())
            .await
            .expect_err("persistent rate limits must terminate the current turn")
    });
    tokio::task::yield_now().await;
    for _ in 0..PROVIDER_RATE_LIMIT_MAX_RETRIES {
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
    }
    let error = task.await.expect("provider task should complete");
    let timeline = crate::provider::provider_attempt_timeline(&error).expect("timeline");
    assert_eq!(timeline.attempts.len(), PROVIDER_RATE_LIMIT_MAX_RETRIES + 1);
    assert!(timeline.attempts[..PROVIDER_RATE_LIMIT_MAX_RETRIES]
        .iter()
        .all(|attempt| attempt.outcome == ProviderAttemptOutcome::Retrying));
    assert_eq!(
        timeline.attempts.last().expect("final attempt").outcome,
        ProviderAttemptOutcome::RetriesExhausted
    );
    assert_eq!(
        timeline.pending_fallback_model_ref.as_deref(),
        Some("openai/gpt-5.4-mini")
    );
}

#[tokio::test]
async fn hard_quota_stops_after_one_request_and_propagates_typed_diagnostics() {
    let script = Arc::new(Mutex::new(vec![
        ScriptedFailure::HardQuota,
        ScriptedFailure::Succeed,
    ]));
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            ProviderCandidate {
                model_ref: "bigmodel@default/glm-5.2".into(),
                provider_name: "bigmodel".into(),
                transport_id: None,
                resolved_image_input: false,
                provider: Arc::new(ScriptedProvider {
                    script: script.clone(),
                    quota_identity: None,
                }),
            },
            scripted_candidate("openai/gpt-5.4-mini", vec![ScriptedFailure::Succeed]),
        ],
    };
    let error = provider
        .complete_turn_with_diagnostics(plain_turn_request())
        .await
        .expect_err("hard quota must stop after the first request");
    assert_eq!(script.lock().await.len(), 1, "only one request dispatched");
    let timeline = crate::provider::provider_attempt_timeline(&error).unwrap();
    assert_eq!(timeline.attempts.len(), 1);
    let attempt = &timeline.attempts[0];
    assert_eq!(attempt.failure_kind.as_deref(), Some("rate_limited"));
    assert_eq!(attempt.outcome, ProviderAttemptOutcome::RetriesExhausted);
    assert_eq!(
        attempt
            .transport_diagnostics
            .as_ref()
            .unwrap()
            .throttle
            .as_ref()
            .unwrap()
            .kind,
        crate::provider::ProviderThrottleKind::HardQuota
    );
    assert_eq!(
        timeline.pending_fallback_model_ref.as_deref(),
        Some("openai/gpt-5.4-mini")
    );
    assert_eq!(
        timeline.pending_fallback_disposition,
        Some(crate::provider::ProviderFallbackDisposition::Deferred)
    );
}

#[tokio::test]
async fn deterministic_provider_error_does_not_retry_same_request() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![scripted_candidate(
            "openai/gpt-5.4",
            vec![
                ScriptedFailure::DeterministicError,
                ScriptedFailure::Succeed,
            ],
        )],
    };

    let error = provider
        .complete_turn(plain_turn_request())
        .await
        .expect_err("deterministic provider error should fail fast");
    let timeline = crate::provider::provider_attempt_timeline(&error).expect("timeline");
    assert_eq!(timeline.attempts.len(), 1);
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("contract_error")
    );
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::FailFastAborted
    );
}

#[tokio::test]
async fn gemini_refusal_stops_fallback_but_other_response_failures_keep_existing_policy() {
    for (body, blocked, expected_attempts) in [
        (
            serde_json::json!({
                "candidates": [{"finishReason": "PROHIBITED_CONTENT"}],
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }),
            true,
            1,
        ),
        (
            serde_json::json!({
                "promptFeedback": {"blockReason": "SAFETY"},
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }),
            true,
            1,
        ),
        (
            serde_json::json!({
                "candidates": [{
                    "finishReason": "SAFETY",
                    "content": {"parts": [{"functionCall": {"args": {}}}]}
                }],
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }),
            true,
            1,
        ),
        (
            serde_json::json!({
                "promptFeedback": {"blockReason": "SAFETY"},
                "candidates": [{"content": {"parts": [{"functionCall": {"args": {}}}]}}],
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }),
            true,
            1,
        ),
        (
            serde_json::json!({
                "candidates": [{"finishReason": "STOP", "content": {}}],
                "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 2}
            }),
            false,
            provider_max_attempts(),
        ),
        (serde_json::json!({"candidates": "invalid"}), false, 1),
        (
            serde_json::json!({
                "candidates": [{"content": {"parts": [{"functionCall": {"args": {}}}]}}]
            }),
            false,
            1,
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let app = Router::new().fallback(post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            async move { Json(body) }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let home = tempfile::tempdir().unwrap();
        let provider = FallbackProvider {
            record_retry: |_| {},
            candidates: vec![
                http_candidate(
                    &base_url,
                    home.path(),
                    ProviderTransportKind::GeminiGenerateContent,
                ),
                scripted_candidate("openai/fallback", vec![ScriptedFailure::Succeed]),
            ],
        };
        let error = provider
            .complete_turn(plain_turn_request())
            .await
            .unwrap_err();
        server.abort();
        let timeline = provider_attempt_timeline(&error).unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), expected_attempts);
        assert_eq!(timeline.attempts.len(), expected_attempts);
        assert_eq!(timeline.pending_fallback_model_ref.is_none(), blocked);
        assert_eq!(timeline.pending_fallback_disposition.is_none(), blocked);
        assert_eq!(
            timeline.attempts.last().unwrap().advanced_to_fallback,
            !blocked
        );
        let classification = classify_provider_error(&error);
        if blocked {
            assert_eq!(provider_error_code(&error), Some("gemini_content_blocked"));
            assert_eq!(classification.disposition, RetryDisposition::FailFast);
            assert_eq!(
                timeline.attempts[0].outcome,
                ProviderAttemptOutcome::FailFastAborted
            );
        } else if expected_attempts > 1 {
            assert_eq!(classification.kind, ProviderFailureKind::EmptyResponse);
            assert_eq!(
                timeline.pending_fallback_disposition,
                Some(crate::provider::ProviderFallbackDisposition::Immediate)
            );
        } else {
            assert_eq!(classification.kind, ProviderFailureKind::InvalidResponse);
        }
        if blocked || expected_attempts > 1 {
            let usage = timeline.aggregated_token_usage.as_ref().unwrap();
            assert_eq!(usage.input_tokens, 11 * expected_attempts as u64);
            assert_eq!(usage.output_tokens, 2 * expected_attempts as u64);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn retry_without_server_hint_keeps_computed_backoff() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![scripted_candidate(
            "openai/gpt-5.4",
            vec![
                ScriptedFailure::ServerError { retry_after: None },
                ScriptedFailure::ServerError { retry_after: None },
                ScriptedFailure::Succeed,
            ],
        )],
    };

    let task = tokio::spawn(async move {
        provider
            .complete_turn_with_diagnostics(plain_turn_request())
            .await
            .expect("third attempt should succeed")
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    let (_, diagnostics) = task.await.expect("retry task should complete");
    let timeline = diagnostics.expect("timeline");
    assert_eq!(timeline.attempts.len(), 3);
    assert!(matches!(
        timeline.attempts[0].backoff_ms,
        Some(2_000..=2_500)
    ));
    assert_eq!(
        timeline.attempts[0].backoff_source.as_deref(),
        Some("server_error_exponential_backoff")
    );
    assert!(matches!(
        timeline.attempts[1].backoff_ms,
        Some(4_000..=5_000)
    ));
    assert_eq!(
        timeline.attempts[1].backoff_source.as_deref(),
        Some("server_error_exponential_backoff")
    );
}

#[tokio::test(start_paused = true)]
async fn shared_account_busy_does_not_block_initial_turn_request() {
    let identity = ProviderQuotaIdentity::coarse("fallback-test", "shared-account-initial-request");
    let held = ProviderQuotaCoordinator::global()
        .acquire(identity.clone())
        .await;
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            scripted_candidate_with_identity(
                "openai/gpt-5.4",
                vec![ScriptedFailure::Succeed],
                identity.clone(),
            ),
            scripted_candidate("openai/gpt-5.4-mini", vec![ScriptedFailure::Succeed]),
        ],
    };

    let task = tokio::spawn(async move {
        provider
            .complete_turn_with_diagnostics(plain_turn_request())
            .await
    });
    tokio::task::yield_now().await;
    let (_, diagnostics) = task
        .await
        .expect("provider task should complete")
        .expect("initial request should not be intercepted");
    let timeline = diagnostics.expect("timeline");
    assert_eq!(timeline.attempts.len(), 1);
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::Succeeded
    );
    assert_eq!(timeline.pending_fallback_model_ref, None);
    drop(held);
}

#[tokio::test(start_paused = true)]
async fn rate_limit_retries_in_current_turn_instead_of_fallback() {
    let identity = ProviderQuotaIdentity::coarse("fallback-test", "shared-account-current-turn");
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            scripted_candidate_with_identity(
                "openai/gpt-5.4",
                vec![
                    ScriptedFailure::Fail {
                        retry_after: Some(std::time::Duration::from_secs(10)),
                    },
                    ScriptedFailure::Succeed,
                ],
                identity,
            ),
            scripted_candidate("openai/gpt-5.4-mini", vec![ScriptedFailure::Succeed]),
        ],
    };

    let task = tokio::spawn(async move {
        provider
            .complete_turn_with_diagnostics(plain_turn_request())
            .await
            .expect("rate-limit retry should stay in the current turn")
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    let (_, diagnostics) = task.await.expect("provider task should complete");
    let timeline = diagnostics.expect("timeline");
    assert_eq!(timeline.attempts.len(), 2);
    assert_eq!(
        timeline.attempts[0].outcome,
        ProviderAttemptOutcome::Retrying
    );
    assert_eq!(
        timeline.attempts[1].outcome,
        ProviderAttemptOutcome::Succeeded
    );
    assert_eq!(
        timeline.winning_model_ref.as_deref(),
        Some("openai/gpt-5.4")
    );
    assert_eq!(timeline.pending_fallback_model_ref, None);
}

fn candidate(model_ref: &str, policy: ProviderContextManagementPolicy) -> ProviderCandidate {
    ProviderCandidate {
        model_ref: model_ref.into(),
        provider_name: "anthropic".into(),
        transport_id: None,
        resolved_image_input: false,
        provider: Arc::new(PolicyProvider {
            policy: Some(policy),
        }),
    }
}

fn recording_candidate(
    model_ref: &str,
    provider_name: &str,
    provider: RecordingProvider,
) -> ProviderCandidate {
    ProviderCandidate {
        model_ref: model_ref.into(),
        provider_name: provider_name.into(),
        transport_id: None,
        resolved_image_input: false,
        provider: Arc::new(provider),
    }
}

fn search_capability(
    provider_id: &str,
    model_ref: &str,
    backend_kind: &str,
) -> ProviderBuiltinWebSearchCapability {
    ProviderBuiltinWebSearchCapability {
        kind: ProviderNativeWebSearchKind::Anthropic,
        provider_id: provider_id.into(),
        provider_model_ref: model_ref.into(),
        provider_transport: "anthropic_messages".into(),
        provider_base_url: "https://api.example.test".into(),
        advertised_tool_type: "web_search_20250305".into(),
        backend_kind: backend_kind.into(),
    }
}

#[test]
fn active_lineage_keeps_freeform_schema_with_a_json_only_fallback() -> Result<()> {
    use crate::provider::build_openai_responses_request;
    use crate::provider::tool_schema::ToolSchemaContract;
    use crate::provider::transports::OpenAiResponsesTransportContract;
    use holon_domain::{ToolFreeformGrammar, ToolSpec};
    use serde_json::json;
    let json = ToolSpec {
        name: "ApplyPatch".into(),
        description: "Apply patch".into(),
        input_schema: json!({"type":"object","properties":{"patch":{"type":"string"}}}),
        freeform_grammar: None,
    };
    let home = tempfile::tempdir()?;
    let mut primary = http_candidate(
        "http://example.invalid",
        home.path(),
        ProviderTransportKind::OpenAiResponses,
    );
    primary.model_ref = "openai-codex@default/gpt-5.4".into();
    let mut fallback = http_candidate(
        "http://example.invalid",
        home.path(),
        ProviderTransportKind::AnthropicMessages,
    );
    fallback.model_ref = "anthropic@default/claude-sonnet-4-6".into();
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![primary, fallback],
    };
    assert!(provider.supports_freeform_grammar_tools());
    let dsl = ToolSpec {
        freeform_grammar: Some(ToolFreeformGrammar {
            syntax: "lark".into(),
            definition: "start: /.+/".into(),
        }),
        ..json.clone()
    };
    let tools = provider.prompt_tool_specs(&[dsl]);
    assert!(tools[0].freeform_grammar.is_some());
    let request = ProviderTurnRequest::plain("test", Vec::new(), tools);
    let body = build_openai_responses_request(
        "gpt-5.4",
        256,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        None,
        None,
    )?;
    assert_eq!(body["tools"][0]["type"], "custom");
    assert_eq!(body["tools"][0]["format"]["type"], "grammar");
    let fallback = provider
        .select_model_lineage(&ModelRouteRef::parse(
            "anthropic@default/claude-sonnet-4-6",
        )?)
        .unwrap();
    assert!(!fallback.supports_freeform_grammar_tools());
    let expected = json.input_schema.clone();
    let tools = fallback.prompt_tool_specs(&[json]);
    assert_eq!(tools[0].input_schema, expected);
    assert_eq!(tools[0].input_schema["type"], "object");
    assert!(tools[0].freeform_grammar.is_none());
    Ok(())
}

#[test]
fn runtime_model_hint_compacts_only_canonical_default_routes() {
    assert_eq!(
        runtime_model_hint(
            "openai@default/gpt-5.4",
            "anthropic@default/claude-sonnet-4-6"
        ),
        "Runtime: active_model=anthropic/claude-sonnet-4-6 requested_model=openai/gpt-5.4"
    );
    assert_eq!(
            runtime_model_hint(
                "dashscope@token-plan/qwen3.7-max",
                "volcengine@plan/glm-5.2"
            ),
            "Runtime: active_model=volcengine@plan/glm-5.2 requested_model=dashscope@token-plan/qwen3.7-max"
        );
    assert_eq!(
        runtime_model_hint("openai/gpt-5.4", "openai/gpt-5.4"),
        "Runtime: active_model=openai/gpt-5.4"
    );
}

#[test]
fn model_attempt_hint_keeps_plain_and_structured_system_bytes_equal() {
    for base in ["", "  ", "base\n"] {
        for (active, hint) in [
            ("gemini/test", "Runtime: active_model=gemini/test"),
            (
                "openai/fallback",
                "Runtime: active_model=openai/fallback requested_model=gemini/test",
            ),
        ] {
            let separator = if base.trim().is_empty() { "" } else { "\n\n" };
            let expected = format!("{base}{separator}{hint}");
            for structured in [false, true] {
                let mut request = ProviderTurnRequest::plain(base, Vec::new(), Vec::new());
                if structured {
                    request.prompt_frame.system_blocks.push(PromptContentBlock {
                        text: base.into(),
                        stability: PromptStability::Stable,
                        cache_breakpoint: true,
                    });
                }
                let attempt = request_for_model_attempt(&request, "gemini/test", active);
                assert_eq!(attempt.prompt_frame.system_prompt, expected);
                if structured {
                    assert_eq!(
                        attempt
                            .prompt_frame
                            .system_blocks
                            .iter()
                            .map(|block| block.text.as_str())
                            .collect::<String>(),
                        expected
                    );
                    assert_eq!(
                        attempt.prompt_frame.system_blocks[0],
                        request.prompt_frame.system_blocks[0]
                    );
                } else {
                    assert!(attempt.prompt_frame.system_blocks.is_empty());
                }
            }
        }
    }
}

#[test]
fn context_management_policy_requires_full_policy_match() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            candidate("anthropic/one", policy(100_000)),
            candidate("anthropic/two", policy(50_000)),
        ],
    };

    assert_eq!(provider.context_management_policy(), None);
}

#[test]
fn builtin_web_search_uses_current_turn_candidate() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            ProviderCandidate {
                model_ref: "zai/glm-4.7".into(),
                provider_name: "zai".into(),
                transport_id: None,
                resolved_image_input: false,
                provider: Arc::new(SearchProvider {
                    capability: Some(search_capability(
                        "zai",
                        "zai/glm-4.7",
                        "zai_web_search_prime",
                    )),
                }),
            },
            ProviderCandidate {
                model_ref: "deepseek/deepseek-v4-flash".into(),
                provider_name: "deepseek".into(),
                transport_id: None,
                resolved_image_input: false,
                provider: Arc::new(SearchProvider { capability: None }),
            },
        ],
    };

    let capability = provider
        .builtin_web_search()
        .expect("active candidate should expose builtin search");
    assert_eq!(capability.provider_id, "zai");
    assert_eq!(capability.provider_model_ref, "zai/glm-4.7");
    assert_eq!(capability.backend_kind, "zai_web_search_prime");
}

#[test]
fn resolved_image_input_uses_current_turn_candidate() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            ProviderCandidate {
                model_ref: "volcengine@plan/glm-5.2".into(),
                provider_name: "volcengine".into(),
                transport_id: None,
                resolved_image_input: false,
                provider: Arc::new(PolicyProvider { policy: None }),
            },
            ProviderCandidate {
                model_ref: "anthropic/claude-sonnet-4-6".into(),
                provider_name: "anthropic".into(),
                transport_id: None,
                resolved_image_input: true,
                provider: Arc::new(PolicyProvider { policy: None }),
            },
        ],
    };

    assert_eq!(provider.resolved_image_input_support(), Some(false));

    let reversed = FallbackProvider {
        record_retry: |_| {},
        candidates: provider.candidates.into_iter().rev().collect(),
    };
    assert_eq!(reversed.resolved_image_input_support(), Some(true));
}

fn image_candidate(
    model_ref: &str,
    resolved_image_input: bool,
    conversations: Arc<Mutex<Vec<Vec<ConversationMessage>>>>,
) -> ProviderCandidate {
    ProviderCandidate {
        model_ref: model_ref.into(),
        provider_name: "openai".into(),
        transport_id: None,
        resolved_image_input,
        provider: Arc::new(ConversationRecorder { conversations }),
    }
}

fn user_image_message(optional_history: bool) -> ConversationMessage {
    ConversationMessage::UserImage {
        prompt: "workspace screenshot".into(),
        media_type: "image/png".into(),
        data_base64: "aGVsbG8=".to_owned().into(),
        optional_history,
    }
}

#[tokio::test]
async fn required_image_fails_fast_for_route_without_image_input() {
    let conversations = Arc::new(Mutex::new(Vec::new()));
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![image_candidate(
            "openai/text-only",
            false,
            conversations.clone(),
        )],
    };

    let error = provider
        .complete_turn_with_diagnostics(ProviderTurnRequest::plain(
            "base",
            vec![user_image_message(false)],
            Vec::new(),
        ))
        .await
        .expect_err("required image must not reach a route without image input");

    assert_eq!(
        crate::provider::provider_error_code(&error),
        Some("image_input_unsupported")
    );
    assert!(conversations.lock().await.is_empty());
    let timeline = crate::provider::provider_attempt_timeline(&error).expect("timeline");
    assert_eq!(timeline.attempts.len(), 1);
    assert_eq!(
        timeline.attempts[0].outcome,
        crate::provider::ProviderAttemptOutcome::FailFastAborted
    );
    assert_eq!(
        timeline.attempts[0].failure_kind.as_deref(),
        Some("contract_error")
    );
}

#[tokio::test]
async fn optional_history_image_degrades_to_text_only_for_route_without_image_input() {
    let conversations = Arc::new(Mutex::new(Vec::new()));
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![image_candidate(
            "openai/text-only",
            false,
            conversations.clone(),
        )],
    };

    provider
        .complete_turn_with_diagnostics(ProviderTurnRequest::plain(
            "base",
            vec![user_image_message(true)],
            Vec::new(),
        ))
        .await
        .expect("optional historical image must degrade instead of failing");

    let recorded = conversations.lock().await;
    assert_eq!(recorded.len(), 1);
    assert!(!recorded[0]
        .iter()
        .any(|message| matches!(message, ConversationMessage::UserImage { .. })));
    assert!(recorded[0].iter().any(|message| matches!(
        message,
        ConversationMessage::UserText(text) if text.contains("does not support image input")
    )));
}

#[tokio::test]
async fn route_with_image_input_keeps_native_image_attachments() {
    let conversations = Arc::new(Mutex::new(Vec::new()));
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![image_candidate(
            "openai/vision",
            true,
            conversations.clone(),
        )],
    };

    provider
        .complete_turn_with_diagnostics(ProviderTurnRequest::plain(
            "base",
            vec![user_image_message(false)],
            Vec::new(),
        ))
        .await
        .expect("image-capable route must keep the attachment");

    let recorded = conversations.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0]
            .iter()
            .filter(|message| matches!(message, ConversationMessage::UserImage { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn generate_image_uses_current_turn_candidate() {
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![ProviderCandidate {
            model_ref: "openai-codex/gpt-5.5".into(),
            provider_name: "openai-codex".into(),
            transport_id: None,
            resolved_image_input: false,
            provider: Arc::new(ImageProvider),
        }],
    };

    let response = provider
        .generate_image(ProviderGenerateImageRequest {
            material: Default::default(),
            prompt: "holon".into(),
            size: None,
            aspect_ratio: None,
            resolution: None,
            background: None,
            output_format: None,
        })
        .await
        .expect("image request should be forwarded");

    assert_eq!(response.provider, "test");
    assert_eq!(response.model, "holon");
}

#[tokio::test]
async fn model_visible_hint_marks_normal_attempt_with_active_model_only() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let system_blocks = Arc::new(Mutex::new(Vec::new()));
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![recording_candidate(
            "openai/gpt-5.4",
            "openai",
            RecordingProvider {
                name: "primary",
                fail: false,
                prompts: prompts.clone(),
                system_blocks: system_blocks.clone(),
            },
        )],
    };

    let (_response, timeline) = provider
        .complete_turn_with_diagnostics(ProviderTurnRequest::plain("base", Vec::new(), Vec::new()))
        .await
        .expect("provider should succeed");

    let recorded = prompts.lock().await;
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].contains("Runtime: active_model=openai/gpt-5.4"));
    assert!(!recorded[0].contains("requested_model="));
    assert!(system_blocks.lock().await[0].is_empty());
    let timeline = timeline.expect("timeline");
    assert_eq!(timeline.requested_model_ref, "openai/gpt-5.4");
    assert_eq!(timeline.active_model_ref.as_deref(), Some("openai/gpt-5.4"));
}

#[tokio::test]
async fn fallback_candidate_is_recorded_but_not_invoked_in_the_same_turn() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let system_blocks = Arc::new(Mutex::new(Vec::new()));
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            recording_candidate(
                "openai/gpt-5.4",
                "openai",
                RecordingProvider {
                    name: "primary",
                    fail: true,
                    prompts: prompts.clone(),
                    system_blocks: system_blocks.clone(),
                },
            ),
            recording_candidate(
                "anthropic/claude-sonnet-4-6",
                "anthropic",
                RecordingProvider {
                    name: "fallback",
                    fail: false,
                    prompts: prompts.clone(),
                    system_blocks: system_blocks.clone(),
                },
            ),
        ],
    };

    let error = provider
        .complete_turn_with_diagnostics(ProviderTurnRequest::plain("base", Vec::new(), Vec::new()))
        .await
        .expect_err("fallback should be deferred to the next turn");

    let recorded = prompts.lock().await;
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].contains("Runtime: active_model=openai/gpt-5.4"));
    assert!(!recorded[0].contains("requested_model="));
    assert!(system_blocks.lock().await[0].is_empty());
    let timeline = super::super::provider_attempt_timeline(&error).expect("timeline");
    assert_eq!(timeline.requested_model_ref, "openai/gpt-5.4");
    assert_eq!(timeline.active_model_ref.as_deref(), None);
    assert_eq!(timeline.winning_model_ref.as_deref(), None);
    assert_eq!(
        timeline.pending_fallback_model_ref.as_deref(),
        Some("anthropic/claude-sonnet-4-6")
    );
    assert_eq!(
        timeline.pending_fallback_disposition,
        Some(crate::provider::ProviderFallbackDisposition::Immediate)
    );
    assert_eq!(timeline.attempts.len(), 1);
    assert!(timeline.attempts[0].advanced_to_fallback);
}

#[tokio::test]
async fn fallback_wrapped_gemini_preserves_rendered_system_text() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let observed = captured.clone();
    let app = Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let observed = observed.clone();
        async move {
            observed.lock().await.push(body);
            Json(serde_json::json!({
                "candidates": [{
                    "finishReason": "STOP",
                    "content": {"parts": [{"text": "ok"}]}
                }]
            }))
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![
            http_candidate(
                &base_url,
                home.path(),
                ProviderTransportKind::GeminiGenerateContent,
            ),
            scripted_candidate("openai/fallback", vec![ScriptedFailure::Succeed]),
        ],
    };
    let shared = "## identity\nfixed role\n\n";
    let suffix = "## response_language\nUse the operator's language.";
    let rendered = format!("{shared}{suffix}");
    for structured in [false, true] {
        let mut request = ProviderTurnRequest::plain(&rendered, Vec::new(), Vec::new());
        if structured {
            request.prompt_frame.system_blocks = vec![
                PromptContentBlock {
                    text: shared.into(),
                    stability: PromptStability::Stable,
                    cache_breakpoint: true,
                },
                PromptContentBlock {
                    text: suffix.into(),
                    stability: PromptStability::AgentScoped,
                    cache_breakpoint: true,
                },
            ];
        }
        provider.complete_turn(request).await.unwrap();
    }
    server.abort();
    let captured = captured.lock().await;
    assert_eq!(captured.len(), 2);
    for body in captured.iter() {
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            format!("{rendered}\n\nRuntime: active_model=gemini/test")
        );
    }
}

#[tokio::test]
async fn model_visible_hint_preserves_structured_system_shape_and_marks_cache_breakpoint() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let system_blocks = Arc::new(Mutex::new(Vec::new()));
    let provider = FallbackProvider {
        record_retry: |_| {},
        candidates: vec![recording_candidate(
            "openai/gpt-5.4",
            "openai",
            RecordingProvider {
                name: "primary",
                fail: false,
                prompts: prompts.clone(),
                system_blocks: system_blocks.clone(),
            },
        )],
    };

    let request = ProviderTurnRequest {
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: None,
        prompt_frame: ProviderPromptFrame::structured(
            "existing",
            vec![PromptContentBlock {
                text: "existing".into(),
                stability: PromptStability::Stable,
                cache_breakpoint: false,
            }],
            Vec::new(),
            None,
        ),
        conversation: Vec::new(),
        tools: Vec::new(),
        native_web_search: None,
        response_format: None,
    };

    provider
        .complete_turn_with_diagnostics(request)
        .await
        .expect("provider should succeed");

    let recorded_blocks = system_blocks.lock().await;
    assert_eq!(recorded_blocks[0].len(), 2);
    let hint = recorded_blocks[0].last().expect("runtime hint block");
    assert_eq!(hint.text, "\n\nRuntime: active_model=openai/gpt-5.4");
    assert_eq!(hint.stability, PromptStability::TurnScoped);
    assert!(hint.cache_breakpoint);
    assert_eq!(
        recorded_blocks[0]
            .iter()
            .map(|block| block.text.as_str())
            .collect::<String>(),
        prompts.lock().await[0].strip_prefix("primary:").unwrap()
    );
}
