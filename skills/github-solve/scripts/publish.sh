#!/bin/bash
# publish.sh - GitHub publishing script for holon github-solve skill
#
# This script provides unified GitHub publishing capabilities by executing
# declarative publishing intents or individual commands.
#
# Usage:
#   # Batch mode (with intent file)
#   publish.sh --intent=/holon/output/publish-intent.json
#
#   # Direct command mode
#   publish.sh create-pr --title "Feature X" --body-file pr.md --head feature/x --base main
#   publish.sh comment --body-file summary.md
#   publish.sh reply-reviews --pr-fix-json pr-fix.json
#
# Environment variables:
#   GITHUB_OUTPUT_DIR    - Output directory (default: /holon/output)
#   HOLON_GITHUB_BOT_LOGIN - Bot login name (default: holonbot[bot])
#   GITHUB_TOKEN           - GitHub authentication token

set -euo pipefail

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Default values
GITHUB_OUTPUT_DIR="${GITHUB_OUTPUT_DIR:-/holon/output}"
INTENT_FILE=""
DRY_RUN=false
FROM_INDEX=0
PR_REF=""
ARGS_PROVIDED=false

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
Usage: publish.sh [OPTIONS] | <COMMAND> [ARGS]

GitHub publishing script for holon github-solve skill.

BATCH MODE (execute multiple actions from intent file):
  publish.sh --intent=/holon/output/publish-intent.json [OPTIONS]

DIRECT COMMAND MODE (execute single action):
  publish.sh create-pr [OPTIONS]
  publish.sh update-pr [OPTIONS]
  publish.sh comment [OPTIONS]
  publish.sh reply-reviews [OPTIONS]

OPTIONS:
  --intent=PATH         Path to publish-intent.json file
  --dry-run              Show what would be done without executing
  --from=N               Start from action N (for resume)
  --pr=OWNER/REPO#NUM    Target PR reference (auto-detected if not specified)
  --help                 Show this help message

ENVIRONMENT:
  GITHUB_OUTPUT_DIR     Output directory for artifacts (default: /holon/output)
  HOLON_GITHUB_BOT_LOGIN Bot login name for idempotency (default: holonbot[bot])
  GITHUB_TOKEN          GitHub authentication token (required for publishing)

COMMANDS:
  create-pr    Create a new pull request
  update-pr    Update an existing pull request
  comment      Post a PR-level comment
  reply-reviews Reply to review comments

EXAMPLES:
  # Batch mode - execute all actions from intent
  publish.sh --intent=/holon/output/publish-intent.json

  # Direct command - create PR
  publish.sh create-pr \
      --title "Feature: GitHub publishing" \
      --body-file pr-description.md \
      --head feature/github-publishing \
      --base main

  # Direct command - post comment
  publish.sh comment --body-file summary.md

  # Direct command - reply to reviews
  publish.sh reply-reviews --pr-fix-json pr-fix.json

  # Dry-run mode
  publish.sh --dry-run --intent=/holon/output/publish-intent.json

For more information on intent file format, see SKILL.md.

EOF
}

# Parse command line arguments
if [[ $# -gt 0 ]]; then
    ARGS_PROVIDED=true
fi

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
        --intent=*)
            INTENT_FILE="${1#*=}"
            shift
            ;;
        --pr=*)
            PR_REF="${1#*=}"
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        create-pr|update-pr|comment|reply-reviews)
            # Direct command mode (not yet implemented)
            log_error "Direct command mode '$1' is not yet supported."
            log_error "Please use --intent mode instead. See SKILL.md for examples."
            exit 1
            ;;
        *)
            log_error "Unknown option: $1"
            usage
            exit 2
            ;;
    esac
done

# If no arguments provided, show help
if [[ "$ARGS_PROVIDED" == "false" ]]; then
    usage
    exit 0
fi

