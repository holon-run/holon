use super::BuiltinToolDefinition;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::validate_non_empty;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::anyhow;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::fs;

pub const NAME: &str = crate::tool::names::GENERATE_IMAGE;

pub const VALID_BACKGROUNDS: &[&str] = &["auto", "transparent", "opaque"];

pub const VALID_OUTPUT_FORMATS: &[&str] = &["png", "jpeg", "webp"];

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerateImageArgs {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub aspect_ratio: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<GenerateImageArgs>(
            NAME,
            include_str!("../tool_descriptions/generate_image.md"),
        )?,
    })
}

pub fn write_generated_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

pub fn optional_enum(
    value: Option<String>,
    field: &str,
    allowed: &[&str],
) -> Result<Option<String>> {
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

pub fn validate_filename_stem(value: &str) -> Result<String> {
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

pub fn render_for_model(result: &crate::spec::ToolOutcome<impl Default>) -> Result<String> {
    let value = result
        .envelope
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("GenerateImage success result missing payload"))?;
    holon_system::resource_material::admit_serialized_value(value)?;
    let result = crate::types::GenerateImageResult::deserialize(value)?;
    holon_system::resource_material::admit_serialized_value(&result)?;
    let count = result
        .images
        .len()
        .checked_add(1)
        .ok_or_else(|| anyhow!("image reference count overflow"))?;
    holon_system::resource_material::charge_owned_metadata(
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
    holon_system::resource_material::admit_serialized_value(&lines)?;
    Ok(lines.join("\n"))
}
