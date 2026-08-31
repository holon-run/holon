#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <Holon.app> <version>" >&2
  exit 2
fi

app_dir="$1"
expected_version="$2"
info_plist="$app_dir/Contents/Info.plist"
menu_binary="$app_dir/Contents/MacOS/HolonMenu"
holon_binary="$app_dir/Contents/Resources/bin/holon"

[[ -f "$info_plist" && -x "$menu_binary" && -x "$holon_binary" ]] || {
  echo "incomplete Holon.app bundle: $app_dir" >&2
  exit 1
}

# The DMG is the only macOS app artifact, so both first-party binaries must
# carry x86_64 and arm64 slices; Sparkle ships prebuilt universal binaries.
require_universal() {
  local binary="$1"
  local archs
  archs="$(lipo -archs "$binary")"
  if [[ "$archs" != *x86_64* || "$archs" != *arm64* ]]; then
    echo "$binary is not a universal (x86_64 + arm64) binary: $archs" >&2
    exit 1
  fi
}
require_universal "$menu_binary"
require_universal "$holon_binary"

[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$info_plist")" == "run.holon.menu" ]]
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$info_plist")" == "$expected_version" ]]
[[ "$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$info_plist")" == "13.0" ]]

version_output="$("$holon_binary" --version)"
if [[ "$version_output" == "holon ${expected_version} ("*"-dirty)" ]]; then
  commit_sha="${version_output#*"("}"
  commit_sha="${commit_sha%-dirty")"}"
  [[ "$commit_sha" =~ ^[0-9a-f]{7,40}$ ]] || {
    echo "unexpected dirty version output: $version_output" >&2
    exit 1
  }
  echo "warning: verified a development bundle built from a dirty worktree" >&2
else
  "$(dirname "$0")/verify-release-version.sh" "$expected_version" "$version_output"
fi

if [[ -d "$app_dir/Contents/_CodeSignature" ]]; then
  codesign --verify --deep --strict --verbose=2 "$app_dir"
fi

echo "verified $app_dir"
