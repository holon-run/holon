#!/usr/bin/env python3
"""Re-encode oversized site assets in place.

The docs site ships its images as repository files, so every byte here is a byte
the browser downloads on a real page view. Editors usually export covers and
screenshots at full resolution with default encoder settings, which is how a
1536x1024 WebP cover ended up at 667 KB and a 1024x1024 logo at 374 KB.

The script keeps existing file paths (no markup changes) and re-encodes:

* cover/decorative WebP images: same aspect ratio, width capped, quality 82
* `assets/lite-paper/*.webp` screenshots: width capped, lossless WebP
* `assets/logo.png`: capped to a small square, real PNG output
* `assets/logo.webp`: derived from `assets/logo.png` for the page header, where
  the logo is downloaded on every page view; the PNG stays for structured data
  and social crawlers that expect a raster icon

Screenshots stay lossless because they contain UI text; lossy WebP blurs it.
When adding a screenshot, downscale it, encode lossless WebP, and update its
links by hand instead of committing a 3000px PNG.

A file is only rewritten when the new encoding saves at least 10%, so the script
is safe to re-run. Run it after adding or replacing any image:

    python3 optimize-assets.py           # rewrite oversized assets
    python3 optimize-assets.py --check   # report only, non-zero if work remains
"""

from __future__ import annotations

import argparse
import io
import sys
from pathlib import Path

from PIL import Image

SITE_ROOT = Path(__file__).resolve().parent.parent

MAX_WEBP_WIDTH = 1600
WEBP_QUALITY = 82
MAX_SCREENSHOT_WIDTH = 1600
LOGO_SIZE = 256
LOGO_WEBP_QUALITY = 82
MIN_SAVING_RATIO = 0.10

WEBP_GLOBS = ("assets/*.webp",)
SCREENSHOT_GLOBS = ("assets/lite-paper/*.webp",)
SCREENSHOT_PNG_GLOBS = ("assets/lite-paper/*.png",)


def _resize_to_width(image: Image.Image, max_width: int) -> Image.Image:
    rgb = image.convert("RGB")
    if rgb.width <= max_width:
        return rgb
    height = round(rgb.height * max_width / rgb.width)
    return rgb.resize((max_width, height), Image.LANCZOS)


def encode_webp(image: Image.Image) -> tuple[bytes, tuple[int, int]]:
    rgb = _resize_to_width(image, MAX_WEBP_WIDTH)
    buffer = io.BytesIO()
    rgb.save(buffer, "WEBP", quality=WEBP_QUALITY, method=6)
    return buffer.getvalue(), rgb.size


def encode_screenshot_lossless(image: Image.Image) -> tuple[bytes, tuple[int, int]]:
    rgb = _resize_to_width(image, MAX_SCREENSHOT_WIDTH)
    buffer = io.BytesIO()
    rgb.save(buffer, "WEBP", lossless=True, method=6)
    return buffer.getvalue(), rgb.size


def encode_screenshot_png(image: Image.Image) -> tuple[bytes, tuple[int, int]]:
    rgb = _resize_to_width(image, MAX_SCREENSHOT_WIDTH)
    buffer = io.BytesIO()
    rgb.save(buffer, "PNG", optimize=True)
    return buffer.getvalue(), rgb.size


def encode_logo(image: Image.Image) -> tuple[bytes, tuple[int, int]]:
    converted = image.convert("RGBA" if image.mode in ("RGBA", "LA") else "RGB")
    converted.thumbnail((LOGO_SIZE, LOGO_SIZE), Image.LANCZOS)
    buffer = io.BytesIO()
    converted.save(buffer, "PNG", optimize=True)
    return buffer.getvalue(), converted.size


def encode_logo_webp(image: Image.Image) -> tuple[bytes, tuple[int, int]]:
    converted = image.convert("RGBA" if image.mode in ("RGBA", "LA") else "RGB")
    converted.thumbnail((LOGO_SIZE, LOGO_SIZE), Image.LANCZOS)
    buffer = io.BytesIO()
    converted.save(buffer, "WEBP", quality=LOGO_WEBP_QUALITY, method=6)
    return buffer.getvalue(), converted.size


ENCODERS = {
    "webp": encode_webp,
    "screenshot_lossless": encode_screenshot_lossless,
    "screenshot_png": encode_screenshot_png,
    "logo": encode_logo,
    "logo_webp": encode_logo_webp,
}


def candidates() -> list[tuple[Path, str, Path | None]]:
    """Return (target path, encoder key, source path) triples.

    A source path is set only for derived assets that are encoded from another
    file, such as the header logo WebP built from the PNG logo.
    """
    found: list[tuple[Path, str, Path | None]] = []
    for pattern in WEBP_GLOBS:
        found.extend(
            (path, "webp", None) for path in sorted(SITE_ROOT.glob(pattern))
        )
    for pattern in SCREENSHOT_GLOBS:
        found.extend(
            (path, "screenshot_lossless", None)
            for path in sorted(SITE_ROOT.glob(pattern))
        )
    for pattern in SCREENSHOT_PNG_GLOBS:
        found.extend(
            (path, "screenshot_png", None)
            for path in sorted(SITE_ROOT.glob(pattern))
        )
    logo = SITE_ROOT / "assets" / "logo.png"
    if logo.exists():
        found.append((logo, "logo", None))
        found.append((SITE_ROOT / "assets" / "logo.webp", "logo_webp", logo))
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="report assets that would shrink by 10%% or more without writing",
    )
    args = parser.parse_args()

    total_before = 0
    total_after = 0
    changed = 0
    pending: list[str] = []

    for path, kind, source in candidates():
        before = path.stat().st_size if path.exists() else 0
        total_before += before
        try:
            with Image.open(source or path) as image:
                image.load()
                payload, size = ENCODERS[kind](image)
        except OSError as error:
            print(f"skip {path.relative_to(SITE_ROOT)}: {error}", file=sys.stderr)
            total_after += before
            continue

        after = len(payload)
        if before and after >= before * (1 - MIN_SAVING_RATIO):
            total_after += before
            continue

        changed += 1
        pending.append(
            f"{path.relative_to(SITE_ROOT)}: {before} -> {after} bytes, {size[0]}x{size[1]}"
        )
        if not args.check:
            path.write_bytes(payload)
        total_after += after

    for line in pending:
        print(("would shrink " if args.check else "rewrote ") + line)

    saved = total_before - total_after
    ratio = saved / total_before if total_before else 0
    print(
        f"{changed} asset(s) {'pending' if args.check else 'rewritten'}; "
        f"total {total_before} -> {total_after} bytes ({ratio:.0%} smaller)"
    )

    if args.check and changed:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
