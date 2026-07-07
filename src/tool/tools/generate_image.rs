use std::{fs, path::PathBuf};

use anyhow::{anyhow, Result};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::ProviderGenerateImageRequest,
    runtime::RuntimeHandle,
    tool::{helpers::invalid_tool_input, spec::typed_spec, ToolError, ToolResult},
    types::{
        agent_home_workspace_id, AuthorityClass, GenerateImageParameters, GenerateImageResult,
        GenerateImageSize, GeneratedImageReference, ToolCapabilityFamily,
    },
};

use super::{serialize_success, view_image::read_visual_reference, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::GENERATE_IMAGE;

const VALID_SIZES: &[&str] = &["1024x1024", "1536x1024", "1024x1536"];
const VALID_BACKGROUNDS: &[&str] = &["auto", "transparent", "opaque"];
const VALID_OUTPUT_FORMATS: &[&str] = &["png", "jpeg", "webp"];

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerateImageArgs {
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) size: Option<String>,
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
    let args: GenerateImageArgs = parse_tool_args(NAME, input)?;
    let prompt = validate_non_empty(args.prompt, NAME, "prompt")?;
    let size = optional_enum(args.size, "size", VALID_SIZES)?;
    let background = optional_enum(args.background, "background", VALID_BACKGROUNDS)?;
    let output_format = optional_enum(args.output_format, "output_format", VALID_OUTPUT_FORMATS)?;
    let name = args
        .name
        .map(|value| validate_filename_stem(&value))
        .transpose()?;

    let provider_result = runtime
        .generate_image(ProviderGenerateImageRequest {
            prompt: prompt.clone(),
            size: size.clone(),
            background: background.clone(),
            output_format: output_format.clone(),
        })
        .await
        .map_err(generate_image_failed)?;

    let generated_dir = runtime.agent_home().join("media").join("generated");
    fs::create_dir_all(&generated_dir).map_err(|error| {
        ToolError::new(
            "generated_media_write_failed",
            format!("failed to create generated media directory: {error}"),
        )
        .with_details(json!({
            "path": generated_dir,
            "io_error": error.to_string(),
        }))
        .with_recovery_hint("ensure the agent_home media directory is writable")
    })?;

    let workspace_id = agent_home_workspace_id(agent_id);
    let mut images = Vec::new();
    for (index, image) in provider_result.images.into_iter().enumerate() {
        let sha256 = format!("{:x}", Sha256::digest(&image.bytes));
        let extension =
            extension_for_mime_or_format(image.mime.as_deref(), output_format.as_deref());
        let stem = name
            .clone()
            .unwrap_or_else(|| format!("generated_{}", Utc::now().format("%Y%m%dT%H%M%S%.3fZ")));
        let filename = if index == 0 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{}.{}", index + 1, extension)
        };
        let path = generated_dir.join(filename);
        fs::write(&path, &image.bytes).map_err(|error| {
            ToolError::new(
                "generated_media_write_failed",
                format!("failed to write generated image: {error}"),
            )
            .with_details(json!({
                "path": path,
                "io_error": error.to_string(),
            }))
            .with_recovery_hint("ensure the agent_home media directory is writable")
        })?;

        let read_image = read_visual_reference(&path).map_err(|error| {
            ToolError::new(
                "generated_image_invalid",
                format!("provider returned image bytes that could not be validated: {error}"),
            )
            .with_details(json!({
                "path": path,
                "sha256": sha256,
                "error": error.to_string(),
            }))
            .with_recovery_hint("retry generation or choose a different image-generation model")
        })?;
        let reference = read_image.visual_reference;
        let relative_path = PathBuf::from("media").join("generated").join(
            path.file_name()
                .ok_or_else(|| anyhow!("generated image path has no filename"))?,
        );
        let uri = format!(
            "workspace://{}/{}",
            workspace_id,
            relative_path.to_string_lossy()
        );
        images.push(GeneratedImageReference {
            kind: "generated_image".to_string(),
            id: format!("img_{}", &reference.sha256[..16]),
            workspace_id: workspace_id.clone(),
            path: relative_path,
            uri,
            sha256: reference.sha256,
            mime: reference.mime,
            byte_count: reference.byte_count,
            size: GenerateImageSize {
                width: reference.size.width,
                height: reference.size.height,
            },
            created_at: reference.created_at,
        });
    }

    serialize_success(
        NAME,
        &GenerateImageResult {
            images,
            provider: provider_result.provider,
            model: provider_result.model,
            prompt,
            parameters: GenerateImageParameters {
                size,
                background,
                output_format,
                name,
            },
            summary_text: Some("GenerateImage created one image".to_string()),
        },
    )
}

pub(crate) fn render_for_model(result: &ToolResult) -> Result<String> {
    let value = result
        .envelope
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("GenerateImage success result missing payload"))?;
    let result: GenerateImageResult = serde_json::from_value(value.clone())?;
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
    Ok(lines.join("\n"))
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

fn extension_for_mime_or_format(mime: Option<&str>, output_format: Option<&str>) -> &'static str {
    match mime {
        Some("image/png") => "png",
        Some("image/jpeg") => "jpg",
        Some("image/webp") => "webp",
        Some("image/gif") => "gif",
        _ => match output_format {
            Some("jpeg") => "jpg",
            Some("webp") => "webp",
            _ => "png",
        },
    }
}

fn generate_image_failed(error: anyhow::Error) -> anyhow::Error {
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
