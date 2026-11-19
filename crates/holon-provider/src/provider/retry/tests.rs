use reqwest::StatusCode;
use std::time::Duration;

use super::{
    classify_status_error_with_trace, provider_fallback_disposition, provider_retry_delay,
    set_provider_transport_quota_identity, ProviderFailureKind, ProviderRetryDelay,
    ProviderRetryDelaySource, ProviderTransportError,
};
use crate::provider::{ProviderFallbackDisposition, ProviderQuotaIdentity};

#[test]
fn network_failures_defer_fallback_but_other_failures_remain_immediate() {
    assert_eq!(
        provider_fallback_disposition(ProviderFailureKind::Timeout),
        ProviderFallbackDisposition::Deferred
    );
    assert_eq!(
        provider_fallback_disposition(ProviderFailureKind::Connection),
        ProviderFallbackDisposition::Deferred
    );
    assert_eq!(
        provider_fallback_disposition(ProviderFailureKind::ServerError),
        ProviderFallbackDisposition::Immediate
    );
    assert_eq!(
        provider_fallback_disposition(ProviderFailureKind::RateLimited),
        ProviderFallbackDisposition::Deferred
    );
}

fn retry_after_headers(value: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::RETRY_AFTER,
        reqwest::header::HeaderValue::from_str(value).expect("valid header value"),
    );
    headers
}

#[test]
fn parse_retry_after_reads_delta_seconds() {
    assert_eq!(
        super::parse_retry_after(&retry_after_headers("13")).map(|hint| hint.delay),
        Some(Duration::from_secs(13))
    );
}

#[test]
fn parse_retry_after_reads_future_http_date() {
    let date = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc2822();
    let parsed = super::parse_retry_after(&retry_after_headers(&date)).expect("future date parses");
    assert!(parsed.delay > Duration::from_secs(20));
    assert!(parsed.delay <= Duration::from_secs(30));
}

#[test]
fn parse_retry_after_ignores_past_http_date() {
    let date = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc2822();
    assert_eq!(super::parse_retry_after(&retry_after_headers(&date)), None);
}

#[test]
fn parse_retry_after_ignores_missing_malformed_and_zero_values() {
    assert_eq!(
        super::parse_retry_after(&reqwest::header::HeaderMap::new()),
        None
    );
    assert_eq!(super::parse_retry_after(&retry_after_headers("soon")), None);
    assert_eq!(super::parse_retry_after(&retry_after_headers("0")), None);
    assert_eq!(super::parse_retry_after(&retry_after_headers(" ")), None);
}

#[test]
fn parse_retry_after_header_lookup_is_case_insensitive() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::HeaderName::from_static("retry-after"),
        reqwest::header::HeaderValue::from_static("5"),
    );
    assert_eq!(
        super::parse_retry_after(&headers).map(|hint| hint.delay),
        Some(Duration::from_secs(5))
    );
}

fn bigmodel_status_error(
    code: &str,
    model_ref: &str,
    url: &str,
    retry_after: Option<super::ProviderRetryAfter>,
) -> anyhow::Error {
    // Constructed from https://docs.bigmodel.cn/cn/api/api-code, not a captured response.
    let body = serde_json::json!({
        "error": {
            "code": code,
            "message": "quota reset: 2026-11-01 00:00:00"
        }
    });
    classify_status_error_with_trace(
        "BigModel request failed",
        "response_status",
        Some("bigmodel"),
        Some(model_ref),
        Some(url),
        StatusCode::TOO_MANY_REQUESTS,
        body.to_string(),
        None,
        retry_after,
    )
}

