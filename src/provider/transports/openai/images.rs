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

/// The API dialect is an endpoint identity, not a shared OpenAI schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OpenAiImagesDialect {
    OpenAi,
    Xai,
    Volcengine,
    Unknown,
}

pub(super) fn openai_images_dialect(provider_id: &str, base_url: &str) -> OpenAiImagesDialect {
    use crate::provider::image_generation::{image_generation_api, ImageGenerationApi};
    match image_generation_api(
        provider_id,
        crate::config::ProviderTransportKind::OpenAiResponses,
        base_url,
    ) {
        ImageGenerationApi::OpenAiImages => OpenAiImagesDialect::OpenAi,
        ImageGenerationApi::XaiImages => OpenAiImagesDialect::Xai,
        ImageGenerationApi::VolcengineImages => OpenAiImagesDialect::Volcengine,
        _ => OpenAiImagesDialect::Unknown,
    }
}

pub(super) fn build_images_request(
    dialect: OpenAiImagesDialect,
    model: &str,
    request: &ProviderGenerateImageRequest,
) -> Result<Value> {
    use crate::provider::image_generation::{
        contract_for_api, ImageGenerationApi, ImageGenerationParameters,
    };
    let api = match dialect {
        OpenAiImagesDialect::OpenAi => ImageGenerationApi::OpenAiImages,
        OpenAiImagesDialect::Xai => ImageGenerationApi::XaiImages,
        OpenAiImagesDialect::Volcengine => ImageGenerationApi::VolcengineImages,
        OpenAiImagesDialect::Unknown => ImageGenerationApi::UnknownImages,
    };
    let contract = contract_for_api(api, model, true);
    // Here output_format is already a native wire option. Delivery conversion
    // belongs to the runtime, not to this network adapter.
    if let Some(format) = &request.output_format {
        if !contract.native_output_formats.contains(format) {
            return Err(
                crate::provider::image_generation::ImageGenerationParameterError {
                    code: "unsupported_image_parameter",
                    route: model.into(),
                    field: "output_format".into(),
                    message:
                        "native output_format is not confirmed; use runtime delivery conversion"
                            .into(),
                    allowed_values: contract.native_output_formats.clone(),
                    recovery_hint: "omit the native format parameter".into(),
                }
                .into(),
            );
        }
    }
    let parameters = contract.resolve_parameters(&ImageGenerationParameters {
        size: request.size.clone(),
        aspect_ratio: request.aspect_ratio.clone(),
        resolution: request.resolution.clone(),
        background: request.background.clone(),
        output_format: request.output_format.clone(),
    })?;
    let p = parameters.provider_parameters;
    let mut body = json!({"model": model, "prompt": request.prompt});
    match dialect {
        OpenAiImagesDialect::OpenAi => {
            body["n"] = json!(1);
        }
        OpenAiImagesDialect::Xai => {
            body["n"] = json!(1);
            body["response_format"] = json!("b64_json");
        }
        OpenAiImagesDialect::Volcengine => {
            body["response_format"] = json!("b64_json");
        }
        OpenAiImagesDialect::Unknown => {}
    }
    for (field, value) in [
        ("size", p.size),
        ("aspect_ratio", p.aspect_ratio),
        ("resolution", p.resolution),
        ("background", p.background),
        ("output_format", p.output_format),
    ] {
        if let Some(value) = value {
            body[field] = json!(value);
        }
    }
    Ok(body)
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
    provider_id: &str,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let model_ref = provider_model_ref(provider_id, &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            provider_id,
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
    let mut response = send_openai_request(
        request.json(&body),
        "Images request failed",
        "request_send",
        provider_id,
        Some(&model_ref),
        Some(url.as_str()),
        RequestDeadline::HeadersOnly,
        request_trace.as_ref(),
        None,
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );
    if !response.status().is_success() {
        let status = response.status();
        let retry_after = parse_retry_after(response.headers());
        let body = read_error_response_body(response, request_trace.as_ref()).await;
        return Err(classify_status_error_with_trace(
            "Images request failed",
            "response_status",
            Some(provider_id),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
            retry_after,
        ));
    }
    let read_body = async {
        const MAX_RESPONSE_BYTES: usize = 46 * 1024 * 1024;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                "Images response body failed",
                "response_body",
                provider_id,
                Some(&model_ref),
                Some(url.as_str()),
                error,
                request_trace.as_ref(),
            )
        })? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(image_receipt_limit("image response exceeds 46 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes)
            .map_err(|error| invalid_response_error("invalid image response encoding", error))
    };
    let body = match tokio::time::timeout(response_body_timeout(), read_body).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => return Err(error),
        Err(_elapsed) => {
            return Err(timeout_transport_error_with_trace(
                "Images response body read timed out",
                "response_body",
                provider_id,
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
        let bytes = decode_generated_image(b64)?;
        // xAI Grok Imagine reports the real media type per item; OpenAI omits
        // it and lets the runtime detect the format from the decoded bytes.
        let mime = item
            .get("mime_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        images.push(ProviderGeneratedImage { bytes, mime });
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
        let bytes = decode_generated_image(b64)?;
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

fn decode_generated_image(encoded: &str) -> Result<Vec<u8>> {
    use crate::provider::image_delivery::MAX_IMAGE_BYTES;
    if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(image_receipt_limit(
            "encoded source exceeds the 32 MiB image limit",
        ));
    }
    let bytes = BASE64_STANDARD
        .decode(encoded)
        .map_err(|error| invalid_response_error("invalid generated image base64 payload", error))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(image_receipt_limit("source exceeds the 32 MiB image limit"));
    }
    Ok(bytes)
}

pub(super) fn image_receipt_limit(message: &str) -> anyhow::Error {
    crate::tool::ToolError::new("image_resource_limit", message)
        .with_details(json!({
            "generation_may_have_succeeded": true,
            "original_retained": false,
            "stage": "bounded_provider_response_receipt",
        }))
        .with_recovery_hint("do not automatically regenerate; recover the original through the provider if available")
        .into()
}

pub(super) async fn send_openai_codex_image_generation_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let model_ref = provider_model_ref("openai-codex", &body);
    let terminal_response = send_openai_responses_streaming_request(
        client,
        url,
        body,
        headers,
        trace,
        agent_id,
        "openai-codex",
        &model_ref,
        None,
    )
    .await?;
    parse_openai_codex_image_generation_response_items(terminal_response.output_items)
}
