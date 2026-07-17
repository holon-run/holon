use super::super::*;
use super::compaction::error_status;
use super::parse::{parse_openai_response_with_transport_state, read_openai_streaming_response};

pub(in super::super) async fn send_openai_responses_request(
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
            "responses",
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
        "OpenAI-style request failed",
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
        return Err(classify_status_error_with_trace(
            "OpenAI-style request failed",
            "response_status",
            Some("openai"),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
        ));
    }

    let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            return Err(classify_reqwest_transport_error_with_trace(
                "OpenAI-style response body failed",
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
                "OpenAI-style response body read timed out",
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
        .map_err(|error| invalid_response_error("invalid OpenAI-style JSON", error))?;
    parse_openai_response_with_transport_state(parsed)
        .map(|parsed| parsed.with_provider_request_id(provider_request_id))
}

pub(in super::super) async fn retry_openai_responses_with_lossless_replay(
    client: &Client,
    url: String,
    plan: &OpenAiRequestPlan,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
    error: anyhow::Error,
    diagnostics: &mut ProviderRequestDiagnostics,
    final_provider_input: &mut Vec<Value>,
    final_replay_loss_reason: &mut Option<String>,
) -> Result<ParsedOpenAiResponse> {
    if !matches!(error_status(&error), Some(400..=499))
        || plan.body.get("previous_response_id").is_none()
    {
        return Err(error);
    }
    let Some((fallback_body, fallback_provider_input)) = plan.fallback_replay.clone() else {
        return Err(error).with_context(|| {
            format!(
                "OpenAI Responses continuation failed and Holon refused provider-window replay because it would lose {}",
                plan.replay_loss_reason
                    .as_deref()
                    .unwrap_or("server-side response context")
            )
        });
    };
    *final_provider_input = fallback_provider_input;
    *final_replay_loss_reason = None;
    diagnostics.request_lowering_mode = "provider_window_replay".into();
    if let Some(continuation) = diagnostics.incremental_continuation.as_mut() {
        continuation.status = "fallback_provider_window_replay".into();
        continuation.fallback_reason = Some("previous_response_id_rejected".into());
        continuation.server_side_context_may_be_lost = None;
    }
    send_openai_responses_request(client, url, fallback_body, headers, trace, agent_id).await
}

pub(in super::super) async fn send_openai_responses_streaming_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<ParsedOpenAiResponse> {
    let model_ref = provider_model_ref("openai-codex", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai-codex",
            Some(&model_ref),
            url.as_str(),
            "responses_streaming",
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
        "OpenAI-style streaming request failed",
        "streaming_request_send",
        "openai-codex",
        Some(&model_ref),
        Some(url.as_str()),
        false,
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
        return Err(classify_status_error_with_trace(
            openai_codex_status_error_context(status),
            "response_status",
            Some("openai-codex"),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
        ));
    }

    let terminal_response =
        read_openai_streaming_response(response, request_trace.as_ref()).await?;
    parse_openai_response_with_transport_state(terminal_response)
        .map(|parsed| parsed.with_provider_request_id(provider_request_id))
}