#[test]
fn bigmodel_1310_is_hard_quota_for_builtin_endpoint_and_profile() {
    for model_ref in ["bigmodel@default/glm-5.2", "bigmodel/glm-5.2"] {
        let error = bigmodel_status_error(
            "1310",
            model_ref,
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
            None,
        );
        let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::RateLimited
        );
        assert_eq!(
            transport.classification.disposition,
            super::RetryDisposition::Retryable
        );
        let throttle = transport
            .diagnostics
            .as_ref()
            .unwrap()
            .throttle
            .as_ref()
            .unwrap();
        assert_eq!(
            throttle.kind,
            crate::provider::ProviderThrottleKind::HardQuota
        );
        assert_eq!(
            throttle.reason,
            crate::provider::ProviderThrottleReason::BigmodelWeeklyMonthlyQuota
        );
        // The timezone-less reset in the constructed body is deliberately ignored.
        assert_eq!(throttle.not_before, None);
        assert_eq!(throttle.hint_source, None);
    }
}

#[test]
fn hard_quota_decoder_rejects_custom_same_name_endpoints_and_profiles() {
    for (model_ref, url) in [
        (
            "bigmodel@default/glm-5.2",
            "https://custom.example/api/anthropic/v1/messages",
        ),
        (
            "bigmodel@custom/glm-5.2",
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
        ),
        (
            "other@default/glm-5.2",
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
        ),
        (
            "bigmodel@default/glm-5.2",
            "https://open.bigmodel.cn.evil.example/api/anthropic/v1/messages",
        ),
        (
            "bigmodel@default/glm-5.2",
            "http://open.bigmodel.cn/api/anthropic/v1/messages",
        ),
        (
            "bigmodel@default/glm-5.2",
            "https://open.bigmodel.cn:8443/api/anthropic/v1/messages",
        ),
        (
            "bigmodel@default/glm-5.2",
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
        ),
        (
            "bigmodel@default/glm-5.2",
            "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        ),
        (
            "bigmodel@default/glm-5.2",
            "https://user@open.bigmodel.cn/api/anthropic/v1/messages",
        ),
        (
            "bigmodel@default/glm-5.2",
            "https://open.bigmodel.cn/api/anthropic/v1/messages?custom=1",
        ),
    ] {
        let error = bigmodel_status_error("1310", model_ref, url, None);
        let throttle = crate::provider::provider_transport_diagnostics(&error)
            .unwrap()
            .throttle
            .as_ref()
            .unwrap();
        assert_eq!(
            throttle.kind,
            crate::provider::ProviderThrottleKind::Transient,
            "{model_ref} {url}"
        );
    }
}

#[test]
fn bigmodel_1302_and_unknown_business_codes_remain_transient() {
    for code in ["1302", "1308", "1309", "1311", "unknown", "1310 quota"] {
        let error = bigmodel_status_error(
            code,
            "bigmodel@default/glm-5.2",
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
            None,
        );
        let throttle = crate::provider::provider_transport_diagnostics(&error)
            .unwrap()
            .throttle
            .as_ref()
            .unwrap();
        assert_eq!(
            throttle.kind,
            crate::provider::ProviderThrottleKind::Transient,
            "{code}"
        );
        assert_eq!(
            throttle.reason,
            crate::provider::ProviderThrottleReason::RateLimited
        );
    }
}

#[test]
fn hard_quota_decoder_requires_documented_error_envelope_and_provider() {
    for (provider, body) in [
        ("bigmodel", r#"{"code":"1310","message":"quota exhausted"}"#),
        (
            "bigmodel",
            r#"{"error":{"code":1310,"message":"quota exhausted"}}"#,
        ),
        (
            "bigmodel",
            r#"{"error":{"message":"1310 quota exhausted"}}"#,
        ),
        (
            "other",
            r#"{"error":{"code":"1310","message":"quota exhausted"}}"#,
        ),
    ] {
        let error = classify_status_error_with_trace(
            "request failed",
            "response_status",
            Some(provider),
            Some("bigmodel@default/glm-5.2"),
            Some("https://open.bigmodel.cn/api/anthropic/v1/messages"),
            StatusCode::TOO_MANY_REQUESTS,
            body.into(),
            None,
            None,
        );
        assert_eq!(
            crate::provider::provider_transport_diagnostics(&error)
                .unwrap()
                .throttle
                .as_ref()
                .unwrap()
                .kind,
            crate::provider::ProviderThrottleKind::Transient
        );
    }
}

#[test]
fn retry_after_accepts_only_positive_hints_within_35_days() {
    for raw in [
        "0",
        "-1",
        "+1",
        "3024001",
        "18446744073709551615",
        "2026-11-01 00:00:00",
    ] {
        assert_eq!(
            super::parse_retry_after(&retry_after_headers(raw)),
            None,
            "{raw}"
        );
    }
    assert_eq!(
        super::parse_retry_after(&retry_after_headers("3024000"))
            .unwrap()
            .delay,
        Duration::from_secs(super::PROVIDER_RETRY_AFTER_MAX_SECONDS)
    );
}

#[test]
fn long_delta_and_http_date_preserve_observed_utc_deadline_through_serde() {
    let observed_at = chrono::DateTime::parse_from_rfc3339("2026-10-24T17:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let expected = observed_at + chrono::Duration::hours(48);
    for raw in [
        "172800".to_string(),
        expected
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .to_rfc2822(),
    ] {
        let hint = super::parse_retry_after_at(&retry_after_headers(&raw), observed_at).unwrap();
        assert_eq!(hint.observed_at, observed_at);
        assert_eq!(hint.not_before, expected);
        let error = bigmodel_status_error(
            "1310",
            "bigmodel@default/glm-5.2",
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
            Some(hint),
        );
        let diagnostics = crate::provider::provider_transport_diagnostics(&error).unwrap();
        let encoded = serde_json::to_value(diagnostics).unwrap();
        assert_eq!(encoded["retry_after_not_before"], "2026-10-26T17:00:00Z");
        assert_eq!(encoded["throttle"]["not_before"], "2026-10-26T17:00:00Z");
        let decoded: crate::provider::ProviderTransportDiagnostics =
            serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.retry_after_not_before, Some(expected));
        let throttle = decoded.throttle.unwrap();
        assert_eq!(throttle.observed_at, observed_at);
        assert_eq!(throttle.not_before, Some(expected));
        assert_eq!(
            throttle.hint_source,
            Some(crate::provider::ProviderThrottleHintSource::RetryAfter)
        );
    }
    let too_late = observed_at + chrono::Duration::days(35) + chrono::Duration::seconds(1);
    assert_eq!(
        super::parse_retry_after_at(&retry_after_headers(&too_late.to_rfc2822()), observed_at),
        None
    );
}

#[test]
fn server_error_retry_after_deadlines_round_trip_without_quota_throttle() {
    let observed_at = chrono::DateTime::parse_from_rfc3339("2026-10-24T17:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let expected = observed_at + chrono::Duration::hours(1);
    for raw in ["3600", "Sun, 25 Oct 2026 02:00:00 +0800"] {
        let hint = super::parse_retry_after_at(&retry_after_headers(raw), observed_at).unwrap();
        for status in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            let error = classify_status_error_with_trace(
                "request failed",
                "response_status",
                Some("openai"),
                Some("openai/gpt-test"),
                Some("https://example.test/v1/responses"),
                status,
                "temporarily unavailable".into(),
                None,
                Some(hint),
            );
            let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
            assert_eq!(
                transport.classification.kind,
                ProviderFailureKind::ServerError
            );
            assert_eq!(
                transport.classification.disposition,
                super::RetryDisposition::Retryable
            );
            assert_eq!(transport.retry_after, Some(Duration::from_secs(3600)));
            let diagnostics = transport.diagnostics.as_ref().unwrap();
            assert_eq!(diagnostics.retry_after_not_before, Some(expected));
            assert_eq!(diagnostics.throttle, None);
            let encoded = serde_json::to_value(diagnostics).unwrap();
            assert_eq!(encoded["retry_after_not_before"], "2026-10-24T18:00:00Z");
            assert!(encoded.get("throttle").is_none());
            let decoded: crate::provider::ProviderTransportDiagnostics =
                serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, *diagnostics);
        }
    }
}

#[test]
fn missing_or_invalid_retry_after_hints_do_not_create_server_deadlines() {
    let observed_at = chrono::DateTime::parse_from_rfc3339("2026-10-24T17:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for raw in [
        None,
        Some(""),
        Some(" "),
        Some("soon"),
        Some("0"),
        Some("-1"),
        Some("+1"),
        Some("3024001"),
        Some("18446744073709551615"),
        Some("2026-10-24 18:00:00"),
        Some("Sat, 24 Oct 2026 16:59:59 GMT"),
        Some("Sun, 29 Nov 2026 17:00:00 GMT"),
    ] {
        let headers = raw.map(retry_after_headers).unwrap_or_default();
        let hint = super::parse_retry_after_at(&headers, observed_at);
        assert_eq!(hint, None, "{raw:?}");
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let error = classify_status_error_with_trace(
                "request failed",
                "response_status",
                Some("openai"),
                None,
                None,
                status,
                "quota resets at 2026-10-24T18:00:00Z; Retry-After: 3600".into(),
                None,
                hint,
            );
            let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
            assert_eq!(transport.retry_after, None);
            let diagnostics = transport.diagnostics.as_ref().unwrap();
            assert_eq!(diagnostics.retry_after_not_before, None);
            assert!(serde_json::to_value(diagnostics)
                .unwrap()
                .get("retry_after_not_before")
                .is_none());
            if let Some(throttle) = diagnostics.throttle.as_ref() {
                assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(
                    throttle.kind,
                    crate::provider::ProviderThrottleKind::Transient
                );
                assert_eq!(throttle.not_before, None);
                assert_eq!(throttle.hint_source, None);
            } else {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }
}

#[test]
fn old_transport_diagnostics_deserialize_without_retry_metadata() {
    let decoded: crate::provider::ProviderTransportDiagnostics =
        serde_json::from_value(serde_json::json!({"stage": "response_status"})).unwrap();
    assert_eq!(decoded.throttle, None);
    assert_eq!(decoded.retry_after_not_before, None);
    let encoded = serde_json::to_value(decoded).unwrap();
    assert!(encoded.get("throttle").is_none());
    assert!(encoded.get("retry_after_not_before").is_none());
}

#[test]
fn retry_delay_uses_server_hint_within_cap() {
    assert_eq!(
        provider_retry_delay(
            1,
            ProviderFailureKind::RateLimited,
            Some(Duration::from_secs(5)),
            0
        ),
        ProviderRetryDelay::Wait {
            backoff: Duration::from_secs(5),
            source: ProviderRetryDelaySource::ServerRetryAfter
        }
    );
}

#[test]
fn retry_delay_keeps_computed_floor_when_hint_is_smaller() {
    assert_eq!(
        provider_retry_delay(
            2,
            ProviderFailureKind::RateLimited,
            Some(Duration::from_millis(50)),
            0
        ),
        ProviderRetryDelay::Wait {
            backoff: Duration::from_millis(400),
            source: ProviderRetryDelaySource::ServerRetryAfter
        }
    );
}

#[test]
fn retry_delay_skips_rate_limit_hint_over_request_wait_cap() {
    assert_eq!(
        provider_retry_delay(
            1,
            ProviderFailureKind::RateLimited,
            Some(Duration::from_secs(45)),
            0
        ),
        ProviderRetryDelay::SkipToFallback
    );
}

#[test]
fn retry_delay_without_hint_uses_computed_backoff() {
    assert_eq!(
        provider_retry_delay(1, ProviderFailureKind::RateLimited, None, 0),
        ProviderRetryDelay::Wait {
            backoff: Duration::from_millis(200),
            source: ProviderRetryDelaySource::ComputedBackoff
        }
    );
}

#[test]
fn server_error_without_hint_uses_stable_exponential_backoff_with_jitter() {
    let seed = super::provider_retry_jitter_seed("openai", "gpt-5.4");
    let first = provider_retry_delay(1, ProviderFailureKind::ServerError, None, seed);
    let second = provider_retry_delay(2, ProviderFailureKind::ServerError, None, seed);
    let backoff = |delay| match delay {
        ProviderRetryDelay::Wait { backoff, .. } => backoff,
        ProviderRetryDelay::SkipToFallback => panic!("expected retry wait"),
    };

    assert_eq!(
        first,
        provider_retry_delay(1, ProviderFailureKind::ServerError, None, seed)
    );
    assert_eq!(
        first,
        ProviderRetryDelay::Wait {
            backoff: backoff(first),
            source: ProviderRetryDelaySource::ServerErrorExponentialBackoff
        }
    );
    assert_eq!(
        second,
        ProviderRetryDelay::Wait {
            backoff: backoff(second),
            source: ProviderRetryDelaySource::ServerErrorExponentialBackoff
        }
    );
    assert!((Duration::from_secs(2)..=Duration::from_millis(2_500)).contains(&backoff(first)));
    assert!((Duration::from_secs(4)..=Duration::from_millis(5_000)).contains(&backoff(second)));
}

#[test]
fn server_error_hint_still_extends_computed_backoff() {
    let seed = super::provider_retry_jitter_seed("openai", "gpt-5.4");
    assert_eq!(
        provider_retry_delay(
            1,
            ProviderFailureKind::ServerError,
            Some(Duration::from_secs(5)),
            seed
        ),
        ProviderRetryDelay::Wait {
            backoff: Duration::from_secs(5),
            source: ProviderRetryDelaySource::ServerRetryAfter
        }
    );
}

#[test]
fn server_error_backoff_is_capped_at_retry_hint_limit() {
    let delay = provider_retry_delay(
        usize::MAX,
        ProviderFailureKind::ServerError,
        None,
        super::provider_retry_jitter_seed("openai", "gpt-5.4"),
    );

    assert_eq!(
        delay,
        ProviderRetryDelay::Wait {
            backoff: Duration::from_millis(super::PROVIDER_RETRY_SERVER_HINT_CAP_MS),
            source: ProviderRetryDelaySource::ServerErrorExponentialBackoff
        }
    );
}

#[test]
fn retry_delay_ignores_hint_for_non_throttle_kinds() {
    assert_eq!(
        provider_retry_delay(
            1,
            ProviderFailureKind::Timeout,
            Some(Duration::from_secs(5)),
            0
        ),
        ProviderRetryDelay::Wait {
            backoff: Duration::from_millis(200),
            source: ProviderRetryDelaySource::ComputedBackoff
        }
    );
}

#[test]
fn payment_required_is_typed_fail_fast_even_with_deterministic_body() {
    for body in [
        r#"{"error":{"code":"personal-team-blocked:spending-limit","message":"You have reached your spending limit"}}"#,
        r#"{"error":{"code":"context_length_exceeded","message":"context length exceeded"}}"#,
    ] {
        let error = classify_status_error_with_trace(
            "request failed",
            "response_status",
            Some("xai"),
            Some("xai@default/grok-4.7"),
            Some("https://api.x.ai/v1/responses"),
            StatusCode::PAYMENT_REQUIRED,
            body.into(),
            None,
            super::ProviderRetryAfter::from_delay(Duration::from_secs(1800)),
        );
        let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::PaymentRequired
        );
        assert_eq!(
            transport.classification.disposition,
            super::RetryDisposition::FailFast
        );
        assert_eq!(transport.retry_after, Some(Duration::from_secs(1800)));
        assert!(transport.diagnostics.as_ref().unwrap().throttle.is_none());
        assert_eq!(
            provider_fallback_disposition(transport.classification.kind),
            super::ProviderFallbackDisposition::Deferred
        );
    }
}

#[test]
fn ordinary_client_errors_are_not_payment_failures() {
    for status in [400, 401, 403, 404, 422] {
        let error = classify_status_error_with_trace(
            "request failed",
            "response_status",
            Some("xai"),
            Some("xai@default/grok-4.7"),
            None,
            StatusCode::from_u16(status).unwrap(),
            r#"{"error":{"message":"request rejected"}}"#.into(),
            None,
            None,
        );
        let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
        assert_eq!(
            transport.classification.kind,
            if matches!(status, 401 | 403) {
                ProviderFailureKind::AuthError
            } else {
                ProviderFailureKind::ContractError
            }
        );
    }
}

#[test]
fn status_error_carries_retry_after_hint() {
    let error = classify_status_error_with_trace(
        "OpenAI request failed",
        "response_status",
        Some("openai-codex"),
        Some("openai-codex/gpt-5.3-codex-spark"),
        Some("https://chatgpt.com/backend-api/codex/responses"),
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"message":"rate limited"}}"#.into(),
        None,
        super::ProviderRetryAfter::from_delay(Duration::from_secs(5)),
    );
    let transport = error
        .downcast_ref::<ProviderTransportError>()
        .expect("transport error");
    assert_eq!(
        transport.classification.kind,
        ProviderFailureKind::RateLimited
    );
    assert_eq!(transport.retry_after, Some(Duration::from_secs(5)));
}

