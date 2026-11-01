//! Bounded, in-process PNG/JPEG/WebP delivery. No resizing or background synthesis.
use std::io::{self, Cursor, Seek, SeekFrom, Write};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diagnostics::resource_io;

pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 16_777_216;
const MAX_IMAGE_EDGE: u32 = 8192;
const MAX_IMAGE_ALLOCATION: u64 = 192 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct ImageDeliveryError {
    pub code: &'static str,
    pub message: String,
}

fn decode_failure(error: image::ImageError) -> ImageDeliveryError {
    let code = if matches!(error, image::ImageError::Limits(_)) {
        "image_resource_limit"
    } else {
        "image_decode_failed"
    };
    failure(code, error.to_string())
}

fn admission_failure(error: io::Error) -> ImageDeliveryError {
    failure("image_resource_limit", error.to_string())
}

fn failure(code: &'static str, message: impl Into<String>) -> ImageDeliveryError {
    ImageDeliveryError {
        code,
        message: message.into(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageConversionMetadata {
    pub source_format: String,
    pub source_mime: String,
    pub source_width: u32,
    pub source_height: u32,
    pub delivered_format: String,
    pub delivered_mime: String,
    pub converted: bool,
    pub orientation_applied: bool,
    pub encoding_policy: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct DeliveredImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub extension: &'static str,
    pub conversion: ImageConversionMetadata,
}

pub fn source_format(bytes: &[u8]) -> Option<&'static str> {
    match image::guess_format(bytes).ok()? {
        ImageFormat::Png => Some("png"),
        ImageFormat::Jpeg => Some("jpeg"),
        ImageFormat::WebP => Some("webp"),
        _ => None,
    }
}

pub fn deliver_image(bytes: &[u8], target: &str) -> Result<DeliveredImage, ImageDeliveryError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(failure(
            "image_resource_limit",
            "source image exceeds the 32 MiB byte limit",
        ));
    }
    let source = source_format(bytes).ok_or_else(|| {
        failure(
            "unsupported_source_image_format",
            "only single-frame PNG, JPEG and WebP can be delivered",
        )
    })?;
    if animated_image(bytes, source)? {
        return Err(failure(
            "unsupported_source_image_format",
            "animated PNG/WebP is not a supported single-frame source",
        ));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_EDGE);
    limits.max_image_height = Some(MAX_IMAGE_EDGE);
    limits.max_alloc = Some(MAX_IMAGE_ALLOCATION);
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| failure("image_decode_failed", e.to_string()))?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(decode_failure)?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
        || decoder.total_bytes() > MAX_IMAGE_ALLOCATION
    {
        return Err(failure(
            "image_resource_limit",
            "source exceeds pixel or decoded allocation limits",
        ));
    }
    resource_io::charge_owned_buffer(decoder.total_bytes()).map_err(admission_failure)?;
    resource_io::charge_owned_metadata(4096, 32).map_err(admission_failure)?;
    let orientation = decoder.orientation().map_err(|e| {
        failure(
            "image_decode_failed",
            format!("cannot read image orientation: {e}"),
        )
    })?;
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(decode_failure)?;
    let converted = source != target;
    // Byte-for-byte delivery retains EXIF orientation; transcoding applies it
    // before encoding because encoders do not copy the source EXIF container.
    let orientation_applied =
        converted && orientation != image::metadata::Orientation::NoTransforms;
    if converted {
        if orientation_applied {
            resource_io::charge_owned_buffer(decoded.as_bytes().len() as u64)
                .map_err(admission_failure)?;
        }
        decoded.apply_orientation(orientation);
    }
    let (mime, extension, format) = match target {
        "png" => ("image/png", "png", ImageFormat::Png),
        "jpeg" => ("image/jpeg", "jpg", ImageFormat::Jpeg),
        "webp" => ("image/webp", "webp", ImageFormat::WebP),
        _ => {
            return Err(failure(
                "invalid_image_parameter",
                "delivery format must be png, jpeg or webp",
            ))
        }
    };
    if converted
        && decoded.color().bits_per_pixel() / u16::from(decoded.color().channel_count()) > 8
        && target != "png"
    {
        return Err(failure(
            "image_conversion_failed",
            "conversion would silently discard source bit depth",
        ));
    }
    if target == "jpeg" && source != "jpeg" && decoded.color().has_alpha() {
        resource_io::charge_owned_buffer(u64::from(width) * u64::from(height) * 4)
            .map_err(admission_failure)?;
        if decoded.to_rgba8().pixels().any(|pixel| pixel[3] != 255) {
            return Err(failure("image_transparency_not_preserved", "source contains transparency; JPEG conversion requires an explicit compositing contract. Use PNG/WebP or recover the saved original"));
        }
    }
    let output = if !converted {
        resource_io::charge_owned_buffer(bytes.len() as u64).map_err(admission_failure)?;
        bytes.to_vec()
    } else {
        let mut output = BoundedOutput(Cursor::new(Vec::new()));
        if format == ImageFormat::Jpeg {
            resource_io::charge_owned_buffer(u64::from(width) * u64::from(height) * 3)
                .map_err(admission_failure)?;
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 90)
                .encode_image(&decoded.to_rgb8())
                .map_err(|e| failure("image_conversion_failed", e.to_string()))?;
        } else {
            decoded
                .write_to(&mut output, format)
                .map_err(|e| failure("image_conversion_failed", e.to_string()))?;
        }
        output.0.into_inner()
    };
    let (actual_width, actual_height) = if converted {
        (decoded.width(), decoded.height())
    } else if matches!(
        orientation,
        image::metadata::Orientation::Rotate90
            | image::metadata::Orientation::Rotate270
            | image::metadata::Orientation::Rotate90FlipH
            | image::metadata::Orientation::Rotate270FlipH
    ) {
        (height, width)
    } else {
        (width, height)
    };
    Ok(DeliveredImage {
        bytes: output,
        mime,
        extension,
        conversion: ImageConversionMetadata {
            source_format: source.into(),
            source_mime: match source {
                "png" => "image/png",
                "jpeg" => "image/jpeg",
                _ => "image/webp",
            }
            .into(),
            source_width: width,
            source_height: height,
            delivered_format: target.into(),
            delivered_mime: mime.into(),
            converted,
            orientation_applied,
            encoding_policy: if !converted {
                "original_bytes"
            } else if target == "jpeg" {
                "jpeg_quality_90_lossy"
            } else if target == "webp" {
                "webp_lossless"
            } else {
                "png_lossless"
            }
            .into(),
            width: actual_width,
            height: actual_height,
        },
    })
}

