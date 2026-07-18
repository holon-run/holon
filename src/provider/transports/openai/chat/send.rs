use super::super::*;
use super::parse::parse_chat_completion_response;
#[cfg(test)]
use super::parse::{
    accumulate_chat_completion_stream_events, process_chat_completion_sse_event,
    ChatCompletionSseEvent,
};

pub(crate) async fn send_chat_completion_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<ParsedOpenAiResponse> {
    let model_ref = provider_model_ref("openai", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai",
            Some(&model_ref),
            url.as_str(),
            "chat_completions",
            &headers,
            &body,
        )
    });
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI Chat Completions request failed",
        "request_send",
        "openai",
        Some(&model_ref),
        Some(url.as_str()),
        true,
        request_trace.as_ref(),
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );
    let provider_request_id = provider_request_id_from_headers(response.headers());

    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_chat_completion_status_error(
            "OpenAI Chat Completions request failed",
            status,
            body,
            Some(&model_ref),
            Some(url.as_str()),
            request_trace.as_ref(),
        ));
    }

    let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
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
    trace_response_body(request_trace.as_ref(), &body);

    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| invalid_response_error("invalid OpenAI Chat Completions JSON", error))?;

    parse_chat_completion_response(parsed)
        .map(|parsed| parsed.with_provider_request_id(provider_request_id))
}

fn classify_chat_completion_status_error(
    context: &str,
    status: reqwest::StatusCode,
    body: String,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
) -> anyhow::Error {
    // Try to parse as OpenAI error response
    if let Ok(error_json) = serde_json::from_str::<Value>(&body) {
        if let Some(error_obj) = error_json.get("error") {
            return classify_openai_chat_completion_error(
                context, error_obj, status, model_ref, url, trace,
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
    )
}

pub(crate) fn classify_openai_chat_completion_error(
    context: &str,
    error: &Value,
    status: reqwest::StatusCode,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
) -> anyhow::Error {
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let error_message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let error_code = error.get("code").and_then(Value::as_str);

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
        Some("server_error") | Some("service_unavailable") => ProviderFailureClassification {
            kind: ProviderFailureKind::ServerError,
            disposition: RetryDisposition::Retryable,
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

    let detail = if let Some(code) = error_code {
        format!("{}: {}", code, error_message)
    } else {
        format!("{}: {}", error_type, error_message)
    };

    crate::provider::retry::provider_transport_error_with_code(
        classification,
        error_code,
        Some(status.as_u16()),
        Some(crate::provider::ProviderTransportDiagnostics {
            stage: "response_status".into(),
            provider: Some("openai".into()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(crate::provider::retry::sanitize_transport_url),
            status: Some(status.as_u16()),
            reqwest: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(Some(status.as_u16()))),
            source_chain: Vec::new(),
        }),
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
        true,
        None,
    )
    .await?;

    if !response.status().is_success() {
        let status = response.status();
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
        pending.push_str(&String::from_utf8_lossy(&chunk));
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
