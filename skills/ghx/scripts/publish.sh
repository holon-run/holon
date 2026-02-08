#!/bin/bash
# publish.sh - GitHub publishing script for ghx skill
#
# Supports batch publish intent execution and direct single-action commands.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ -z "${GITHUB_OUTPUT_DIR:-}" ]]; then
  if [[ -d /holon/output && -w /holon/output ]]; then
    GITHUB_OUTPUT_DIR="/holon/output"
  else
    GITHUB_OUTPUT_DIR="$(mktemp -d /tmp/holon-ghpub-XXXXXX)"
  fi
fi
INTENT_FILE=""
DRY_RUN=false
FROM_INDEX=0
PR_REF=""
REPO_REF=""
DIRECT_CMD=""
DIRECT_ARGS=()
ARGS_PROVIDED=false

export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
export BLUE='\033[0;34m'
export NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $*"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_dry() { echo -e "${BLUE}[DRY-RUN]${NC} $*"; }

usage() {
  cat <<USAGE
Usage: publish.sh [OPTIONS] [COMMAND [ARGS]]

Batch mode:
  publish.sh --intent=/holon/output/publish-intent.json [OPTIONS]

Direct command mode:
  publish.sh --pr=OWNER/REPO#NUM comment --body-file summary.md
  publish.sh --pr=OWNER/REPO#NUM reply-reviews --pr-fix-json pr-fix.json
  publish.sh --pr=OWNER/REPO#NUM post-review --body-file review.md [--comments-file review.json]
  publish.sh --pr=OWNER/REPO#NUM update-pr --title "..." [--body-file summary.md]
  publish.sh --repo=OWNER/REPO create-pr --title "..." --body-file summary.md --head feature/x --base main

Global options:
  --intent=PATH         Path to publish-intent.json file
  --dry-run             Show what would be done without executing
  --from=N              Start from action N (for resume)
  --pr=OWNER/REPO#NUM   Target PR reference
  --repo=OWNER/REPO     Target repository (for create-pr)
  --help                Show this help message
USAGE
}

check_dependencies() {
  local missing=()
  if ! command -v gh >/dev/null 2>&1; then
    missing+=("gh CLI")
  elif ! gh auth status >/dev/null 2>&1; then
    missing+=("gh CLI authentication (run 'gh auth login')")
  fi
  if ! command -v jq >/dev/null 2>&1; then
    missing+=("jq")
  fi
  if ! command -v git >/dev/null 2>&1; then
    missing+=("git")
  fi
  if [[ ${#missing[@]} -gt 0 ]]; then
    log_error "Missing dependencies: ${missing[*]}"
    exit 1
  fi
}

parse_pr_ref_string() {
  local ref="$1"
  if [[ "$ref" =~ ^([^/]+)/([^#]+)#([0-9]+)$ ]]; then
    PR_OWNER="${BASH_REMATCH[1]}"
    PR_REPO="${BASH_REMATCH[2]}"
    PR_NUMBER="${BASH_REMATCH[3]}"
    return 0
  fi
  return 1
}

parse_repo_ref_string() {
  local ref="$1"
  if [[ "$ref" =~ ^([^/]+)/([^/]+)$ ]]; then
    PR_OWNER="${BASH_REMATCH[1]}"
    PR_REPO="${BASH_REMATCH[2]}"
    return 0
  fi
  return 1
}

validate_intent() {
  local intent_file="$1"
  log_info "Validating intent file: $intent_file"

  if [[ ! -f "$intent_file" ]]; then
    log_error "Intent file not found: $intent_file"
    return 1
  fi

  local version
  version=$(jq -r '.version // "1.0"' "$intent_file" 2>/dev/null)
  if [[ "$version" != "1.0" ]]; then
    log_error "Unsupported version: $version (supported: 1.0)"
    return 1
  fi

  if ! jq -e 'has("pr_ref") and has("actions")' "$intent_file" >/dev/null 2>&1; then
    log_error "Missing required fields in intent file (pr_ref, actions)"
    return 1
  fi

  local action_count
  action_count=$(jq '.actions | length' "$intent_file")
  if [[ "$action_count" -eq 0 ]]; then
    log_warn "No actions to execute in intent file"
    return 0
  fi

  for ((i=0; i<action_count; i++)); do
    local action_type
    action_type=$(jq -r ".actions[$i].type // empty" "$intent_file")
    case "$action_type" in
      create_pr|update_pr|post_comment|reply_review|post_review) ;;
      *)
        log_error "Action $i: invalid type '$action_type'"
        return 1
        ;;
    esac
  done

  log_info "Intent file validation passed"
}

parse_pr_ref_from_intent() {
  if [[ -z "$PR_REF" ]]; then
    PR_REF=$(jq -r '.pr_ref' "$INTENT_FILE")
  fi
  if [[ "$PR_REF" == "null" || -z "$PR_REF" ]]; then
    log_error "No PR reference specified and not found in intent file"
    return 1
  fi
  if ! parse_pr_ref_string "$PR_REF"; then
    log_error "Invalid PR reference format: $PR_REF (expected OWNER/REPO#NUMBER)"
    return 1
  fi
}

execute_action() {
  local _index="$1"
  local action_type="$2"
  local params="$3"

  case "$action_type" in
    create_pr) action_create_pr "$params" ;;
    update_pr) action_update_pr "$params" ;;
    post_comment) action_post_comment "$params" ;;
    reply_review) action_reply_review "$params" ;;
    post_review) action_post_review "$params" ;;
    *)
      log_error "Unknown action type: $action_type"
      return 1
      ;;
  esac
}

