use super::*;

pub(super) fn trace_response_headers(
    trace: Option<&ProviderHttpTraceRequest>,
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) {
    if let Some(trace) = trace {
        trace.write_response_headers(status, headers);
    }
}

pub(super) fn trace_response_body(trace: Option<&ProviderHttpTraceRequest>, body: &str) {
    if let Some(trace) = trace {
        trace.write_response_body(body);
    }
}

pub(super) fn trace_stream_chunk(trace: Option<&ProviderHttpTraceRequest>, chunk: &[u8]) {
    if let Some(trace) = trace {
        trace.write_stream_chunk(chunk);
    }
}

pub(super) fn trace_stream_terminal(trace: Option<&ProviderHttpTraceRequest>, body: &Value) {
    if let Some(trace) = trace {
        trace.write_stream_terminal(body);
    }
}

pub(super) fn provider_request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("request-id"))
        .or_else(|| headers.get("openai-request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) async fn send_openai_request(
    mut request: RequestBuilder,
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    enforce_full_request_deadline: bool,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<Response> {
    let timeout = request_send_timeout();
    if enforce_full_request_deadline {
        request = request.timeout(timeout);
        return request.send().await.map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                context, stage, provider, model_ref, url, error, trace,
            )
        });
    }
    tokio::time::timeout(timeout, request.send())
        .await
        .map_err(|_| {
            timeout_transport_error_with_trace(
                context,
                stage,
                provider,
                model_ref,
                url,
                format!("request_send_timeout_ms={}", timeout.as_millis()),
                trace,
            )
        })?
        .map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                context, stage, provider, model_ref, url, error, trace,
            )
        })
}

pub(super) fn model_from_request(body: &Value) -> &str {
    body.get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

pub(super) fn provider_model_ref(provider: &str, body: &Value) -> String {
    format!("{provider}/{}", model_from_request(body))
}
