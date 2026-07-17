use super::*;

pub(super) fn openai_images_generations_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/images/generations") {
        trimmed.to_string()
    } else if has_trailing_version_segment(trimmed) {
        format!("{trimmed}/images/generations")
    } else {
        format!("{trimmed}/v1/images/generations")
    }
}

pub(super) fn build_openai_images_request(
    model: &str,
    request: &ProviderGenerateImageRequest,
) -> Value {
    let mut body = json!({
        "model": model,
        "prompt": request.prompt,
        "n": 1,
        "response_format": "b64_json",
    });
    if let Some(size) = request
        .size
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["size"] = Value::String(size.clone());
    }
    if let Some(background) = request
        .background
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["background"] = Value::String(background.clone());
    }
    if let Some(output_format) = request
        .output_format
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["output_format"] = Value::String(output_format.clone());
    }
    body
}

pub(super) fn build_openai_codex_image_generation_request(
    model: &str,
    request: &ProviderGenerateImageRequest,
) -> Value {
    let mut body = json!({
        "model": model,
        "input": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": request.prompt,
                    }
                ],
            }
        ],
        "tools": [
            {
                "type": "image_generation",
                "output_format": request
                    .output_format
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("png"),
            }
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "stream": true,
    });
    if let Some(size) = request
        .size
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["tools"][0]["size"] = Value::String(size.clone());
    }
    if let Some(background) = request
        .background
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["tools"][0]["background"] = Value::String(background.clone());
    }
    body
}

pub(super) async fn send_openai_images_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let model_ref = provider_model_ref("openai", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai",
            Some(&model_ref),
            url.as_str(),
            "images_generations",
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
        "OpenAI Images request failed",
        "request_send",
        "openai",
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
    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_status_error_with_trace(
            "OpenAI Images request failed",
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
                "OpenAI Images response body failed",
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
                "OpenAI Images response body read timed out",
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
        .map_err(|error| invalid_response_error("invalid OpenAI Images JSON", error))?;
    parse_openai_images_response(parsed)
}

pub(super) fn parse_openai_images_response(value: Value) -> Result<Vec<ProviderGeneratedImage>> {
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
        invalid_response_error("OpenAI Images response missing data", "missing data")
    })?;
    let mut images = Vec::new();
    for item in data {
        let b64 = item
            .get("b64_json")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Images response item missing b64_json",
                    "missing b64_json",
                )
            })?;
        let bytes = BASE64_STANDARD.decode(b64).map_err(|error| {
            invalid_response_error("invalid OpenAI Images base64 payload", error)
        })?;
        images.push(ProviderGeneratedImage { bytes, mime: None });
    }
    if images.is_empty() {
        return Err(invalid_response_error(
            "OpenAI Images response contained no images",
            "empty data",
        ));
    }
    Ok(images)
}

pub(super) fn parse_openai_codex_image_generation_response_items(
    output_items: Vec<Value>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let mut images = Vec::new();
    for item in output_items {
        if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            continue;
        }
        let b64 = item.get("result").and_then(Value::as_str).ok_or_else(|| {
            invalid_response_error(
                "OpenAI Codex image_generation_call item missing result",
                "missing result",
            )
        })?;
        let bytes = BASE64_STANDARD.decode(b64).map_err(|error| {
            invalid_response_error(
                "invalid OpenAI Codex image_generation base64 payload",
                error,
            )
        })?;
        images.push(ProviderGeneratedImage {
            bytes,
            mime: Some("image/png".into()),
        });
    }
    if images.is_empty() {
        return Err(invalid_response_error(
            "OpenAI Codex image generation response contained no completed images",
            "missing completed image_generation_call",
        ));
    }
    Ok(images)
}

pub(super) async fn send_openai_codex_image_generation_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let terminal_response =
        send_openai_responses_streaming_request(client, url, body, headers, trace, agent_id)
            .await?;
    parse_openai_codex_image_generation_response_items(terminal_response.output_items)
}
