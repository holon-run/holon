use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::time::sleep;
use tracing::warn;

use super::{
    aggregate_attempt_token_usage, provider_error_cache_usage, provider_error_code,
    provider_error_normalized_token_usage, provider_error_token_usage,
    provider_transport_diagnostics, provider_transport_error_with_code, provider_turn_error,
    quota::ProviderQuotaCoordinator,
    retry::{
        classify_provider_error, format_provider_failure, provider_error_retry_after,
        provider_fallback_disposition, provider_max_attempts, provider_retry_delay,
        provider_retry_jitter_seed, ProviderFailureClassification, ProviderFailureKind,
        ProviderRetryDelay, RetryDisposition, PROVIDER_RATE_LIMIT_MAX_RETRIES,
    },
    transports::is_dispatch_guard_rejected,
    AgentProvider, ConversationMessage, PromptContentBlock, ProviderAttemptOutcome,
    ProviderAttemptRecord, ProviderAttemptTimeline, ProviderBuiltinWebSearchCapability,
    ProviderContextManagementPolicy, ProviderGenerateImageRequest, ProviderGenerateImageResponse,
    ProviderNativeWebSearchRequest, ProviderTurnRequest, ProviderTurnResponse,
};
use crate::config::ModelRouteRef;
use holon_domain::PromptStability;

use std::sync::Arc;
#[derive(Clone)]
pub struct ProviderCandidate {
    pub model_ref: String,
    pub provider_name: String,
    /// Wire transport of this candidate, copied onto every attempt record.
    pub transport_id: Option<&'static str>,
    pub resolved_image_input: bool,
    pub provider: Arc<dyn AgentProvider>,
}

#[derive(Clone)]
pub struct FallbackProvider {
    candidates: Vec<ProviderCandidate>,
    record_retry: fn(std::time::Duration),
}

impl FallbackProvider {
    pub fn new(candidates: Vec<ProviderCandidate>, record_retry: fn(std::time::Duration)) -> Self {
        Self {
            candidates,
            record_retry,
        }
    }
}

#[cfg(test)]
mod tests;