#[test]
fn explicit_tool_protocol_rejection_is_fail_fast_with_configuration_guidance() {
    let error = classify_status_error_with_trace(
        "OpenAI request failed",
        "response_status",
        Some("openai"),
        Some("openai/gpt-5"),
        Some("https://example.test/v1/responses"),
        StatusCode::BAD_REQUEST,
        r#"{"error":{"code":"unsupported_tool","message":"tool protocol rejected"}}"#.into(),
        None,
        None,
    );
    let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
    assert_eq!(
        transport.classification.kind,
        ProviderFailureKind::ContractError
    );
    assert_eq!(
        transport.classification.disposition,
        super::RetryDisposition::FailFast
    );
    assert!(error
        .to_string()
        .contains("builtin_web_search configuration"));
}

#[test]
fn ordinary_bad_request_has_no_tool_configuration_guidance() {
    let error = classify_status_error_with_trace(
        "OpenAI request failed",
        "response_status",
        Some("openai"),
        Some("openai/gpt-5"),
        Some("https://example.test/v1/responses"),
        StatusCode::BAD_REQUEST,
        r#"{"error":{"code":"invalid_request","message":"bad input"}}"#.into(),
        None,
        None,
    );
    assert!(!error
        .to_string()
        .contains("builtin_web_search configuration"));
}

