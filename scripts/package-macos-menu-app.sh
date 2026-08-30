#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <holon-binary> <output-dir> [version]" >&2
  exit 2
}

[[ $# -ge 2 && $# -le 3 ]] || usage

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
holon_binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
output_dir="$2"
version="${3:-$(awk -F '"' '/^version = / { print $2; exit }' "$repo_root/Cargo.toml")}"
build_number="${HOLON_BUILD_NUMBER:-$(date -u +%Y%m%d%H%M)}"
app_dir="$output_dir/Holon.app"
contents_dir="$app_dir/Contents"
package_dir="$repo_root/apps/macos/HolonMenu"

[[ -x "$holon_binary" ]] || {
  echo "holon binary is missing or not executable: $holon_binary" >&2
  exit 1
}

rm -rf "$app_dir"
mkdir -p "$contents_dir/MacOS" "$contents_dir/Resources/bin"

swift build \
  --package-path "$package_dir" \
  -c release \
  --product HolonMenu

swift_bin_path="$(swift build \
  --package-path "$package_dir" \
  -c release \
  --show-bin-path)"
cp "$swift_bin_path/HolonMenu" "$contents_dir/MacOS/HolonMenu"
cp "$holon_binary" "$contents_dir/Resources/bin/holon"
while IFS= read -r resource_bundle; do
  ditto "$resource_bundle" "$contents_dir/Resources/$(basename "$resource_bundle")"
done < <(find "$swift_bin_path" -maxdepth 1 -type d -name '*HolonMenu*.bundle' -print)
chmod 755 "$contents_dir/MacOS/HolonMenu" "$contents_dir/Resources/bin/holon"
install_name_tool -add_rpath "@executable_path/../Frameworks" "$contents_dir/MacOS/HolonMenu"

cp "$package_dir/Resources/Info.plist" "$contents_dir/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$contents_dir/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $build_number" "$contents_dir/Info.plist"
/usr/libexec/PlistBuddy -c \
  "Set :SUFeedURL ${HOLON_SPARKLE_FEED_URL:-https://releases.holon.run/macos/appcast.xml}" \
  "$contents_dir/Info.plist"
/usr/libexec/PlistBuddy -c \
  "Set :SUPublicEDKey ${HOLON_SPARKLE_PUBLIC_KEY:-UNCONFIGURED}" \
  "$contents_dir/Info.plist"

sparkle_framework="$(find "$package_dir/.build" -path '*/Sparkle.framework' -type d -print -quit)"
if [[ -n "$sparkle_framework" ]]; then
  mkdir -p "$contents_dir/Frameworks"
  ditto "$sparkle_framework" "$contents_dir/Frameworks/Sparkle.framework"
fi

if [[ -n "${MACOS_DEVELOPER_ID_APPLICATION:-}" ]]; then
  while IFS= read -r nested; do
    codesign --force --timestamp --options runtime \
      --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$nested"
  done < <(find "$contents_dir/Frameworks" -depth \
    \( -name '*.framework' -o -name '*.xpc' -o -name '*.app' \) 2>/dev/null)
  codesign --force --timestamp --options runtime \
    --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$contents_dir/Resources/bin/holon"
  codesign --force --timestamp --options runtime \
    --entitlements "$package_dir/Resources/HolonMenu.entitlements" \
    --sign "$MACOS_DEVELOPER_ID_APPLICATION" "$app_dir"
fi

"$repo_root/scripts/verify-macos-menu-app.sh" "$app_dir" "$version"

submit_notarization() {
  local artifact="$1"
  local submission_output
  local submission_id

  if [[ -n "${MACOS_NOTARY_PROFILE:-}" ]]; then
    if ! submission_output="$(xcrun notarytool submit "$artifact" \
      --keychain-profile "$MACOS_NOTARY_PROFILE" \
      --output-format json \
      --wait 2>&1)"; then
      printf '%s\n' "$submission_output" >&2
      submission_id="$(printf '%s\n' "$submission_output" | sed -nE 's/.*"id"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | tail -1)"
      if [[ -n "$submission_id" ]]; then
        echo "notarytool submission log for $submission_id:" >&2
        xcrun notarytool log "$submission_id" \
          --keychain-profile "$MACOS_NOTARY_PROFILE" \
          --output-format json >&2 || true
      fi
      return 1
    fi
  else
    : "${MACOS_NOTARY_APPLE_ID:?MACOS_NOTARY_APPLE_ID is required}"
    : "${MACOS_NOTARY_PASSWORD:?MACOS_NOTARY_PASSWORD is required}"
    : "${MACOS_NOTARY_TEAM_ID:?MACOS_NOTARY_TEAM_ID is required}"
    if ! submission_output="$(xcrun notarytool submit "$artifact" \
      --apple-id "$MACOS_NOTARY_APPLE_ID" \
      --password "$MACOS_NOTARY_PASSWORD" \
      --team-id "$MACOS_NOTARY_TEAM_ID" \
      --output-format json \
      --wait 2>&1)"; then
      printf '%s\n' "$submission_output" >&2
      submission_id="$(printf '%s\n' "$submission_output" | sed -nE 's/.*"id"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | tail -1)"
      if [[ -n "$submission_id" ]]; then
        echo "notarytool submission log for $submission_id:" >&2
        xcrun notarytool log "$submission_id" \
          --apple-id "$MACOS_NOTARY_APPLE_ID" \
          --password "$MACOS_NOTARY_PASSWORD" \
          --team-id "$MACOS_NOTARY_TEAM_ID" \
          --output-format json >&2 || true
      fi
      return 1
    fi
  fi
  printf '%s\n' "$submission_output"
}

notary_configured=false
if [[ -n "${MACOS_NOTARY_PROFILE:-}${MACOS_NOTARY_APPLE_ID:-}" ]]; then
  notary_configured=true
  app_archive="$output_dir/Holon-${version}.zip"
  rm -f "$app_archive"
  ditto -c -k --keepParent "$app_dir" "$app_archive"
  submit_notarization "$app_archive"
  xcrun stapler staple "$app_dir"
  rm -f "$app_archive"
fi

dmg_path="$output_dir/Holon-${version}.dmg"
rm -f "$dmg_path"
hdiutil create -quiet -volname Holon -srcfolder "$app_dir" -ov -format UDZO "$dmg_path"

if $notary_configured; then
  submit_notarization "$dmg_path"
  xcrun stapler staple "$dmg_path"
fi

shasum -a 256 "$dmg_path" > "$dmg_path.sha256"
echo "$dmg_path"
