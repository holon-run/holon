#!/bin/bash
# Wrapper collector for github-review -> delegates to shared github-context collector

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_COLLECTOR="$SCRIPT_DIR/../../github-context/scripts/collect.sh"

# Default output and context dirs (aligned with shared github-context layout)
if [[ -d /holon/output ]]; then
    GITHUB_OUTPUT_DIR="${GITHUB_OUTPUT_DIR:-/holon/output}"
else
    GITHUB_OUTPUT_DIR="${GITHUB_OUTPUT_DIR:-$(mktemp -d /tmp/holon-ghreview-XXXXXX)}"
fi

if [[ -z "${GITHUB_CONTEXT_DIR:-}" ]]; then
    GITHUB_CONTEXT_DIR="$GITHUB_OUTPUT_DIR/github-context"
fi

# Review prefers leaner collection (no checks by default)
export GITHUB_OUTPUT_DIR
export GITHUB_CONTEXT_DIR
export MANIFEST_PROVIDER="github-review"
export COLLECT_PROVIDER="github-review"
export INCLUDE_DIFF="${INCLUDE_DIFF:-true}"
export INCLUDE_THREADS="${INCLUDE_THREADS:-true}"
export INCLUDE_FILES="${INCLUDE_FILES:-true}"
export INCLUDE_COMMITS="${INCLUDE_COMMITS:-true}"
export INCLUDE_CHECKS="${INCLUDE_CHECKS:-false}"
export MAX_FILES="${MAX_FILES:-100}"
export TRIGGER_COMMENT_ID="${TRIGGER_COMMENT_ID:-}"

exec "$SHARED_COLLECTOR" "$@"