write_results() {
  local total="$1"
  local completed="$2"
  local failed="$3"
  local results_json="$4"

  mkdir -p "$GITHUB_OUTPUT_DIR"
  local results_file="$GITHUB_OUTPUT_DIR/publish-results.json"
  local status="failed"
  if [[ "$failed" -eq 0 ]]; then
    status="success"
  fi

  jq -n \
    --arg version "1.0" \
    --arg pr_ref "${PR_REF:-}" \
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

execute_intent() {
  local intent_file="$1"
  validate_intent "$intent_file" || return 1
  parse_pr_ref_from_intent || return 1

  local action_count
  action_count=$(jq '.actions | length' "$intent_file")
  local results_json='[]'
  local total=0 completed=0 failed=0

  source "${SCRIPT_DIR}/lib/publish.sh"

  for ((i=FROM_INDEX; i<action_count; i++)); do
    local action_type action_params
    action_type=$(jq -r ".actions[$i].type" "$intent_file")
    action_params=$(jq ".actions[$i].params // (.actions[$i] | del(.type, .description))" "$intent_file")
    total=$((total + 1))

    if [[ "$DRY_RUN" == "true" ]]; then
      log_dry "Would execute action $i: $action_type"
      results_json=$(echo "$results_json" | jq --argjson i "$i" --arg t "$action_type" '. += [{index:$i,type:$t,status:"dry-run"}]')
      continue
    fi

    if execute_action "$i" "$action_type" "$action_params" >/dev/null; then
      completed=$((completed + 1))
      results_json=$(echo "$results_json" | jq --argjson i "$i" --arg t "$action_type" '. += [{index:$i,type:$t,status:"completed"}]')
    else
      failed=$((failed + 1))
      results_json=$(echo "$results_json" | jq --argjson i "$i" --arg t "$action_type" '. += [{index:$i,type:$t,status:"failed"}]')
    fi
  done

  write_results "$total" "$completed" "$failed" "$results_json"
  show_summary "$total" "$completed" "$failed"

  if [[ "$failed" -gt 0 && "$DRY_RUN" != "true" ]]; then
    return 1
  fi
}

parse_direct_params() {
  local cmd="$1"
  shift

  local title="" body="" body_file="" head="" base="" draft="false"
  local pr_number="" state="" marker="" replies_file="" comments_file="review.json"
  local max_inline="20" post_empty="false" commit_id=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --title=*) title="${1#*=}" ;;
      --title) shift; title="${1:-}" ;;
      --body=*) body="${1#*=}" ;;
      --body) shift; body="${1:-}" ;;
      --body-file=*) body_file="${1#*=}" ;;
      --body-file) shift; body_file="${1:-}" ;;
      --head=*) head="${1#*=}" ;;
      --head) shift; head="${1:-}" ;;
      --base=*) base="${1#*=}" ;;
      --base) shift; base="${1:-}" ;;
      --draft) draft="true" ;;
      --pr-number=*) pr_number="${1#*=}" ;;
      --pr-number) shift; pr_number="${1:-}" ;;
      --state=*) state="${1#*=}" ;;
      --state) shift; state="${1:-}" ;;
      --marker=*) marker="${1#*=}" ;;
      --marker) shift; marker="${1:-}" ;;
      --pr-fix-json=*) replies_file="${1#*=}" ;;
      --pr-fix-json) shift; replies_file="${1:-}" ;;
      --replies-file=*) replies_file="${1#*=}" ;;
      --replies-file) shift; replies_file="${1:-}" ;;
      --comments-file=*) comments_file="${1#*=}" ;;
      --comments-file) shift; comments_file="${1:-}" ;;
      --max-inline=*) max_inline="${1#*=}" ;;
      --max-inline) shift; max_inline="${1:-}" ;;
      --post-empty) post_empty="true" ;;
      --commit-id=*) commit_id="${1#*=}" ;;
      --commit-id) shift; commit_id="${1:-}" ;;
      *)
        log_error "Unknown direct-command option: $1"
        return 2
        ;;
    esac
    shift
  done

  local body_value="$body"
  if [[ -n "$body_file" ]]; then
    body_value="$body_file"
  fi

  case "$cmd" in
    create-pr)
      jq -n --arg t "$title" --arg b "$body_value" --arg h "$head" --arg ba "$base" --argjson d "$draft" '{title:$t, body:$b, head:$h, base:$ba, draft:$d}'
      ;;
    update-pr)
      if [[ -z "$pr_number" ]]; then
        pr_number="$PR_NUMBER"
      fi
      jq -n --arg n "$pr_number" --arg t "$title" --arg b "$body_value" --arg s "$state" '{pr_number:($n|tonumber), title:$t, body:$b, state:$s}'
      ;;
    comment)
      jq -n --arg b "$body_value" --arg m "$marker" '{body:$b, marker:$m}'
      ;;
    reply-reviews)
      jq -n --arg rf "$replies_file" '{replies_file:$rf}'
      ;;
    post-review)
      jq -n --arg b "$body_value" --arg cf "$comments_file" --arg mi "$max_inline" --arg pe "$post_empty" --arg ci "$commit_id" '{body:$b, comments_file:$cf, max_inline:($mi|tonumber), post_empty:$pe, commit_id:$ci}'
      ;;
    *)
      log_error "Unsupported command: $cmd"
      return 2
      ;;
  esac
}

