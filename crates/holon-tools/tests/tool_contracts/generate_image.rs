use super::serialize_success;
use holon_domain::{
    GenerateImageResult, GeneratedImageSourceReference, ImageConversionMetadata,
    ImageGenerationParameters,
};
use holon_tools::builtins::generate_image::*;
use serde_json::{json, Value};

#[test]
fn generate_image_historical_result_replays_without_new_metadata() {
    let historical = historical_result();
    let result: GenerateImageResult = serde_json::from_value(historical.clone()).unwrap();
    assert!(result.selection.is_none());
    assert!(result.generation_succeeded.is_none());
    assert!(result.source_images.is_empty());
    assert!(result.conversion.is_empty());
    assert_eq!(serde_json::to_value(&result).unwrap(), historical);
    let replay = serialize_success(NAME, &result).unwrap();
    let rendered = render_for_model(&replay).unwrap();
    assert!(rendered.contains("historical-provider/historical-model"));
    assert!(!rendered.contains("Selection:"));
    assert!(!rendered.contains("Delivery:"));
}

#[test]
fn generate_image_result_round_trip_preserves_delivery_metadata() {
    let mut result: GenerateImageResult = serde_json::from_value(historical_result()).unwrap();
    result.parameters.aspect_ratio = Some("4:3".into());
    result.parameters.resolution = Some("1k".into());
    result.selection = Some(json!({
        "requested_model": "xai@default/grok-imagine-image",
        "resolved_model": "xai@default/grok-imagine-image",
        "selection_source": "explicit",
        "contract_version": "image-generation-v1"
    }));
    result.generation_succeeded = Some(true);
    result.requested_parameters = Some(ImageGenerationParameters {
        aspect_ratio: Some("4:3".into()),
        resolution: Some("1k".into()),
        output_format: Some("png".into()),
        ..Default::default()
    });
    result.provider_parameters = Some(ImageGenerationParameters {
        aspect_ratio: Some("4:3".into()),
        resolution: Some("1k".into()),
        ..Default::default()
    });
    result.provider_effective_parameters = Some(json!({
        "aspect_ratio": {"value": "4:3", "source": "caller", "sent": true}
    }));
    result.parameter_defaults = Some(json!({
        "delivery_format": {"value": "png", "source": "caller"}
    }));
    result.source_images = vec![GeneratedImageSourceReference {
        uri: "/historical/media/generated/original.source.jpg".into(),
        sha256: "source-sha256".into(),
        byte_count: 100,
        format: Some("jpeg".into()),
        provider_reported_mime: Some("image/png".into()),
        mime: Some("image/jpeg".into()),
        width: Some(4),
        height: Some(3),
    }];
    result.conversion = vec![ImageConversionMetadata {
        source_format: "jpeg".into(),
        source_mime: "image/jpeg".into(),
        source_width: 4,
        source_height: 3,
        delivered_format: "png".into(),
        delivered_mime: "image/png".into(),
        converted: true,
        orientation_applied: false,
        encoding_policy: "png_lossless".into(),
        width: 4,
        height: 3,
    }];
    let encoded = serde_json::to_value(&result).unwrap();
    let decoded: GenerateImageResult = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, result);
    let replay = serialize_success(NAME, &decoded).unwrap();
    let rendered = render_for_model(&replay).unwrap();
    assert!(rendered.contains("xai@default/grok-imagine-image"));
    assert!(rendered.contains("png_lossless"));
    assert!(rendered.contains("original.source.jpg"));
    let schema = serde_json::to_value(schemars::schema_for!(GenerateImageResult)).unwrap();
    assert!(schema["properties"]["conversion"].is_object());
    assert!(schema["properties"]["contract"].is_object());
}

#[test]
fn generate_image_args_preserve_omission_and_route_specific_sizes() {
    let args: GenerateImageArgs = serde_json::from_value(json!({"prompt": "draw"})).unwrap();
    assert!(args.model.is_none());
    assert!(args.size.is_none());
    assert!(args.output_format.is_none());
    let args: GenerateImageArgs = serde_json::from_value(json!({
        "prompt": "draw", "model": "openai@default/gpt-image-2", "size": "1536x864",
    }))
    .unwrap();
    assert_eq!(args.size.as_deref(), Some("1536x864"));
    assert_eq!(args.model.as_deref(), Some("openai@default/gpt-image-2"));
}

fn historical_result() -> Value {
    json!({
        "images": [{
            "type": "generated_image",
            "id": "img_historical",
            "workspace_id": "agent_home:historical",
            "path": "media/generated/historical.png",
            "uri": "/historical/media/generated/historical.png",
            "sha256": "historical-sha256",
            "mime": "image/png",
            "byte_count": 123,
            "size": {"width": 4, "height": 3},
            "created_at": "2026-10-01T00:00:00Z"
        }],
        "provider": "historical-provider",
        "model": "historical-model",
        "prompt": "draw",
        "parameters": {"output_format": "png"}
    })
}
