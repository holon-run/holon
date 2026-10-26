use super::super::*;
use super::parse::parse_chat_completion_response;
#[cfg(test)]
use super::parse::{
    accumulate_chat_completion_stream_events, process_chat_completion_sse_event,
    ChatCompletionSseEvent,
};

#[cfg(test)]
pub(crate) async fn send_chat_completion_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
    dispatch_guard: Option<&dyn ProviderDispatchGuard>,
) -> Result<ParsedOpenAiResponse> {
    send_chat_completion_request_with_material(
        client,
        url,
        body,
        headers,
        trace,
        agent_id,
        dispatch_guard,
        &Default::default(),
    )
    .await
}

pub(crate) async fn send_chat_completion_request_with_material(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
    dispatch_guard: Option<&dyn ProviderDispatchGuard>,
    material: &crate::provider::ProviderRequestMaterial,
) -> Result<ParsedOpenAiResponse> {
    let result: Result<ParsedOpenAiResponse> = async {
        let model_ref = provider_model_ref("openai", &body);
        let request_trace = trace.map(|trace| {
            trace.begin_request_with_material(
                agent_id,
                "openai",
                Some(&model_ref),
                url.as_str(),
                "chat_completions",
                &headers,
                &body,
                material,
            )
        }).transpose()?.flatten();
        let mut request = client.post(&url).header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(name, value);
        }

        let request_started_at = chrono::Utc::now();
        let response = send_openai_request(
            request.body(material.json_bytes(&body)?),
            "OpenAI Chat Completions request failed",
            "request_send",
            "openai",
            Some(&model_ref),
            Some(url.as_str()),
            RequestDeadline::FullRequest,
            request_trace.as_ref(),
            dispatch_guard,
        )
        .await?;
        let response_headers_at = chrono::Utc::now();
        trace_response_headers(
            request_trace.as_ref(),
            response.status(),
            response.headers(),
        );
        let provider_request_id = provider_request_id_from_headers(response.headers());

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = parse_retry_after(response.headers());
            let body = read_error_response_body(response, request_trace.as_ref(), material).await?;
            return Err(classify_chat_completion_status_error(
                "OpenAI Chat Completions request failed",
                status,
                body,
                Some(&model_ref),
                Some(url.as_str()),
                request_trace.as_ref(),
                retry_after,
                false,
            ));
        }

        let body = match tokio::time::timeout(response_body_timeout(), material.response_text(response)).await {
            Ok(Ok(text)) => text,
            Ok(Err(error)) => {
                let error = match error.downcast::<reqwest::Error>() {
                    Ok(error) => error,
                    Err(error) => return Err(material.hold_error(error)),
                };
                return Err(classify_reqwest_transport_error_with_trace(
                    "OpenAI Chat Completions response body failed",
                    "response_body",
                    "openai",
                    Some(&model_ref),
                    Some(url.as_str()),
                    error,
                    request_trace.as_ref(),
                ));
            }
            Err(_elapsed) => {
                return Err(timeout_transport_error_with_trace(
                    "OpenAI Chat Completions response body read timed out",
                    "response_body",
                    "openai",
                    Some(&model_ref),
                    Some(url.as_str()),
                    format!("timed out after {:?}", response_body_timeout()),
                    request_trace.as_ref(),
                ));
            }
        };
        let response_body_completed_at = chrono::Utc::now();
        trace_response_body(request_trace.as_ref(), &body);

        material.read(|| crate::diagnostics::resource_io::admit_json_payload(&body))?;
        let parsed: Value = serde_json::from_str(&body).map_err(|error| {
            invalid_response_error("invalid OpenAI Chat Completions JSON", error)
        })?;

        let parsed = parse_chat_completion_response(parsed)?;
        let parse_completed_at = chrono::Utc::now();
        Ok(parsed
            .with_provider_request_id(provider_request_id)
            .with_transport_timeline(ProviderTransportTimeline {
                request_started_at,
                response_headers_at,
                response_body_completed_at: Some(response_body_completed_at),
                parse_completed_at,
                streaming: false,
            }))
    }
    .await;
    if let Some(guard) = dispatch_guard {
        guard.after_request(result.as_ref().map(|parsed| Some(&parsed.response)))?;
    }
    result
}

