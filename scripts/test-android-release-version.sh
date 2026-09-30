#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "$0")" && pwd)"
check_version() {
  local actual
  actual="$(bash "$script_dir/android-release-version.sh" "$1")"
  [[ "$actual" == "$(printf 'version_name=%s\nversion_code=%s' "$2" "$3")" ]]
}
check_version v0.46.0 0.46.0 46000
check_version 0.46.1 0.46.1 46001
check_version 0.47.0 0.47.0 47000
check_version 1.0.0 1.0.0 1000000
check_version 2099.999.999 2099.999.999 2099999999
for invalid in 0.0.0 01.2.3 1.02.3 1.2.03 v1.2 1.2.3-beta 1.1000.0 1.0.1000 2100.0.0 999999999999999999999.0.0 '1.2.3;exit'; do
  if bash "$script_dir/android-release-version.sh" "$invalid" >/dev/null 2>&1; then
    echo "Unexpectedly accepted version: $invalid" >&2
    exit 1
  fi
done
echo 'Android release version tests passed.'
