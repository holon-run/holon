#!/bin/bash
# collect.sh - GitHub PR context collection script for github-review skill
#
# This script collects PR context (metadata, changed files with patches, existing reviews)
# for automated code review. It fetches only what's needed for review, not CI logs.
#
# Usage: collect.sh <pr_ref> [repo_hint]
#   pr_ref: PR reference (e.g., "123", "owner/repo#123", "https://github.com/.../pull/123")
#   repo_hint: Optional repository hint (e.g., "owner/repo") for numeric refs
#
# Environment variables:
#   GITHUB_OUTPUT_DIR: Output directory for artifacts (default: /holon/output if present, else tmp)
#   GITHUB_CONTEXT_DIR: Output directory for collected context (default: ${GITHUB_OUTPUT_DIR}/github-review-context)
#   MAX_FILES: Maximum number of files to fetch (default: 100)
#   INCLUDE_THREADS: Include existing review threads (default: true)
#
# Exit codes:
#   0: Success
#   1: Error (see error message)
#   2: Validation error

set -euo pipefail

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source helper functions from github-solve (reuse existing helpers)
# shellcheck source=../github-solve/scripts/lib/helpers.sh
source "$SCRIPT_DIR/../github-solve/scripts/lib/helpers.sh"

# Default values (prefer Holon contract path; fallback to temp to avoid polluting workspace)
if [[ -d /holon/output ]]; then
    GITHUB_OUTPUT_DIR="${GITHUB_OUTPUT_DIR:-/holon/output}"
else
    GITHUB_OUTPUT_DIR="${GITHUB_OUTPUT_DIR:-$(mktemp -d /tmp/holon-ghreview-XXXXXX)}"
fi

if [[ -z "${GITHUB_CONTEXT_DIR:-}" ]]; then
    GITHUB_CONTEXT_DIR="$GITHUB_OUTPUT_DIR/github-review-context"
fi

MAX_FILES="${MAX_FILES:-100}"
INCLUDE_THREADS="${INCLUDE_THREADS:-true}"

# Show usage
usage() {
    cat <<EOF
Usage: collect.sh <pr_ref> [repo_hint]

Collects GitHub PR context for automated code review using gh CLI.

Arguments:
  pr_ref     PR reference (e.g., "123", "owner/repo#123", URL)
  repo_hint  Optional repository hint (e.g., "owner/repo") for numeric refs

Environment:
  GITHUB_OUTPUT_DIR   Output directory for artifacts (default: /holon/output if present, else temp dir)
  GITHUB_CONTEXT_DIR  Output directory for context (default: \${GITHUB_OUTPUT_DIR}/github-review-context)
  MAX_FILES           Maximum files to fetch (default: 100)
  INCLUDE_THREADS     Include existing review threads (default: true)

Examples:
  collect.sh holon-run/holon#123
  collect.sh 123 holon-run/holon
  collect.sh https://github.com/holon-run/holon/pull/123

EOF
}

