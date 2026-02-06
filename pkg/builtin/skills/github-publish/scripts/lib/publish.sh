#!/bin/bash
# lib/publish.sh - Publishing action functions for github-publish skill
#
# This library contains action implementations for the publish.sh script.
# Each action function handles a specific GitHub publishing operation.
#
# Actions:
#   - action_create_pr()      - Create a new pull request
#   - action_update_pr()      - Update an existing pull request
#   - action_post_comment()   - Post a PR-level comment
#   - action_reply_review()   - Reply to review comments
#   - action_post_review()    - Post a PR review with inline comments
#
# Usage:
#   source "${SCRIPT_DIR}/lib/publish.sh"
#   action_create_pr "$params"

set -euo pipefail

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Source helpers if not already loaded
[[ -z "${helpers_sourced:-}" ]] && source "${SCRIPT_DIR}/../../github-context/scripts/lib/helpers.sh"

# ============================================================================
# Helper Functions
# ============================================================================

# Find existing comment by marker
# Usage: find_existing_comment <pr_number> <marker>
# Output: Comment ID if found, empty string otherwise
find_existing_comment() {
    local pr_number="$1"
    local marker="$2"

    log_info "Looking for existing comment with marker: $marker"

    # Get all comments for the PR
    local comments
    if ! comments=$(gh api "repos/$PR_OWNER/$PR_REPO/issues/$pr_number/comments" 2>/dev/null); then
        log_warn "Failed to fetch comments for PR #$pr_number"
        echo ""
        return 0
    fi

    # Search for comment with marker
    local comment_id
    comment_id=$(echo "$comments" | jq -r --arg marker "$marker" '
        map(select(.body | contains($marker)) | .id) | .[0] // empty
    ')

    if [[ -n "$comment_id" ]]; then
        log_info "Found existing comment: #$comment_id"
        echo "$comment_id"
    else
        log_info "No existing comment found with marker"
        echo ""
    fi
}

# Parse file parameter (handles inline content and file references)
# Usage: parse_body_param <params> <output_var>
# Output: Body content (either inline or from file)
parse_body_param() {
    local params="$1"
    local body_param

    # Check if it's a file reference (ends in .md or contains newlines)
    body_param=$(echo "$params" | jq -r '.body // empty')

    if [[ -z "$body_param" || "$body_param" == "null" ]]; then
        log_error "Missing 'body' parameter"
        return 1
    fi

    # Check if it's a file path (ends in .md)
    if [[ "$body_param" =~ \.md$ ]]; then
        # Resolve and validate file path within GITHUB_OUTPUT_DIR
        local resolved_file
        if [[ "$body_param" =~ ^/ ]]; then
            # Absolute path - reject for security
            log_error "Absolute paths not allowed for security: $body_param"
            return 1
        fi

        # Resolve relative to GITHUB_OUTPUT_DIR
        resolved_file="$GITHUB_OUTPUT_DIR/$body_param"

        # Validate the resolved path is within GITHUB_OUTPUT_DIR
        if [[ "$resolved_file" != "$GITHUB_OUTPUT_DIR"/* ]]; then
            log_error "Invalid file path (outside output directory): $body_param"
            return 1
        fi

        # Read from file if it exists
        if [[ -f "$resolved_file" ]]; then
            cat "$resolved_file"
        else
            log_error "File not found: $resolved_file"
            return 1
        fi
    else
        # Inline content (not a .md file)
        echo "$body_param"
    fi
}

# ============================================================================
# Action: Create PR
# ============================================================================

# Create a new pull request
# Usage: action_create_pr <params_json>
# Params:
#   - title (required): PR title
#   - body (required): PR description (file path or inline)
#   - head (required): Head branch
#   - base (required): Base branch
#   - draft (optional): Create as draft PR (default: false)
#   - labels (optional): Array of label strings
# Output: PR number, URL, and creation status
action_create_pr() {
    local params="$1"

    log_info "Creating pull request..."

    # Extract required parameters
    local title body head base draft labels
    title=$(echo "$params" | jq -r '.title // empty')
    head=$(echo "$params" | jq -r '.head // empty')
    base=$(echo "$params" | jq -r '.base // empty')
    draft=$(echo "$params" | jq -r '.draft // "false"')
    labels=$(echo "$params" | jq -r '.labels // []')

    # Validate required parameters
    if [[ -z "$title" ]]; then
        log_error "Missing required parameter: title"
        return 1
    fi

    if [[ -z "$head" ]]; then
        log_error "Missing required parameter: head"
        return 1
    fi

    if [[ -z "$base" ]]; then
        log_error "Missing required parameter: base"
        return 1
    fi

    # Get body content (from file or inline)
    local body_content
    body_content=$(parse_body_param "$params")
    if [[ $? -ne 0 ]]; then
        log_error "Failed to read PR body"
        return 1
    fi

    log_info "PR Title: $title"
    log_info "PR Branch: $head -> $base"
    log_info "Draft: $draft"

    # Check if PR already exists (idempotency)
    log_info "Checking if PR already exists for branch: $head"
    local existing_pr
    existing_pr=$(gh pr list --head "$head" --repo "$PR_OWNER/$PR_REPO" --json number,title --jq '.[0] // empty' 2>/dev/null || echo "")

    if [[ -n "$existing_pr" ]]; then
        local existing_number
        existing_number=$(echo "$existing_pr" | jq -r '.number')
        log_info "PR already exists: #$existing_number"

        # Return existing PR info
        jq -n \
            --argjson number "$existing_number" \
            --arg url "https://github.com/$PR_OWNER/$PR_REPO/pull/$existing_number" \
            --argjson created false \
            '{pr_number: $number, pr_url: $url, created: $created, message: "PR already exists"}'
        return 0
    fi

    # Check if head branch exists (best-effort, don't fail for remote branches)
    log_info "Verifying head branch (best-effort): $head"
    if ! git rev-parse --verify "$head" >/dev/null 2>&1; then
        log_warn "Unable to verify local branch '$head'; it may be remote-only or a cross-fork ref. Proceeding and letting 'gh pr create' validate the head."
    fi

    # Build gh pr create command
    local cmd=(gh pr create)
    cmd+=("--repo" "$PR_OWNER/$PR_REPO")
    cmd+=("--title" "$title")
    cmd+=("--body" "$body_content")
    cmd+=("--base" "$base")
    cmd+=("--head" "$head")

    if [[ "$draft" == "true" ]]; then
        cmd+=("--draft")
    fi

    # Add labels if provided
    if [[ "$labels" != "null" && "$labels" != "[]" ]]; then
        local label_list
        label_list=$(echo "$labels" | jq -r 'join(",")')
        cmd+=("--label" "$label_list")
    fi

    # Create PR
    log_info "Creating PR..."
    local pr_json
    if ! pr_json=$("${cmd[@]}" --json number,url 2>&1); then
        log_error "Failed to create PR: $pr_json"
        return 1
    fi

    # Extract PR info from structured JSON output
    local pr_number pr_url
    pr_number=$(echo "$pr_json" | jq -r '.number')
    pr_url=$(echo "$pr_json" | jq -r '.url')

    log_info "✅ Created PR #$pr_number"
    log_info "   URL: $pr_url"

    # Return PR info as JSON
    jq -n \
        --argjson number "$pr_number" \
        --arg url "$pr_url" \
        --argjson created true \
        '{pr_number: $number, pr_url: $url, created: $created}'
}

# ============================================================================
# Action: Update PR
# ============================================================================

# Update an existing pull request
# Usage: action_update_pr <params_json>
# Params:
#   - pr_number (required): PR number to update
#   - title (optional): New PR title
#   - body (optional): New PR description (file path or inline)
#   - state (optional): New state (open/closed)
# Output: Updated PR info
action_update_pr() {
    local params="$1"

    log_info "Updating pull request..."

    # Extract required parameters
    local pr_number title body state
    pr_number=$(echo "$params" | jq -r '.pr_number // empty')
    title=$(echo "$params" | jq -r '.title // empty')
    state=$(echo "$params" | jq -r '.state // empty')

    # Validate required parameters
    if [[ -z "$pr_number" ]]; then
        log_error "Missing required parameter: pr_number"
        return 1
    fi

    # Validate at least one field to update
    if [[ -z "$title" && -z "$state" ]] && ! echo "$params" | jq -e '.body' >/dev/null; then
        log_error "No fields to update (need title, body, or state)"
        return 1
    fi

    log_info "Updating PR #$pr_number..."

    # Build gh pr edit command
    local cmd=(gh pr edit "$pr_number" --repo "$PR_OWNER/$PR_REPO")

    if [[ -n "$title" ]]; then
        cmd+=("--title" "$title")
        log_info "  New title: $title"
    fi

    if [[ -n "$state" ]]; then
        if [[ "$state" == "open" || "$state" == "closed" ]]; then
            cmd+=("--state" "$state")
            log_info "  New state: $state"
        else
            log_warn "Invalid state: $state (must be 'open' or 'closed')"
        fi
    fi

    # Handle body parameter
    if echo "$params" | jq -e '.body' >/dev/null; then
        local body_content
        body_content=$(parse_body_param "$params")
        if [[ $? -eq 0 ]]; then
            cmd+=("--body" "$body_content")
            log_info "  Body updated"
        fi
    fi

    # Update PR
    local result
    if ! result=$("${cmd[@]}" 2>&1); then
        log_error "Failed to update PR: $result"
        return 1
    fi

    log_info "✅ Updated PR #$pr_number"

    # Return updated PR info as JSON
    jq -n \
        --argjson number "$pr_number" \
        --arg url "https://github.com/$PR_OWNER/$PR_REPO/pull/$pr_number" \
        '{pr_number: $number, pr_url: $url, updated: true}'
}

# ============================================================================
# Action: Post Comment
# ============================================================================

# Post a PR-level comment
# Usage: action_post_comment <params_json>
# Params:
#   - body (required): Comment content (file path or inline)
#   - marker (optional): Unique marker for idempotency (default: auto-generated)
# Output: Comment ID and whether it was created or updated
action_post_comment() {
    local params="$1"

    log_info "Posting PR comment..."

    # Get body content
    local body_content
    body_content=$(parse_body_param "$params")
    if [[ $? -ne 0 ]]; then
        log_error "Failed to read comment body"
        return 1
    fi

    # Get or generate marker for idempotency
    local marker
    marker=$(echo "$params" | jq -r '.marker // "holon-publish-marker"')

    # Add marker to body if not present
    if [[ ! "$body_content" =~ $marker ]]; then
        body_content="$body_content

<!-- $marker -->"
    fi

    log_info "Looking for existing comment with marker..."

    # Find existing comment
    local existing_id
    existing_id=$(find_existing_comment "$PR_NUMBER" "$marker")

    if [[ -n "$existing_id" ]]; then
        # Update existing comment
        log_info "Updating existing comment #$existing_id..."

        if gh api "repos/$PR_OWNER/$PR_REPO/issues/comments/$existing_id" \
            -X PATCH \
            -f body="$body_content" >/dev/null 2>&1; then
            log_info "✅ Updated comment #$existing_id"

            jq -n \
                --argjson comment_id "$existing_id" \
                --argjson updated true \
                '{comment_id: $comment_id, updated: $updated}'
        else
            log_error "Failed to update comment #$existing_id"
            return 1
        fi
    else
        # Create new comment
        log_info "Creating new comment..."

        local comment_id
        if ! comment_id=$(gh api "repos/$PR_OWNER/$PR_REPO/issues/$PR_NUMBER/comments" \
            -X POST \
            -f body="$body_content" \
            -q '.id' 2>/dev/null); then
            log_error "Failed to create comment"
            return 1
        fi

        log_info "✅ Created comment #$comment_id"

        jq -n \
            --argjson comment_id "$comment_id" \
            --argjson updated false \
            '{comment_id: $comment_id, updated: $updated}'
    fi
}

# ============================================================================
# Action: Reply Review
# ============================================================================

# Reply to review comments
# Usage: action_reply_review <params_json>
# Params:
#   - replies_file (recommended): Path to pr-fix.json file
#   - replies (optional): Array of inline reply objects
# Output: Summary of replies posted
action_reply_review() {
    local params="$1"

    log_info "Processing review replies..."

    # Load replies from file if provided
    local replies_json="[]"
    if echo "$params" | jq -e '.replies_file' >/dev/null; then
        local replies_file
        replies_file=$(echo "$params" | jq -r '.replies_file')

        if [[ "$replies_file" =~ ^/ ]]; then
            log_error "Absolute paths not allowed for replies_file: $replies_file"
            return 1
        fi
        local resolved_file="$GITHUB_OUTPUT_DIR/$replies_file"
        if [[ "$resolved_file" != "$GITHUB_OUTPUT_DIR"/* ]]; then
            log_error "Invalid replies_file path (outside output dir): $replies_file"
            return 1
        fi
        if [[ ! -f "$resolved_file" ]]; then
            log_error "Replies file not found: $resolved_file"
            return 1
        fi
        replies_json=$(jq -c '.review_replies // []' "$resolved_file" 2>/dev/null || echo "[]")
    else
        replies_json=$(echo "$params" | jq -c '.replies // []')
    fi

    if [[ "$replies_json" == "[]" ]]; then
        log_warn "No replies provided"
        jq -n '{total:0, posted:0, skipped:0, failed:0}'
        return 0
    fi

    local count posted skipped failed
    count=$(echo "$replies_json" | jq 'length')
    posted=0; skipped=0; failed=0

    for reply in $(echo "$replies_json" | jq -c '.[]'); do
        local comment_id status message action_taken
        comment_id=$(echo "$reply" | jq -r '.comment_id // empty')
        status=$(echo "$reply" | jq -r '.status // "info"')
        message=$(echo "$reply" | jq -r '.message // empty')
        action_taken=$(echo "$reply" | jq -r '.action_taken // empty')

        if [[ -z "$comment_id" || -z "$message" ]]; then
            log_warn "Skipping reply missing comment_id or message"
            ((skipped++))
            continue
        fi

        local emoji="📝"
        case "$status" in
            fixed) emoji="✅" ;;
            deferred) emoji="⏳" ;;
            wontfix) emoji="🙅" ;;
            need-info|need_info|needinfo) emoji="❓" ;;
        esac

        local body="$emoji **$(echo "$status" | tr '[:lower:]' '[:upper:]')**: $message"
        if [[ -n "$action_taken" ]]; then
            body="$body

**Action taken**: $action_taken"
        fi

        log_info "Replying to comment_id: $comment_id"
        if echo "$body" | jq -Rs '{body: .}' | gh api "repos/$PR_OWNER/$PR_REPO/pulls/comments/$comment_id/replies" --input - >/dev/null 2>&1; then
            ((posted++))
        else
            log_warn "Failed to post reply for comment_id $comment_id"
            ((failed++))
        fi
    done

    jq -n         --argjson total "$count"         --argjson posted "$posted"         --argjson skipped "$skipped"         --argjson failed "$failed"         '{total:$total, posted:$posted, skipped:$skipped, failed:$failed}'
}

# ============================================================================
# Action: Post Review (body + inline comments)
# ============================================================================

# Usage: action_post_review <params_json>
# Params:
#   - body (required): review body (inline or .md file relative to GITHUB_OUTPUT_DIR)
#   - comments_file (optional): path to review.json (default: review.json in output dir)
#   - max_inline (optional): limit inline comments (default: 20)
#   - post_empty (optional): post even with zero findings (default: false)
#   - commit_id (optional): head SHA; fetched if missing
action_post_review() {
    local params="$1"

    local comments_file max_inline post_empty commit_id
    comments_file=$(echo "$params" | jq -r '.comments_file // "review.json"')
    max_inline=$(echo "$params" | jq -r '.max_inline // 20')
    post_empty=$(echo "$params" | jq -r '.post_empty // "false"')
    commit_id=$(echo "$params" | jq -r '.commit_id // empty')

    # Resolve body
    local body_content
    body_content=$(parse_body_param "$params") || return 1

    # Resolve comments file path (relative to output dir)
    if [[ "$comments_file" =~ ^/ ]]; then
        log_error "Absolute paths not allowed for comments_file: $comments_file"
        return 1
    fi
    local comments_path="$GITHUB_OUTPUT_DIR/$comments_file"

    local inline_comments=()
    if [[ -f "$comments_path" ]]; then
        while IFS= read -r finding; do
            local path line message suggestion
            path=$(echo "$finding" | jq -r '.path // empty')
            line=$(echo "$finding" | jq -r '.line // empty')
            message=$(echo "$finding" | jq -r '.message // empty')
            suggestion=$(echo "$finding" | jq -r '.suggestion // empty')

            if [[ -z "$path" || -z "$line" || -z "$message" ]]; then
                log_warn "Skipping finding missing path/line/message"
                continue
            fi
            if ! [[ "$line" =~ ^[0-9]+$ ]]; then
                log_warn "Skipping finding with non-numeric line: $line"
                continue
            fi

            local comment_body="$message"
            if [[ -n "$suggestion" ]]; then
                comment_body="$comment_body\n\n**Suggestion:** $suggestion"
            fi

            local comment_json
            comment_json=$(jq -n \
                --arg path "$path" \
                --argjson line "$line" \
                --arg body "$comment_body" \
                '{path:$path, line:$line, side:"RIGHT", body:$body}')
            inline_comments+=("$comment_json")
        done < <(jq -c '.[]' "$comments_path" 2>/dev/null)
    else
        log_warn "comments_file not found, proceeding with summary-only review: $comments_path"
    fi

    # Limit inline
    if [[ ${#inline_comments[@]} -gt "$max_inline" ]]; then
        log_warn "Found ${#inline_comments[@]} inline comments, limiting to $max_inline"
        inline_comments=("${inline_comments[@]:0:$max_inline}")
    fi

    if [[ ${#inline_comments[@]} -eq 0 && "$post_empty" != "true" ]]; then
        log_info "No findings and post_empty=false; skipping review post."
        return 0
    fi

    # Get commit id if missing
    if [[ -z "$commit_id" ]]; then
        commit_id=$(gh pr view "$PR_NUMBER" --repo "$PR_OWNER/$PR_REPO" --json headRefOid -q '.headRefOid' 2>/dev/null || true)
    fi
    if [[ -z "$commit_id" ]]; then
        log_error "Unable to determine commit_id for review"
        return 1
    fi

    # Build comments JSON
    local comments_json="[]"
    if [[ ${#inline_comments[@]} -gt 0 ]]; then
        comments_json=$(printf '%s\n' "${inline_comments[@]}" | jq -s '.')
    fi

    # Build payload
    local payload
    payload=$(cat <<EOF
{
  "commit_id": "$commit_id",
  "body": $(printf '%s' "$body_content" | jq -Rs .),
  "event": "COMMENT",
  "comments": $comments_json
}
EOF
)

    if [[ "$DRY_RUN" == "true" ]]; then
        log_dry "Would post review to $PR_OWNER/$PR_REPO#$PR_NUMBER with ${#inline_comments[@]} inline comments"
        return 0
    fi

    local response
    if ! response=$(gh api -X POST "repos/$PR_OWNER/$PR_REPO/pulls/$PR_NUMBER/reviews" --input - <<<"$payload" 2>&1); then
        log_error "Failed to post review"
        log_error "$response"
        return 1
    fi

    local review_id
    review_id=$(echo "$response" | jq -r '.id // empty' 2>/dev/null)
    log_info "Posted review (id: ${review_id:-unknown}) with ${#inline_comments[@]} inline comments"
    return 0
}

# Mark library as sourced
helpers_sourced=publish
