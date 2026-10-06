#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
action="${1:-test}"
case "$action" in build|test) ;; *) echo "Usage: $0 [build|test]" >&2; exit 2 ;; esac
[[ $# -le 1 ]] || { echo "Unexpected arguments" >&2; exit 2; }
derived_data="${IOS_DERIVED_DATA_PATH:-$root/apps/ios/DerivedData}"
[[ "$derived_data" == /* && "$derived_data" != / ]] || { echo "DerivedData must be a non-root absolute path" >&2; exit 2; }
extra_args=(-derivedDataPath "$derived_data")
if [[ "$action" == test ]]; then
    extra_args+=(-only-testing:HolonTests)
fi
if [[ -n "${IOS_RESULT_BUNDLE_PATH:-}" ]]; then
    [[ "$IOS_RESULT_BUNDLE_PATH" == /* && "$IOS_RESULT_BUNDLE_PATH" != / && ! -e "$IOS_RESULT_BUNDLE_PATH" ]] ||
        { echo "ResultBundle must be an unused absolute path" >&2; exit 2; }
    extra_args+=(-resultBundlePath "$IOS_RESULT_BUNDLE_PATH")
fi
if [[ -n "${IOS_SIMULATOR_ID:-}" ]]; then
    [[ "$IOS_SIMULATOR_ID" =~ ^[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$ ]] ||
        { echo "IOS_SIMULATOR_ID must be a simulator UUID" >&2; exit 2; }
    destination="platform=iOS Simulator,id=$IOS_SIMULATOR_ID"
else
    simulator="$(xcrun simctl list devices available -j | python3 -c '
import json,sys
devices=json.load(sys.stdin)["devices"]
phones=[d for runtime, values in devices.items()
        if ".iOS-" in runtime and int(runtime.split(".iOS-")[1].split("-")[0]) >= 18
        for d in values if d["name"].startswith("iPhone")]
if not phones: sys.exit("No available iOS 18+ iPhone simulator; install an iOS runtime in Xcode.")
print(phones[0]["udid"])
')"
    destination="platform=iOS Simulator,id=$simulator"
fi
exec xcodebuild -project "$root/apps/ios/Holon.xcodeproj" -scheme Holon \
    -destination "$destination" "${extra_args[@]}" \
    CODE_SIGN_IDENTITY=- "$action"
