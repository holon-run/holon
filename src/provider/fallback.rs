use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::time::sleep;
use tracing::warn;

use super::{
    aggregate_attempt_token_usage,
    catalog::ProviderCandidate,
    provider_transport_diagnostics, provider_turn_error,
    retry::{
        classify_provider_error, format_provider_failure, provider_max_attempts,
        provider_retry_backoff, RetryDisposition,
    },
    AgentProvider, PromptContentBlock, ProviderAttemptOutcome, ProviderAttemptRecord,
    ProviderAttemptTimeline, ProviderContextManagementPolicy, ProviderTurnRequest,
    ProviderTurnResponse,
};
use crate::prompt::PromptStability;

#[derive(Clone)]
pub(super) struct FallbackProvider {
    pub(crate) candidates: Vec<ProviderCandidate>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use super::*;
    use crate::provider::{ModelBlock, ProviderCacheUsage, ProviderPromptFrame};

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

    #[async_trait]
    impl AgentProvider for PolicyProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
            Ok(ProviderTurnResponse {
                blocks: vec![ModelBlock::Text { text: "ok".into() }],
                stop_reason: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: Some(ProviderCacheUsage {
                    read_input_tokens: 0,
                    creation_input_tokens: 0,
                }),
                request_diagnostics: None,
            })
        }

        fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
            self.policy.clone()
        }
    }

    #[async_trait]
    impl AgentProvider for RecordingProvider {
        async fn complete_turn(
            &self,
            request: ProviderTurnRequest,
        ) -> Result<ProviderTurnResponse> {
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
                blocks: vec![ModelBlock::Text { text: "ok".into() }],
                stop_reason: None,
                input_tokens: 1,
                output_tokens: 1,
                cache_usage: Some(ProviderCacheUsage {
                    read_input_tokens: 0,
                    creation_input_tokens: 0,
                }),
                request_diagnostics: None,
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

    fn candidate(model_ref: &str, policy: ProviderContextManagementPolicy) -> ProviderCandidate {
        ProviderCandidate {
            model_ref: model_ref.into(),
            provider_name: "anthropic".into(),
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
            provider: Arc::new(provider),
        }
    }

    #[test]
    fn context_management_policy_requires_full_policy_match() {
        let provider = FallbackProvider {
            candidates: vec![
                candidate("anthropic/one", policy(100_000)),
                candidate("anthropic/two", policy(50_000)),
            ],
        };

        assert_eq!(provider.context_management_policy(), None);
    }

    #[tokio::test]
    async fn model_visible_hint_marks_normal_attempt_with_active_model_only() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let system_blocks = Arc::new(Mutex::new(Vec::new()));
        let provider = FallbackProvider {
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
            .complete_turn_with_diagnostics(ProviderTurnRequest::plain(
                "base",
                Vec::new(),
                Vec::new(),
            ))
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
    async fn model_visible_hint_marks_fallback_attempt_with_requested_model() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let system_blocks = Arc::new(Mutex::new(Vec::new()));
        let provider = FallbackProvider {
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

        let (_response, timeline) = provider
            .complete_turn_with_diagnostics(ProviderTurnRequest::plain(
                "base",
                Vec::new(),
                Vec::new(),
            ))
            .await
            .expect("fallback provider should succeed");

        let recorded = prompts.lock().await;
        assert_eq!(recorded.len(), 2);
        assert!(recorded[0].contains("Runtime: active_model=openai/gpt-5.4"));
        assert!(!recorded[0].contains("requested_model="));
        assert!(recorded[1].contains(
            "Runtime: active_model=anthropic/claude-sonnet-4-6 requested_model=openai/gpt-5.4"
        ));
        assert!(system_blocks.lock().await[0].is_empty());
        let timeline = timeline.expect("timeline");
        assert_eq!(timeline.requested_model_ref, "openai/gpt-5.4");
        assert_eq!(
            timeline.active_model_ref.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(
            timeline.winning_model_ref.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
    }

    #[tokio::test]
    async fn model_visible_hint_preserves_structured_system_shape_and_marks_cache_breakpoint() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let system_blocks = Arc::new(Mutex::new(Vec::new()));
        let provider = FallbackProvider {
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
            prompt_frame: ProviderPromptFrame::structured(
                "base",
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
        };

        provider
            .complete_turn_with_diagnostics(request)
            .await
            .expect("provider should succeed");

        let recorded_blocks = system_blocks.lock().await;
        assert_eq!(recorded_blocks[0].len(), 2);
        let hint = recorded_blocks[0].last().expect("runtime hint block");
        assert_eq!(hint.text, "Runtime: active_model=openai/gpt-5.4");
        assert_eq!(hint.stability, PromptStability::TurnScoped);
        assert!(hint.cache_breakpoint);
    }
}

#[async_trait]
impl AgentProvider for FallbackProvider {
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
        let mut errors = Vec::new();
        let mut timeline = Vec::new();
        for (candidate_index, candidate) in self.candidates.iter().enumerate() {
            let max_attempts = provider_max_attempts();
            let mut last_error = None;
            for attempt in 1..=max_attempts {
                let attempt_request =
                    request_for_model_attempt(&request, &requested_model_ref, &candidate.model_ref);
                match candidate.provider.complete_turn(attempt_request).await {
                    Ok(response) => {
                        timeline.push(ProviderAttemptRecord {
                            provider: candidate.provider_name.clone(),
                            model_ref: candidate.model_ref.clone(),
                            attempt,
                            max_attempts,
                            failure_kind: None,
                            disposition: None,
                            outcome: ProviderAttemptOutcome::Succeeded,
                            advanced_to_fallback: false,
                            backoff_ms: None,
                            token_usage: Some(crate::types::TokenUsage::new(
                                response.input_tokens,
                                response.output_tokens,
                            )),
                            transport_diagnostics: None,
                        });
                        let diagnostics = ProviderAttemptTimeline {
                            aggregated_token_usage: aggregate_attempt_token_usage(&timeline),
                            attempts: timeline,
                            requested_model_ref: requested_model_ref.clone(),
                            active_model_ref: Some(candidate.model_ref.clone()),
                            winning_model_ref: Some(candidate.model_ref.clone()),
                        };
                        return Ok((response, Some(diagnostics)));
                    }
                    Err(error) => {
                        let classification = classify_provider_error(&error);
                        let should_retry = classification.disposition
                            == RetryDisposition::Retryable
                            && attempt < max_attempts;
                        if should_retry {
                            let backoff = provider_retry_backoff(attempt);
                            timeline.push(ProviderAttemptRecord {
                                provider: candidate.provider_name.clone(),
                                model_ref: candidate.model_ref.clone(),
                                attempt,
                                max_attempts,
                                failure_kind: Some(classification.kind.as_str().to_string()),
                                disposition: Some(classification.disposition.as_str().to_string()),
                                outcome: ProviderAttemptOutcome::Retrying,
                                advanced_to_fallback: false,
                                backoff_ms: Some(backoff.as_millis() as u64),
                                token_usage: None,
                                transport_diagnostics: provider_transport_diagnostics(&error)
                                    .cloned(),
                            });
                            warn!(
                                model_ref = %candidate.model_ref,
                                attempt,
                                max_attempts,
                                failure_kind = classification.kind.as_str(),
                                disposition = classification.disposition.as_str(),
                                backoff_ms = backoff.as_millis(),
                                "provider turn failed; retrying"
                            );
                            sleep(backoff).await;
                            last_error = Some(error);
                            continue;
                        }
                        timeline.push(ProviderAttemptRecord {
                            provider: candidate.provider_name.clone(),
                            model_ref: candidate.model_ref.clone(),
                            attempt,
                            max_attempts,
                            failure_kind: Some(classification.kind.as_str().to_string()),
                            disposition: Some(classification.disposition.as_str().to_string()),
                            outcome: match classification.disposition {
                                RetryDisposition::Retryable => {
                                    ProviderAttemptOutcome::RetriesExhausted
                                }
                                RetryDisposition::FailFast => {
                                    ProviderAttemptOutcome::FailFastAborted
                                }
                            },
                            advanced_to_fallback: candidate_index + 1 < self.candidates.len(),
                            backoff_ms: None,
                            token_usage: None,
                            transport_diagnostics: provider_transport_diagnostics(&error).cloned(),
                        });
                        last_error = Some(error);
                        break;
                    }
                }
            }
            if let Some(error) = last_error {
                errors.push(format_provider_failure(
                    &candidate.model_ref,
                    max_attempts,
                    &error,
                ));
            }
        }
        let source = anyhow!(
            "all configured providers failed for this turn: {}",
            errors.join("; ")
        );
        Err(provider_turn_error(
            source.to_string(),
            ProviderAttemptTimeline {
                aggregated_token_usage: aggregate_attempt_token_usage(&timeline),
                attempts: timeline,
                requested_model_ref,
                active_model_ref: None,
                winning_model_ref: None,
            },
            source,
        ))
    }

    #[cfg(test)]
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
        !self.candidates.is_empty()
            && self
                .candidates
                .iter()
                .all(|candidate| candidate.provider.supports_freeform_grammar_tools())
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

    if !request.prompt_frame.system_prompt.trim().is_empty() {
        request.prompt_frame.system_prompt.push_str("\n\n");
    }
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
    if requested_model_ref == active_model_ref || requested_model_ref.is_empty() {
        format!("Runtime: active_model={active_model_ref}")
    } else {
        format!("Runtime: active_model={active_model_ref} requested_model={requested_model_ref}")
    }
}
