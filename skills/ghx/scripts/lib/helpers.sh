#!/bin/bash
# helpers.sh - Reusable helper functions for GitHub context collection
#
# Shared by ghx, github-issue-solve, github-pr-fix, and github-review skills.

set -euo pipefail

# Prevent double sourcing
helpers_sourced=true

# Color output for better readability
export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
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

# Check CLI dependencies (gh + auth) and jq
check_dependencies() {
    local missing=()

    if ! command -v gh &> /dev/null; then
        missing+=("gh CLI")
    elif ! gh auth status &> /dev/null; then
        missing+=("gh auth (run 'gh auth login' or set GITHUB_TOKEN/GH_TOKEN)")
    fi

    if ! command -v jq &> /dev/null; then
        missing+=("jq")
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing dependencies: ${missing[*]}"
        return 1
    fi

    return 0
}

# Parse GitHub reference and extract owner, repo, and number
# Usage: parse_ref <ref> [repo_hint]
# Outputs: OWNER REPO NUMBER REF_TYPE
parse_ref() {
    local ref="$1"
    local repo_hint="${2:-}"
    local owner="" repo="" number="" ref_type=""

    # Default from repo_hint if provided
    if [[ -n "$repo_hint" ]]; then
        owner=$(echo "$repo_hint" | cut -d'/' -f1)
        repo=$(echo "$repo_hint" | cut -d'/' -f2)
    fi

    # Check if ref is a URL
    if [[ "$ref" =~ github\.com ]]; then
        # Extract parts from URL
        # https://github.com/owner/repo/pull/123 or /issues/123
        local path
        path=$(echo "$ref" | sed -E 's|^https?://github\.com/||' | sed 's|/$||')
        owner=$(echo "$path" | cut -d'/' -f1)
        repo=$(echo "$path" | cut -d'/' -f2)
        local type_part
        type_part=$(echo "$path" | cut -d'/' -f3)
        number=$(echo "$path" | cut -d'/' -f4)

        if [[ "$type_part" == "pull" ]]; then
            ref_type="pr"
        else
            ref_type="issue"
        fi
    # Check if ref contains owner/repo#num format
    elif [[ "$ref" =~ ^([^/]+)/([^#]+)#([0-9]+)$ ]]; then
        owner="${BASH_REMATCH[1]}"
        repo="${BASH_REMATCH[2]}"
        number="${BASH_REMATCH[3]}"
        ref_type="unknown"  # Will be determined later
    # Check if ref contains #num format (needs repo_hint)
    elif [[ "$ref" =~ ^#?([0-9]+)$ ]]; then
        number="${BASH_REMATCH[1]}"
        ref_type="unknown"  # Will be determined later
    else
        log_error "Unable to parse reference: $ref"
        return 1
    fi

    # Validate required fields
    if [[ -z "$owner" || -z "$repo" || -z "$number" ]]; then
        log_error "Incomplete reference: owner=$owner, repo=$repo, number=$number"
        return 1
    fi

    echo "$owner" "$repo" "$number" "$ref_type"
}

# Determine if a number is a PR or issue by checking if PR exists
# Usage: determine_ref_type <owner> <repo> <number>
# Outputs: "pr" or "issue"
determine_ref_type() {
    local owner="$1"
    local repo="$2"
    local number="$3"

    # Try to fetch as PR
    if gh pr view "$number" --repo "$owner/$repo" --json title &> /dev/null; then
        echo "pr"
        return 0
    fi

    # Try to fetch as issue
    if gh issue view "$number" --repo "$owner/$repo" --json title &> /dev/null; then
        echo "issue"
        return 0
    fi

    log_error "Unable to determine reference type for $owner/$repo#$number"
    return 1
}

# Fetch issue metadata and write to file
fetch_issue_metadata() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"

    log_info "Fetching issue metadata for $owner/$repo#$number..."
    if gh issue view "$number" --repo "$owner/$repo" --json number,title,body,state,url,author,createdAt,updatedAt,labels > "$output_file"; then
        return 0
    else
        log_error "Failed to fetch issue metadata"
        return 1
    fi
}

# Fetch issue comments and write to file
fetch_issue_comments() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"
    local trigger_comment_id="${5:-}"

    log_info "Fetching comments for $owner/$repo#$number..."

    local tmp_file
    tmp_file=$(mktemp "${output_file}.XXXXXX")
    if [[ $? -ne 0 ]]; then
        log_error "Failed to create temporary file for issue comments"
        return 1
    fi

    local api_path="repos/$owner/$repo/issues/$number/comments"
    gh api "$api_path" --paginate > "$tmp_file"

    if [[ $? -ne 0 ]]; then
        log_error "Failed to fetch issue comments"
        rm -f "$tmp_file"
        return 1
    fi

    if [[ -n "$trigger_comment_id" ]]; then
        if [[ "$trigger_comment_id" =~ ^[0-9]+$ ]]; then
            jq --argjson trigger_id "$trigger_comment_id" \
               'map(. + {is_trigger: (.id == $trigger_id)})' \
               "$tmp_file" > "$output_file"
            rm -f "$tmp_file"
        else
            log_warn "Invalid trigger_comment_id '$trigger_comment_id'; expected numeric. Skipping trigger marking."
            mv "$tmp_file" "$output_file"
        fi
    else
        mv "$tmp_file" "$output_file"
    fi

    local count
    if ! count=$(jq 'length' "$output_file" 2>/dev/null); then
        log_warn "Failed to parse comments JSON; defaulting count to 0"
        count=0
    fi
    log_info "Found $count comments"
    return 0
}

# Fetch PR metadata and write to file
fetch_pr_metadata() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"

    log_info "Fetching PR metadata for $owner/$repo#$number..."
    if gh pr view "$number" --repo "$owner/$repo" \
        --json number,title,body,state,url,baseRefName,headRefName,headRefOid,author,createdAt,updatedAt,mergeCommit,reviews,additions,deletions,changedFiles,mergeable \
        > "$output_file"; then
        return 0
    else
        log_error "Failed to fetch PR metadata"
        return 1
    fi
}

# Fetch PR files list (limited) and write to file
fetch_pr_files() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"
    local max_files="${5:-200}"

    log_info "Fetching PR files (limit: $max_files) for $owner/$repo#$number..."
    if gh pr view "$number" --repo "$owner/$repo" \
        --json files \
        --jq ".files // [] | .[:$max_files]" \
        > "$output_file"; then
        local count
        if ! count=$(jq 'length' "$output_file" 2>/dev/null); then
            log_warn "Failed to parse files JSON; defaulting count to 0"
            count=0
        fi
        log_info "Found $count files"
        return 0
    else
        log_warn "Failed to fetch PR files"
        return 1
    fi
}

# Fetch PR review threads and write to file
fetch_pr_review_threads() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"
    local unresolved_only="${5:-false}"
    local trigger_comment_id="${6:-}"

    log_info "Fetching review threads for $owner/$repo#$number..."

    local tmp_file
    tmp_file=$(mktemp "${output_file}.XXXXXX")
    if [[ $? -ne 0 ]]; then
        log_error "Failed to create temporary file for review threads"
        return 1
    fi

    echo "[]" > "$tmp_file"

    local has_next="true"
    local cursor=""
    local first_page="true"
    while [[ "$has_next" == "true" ]]; do
        local response
        if [[ "$first_page" == "true" ]]; then
            if ! response=$(gh api graphql \
                -F owner="$owner" \
                -F repo="$repo" \
                -F number="$number" \
                -f query='query($owner:String!,$repo:String!,$number:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100){nodes{id isResolved isOutdated path line startLine comments(first:100){nodes{id databaseId body url createdAt author{login}}} } pageInfo{hasNextPage endCursor}}}}}'); then
                log_error "Failed to fetch review threads via GraphQL"
                rm -f "$tmp_file"
                return 1
            fi
        else
            if ! response=$(gh api graphql \
                -F owner="$owner" \
                -F repo="$repo" \
                -F number="$number" \
                -F after="$cursor" \
                -f query='query($owner:String!,$repo:String!,$number:Int!,$after:String!){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100,after:$after){nodes{id isResolved isOutdated path line startLine comments(first:100){nodes{id databaseId body url createdAt author{login}}} } pageInfo{hasNextPage endCursor}}}}}'); then
                log_error "Failed to fetch paginated review threads via GraphQL"
                rm -f "$tmp_file"
                return 1
            fi
        fi

        local nodes
        if ! nodes=$(echo "$response" | jq -c '.data.repository.pullRequest.reviewThreads.nodes // []'); then
            log_error "Failed to parse review thread response payload"
            rm -f "$tmp_file"
            return 1
        fi

        if ! jq -s '.[0] + .[1]' "$tmp_file" <(echo "$nodes") > "${tmp_file}.next"; then
            log_error "Failed to merge paginated review thread payloads"
            rm -f "$tmp_file" "${tmp_file}.next"
            return 1
        fi
        mv "${tmp_file}.next" "$tmp_file"

        if ! has_next=$(echo "$response" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.hasNextPage // false'); then
            log_error "Failed to read review thread pageInfo.hasNextPage"
            rm -f "$tmp_file"
            return 1
        fi
        if [[ "$has_next" == "true" ]]; then
            if ! cursor=$(echo "$response" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.endCursor // empty'); then
                log_error "Failed to read review thread pageInfo.endCursor"
                rm -f "$tmp_file"
                return 1
            fi
            if [[ -z "$cursor" ]]; then
                log_error "Review threads pagination indicated hasNextPage=true but endCursor is empty"
                rm -f "$tmp_file"
                return 1
            fi
        fi
        first_page="false"
    done

    local filter_cmd='map(select(.isOutdated != true))'
    if [[ "$unresolved_only" == "true" ]]; then
        filter_cmd+=' | map(select(.isResolved != true))'
    fi
    filter_cmd+=' | map(. + {comments: ((.comments // {}) | .nodes // [] | map(. + {comment_id: (.databaseId // 0)}))})'

    if [[ -n "$trigger_comment_id" ]]; then
        if [[ "$trigger_comment_id" =~ ^[0-9]+$ ]]; then
            jq --argjson trigger_id "$trigger_comment_id" \
               "$filter_cmd | map(. + {comments: (.comments | map(. + {is_trigger: ((.databaseId // .comment_id // -1) == $trigger_id)}))})" \
               "$tmp_file" > "$output_file"
        else
            log_warn "Invalid trigger_comment_id '$trigger_comment_id'; skipping trigger marking"
            jq "$filter_cmd" "$tmp_file" > "$output_file"
        fi
    else
        jq "$filter_cmd" "$tmp_file" > "$output_file"
    fi

    rm -f "$tmp_file" "${tmp_file}.next"

    local count
    if ! count=$(jq 'length' "$output_file" 2>/dev/null); then
        log_warn "Failed to parse review threads JSON; defaulting count to 0"
        count=0
    fi
    log_info "Found $count review threads"
    return 0
}

# Fetch PR comments (general discussion) and write to file
fetch_pr_comments() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"
    local trigger_comment_id="${5:-}"

    log_info "Fetching PR comments for $owner/$repo#$number..."

    local tmp_file
    tmp_file=$(mktemp "${output_file}.XXXXXX")
    if [[ $? -ne 0 ]]; then
        log_error "Failed to create temporary file for PR comments"
        return 1
    fi

    local api_path="repos/$owner/$repo/issues/$number/comments"

    gh api "$api_path" --paginate > "$tmp_file"

    if [[ $? -ne 0 ]]; then
        log_error "Failed to fetch PR comments"
        rm -f "$tmp_file"
        return 1
    fi

    if [[ -n "$trigger_comment_id" ]]; then
        if [[ "$trigger_comment_id" =~ ^[0-9]+$ ]]; then
            jq --argjson trigger_id "$trigger_comment_id" \
               'map(. + {is_trigger: (.id == $trigger_id)})' \
               "$tmp_file" > "$output_file"
            rm -f "$tmp_file"
        else
            log_warn "Invalid trigger_comment_id '$trigger_comment_id'; skipping trigger comment marking"
            mv "$tmp_file" "$output_file"
        fi
    else
        mv "$tmp_file" "$output_file"
    fi

    local count
    if ! count=$(jq 'length' "$output_file" 2>/dev/null); then
        log_warn "Failed to parse PR comments JSON; defaulting count to 0"
        count=0
    fi
    log_info "Found $count PR comments"
    return 0
}

# Fetch PR diff and write to file
fetch_pr_diff() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"

    log_info "Fetching PR diff for $owner/$repo#$number..."
    if gh pr diff "$number" --repo "$owner/$repo" > "$output_file" 2>&1; then
        return 0
    else
        log_warn "Failed to fetch PR diff (may be empty or too large)"
        return 1
    fi
}

# Fetch PR check runs and write to file
fetch_pr_check_runs() {
    local owner="$1"
    local repo="$2"
    local head_sha="$3"
    local output_file="$4"
    local max_runs_arg="${5:-}"
    local max_runs_env="${MAX_CHECK_RUNS:-200}"
    local max_runs="${max_runs_arg:-$max_runs_env}"

    log_info "Fetching check runs for $head_sha..."
    local api_path="repos/$owner/$repo/commits/$head_sha/check-runs?per_page=100"

    gh api "$api_path" --paginate -q ".check_runs[:$max_runs]" > "$output_file"

    if [[ $? -ne 0 ]]; then
        log_warn "Failed to fetch check runs"
        return 1
    fi

    local count
    if ! count=$(jq 'length' "$output_file" 2>/dev/null); then
        log_warn "Failed to parse check runs JSON; defaulting count to 0"
        count=0
    fi
    log_info "Found $count check runs"
    return 0
}

# Fetch workflow logs for failed checks
fetch_workflow_logs() {
    local output_dir="$1"
    local check_runs_file="$2"
    local logs_file="$output_dir/test-failure-logs.txt"

    local failed_checks
    failed_checks=$(jq -r '.[] | select(.conclusion == "failure" or .conclusion == "timed_out" or .conclusion == "action_required") | select(.details_url != null) | "\(.name)|\(.details_url)|\(.conclusion)"' "$check_runs_file")

    if [[ -z "$failed_checks" ]]; then
        log_info "No failed checks with workflow logs found"
        return 0
    fi

    log_info "Downloading workflow logs for failed checks..."
    local first=true

    while IFS='|' read -r name url conclusion; do
        [[ -z "$name" ]] && continue

        log_info "  Downloading logs for: $name"

        local logs
        logs=$(gh api "$url" 2>/dev/null || echo "")

        if [[ -z "$logs" ]]; then
            log_warn "    Failed to download logs for $name"
            continue
        fi

        if [[ "$first" == "true" ]]; then
            first=false
        else
            echo -e "\n$(printf '=%.0s' {1..80})\n" >> "$logs_file"
        fi

        echo -e "Check: $name\nConclusion: $conclusion\nDetails URL: $url\n\n" >> "$logs_file"
        echo "$logs" >> "$logs_file"
    done <<< "$failed_checks"

    log_info "Saved workflow logs to $logs_file"
    return 0
}

# Fetch PR commits and write to file
fetch_pr_commits() {
    local owner="$1"
    local repo="$2"
    local number="$3"
    local output_file="$4"

    log_info "Fetching commits for $owner/$repo#$number..."
    if gh api "repos/$owner/$repo/pulls/$number/commits" --paginate > "$output_file"; then
        local count
        if ! count=$(jq 'length' "$output_file" 2>/dev/null); then
            log_warn "Failed to parse commits JSON; defaulting count to 0"
            count=0
        fi
        log_info "Found $count commits"
        return 0
    else
        log_warn "Failed to fetch commits (continuing...)"
        return 1
    fi
}

# Verify that required context files exist and are non-empty where appropriate
verify_context_files() {
    local context_dir="$1"
    local ref_type="$2"
    local include_diff="${3:-false}"
    local include_checks="${4:-false}"
    local include_files="${5:-false}"
    local include_commits="${6:-false}"
    local include_threads="${7:-false}"

    local required_files=()
    local optional_files=()

    if [[ "$ref_type" == "pr" ]]; then
        required_files+=("$context_dir/github/pr.json")
        if [[ "$include_files" == "true" ]]; then
            required_files+=("$context_dir/github/files.json")
        else
            optional_files+=("$context_dir/github/files.json")
        fi

        optional_files+=("$context_dir/github/comments.json")

        if [[ "$include_threads" == "true" ]]; then
            optional_files+=("$context_dir/github/review_threads.json")
        fi

        if [[ "$include_diff" == "true" ]]; then
            optional_files+=("$context_dir/github/pr.diff")
        fi

        if [[ "$include_checks" == "true" ]]; then
            optional_files+=("$context_dir/github/check_runs.json")
        fi

        if [[ "$include_commits" == "true" ]]; then
            optional_files+=("$context_dir/github/commits.json")
        fi
    elif [[ "$ref_type" == "issue" ]]; then
        required_files=("$context_dir/github/issue.json")
        optional_files=("$context_dir/github/comments.json")
    fi

    for file in "${required_files[@]}"; do
        if [[ ! -f "$file" ]]; then
            log_error "Required context file missing: $file"
            return 1
        fi

        if [[ ! -s "$file" ]]; then
            log_error "Required context file is empty: $file"
            return 1
        fi
    done

    for file in "${optional_files[@]}"; do
        if [[ ! -f "$file" ]]; then
            log_warn "Optional context file missing: $file"
            continue
        fi
        if [[ ! -s "$file" ]]; then
            log_warn "Optional context file is empty (allowed): $file"
            continue
        fi
        if [[ "$file" =~ \.json$ ]]; then
            if ! jq empty "$file" 2>/dev/null; then
                log_warn "Optional context file has invalid JSON: $file"
            fi
        fi
    done

    return 0
}

# Write collection manifest (schema v2.0)
# Usage: write_manifest <output_dir> <owner> <repo> <number> <ref_type> <success> [artifacts_json] [notes_json]
write_manifest() {
    local output_dir="$1"
    local owner="$2"
    local repo="$3"
    local number="$4"
    local ref_type="$5"
    local success="$6"
    local artifacts_json="${7:-[]}"
    local notes_json="${8:-[]}"

    local provider="${MANIFEST_PROVIDER:-ghx}"
    local manifest_file="$output_dir/manifest.json"
    local timestamp
    timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

    jq -n \
      --arg schema_version "2.0" \
      --arg provider "$provider" \
      --arg kind "$ref_type" \
      --arg ref "$owner/$repo#$number" \
      --arg owner "$owner" \
      --arg repo "$repo" \
      --arg collected_at "$timestamp" \
      --argjson number "$number" \
      --argjson success "$success" \
      --argjson artifacts "$artifacts_json" \
      --argjson notes "$notes_json" \
      '{
        schema_version: $schema_version,
        provider: $provider,
        kind: $kind,
        ref: $ref,
        owner: $owner,
        repo: $repo,
        number: $number,
        collected_at: $collected_at,
        success: $success,
        artifacts: $artifacts,
        notes: $notes
      }' > "$manifest_file"

    log_info "Wrote collection manifest to $manifest_file"
    return 0
}