// Inspect container chunk boundaries, not arbitrary substrings in pixel data.
fn animated_image(bytes: &[u8], format: &str) -> Result<bool, ImageDeliveryError> {
    let mut offset: usize = if format == "png" {
        8
    } else if format == "webp" {
        12
    } else {
        return Ok(false);
    };
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let header = &bytes[offset..offset + 8];
        let (kind, length) = if format == "png" {
            (
                &header[4..8],
                u32::from_be_bytes(header[..4].try_into().expect("four bytes")) as usize,
            )
        } else {
            (
                &header[..4],
                u32::from_le_bytes(header[4..8].try_into().expect("four bytes")) as usize,
            )
        };
        if kind == b"acTL" || kind == b"ANIM" || kind == b"ANMF" {
            return Ok(true);
        }
        let padding = if format == "png" { 4 } else { length % 2 };
        offset = offset
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .and_then(|n| n.checked_add(padding))
            .ok_or_else(|| failure("image_resource_limit", "invalid image chunk length"))?;
    }
    Ok(false)
}

struct BoundedOutput(Cursor<Vec<u8>>);

impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .0
            .position()
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("encoded image size overflow"))?;
        if end > MAX_IMAGE_BYTES as u64 {
            return Err(io::Error::other(
                "encoded image exceeds 32 MiB output limit",
            ));
        }
        resource_io::charge_owned_buffer(end.saturating_sub(self.0.get_ref().len() as u64))?;
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Seek for BoundedOutput {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(alpha: u8) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            3,
            image::Rgba([1, 2, 3, alpha]),
        ));
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, ImageFormat::Png).unwrap();
        output.into_inner()
    }

    #[test]
    fn image_delivery_validates_and_preserves_original_encoding() {
        let bytes = png(255);
        let output = deliver_image(&bytes, "png").unwrap();
        assert_eq!(output.bytes, bytes);
        assert!(!output.conversion.converted);
        assert_eq!((output.conversion.width, output.conversion.height), (2, 3));
    }

    #[test]
    fn image_delivery_alpha_and_codecs() {
        let transparent = png(100);
        assert_eq!(
            deliver_image(&transparent, "jpeg").unwrap_err().code,
            "image_transparency_not_preserved"
        );
        let webp = deliver_image(&transparent, "webp").unwrap();
        assert_eq!(source_format(&webp.bytes), Some("webp"));
        let back = deliver_image(&webp.bytes, "png").unwrap();
        assert_eq!(
            image::load_from_memory(&back.bytes)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            100
        );
        let jpeg = deliver_image(&png(255), "jpeg").unwrap();
        assert_eq!(source_format(&jpeg.bytes), Some("jpeg"));
        assert_eq!(jpeg.conversion.encoding_policy, "jpeg_quality_90_lossy");
    }

    #[test]
    fn image_delivery_exif_orientation_survives_transcoding() {
        let jpeg = deliver_image(&png(255), "jpeg").unwrap().bytes;
        // EXIF IFD0 Orientation=6 (90 degrees clockwise).
        let exif: &[u8] =
            b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut oriented = jpeg[..2].to_vec();
        oriented.extend_from_slice(&[0xff, 0xe1, 0, 34]);
        oriented.extend_from_slice(exif);
        oriented.extend_from_slice(&jpeg[2..]);
        let unchanged = deliver_image(&oriented, "jpeg").unwrap();
        assert_eq!(unchanged.bytes, oriented);
        assert_eq!(
            (unchanged.conversion.width, unchanged.conversion.height),
            (3, 2)
        );
        let png = deliver_image(&oriented, "png").unwrap();
        assert!(png.conversion.orientation_applied);
        assert_eq!((png.conversion.width, png.conversion.height), (3, 2));
        let actual = image::load_from_memory(&png.bytes).unwrap();
        assert_eq!((actual.width(), actual.height()), (3, 2));
    }

    #[test]
    fn image_delivery_rejects_animation_without_substring_false_positives() {
        let mut animated_png = b"\x89PNG\r\n\x1a\n".to_vec();
        animated_png.extend_from_slice(&[0, 0, 0, 8]);
        animated_png.extend_from_slice(b"acTL");
        animated_png.extend_from_slice(&[0; 12]);
        assert_eq!(
            deliver_image(&animated_png, "png").unwrap_err().code,
            "unsupported_source_image_format"
        );
        let mut animated_webp = b"RIFF\x10\0\0\0WEBP".to_vec();
        animated_webp.extend_from_slice(b"ANIM\x06\0\0\0");
        animated_webp.extend_from_slice(&[0; 6]);
        assert_eq!(
            deliver_image(&animated_webp, "png").unwrap_err().code,
            "unsupported_source_image_format"
        );
        assert!(!animated_image(&png(255), "png").unwrap());
    }

    #[test]
    fn image_delivery_preserves_sixteen_bit_png_and_rejects_lossy_depth_reduction() {
        let image = DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(
            2,
            3,
            image::Rgba([1000u16, 2000, 3000, u16::MAX]),
        ));
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, ImageFormat::Png).unwrap();
        let bytes = encoded.into_inner();
        assert_eq!(deliver_image(&bytes, "png").unwrap().bytes, bytes);
        assert_eq!(
            deliver_image(&bytes, "webp").unwrap_err().code,
            "image_conversion_failed"
        );
    }

    #[test]
    fn image_delivery_corruption_and_limits() {
        assert_eq!(
            deliver_image(b"not an image", "png").unwrap_err().code,
            "unsupported_source_image_format"
        );
        assert!(deliver_image(b"\x89PNG\r\n\x1a\nbroken", "png").is_err());
        let bytes = vec![0; MAX_IMAGE_BYTES + 1];
        assert_eq!(
            deliver_image(&bytes, "png").unwrap_err().code,
            "image_resource_limit"
        );
        let mut output = BoundedOutput(Cursor::new(Vec::new()));
        output
            .seek(SeekFrom::Start(MAX_IMAGE_BYTES as u64))
            .unwrap();
        assert!(output.write_all(&[1]).is_err());
    }
}
