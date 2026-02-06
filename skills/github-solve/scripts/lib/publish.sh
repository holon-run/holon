#!/bin/bash
# lib/publish.sh - Publishing action functions for github-solve skill
#
# This library contains action implementations for the publish.sh script.
# Each action function handles a specific GitHub publishing operation.
#
# Actions:
#   - action_create_pr()      - Create a new pull request
#   - action_update_pr()      - Update an existing pull request
#   - action_post_comment()   - Post a PR-level comment
#   - action_reply_review()   - Reply to review comments
#
# Usage:
#   source "${SCRIPT_DIR}/lib/publish.sh"
#   action_create_pr "$params"

set -euo pipefail

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Source helpers if not already loaded
[[ -z "${helpers_sourced:-}" ]] && source "${SCRIPT_DIR}/lib/helpers.sh"

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

    # Check if replies_file is specified (recommended)
    if echo "$params" | jq -e '.replies_file' >/dev/null; then
        local replies_file
        replies_file=$(echo "$params" | jq -r '.replies_file')

        # Security: Validate path doesn't escape GITHUB_OUTPUT_DIR
        if [[ "$replies_file" =~ ^/ ]]; then
            log_error "Absolute paths not allowed for security: $replies_file"
            return 1
        fi

        # Resolve relative to GITHUB_OUTPUT_DIR
        local resolved_file="$GITHUB_OUTPUT_DIR/$replies_file"

        # Validate the resolved path is within GITHUB_OUTPUT_DIR
        if [[ "$resolved_file" != "$GITHUB_OUTPUT_DIR"/* ]]; then
            log_error "Invalid replies file path (outside output directory): $replies_file"
            return 1
        fi

        replies_file="$resolved_file"

        if [[ ! -f "$replies_file" ]]; then
            log_error "Replies file not found: $replies_file"
            return 1
        fi

        log_info "Using replies file: $replies_file"

        # Delegate to reply-reviews.sh
        log_info "Delegating to reply-reviews.sh..."

        # Set environment variables for reply-reviews.sh
        local old_pr_ref="$PR_REF"
        export PR_REF="$PR_OWNER/$PR_REPO#$PR_NUMBER"
        export PR_FIX_JSON="$replies_file"

        # Run reply-reviews.sh
        if bash "${SCRIPT_DIR}/reply-reviews.sh"; then
            export PR_REF="$old_pr_ref"

            # Parse and include results
            local results_file="$GITHUB_OUTPUT_DIR/reply-results.json"
            if [[ -f "$results_file" ]]; then
                log_info "Reply results: $(cat "$results_file" | jq '{total, posted, skipped, failed}')"
                cat "$results_file"
            else
                # Fallback results
                jq -n '{total: 0, posted: 0, skipped: 0, failed: 0}'
            fi
        else
            local status=$?
            export PR_REF="$old_pr_ref"
            log_error "reply-reviews.sh failed with status $status"
            return 1
        fi

        return 0
    fi

    # Inline replies (backward compatibility)
    log_info "Processing inline replies..."

    local inline_replies
    inline_replies=$(echo "$params" | jq -r '.replies // []')

    if [[ "$inline_replies" == "null" || "$inline_replies" == "[]" ]]; then
        log_error "No replies found in params"
        return 1
    fi

    # Process inline replies
    local count
    count=$(echo "$inline_replies" | jq 'length')
    log_info "Processing $count inline replies"

    local posted=0 skipped=0 failed=0

    # Process each reply
    for ((i=0; i<count; i++)); do
        local reply
        reply=$(echo "$inline_replies" | jq ".[$i]")

        local comment_id status message
        comment_id=$(echo "$reply" | jq -r '.comment_id')
        status=$(echo "$reply" | jq -r '.status // "fixed"')
        message=$(echo "$reply" | jq -r '.message // ""')

        log_info "Replying to comment #$comment_id..."

        # Format reply with emoji
        local emoji
        case "$status" in
            fixed) emoji="✅" ;;
            deferred) emoji="⏳" ;;
            wontfix) emoji="🙅" ;;
            *) emoji="📝" ;;
        esac

        local formatted_msg
        formatted_msg="$emoji **$(echo "$status" | tr '[:lower:]' '[:upper:]')**: $message"

        local action_taken
        action_taken=$(echo "$reply" | jq -r '.action_taken // ""')
        if [[ -n "$action_taken" ]]; then
            formatted_msg="$formatted_msg

**Action taken**: $action_taken"
        fi

        # Post reply using JSON format (handles newlines correctly)
        if echo "$formatted_msg" | jq -Rs '{body: .}' | \
            gh api "repos/$PR_OWNER/$PR_REPO/pulls/$PR_NUMBER/comments/$comment_id/replies" \
            --input - >/dev/null 2>&1; then
            log_info "  ✅ Posted reply"
            ((posted++))
        else
            log_warn "  ⚠️  Failed to post reply"
            ((failed++))
        fi
    done

    # Return results as JSON
    jq -n \
        --argjson total "$count" \
        --argjson posted "$posted" \
        --argjson skipped "$skipped" \
        --argjson failed "$failed" \
        '{total: $total, posted: $posted, skipped: $skipped, failed: $failed}'
}

# Mark library as sourced
helpers_sourced=publish
