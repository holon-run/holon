#!/bin/bash
# test_helper_drift.sh - ensure github helpers stay centralized

set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$TEST_DIR/../.." && pwd)"

helpers=($(find "$REPO_ROOT/skills" -path '*/scripts/lib/helpers.sh' | sort))
expected="$REPO_ROOT/skills/github-context/scripts/lib/helpers.sh"

if [[ ${#helpers[@]} -ne 1 || "${helpers[0]}" != "$expected" ]]; then
    echo "[ERROR] Expected single shared helpers.sh at $expected" >&2
    echo "[ERROR] Found:" >&2
    printf '  - %s\n' "${helpers[@]}" >&2
    exit 1
fi

echo "[INFO] Helper drift check passed (single shared helpers.sh)"