#[async_trait]
impl AgentProvider for FallbackProvider {
    fn transport_id(&self) -> Option<&'static str> {
        self.candidates.first().and_then(|c| c.transport_id)
    }

    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let (response, _) = self.complete_turn_with_diagnostics(request).await?;
        Ok(response)
    }

    async fn complete_turn_with_diagnostics(
        &self,
        request: ProviderTurnRequest,
    ) -> Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)> {
        let requested_model_ref = self
            .candidates
            .first()
            .map(|candidate| candidate.model_ref.clone())
            .unwrap_or_default();
        let mut pending_fallback_model_ref = self
            .candidates
            .get(1)
            .map(|candidate| candidate.model_ref.clone());
        let mut timeline = Vec::new();
        let Some(candidate) = self.candidates.first() else {
            let source = anyhow!("all configured providers failed for this turn: no candidates");
            return Err(provider_turn_error(
                source.to_string(),
                ProviderAttemptTimeline {
                    aggregated_token_usage: None,
                    attempts: timeline,
                    requested_model_ref,
                    active_model_ref: None,
                    winning_model_ref: None,
                    pending_fallback_model_ref: None,
                    pending_fallback_disposition: None,
                },
                source,
            ));
        };
        let candidate_request = match lower_request_for_candidate(&request, candidate) {
            Ok(lowered) => lowered,
            Err(error) => {
                let source = error.context("provider image input admission failed");
                return Err(provider_turn_error(
                    source.to_string(),
                    ProviderAttemptTimeline {
                        aggregated_token_usage: None,
                        attempts: vec![ProviderAttemptRecord {
                            provider: candidate.provider_name.clone(),
                            model_ref: candidate.model_ref.clone(),
                            transport: candidate.transport_id.map(str::to_string),
                            served_service_tier: None,
                            attempt: 1,
                            max_attempts: provider_max_attempts(),
                            started_at: None,
                            completed_at: None,
                            duration_ms: None,
                            failure_kind: Some(
                                ProviderFailureKind::ContractError.as_str().to_string(),
                            ),
                            disposition: Some(RetryDisposition::FailFast.as_str().to_string()),
                            outcome: ProviderAttemptOutcome::FailFastAborted,
                            advanced_to_fallback: pending_fallback_model_ref.is_some(),
                            backoff_ms: None,
                            backoff_source: None,
                            token_usage: None,
                            cache_usage: None,
                            normalized_token_usage: None,
                            provider_message_id: None,
                            provider_request_id: None,
                            provider_http_trace_id: None,
                            transport_diagnostics: None,
                            transport_timeline: None,
                        }],
                        requested_model_ref,
                        active_model_ref: None,
                        winning_model_ref: None,
                        pending_fallback_model_ref,
                        pending_fallback_disposition: None,
                    },
                    source,
                ));
            }
        };
        let max_attempts = provider_max_attempts();
        let last_error;
        let pending_fallback_disposition;
        let mut attempt = 1;
        let mut rate_limit_retries = 0;
        let mut shared_account_retry = false;
        loop {
            let attempt_request = request_for_model_attempt(
                &candidate_request,
                &requested_model_ref,
                &candidate.model_ref,
            );
            let attempt_started_at = Utc::now();
            let attempt_started = std::time::Instant::now();
            let quota_identity = candidate.provider.quota_identity();
            let quota_permit = if shared_account_retry {
                quota_identity.clone().map(|identity| async move {
                    ProviderQuotaCoordinator::global().acquire(identity).await
                })
            } else {
                None
            };
            let quota_permit = match quota_permit {
                Some(acquire) => Some(acquire.await),
                None => None,
            };
            let result = candidate.provider.complete_turn(attempt_request).await;
            if let Some(guard) = request.dispatch_guard.as_ref() {
                guard.after_response(&result)?;
            }
            match result {
                Ok(response) => {
                    drop(quota_permit);
                    let attempt_completed_at = Utc::now();
                    let attempt_duration_ms = attempt_started.elapsed().as_millis() as u64;
                    timeline.push(ProviderAttemptRecord {
                        provider: candidate.provider_name.clone(),
                        model_ref: candidate.model_ref.clone(),
                        transport: candidate.transport_id.map(str::to_string),
                        attempt,
                        max_attempts,
                        started_at: Some(attempt_started_at),
                        completed_at: Some(attempt_completed_at),
                        duration_ms: Some(attempt_duration_ms),
                        failure_kind: None,
                        disposition: None,
                        outcome: ProviderAttemptOutcome::Succeeded,
                        advanced_to_fallback: false,
                        backoff_ms: None,
                        backoff_source: None,
                        token_usage: Some(crate::types::TokenUsage::new(
                            response.input_tokens,
                            response.output_tokens,
                        )),
                        cache_usage: response.cache_usage.clone(),
                        normalized_token_usage: response.normalized_token_usage,
                        served_service_tier: response
                            .request_diagnostics
                            .as_ref()
                            .and_then(|diagnostics| diagnostics.openai_request_controls.as_ref())
                            .and_then(|controls| controls.served_service_tier.clone()),
                        provider_message_id: response.provider_message_id.clone(),
                        provider_request_id: response.provider_request_id.clone(),
                        provider_http_trace_id: None,
                        transport_diagnostics: None,
                        transport_timeline: response
                            .request_diagnostics
                            .as_ref()
                            .and_then(|diagnostics| diagnostics.transport_timeline.clone()),
                    });
                    let diagnostics = ProviderAttemptTimeline {
                        aggregated_token_usage: aggregate_attempt_token_usage(&timeline),
                        attempts: timeline,
                        requested_model_ref: requested_model_ref.clone(),
                        active_model_ref: Some(candidate.model_ref.clone()),
                        winning_model_ref: Some(candidate.model_ref.clone()),
                        pending_fallback_model_ref: None,
                        pending_fallback_disposition: None,
                    };
                    return Ok((response, Some(diagnostics)));
                }
                Err(error) => {
                    if is_dispatch_guard_rejected(&error) {
                        if timeline.is_empty() {
                            return Err(error);
                        }
                        // Preserve prior HTTP attempts without recording a local denial as IO.
                        return Err(provider_turn_error(
                            error.to_string(),
                            ProviderAttemptTimeline {
                                aggregated_token_usage: aggregate_attempt_token_usage(&timeline),
                                attempts: timeline,
                                requested_model_ref,
                                active_model_ref: None,
                                winning_model_ref: None,
                                pending_fallback_model_ref: None,
                                pending_fallback_disposition: None,
                            },
                            error,
                        ));
                    }
                    let classification = classify_provider_error(&error);
                    let retry_after = provider_error_retry_after(&error);
                    if request.dispatch_guard.is_none()
                        && classification.kind == super::retry::ProviderFailureKind::RateLimited
                    {
                        if let Some(permit) = quota_permit.as_ref() {
                            permit.record_rate_limit(retry_after);
                        } else if let Some(identity) = quota_identity {
                            ProviderQuotaCoordinator::global()
                                .record_rate_limit(identity, retry_after);
                        }
                        shared_account_retry = true;
                    } else {
                        shared_account_retry = false;
                    }
                    let attempt_completed_at = Utc::now();
                    let attempt_duration_ms = attempt_started.elapsed().as_millis() as u64;
                    let rate_limit_retry_available = classification.kind
                        == super::retry::ProviderFailureKind::RateLimited
                        && rate_limit_retries < PROVIDER_RATE_LIMIT_MAX_RETRIES;
                    let shared_recovery = request.dispatch_guard.is_some()
                        && matches!(
                            classification.kind,
                            super::retry::ProviderFailureKind::Timeout
                                | super::retry::ProviderFailureKind::Connection
                                | super::retry::ProviderFailureKind::ServerError
                                | super::retry::ProviderFailureKind::RateLimited
                                | super::retry::ProviderFailureKind::PaymentRequired
                        );
                    let should_retry = !shared_recovery
                        && classification.disposition == RetryDisposition::Retryable
                        && (attempt < max_attempts || rate_limit_retry_available);
                    let hard_quota = provider_transport_diagnostics(&error)
                        .and_then(|diagnostics| diagnostics.throttle.as_ref())
                        .is_some_and(|throttle| {
                            throttle.kind == super::ProviderThrottleKind::HardQuota
                        });
                    let retry_delay = if hard_quota {
                        Some(ProviderRetryDelay::SkipToFallback)
                    } else if should_retry {
                        Some(provider_retry_delay(
                            attempt,
                            classification.kind,
                            retry_after,
                            provider_retry_jitter_seed(
                                &candidate.provider_name,
                                &candidate.model_ref,
                            ),
                        ))
                    } else {
                        None
                    };
                    if let Some(ProviderRetryDelay::Wait { backoff, source }) = retry_delay {
                        timeline.push(ProviderAttemptRecord {
                            provider: candidate.provider_name.clone(),
                            model_ref: candidate.model_ref.clone(),
                            transport: candidate.transport_id.map(str::to_string),
                            served_service_tier: None,
                            attempt,
                            max_attempts,
                            started_at: Some(attempt_started_at),
                            completed_at: Some(attempt_completed_at),
                            duration_ms: Some(attempt_duration_ms),
                            failure_kind: Some(classification.kind.as_str().to_string()),
                            disposition: Some(classification.disposition.as_str().to_string()),
                            outcome: ProviderAttemptOutcome::Retrying,
                            advanced_to_fallback: false,
                            backoff_ms: Some(backoff.as_millis() as u64),
                            backoff_source: Some(source.as_str().to_string()),
                            token_usage: provider_error_token_usage(&error).cloned(),
                            cache_usage: provider_error_cache_usage(&error).cloned(),
                            normalized_token_usage: provider_error_normalized_token_usage(&error)
                                .cloned(),
                            provider_message_id: None,
                            provider_request_id: None,
                            provider_http_trace_id: provider_transport_diagnostics(&error)
                                .and_then(|diagnostics| diagnostics.http_trace.as_ref())
                                .map(|trace| trace.capture_id.clone()),
                            transport_diagnostics: provider_transport_diagnostics(&error).cloned(),
                            transport_timeline: None,
                        });
                        warn!(
                            model_ref = %candidate.model_ref,
                            attempt,
                            max_attempts,
                            failure_kind = classification.kind.as_str(),
                            disposition = classification.disposition.as_str(),
                            backoff_ms = backoff.as_millis(),
                            backoff_source = source.as_str(),
                            "provider turn failed; retrying"
                        );
                        let retry_started = std::time::Instant::now();
                        drop(quota_permit);
                        sleep(backoff).await;
                        (self.record_retry)(retry_started.elapsed());
                        if classification.kind == super::retry::ProviderFailureKind::RateLimited {
                            rate_limit_retries = rate_limit_retries.saturating_add(1);
                        }
                        attempt = attempt.saturating_add(1);
                        continue;
                    }
                    // Content refusal is not an availability failure to route around.
                    if provider_error_code(&error) == Some("gemini_content_blocked") {
                        pending_fallback_model_ref = None;
                    }
                    pending_fallback_disposition = pending_fallback_model_ref
                        .as_ref()
                        .map(|_| provider_fallback_disposition(classification.kind));
                    let has_fallback = pending_fallback_disposition.is_some();
                    if matches!(retry_delay, Some(ProviderRetryDelay::SkipToFallback)) {
                        warn!(
                            model_ref = %candidate.model_ref,
                            attempt,
                            max_attempts,
                            failure_kind = classification.kind.as_str(),
                            hard_quota,
                            server_hint_cap_ms = super::retry::PROVIDER_RETRY_SERVER_HINT_CAP_MS,
                            "provider throttle requires skipping remaining retries"
                        );
                    }
                    timeline.push(ProviderAttemptRecord {
                        provider: candidate.provider_name.clone(),
                        model_ref: candidate.model_ref.clone(),
                        transport: candidate.transport_id.map(str::to_string),
                        served_service_tier: None,
                        attempt,
                        max_attempts,
                        started_at: Some(attempt_started_at),
                        completed_at: Some(attempt_completed_at),
                        duration_ms: Some(attempt_duration_ms),
                        failure_kind: Some(classification.kind.as_str().to_string()),
                        disposition: Some(classification.disposition.as_str().to_string()),
                        outcome: match classification.disposition {
                            RetryDisposition::Retryable => ProviderAttemptOutcome::RetriesExhausted,
                            RetryDisposition::FailFast => ProviderAttemptOutcome::FailFastAborted,
                        },
                        advanced_to_fallback: has_fallback,
                        backoff_ms: None,
                        backoff_source: None,
                        token_usage: provider_error_token_usage(&error).cloned(),
                        cache_usage: provider_error_cache_usage(&error).cloned(),
                        normalized_token_usage: provider_error_normalized_token_usage(&error)
                            .cloned(),
                        provider_message_id: None,
                        provider_request_id: None,
                        provider_http_trace_id: provider_transport_diagnostics(&error)
                            .and_then(|diagnostics| diagnostics.http_trace.as_ref())
                            .map(|trace| trace.capture_id.clone()),
                        transport_diagnostics: provider_transport_diagnostics(&error).cloned(),
                        transport_timeline: None,
                    });
                    last_error = Some(error);
                    warn!(
                        model_ref = %candidate.model_ref,
                        attempt,
                        max_attempts,
                        failure_kind = classification.kind.as_str(),
                        disposition = classification.disposition.as_str(),
                        fallback_disposition = ?pending_fallback_disposition,
                        has_fallback,
                        "provider turn failed; {}", if has_fallback { "deferring fallback to next turn" } else { "no fallback remaining" }
                    );
                    break;
                }
            }
        }
        let error_summary = last_error
            .as_ref()
            .map(|error| format_provider_failure(&candidate.model_ref, max_attempts, error))
            .unwrap_or_else(|| format!("{}: provider failed", candidate.model_ref));
        let source_prefix = if pending_fallback_model_ref.is_some() {
            "current provider failed for this turn"
        } else {
            "all configured providers failed for this turn"
        };
        let source = last_error
            .unwrap_or_else(|| anyhow!("provider failed without an error"))
            .context(format!("{source_prefix}: {error_summary}"));
        Err(provider_turn_error(
            source.to_string(),
            ProviderAttemptTimeline {
                aggregated_token_usage: aggregate_attempt_token_usage(&timeline),
                attempts: timeline,
                requested_model_ref,
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref,
                pending_fallback_disposition,
            },
            source,
        ))
    }

    fn select_model_lineage(
        &self,
        model_ref: &ModelRouteRef,
    ) -> Option<std::sync::Arc<dyn AgentProvider>> {
        let model_ref = model_ref.as_string();
        let index = self
            .candidates
            .iter()
            .position(|candidate| candidate.model_ref == model_ref)?;
        Some(std::sync::Arc::new(Self {
            candidates: self.candidates[index..].to_vec(),
            record_retry: self.record_retry,
        }))
    }

    #[cfg(any(test, feature = "runtime-tests"))]
    fn configured_model_refs(&self) -> Vec<String> {
        self.candidates
            .iter()
            .map(|candidate| candidate.model_ref.clone())
            .collect()
    }

    fn prompt_capabilities(&self) -> Vec<super::ProviderPromptCapability> {
        let mut candidates = self.candidates.iter();
        let Some(first) = candidates.next() else {
            return vec![super::ProviderPromptCapability::FullRequestOnly];
        };

        let mut capabilities = first.provider.prompt_capabilities();
        for candidate in candidates {
            let candidate_capabilities = candidate.provider.prompt_capabilities();
            capabilities.retain(|capability| candidate_capabilities.contains(capability));
        }

        capabilities
    }

    fn supports_freeform_grammar_tools(&self) -> bool {
        // A fallback is a later turn with its own selected lineage/tool surface.
        // Its capabilities must not strip the active route's grammar metadata.
        self.candidates
            .first()
            .is_some_and(|candidate| candidate.provider.supports_freeform_grammar_tools())
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        self.candidates.first()?.provider.builtin_web_search()
    }

    fn resolved_image_input_support(&self) -> Option<bool> {
        self.candidates
            .first()
            .map(|candidate| candidate.resolved_image_input)
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        self.candidates
            .first()
            .ok_or_else(|| anyhow!("fallback provider has no candidates"))?
            .provider
            .generate_image(request)
            .await
    }

    async fn probe_builtin_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        self.candidates
            .first()
            .ok_or_else(|| anyhow!("fallback provider has no candidates"))?
            .provider
            .probe_builtin_web_search(request)
            .await
    }

    fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
        let mut candidates = self.candidates.iter();
        let first = candidates.next()?.provider.context_management_policy()?;
        for candidate in candidates {
            let candidate_policy = candidate.provider.context_management_policy()?;
            if candidate_policy != first {
                return None;
            }
        }
        Some(first)
    }
}

