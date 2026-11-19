use super::classify_chat_completion_status_error;
use crate::provider::retry::{ProviderFailureKind, ProviderRetryAfter, ProviderTransportError};
use crate::provider::{ProviderThrottleKind, ProviderTransportDiagnostics};

#[test]
fn chat_status_errors_preserve_absolute_retry_after_without_turning_5xx_into_quota() {
    let observed_at = chrono::DateTime::parse_from_rfc3339("2026-10-24T17:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let not_before = observed_at + chrono::Duration::hours(1);
    let hint = ProviderRetryAfter {
        observed_at,
        not_before,
        delay: std::time::Duration::from_secs(3600),
    };
    for (status, body, kind) in [
        (
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":{"type":"server_error","message":"temporarily unavailable"}}"#,
            ProviderFailureKind::ServerError,
        ),
        (
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "temporarily unavailable",
            ProviderFailureKind::ServerError,
        ),
        (
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_error","message":"rate limited"}}"#,
            ProviderFailureKind::RateLimited,
        ),
        (
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "rate limited",
            ProviderFailureKind::RateLimited,
        ),
    ] {
        for streaming in [false, true] {
            let error = classify_chat_completion_status_error(
                "request failed",
                status,
                body.into(),
                Some("openai/gpt-test"),
                Some("https://example.test/v1/chat/completions"),
                None,
                Some(hint),
                streaming,
            );
            let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
            assert_eq!(transport.classification.kind, kind);
            let diagnostics = transport.diagnostics.as_ref().unwrap();
            assert_eq!(diagnostics.retry_after_not_before, Some(not_before));
            if let Some(throttle) = diagnostics.throttle.as_ref() {
                assert_eq!(kind, ProviderFailureKind::RateLimited);
                assert_eq!(throttle.kind, ProviderThrottleKind::Transient);
                assert_eq!(throttle.not_before, Some(not_before));
            } else {
                assert_eq!(kind, ProviderFailureKind::ServerError);
            }
            let encoded = serde_json::to_value(diagnostics).unwrap();
            assert_eq!(encoded["retry_after_not_before"], "2026-10-24T18:00:00Z");
            let decoded: ProviderTransportDiagnostics = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, *diagnostics);
        }
    }
}
