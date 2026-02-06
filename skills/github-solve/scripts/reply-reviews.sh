#!/bin/bash
# reply-reviews.sh - Post review replies to GitHub PR
#
# This script reads pr-fix.json and posts formatted replies to review comments.
# It implements idempotency by checking if the bot has already replied.
#
# Usage: reply-reviews.sh [OPTIONS]
#
# Options:
#   --dry-run              Show what would be posted without actually posting
#   --from=N               Start from reply N (for resume functionality)
#   --pr=OWNER/REPO#NUMBER PR reference (default: read from pr-fix.json)
#   --bot-login=NAME       Bot login name for idempotency check (default: holonbot[bot])
#   --help                 Show this help message
#
# Environment variables:
#   PR_FIX_JSON           Path to pr-fix.json (default: ./pr-fix.json)
#   HOLON_GITHUB_BOT_LOGIN Bot login name (default: holonbot[bot])
#
# Output files:
#   reply-results.json     JSON with results of each reply attempt
#
# Exit codes:
#   0: Success (all replies posted or skipped)
#   1: Error (some or all replies failed)

set -euo pipefail

# Default values
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PR_FIX_JSON="${PR_FIX_JSON:-$SCRIPT_DIR/pr-fix.json}"
RESULTS_FILE="$SCRIPT_DIR/reply-results.json"
BOT_LOGIN="${HOLON_GITHUB_BOT_LOGIN:-holonbot[bot]}"
DRY_RUN=false
FROM_INDEX=0
PR_REF=""

# Colors for output
export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
export BLUE='\033[0;34m'
export NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

log_dry() {
    echo -e "${BLUE}[DRY-RUN]${NC} $*"
}

# Show usage
usage() {
    cat <<EOF
Usage: reply-reviews.sh [OPTIONS]

Post review replies to GitHub PR based on pr-fix.json.

Options:
  --dry-run              Show what would be posted without actually posting
  --from=N               Start from reply N (for resume functionality)
  --pr=OWNER/REPO#NUMBER PR reference (default: read from pr-fix.json)
  --bot-login=NAME       Bot login name for idempotency check (default: holonbot[bot])
  --help                 Show this help message

Environment:
  PR_FIX_JSON           Path to pr-fix.json (default: ./pr-fix.json)
  HOLON_GITHUB_BOT_LOGIN Bot login name (default: holonbot[bot])

Examples:
  reply-reviews.sh --dry-run                    # Preview replies without posting
  reply-reviews.sh --pr=holon-run/holon#507    # Post replies to specific PR
  reply-reviews.sh --from=5                     # Resume from reply #5

EOF
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --from=*)
            FROM_INDEX="${1#*=}"
            shift
            ;;
        --pr=*)
            PR_REF="${1#*=}"
            shift
            ;;
        --bot-login=*)
            BOT_LOGIN="${1#*=}"
            shift
            ;;
        --help)
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

# Check if gh CLI is available
if ! command -v gh &> /dev/null; then
    log_error "gh CLI is not installed"
    exit 1
fi

# Check if jq is available
if ! command -v jq &> /dev/null; then
    log_error "jq is not installed"
    exit 1
fi

# Check if pr-fix.json exists
if [[ ! -f "$PR_FIX_JSON" ]]; then
    log_error "pr-fix.json not found at: $PR_FIX_JSON"
    exit 1
fi

# Read and parse pr-fix.json
log_info "Reading pr-fix.json from: $PR_FIX_JSON"
PR_FIX_DATA=$(cat "$PR_FIX_JSON")

# Extract PR reference from pr-fix.json if not provided
if [[ -z "$PR_REF" ]]; then
    # Try to extract from the first comment_id by looking up the PR
    # This is a fallback - ideally pr-fix.json should include the PR reference
    log_warn "No --pr specified, attempting to detect from environment"
    # For now, require explicit --pr or read from git remote
    if git rev-parse --git-dir > /dev/null 2>&1; then
        REMOTE_URL=$(git remote get-url origin 2>/dev/null || echo "")
        if [[ "$REMOTE_URL" =~ github\.com[/:]([^/]+)/([^/]+)\.git ]]; then
            OWNER="${BASH_REMATCH[1]}"
            REPO="${BASH_REMATCH[2]}"
            BRANCH=$(git branch --show-current 2>/dev/null || echo "")
            if [[ -n "$BRANCH" ]]; then
                PR_NUM=$(gh pr list --repo "$OWNER/$REPO" --head "$BRANCH" --json number --jq '.[0].number' 2>/dev/null || echo "")
                if [[ -n "$PR_NUM" ]]; then
                    PR_REF="$OWNER/$REPO#$PR_NUM"
                    log_info "Auto-detected PR: $PR_REF"
                fi
            fi
        fi
    fi

    if [[ -z "$PR_REF" ]]; then
        log_error "Could not auto-detect PR. Please specify --pr=OWNER/REPO#NUMBER"
        exit 2
    fi
fi

