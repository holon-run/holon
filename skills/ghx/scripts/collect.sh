#!/bin/bash
# collect.sh - Shared GitHub context collection script (issues + PRs)
#
# Used by ghx directly and wrapped by github-issue-solve/github-pr-fix/github-review.
# Provides toggles for diff, checks, threads, files, and commits.
#
# Usage: collect.sh <ref> [repo_hint]
#   ref: GitHub reference ("123", "owner/repo#123", full URL)
#   repo_hint: Optional owner/repo when ref is numeric
#
# Environment:
#   GITHUB_CONTEXT_DIR   Output directory (default: /holon/output/github-context if present, else tmp)
#   TRIGGER_COMMENT_ID   Comment ID to mark as trigger
#   INCLUDE_DIFF         Include PR diff (default: true)
#   INCLUDE_CHECKS       Include check runs + logs (default: true)
#   INCLUDE_THREADS      Include review comment threads (default: true)
#   INCLUDE_FILES        Include changed files list (default: true)
#   INCLUDE_COMMITS      Include PR commits (default: true)
#   MAX_FILES            Max files to fetch (default: 200)
#   UNRESOLVED_ONLY      Deprecated; kept for compatibility (default: false)
#   MANIFEST_PROVIDER    Provider name written to manifest (default: ghx)
#   COLLECT_PROVIDER     Alias for MANIFEST_PROVIDER (if set)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=./lib/helpers.sh
source "$SCRIPT_DIR/lib/helpers.sh"

# Default provider for manifest
if [[ -n "${COLLECT_PROVIDER:-}" && -z "${MANIFEST_PROVIDER:-}" ]]; then
    MANIFEST_PROVIDER="$COLLECT_PROVIDER"
fi
MANIFEST_PROVIDER="${MANIFEST_PROVIDER:-ghx}"

# Default output directory
if [[ -z "${GITHUB_CONTEXT_DIR:-}" ]]; then
    if [[ -d /holon/output ]]; then
        GITHUB_CONTEXT_DIR="/holon/output/github-context"
    else
        GITHUB_CONTEXT_DIR="$(mktemp -d /tmp/holon-ghctx-XXXXXX)"
    fi
fi

TRIGGER_COMMENT_ID="${TRIGGER_COMMENT_ID:-}"
INCLUDE_DIFF="${INCLUDE_DIFF:-true}"
INCLUDE_CHECKS="${INCLUDE_CHECKS:-true}"
INCLUDE_THREADS="${INCLUDE_THREADS:-true}"
INCLUDE_FILES="${INCLUDE_FILES:-true}"
INCLUDE_COMMITS="${INCLUDE_COMMITS:-true}"
MAX_FILES="${MAX_FILES:-200}"
UNRESOLVED_ONLY="${UNRESOLVED_ONLY:-false}"

usage() {
    cat <<EOF
Usage: collect.sh <ref> [repo_hint]

Collects GitHub context using gh CLI and writes to output directory.

Arguments:
  ref        GitHub reference (e.g., "123", "owner/repo#123", URL)
  repo_hint  Optional repository hint (e.g., "owner/repo") for numeric refs

Environment:
  GITHUB_CONTEXT_DIR   Output directory (default: /holon/output/github-context if present, else temp dir)
  TRIGGER_COMMENT_ID   Comment ID to mark as trigger
  INCLUDE_DIFF         Include PR diff (default: true)
  INCLUDE_CHECKS       Include CI checks (default: true)
  INCLUDE_THREADS      Include review threads (default: true)
  INCLUDE_FILES        Include changed files list (default: true)
  INCLUDE_COMMITS      Include PR commits (default: true)
  MAX_FILES            Max files to fetch when INCLUDE_FILES=true (default: 200)
  UNRESOLVED_ONLY      Deprecated; unresolved filtering is not supported (ignored)

Examples:
  collect.sh holon-run/holon#502
  collect.sh 502 holon-run/holon
  collect.sh https://github.com/holon-run/holon/issues/502

EOF
}