#[test]
fn rate_limit_diagnostics_carry_redacted_quota_identity_and_round_trip() {
    let identity = ProviderQuotaIdentity::exact("codex-account", "account-secret")
        .expect("non-empty account should produce an identity");
    let error = set_provider_transport_quota_identity(
        classify_status_error_with_trace(
            "OpenAI request failed",
            "response_status",
            Some("openai-codex"),
            Some("openai-codex/gpt-5.3-codex-spark"),
            Some("https://chatgpt.com/backend-api/codex/responses"),
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"rate limited"}}"#.into(),
            None,
            super::ProviderRetryAfter::from_delay(Duration::from_secs(5)),
        ),
        identity.clone(),
    );
    let transport = error
        .downcast_ref::<ProviderTransportError>()
        .expect("transport error");
    assert_eq!(
        transport.status,
        Some(StatusCode::TOO_MANY_REQUESTS.as_u16())
    );
    assert_eq!(transport.retry_after, Some(Duration::from_secs(5)));

    let diagnostics = transport
        .diagnostics
        .as_ref()
        .expect("rate limit should include transport diagnostics");
    assert_eq!(diagnostics.provider.as_deref(), Some("openai-codex"));
    assert_eq!(
        diagnostics.status,
        Some(StatusCode::TOO_MANY_REQUESTS.as_u16())
    );
    assert_eq!(diagnostics.quota_identity.as_ref(), Some(&identity));

    let encoded = serde_json::to_value(diagnostics).expect("diagnostics should serialize");
    assert_eq!(
        encoded["quota_identity"],
        serde_json::to_value(&identity).expect("identity should serialize")
    );
    assert!(!encoded.to_string().contains("account-secret"));

    let decoded: crate::provider::ProviderTransportDiagnostics =
        serde_json::from_value(encoded).expect("diagnostics should deserialize");
    assert_eq!(decoded, *diagnostics);
}

