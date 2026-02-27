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
#   GITHUB_CONTEXT_DIR   Output directory (default: ${GITHUB_OUTPUT_DIR}/github-context if set, else tmp)
#   GITHUB_OUTPUT_DIR    Optional base output directory used when GITHUB_CONTEXT_DIR is unset
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
    if [[ -n "${GITHUB_OUTPUT_DIR:-}" ]]; then
        GITHUB_CONTEXT_DIR="${GITHUB_OUTPUT_DIR}/github-context"
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
ARTIFACTS_JSON='[]'
MANIFEST_NOTES_JSON='[]'

append_manifest_note() {
    local message="$1"
    MANIFEST_NOTES_JSON=$(jq -c --arg message "$message" '. + [$message]' <<< "$MANIFEST_NOTES_JSON")
}

record_artifact() {
    local artifact_id="$1"
    local artifact_path="$2"
    local artifact_status="$3"
    local artifact_format="$4"
    local artifact_description="$5"
    local required_for_csv="${6:-}"

    local required_for_json='[]'
    if [[ -n "$required_for_csv" ]]; then
        required_for_json=$(printf '%s' "$required_for_csv" | tr ',' '\n' | sed '/^[[:space:]]*$/d' | jq -R . | jq -s .)
    fi

    ARTIFACTS_JSON=$(jq -c \
      --arg id "$artifact_id" \
      --arg path "$artifact_path" \
      --arg status "$artifact_status" \
      --arg format "$artifact_format" \
      --arg description "$artifact_description" \
      --argjson required_for "$required_for_json" \
      '. + [{
        id: $id,
        path: $path,
        required_for: $required_for,
        status: $status,
        format: $format,
        description: $description
      }]' <<< "$ARTIFACTS_JSON")
}