execute_direct_command() {
  local cmd="$1"
  shift

  source "${SCRIPT_DIR}/lib/publish.sh"

  case "$cmd" in
    create-pr)
      if [[ -z "$REPO_REF" ]] || ! parse_repo_ref_string "$REPO_REF"; then
        log_error "create-pr requires --repo=OWNER/REPO"
        return 2
      fi
      ;;
    update-pr|comment|reply-reviews|post-review)
      if [[ -z "$PR_REF" ]] || ! parse_pr_ref_string "$PR_REF"; then
        log_error "$cmd requires --pr=OWNER/REPO#NUMBER"
        return 2
      fi
      ;;
    *)
      log_error "Unknown command: $cmd"
      return 2
      ;;
  esac

  local action_type
  case "$cmd" in
    create-pr) action_type="create_pr" ;;
    update-pr) action_type="update_pr" ;;
    comment) action_type="post_comment" ;;
    reply-reviews) action_type="reply_review" ;;
    post-review) action_type="post_review" ;;
  esac

  local params
  params=$(parse_direct_params "$cmd" "$@") || return $?

  local results_json='[]'
  local status_text="failed"

  if [[ "$DRY_RUN" == "true" ]]; then
    log_dry "Would execute $cmd"
    results_json=$(echo "$results_json" | jq --arg t "$action_type" '. += [{index:0,type:$t,status:"dry-run"}]')
    write_results 1 1 0 "$results_json"
    return 0
  fi

  if execute_action 0 "$action_type" "$params" >/dev/null; then
    results_json=$(echo "$results_json" | jq --arg t "$action_type" '. += [{index:0,type:$t,status:"completed"}]')
    write_results 1 1 0 "$results_json"
    status_text="success"
  else
    results_json=$(echo "$results_json" | jq --arg t "$action_type" '. += [{index:0,type:$t,status:"failed"}]')
    write_results 1 0 1 "$results_json"
    status_text="failed"
  fi

  log_info "Direct command result: $status_text"
  [[ "$status_text" == "success" ]]
}

if [[ $# -gt 0 ]]; then
  ARGS_PROVIDED=true
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=true
      ;;
    --from=*)
      FROM_INDEX="${1#*=}"
      ;;
    --intent=*)
      INTENT_FILE="${1#*=}"
      ;;
    --pr=*)
      PR_REF="${1#*=}"
      ;;
    --repo=*)
      REPO_REF="${1#*=}"
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    create-pr|update-pr|comment|reply-reviews|post-review)
      DIRECT_CMD="$1"
      shift
      DIRECT_ARGS=("$@")
      break
      ;;
    *)
      log_error "Unknown option: $1"
      usage
      exit 2
      ;;
  esac
  shift
done

if [[ "$ARGS_PROVIDED" == "false" ]]; then
  usage
  exit 0
fi

main() {
  log_info "GitHub publishing script for ghx skill"
  check_dependencies

  if [[ -n "$INTENT_FILE" ]]; then
    execute_intent "$INTENT_FILE"
    exit $?
  fi

  if [[ -n "$DIRECT_CMD" ]]; then
    execute_direct_command "$DIRECT_CMD" "${DIRECT_ARGS[@]}"
    exit $?
  fi

  log_error "No mode specified. Use --intent=<file> or a direct command."
  usage
  exit 2
}

main "$@"