#[test]
fn rate_limit_status_wins_over_deterministic_marker() {
    let error = classify_status_error_with_trace(
        "Provider request failed",
        "response_status",
        Some("openai"),
        Some("openai/gpt-5.4"),
        Some("https://example.com/v1/chat/completions"),
        StatusCode::TOO_MANY_REQUESTS,
        "too many tokens per minute".into(),
        None,
        super::ProviderRetryAfter::from_delay(Duration::from_secs(5)),
    );
    let transport = error
        .downcast_ref::<ProviderTransportError>()
        .expect("transport error");
    assert_eq!(
        transport.classification.kind,
        ProviderFailureKind::RateLimited
    );
    assert_eq!(
        transport.classification.disposition,
        super::RetryDisposition::Retryable
    );
    assert_eq!(transport.retry_after, Some(Duration::from_secs(5)));
}

#[test]
fn deterministic_context_overflow_on_server_error_is_fail_fast() {
    let error = classify_status_error_with_trace(
        "Ollama request failed",
        "response_status",
        Some("ollama"),
        Some("ollama/qwen3.8:latest"),
        Some("http://localhost:11434/v1/chat/completions"),
        StatusCode::INTERNAL_SERVER_ERROR,
        "context length exceeded: prompt is too long".into(),
        None,
        None,
    );
    let transport = error
        .downcast_ref::<ProviderTransportError>()
        .expect("transport error");
    assert_eq!(
        transport.classification.kind,
        ProviderFailureKind::ContractError
    );
    assert_eq!(
        transport.classification.disposition,
        super::RetryDisposition::FailFast
    );
}

