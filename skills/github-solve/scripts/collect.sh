#!/bin/bash
# collect.sh - GitHub context collection script for github-solve skill
#
# This script collects GitHub context (issue/PR metadata, comments, diffs, etc.)
# inside the container using the gh CLI. It's designed to be called by the agent
# before starting work, when pre-populated context is not available.
#
# Usage: collect.sh <ref> [repo_hint]
#   ref: GitHub reference (e.g., "123", "owner/repo#123", "https://github.com/...")
#   repo_hint: Optional repository hint (e.g., "owner/repo") for numeric refs
#
# Environment variables:
#   GITHUB_CONTEXT_DIR: Output directory for collected context (default: /holon/output/github-context)
#   TRIGGER_COMMENT_ID: Optional comment ID to mark as trigger
#   INCLUDE_DIFF: Set to "true" to include PR diff (default: true)
#   INCLUDE_CHECKS: Set to "true" to include CI checks (default: true)
#
# Exit codes:
#   0: Success
#   1: Error (see error message)
#   2: Validation error

set -euo pipefail

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source helper functions
# shellcheck source=scripts/lib/helpers.sh
source "$SCRIPT_DIR/lib/helpers.sh"

# Default values
GITHUB_CONTEXT_DIR="${GITHUB_CONTEXT_DIR:-/holon/output/github-context}"
TRIGGER_COMMENT_ID="${TRIGGER_COMMENT_ID:-}"
INCLUDE_DIFF="${INCLUDE_DIFF:-true}"
INCLUDE_CHECKS="${INCLUDE_CHECKS:-true}"
UNRESOLVED_ONLY="${UNRESOLVED_ONLY:-true}"

# Show usage
usage() {
    cat <<EOF
Usage: collect.sh <ref> [repo_hint]

Collects GitHub context using gh CLI and writes to output directory.

Arguments:
  ref        GitHub reference (e.g., "123", "owner/repo#123", URL)
  repo_hint  Optional repository hint (e.g., "owner/repo") for numeric refs

Environment:
  GITHUB_CONTEXT_DIR   Output directory (default: /holon/output/github-context)
  TRIGGER_COMMENT_ID   Comment ID to mark as trigger
  INCLUDE_DIFF         Include PR diff (default: true)
  INCLUDE_CHECKS       Include CI checks (default: true)
  UNRESOLVED_ONLY      Only unresolved review threads (default: true)

Examples:
  collect.sh holon-run/holon#502
  collect.sh 502 holon-run/holon
  collect.sh https://github.com/holon-run/holon/issues/502

EOF
}

# Parse arguments
if [[ $# -lt 1 ]]; then
    log_error "Missing required argument: ref"
    usage
    exit 2
fi

REF="$1"
REPO_HINT="${2:-}"

# Check gh CLI
if ! check_gh_cli; then
    exit 1
fi

# Check jq
if ! check_jq; then
    exit 1
fi

# Parse reference
log_info "Parsing reference: $REF"
read -r OWNER REPO NUMBER REF_TYPE <<< "$(parse_ref "$REF" "$REPO_HINT")"

if [[ -z "$OWNER" || -z "$REPO" || -z "$NUMBER" ]]; then
    log_error "Failed to parse reference. Make sure ref is valid or provide repo_hint."
    exit 2
fi

log_info "Parsed: owner=$OWNER, repo=$REPO, number=$NUMBER"

# Determine ref type if unknown
if [[ "$REF_TYPE" == "unknown" ]]; then
    log_info "Determining reference type..."
    REF_TYPE=$(determine_ref_type "$OWNER" "$REPO" "$NUMBER")
fi

log_info "Reference type: $REF_TYPE"

# Create output directory
mkdir -p "$GITHUB_CONTEXT_DIR/github"

# Collect context based on type
SUCCESS=false

if [[ "$REF_TYPE" == "pr" ]]; then
    # ===== PR Context Collection =====

    # Fetch PR metadata
    if ! fetch_pr_metadata "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/pr.json"; then
        log_error "Failed to fetch PR metadata"
        write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
        exit 1
    fi

    # Fetch review threads
    if ! fetch_pr_review_threads "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/review_threads.json" "$UNRESOLVED_ONLY" "$TRIGGER_COMMENT_ID"; then
        log_warn "Failed to fetch review threads (continuing...)"
    fi

    # Fetch PR comments
    if ! fetch_pr_comments "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/comments.json" "$TRIGGER_COMMENT_ID"; then
        log_warn "Failed to fetch PR comments (continuing...)"
    fi

    # Fetch diff if requested
    if [[ "$INCLUDE_DIFF" == "true" ]]; then
        if ! fetch_pr_diff "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/pr.diff"; then
            log_warn "Failed to fetch PR diff (continuing...)"
        fi
    fi

    # Fetch check runs if requested
    if [[ "$INCLUDE_CHECKS" == "true" ]]; then
        # Get head SHA from PR metadata
        HEAD_SHA=$(jq -r '.headRefOid' "$GITHUB_CONTEXT_DIR/github/pr.json")

        if [[ -n "$HEAD_SHA" && "$HEAD_SHA" != "null" ]]; then
            if ! fetch_pr_check_runs "$OWNER" "$REPO" "$HEAD_SHA" "$GITHUB_CONTEXT_DIR/github/check_runs.json"; then
                log_warn "Failed to fetch check runs (continuing...)"
            else
                # Try to fetch workflow logs for failed checks
                if [[ -f "$GITHUB_CONTEXT_DIR/github/check_runs.json" ]]; then
                    fetch_workflow_logs "$GITHUB_CONTEXT_DIR/github" "$GITHUB_CONTEXT_DIR/github/check_runs.json"
                fi
            fi
        else
            log_warn "Could not get head SHA from PR metadata, skipping check runs"
        fi
    fi

    SUCCESS=true

elif [[ "$REF_TYPE" == "issue" ]]; then
    # ===== Issue Context Collection =====

    # Fetch issue metadata
    if ! fetch_issue_metadata "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/issue.json"; then
        log_error "Failed to fetch issue metadata"
        write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
        exit 1
    fi

    # Fetch comments
    if ! fetch_issue_comments "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/comments.json" "$TRIGGER_COMMENT_ID"; then
        log_warn "Failed to fetch issue comments (continuing...)"
    fi

    SUCCESS=true

else
    log_error "Unknown reference type: $REF_TYPE"
    write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
    exit 1
fi

# Verify required files
if ! verify_context_files "$GITHUB_CONTEXT_DIR" "$REF_TYPE" "$INCLUDE_DIFF" "$INCLUDE_CHECKS"; then
    log_error "Context verification failed"
    write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false"
    exit 1
fi

# Write manifest
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
