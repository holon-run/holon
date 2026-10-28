use std::io::Cursor;

use base64::{engine::general_purpose::STANDARD, Engine};
use image::{ImageReader, Limits};
use serde_json::Value;

// Heuristic fallbacks for missing dimensions/model rules, not universal maxima.
pub(crate) const ANTHROPIC_IMAGE_RESERVE_TOKENS: usize = 1_568;
pub(crate) const OPENAI_IMAGE_RESERVE_TOKENS: usize = 2_048;

pub(crate) fn conservative_image_reserve_tokens() -> usize {
    ANTHROPIC_IMAGE_RESERVE_TOKENS.max(OPENAI_IMAGE_RESERVE_TOKENS)
}

// Header probing is bounded independently of the payload length; never decode
// pixels or fetch remote URLs. Failure means unknown dimensions, not rejection.
pub(crate) fn inline_image_dimensions(data_base64: &str) -> Option<(u32, u32)> {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    let prefix_len = data_base64.len().min(MAX_HEADER_BYTES / 3 * 4) / 4 * 4;
    let header = STANDARD
        .decode(&data_base64.as_bytes()[..prefix_len])
        .ok()?;
    let mut reader = ImageReader::new(Cursor::new(header))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_HEADER_BYTES as u64);
    reader.limits(limits);
    reader
        .into_dimensions()
        .ok()
        .filter(|(width, height)| *width > 0 && *height > 0)
}

// Provider-neutral history projection: a size-sensitive heuristic, deliberately
// not gpt-4o-mini's unusually large billing multiplier.
pub(crate) fn estimate_image_tokens(data_base64: &str) -> usize {
    generic_image_tokens(inline_image_dimensions(data_base64))
}

fn generic_image_tokens(dimensions: Option<(u32, u32)>) -> usize {
    dimensions.map_or_else(conservative_image_reserve_tokens, |dimensions| {
        patch_tokens(dimensions, 28, OPENAI_IMAGE_RESERVE_TOKENS, 2_048)
    })
}

// Admission estimates follow the documented native patch/tile rules where
// known, but are not provider token counts or billing guarantees:
// https://platform.claude.com/docs/en/build-with-claude/vision
// https://developers.openai.com/api/docs/guides/images-vision
pub(crate) fn anthropic_image_tokens(model: &str, dimensions: Option<(u32, u32)>) -> usize {
    let mut version = model.split('-').filter_map(|part| part.parse::<u32>().ok());
    let high_resolution = version
        .next()
        .is_some_and(|major| major > 4 || (major == 4 && version.next().unwrap_or(0) >= 7));
    let (limit, edge) = if high_resolution {
        (4_784, 2_576)
    } else {
        (ANTHROPIC_IMAGE_RESERVE_TOKENS, 1_568)
    };
    dimensions.map_or(limit, |dimensions| {
        patch_tokens(dimensions, 28, limit, edge)
    })
}

pub(crate) fn openai_image_tokens(
    model: &str,
    detail: Option<&str>,
    dimensions: Option<(u32, u32)>,
) -> usize {
    let tile_rule = if model_matches(model, "gpt-4o-mini") {
        Some((2_833, 5_667))
    } else if ["gpt-4o", "gpt-4.1"]
        .iter()
        .any(|name| model_matches(model, name))
    {
        Some((85, 170))
    } else if ["gpt-5", "gpt-5.1"]
        .iter()
        .any(|name| model_matches(model, name))
    {
        Some((70, 140))
    } else if ["o1", "o1-pro", "o3"]
        .iter()
        .any(|name| model_matches(model, name))
    {
        Some((75, 150))
    } else {
        None
    };
    if let Some((base, tile)) = tile_rule {
        if detail == Some("low") {
            return base;
        }
        // Unknown dimensions use a conventional four-tile heuristic, not a
        // universal worst-case image or a fabricated base64-derived size.
        return dimensions.map_or(base + 4 * tile, |(width, height)| {
            let scale = (2_048.0 / f64::from(width.max(height)))
                .min(768.0 / f64::from(width.min(height)))
                .min(1.0);
            let width = (f64::from(width) * scale).floor().max(1.0) as usize;
            let height = (f64::from(height) * scale).floor().max(1.0) as usize;
            base + width.div_ceil(512) * height.div_ceil(512) * tile
        });
    }
    let patch_rule = if model_matches(model, "gpt-4.1-mini") {
        Some((6_144, 2_048, 162))
    } else if model_matches(model, "gpt-4.1-nano") {
        Some((1_536, 2_048, 246))
    } else if model_matches(model, "o4-mini") {
        Some((1_536, 2_048, 172))
    } else if model_matches(model, "gpt-5-mini") {
        Some((1_536, 2_048, 120))
    } else if model_matches(model, "gpt-5-nano") {
        Some((1_536, 2_048, 150))
    } else if model_matches(model, "gpt-5.2") {
        Some((6_144, 2_048, 120))
    } else if ["gpt-5.4", "gpt-5.4-mini", "gpt-5.4-nano"]
        .iter()
        .any(|name| model_matches(model, name))
    {
        Some(match detail {
            Some("original") => (10_000, 6_000, 120),
            Some("low") => (6_144, 2_048, 120),
            _ => (2_500, 2_048, 120),
        })
    } else {
        None
    };
    if let Some((budget, edge, multiplier)) = patch_rule {
        return dimensions.map_or_else(conservative_image_reserve_tokens, |dimensions| {
            patch_tokens(dimensions, 32, budget, edge)
                .saturating_mul(multiplier)
                .div_ceil(100)
        });
    }
    // Compatible-provider aliases and unlisted models remain heuristic.
    generic_image_tokens(dimensions)
}