usage() {
    cat <<EOF
Usage: collect.sh <ref> [repo_hint]

Collects GitHub context using gh CLI and writes to output directory.

Arguments:
  ref        GitHub reference (e.g., "123", "owner/repo#123", URL)
  repo_hint  Optional repository hint (e.g., "owner/repo") for numeric refs

Environment:
  GITHUB_CONTEXT_DIR   Output directory (default: \${GITHUB_OUTPUT_DIR}/github-context if set, else temp dir)
  GITHUB_OUTPUT_DIR    Optional base output directory used when GITHUB_CONTEXT_DIR is unset
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
        record_artifact "pr_metadata" "github/pr.json" "error" "json" "Pull request metadata and head/base refs." "review,pr_fix"
        append_manifest_note "Failed to fetch PR metadata."
        write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false" "$ARTIFACTS_JSON" "$MANIFEST_NOTES_JSON"
        exit 1
    fi
    record_artifact "pr_metadata" "github/pr.json" "present" "json" "Pull request metadata and head/base refs." "review,pr_fix"

    if [[ "$INCLUDE_FILES" == "true" ]]; then
        if fetch_pr_files "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/files.json" "$MAX_FILES"; then
            record_artifact "files" "github/files.json" "present" "json" "Changed files list with per-file stats." "review,pr_fix"
        else
            log_warn "Failed to fetch PR files (continuing...)"
            record_artifact "files" "github/files.json" "error" "json" "Changed files list with per-file stats." "review,pr_fix"
            append_manifest_note "Failed to fetch PR files."
        fi
    else
        record_artifact "files" "github/files.json" "missing" "json" "Changed files list with per-file stats." "review,pr_fix"
        append_manifest_note "Skipped files collection because INCLUDE_FILES=false."
    fi

    if [[ "$INCLUDE_THREADS" == "true" ]]; then
        if fetch_pr_review_threads "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/review_threads.json" "$UNRESOLVED_ONLY" "$TRIGGER_COMMENT_ID"; then
            record_artifact "review_threads" "github/review_threads.json" "present" "json" "Existing review threads for deduplication." "review,pr_fix"
        else
            log_warn "Failed to fetch review threads (continuing...)"
            record_artifact "review_threads" "github/review_threads.json" "error" "json" "Existing review threads for deduplication." "review,pr_fix"
            append_manifest_note "Failed to fetch review threads."
        fi
    else
        record_artifact "review_threads" "github/review_threads.json" "missing" "json" "Existing review threads for deduplication." "review,pr_fix"
        append_manifest_note "Skipped review thread collection because INCLUDE_THREADS=false."
    fi

    if fetch_pr_comments "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/comments.json" "$TRIGGER_COMMENT_ID"; then
        record_artifact "comments" "github/comments.json" "present" "json" "Issue-style PR comments from the discussion timeline." "review,pr_fix"
    else
        log_warn "Failed to fetch PR comments (continuing...)"
        record_artifact "comments" "github/comments.json" "error" "json" "Issue-style PR comments from the discussion timeline." "review,pr_fix"
        append_manifest_note "Failed to fetch PR comments."
    fi

    if [[ "$INCLUDE_DIFF" == "true" ]]; then
        if fetch_pr_diff "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/pr.diff"; then
            record_artifact "diff" "github/pr.diff" "present" "text" "Unified diff for the pull request." "review,pr_fix"
        else
            log_warn "Failed to fetch PR diff (continuing...)"
            record_artifact "diff" "github/pr.diff" "error" "text" "Unified diff for the pull request." "review,pr_fix"
            append_manifest_note "Failed to fetch PR diff."
        fi
    else
        record_artifact "diff" "github/pr.diff" "missing" "text" "Unified diff for the pull request." "review,pr_fix"
        append_manifest_note "Skipped diff collection because INCLUDE_DIFF=false."
    fi

    if [[ "$INCLUDE_CHECKS" == "true" ]]; then
        HEAD_SHA=$(jq -r '.headRefOid' "$GITHUB_CONTEXT_DIR/github/pr.json")
        if [[ -n "$HEAD_SHA" && "$HEAD_SHA" != "null" ]]; then
            if fetch_pr_check_runs "$OWNER" "$REPO" "$HEAD_SHA" "$GITHUB_CONTEXT_DIR/github/check_runs.json"; then
                record_artifact "check_runs" "github/check_runs.json" "present" "json" "Check runs on the PR head SHA." "review,pr_fix"
                if [[ -f "$GITHUB_CONTEXT_DIR/github/check_runs.json" ]]; then
                    fetch_workflow_logs "$GITHUB_CONTEXT_DIR/github" "$GITHUB_CONTEXT_DIR/github/check_runs.json" || {
                        log_warn "Failed to fetch workflow logs (continuing...)"
                        append_manifest_note "Failed to fetch workflow logs for failed check runs."
                    }
                fi
            else
                log_warn "Failed to fetch check runs (continuing...)"
                record_artifact "check_runs" "github/check_runs.json" "error" "json" "Check runs on the PR head SHA." "review,pr_fix"
                append_manifest_note "Failed to fetch check runs."
            fi
        else
            log_warn "Could not get head SHA from PR metadata, skipping check runs"
            record_artifact "check_runs" "github/check_runs.json" "missing" "json" "Check runs on the PR head SHA." "review,pr_fix"
            append_manifest_note "Skipped check runs because headRefOid was not available."
        fi
    else
        record_artifact "check_runs" "github/check_runs.json" "missing" "json" "Check runs on the PR head SHA." "review,pr_fix"
        append_manifest_note "Skipped check run collection because INCLUDE_CHECKS=false."
    fi

    if [[ "$INCLUDE_COMMITS" == "true" ]]; then
        if fetch_pr_commits "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/commits.json"; then
            record_artifact "commits" "github/commits.json" "present" "json" "Commit list and metadata for the pull request." "review,pr_fix"
        else
            log_warn "Failed to fetch commits (continuing...)"
            record_artifact "commits" "github/commits.json" "error" "json" "Commit list and metadata for the pull request." "review,pr_fix"
            append_manifest_note "Failed to fetch PR commits."
        fi
    else
        record_artifact "commits" "github/commits.json" "missing" "json" "Commit list and metadata for the pull request." "review,pr_fix"
        append_manifest_note "Skipped commit collection because INCLUDE_COMMITS=false."
    fi

    SUCCESS=true

elif [[ "$REF_TYPE" == "issue" ]]; then
    if ! fetch_issue_metadata "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/issue.json"; then
        log_error "Failed to fetch issue metadata"
        record_artifact "issue_metadata" "github/issue.json" "error" "json" "Issue metadata including title/body/state." "issue_solve"
        append_manifest_note "Failed to fetch issue metadata."
        write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false" "$ARTIFACTS_JSON" "$MANIFEST_NOTES_JSON"
        exit 1
    fi
    record_artifact "issue_metadata" "github/issue.json" "present" "json" "Issue metadata including title/body/state." "issue_solve"

    if fetch_issue_comments "$OWNER" "$REPO" "$NUMBER" "$GITHUB_CONTEXT_DIR/github/comments.json" "$TRIGGER_COMMENT_ID"; then
        record_artifact "comments" "github/comments.json" "present" "json" "Issue comments in chronological order." "issue_solve"
    else
        log_warn "Failed to fetch issue comments (continuing...)"
        record_artifact "comments" "github/comments.json" "error" "json" "Issue comments in chronological order." "issue_solve"
        append_manifest_note "Failed to fetch issue comments."
    fi

    SUCCESS=true
else
    log_error "Unknown reference type: $REF_TYPE"
    append_manifest_note "Unknown reference type: $REF_TYPE"
    write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false" "$ARTIFACTS_JSON" "$MANIFEST_NOTES_JSON"
    exit 1
fi

if ! verify_context_files "$GITHUB_CONTEXT_DIR" "$REF_TYPE" "$INCLUDE_DIFF" "$INCLUDE_CHECKS" "$INCLUDE_FILES" "$INCLUDE_COMMITS" "$INCLUDE_THREADS"; then
    log_error "Context verification failed"
    append_manifest_note "Context verification failed."
    write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "false" "$ARTIFACTS_JSON" "$MANIFEST_NOTES_JSON"
    exit 1
fi

write_manifest "$GITHUB_CONTEXT_DIR" "$OWNER" "$REPO" "$NUMBER" "$REF_TYPE" "true" "$ARTIFACTS_JSON" "$MANIFEST_NOTES_JSON"

log_info "Context collection complete!"
log_info "Context written to: $GITHUB_CONTEXT_DIR"
echo ""
log_info "Collected files:"
find "$GITHUB_CONTEXT_DIR" -type f | sort | while read -r file; do
    rel_path="${file#$GITHUB_CONTEXT_DIR/}"
    size=$(wc -c < "$file")
    echo "  - $rel_path ($size bytes)"
done
echo ""
log_info "Artifact summary:"
echo "$ARTIFACTS_JSON" | jq -r '.[] | "  - \(.id): \(.path) [\(.status)] - \(.description)"'

if [[ "$(jq 'length' <<< "$MANIFEST_NOTES_JSON")" -gt 0 ]]; then
    echo ""
    log_info "Collection notes:"
    echo "$MANIFEST_NOTES_JSON" | jq -r '.[] | "  - \(.)"'
fi

exit 0