fn classify_chat_completion_status_error(
    context: &str,
    status: reqwest::StatusCode,
    body: String,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
    retry_after: Option<crate::provider::retry::ProviderRetryAfter>,
    streaming: bool,
) -> anyhow::Error {
    // Try to parse as OpenAI error response
    if let Ok(error_json) = serde_json::from_str::<Value>(&body) {
        if let Some(error_obj) = error_json.get("error") {
            return classify_openai_chat_completion_error_with_streaming(
                context,
                error_obj,
                status,
                model_ref,
                url,
                trace,
                retry_after,
                streaming,
            );
        }
    }

    // Fallback to generic status error classification
    classify_status_error_with_trace(
        context,
        "response_status",
        Some("openai"),
        model_ref,
        url,
        status,
        body,
        trace,
        retry_after,
    )
}

#[cfg(test)]
pub(crate) fn classify_openai_chat_completion_error(
    context: &str,
    error: &Value,
    status: reqwest::StatusCode,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
    retry_after: Option<std::time::Duration>,
) -> anyhow::Error {
    classify_openai_chat_completion_error_with_streaming(
        context,
        error,
        status,
        model_ref,
        url,
        trace,
        retry_after.and_then(crate::provider::retry::ProviderRetryAfter::from_delay),
        false,
    )
}

fn classify_openai_chat_completion_error_with_streaming(
    context: &str,
    error: &Value,
    status: reqwest::StatusCode,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
    retry_after: Option<crate::provider::retry::ProviderRetryAfter>,
    streaming: bool,
) -> anyhow::Error {
    let detail = extract_upstream_error_detail_from_value(error);
    let error_type = detail
        .as_ref()
        .and_then(|detail| detail.error_type.as_deref())
        .unwrap_or("unknown");
    let error_message = detail
        .as_ref()
        .and_then(|detail| detail.message.as_deref())
        .unwrap_or("unknown error");
    let error_code = detail.as_ref().and_then(|detail| detail.code.as_deref());
    let deterministic_error =
        is_known_deterministic_provider_error(detail.as_ref(), error.as_str());

    let classification = match error_code {
        Some("rate_limit_exceeded") | Some("rate_limit_exceeded_error") => {
            ProviderFailureClassification {
                kind: ProviderFailureKind::RateLimited,
                disposition: RetryDisposition::Retryable,
            }
        }
        Some("insufficient_quota") | Some("quota_exceeded") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("invalid_api_key") | Some("invalid_authentication") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("context_length_exceeded") | Some("max_tokens") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("invalid_request_error") | Some("invalid_request") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("server_error") | Some("service_unavailable") if !deterministic_error => {
            ProviderFailureClassification {
                kind: ProviderFailureKind::ServerError,
                disposition: RetryDisposition::Retryable,
            }
        }
        _ if error_type == "rate_limit_error" => ProviderFailureClassification {
            kind: ProviderFailureKind::RateLimited,
            disposition: RetryDisposition::Retryable,
        },
        _ if deterministic_error => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        _ => match error_type {
            "rate_limit_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::RateLimited,
                disposition: RetryDisposition::Retryable,
            },
            "invalid_request_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::ContractError,
                disposition: RetryDisposition::FailFast,
            },
            "authentication_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::AuthError,
                disposition: RetryDisposition::FailFast,
            },
            "server_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::ServerError,
                disposition: RetryDisposition::Retryable,
            },
            _ => ProviderFailureClassification {
                kind: ProviderFailureKind::ContractError,
                disposition: RetryDisposition::FailFast,
            },
        },
    };

    let detail = if let Some(detail) = detail.as_ref() {
        format_upstream_error_detail(detail)
    } else if let Some(code) = error_code {
        format!("{code}: {error_message}")
    } else {
        format!("{error_type}: {error_message}")
    };

    let throttle = (classification.kind == ProviderFailureKind::RateLimited)
        .then(|| crate::provider::retry::provider_throttle_metadata(retry_after));
    crate::provider::retry::provider_transport_error_with_code_and_retry_after(
        classification,
        error_code,
        Some(status.as_u16()),
        Some(crate::provider::ProviderTransportDiagnostics {
            stage: "response_status".into(),
            streaming: Some(streaming),
            provider: Some("openai".into()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(crate::provider::retry::sanitize_transport_url),
            status: Some(status.as_u16()),
            reqwest: None,
            context_budget: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(Some(status.as_u16()))),
            quota_identity: None,
            retry_after_not_before: retry_after.map(|hint| hint.not_before),
            throttle,
            source_chain: Vec::new(),
        }),
        retry_after.map(|hint| hint.delay),
        format!("{}: {}", context, detail),
    )
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) async fn send_chat_completion_stream_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
) -> Result<ParsedOpenAiResponse> {
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI Chat Completions streaming request failed",
        "request_send",
        "openai",
        Some(&provider_model_ref("openai", &body)),
        Some(url.as_str()),
        RequestDeadline::FullRequest,
        None,
        None,
    )
    .await?;

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = parse_retry_after(response.headers());
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        return Err(classify_chat_completion_status_error(
            "OpenAI Chat Completions streaming request failed",
            status,
            body,
            None,
            None,
            None,
            retry_after,
            true,
        ));
    }

    let response = read_chat_completion_stream(response).await?;
    parse_chat_completion_response(response)
}

