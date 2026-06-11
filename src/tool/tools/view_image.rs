use std::{fs, path::Path};

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    runtime::RuntimeHandle,
    system::ExecutionScopeKind,
    tool::{helpers::invalid_tool_input, spec::typed_spec, ToolError},
    types::{
        AuthorityClass, ToolCapabilityFamily, ViewImageMetadata, ViewImageResult, ViewImageStatus,
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
    let metadata = read_image_metadata(&resolved_path)?;
    let observation =
        "visual observation unavailable: provider-native image observation is not implemented yet"
            .to_string();
    let summary_text = Some(format!(
        "ViewImage recorded {} ({} bytes); visual observation unavailable",
        metadata.media_type, metadata.byte_count
    ));
    serialize_success(
        NAME,
        &ViewImageResult {
            status: ViewImageStatus::Unavailable,
            path,
            resolved_path,
            prompt: Some(prompt),
            metadata,
            observation,
            summary_text,
        },
    )
}

fn read_image_metadata(path: &Path) -> Result<ViewImageMetadata> {
    let file_metadata = fs::metadata(path).map_err(|error| {
        ToolError::new(
            "image_read_failed",
            format!("failed to inspect image file: {error}"),
        )
        .with_details(json!({
            "path": path,
            "io_error": error.to_string(),
        }))
        .with_recovery_hint("provide a readable local image file path")
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
    let bytes = fs::read(path).map_err(|error| {
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
    Ok(ViewImageMetadata {
        media_type: header.media_type.to_string(),
        byte_count,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        width: header.width,
        height: header.height,
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
        _ => Some(ImageHeader {
            media_type: "image/webp",
            width: None,
            height: None,
        }),
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
    Some(ImageHeader {
        media_type: "image/jpeg",
        width: None,
        height: None,
    })
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
    fn reads_image_metadata() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("image.png");
        let bytes = png_header_bytes(320, 240);
        fs::write(&path, &bytes).unwrap();

        let metadata = read_image_metadata(&path).unwrap();

        assert_eq!(metadata.media_type, "image/png");
        assert_eq!(metadata.byte_count, bytes.len() as u64);
        assert_eq!(metadata.width, Some(320));
        assert_eq!(metadata.height, Some(240));
        assert_eq!(metadata.sha256, format!("{:x}", Sha256::digest(&bytes)));
    }

    #[test]
    fn rejects_non_image_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("not-image.txt");
        fs::write(&path, b"hello").unwrap();

        let error = ToolError::from_anyhow(&read_image_metadata(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("supported image file"));
    }

    #[test]
    fn rejects_oversized_file_before_reading_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.png");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();

        let error = ToolError::from_anyhow(&read_image_metadata(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("file size"));
    }

    #[test]
    fn rejects_oversized_decoded_pixel_count() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("huge.png");
        fs::write(&path, png_header_bytes(100_000, 100_000)).unwrap();

        let error = ToolError::from_anyhow(&read_image_metadata(&path).unwrap_err());

        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error.message.contains("pixel count"));
    }

    fn png_header_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }
}
