#!/bin/bash
# publish.sh - GitHub PR review publishing script for github-review skill
#
# This script reads agent-generated review artifacts and posts a structured PR review
# with inline comments and summary via GitHub API.
#
# Usage: publish.sh [options]
#
# Options:
#   --dry-run              Preview review without posting
#   --max-inline=N         Maximum inline comments to post (default: 20)
#   --post-empty           Post review even if no findings (default: false, silent if empty)
#   --pr=OWNER/REPO#NUMBER Target PR (required unless in DRY_RUN with context)
#
# Environment variables:
#   GITHUB_OUTPUT_DIR: Directory containing review artifacts (default: /holon/output if present)
#   DRY_RUN: Set to "true" to preview without posting
#   MAX_INLINE: Maximum inline comments (default: 20)
#   POST_EMPTY: Post review even with no findings (default: false)
#
# Required artifacts:
#   ${GITHUB_OUTPUT_DIR}/review.json      - Structured review findings
#   ${GITHUB_OUTPUT_DIR}/review.md        - Review summary in markdown
#   ${GITHUB_OUTPUT_DIR}/github-context/manifest.json - Collection manifest
#
# Exit codes:
#   0: Success
#   1: Error (see error message)
#   2: Validation error

set -euo pipefail

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source helper functions from shared github-context
# shellcheck source=../../github-context/scripts/lib/helpers.sh
source "$SCRIPT_DIR/../../github-context/scripts/lib/helpers.sh"

# Default values
if [[ -d /holon/output ]]; then
    GITHUB_OUTPUT_DIR="${GITHUB_OUTPUT_DIR:-/holon/output}"
else
    GITHUB_OUTPUT_DIR="${GITHUB_OUTPUT_DIR:-$(mktemp -d /tmp/holon-ghreview-XXXXXX)}"
fi

DRY_RUN="${DRY_RUN:-false}"
MAX_INLINE="${MAX_INLINE:-20}"
POST_EMPTY="${POST_EMPTY:-false}"
PR_REF=""

# Show usage
usage() {
    cat <<EOF
Usage: publish.sh [options]

Posts a GitHub PR review from agent-generated artifacts.

Options:
  --dry-run              Preview review without posting
  --max-inline=N         Maximum inline comments to post (default: 20)
  --post-empty           Post review even if no findings (default: false)
  --pr=OWNER/REPO#NUMBER Target PR (can be omitted if manifest exists)

Environment:
  GITHUB_OUTPUT_DIR   Directory containing review artifacts (default: /holon/output if present)
  DRY_RUN             Preview without posting (default: false)
  MAX_INLINE          Maximum inline comments (default: 20)
  POST_EMPTY          Post review even with no findings (default: false)

Required artifacts:
  \${GITHUB_OUTPUT_DIR}/review.json      - Structured review findings
  \${GITHUB_OUTPUT_DIR}/review.md        - Review summary in markdown
  \${GITHUB_OUTPUT_DIR}/github-review-context/manifest.json - Collection manifest

Examples:
  publish.sh --dry-run
  publish.sh --max-inline=10
  publish.sh --pr=holon-run/holon#123

EOF
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --max-inline=*)
            MAX_INLINE="${1#*=}"
            shift
            ;;
        --post-empty)
            POST_EMPTY=true
            shift
            ;;
        --pr=*)
            PR_REF="${1#*=}"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            usage
            exit 2
            ;;
    esac
done

# Check dependencies
if ! check_dependencies; then
    exit 1
fi

# Define artifact paths
REVIEW_JSON="$GITHUB_OUTPUT_DIR/review.json"
REVIEW_MD="$GITHUB_OUTPUT_DIR/review.md"
MANIFEST_JSON="$GITHUB_OUTPUT_DIR/github-context/manifest.json"
SUMMARY_MD="$GITHUB_OUTPUT_DIR/summary.md"

# Verify required artifacts
log_info "Verifying required artifacts..."

MISSING=()

if [[ ! -f "$REVIEW_JSON" ]]; then
    MISSING+=("review.json")
fi

if [[ ! -f "$REVIEW_MD" ]]; then
    MISSING+=("review.md")
fi

if [[ ! -f "$MANIFEST_JSON" ]]; then
    MISSING+=("github-context/manifest.json")
fi