#[cfg(test)]
#[allow(dead_code)]
async fn read_chat_completion_stream(response: Response) -> Result<Value> {
    const MAX_STREAMED_EVENTS: usize = 128;
    let mut streamed_events = Vec::new();

    let mut response = response;
    let mut pending = String::new();
    let mut data_lines = Vec::new();
    let mut text_decoder = crate::utf8::IncrementalUtf8LossyDecoder::new();

    while let Some(chunk) = response.chunk().await.map_err(|error| {
        crate::provider::retry::classify_reqwest_transport_error_with_trace(
            "OpenAI Chat Completions streaming response failed",
            "streaming_response_body",
            "openai",
            None,
            None,
            error,
            None,
        )
    })? {
        pending.push_str(&text_decoder.push(&chunk));
        while let Some(newline_idx) = pending.find('\n') {
            let mut line = pending[..newline_idx].to_string();
            pending.drain(..newline_idx + 1);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                if let Some(event) = process_chat_completion_sse_event(&mut data_lines)? {
                    match event {
                        ChatCompletionSseEvent::ContentDelta(delta) => {
                            if streamed_events.len() >= MAX_STREAMED_EVENTS {
                                return Err(invalid_response_error(
                                    "Chat Completions streaming exceeded maximum event count",
                                    "max_streamed_events_exceeded",
                                ));
                            }
                            streamed_events.push(json!({"delta": {"content": delta}}));
                        }
                        ChatCompletionSseEvent::ToolCallDelta(tool_call_delta) => {
                            if streamed_events.len() >= MAX_STREAMED_EVENTS {
                                return Err(invalid_response_error(
                                    "Chat Completions streaming exceeded maximum event count",
                                    "max_streamed_events_exceeded",
                                ));
                            }
                            // Extract the tool_calls array and store in delta format
                            if let Some(tool_calls_array) = tool_call_delta.get("tool_calls") {
                                streamed_events
                                    .push(json!({"delta": {"tool_calls": tool_calls_array}}));
                            }
                        }
                        ChatCompletionSseEvent::Done => {
                            // Stream ended
                            break;
                        }
                    }
                }
                continue;
            }
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start().to_string());
            }
        }
    }

    pending.push_str(&text_decoder.finish());
    // Process remaining data
    if !pending.is_empty() {
        let line = pending.trim();
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }

    // Process final event
    if let Some(event) = process_chat_completion_sse_event(&mut data_lines)? {
        match event {
            ChatCompletionSseEvent::ContentDelta(delta) => {
                if streamed_events.len() >= MAX_STREAMED_EVENTS {
                    return Err(invalid_response_error(
                        "Chat Completions streaming exceeded maximum event count",
                        "max_streamed_events_exceeded",
                    ));
                }
                streamed_events.push(json!({"delta": {"content": delta}}));
            }
            ChatCompletionSseEvent::ToolCallDelta(tool_call_delta) => {
                if streamed_events.len() >= MAX_STREAMED_EVENTS {
                    return Err(invalid_response_error(
                        "Chat Completions streaming exceeded maximum event count",
                        "max_streamed_events_exceeded",
                    ));
                }
                // Extract the tool_calls array and store in delta format
                if let Some(tool_calls_array) = tool_call_delta.get("tool_calls") {
                    streamed_events.push(json!({"delta": {"tool_calls": tool_calls_array}}));
                }
            }
            ChatCompletionSseEvent::Done => {}
        }
    }

    // Accumulate streamed events into final response
    let accumulated = accumulate_chat_completion_stream_events(streamed_events)?;
    Ok(accumulated)
}

#[cfg(test)]
mod retry_after_tests {
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
                let decoded: ProviderTransportDiagnostics =
                    serde_json::from_value(encoded).unwrap();
                assert_eq!(decoded, *diagnostics);
            }
        }
    }
}
