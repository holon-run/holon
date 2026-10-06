#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <output.icns>" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_image="$repo_root/apps/macos/HolonMenu/Sources/HolonMenu/Resources/holon-mark.png"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/holon-app-icon.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

# Reuse the existing brand mark, on an inset rounded white tile. Render at an
# explicit pixel size rather than depending on the build host's screen scale.
swift - "$source_image" "$work_dir/icon.png" <<'SWIFT'
import AppKit

guard let mark = NSImage(contentsOfFile: CommandLine.arguments[1]),
      let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: 1024, pixelsHigh: 1024,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
      ),
      let context = NSGraphicsContext(bitmapImageRep: bitmap)
else { fatalError("cannot load Holon mark or create icon bitmap") }

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = context
context.imageInterpolation = .high
NSColor.clear.setFill()
NSRect(x: 0, y: 0, width: 1024, height: 1024).fill()
NSColor.white.setFill()
NSBezierPath(
    roundedRect: NSRect(x: 64, y: 64, width: 896, height: 896),
    xRadius: 190, yRadius: 190
).fill()
mark.draw(in: NSRect(x: 128, y: 128, width: 768, height: 768))
NSGraphicsContext.restoreGraphicsState()

guard let png = bitmap.representation(using: .png, properties: [:])
else { fatalError("cannot encode Holon icon") }
try png.write(to: URL(fileURLWithPath: CommandLine.arguments[2]))
SWIFT

iconset="$work_dir/Holon.iconset"
mkdir -p "$iconset" "$(dirname "$1")"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$work_dir/icon.png" \
    --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double_size=$((size * 2))
  sips -z "$double_size" "$double_size" "$work_dir/icon.png" \
    --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$1"
