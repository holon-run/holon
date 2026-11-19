use std::io::Cursor;

use base64::{engine::general_purpose::STANDARD, Engine};
use image::{ImageReader, Limits};
use serde_json::Value;

// Heuristic fallbacks for missing dimensions/model rules, not universal maxima.
pub const ANTHROPIC_IMAGE_RESERVE_TOKENS: usize = 1_568;
pub const OPENAI_IMAGE_RESERVE_TOKENS: usize = 2_048;

pub fn conservative_image_reserve_tokens() -> usize {
    ANTHROPIC_IMAGE_RESERVE_TOKENS.max(OPENAI_IMAGE_RESERVE_TOKENS)
}

// Header probing is bounded independently of the payload length; never decode
// pixels or fetch remote URLs. Failure means unknown dimensions, not rejection.
pub fn inline_image_dimensions(data_base64: &str) -> Option<(u32, u32)> {
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
pub fn estimate_image_tokens(data_base64: &str) -> usize {
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
pub fn anthropic_image_tokens(model: &str, dimensions: Option<(u32, u32)>) -> usize {
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

pub fn openai_image_tokens(
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

pub fn estimate_text_tokens(text: &str) -> usize {
    let bytes = text.len();
    bytes.saturating_add(3) / 4
}

pub fn estimate_json_tokens(value: &Value) -> usize {
    match serde_json::to_string(value) {
        Ok(json) => estimate_text_tokens(&json),
        Err(_) => 1,
    }
}

#[cfg(test)]
mod tests;