#[test]
fn ollama_missing_user_query_on_server_error_is_fail_fast() {
    let error = classify_status_error_with_trace(
        "Ollama request failed",
        "response_status",
        Some("ollama"),
        Some("ollama/qwen3.8:latest"),
        Some("http://localhost:11434/v1/chat/completions"),
        StatusCode::INTERNAL_SERVER_ERROR,
        "no user query found in messages".into(),
        None,
        None,
    );
    let transport = error
        .downcast_ref::<ProviderTransportError>()
        .expect("transport error");
    assert_eq!(
        transport.classification.kind,
        ProviderFailureKind::ContractError
    );
    assert_eq!(
        transport.classification.disposition,
        super::RetryDisposition::FailFast
    );
}

#[test]
fn generic_server_error_remains_retryable() {
    let error = classify_status_error_with_trace(
        "Provider request failed",
        "response_status",
        Some("ollama"),
        Some("ollama/qwen3.8:latest"),
        Some("http://localhost:11434/v1/chat/completions"),
        StatusCode::INTERNAL_SERVER_ERROR,
        "temporary upstream failure".into(),
        None,
        None,
    );
    let transport = error
        .downcast_ref::<ProviderTransportError>()
        .expect("transport error");
    assert_eq!(
        transport.classification.kind,
        ProviderFailureKind::ServerError
    );
    assert_eq!(
        transport.classification.disposition,
        super::RetryDisposition::Retryable
    );
}

