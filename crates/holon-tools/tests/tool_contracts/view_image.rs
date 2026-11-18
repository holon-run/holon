use anyhow::anyhow;
use chrono::Utc;
use holon_domain::{
    ViewImageGeneratedBy, ViewImageObservation, ViewImageReferenceSize, ViewImageResult,
    ViewImageSelectedMode, ViewImageVisionSelection, ViewImageVisualReference,
};
use holon_tools::builtins::view_image::*;
use holon_tools::ToolResult;
use serde_json::json;
use std::path::Path;

#[test]
fn parses_png_header_dimensions() {
    let bytes = png_header_bytes(640, 480);

    let header = ImageHeader::parse(&bytes).unwrap();

    assert_eq!(header.media_type, "image/png");
    assert_eq!(header.width, Some(640));
    assert_eq!(header.height, Some(480));
}

#[test]
fn parses_jpeg_sof_dimensions() {
    let bytes = [
        0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x01, 0x2c,
        0x02, 0x80, 0x03, 0x01, 0x11, 0x00,
    ];

    let header = ImageHeader::parse(&bytes).unwrap();

    assert_eq!(header.media_type, "image/jpeg");
    assert_eq!(header.width, Some(640));
    assert_eq!(header.height, Some(300));
}

#[test]
fn parses_visual_observation_json_with_trusted_metadata() {
    let reference = test_visual_reference();
    let selection = test_vision_selection();

    let observation = parse_visual_observation(
        r#"{
            "type": "visual_observation",
            "schema": "visual_observation.v1",
            "visual_reference_id": "provider_supplied",
            "prompt": "provider supplied",
            "summary": "A red square is visible.",
            "ocr": [{"text": "OK"}],
            "uncertainties": ["small text is unclear"]
        }"#,
        &reference,
        "Inspect visible content.",
        &selection,
    )
    .unwrap();

    assert_eq!(observation.kind, "visual_observation");
    assert_eq!(observation.schema, "visual_observation.v1");
    assert_eq!(observation.visual_reference_id, reference.id);
    assert_eq!(observation.prompt, "Inspect visible content.");
    assert_eq!(observation.generated_by.provider.as_deref(), Some("openai"));
    assert_eq!(observation.summary, "A red square is visible.");
    assert_eq!(observation.ocr.len(), 1);
    assert_eq!(
        observation.uncertainties,
        vec!["small text is unclear".to_string()]
    );
    assert!(observation.elements.is_empty());
}

#[test]
fn normalizes_object_uncertainties_from_vision_adapter() {
    let observation = parse_visual_observation(
        r#"{
            "type": "visual_observation",
            "schema": "visual_observation.v1",
            "summary": "A logo is visible.",
            "uncertainties": [
                {"text": "The inner shape may be stylized."},
                {"description": "Small text is hard to read."}
            ]
        }"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap();

    assert_eq!(
        observation.uncertainties,
        vec![
            "The inner shape may be stylized.".to_string(),
            "Small text is hard to read.".to_string()
        ]
    );
}

#[test]
fn normalizes_non_array_optional_fields_from_vision_adapter() {
    let observation = parse_visual_observation(
        r#"{
            "type": "visual_observation",
            "schema": "visual_observation.v1",
            "summary": "A logo is visible.",
            "ocr": "HOLON",
            "elements": {"type": "logo"},
            "relations": "The logo is above the wordmark.",
            "issues": null,
            "uncertainties": {"message": "Small details may be unclear."},
            "external_sources": 42
        }"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap();

    assert_eq!(observation.ocr, vec![json!({"text": "HOLON"})]);
    assert_eq!(observation.elements, vec![json!({"type": "logo"})]);
    assert_eq!(
        observation.relations,
        vec![json!({"description": "The logo is above the wordmark."})]
    );
    assert!(observation.issues.is_empty());
    assert_eq!(
        observation.uncertainties,
        vec!["Small details may be unclear.".to_string()]
    );
    assert!(observation.external_sources.is_empty());
}

#[test]
fn ignores_unusable_optional_entries_without_hiding_invalid_core_fields() {
    let observation = parse_visual_observation(
        r#"{
            "type": "visual_observation",
            "schema": "visual_observation.v1",
            "summary": "A logo is visible.",
            "ocr": false,
            "uncertainties": [null, 12, {}, "", {"text": "  "}]
        }"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap();

    assert!(observation.ocr.is_empty());
    assert!(observation.uncertainties.is_empty());

    let error = parse_visual_observation(
        r#"{
            "type": "visual_observation",
            "schema": "visual_observation.v1",
            "summary": "",
            "ocr": "HOLON"
        }"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("non-empty `summary`"));
}