if [[ $# -lt 1 ]]; then
    log_error "Missing required argument: ref"
    usage
    exit 2
fi

REF="$1"
REPO_HINT="${2:-}"

if ! check_dependencies; then
    exit 1
fi

log_info "Parsing reference: $REF"
read -r OWNER REPO NUMBER REF_TYPE <<< "$(parse_ref "$REF" "$REPO_HINT")"

if [[ -z "$OWNER" || -z "$REPO" || -z "$NUMBER" ]]; then
    log_error "Failed to parse reference. Make sure ref is valid or provide repo_hint."
    exit 2
fi

log_info "Parsed: owner=$OWNER, repo=$REPO, number=$NUMBER"

if [[ "$REF_TYPE" == "unknown" ]]; then
    log_info "Determining reference type..."
    REF_TYPE=$(determine_ref_type "$OWNER" "$REPO" "$NUMBER")
fi

log_info "Reference type: $REF_TYPE"

mkdir -p "$GITHUB_CONTEXT_DIR/github"

SUCCESS=false

if [[ "$REF_TYPE" == "pr" ]]; then
    if ! fetch_pr_metadata "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/pr.json"; then
        log_error "Failed to fetch PR metadata"
        write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
        exit 1
    fi

    if [[ "$INCLUDE_FILES" == "true" ]]; then
        fetch_pr_files "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/files.json" "$MAX_FILES" || log_warn "Failed to fetch PR files (continuing...)"
    fi

    if [[ "$INCLUDE_THREADS" == "true" ]]; then
        fetch_pr_review_threads "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/review_threads.json" "$UNRESOLVED_ONLY" "$TRIGGER_COMMENT_ID" || log_warn "Failed to fetch review threads (continuing...)"
    fi

    fetch_pr_comments "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/comments.json" "$TRIGGER_COMMENT_ID" || log_warn "Failed to fetch PR comments (continuing...)"

    if [[ "$INCLUDE_DIFF" == "true" ]]; then
        fetch_pr_diff "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/pr.diff" || log_warn "Failed to fetch PR diff (continuing...)"
    fi

    if [[ "$INCLUDE_CHECKS" == "true" ]]; then
        HEAD_SHA=$(jq -r '.headRefOid' "$GITHUB_CONTEXT_DIR/github/pr.json")
        if [[ -n "$HEAD_SHA" && "$HEAD_SHA" != "null" ]]; then
            fetch_pr_check_runs "$OWNER" "$REPO" "$HEAD_SHA" "$GITHUB_CONTEXT_DIR/github/check_runs.json" || log_warn "Failed to fetch check runs (continuing...)"
            if [[ -f "$GITHUB_CONTEXT_DIR/github/check_runs.json" ]]; then
                fetch_workflow_logs "$GITHUB_CONTEXT_DIR/github" "$GITHUB_CONTEXT_DIR/github/check_runs.json"
            fi
        else
            log_warn "Could not get head SHA from PR metadata, skipping check runs"
        fi
    fi

    if [[ "$INCLUDE_COMMITS" == "true" ]]; then
        fetch_pr_commits "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/commits.json" || log_warn "Failed to fetch commits (continuing...)"
    fi

    SUCCESS=true

elif [[ "$REF_TYPE" == "issue" ]]; then
    if ! fetch_issue_metadata "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/issue.json"; then
        log_error "Failed to fetch issue metadata"
        write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
        exit 1
    fi

    fetch_issue_comments "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/comments.json" "$TRIGGER_COMMENT_ID" || log_warn "Failed to fetch issue comments (continuing...)"

    SUCCESS=true
else
    log_error "Unknown reference type: $REF_TYPE"
    write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
    exit 1
fi

if ! verify_context_files "$GITHUB_CONTEXT_DIR" "$REF_TYPE" "$INCLUDE_DIFF" "$INCLUDE_CHECKS" "$INCLUDE_FILES" "$INCLUDE_COMMITS" "$INCLUDE_THREADS"; then
    log_error "Context verification failed"
    write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
    exit 1
fi

write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "true"

log_info "Context collection complete!"
log_info "Context written to: $GITHUB_CONTEXT_DIR"
echo ""
log_info "Collected files:"
find "$GITHUB_CONTEXT_DIR" -type f | sort | while read -r file; do
    rel_path="${file#$GITHUB_CONTEXT_DIR/}"
    size=$(wc -c < "$file")
    echo "  - $rel_path ($size bytes)"
done

exit 0
