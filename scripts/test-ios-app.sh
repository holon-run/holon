#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
action="${1:-test}"
case "$action" in build|test) ;; *) echo "Usage: $0 [build|test]" >&2; exit 2 ;; esac
if [[ -n "${IOS_SIMULATOR_ID:-}" ]]; then
    destination="platform=iOS Simulator,id=$IOS_SIMULATOR_ID"
else
    simulator="$(xcrun simctl list devices available -j | python3 -c '
import json,sys
devices=json.load(sys.stdin)["devices"]
phones=[d for runtime, values in devices.items() if ".iOS-" in runtime for d in values if d["name"].startswith("iPhone")]
if not phones: sys.exit("No available iPhone simulator; install an iOS runtime in Xcode.")
print(phones[0]["udid"])
')"
    destination="platform=iOS Simulator,id=$simulator"
fi
exec xcodebuild -project "$root/apps/ios/Holon.xcodeproj" -scheme Holon \
    -destination "$destination" -derivedDataPath "$root/apps/ios/DerivedData" \
    CODE_SIGN_IDENTITY=- "$action"