#[test]
fn transport_url_sanitizer_removes_credentials_query_and_fragment() {
    assert_eq!(
        super::sanitize_transport_url(
            "https://user:secret@example.com/v1/responses?api_key=token#frag"
        ),
        "https://example.com/v1/responses"
    );
}

#[test]
fn status_error_display_preserves_safe_upstream_detail_without_secrets() {
    let error = classify_status_error_with_trace(
            "OpenAI compact request failed",
            "response_status",
            Some("openai"),
            Some("openai/gpt-5.4"),
            Some("https://api.openai.com/v1/responses/compact"),
            StatusCode::NOT_FOUND,
            r#"{"error":{"message":"Items are not persisted when `store` is set to false","access_token":"short-secret"}}"#.into(),
            None,
            None,
        );

    assert_eq!(
            error.to_string(),
            "OpenAI compact request failed with status 404 Not Found: message=Items are not persisted when `store` is set to false"
        );
    assert!(!error.to_string().contains("short-secret"));
    assert_eq!(
        error
            .downcast_ref::<ProviderTransportError>()
            .and_then(|error| error.code.as_deref()),
        Some("non_persisted_item_id")
    );
}

#[test]
fn gateway_entitlement_error_preserves_structured_type_and_message() {
    let error = classify_status_error_with_trace(
            "Vercel AI Gateway request failed",
            "response_status",
            Some("openai"),
            Some("openai/gpt-5.4"),
            Some("https://gateway.example/v1/chat/completions"),
            StatusCode::FORBIDDEN,
            r#"{"error":{"type":"no_providers_available","message":"Free tier users do not have access to this model","token":"must-not-leak"}}"#.into(),
            None,
            None,
        );

    assert_eq!(
            error.to_string(),
            "Vercel AI Gateway request failed with status 403 Forbidden: type=no_providers_available, message=Free tier users do not have access to this model"
        );
    assert!(!error.to_string().contains("must-not-leak"));
}