# Check dependencies
check_dependencies() {
    local missing=()

    # Check gh CLI
    if ! command -v gh &> /dev/null; then
        missing+=("gh CLI")
    fi

    # Check gh authentication
    if command -v gh &> /dev/null; then
        if ! gh auth status &> /dev/null; then
            missing+=("gh CLI authentication (run 'gh auth login')")
        fi
    fi

    # Check jq
    if ! command -v jq &> /dev/null; then
        missing+=("jq")
    fi

    # Check git (for some operations)
    if ! command -v git &> /dev/null; then
        missing+=("git")
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing dependencies: ${missing[*]}"
        exit 1
    fi
}

# Validate intent file
validate_intent() {
    local intent_file="$1"

    log_info "Validating intent file: $intent_file"

    # Check if file exists
    if [[ ! -f "$intent_file" ]]; then
        log_error "Intent file not found: $intent_file"
        return 1
    fi

    # Check version
    local version
    version=$(jq -r '.version' "$intent_file" 2>/dev/null)
    if [[ "$version" == "null" || -z "$version" ]]; then
        log_error "Missing or invalid 'version' field in intent file"
        return 1
    fi

    if [[ "$version" != "1.0" ]]; then
        log_error "Unsupported version: $version (supported: 1.0)"
        return 1
    fi

    # Check required fields
    if ! jq -e 'has("version") and has("pr_ref") and has("actions")' "$intent_file" >/dev/null 2>&1; then
        log_error "Missing required fields in intent file (version, pr_ref, actions)"
        return 1
    fi

    # Validate actions
    local action_count
    action_count=$(jq '.actions | length' "$intent_file")

    if [[ "$action_count" -eq 0 ]]; then
        log_warn "No actions to execute in intent file"
        return 0
    fi

    log_info "Found $action_count actions to execute"

    # Validate each action type
    for ((i=0; i<action_count; i++)); do
        local action
        action=$(jq ".actions[$i]" "$intent_file")
        local action_type
        action_type=$(echo "$action" | jq -r '.type // empty')

        if [[ -z "$action_type" ]]; then
            log_error "Action $i: Missing 'type' field"
            return 1
        fi

        case "$action_type" in
            create_pr|update_pr|post_comment|reply_review)
                # Valid action type
                ;;
            *)
                log_error "Action $i: Invalid type '$action_type' (must be: create_pr, update_pr, post_comment, reply_review)"
                return 1
                ;;
        esac
    done

    log_info "Intent file validation passed"
    return 0
}

# Parse PR reference
parse_pr_ref() {
    # Already parsed
    if [[ -n "$PR_REF" ]]; then
        return 0
    fi

    # Extract from intent file if not specified
    PR_REF=$(jq -r '.pr_ref' "$INTENT_FILE")
    if [[ "$PR_REF" == "null" || -z "$PR_REF" ]]; then
        log_error "No PR reference specified and not found in intent file"
        return 1
    fi

    log_info "PR reference: $PR_REF"
    return 0
}

