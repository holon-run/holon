use super::super::*;
use super::continuation::{canonicalize_openai_provider_item, normalize_openai_function_arguments};

pub(super) async fn read_openai_streaming_response(
    response: Response,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<Value> {
    let idle_timeout = stream_idle_timeout();
    read_openai_streaming_response_with_timeout(response, idle_timeout, trace).await
}

async fn read_openai_streaming_response_with_timeout(
    response: Response,
    idle_timeout: Duration,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<Value> {
    const MAX_STREAMED_OUTPUT_ITEMS: usize = 128;

    let mut response = response;
    let mut pending = String::new();
    let mut data_lines = Vec::new();
    let mut streamed_output_items = Vec::new();

    while let Some(chunk) = tokio::time::timeout(idle_timeout, response.chunk())
        .await
        .map_err(|_| {
            timeout_transport_error_with_trace(
                "OpenAI-style streaming response body timed out",
                "streaming_response_body",
                "openai-codex",
                None,
                None,
                format!(
                    "timed out waiting for SSE chunk after {} ms",
                    idle_timeout.as_millis()
                ),
                trace,
            )
        })?
        .map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                "OpenAI-style streaming response body failed",
                "streaming_response_body",
                "openai-codex",
                None,
                None,
                error,
                trace,
            )
        })?
    {
        trace_stream_chunk(trace, &chunk);
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline_idx) = pending.find('\n') {
            let mut line = pending[..newline_idx].to_string();
            pending.drain(..=newline_idx);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                match consume_openai_sse_event(&mut data_lines)? {
                    StreamingSseEvent::Continue => {}
                    StreamingSseEvent::OutputItem(item) => push_streamed_output_item(
                        &mut streamed_output_items,
                        item,
                        MAX_STREAMED_OUTPUT_ITEMS,
                    )?,
                    StreamingSseEvent::Incomplete(response) => {
                        return recover_openai_incomplete_response(response, &streamed_output_items)
                    }
                    StreamingSseEvent::Done => return Err(early_done_protocol_violation_error()),
                    StreamingSseEvent::Terminal(response) => {
                        let response =
                            finalize_openai_terminal_response(response, &streamed_output_items);
                        trace_stream_terminal(trace, &response);
                        return Ok(response);
                    }
                }
                continue;
            }
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start().to_string());
            }
        }
    }

    if !pending.is_empty() {
        let line = pending.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }
    match consume_openai_sse_event(&mut data_lines)? {
        StreamingSseEvent::Continue => {}
        StreamingSseEvent::OutputItem(item) => {
            push_streamed_output_item(&mut streamed_output_items, item, MAX_STREAMED_OUTPUT_ITEMS)?
        }
        StreamingSseEvent::Incomplete(response) => {
            return recover_openai_incomplete_response(response, &streamed_output_items)
        }
        StreamingSseEvent::Done => return Err(early_done_protocol_violation_error()),
        StreamingSseEvent::Terminal(response) => {
            let response = finalize_openai_terminal_response(response, &streamed_output_items);
            trace_stream_terminal(trace, &response);
            return Ok(response);
        }
    }

    Err(invalid_response_error(
        "OpenAI-style streaming response did not contain a terminal response event",
        "missing terminal response",
    ))
}

fn early_done_protocol_violation_error() -> anyhow::Error {
    invalid_response_error(
        "OpenAI-style streaming response ended before a terminal response event",
        "[DONE] observed before terminal response",
    )
}

pub(in super::super) enum StreamingSseEvent {
    Continue,
    OutputItem(Value),
    Incomplete(Value),
    Done,
    Terminal(Value),
}

fn push_streamed_output_item(
    streamed_output_items: &mut Vec<Value>,
    item: Value,
    max_items: usize,
) -> Result<()> {
    if streamed_output_items.len() >= max_items {
        return Err(invalid_response_error(
            "OpenAI-style streaming response emitted too many output items",
            format!("received more than {max_items} streamed output items"),
        ));
    }
    streamed_output_items.push(item);
    Ok(())
}

pub(in super::super) fn consume_openai_sse_event(
    data_lines: &mut Vec<String>,
) -> Result<StreamingSseEvent> {
    if data_lines.is_empty() {
        return Ok(StreamingSseEvent::Continue);
    }

    let payload = data_lines.join("\n");
    data_lines.clear();
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Ok(StreamingSseEvent::Continue);
    }
    if trimmed == "[DONE]" {
        return Ok(StreamingSseEvent::Done);
    }

    let event: Value = serde_json::from_str(trimmed)
        .map_err(|error| invalid_response_error("invalid OpenAI-style streaming JSON", error))?;

    if event.get("type").and_then(Value::as_str) == Some("error") {
        return Err(classify_openai_streaming_error(
            "OpenAI-style streaming response reported an error event",
            event.get("error"),
            Some(&event),
        ));
    }

    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == "response.output_item.done" {
        if let Some(item) = event.get("item") {
            return Ok(StreamingSseEvent::OutputItem(item.clone()));
        }
        return Err(invalid_response_error(
            "OpenAI-style streaming response output item event was missing item",
            "missing item",
        ));
    }
    if let Some(response) = event.get("response") {
        let status = response.get("status").and_then(Value::as_str);
        if event_type == "response.completed" || status == Some("completed") {
            return Ok(StreamingSseEvent::Terminal(response.clone()));
        }
        if event_type == "response.failed" || status == Some("failed") {
            return Err(classify_openai_streaming_error(
                "OpenAI-style streaming response failed",
                response.get("error"),
                Some(response),
            ));
        }
        if event_type == "response.incomplete" || status == Some("incomplete") {
            if openai_incomplete_reason(response) == Some("max_output_tokens") {
                return Ok(StreamingSseEvent::Incomplete(response.clone()));
            }
            return Err(classify_openai_incomplete_response(response));
        }
        if status == Some("cancelled") {
            return Err(classify_openai_incomplete_response(response));
        }
    }

    Ok(StreamingSseEvent::Continue)
}

