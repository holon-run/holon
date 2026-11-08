mod support;

use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use holon::{
    config::{AppConfig, ModelRouteRef, ProviderId},
    host::RuntimeHost,
    provider::{
        build_provider_from_model_chain, ConversationMessage, ModelBlock, ProviderTurnRequest,
    },
    tool::{ToolCall, ToolRegistry},
    types::AuthorityClass,
};
use serde_json::{json, Value};

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
#[ignore = "requires explicit HOLON_LIVE_VIEW_IMAGE_MODEL, configured credentials, and network access"]
async fn live_view_image_runtime_observation_and_cache() -> Result<()> {
    let model = ModelRouteRef::parse_compatible(
        &std::env::var("HOLON_LIVE_VIEW_IMAGE_MODEL")
            .context("set HOLON_LIVE_VIEW_IMAGE_MODEL to authorize the exact live route")?,
    )?;
    let mut config = AppConfig::load()?;
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    support::init_git_repo(workspace.path())?;
    let image_path = workspace.path().join("logo.png");
    std::fs::copy("docs/website/assets/logo.png", &image_path)?;
    config.default_agent_id = "default".into();
    config.default_model = model.clone();
    config.fallback_models.clear();
    config.disable_provider_fallback = true;
    config.vision_model = Some(model.clone());
    config.vision_candidate_models.clear();
    config.runtime_max_output_tokens = 4096;
    config.home_dir = home.path().to_path_buf();
    config.data_dir = home.path().to_path_buf();
    config.config_file_path = home.path().join("config.json");
    config.socket_path = home.path().join("run/holon.sock");
    config.workspace_dir = workspace.path().to_path_buf();
    let host = RuntimeHost::new(config)?;
    support::attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let registry = ToolRegistry::new(workspace.path().to_path_buf());
    // Two distinct prompts exercise real default and explicit dispatch. The third
    // request repeats the explicit call to verify cache reuse without more spend.
    for (index, explicit, cached) in [(0, false, false), (1, true, false), (1, true, true)] {
        let mut input = json!({
            "path": image_path,
            "prompt": format!("Describe the public HOLON logo and visible text. Inspection {index}."),
        });
        if explicit {
            input["model"] = json!(model.as_string());
        }
        let (output, _) = registry
            .execute(
                &runtime,
                "default",
                &AuthorityClass::OperatorInstruction,
                &ToolCall {
                    id: format!("live-view-image-{index}-{cached}"),
                    name: "ViewImage".into(),
                    input,
                },
            )
            .await?;
        ensure!(
            output.envelope.error.is_none(),
            "runtime ViewImage failed: {:?}",
            output.envelope.error
        );
        let result = output.envelope.result.context("missing ViewImage result")?;
        ensure!(result["cache_hit"] == cached, "unexpected cache outcome");
        ensure!(
            result["vision_selection"]["resolved_model"] == model.as_string(),
            "unexpected route"
        );
        ensure!(
            result["vision_selection"]["selection_source"]
                == if explicit {
                    "explicit_call"
                } else {
                    "vision_default"
                },
            "unexpected selection source"
        );
        let observation = &result["observation"];
        ensure!(
            observation["type"] == "visual_observation",
            "invalid observation type"
        );
        ensure!(
            observation["schema"] == "visual_observation.v1",
            "invalid observation schema"
        );
        let summary = observation["summary"].as_str().context("missing summary")?;
        ensure!(
            summary.to_ascii_lowercase().contains("holon"),
            "logo text was not recognized"
        );
        ensure!(
            observation["uncertainties"]
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            "invalid uncertainties"
        );
        println!(
            "runtime ViewImage route={} explicit={explicit} cache_hit={cached} summary={summary}",
            model.as_string()
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires configured live vision provider credentials and network access"]
async fn live_view_image_vision_adapter_returns_visual_observation_json() -> Result<()> {
    let config = AppConfig::load()?;
    let model_override = std::env::var("HOLON_LIVE_VIEW_IMAGE_MODEL")
        .ok()
        .map(|value| ModelRouteRef::parse_compatible(&value))
        .transpose()
        .context("HOLON_LIVE_VIEW_IMAGE_MODEL must be a valid model route")?;
    let model_ref = match model_override {
        Some(model_ref) => model_ref,
        None => config
            .provider_chain()
            .into_iter()
            .find(|model| {
                matches!(
                    model.provider.as_str(),
                    ProviderId::OPENAI | ProviderId::OPENAI_CODEX
                )
            })
            .context(
                "live ViewImage needs a configured OpenAI-compatible vision provider or \
                 HOLON_LIVE_VIEW_IMAGE_MODEL",
            )?,
    };
    let provider = build_provider_from_model_chain(&config, &[model_ref.clone()])?;
    let image = std::fs::read("docs/website/assets/logo.png")?;
    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "You are a vision adapter for a headless agent. Inspect only the provided image and task prompt. Return exactly one JSON object and no markdown, prose, or implementation advice. The JSON object must match this shape: {\"type\":\"visual_observation\",\"schema\":\"visual_observation.v1\",\"summary\":\"string\",\"ocr\":[],\"elements\":[],\"relations\":[],\"issues\":[],\"uncertainties\":[],\"external_sources\":[]}. Required fields: type=\"visual_observation\", schema=\"visual_observation.v1\", summary, uncertainties. The uncertainties field must be an array of strings; use [] when there are no caveats. The ocr, elements, relations, issues, and external_sources fields must be arrays of objects; omit them or use [] when empty. Include visible text in ocr or summary; include bounding boxes when location matters; describe only visible evidence; say when uncertain.",
            vec![ConversationMessage::UserImage {
                prompt: "Describe the visible logo and any text. Return only the required JSON object.".to_string(),
                media_type: "image/png".to_string(),
                data_base64: BASE64_STANDARD.encode(image).into(),
                optional_history: false,
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
    let uncertainties = match value.get("uncertainties") {
        Some(Value::Array(values)) => values.as_slice(),
        Some(value) => std::slice::from_ref(value),
        None => &[],
    };
    assert!(
        uncertainties
            .iter()
            .all(|entry| accepted_uncertainty_text(entry).is_some()),
        "uncertainties must have readable entries when present; raw={text:?}"
    );
    println!(
        "live_view_image ref={} input_tokens={} output_tokens={} text={text}",
        model_ref.as_string(),
        output.input_tokens,
        output.output_tokens
    );
    Ok(())
}