fn model_matches(model: &str, name: &str) -> bool {
    model == name
        || model
            .strip_prefix(name)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .is_some_and(|snapshot| {
                snapshot.len() == 10
                    && snapshot
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'-')
            })
}

fn patch_tokens((width, height): (u32, u32), patch: usize, budget: usize, edge: u32) -> usize {
    let scale = (f64::from(edge) / f64::from(width.max(height))).min(1.0);
    let width = (f64::from(width) * scale).floor().max(1.0) as usize;
    let height = (f64::from(height) * scale).floor().max(1.0) as usize;
    // A capped reserve avoids overprecision about the vendor's final
    // aspect-ratio/patch-boundary rounding when resizing is necessary.
    width
        .div_ceil(patch)
        .saturating_mul(height.div_ceil(patch))
        .min(budget)
}

pub(crate) fn estimate_text_tokens(text: &str) -> usize {
    let bytes = text.len();
    bytes.saturating_add(3) / 4
}

pub(crate) fn estimate_json_tokens(value: &Value) -> usize {
    match serde_json::to_string(value) {
        Ok(json) => estimate_text_tokens(&json),
        Err(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        anthropic_image_tokens, conservative_image_reserve_tokens, estimate_image_tokens,
        inline_image_dimensions, openai_image_tokens,
    };
    use base64::{engine::general_purpose::STANDARD, Engine};
    use image::{DynamicImage, ImageFormat};
    use std::io::Cursor;

    #[test]
    fn small_images_use_dimensions_instead_of_a_worst_case_reserve() {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(200, 200)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        let encoded = STANDARD.encode(bytes.into_inner());
        assert_eq!(inline_image_dimensions(&encoded), Some((200, 200)));
        assert_eq!(estimate_image_tokens(&encoded), 64);
        assert_eq!(
            anthropic_image_tokens("claude-sonnet-4-6", Some((200, 200))),
            64
        );
        assert_eq!(openai_image_tokens("gpt-4o", None, Some((200, 200))), 255);
        assert_eq!(openai_image_tokens("gpt-4o-mini", Some("low"), None), 2_833);
        assert_eq!(
            openai_image_tokens("gpt-4.1-mini", None, Some((200, 200))),
            80
        );
    }

    #[test]
    fn known_model_rules_preserve_detail_and_resolution_tiers() {
        assert_eq!(
            openai_image_tokens("gpt-4o-2024-08-06", Some("low"), None),
            85
        );
        assert_eq!(
            openai_image_tokens("gpt-4o", Some("high"), Some((1024, 1024))),
            765
        );
        assert_eq!(
            openai_image_tokens("gpt-4o-mini", None, Some((1024, 1024))),
            25_501
        );
        assert_eq!(
            openai_image_tokens("gpt-4.1-mini", None, Some((1024, 1024))),
            1_659
        );
        assert_eq!(
            openai_image_tokens("gpt-5.4", Some("high"), Some((4096, 4096))),
            3_000
        );
        assert_eq!(
            anthropic_image_tokens("claude-sonnet-4-6", Some((4096, 4096))),
            1_568
        );
        assert_eq!(
            anthropic_image_tokens("claude-opus-4-7", Some((4096, 4096))),
            4_784
        );
    }

    #[test]
    fn unknown_and_unreadable_images_keep_a_bounded_heuristic() {
        assert_eq!(inline_image_dimensions(&"A".repeat(4_000_000)), None);
        assert_eq!(inline_image_dimensions("not base64"), None);
        assert_eq!(
            estimate_image_tokens("not base64"),
            conservative_image_reserve_tokens()
        );
        assert_eq!(openai_image_tokens("compatible-alias", None, None), 2_048);
        assert_eq!(
            openai_image_tokens("gpt-4o-custom", None, Some((200, 200))),
            64
        );
    }

    #[test]
    fn header_probe_supports_delivery_formats_and_ignores_payload_tail() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let mut bytes = Cursor::new(Vec::new());
            DynamicImage::new_rgb8(200, 100)
                .write_to(&mut bytes, format)
                .unwrap();
            let mut bytes = bytes.into_inner();
            assert_eq!(
                inline_image_dimensions(&STANDARD.encode(&bytes)),
                Some((200, 100)),
                "{format:?}"
            );
            bytes.resize(2_000_000, 0);
            let encoded = STANDARD.encode(bytes);
            assert_eq!(inline_image_dimensions(&encoded), Some((200, 100)));
            assert_eq!(estimate_image_tokens(&encoded), 32);
        }
    }

    #[test]
    fn header_probe_handles_truncation_and_the_prefix_boundary() {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(200, 100)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        let mut bytes = bytes.into_inner();
        let encoded = STANDARD.encode(&bytes);
        for len in [0, 1, 2, 3, 4, 12, 24] {
            assert_eq!(inline_image_dimensions(&encoded[..len]), None, "{len}");
        }

        // The bounded base64 prefix decodes to the largest aligned length
        // below 64 KiB; invalid data after it must never be examined.
        bytes.resize(64 * 1024 / 3 * 3, 0);
        let mut encoded = STANDARD.encode(bytes);
        assert_eq!(encoded.len(), 64 * 1024 / 3 * 4);
        encoded.push('!');
        assert_eq!(inline_image_dimensions(&encoded), Some((200, 100)));

        // A scalar straddling the byte boundary is malformed base64, not a
        // string-slicing panic.
        encoded.truncate(encoded.len() - 2);
        encoded.push('\u{1f5bc}');
        assert_eq!(inline_image_dimensions(&encoded), None);
        assert_eq!(
            estimate_image_tokens(&encoded),
            conservative_image_reserve_tokens()
        );
    }
}
