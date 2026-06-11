use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

use anyhow::Result;
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    runtime::RuntimeHandle,
    system::ExecutionScopeKind,
    tool::{helpers::invalid_tool_input, spec::typed_spec, ToolError},
    types::{
        AuthorityClass, ToolCapabilityFamily, ViewImageGeneratedBy, ViewImageObservation,
        ViewImageReferenceSize, ViewImageResult, ViewImageSelectedMode, ViewImageVisionSelection,
        ViewImageVisualReference,
    },
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "ViewImage";
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 50_000_000;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewImageArgs {
    pub(crate) path: String,
    pub(crate) prompt: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<ViewImageArgs>(
            NAME,
            include_str!("../tool_descriptions/view_image.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ViewImageArgs = parse_tool_args(NAME, input)?;
    let path = validate_non_empty(args.path, NAME, "path")?;
    let prompt = validate_non_empty(args.prompt, NAME, "prompt")?;
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    let resolved_path = execution.workspace.resolve_read_path(&path)?;
    let visual_reference = read_visual_reference(&resolved_path)?;
    let vision_selection = runtime.current_view_image_vision_selection().await?;
    if vision_selection.selected_mode == ViewImageSelectedMode::Unavailable {
        return Err(vision_adapter_unavailable(vision_selection));
    }
    let observation = ViewImageObservation {
        kind: "visual_observation".to_string(),
        schema: "visual_observation.v1".to_string(),
        visual_reference_id: visual_reference.id.clone(),
        prompt,
        generated_by: ViewImageGeneratedBy {
            mode: vision_selection.selected_mode.clone(),
            provider: vision_selection.vision_provider.clone(),
            model: vision_selection.vision_model.clone(),
            selection_reason: Some(vision_selection.selection_reason.clone()),
        },
        summary:
            "visual observation unavailable: provider-native image observation is not implemented yet"
                .to_string(),
        ocr: Vec::new(),
        elements: Vec::new(),
        relations: Vec::new(),
        issues: Vec::new(),
        uncertainties: vec![
            "ViewImage currently records image metadata only; visual observation generation is not implemented yet."
                .to_string(),
        ],
        external_sources: Vec::new(),
    };
    let summary_text = Some(format!(
        "ViewImage selected {}/{} for {}; visual observation generation is not implemented yet",
        vision_selection
            .vision_provider
            .as_deref()
            .unwrap_or("unknown-provider"),
        vision_selection
            .vision_model
            .as_deref()
            .unwrap_or("unknown-model"),
        visual_reference.mime
    ));
    serialize_success(
        NAME,
        &ViewImageResult {
            visual_reference,
            observation,
            selected_mode: vision_selection.selected_mode.clone(),
            vision_selection,
            summary_text,
        },
    )
}

fn vision_adapter_unavailable(selection: ViewImageVisionSelection) -> anyhow::Error {
    ToolError::new(
        "vision_adapter_unavailable",
        "ViewImage requires a model with image input support, but no configured provider/model advertises image_input.",
    )
    .with_details(json!({
        "selected_mode": selection.selected_mode,
        "selection_reason": selection.selection_reason,
        "primary_provider": selection.primary_provider,
        "primary_model": selection.primary_model,
        "candidates": selection.candidates,
    }))
    .with_recovery_hint("configure a primary or fallback model whose metadata advertises image_input")
    .into()
}

fn read_visual_reference(path: &Path) -> Result<ViewImageVisualReference> {
    let file_metadata = fs::metadata(path).map_err(|error| {
        invalid_tool_input(
            NAME,
            "ViewImage `path` must refer to a readable local image file",
            json!({
                "field": "path",
                "path": path,
                "io_error": error.to_string(),
            }),
            "provide a readable PNG, JPEG, GIF, or WebP image file path",
        )
    })?;
    if !file_metadata.is_file() {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` must refer to a regular file",
            json!({
                "field": "path",
                "path": path,
                "validation_error": "not a regular file",
            }),
            "provide a path to a readable PNG, JPEG, GIF, or WebP image file",
        ));
    }
    let byte_count = file_metadata.len();
    if byte_count > MAX_IMAGE_BYTES {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` exceeds the maximum supported image file size",
            json!({
                "field": "path",
                "path": path,
                "byte_count": byte_count,
                "max_byte_count": MAX_IMAGE_BYTES,
            }),
            "provide a smaller local image file",
        ));
    }
    let mut file = File::open(path).map_err(|error| {
        ToolError::new(
            "image_read_failed",
            format!("failed to read image file: {error}"),
        )
        .with_details(json!({
            "path": path,
            "io_error": error.to_string(),
        }))
        .with_recovery_hint("provide a readable local image file path")
    })?;
    let mut bytes = Vec::with_capacity(byte_count as usize);
    file.by_ref()
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            ToolError::new(
                "image_read_failed",
                format!("failed to read image file: {error}"),
            )
            .with_details(json!({
                "path": path,
                "io_error": error.to_string(),
            }))
            .with_recovery_hint("provide a readable local image file path")
        })?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` exceeds the maximum supported image file size",
            json!({
                "field": "path",
                "path": path,
                "byte_count": bytes.len(),
                "max_byte_count": MAX_IMAGE_BYTES,
            }),
            "provide a smaller local image file",
        ));
    }
    let Some(header) = ImageHeader::parse(&bytes) else {
        return Err(invalid_tool_input(
            NAME,
            "ViewImage `path` must refer to a supported image file",
            json!({
                "field": "path",
                "path": path,
                "validation_error": "unsupported image header",
                "supported_media_types": ["image/png", "image/jpeg", "image/gif", "image/webp"],
            }),
            "provide a PNG, JPEG, GIF, or WebP image file",
        ));
    };
    if let (Some(width), Some(height)) = (header.width, header.height) {
        let pixels = u64::from(width) * u64::from(height);
        if pixels > MAX_IMAGE_PIXELS {
            return Err(invalid_tool_input(
                NAME,
                "ViewImage `path` exceeds the maximum supported decoded pixel count",
                json!({
                    "field": "path",
                    "path": path,
                    "width": width,
                    "height": height,
                    "pixel_count": pixels,
                    "max_pixel_count": MAX_IMAGE_PIXELS,
                }),
                "provide an image with fewer decoded pixels",
            ));
        }
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    Ok(ViewImageVisualReference {
        kind: "visual_reference".to_string(),
        id: format!("vis_{}", &sha256[..16]),
        path: path.to_path_buf(),
        sha256,
        mime: header.media_type.to_string(),
        byte_count: bytes.len() as u64,
        size: ViewImageReferenceSize {
            width: header.width,
            height: header.height,
        },
        created_at: Utc::now(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageHeader {
    media_type: &'static str,
    width: Option<u32>,
    height: Option<u32>,
}

impl ImageHeader {
    fn parse(bytes: &[u8]) -> Option<Self> {
        parse_png(bytes)
            .or_else(|| parse_gif(bytes))
            .or_else(|| parse_webp(bytes))
            .or_else(|| parse_jpeg(bytes))
    }
}

fn parse_png(bytes: &[u8]) -> Option<ImageHeader> {
    if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some(ImageHeader {
        media_type: "image/png",
        width: Some(u32::from_be_bytes(bytes[16..20].try_into().ok()?)),
        height: Some(u32::from_be_bytes(bytes[20..24].try_into().ok()?)),
    })
}

fn parse_gif(bytes: &[u8]) -> Option<ImageHeader> {
    if bytes.len() < 10 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    Some(ImageHeader {
        media_type: "image/gif",
        width: Some(u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32),
        height: Some(u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32),
    })
}

fn parse_webp(bytes: &[u8]) -> Option<ImageHeader> {
    if bytes.len() < 30 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WEBP" {
        return None;
    }
    match &bytes[12..16] {
        b"VP8 " if bytes.len() >= 30 && &bytes[23..26] == b"\x9d\x01\x2a" => {
            let width = u16::from_le_bytes(bytes[26..28].try_into().ok()?) & 0x3fff;
            let height = u16::from_le_bytes(bytes[28..30].try_into().ok()?) & 0x3fff;
            Some(ImageHeader {
                media_type: "image/webp",
                width: Some(width as u32),
                height: Some(height as u32),
            })
        }
        b"VP8L" if bytes.len() >= 25 => {
            let packed = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
            Some(ImageHeader {
                media_type: "image/webp",
                width: Some((packed & 0x3fff) + 1),
                height: Some(((packed >> 14) & 0x3fff) + 1),
            })
        }
        b"VP8X" if bytes.len() >= 30 => Some(ImageHeader {
            media_type: "image/webp",
            width: Some(read_u24_le(&bytes[24..27]) + 1),
            height: Some(read_u24_le(&bytes[27..30]) + 1),
        }),
        _ => None,
    }
}

fn read_u24_le(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

fn parse_jpeg(bytes: &[u8]) -> Option<ImageHeader> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut offset = 2usize;
    while offset + 4 <= bytes.len() {
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        if offset >= bytes.len() {
            break;
        }
        let marker = bytes[offset];
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if offset + 2 > bytes.len() {
            break;
        }
        let segment_len = u16::from_be_bytes(bytes[offset..offset + 2].try_into().ok()?) as usize;
        if segment_len < 2 || offset + segment_len > bytes.len() {
            break;
        }
        if is_jpeg_sof_marker(marker) && segment_len >= 7 {
            return Some(ImageHeader {
                media_type: "image/jpeg",
                height: Some(
                    u16::from_be_bytes(bytes[offset + 3..offset + 5].try_into().ok()?) as u32,
                ),
                width: Some(
                    u16::from_be_bytes(bytes[offset + 5..offset + 7].try_into().ok()?) as u32,
                ),
            });
        }
        offset += segment_len;
    }
    None
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

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
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x01,
            0x2c, 0x02, 0x80, 0x03, 0x01, 0x11, 0x00,
        ];

        let header = ImageHeader::parse(&bytes).unwrap();

        assert_eq!(header.media_type, "image/jpeg");
        assert_eq!(header.width, Some(640));
        assert_eq!(header.height, Some(300));
    }

    #[test]
    fn reads_visual_reference() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("image.png");
        let bytes = png_header_bytes(320, 240);
        fs::write(&path, &bytes).unwrap();

        let reference = read_visual_reference(&path).unwrap();

        assert_eq!(reference.kind, "visual_reference");
        assert_eq!(reference.mime, "image/png");
        assert_eq!(reference.byte_count, bytes.len() as u64);
        assert_eq!(reference.size.width, Some(320));
        assert_eq!(reference.size.height, Some(240));
        assert_eq!(reference.sha256, format!("{:x}", Sha256::digest(&bytes)));
        assert!(reference.id.starts_with("vis_"));
    }

    #[test]
    fn rejects_missing_file_as_invalid_tool_input() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.png");

        let error = ToolError::from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("readable local image file"));
    }

    #[test]
    fn rejects_non_image_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("not-image.txt");
        fs::write(&path, b"hello").unwrap();

        let error = ToolError::from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("supported image file"));
    }

    #[test]
    fn rejects_oversized_file_before_reading_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.png");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();

        let error = ToolError::from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("file size"));
    }

    #[test]
    fn rejects_oversized_decoded_pixel_count() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("huge.png");
        fs::write(&path, png_header_bytes(100_000, 100_000)).unwrap();

        let error = ToolError::from_anyhow(&read_visual_reference(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("pixel count"));
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
}
