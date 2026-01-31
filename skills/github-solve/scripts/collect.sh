#!/bin/bash
# Wrapper collector for github-solve -> delegates to shared github-context collector

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_COLLECTOR="$SCRIPT_DIR/../../github-context/scripts/collect.sh"

# Default context dir mirrors previous github-solve behavior
if [[ -z "${GITHUB_CONTEXT_DIR:-}" ]]; then
    if [[ -d /holon/output ]]; then
        GITHUB_CONTEXT_DIR="/holon/output/github-context"
    else
        GITHUB_CONTEXT_DIR="$(mktemp -d /tmp/holon-ghctx-XXXXXX)"
    fi
fi

# Solve wants full fidelity (diff, checks, threads, files, commits)
export GITHUB_CONTEXT_DIR
export MANIFEST_PROVIDER="github-solve"
export COLLECT_PROVIDER="github-solve"
export INCLUDE_DIFF="${INCLUDE_DIFF:-true}"
export INCLUDE_CHECKS="${INCLUDE_CHECKS:-true}"
export INCLUDE_THREADS="${INCLUDE_THREADS:-true}"
export INCLUDE_FILES="${INCLUDE_FILES:-true}"
export INCLUDE_COMMITS="${INCLUDE_COMMITS:-true}"
export MAX_FILES="${MAX_FILES:-200}"
export TRIGGER_COMMENT_ID="${TRIGGER_COMMENT_ID:-}"
export UNRESOLVED_ONLY="${UNRESOLVED_ONLY:-false}"

exec "$SHARED_COLLECTOR" "$@"