#[test]
fn parses_visual_observation_json_from_markdown_fence() {
    let observation = parse_visual_observation(
        "```json\n{\"type\":\"visual_observation\",\"schema\":\"visual_observation.v1\",\"summary\":\"A chart is visible.\"}\n```",
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap();

    assert_eq!(observation.summary, "A chart is visible.");
}

#[test]
fn rejects_malformed_visual_observation_json() {
    let error = parse_visual_observation(
        "not json",
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("did not contain JSON"));
}

#[test]
fn normalizes_missing_or_unknown_schema_from_visual_observation() {
    let missing_schema = parse_visual_observation(
        r#"{"type":"visual_observation","summary":"x"}"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap();
    let unknown_schema = parse_visual_observation(
        r#"{"type":"visual_observation","schema":"visual_observation.v2","summary":"x"}"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap();

    assert_eq!(missing_schema.schema, VISUAL_OBSERVATION_SCHEMA);
    assert_eq!(unknown_schema.schema, VISUAL_OBSERVATION_SCHEMA);
}

#[test]
fn rejects_missing_visual_observation_type_but_accepts_missing_schema() {
    let error = parse_visual_observation(
        r#"{"schema":"visual_observation.v1","summary":"x"}"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("`type`"));

    let observation = parse_visual_observation(
        r#"{"type":"visual_observation","summary":"x"}"#,
        &test_visual_reference(),
        "Describe the image.",
        &test_vision_selection(),
    )
    .unwrap();

    assert_eq!(observation.schema, VISUAL_OBSERVATION_SCHEMA);
}

#[test]
fn bounds_response_preview_and_removes_control_characters() {
    let preview = bounded_response_preview(&format!(
        "a\n{}",
        "x".repeat(VIEW_IMAGE_RESPONSE_PREVIEW_MAX_CHARS)
    ));

    assert!(!preview.contains('\n'));
    assert!(preview.ends_with('…'));
    assert!(preview.chars().count() <= VIEW_IMAGE_RESPONSE_PREVIEW_MAX_CHARS + 1);
}

#[tokio::test]
async fn retries_once_after_response_validation_failure() {
    let mut prompts = Vec::new();
    let mut responses = vec![
        "not valid visual observation".to_string(),
        r#"{"type":"visual_observation","summary":"A chart is visible."}"#.to_string(),
    ];

    let observation = generate_and_parse_view_image_observation(
        "Describe the image.",
        &test_visual_reference(),
        &test_vision_selection(),
        |request_prompt| {
            prompts.push(request_prompt);
            let response = responses.remove(0);
            async move { Ok(response) }
        },
    )
    .await
    .unwrap();

    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0], "Describe the image.");
    assert!(prompts[1].contains(VIEW_IMAGE_VALIDATION_RETRY_PROMPT));
    assert!(!prompts[1].contains("not valid visual observation"));
    assert_eq!(observation.summary, "A chart is visible.");
    assert_eq!(observation.schema, VISUAL_OBSERVATION_SCHEMA);
}

#[tokio::test]
async fn does_not_retry_provider_failure() {
    let mut call_count = 0;

    let error = generate_and_parse_view_image_observation(
        "Describe the image.",
        &test_visual_reference(),
        &test_vision_selection(),
        |_request_prompt| {
            call_count += 1;
            async { Err(anyhow!("provider unavailable")) }
        },
    )
    .await
    .unwrap_err();

    assert_eq!(call_count, 1);
    assert!(matches!(
        error,
        ViewImageObservationGenerationError::Provider {
            initial_validation_error: None,
            ..
        }
    ));
}

#[tokio::test]
async fn stops_after_one_correction_retry() {
    let mut call_count = 0;

    let error = generate_and_parse_view_image_observation(
        "Describe the image.",
        &test_visual_reference(),
        &test_vision_selection(),
        |_request_prompt| {
            call_count += 1;
            async { Ok("still not valid visual observation".to_string()) }
        },
    )
    .await
    .unwrap_err();

    assert_eq!(call_count, 2);
    assert!(matches!(
        error,
        ViewImageObservationGenerationError::Validation { .. }
    ));
}

#[test]
fn view_image_model_is_optional_and_old_selection_deserializes() {
    let args: ViewImageArgs =
        serde_json::from_value(json!({"path": "image.png", "prompt": "Inspect"})).unwrap();
    assert!(args.model.is_none());
    let args: ViewImageArgs = serde_json::from_value(
        json!({"path": "image.png", "prompt": "Inspect", "model": "openai/gpt-5.4"}),
    )
    .unwrap();
    assert_eq!(args.model.as_deref(), Some("openai/gpt-5.4"));
    let old: ViewImageVisionSelection = serde_json::from_value(json!({
        "selected_mode": "vision_adapter",
        "vision_provider": "openai",
        "vision_model": "gpt-5.4",
        "selection_reason": "old-result",
    }))
    .unwrap();
    assert!(old.requested_model.is_none());
    assert!(old.resolved_model.is_none());
    assert!(old.contract_version.is_none());
}

#[test]
fn renders_visual_observation_for_text_only_replay() {
    let mut reference = test_visual_reference();
    reference.workspace_uri = Some("/tmp/image.png".to_string());
    let selection = test_vision_selection();
    let result = ViewImageResult {
        cache_hit: false,
        visual_reference: reference.clone(),
        observation: ViewImageObservation {
            kind: "visual_observation".to_string(),
            schema: "visual_observation.v1".to_string(),
            visual_reference_id: reference.id.clone(),
            prompt: "What is visible?".to_string(),
            generated_by: ViewImageGeneratedBy {
                resolved_model: selection.resolved_model.clone(),
                mode: selection.selected_mode.clone(),
                provider: selection.vision_provider.clone(),
                model: selection.vision_model.clone(),
                selection_reason: Some(selection.selection_reason.clone()),
            },
            summary: "A red warning icon is visible.".to_string(),
            ocr: vec![json!({"text": "WARN"})],
            elements: vec![json!({"kind": "icon", "color": "red"})],
            relations: Vec::new(),
            issues: Vec::new(),
            uncertainties: vec!["small text is unclear".to_string()],
            external_sources: Vec::new(),
        },
        selected_mode: selection.selected_mode.clone(),
        vision_selection: selection,
        summary_text: Some("generated observation".to_string()),
    };
    let value = serde_json::to_value(result).unwrap();
    let tool_result = ToolResult::success(NAME, value.clone(), None);
    let rendered = render_for_model(&tool_result).unwrap();
    let mut legacy = value;
    legacy.as_object_mut().unwrap().remove("cache_hit");
    let selection = legacy["vision_selection"].as_object_mut().unwrap();
    for field in [
        "requested_model",
        "resolved_model",
        "selection_source",
        "contract_version",
    ] {
        selection.remove(field);
    }
    legacy["observation"]["generated_by"]
        .as_object_mut()
        .unwrap()
        .remove("resolved_model");
    let legacy_result: ViewImageResult = serde_json::from_value(legacy.clone()).unwrap();
    assert!(!legacy_result.cache_hit);
    assert!(legacy_result
        .observation
        .generated_by
        .resolved_model
        .is_none());
    let legacy_tool_result = ToolResult::success(NAME, legacy, None);
    assert_eq!(render_for_model(&legacy_tool_result).unwrap(), rendered);

    assert!(rendered.starts_with("ViewImage visual observation"));
    assert!(rendered.contains("Reference: vis_test"));
    assert!(rendered.contains("Absolute path: /tmp/image.png"));
    assert!(!rendered.contains("Workspace URI:"));
    assert!(rendered.contains("Prompt: What is visible?"));
    assert!(rendered.contains("Summary: A red warning icon is visible."));
    assert!(rendered.contains(r#""text":"WARN""#));
    assert!(rendered.contains("Uncertainties: small text is unclear"));
    assert!(!rendered.contains("\"tool_name\":\"ViewImage\""));
    assert!(!rendered.contains("data_base64"));
    assert!(!rendered.contains("input_image"));
}

#[test]
fn rejects_jpeg_without_sof_dimensions() {
    let bytes = [0xff, 0xd8, 0xff, 0xd9];

    assert!(ImageHeader::parse(&bytes).is_none());
}

#[test]
fn rejects_webp_without_recognized_dimensions() {
    let mut bytes = b"RIFF\x1e\0\0\0WEBPUNKN".to_vec();
    bytes.resize(30, 0);

    assert!(ImageHeader::parse(&bytes).is_none());
}

fn png_header_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

fn test_visual_reference() -> ViewImageVisualReference {
    ViewImageVisualReference {
        kind: "visual_reference".to_string(),
        id: "vis_test".to_string(),
        path: Path::new("image.png").to_path_buf(),
        workspace_uri: None,
        sha256: "sha256".to_string(),
        mime: "image/png".to_string(),
        byte_count: 10,
        size: ViewImageReferenceSize {
            width: Some(1),
            height: Some(1),
        },
        created_at: Utc::now(),
    }
}

fn test_vision_selection() -> ViewImageVisionSelection {
    ViewImageVisionSelection {
        requested_model: None,
        resolved_model: Some("openai@default/gpt-5.4".to_string()),
        selection_source: Some("automatic".to_string()),
        contract_version: Some("view_image.v2".to_string()),
        selected_mode: ViewImageSelectedMode::VisionAdapter,
        vision_provider: Some("openai".to_string()),
        vision_model: Some("gpt-5.4".to_string()),
        selection_reason: "test".to_string(),
        primary_provider: Some("openai".to_string()),
        primary_model: Some("gpt-5.4".to_string()),
        candidates: Vec::new(),
    }
}