# Parse PR reference
if [[ "$PR_REF" =~ ^([^/]+)/([^#]+)#([0-9]+)$ ]]; then
    PR_OWNER="${BASH_REMATCH[1]}"
    PR_REPO="${BASH_REMATCH[2]}"
    PR_NUMBER="${BASH_REMATCH[3]}"
else
    log_error "Invalid PR reference format: $PR_REF (expected OWNER/REPO#NUMBER)"
    exit 2
fi

log_info "Target PR: $PR_OWNER/$PR_REPO #$PR_NUMBER"
log_info "Bot login: $BOT_LOGIN"

# Get review replies count
REPLY_COUNT=$(echo "$PR_FIX_DATA" | jq '.review_replies | length')
log_info "Found $REPLY_COUNT review replies in pr-fix.json"

if [[ "$FROM_INDEX" -gt 0 ]]; then
    log_info "Starting from reply index: $FROM_INDEX"
fi

# Initialize results array
RESULTS_JSON="[]"
POSTED=0
SKIPPED=0
FAILED=0

# Process each review reply
for ((i=FROM_INDEX; i<REPLY_COUNT; i++)); do
    REPLY=$(echo "$PR_FIX_DATA" | jq ".review_replies[$i]")
    COMMENT_ID=$(echo "$REPLY" | jq -r '.comment_id')
    STATUS=$(echo "$REPLY" | jq -r '.status')
    MESSAGE=$(echo "$REPLY" | jq -r '.message')
    ACTION_TAKEN=$(echo "$REPLY" | jq -r '.action_taken // ""')

    # Check if comment_id is valid
    if [[ "$COMMENT_ID" == "null" || -z "$COMMENT_ID" ]]; then
        log_error "Reply #$i: Missing comment_id, skipping"
        FAILED=$((FAILED + 1))
        continue
    fi

    # Check if we've already replied
    log_info "Reply #$i: Checking if bot already replied to comment $COMMENT_ID..."

    HAS_REPLIED=false
    if ! HAS_REPLIED=$(gh api "repos/$PR_OWNER/$PR_REPO/pulls/$PR_NUMBER/comments" \
        --jq "[.[] | select(.in_reply_to == $COMMENT_ID and .user.login == \"$BOT_LOGIN\")] | length" 2>/dev/null); then
        log_warn "Reply #$i: Failed to check existing replies, assuming not replied"
        HAS_REPLIED=0
    fi

    if [[ "$HAS_REPLIED" -gt 0 ]]; then
        log_info "Reply #$i: Already replied to comment $COMMENT_ID, skipping"
        SKIPPED=$((SKIPPED + 1))

        # Add to results
        RESULTS_JSON=$(echo "$RESULTS_JSON" | jq --argjson i "$i" --argjson cid "$COMMENT_ID" \
            '. += [{"index": $i, "comment_id": $cid, "status": "skipped", "reason": "Already replied"}]')
        continue
    fi

    # Format the reply message (matching Go implementation)
    EMOJI=""
    case "$STATUS" in
        fixed)
            EMOJI="✅"
            ;;
        wontfix)
            EMOJI="⚠️"
            ;;
        need-info)
            EMOJI="❓"
            ;;
        deferred)
            EMOJI="🔜"
            ;;
        *)
            EMOJI="📝"
            ;;
    esac

    # Convert status to uppercase (portable way)
    STATUS_UPPER=$(echo "$STATUS" | tr '[:lower:]' '[:upper:]')
    FORMATTED_MSG="$EMOJI **$STATUS_UPPER**: $MESSAGE"
    if [[ -n "$ACTION_TAKEN" ]]; then
        FORMATTED_MSG="$FORMATTED_MSG\n\n**Action taken**: $ACTION_TAKEN"
    fi

    # Post or preview the reply
    if [[ "$DRY_RUN" == "true" ]]; then
        log_dry "Reply #$i: Would post to comment $COMMENT_ID:"
        echo -e "$FORMATTED_MSG" | sed 's/^/    /'
        echo ""

        RESULTS_JSON=$(echo "$RESULTS_JSON" | jq --argjson i "$i" --argjson cid "$COMMENT_ID" \
            '. += [{"index": $i, "comment_id": $cid, "status": "dry-run", "message": "Would post"}]')
    else
        log_info "Reply #$i: Posting reply to comment $COMMENT_ID..."

        if gh api "repos/$PR_OWNER/$PR_REPO/pulls/$PR_NUMBER/comments/$COMMENT_ID/replies" \
            -f body="$FORMATTED_MSG" --silent 2>/dev/null; then
            log_info "Reply #$i: Successfully posted reply to comment $COMMENT_ID"
            POSTED=$((POSTED + 1))

            RESULTS_JSON=$(echo "$RESULTS_JSON" | jq --argjson i "$i" --argjson cid "$COMMENT_ID" \
                '. += [{"index": $i, "comment_id": $cid, "status": "posted"}]')
        else
            log_error "Reply #$i: Failed to post reply to comment $COMMENT_ID"
            FAILED=$((FAILED + 1))

            RESULTS_JSON=$(echo "$RESULTS_JSON" | jq --argjson i "$i" --argjson cid "$COMMENT_ID" \
                '. += [{"index": $i, "comment_id": $cid, "status": "failed", "reason": "API error"}]')
        fi
    fi
done

# Write results to file
cat > "$RESULTS_FILE" <<EOF
{
  "pr_ref": "$PR_REF",
  "bot_login": "$BOT_LOGIN",
  "dry_run": $DRY_RUN,
  "total": $REPLY_COUNT,
  "posted": $POSTED,
  "skipped": $SKIPPED,
  "failed": $FAILED,
  "details": $RESULTS_JSON
}
EOF

# Print summary
echo ""
log_info "=== Summary ==="
log_info "Total replies: $REPLY_COUNT"
if [[ "$DRY_RUN" == "true" ]]; then
    log_info "Mode: DRY RUN (no replies posted)"
else
    log_info "Posted: $POSTED"
fi
log_info "Skipped: $SKIPPED"
log_info "Failed: $FAILED"
log_info "Results written to: $RESULTS_FILE"

# Exit with error if any failures
if [[ "$FAILED" -gt 0 && "$DRY_RUN" != "true" ]]; then
    exit 1
fi

exit 0