if [[ ${#MISSING[@]} -gt 0 ]]; then
    log_error "Missing required artifacts: ${MISSING[*]}"
    log_error "Expected location: $GITHUB_OUTPUT_DIR"
    exit 2
fi

# Read manifest to get PR reference
if [[ -z "$PR_REF" ]]; then
    if ! PR_REF=$(jq -r '.ref' "$MANIFEST_JSON" 2>/dev/null); then
        log_error "Failed to read PR reference from manifest"
        exit 2
    fi
    log_info "Using PR reference from manifest: $PR_REF"
fi

# Parse PR reference
log_info "Parsing PR reference: $PR_REF"
read -r OWNER REPO NUMBER REF_TYPE <<< "$(parse_ref "$PR_REF" "")"

if [[ -z "$OWNER" || -z "$REPO" || -z "$NUMBER" ]]; then
    log_error "Failed to parse PR reference: $PR_REF"
    exit 2
fi

log_info "Target PR: $OWNER/$REPO#$NUMBER"

# Read review findings
log_info "Reading review findings..."

if ! FINDINGS_COUNT=$(jq 'length' "$REVIEW_JSON" 2>/dev/null); then
    log_error "Failed to parse review.json"
    exit 2
fi

log_info "Found $FINDINGS_COUNT review findings"

# Check if should post empty review
if [[ "$FINDINGS_COUNT" -eq 0 && "$POST_EMPTY" != "true" ]]; then
    log_info "No findings to review and POST_EMPTY=false, exiting silently"
    exit 0
fi

# Read review summary
if ! REVIEW_BODY=$(cat "$REVIEW_MD"); then
    log_error "Failed to read review.md"
    exit 2
fi

# Prepare review comments
log_info "Preparing review comments..."

# Extract inline comments from review.json
# Expected format: [{"path": "file.go", "line": 42, "severity": "error", "message": "...", "suggestion": "..."}]
INLINE_COMMENTS=()

while IFS= read -r finding; do
    FILE_PATH=$(echo "$finding" | jq -r '.path // empty')
    LINE=$(echo "$finding" | jq -r '.line // empty')
    MESSAGE=$(echo "$finding" | jq -r '.message // empty')
    SUGGESTION=$(echo "$finding" | jq -r '.suggestion // empty')

    if [[ -n "$FILE_PATH" && -n "$LINE" && -n "$MESSAGE" ]]; then
        # Validate LINE is numeric
        if ! [[ "$LINE" =~ ^[0-9]+$ ]]; then
            log_warn "Skipping finding with invalid line number: $LINE"
            continue
        fi

        # Build comment body
        COMMENT_BODY="$MESSAGE"
        if [[ -n "$SUGGESTION" ]]; then
            COMMENT_BODY="$COMMENT_BODY

**Suggestion:** $SUGGESTION"
        fi

        # Build JSON object safely with jq to ensure proper escaping
        COMMENT_JSON=$(jq -n \
            --arg path "$FILE_PATH" \
            --argjson line "$LINE" \
            --arg body "$COMMENT_BODY" \
            '{path: $path, line: $line, side: "RIGHT", body: $body}')

        INLINE_COMMENTS+=("$COMMENT_JSON")
    fi
done < <(jq -c '.[]' "$REVIEW_JSON" 2>/dev/null)

# Limit inline comments
if [[ ${#INLINE_COMMENTS[@]} -gt "$MAX_INLINE" ]]; then
    log_warn "Found ${#INLINE_COMMENTS[@]} inline comments, limiting to $MAX_INLINE"
    INLINE_COMMENTS=("${INLINE_COMMENTS[@]:0:$MAX_INLINE}")
fi

log_info "Prepared ${#INLINE_COMMENTS[@]} inline comments (max: $MAX_INLINE)"

# Dry run mode
if [[ "$DRY_RUN" == "true" ]]; then
    log_info "DRY RUN MODE - Not posting review"
    echo ""
    echo "=== Review Preview ==="
    echo ""
    echo "PR: $OWNER/$REPO#$NUMBER"
    echo "Findings: $FINDINGS_COUNT"
    echo "Inline Comments: ${#INLINE_COMMENTS[@]}"
    echo ""
    echo "=== Review Body ==="
    echo "$REVIEW_BODY"
    echo ""
    echo "=== Inline Comments ==="
    for comment in "${INLINE_COMMENTS[@]}"; do
        echo "$comment" | jq -r '"\(.path):\(.line): \(.body)"'
    done
    echo ""
    exit 0
fi

# Post review via GitHub API
log_info "Posting review to $OWNER/$REPO#$NUMBER..."

# Build review comments JSON
COMMENTS_JSON="[]"
if [[ ${#INLINE_COMMENTS[@]} -gt 0 ]]; then
    # Join comments with commas
    COMMENTS_JSON=$(printf '%s\n' "${INLINE_COMMENTS[@]}" | jq -s '.')
fi

# Get PR head SHA for review
HEAD_SHA=$(jq -r '.headRefOid // empty' "$GITHUB_OUTPUT_DIR/github-context/github/pr.json" 2>/dev/null)

if [[ -z "$HEAD_SHA" ]]; then
    log_warn "Could not get head SHA from PR metadata, fetching from API..."
    if ! HEAD_SHA=$(gh pr view "$NUMBER" --repo "$OWNER/$REPO" --json headRefOid -q '.headRefOid' 2>/dev/null); then
        log_error "Failed to get PR head SHA"
        exit 1
    fi
fi

log_info "PR head SHA: $HEAD_SHA"

# Escape review body for JSON
REVIEW_BODY_ESCAPED=$(echo "$REVIEW_BODY" | jq -Rs .)

# Build review payload
PAYLOAD=$(cat <<EOF
{
  "commit_id": "$HEAD_SHA",
  "body": $REVIEW_BODY_ESCAPED,
  "event": "COMMENT",
  "comments": $COMMENTS_JSON
}
EOF
)

# Post review using GitHub API
API_PATH="repos/$OWNER/$REPO/pulls/$NUMBER/reviews"

log_info "Posting review via API..."
if ! RESPONSE=$(gh api -X POST "$API_PATH" --input - <<< "$PAYLOAD" 2>&1); then
    log_error "Failed to post review"
    log_error "API response: $RESPONSE"
    exit 1
fi

# Get review ID from response
REVIEW_ID=$(echo "$RESPONSE" | jq -r '.id // empty' 2>/dev/null)

if [[ -n "$REVIEW_ID" ]]; then
    log_info "Review posted successfully! Review ID: $REVIEW_ID"
else
    log_warn "Review posted but could not extract review ID from response"
fi

# Create summary
log_info "Creating summary..."

cat > "$SUMMARY_MD" <<EOF
# PR Review Published

**PR:** $OWNER/$REPO#$NUMBER
**Review ID:** ${REVIEW_ID:-"unknown"}
**Findings:** $FINDINGS_COUNT
**Inline Comments Posted:** ${#INLINE_COMMENTS[@]}

## Review Summary

$REVIEW_BODY

## Posted Comments

${#INLINE_COMMENTS[@]} inline comments were posted on the PR.

EOF

log_info "Summary written to $SUMMARY_MD"

exit 0
