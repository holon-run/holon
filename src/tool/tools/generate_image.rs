use std::{fs, path::PathBuf};

use anyhow::{anyhow, Result};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::{
        image_delivery::{deliver_image, source_format},
        ProviderGenerateImageRequest,
    },
    runtime::RuntimeHandle,
    tool::{helpers::invalid_tool_input, spec::typed_spec, ToolError, ToolResult},
    types::{
        agent_home_workspace_id, AuthorityClass, GenerateImageParameters, GenerateImageResult,
        GenerateImageSize, GeneratedImageReference, GeneratedImageSourceReference,
        ToolCapabilityFamily,
    },
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::GENERATE_IMAGE;

#[cfg(test)]
#[path = "generate_image_material_tests.rs"]
mod material_tests;

const VALID_BACKGROUNDS: &[&str] = &["auto", "transparent", "opaque"];
const VALID_OUTPUT_FORMATS: &[&str] = &["png", "jpeg", "webp"];

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerateImageArgs {
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) size: Option<String>,
    #[serde(default)]
    pub(crate) aspect_ratio: Option<String>,
    #[serde(default)]
    pub(crate) resolution: Option<String>,
    #[serde(default)]
    pub(crate) background: Option<String>,
    #[serde(default)]
    pub(crate) output_format: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<GenerateImageArgs>(
            NAME,
            include_str!("../tool_descriptions/generate_image.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let acquisition = runtime.context_resource_material()?;
    let material = crate::provider::ProviderRequestMaterial::scoped(acquisition.clone());
    let args: GenerateImageArgs = material.read(|| {
        crate::diagnostics::resource_io::admit_serialized_value(input)?;
        parse_tool_args(NAME, input)
    })?;
    let prompt = validate_non_empty(args.prompt, NAME, "prompt")?;
    let size = args.size;
    let background = optional_enum(args.background, "background", VALID_BACKGROUNDS)?;
    let output_format = optional_enum(args.output_format, "output_format", VALID_OUTPUT_FORMATS)?;
    let name = args
        .name
        .map(|value| validate_filename_stem(&value))
        .transpose()?;

    let request = material.read(|| {
        material.admit_value(&(
            &prompt,
            &size,
            &args.aspect_ratio,
            &args.resolution,
            &background,
            &output_format,
        ))?;
        Ok(ProviderGenerateImageRequest {
            prompt: prompt.clone(),
            size: size.clone(),
            aspect_ratio: args.aspect_ratio.clone(),
            resolution: args.resolution.clone(),
            background: background.clone(),
            output_format: output_format.clone(),
            material: material.clone(),
        })
    })?;
    let execution = runtime
        .generate_image(args.model.as_deref(), request)
        .await
        .map_err(generate_image_failed)?;
    material.read(|| {
        Ok(crate::diagnostics::resource_io::charge_owned_metadata(
            4096, 32,
        )?)
    })?;
    let generated_dir = runtime.agent_home().join("media").join("generated");
    fs::create_dir_all(&generated_dir).map_err(|error| {
        ToolError::new(
            "generated_media_write_failed",
            format!("generation succeeded but media directory creation failed: {error}"),
        )
        .with_details(json!({"generation_succeeded": true, "selection": execution.selection}))
        .with_recovery_hint(
            "repair local storage; do not regenerate to fix a local delivery failure",
        )
    })?;
    let workspace_id = agent_home_workspace_id(agent_id);
    let mut images = Vec::new();
    let mut conversions = Vec::new();
    let mut sources = Vec::new();
    for image in execution.response.images {
        material.read(|| {
            crate::diagnostics::resource_io::admit_serialized_value(&(
                &generated_dir,
                &name,
                &workspace_id,
                &image.mime,
            ))?;
            crate::diagnostics::resource_io::charge_owned_metadata(4096, 32)?;
            Ok(())
        })?;
        let source_sha256 = format!("{:x}", Sha256::digest(&image.bytes));
        let source_format = source_format(&image.bytes);
        let stem = name.clone().unwrap_or_else(|| "generated".into());
        let token = uuid::Uuid::new_v4().simple().to_string();
        let source_path = generated_dir.join(format!(
            "{stem}-{token}.source.{}",
            source_format.unwrap_or("bin")
        ));
        write_generated_file(&source_path, &image.bytes).map_err(|error| {
            ToolError::new(
                "generated_media_write_failed",
                format!("image generation succeeded; original image could not be saved: {error}"),
            )
            .with_details(json!({"generation_succeeded": true, "selection": execution.selection}))
            .with_recovery_hint(
                "repair local storage; do not regenerate to fix a local write failure",
            )
        })?;
        let mut source = GeneratedImageSourceReference {
            uri: source_path.to_string_lossy().into_owned(),
            sha256: source_sha256,
            byte_count: image.bytes.len() as u64,
            format: source_format.map(str::to_owned),
            provider_reported_mime: image.mime,
            mime: None,
            width: None,
            height: None,
        };
        material.admit_value(&execution.parameters.delivery_format)?;
        let target = execution.parameters.delivery_format.clone();
        let delivery_material = material.clone();
        let (delivered, _delivery_material) = tokio::task::spawn_blocking(move || {
            match delivery_material.read(|| Ok(deliver_image(&image.bytes, &target))) {
                Ok(delivered) => Ok((delivered, delivery_material)),
                Err(error) => Err(delivery_material.hold_error(error)),
            }
        })
            .await
            .map_err(|error| ToolError::new("image_conversion_failed", format!("local delivery worker failed: {error}"))
                .with_details(json!({"generation_succeeded": true, "source": source, "selection": execution.selection}))
                .with_recovery_hint("recover the saved original locally; do not regenerate"))??;
        let delivered = delivered
            .map_err(|error| ToolError::new(error.code, format!("image generation succeeded; local delivery failed: {error}"))
                .with_details(json!({"generation_succeeded": true, "source": source, "selection": execution.selection,
                    "requested_parameters": execution.parameters.requested, "delivery_format": execution.parameters.delivery_format,
                    "conversion": {"status": "failed", "code": error.code, "message": error.message}}))
                .with_recovery_hint("use the saved original or convert it locally with safe settings; do not regenerate"))?;
        let path = if delivered.conversion.converted {
            let path = generated_dir.join(format!("{stem}-{token}.{}", delivered.extension));
            write_generated_file(&path, &delivered.bytes).map_err(|error| {
                ToolError::new(
                    "generated_media_write_failed",
                    format!("image generation succeeded; final file write failed: {error}"),
                )
                .with_details(json!({"generation_succeeded": true, "source": source}))
                .with_recovery_hint("recover the saved original locally; do not regenerate")
            })?;
            path
        } else {
            source_path
        };
        let sha256 = format!("{:x}", Sha256::digest(&delivered.bytes));
        let relative_path = PathBuf::from("media").join("generated").join(
            path.file_name()
                .ok_or_else(|| anyhow!("generated image path has no filename"))?,
        );
        images.push(GeneratedImageReference {
            kind: "generated_image".into(),
            id: format!("img_{}", &sha256[..16]),
            workspace_id: workspace_id.clone(),
            path: relative_path,
            uri: path.to_string_lossy().into_owned(),
            sha256,
            mime: delivered.mime.into(),
            byte_count: delivered.bytes.len() as u64,
            size: GenerateImageSize {
                width: Some(delivered.conversion.width),
                height: Some(delivered.conversion.height),
            },
            created_at: Utc::now(),
        });
        source.mime = Some(delivered.conversion.source_mime.clone());
        source.width = Some(delivered.conversion.source_width);
        source.height = Some(delivered.conversion.source_height);
        conversions.push(delivered.conversion);
        sources.push(source);
    }
    let payload = GenerateImageResult {
        images,
        provider: execution.response.provider,
        model: execution.response.model,
        prompt,
        parameters: GenerateImageParameters {
            size,
            aspect_ratio: args.aspect_ratio,
            resolution: args.resolution,
            background,
            output_format,
            name,
        },
        selection: Some(execution.selection),
        generation_succeeded: Some(true),
        requested_parameters: Some(execution.parameters.requested),
        provider_parameters: Some(execution.parameters.provider_parameters),
        provider_effective_parameters: Some(execution.parameters.effective_parameters),
        parameter_defaults: Some(execution.parameters.defaults),
        parameter_mapping: (!execution.parameters.mapping.is_null())
            .then_some(execution.parameters.mapping),
        contract: Some(execution.contract),
        source_images: sources,
        conversion: conversions,
        summary_text: Some("GenerateImage created one image; original retained".into()),
    };
    material.read(|| {
        material.admit_value(&payload)?;
        let mut result = serialize_success(NAME, &payload)?;
        result.resource_material = Some(acquisition);
        Ok(result)
    })
}

fn write_generated_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

pub(crate) fn render_for_model(result: &ToolResult) -> Result<String> {
    let material = result
        .resource_material
        .clone()
        .map(crate::provider::ProviderRequestMaterial::scoped)
        .unwrap_or_default();
    material.read(|| {
        let value = result
            .envelope
            .result
            .as_ref()
            .ok_or_else(|| anyhow!("GenerateImage success result missing payload"))?;
        crate::diagnostics::resource_io::admit_serialized_value(value)?;
        let result = GenerateImageResult::deserialize(value)?;
        crate::diagnostics::resource_io::admit_serialized_value(&result)?;
        let count = result
            .images
            .len()
            .checked_add(1)
            .ok_or_else(|| anyhow!("image reference count overflow"))?;
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(count)?
                .checked_mul(512)
                .ok_or_else(|| anyhow!("image reference size overflow"))?,
            count
                .checked_mul(8)
                .ok_or_else(|| anyhow!("image reference count overflow"))?,
        )?;
        let mut lines = vec![
            "GenerateImage result".to_string(),
            format!("Provider/model: {}/{}", result.provider, result.model),
            format!("Prompt: {}", result.prompt),
        ];
        for image in result.images {
            lines.push(format!(
                "Image: {} ({}, {} bytes, sha256 {}, uri {})",
                image.id, image.mime, image.byte_count, image.sha256, image.uri
            ));
            lines.push(format!(
                "Size: {}",
                match (image.size.width, image.size.height) {
                    (Some(width), Some(height)) => format!("{width}x{height}"),
                    (Some(width), None) => format!("{width}xunknown"),
                    (None, Some(height)) => format!("unknownx{height}"),
                    (None, None) => "unknown".to_string(),
                }
            ));
        }
        if let Some(selection) = value.get("selection") {
            lines.push(format!("Selection: {selection}"));
        }
        if let Some(conversion) = value.get("conversion") {
            lines.push(format!("Delivery: {conversion}"));
        }
        if let Some(sources) = value.get("source_images") {
            lines.push(format!("Retained originals: {sources}"));
        }
        if let Some(mapping) = value
            .get("parameter_mapping")
            .filter(|mapping| !mapping.is_null())
        {
            lines.push(format!(
                "Size compatibility mapping (not exact pixels): {mapping}"
            ));
        }
        crate::diagnostics::resource_io::admit_serialized_value(&lines)?;
        Ok(lines.join("\n"))
    })
}