# Parse arguments
if [[ $# -lt 1 ]]; then
    log_error "Missing required argument: pr_ref"
    usage
    exit 2
fi

PR_REF="$1"
REPO_HINT="${2:-}"

# Check dependencies (gh + auth + jq)
if ! check_dependencies; then
    exit 1
fi

# Parse reference
log_info "Parsing PR reference: $PR_REF"
read -r OWNER REPO NUMBER REF_TYPE <<< "$(parse_ref "$PR_REF" "$REPO_HINT")"

if [[ -z "$OWNER" || -z "$REPO" || -z "$NUMBER" ]]; then
    log_error "Failed to parse reference. Make sure ref is valid or provide repo_hint."
    exit 2
fi

log_info "Parsed: owner=$OWNER, repo=$REPO, number=$NUMBER"

# Determine ref type if unknown - must be a PR
if [[ "$REF_TYPE" == "unknown" ]]; then
    log_info "Determining reference type..."
    REF_TYPE=$(determine_ref_type "$OWNER" "$REPO" "$NUMBER")
fi

if [[ "$REF_TYPE" != "pr" ]]; then
    log_error "github-review skill only supports pull requests, not issues (got: $REF_TYPE)"
    exit 2
fi

log_info "Reference type: $REF_TYPE"

# Create output directory
mkdir -p "$GITHUB_CONTEXT_DIR/github"

# ===== PR Context Collection for Review =====

# Fetch PR metadata
log_info "Fetching PR metadata for $OWNER/$REPO#$NUMBER..."
if ! gh pr view "$NUMBER" --repo "$OWNER/$REPO" \
    --json number,title,body,state,url,baseRefName,headRefName,headRefOid,author,createdAt,updatedAt,additions,deletions,changedFiles,mergeable \
    > "$GITHUB_CONTEXT_DIR/github/pr.json"; then
    log_error "Failed to fetch PR metadata"
    exit 1
fi

# Fetch changed files with patches
log_info "Fetching changed files for $OWNER/$REPO#$NUMBER..."
if ! gh pr view "$NUMBER" --repo "$OWNER/$REPO" \
    --json files \
    --jq '.files[:'"$MAX_FILES"']' \
    > "$GITHUB_CONTEXT_DIR/github/files.json"; then
    log_error "Failed to fetch changed files"
    exit 1
fi

# Fetch patches for each file
log_info "Fetching file patches..."
FILES_COUNT=$(jq 'length' "$GITHUB_CONTEXT_DIR/github/files.json")
log_info "Found $FILES_COUNT changed files (limit: $MAX_FILES)"

# Create a files-with-patches.json that includes patches
# Note: GitHub API doesn't provide patches in the files endpoint, so we use gh pr diff
gh pr diff "$NUMBER" --repo "$OWNER/$REPO" > "$GITHUB_CONTEXT_DIR/github/pr.diff" 2>/dev/null || {
    log_warn "Failed to fetch PR diff (may be empty or too large)"
}

# Parse diff to create file-level patches with line numbers
# This creates a more structured version of the diff for review
if [[ -f "$GITHUB_CONTEXT_DIR/github/pr.diff" && -s "$GITHUB_CONTEXT_DIR/github/pr.diff" ]]; then
    # The diff file is already there, agent can parse it
    log_info "PR diff saved for review"
fi

# Fetch existing review threads (to avoid duplicating comments)
if [[ "$INCLUDE_THREADS" == "true" ]]; then
    log_info "Fetching existing review threads..."
    # Fetch review comments
    api_path="repos/$OWNER/$REPO/pulls/$NUMBER/comments"
    if gh api "$api_path" --paginate > "$GITHUB_CONTEXT_DIR/github/review_threads.json" 2>/dev/null; then
        THREADS_COUNT=$(jq 'length' "$GITHUB_CONTEXT_DIR/github/review_threads.json")
        log_info "Found $THREADS_COUNT existing review comments"
    else
        log_warn "Failed to fetch review threads (continuing...)"
    fi
fi

# Fetch PR comments (general discussion)
log_info "Fetching PR discussion comments..."
api_path="repos/$OWNER/$REPO/issues/$NUMBER/comments"
if gh api "$api_path" --paginate > "$GITHUB_CONTEXT_DIR/github/comments.json" 2>/dev/null; then
    COMMENTS_COUNT=$(jq 'length' "$GITHUB_CONTEXT_DIR/github/comments.json")
    log_info "Found $COMMENTS_COUNT PR discussion comments"
else
    log_warn "Failed to fetch PR comments (continuing...)"
fi

# Fetch commits (optional, useful for understanding change context)
log_info "Fetching PR commits..."
if gh api "repos/$OWNER/$REPO/pulls/$NUMBER/commits" --paginate > "$GITHUB_CONTEXT_DIR/github/commits.json" 2>/dev/null; then
    COMMITS_COUNT=$(jq 'length' "$GITHUB_CONTEXT_DIR/github/commits.json")
    log_info "Found $COMMITS_COUNT commits"
else
    log_warn "Failed to fetch commits (continuing...)"
fi

# Write manifest
MANIFEST_FILE="$GITHUB_CONTEXT_DIR/manifest.json"
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

cat > "$MANIFEST_FILE" <<EOF
{
  "provider": "github-review",
  "kind": "pr",
  "ref": "$OWNER/$REPO#$NUMBER",
  "owner": "$OWNER",
  "repo": "$REPO",
  "number": $NUMBER,
  "collected_at": "$TIMESTAMP",
  "max_files": $MAX_FILES,
  "include_threads": $INCLUDE_THREADS,
  "success": true
}
EOF

log_info "Wrote collection manifest to $MANIFEST_FILE"

# Verify required files
REQUIRED_FILES=(
    "$GITHUB_CONTEXT_DIR/github/pr.json"
    "$GITHUB_CONTEXT_DIR/github/files.json"
)

for file in "${REQUIRED_FILES[@]}"; do
    if [[ ! -f "$file" ]]; then
        log_error "Required context file missing: $file"
        exit 1
    fi
    if [[ ! -s "$file" ]]; then
        log_error "Required context file is empty: $file"
        exit 1
    fi
done

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
