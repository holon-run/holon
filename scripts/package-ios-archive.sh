#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[[ $# == 0 ]] || { echo "Usage: IOS_DEVELOPMENT_TEAM=... IOS_ARCHIVE_PATH=... $0" >&2; exit 2; }
[[ "${IOS_DEVELOPMENT_TEAM:-}" =~ ^[A-Z0-9]{10}$ ]] ||
    { echo "Supply an explicit 10-character IOS_DEVELOPMENT_TEAM; no Apple registration is performed." >&2; exit 2; }
[[ "${IOS_ARCHIVE_PATH:-}" == /* && "$IOS_ARCHIVE_PATH" == *.xcarchive && ! -e "$IOS_ARCHIVE_PATH" ]] ||
    { echo "IOS_ARCHIVE_PATH must be an unused absolute .xcarchive path" >&2; exit 2; }
# Keep the project's bundle identifier; provisioning must already exist locally.
# Deliberately omit allowProvisioningUpdates and any export/upload action.
exec xcodebuild -project "$root/apps/ios/Holon.xcodeproj" -scheme Holon \
    -destination 'generic/platform=iOS' -archivePath "$IOS_ARCHIVE_PATH" \
    "DEVELOPMENT_TEAM=$IOS_DEVELOPMENT_TEAM" archive
