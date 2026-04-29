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
    AgentProvider, ProviderAttemptOutcome, ProviderAttemptRecord, ProviderAttemptTimeline,
    ProviderContextManagementPolicy, ProviderTurnRequest, ProviderTurnResponse,
};

#[derive(Clone)]
pub(super) struct FallbackProvider {
    pub(crate) candidates: Vec<ProviderCandidate>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;
    use async_trait::async_trait;

    use super::*;
    use crate::provider::{ModelBlock, ProviderCacheUsage};

    #[derive(Clone)]
    struct PolicyProvider {
        policy: Option<ProviderContextManagementPolicy>,
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
        let mut errors = Vec::new();
        let mut timeline = Vec::new();
        for (candidate_index, candidate) in self.candidates.iter().enumerate() {
            let max_attempts = provider_max_attempts();
            let mut last_error = None;
            for attempt in 1..=max_attempts {
                match candidate.provider.complete_turn(request.clone()).await {
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
