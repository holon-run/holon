#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <archives-dir> <download-url-prefix> [output-name]" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archives_dir="$1"
download_url_prefix="$2"
output_name="${3:-appcast.xml}"
tool="$repo_root/apps/macos/HolonMenu/.build/artifacts/sparkle/Sparkle/bin/generate_appcast"

[[ -x "$tool" ]] || {
  echo "Sparkle generate_appcast is unavailable; run swift build for apps/macos/HolonMenu first" >&2
  exit 1
}
[[ -n "${HOLON_SPARKLE_PRIVATE_KEY:-}" ]] || {
  echo "HOLON_SPARKLE_PRIVATE_KEY is required to sign the appcast" >&2
  exit 1
}

printf '%s' "$HOLON_SPARKLE_PRIVATE_KEY" |
  "$tool" \
    --ed-key-file - \
    --download-url-prefix "$download_url_prefix" \
    --maximum-versions 3 \
    --maximum-deltas 2 \
    -o "$output_name" \
    "$archives_dir"

test -s "$archives_dir/$output_name"
echo "$archives_dir/$output_name"
