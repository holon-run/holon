use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use holon::{
    config::{AppConfig, ProviderId},
    provider::{
        build_provider_from_model_chain, ConversationMessage, ModelBlock, ProviderTurnRequest,
    },
};
use serde_json::Value;

fn response_text(blocks: &[ModelBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn parse_observation_json(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("vision adapter response was empty");
    }
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Ok(value);
    }
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim);
    if let Some(unfenced) = unfenced {
        if let Ok(value) = serde_json::from_str(unfenced) {
            return Ok(value);
        }
    }
    let Some(start) = trimmed.find('{') else {
        bail!("vision adapter response did not contain JSON");
    };
    let Some(end) = trimmed.rfind('}') else {
        bail!("vision adapter response did not contain a complete JSON object");
    };
    serde_json::from_str(&trimmed[start..=end])
        .map_err(|error| anyhow!("vision adapter returned invalid observation JSON: {error}"))
}

fn accepted_uncertainty_text(value: &Value) -> Option<&str> {
    if let Some(text) = value.as_str() {
        return Some(text);
    }
    let object = value.as_object()?;
    ["text", "description", "summary", "message"]
        .iter()
        .find_map(|field| object.get(*field).and_then(Value::as_str))
}

#[tokio::test]
#[ignore = "requires configured live vision provider credentials and network access"]
async fn live_view_image_vision_adapter_returns_visual_observation_json() -> Result<()> {
    let config = AppConfig::load()?;
    let model_ref = config
        .provider_chain()
        .into_iter()
        .find(|model| {
            matches!(
                model.provider.as_str(),
                ProviderId::OPENAI | ProviderId::OPENAI_CODEX
            )
        })
        .context("live ViewImage needs a configured OpenAI-compatible vision provider")?;
    let provider = build_provider_from_model_chain(&config, &[model_ref.clone()])?;
    let image = std::fs::read("docs/website/assets/logo.png")?;
    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "You are a vision adapter for a headless agent. Inspect only the provided image and task prompt. Return exactly one JSON object and no markdown, prose, or implementation advice. The JSON object must match this shape: {\"type\":\"visual_observation\",\"schema\":\"visual_observation.v1\",\"summary\":\"string\",\"ocr\":[],\"elements\":[],\"relations\":[],\"issues\":[],\"uncertainties\":[],\"external_sources\":[]}. Required fields: type=\"visual_observation\", schema=\"visual_observation.v1\", summary, uncertainties. The uncertainties field must be an array of strings; use [] when there are no caveats. The ocr, elements, relations, issues, and external_sources fields must be arrays of objects; omit them or use [] when empty. Include visible text in ocr or summary; include bounding boxes when location matters; describe only visible evidence; say when uncertain.",
            vec![ConversationMessage::UserImage {
                prompt: "Describe the visible logo and any text. Return only the required JSON object.".to_string(),
                media_type: "image/png".to_string(),
                data_base64: BASE64_STANDARD.encode(image),
            }],
            Vec::new(),
        ))
        .await?;
    let text = response_text(&output.blocks);
    let value = parse_observation_json(&text).with_context(|| {
        format!(
            "raw live ViewImage response from {} was {text:?}",
            model_ref.as_string()
        )
    })?;
    assert_eq!(
        value.get("type").and_then(Value::as_str),
        Some("visual_observation"),
        "raw={text:?}"
    );
    assert_eq!(
        value.get("schema").and_then(Value::as_str),
        Some("visual_observation.v1"),
        "raw={text:?}"
    );
    assert!(
        value
            .get("summary")
            .and_then(Value::as_str)
            .is_some_and(|summary| !summary.trim().is_empty()),
        "raw={text:?}"
    );
    let uncertainties = value
        .get("uncertainties")
        .and_then(Value::as_array)
        .context("raw live response must include uncertainties as an array")?;
    assert!(
        uncertainties
            .iter()
            .all(|entry| accepted_uncertainty_text(entry).is_some()),
        "uncertainties must be strings or text-bearing objects so ViewImage can normalize them; raw={text:?}"
    );
    println!(
        "live_view_image ref={} input_tokens={} output_tokens={} text={text}",
        model_ref.as_string(),
        output.input_tokens,
        output.output_tokens
    );
    Ok(())
}