fn optional_enum(value: Option<String>, field: &str, allowed: &[&str]) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = validate_non_empty(value, NAME, field)?;
    if allowed.contains(&value.as_str()) {
        return Ok(Some(value));
    }
    Err(invalid_tool_input(
        NAME,
        format!("GenerateImage `{field}` is not supported"),
        json!({
            "field": field,
            "value": value,
            "allowed_values": allowed,
        }),
        "use one of the supported values from the tool schema",
    ))
}

fn validate_filename_stem(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_tool_input(
            NAME,
            "GenerateImage `name` must be non-empty when provided",
            json!({"field": "name"}),
            "omit `name` or provide a simple filename stem",
        ));
    }
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('_').to_string();
    if sanitized.is_empty() {
        return Err(invalid_tool_input(
            NAME,
            "GenerateImage `name` must contain letters, numbers, '-' or '_'",
            json!({"field": "name", "value": value}),
            "provide a simple filename stem such as `logo_sketch`",
        ));
    }
    Ok(sanitized)
}

fn generate_image_failed(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<ToolError>().is_some() {
        return error;
    }
    ToolError::new(
        "image_generation_failed",
        format!("GenerateImage could not create an image: {error}"),
    )
    .with_details(json!({
        "error": error.to_string(),
    }))
    .with_recovery_hint(
        "configure an OpenAI image-generation model with valid credentials, or retry after provider failures are resolved",
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::provider::{
            image_delivery::ImageConversionMetadata, image_generation::ImageGenerationParameters,
        };

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

    #[test]
    fn generate_image_original_survives_failed_local_delivery_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("retained.source.bin");
        let bytes = b"unknown or damaged provider output";
        write_generated_file(&original, bytes).unwrap();
        let failure = deliver_image(bytes, "png").unwrap_err();
        assert_eq!(failure.code, "unsupported_source_image_format");
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert!(write_generated_file(&original, b"must not replace").is_err());
        assert_eq!(fs::read(&original).unwrap(), bytes);
    }

    #[test]
    fn generate_image_preserves_structured_preflight_errors() {
        let error: anyhow::Error =
            ToolError::new("unsupported_image_parameter", "unsupported background").into();
        let error = generate_image_failed(error);
        assert_eq!(
            error.downcast_ref::<ToolError>().unwrap().kind,
            "unsupported_image_parameter"
        );
    }
}
