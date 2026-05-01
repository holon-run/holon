use super::*;
use crate::provider::{sanitize_diagnostic_url, ProviderAttemptTimeline};
use crate::types::{FailureArtifact, FailureArtifactCategory};

impl RuntimeHandle {
    fn metadata_enum_value<T: serde::Serialize>(value: &T) -> String {
        let json = to_json_value(value);
        json.as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| json.to_string())
    }

    fn provider_failure_category(failure_kind: &str) -> FailureArtifactCategory {
        match failure_kind {
            "timeout" | "connection" | "rate_limited" | "server_error" => {
                FailureArtifactCategory::Transport
            }
            "auth_error" | "contract_error" | "invalid_response" | "unsupported_transport" => {
                FailureArtifactCategory::Protocol
            }
            _ => FailureArtifactCategory::Unknown,
        }
    }

    fn failure_artifact_for_provider_timeline(
        error_summary: &str,
        attempt_timeline: &ProviderAttemptTimeline,
    ) -> FailureArtifact {
        let latest_attempt = attempt_timeline.attempts.iter().rev().find(|attempt| {
            attempt.failure_kind.is_some() || attempt.transport_diagnostics.is_some()
        });

        let mut kind = latest_attempt
            .and_then(|attempt| attempt.failure_kind.clone())
            .unwrap_or_else(|| "unknown".into());
        kind.retain(|c: char| !c.is_whitespace());

        let mut metadata = std::collections::BTreeMap::new();
        if let Some(winning_model_ref) = attempt_timeline.winning_model_ref.clone() {
            metadata.insert("winning_model_ref".into(), winning_model_ref);
        }
        if let Some(provider) = latest_attempt.map(|attempt| attempt.provider.clone()) {
            metadata.insert("provider".into(), provider);
        }
        if let Some(model_ref) = latest_attempt.map(|attempt| attempt.model_ref.clone()) {
            metadata.insert("model_ref".into(), model_ref);
        }
        if let Some(diag) =
            latest_attempt.and_then(|attempt| attempt.transport_diagnostics.as_ref())
        {
            if let Some(status) = diag.status {
                metadata.insert("status".into(), status.to_string());
            }
            metadata.insert("transport_stage".into(), diag.stage.clone());
            if let Some(url) = diag.url.clone() {
                metadata.insert("url".into(), sanitize_diagnostic_url(&url));
            }
            metadata.insert(
                "reqwest_is_timeout".into(),
                diag.reqwest
                    .as_ref()
                    .map(|req| req.is_timeout)
                    .unwrap_or(false)
                    .to_string(),
            );
            metadata.insert(
                "reqwest_is_connect".into(),
                diag.reqwest
                    .as_ref()
                    .map(|req| req.is_connect)
                    .unwrap_or(false)
                    .to_string(),
            );
            metadata.insert(
                "reqwest_is_request".into(),
                diag.reqwest
                    .as_ref()
                    .map(|req| req.is_request)
                    .unwrap_or(false)
                    .to_string(),
            );
            metadata.insert(
                "reqwest_is_body".into(),
                diag.reqwest
                    .as_ref()
                    .map(|req| req.is_body)
                    .unwrap_or(false)
                    .to_string(),
            );
            metadata.insert(
                "reqwest_is_decode".into(),
                diag.reqwest
                    .as_ref()
                    .map(|req| req.is_decode)
                    .unwrap_or(false)
                    .to_string(),
            );
            metadata.insert(
                "reqwest_is_redirect".into(),
                diag.reqwest
                    .as_ref()
                    .map(|req| req.is_redirect)
                    .unwrap_or(false)
                    .to_string(),
            );
        }

        let source_chain = latest_attempt
            .and_then(|attempt| attempt.transport_diagnostics.as_ref())
            .map(|diag| diag.source_chain.clone())
            .unwrap_or_default();

        FailureArtifact {
            category: Self::provider_failure_category(&kind),
            kind,
            summary: error_summary.to_string(),
            provider: latest_attempt
                .map(|attempt| attempt.provider.clone())
                .or_else(|| metadata.get("provider").cloned()),
            model_ref: latest_attempt
                .map(|attempt| attempt.model_ref.clone())
                .or_else(|| metadata.get("model_ref").cloned()),
            status: latest_attempt
                .and_then(|attempt| attempt.transport_diagnostics.as_ref())
                .and_then(|diag| diag.status),
            task_id: None,
            exit_status: None,
            source_chain,
            metadata,
        }
    }

    fn failure_artifact_for_runtime_error(
        message: &MessageEnvelope,
        failure_text: &str,
    ) -> FailureArtifact {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert(
            "message_kind".into(),
            Self::metadata_enum_value(&message.kind),
        );
        metadata.insert("message_id".into(), message.id.clone());
        metadata.insert(
            "delivery_surface".into(),
            Self::metadata_enum_value(&message.delivery_surface),
        );
        metadata.insert(
            "admission_context".into(),
            Self::metadata_enum_value(&message.admission_context),
        );
        metadata.insert(
            "authority_class".into(),
            Self::metadata_enum_value(&message.authority_class),
        );
        FailureArtifact {
            category: FailureArtifactCategory::Runtime,
            kind: "runtime_error".into(),
            summary: failure_text.to_string(),
            provider: None,
            model_ref: None,
            status: None,
            task_id: None,
            exit_status: None,
            source_chain: Vec::new(),
            metadata,
        }
    }

    fn failure_artifact(
        message: &MessageEnvelope,
        failure_text: &str,
        error: &anyhow::Error,
    ) -> FailureArtifact {
        if let Some(timeline) = provider_attempt_timeline(error) {
            if !timeline.attempts.is_empty() {
                return Self::failure_artifact_for_provider_timeline(failure_text, timeline);
            }
        }

        Self::failure_artifact_for_runtime_error(message, failure_text)
    }

    pub(super) fn summarize_runtime_failure_error(error: &anyhow::Error) -> String {
        const MAX_ERROR_SUMMARY_LEN: usize = 200;

        let raw = error.to_string();
        let first_line = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("unknown error");

        let mut summary = String::new();
        let mut char_count = 0usize;
        let mut truncated = false;

        for segment in first_line.split_whitespace() {
            let segment_len = segment.chars().count();
            let separator_len = usize::from(!summary.is_empty());
            if char_count + separator_len + segment_len > MAX_ERROR_SUMMARY_LEN {
                truncated = true;
                if summary.is_empty() {
                    let prefix_limit = MAX_ERROR_SUMMARY_LEN.saturating_sub(1);
                    let prefix = segment.chars().take(prefix_limit).collect::<String>();
                    if !prefix.is_empty() {
                        summary.push_str(&prefix);
                    }
                }
                break;
            }
            if !summary.is_empty() {
                summary.push(' ');
                char_count += 1;
            }
            summary.push_str(segment);
            char_count += segment_len;
        }

        if truncated {
            while summary.chars().count() >= MAX_ERROR_SUMMARY_LEN {
                summary.pop();
            }
            summary.push('…');
        }

        summary
    }

    pub(super) fn concise_runtime_failure_text(
        message: &MessageEnvelope,
        error: &anyhow::Error,
    ) -> String {
        let message_kind = to_json_value(&message.kind)
            .as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| "message".to_string());
        let error_summary = Self::summarize_runtime_failure_error(error);
        format!(
            "Turn failed while processing {}: {}",
            message_kind, error_summary
        )
    }

    pub(super) async fn persist_runtime_failure_artifacts(
        &self,
        message: &MessageEnvelope,
        error: &anyhow::Error,
    ) -> Result<()> {
        let failure_text = Self::concise_runtime_failure_text(message, error);
        let attempt_timeline = provider_attempt_timeline(error);
        let failure_artifact = Self::failure_artifact(message, &failure_text, error);
        let token_usage = attempt_timeline
            .as_ref()
            .and_then(|timeline| timeline.aggregated_token_usage.clone());
        let error_chain = error
            .chain()
            .skip(1)
            .map(ToString::to_string)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let brief = brief::make_failure(&message.agent_id, message, failure_text.clone());
        self.persist_brief(&brief).await?;
        self.inner
            .storage
            .append_transcript_entry(&TranscriptEntry::new(
                message.agent_id.clone(),
                TranscriptEntryKind::RuntimeFailure,
                None,
                Some(message.id.clone()),
                serde_json::json!({
                    "kind": message.kind,
                    "origin": message.origin,
                    "trust": message.trust,
                    "authority_class": message.authority_class,
                    "delivery_surface": message.delivery_surface,
                    "admission_context": message.admission_context,
                    "error": error.to_string(),
                    "error_chain": error_chain,
                    "text": failure_text,
                    "failure_artifact": failure_artifact.clone(),
                    "token_usage": token_usage,
                    "provider_attempt_timeline": attempt_timeline,
                }),
            ))?;
        {
            let mut guard = self.inner.agent.lock().await;
            guard.state.last_runtime_failure = Some(RuntimeFailureSummary {
                occurred_at: Utc::now(),
                summary: failure_text,
                phase: RuntimeFailurePhase::RuntimeTurn,
                detail_hint: Some("run `holon daemon logs` for details".into()),
                failure_artifact: Some(failure_artifact),
            });
            self.inner.storage.write_agent(&guard.state)?;
        }
        Ok(())
    }
}
