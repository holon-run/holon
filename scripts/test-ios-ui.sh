#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:---ui}"
[[ $# -le 1 && ( "$mode" == --sdk-only || "$mode" == --ui ) ]] || { echo 'Usage: test-ios-ui.sh [--sdk-only|--ui]' >&2; exit 2; }
cargo build --bin holon
binary="${CARGO_TARGET_DIR:-$PWD/target}/debug/holon"
python3 scripts/ios_ui_fixture.py "$binary" "$PWD" "$mode"