# Execute intent file
execute_intent() {
    local intent_file="$1"

    # Validate intent
    if ! validate_intent "$intent_file"; then
        return 1
    fi

    # Parse PR reference
    if ! parse_pr_ref "$PR_REF"; then
        return 1
    fi

    # Parse owner/repo/number
    if [[ "$PR_REF" =~ ^([^/]+)/([^#]+)#([0-9]+)$ ]]; then
        PR_OWNER="${BASH_REMATCH[1]}"
        PR_REPO="${BASH_REMATCH[2]}"
        PR_NUMBER="${BASH_REMATCH[3]}"
    else
        log_error "Invalid PR reference format: $PR_REF (expected OWNER/REPO#NUMBER)"
        return 1
    fi

    log_info "Target PR: $PR_OWNER/$PR_REPO #$PR_NUMBER"

    # Get actions
    local action_count
    action_count=$(jq '.actions | length' "$intent_file")

    # Initialize results
    local results_json="[]"
    local total=0
    local completed=0
    local failed=0

    # Source publish functions once (before the action loop)
    source "${SCRIPT_DIR}/lib/publish.sh"

    # Execute each action
    for ((i=FROM_INDEX; i<action_count; i++)); do
        local action
        action=$(jq ".actions[$i]" "$intent_file")
        local action_type
        local description
        action_type=$(echo "$action" | jq -r '.type')
        description=$(echo "$action" | jq -r '.description // "No description"')

        local action_params
        action_params=$(echo "$action" | jq '.params')

        total=$((total + 1))

        log_info "Action $i: $action_type - $description"

        if [[ "$DRY_RUN" == "true" ]]; then
            log_dry "Would execute: $action_type"
            results_json=$(echo "$results_json" | jq --argjson i "$i" --arg type "$action_type" \
                '. += [{"index": $i, "type": $type, "status": "dry-run"}]')
        else
            # Execute action
            local result
            result=$(execute_action "$i" "$action_type" "$action_params")
            local status=$?

            if [[ $status -eq 0 ]]; then
                completed=$((completed + 1))
                results_json=$(echo "$results_json" | jq --argjson i "$i" --arg type "$action_type" \
                    '. += [{"index": $i, "type": $type, "status": "completed"}]')
            else
                failed=$((failed + 1))
                results_json=$(echo "$results_json" | jq --argjson i "$i" --arg type "$action_type" \
                    '. += [{"index": $i, "type": $type, "status": "failed"}]')
            fi
        fi
    done

    # Write results
    write_results "$total" "$completed" "$failed" "$results_json"

    # Show summary
    show_summary "$total" "$completed" "$failed"

    # Exit with error if any failures
    if [[ "$failed" -gt 0 && "$DRY_RUN" != "true" ]]; then
        return 1
    fi

    return 0
}

# Execute single action
execute_action() {
    local index="$1"
    local action_type="$2"
    local params="$3"

    case "$action_type" in
        create_pr)
            action_create_pr "$params"
            ;;
        update_pr)
            action_update_pr "$params"
            ;;
        post_comment)
            action_post_comment "$params"
            ;;
        reply_review)
            action_reply_review "$params"
            ;;
        *)
            log_error "Unknown action type: $action_type"
            return 1
            ;;
    esac
}

# Write results JSON
write_results() {
    local total="$1"
    local completed="$2"
    local failed="$3"
    local results_json="$4"

    # Ensure output directory exists
    mkdir -p "$GITHUB_OUTPUT_DIR"

    local results_file="$GITHUB_OUTPUT_DIR/publish-results.json"

    # Use jq for safe JSON construction (prevents injection)
    local status
    if [[ $failed -eq 0 ]]; then
        status="success"
    else
        status="failed"
    fi

    jq -n \
        --arg version "1.0" \
        --arg pr_ref "$PR_REF" \
        --arg executed_at "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
        --argjson dry_run "$DRY_RUN" \
        --argjson actions "$results_json" \
        --argjson total "$total" \
        --argjson completed "$completed" \
        --argjson failed "$failed" \
        --arg status "$status" \
        '{
            version: $version,
            pr_ref: $pr_ref,
            executed_at: $executed_at,
            dry_run: $dry_run,
            actions: $actions,
            summary: {total: $total, completed: $completed, failed: $failed},
            overall_status: $status
        }' > "$results_file"

    log_info "Results written to: $results_file"
}

# Show summary
show_summary() {
    local total="$1"
    local completed="$2"
    local failed="$3"

    echo ""
    log_info "=== Summary ==="
    log_info "Total actions: $total"
    log_info "Completed: $completed"
    log_info "Failed: $failed"
    echo ""
}

# Main flow
main() {
    log_info "GitHub publishing script for github-solve skill"

    # Check dependencies
    if ! check_dependencies; then
        exit 1
    fi

    # Batch mode: execute intent file
    if [[ -n "$INTENT_FILE" ]]; then
        execute_intent "$INTENT_FILE"
        exit $?
    fi

    # If we reach here, no valid mode was specified
    log_error "No mode specified. Use --intent=<file> or a direct command."
    usage
    exit 2
}

# Run main
main "$@"
