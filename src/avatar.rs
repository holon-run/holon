//! Pure, versioned agent artwork. IDs select a recipe and never enter SVG markup.
//!
//! The checked-in JS renderer remains the byte-level reference for this version.
//! PNGs are rasterized from this very SVG, without fonts or external resources.

mod data;

use anyhow::{ensure, Context, Result};
use data::{PALETTES, SHELLS};
use std::str::FromStr;

pub const VERSION: &str = "light-mech-1-vivid-2";
const SEED: &str = "light-mech-1:";
const CORE: usize = SHELLS.len() * PALETTES.len();
const TOTAL: usize = CORE * 4 * 3;
const INK: &str = "#243442";
const SCREEN: &str = "#fffdf5";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Expression {
    #[default]
    Neutral,
    Focused,
    Questioning,
    Waiting,
    Sleeping,
}

impl FromStr for Expression {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "neutral" => Ok(Self::Neutral),
            "focused" => Ok(Self::Focused),
            "questioning" => Ok(Self::Questioning),
            "waiting" => Ok(Self::Waiting),
            "sleeping" => Ok(Self::Sleeping),
            _ => anyhow::bail!("invalid avatar expression"),
        }
    }
}

/// FNV-1a over Unicode code points, not UTF-8 bytes or UTF-16 code units.
/// The empty string and `holonbot` follow the same pure mapping as all other IDs.
pub fn recipe_index(agent_id: &str) -> usize {
    SEED.chars()
        .chain(agent_id.chars())
        .fold(2_166_136_261u32, |hash, ch| {
            (hash ^ u32::from(ch)).wrapping_mul(16_777_619)
        }) as usize
        % TOTAL
}

fn path(d: &str, fill: &str, stroke: &str, width: &str) -> String {
    format!(r#"<path d="{d}" fill="{fill}" stroke="{stroke}" stroke-width="{width}"/>"#)
}

/// Render one of the fixed recipes. Out-of-range indices wrap in the original
/// 1152-slot namespace (which deliberately retains the former expression slots).
pub fn render_svg(index: usize, expression: Expression) -> String {
    let index = index % TOTAL;
    let shell = &SHELLS[index % SHELLS.len()];
    let [body, shade, background] = PALETTES[index / SHELLS.len() % PALETTES.len()];
    let crown = index / (CORE * 4) % 3;
    let [cx, cy] = shell.anchor;
    let mut art = match crown {
        0 => path(
            &format!("M{} {}V{}H{}V{}Z", cx - 8, cy + 1, cy - 5, cx + 8, cy + 1),
            shade,
            INK,
            "3",
        ),
        1 => {
            path(&format!("M{cx} {cy}V{}", cy - 9), "none", INK, "4")
                + &format!(
                    r#"<rect x="{}" y="{}" width="10" height="8" rx="3" fill="{body}" stroke="{INK}" stroke-width="3"/>"#,
                    cx - 5,
                    cy - 15
                )
        }
        _ => path(
            &format!(
                "M{} {}V{}Q{} {} {} {}H{}Q{} {} {} {}V{}",
                cx - 12,
                cy + 1,
                cy - 7,
                cx - 12,
                cy - 10,
                cx - 8,
                cy - 10,
                cx + 8,
                cx + 12,
                cy - 10,
                cx + 12,
                cy - 7,
                cy + 1
            ),
            "none",
            INK,
            "5",
        ),
    };
    art += &path(shell.side, shade, INK, "3");
    art += &path(shell.head, body, INK, "3");
    art += &path(shell.shade, shade, "none", "3");
    art += &path(shell.face, SCREEN, INK, "2.8");
    let [x, y, gap] = shell.eyes;
    art += &format!(
        r#"<g data-avatar-part="expression">{}</g>"#,
        eyes(expression, x, y, gap)
    );
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><rect width="100" height="100" fill="{background}"/><g stroke-linecap="round" stroke-linejoin="round">{art}</g></svg>"#
    )
}

fn eyes(expression: Expression, x: i32, y: i32, gap: i32) -> String {
    let left = x - gap;
    let right = x + gap;
    match expression {
        Expression::Focused => {
            path(
                &format!(
                    "M{} {}H{} M{} {}H{}",
                    left - 4,
                    y - 5,
                    left + 4,
                    right - 4,
                    y - 5,
                    right + 4
                ),
                "none",
                INK,
                "3",
            ) + &format!(
                r#"<rect x="{}" y="{}" width="6" height="4" rx="1.5" fill="{INK}"/><rect x="{}" y="{}" width="6" height="4" rx="1.5" fill="{INK}"/>"#,
                left - 3,
                y - 1,
                right - 3,
                y - 1
            )
        }
        Expression::Questioning => {
            format!(
                r#"<circle cx="{left}" cy="{}" r="3" fill="{INK}"/><rect x="{}" y="{}" width="6" height="10" rx="3" fill="{INK}"/>"#,
                y + 1,
                right - 3,
                y - 5
            ) + &path(
                &format!(
                    "M{} {}Q{} {} {} {}",
                    right - 4,
                    y - 9,
                    right,
                    y - 12,
                    right + 4,
                    y - 9
                ),
                "none",
                INK,
                "2.5",
            )
        }
        Expression::Waiting => format!(
            r#"<circle cx="{}" cy="{y}" r="3.5" fill="{INK}"/><circle cx="{}" cy="{y}" r="3.5" fill="{INK}"/>"#,
            left + 2,
            right + 2
        ),
        Expression::Sleeping => path(
            &format!(
                "M{} {y}Q{left} {} {} {y} M{} {y}Q{right} {} {} {y}",
                left - 4,
                y + 4,
                left + 4,
                right - 4,
                y + 4,
                right + 4
            ),
            "none",
            INK,
            "3.5",
        ),
        Expression::Neutral => format!(
            r#"<rect x="{}" y="{}" width="5.4" height="9" rx="2.7" fill="{INK}"/><rect x="{}" y="{}" width="5.4" height="9" rx="2.7" fill="{INK}"/>"#,
            f64::from(left) - 2.7,
            f64::from(y) - 4.5,
            f64::from(right) - 2.7,
            f64::from(y) - 4.5
        ),
    }
}

/// Rasterize the same controlled SVG. No caller-supplied SVG, font database,
/// filesystem resource directory, or network resolver enters this path.
pub fn render_png(index: usize, expression: Expression, size: u32) -> Result<Vec<u8>> {
    ensure!((1..=512).contains(&size), "avatar size must be 1..=512");
    let svg = render_svg(index, expression);
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .context("parse controlled avatar SVG")?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).context("allocate avatar pixels")?;
    let scale = size as f32 / 100.0;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().context("encode avatar PNG")
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
#[path = "avatar/tests.rs"]
mod tests;