fn recover_openai_incomplete_response(
    response: Value,
    streamed_output_items: &[Value],
) -> Result<Value> {
    let response = finalize_openai_terminal_response(response, streamed_output_items);
    let has_output = response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| !output.is_empty());
    if has_output {
        Ok(response)
    } else {
        Err(classify_openai_incomplete_response(&response))
    }
}

fn finalize_openai_terminal_response(
    mut response: Value,
    streamed_output_items: &[Value],
) -> Value {
    let has_output = response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| !output.is_empty());
    if has_output || streamed_output_items.is_empty() {
        return response;
    }

    if let Some(object) = response.as_object_mut() {
        object.insert(
            "output".to_string(),
            Value::Array(streamed_output_items.to_vec()),
        );
    }
    response
}

fn classify_openai_streaming_error(
    context: &str,
    error: Option<&Value>,
    response: Option<&Value>,
) -> anyhow::Error {
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .and_then(|response| response.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown streaming error");
    let classification = match code {
        Some("rate_limit_exceeded") => ProviderFailureClassification {
            kind: ProviderFailureKind::RateLimited,
            disposition: RetryDisposition::Retryable,
        },
        Some("server_error" | "service_unavailable" | "server_is_overloaded" | "slow_down") => {
            ProviderFailureClassification {
                kind: ProviderFailureKind::ServerError,
                disposition: RetryDisposition::Retryable,
            }
        }
        Some("insufficient_quota") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("context_length_exceeded" | "invalid_prompt") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        _ => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
    };
    let detail = code
        .map(|code| format!("{code}: {message}"))
        .unwrap_or_else(|| message.to_string());
    provider_transport_error(classification, None, None, format!("{context}: {detail}"))
}

fn classify_openai_incomplete_response(response: &Value) -> anyhow::Error {
    let reason = openai_incomplete_reason(response)
        .or_else(|| response.get("status").and_then(Value::as_str))
        .unwrap_or("unknown");
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        None,
        None,
        format!("OpenAI-style streaming response did not complete successfully: {reason}"),
    )
}

fn openai_incomplete_reason(response: &Value) -> Option<&str> {
    response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
}

#[allow(dead_code)]
pub(crate) fn parse_openai_response(response: Value) -> Result<ProviderTurnResponse> {
    parse_openai_response_with_transport_state(response).map(|parsed| parsed.response)
}

pub(in super::super) fn parse_openai_response_with_transport_state(
    response: Value,
) -> Result<ParsedOpenAiResponse> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI-style response did not contain an output array",
                "missing output array",
            )
        })?;
    let output_items = output
        .iter()
        .map(canonicalize_openai_provider_item)
        .collect::<Vec<_>>();
    let mut blocks = Vec::new();

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for content_item in content {
                        match content_item.get("type").and_then(Value::as_str) {
                            Some("output_text") | Some("text") | Some("input_text") => {
                                if let Some(text) = content_item.get("text").and_then(Value::as_str)
                                {
                                    blocks.push(ModelBlock::Text {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_response_error(
                            "function_call item did not include call_id",
                            "missing call_id",
                        )
                    })?;
                let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "function_call item did not include name",
                        "missing name",
                    )
                })?;
                let input = normalize_openai_function_arguments(item.get("arguments"));
                blocks.push(ModelBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input,
                    kind: ModelToolCallKind::Function,
                });
            }
            Some("custom_tool_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_response_error(
                            "custom_tool_call item did not include call_id",
                            "missing call_id",
                        )
                    })?;
                let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "custom_tool_call item did not include name",
                        "missing name",
                    )
                })?;
                let input = item.get("input").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "custom_tool_call item did not include string input",
                        "missing input",
                    )
                })?;
                blocks.push(ModelBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: Value::String(input.to_string()),
                    kind: ModelToolCallKind::Custom,
                });
            }
            _ => {}
        }
    }

    if blocks.is_empty() {
        return Err(invalid_response_error(
            "OpenAI-style response contained no supported content blocks",
            "empty supported block set",
        ));
    }

    let usage = response.get("usage").and_then(Value::as_object);
    let cache_usage = usage.map(|usage| ProviderCacheUsage {
        read_input_tokens: usage
            .get("input_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(Value::as_object)
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or(0),
        creation_input_tokens: 0,
    });
    Ok(ParsedOpenAiResponse {
        response: ProviderTurnResponse {
            blocks,
            stop_reason: response
                .get("incomplete_details")
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    response
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .or_else(|| {
                    response
                        .get("stop_reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
            input_tokens: usage
                .and_then(|usage| usage.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_usage,
            provider_message_id: response_id.clone(),
            provider_request_id: None,
            request_diagnostics: None,
        },
        response_id,
        output_items,
    })
}

#[allow(dead_code)]
fn unsupported_streaming_transport_error(provider_name: &str) -> anyhow::Error {
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::UnsupportedTransport,
            disposition: RetryDisposition::FailFast,
        },
        None,
        None,
        format!("{provider_name} requires a streaming transport contract"),
    )
}