#[test]
fn timeout_transport_summary_does_not_present_decode_wording_as_root_cause() {
    assert_eq!(
            super::format_reqwest_transport_error_message(
                "Anthropic streaming response body failed",
                "streaming_response_body",
                true,
                "error decoding response body".into(),
            ),
            "Anthropic streaming response body failed: timed out while reading the streaming response body"
        );
    assert_eq!(
        super::format_reqwest_transport_error_message(
            "Anthropic streaming response body failed",
            "streaming_response_parse",
            true,
            "error decoding response body".into(),
        ),
        "Anthropic streaming response body failed: error decoding response body"
    );
}

#[test]
fn upstream_error_detail_requires_error_object_when_envelope_is_present() {
    let value = serde_json::json!({
        "error": "not an object",
        "message": "should not be used"
    });
    assert_eq!(
        super::extract_upstream_error_detail_from_value(&value),
        None
    );
}

#[test]
fn streaming_request_send_connection_source_chain_is_retryable() {
    let source_chain = vec![
        "client error (SendRequest)".to_string(),
        "connection error".to_string(),
        "peer closed connection without sending TLS close_notify".to_string(),
    ];

    assert!(super::is_retryable_request_send_transport_failure(
        "streaming_request_send",
        &source_chain
    ));
}

#[test]
fn request_send_connection_closed_source_chain_is_retryable() {
    let source_chain = vec![
        "client error (SendRequest)".to_string(),
        "connection closed before message completed".to_string(),
    ];

    assert!(super::is_retryable_request_send_transport_failure(
        "request_send",
        &source_chain
    ));
}

#[test]
fn request_send_connection_source_chain_is_stage_limited() {
    let source_chain = vec!["connection error".to_string()];

    assert!(!super::is_retryable_request_send_transport_failure(
        "response_status",
        &source_chain
    ));
}

#[test]
fn request_send_non_transport_source_chain_is_not_retryable() {
    let source_chain = vec![
        "builder error".to_string(),
        "invalid header value".to_string(),
    ];

    assert!(!super::is_retryable_request_send_transport_failure(
        "request_send",
        &source_chain
    ));
}