fn request_for_model_attempt(
    request: &ProviderTurnRequest,
    requested_model_ref: &str,
    active_model_ref: &str,
) -> ProviderTurnRequest {
    let mut request = request.clone();
    let hint = runtime_model_hint(requested_model_ref, active_model_ref);
    let hint = if request.prompt_frame.system_prompt.trim().is_empty() {
        hint
    } else {
        format!("\n\n{hint}")
    };
    request.prompt_frame.system_prompt.push_str(&hint);
    if !request.prompt_frame.system_blocks.is_empty() {
        request.prompt_frame.system_blocks.push(PromptContentBlock {
            text: hint,
            stability: PromptStability::TurnScoped,
            cache_breakpoint: true,
        });
    }
    request
}

fn runtime_model_hint(requested_model_ref: &str, active_model_ref: &str) -> String {
    let requested_model_display = compact_model_route_display(requested_model_ref);
    let active_model_display = compact_model_route_display(active_model_ref);
    if requested_model_ref == active_model_ref || requested_model_ref.is_empty() {
        format!("Runtime: active_model={active_model_display}")
    } else {
        format!(
            "Runtime: active_model={active_model_display} requested_model={requested_model_display}"
        )
    }
}

/// Image admission stays a resolved-route capability decision.
///
/// A candidate whose resolved route has no image input cannot serve a
/// required image, so the request fails fast before dispatch instead of
/// dropping the attachment or letting the upstream API reject it. Optional
/// historical attachments degrade to an explicit text-only note.
fn lower_request_for_candidate(
    request: &ProviderTurnRequest,
    candidate: &ProviderCandidate,
) -> Result<ProviderTurnRequest> {
    if candidate.resolved_image_input {
        return Ok(request.clone());
    }
    let mut lowered = request.clone();
    let mut dropped_history_images = 0usize;
    lowered.conversation.retain(|message| match message {
        ConversationMessage::UserImage {
            optional_history, ..
        } => {
            if *optional_history {
                dropped_history_images += 1;
                false
            } else {
                true
            }
        }
        _ => true,
    });
    if lowered
        .conversation
        .iter()
        .any(|message| matches!(message, ConversationMessage::UserImage { .. }))
    {
        return Err(image_input_unsupported_error(&candidate.model_ref));
    }
    if dropped_history_images > 0 {
        lowered
            .conversation
            .push(ConversationMessage::UserText(format!(
                "[Holon image attachment: omitted {dropped_history_images} optional historical \
                 image attachment(s) because the active model route does not support image input.]"
            )));
    }
    Ok(lowered)
}

fn image_input_unsupported_error(model_ref: &str) -> anyhow::Error {
    provider_transport_error_with_code(
        ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("image_input_unsupported"),
        None,
        None,
        format!(
            "active model route does not support image input; required image input cannot be \
             omitted: model_ref={model_ref}"
        ),
    )
}

fn compact_model_route_display(model_ref: &str) -> String {
    ModelRouteRef::parse(model_ref)
        .map(|model_ref| model_ref.as_compact_display())
        .unwrap_or_else(|_| model_ref.to_string())
}
