#!/bin/bash
# test_publisher.sh - Tests for the ghx publish entrypoint
#
# These tests validate ghx batch/direct publish behavior using real jq.

set -euo pipefail

# Test directory setup
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$TEST_DIR/../.." && pwd)"
GHX_SCRIPT="$REPO_ROOT/skills/ghx/scripts/ghx.sh"

# Test counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Helper functions
log_info() { echo -e "${GREEN}[INFO]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }

assert_file_exists() {
    local file="$1"
    local msg="${2:-File should exist: $file}"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ -f "$file" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ $msg"
        return 0
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ $msg"
        return 1
    fi
}

assert_json_valid() {
    local file="$1"
    local msg="${2:-File should be valid JSON: $file}"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if command -v jq >/dev/null 2>&1; then
        if jq empty "$file" 2>/dev/null; then
            TESTS_PASSED=$((TESTS_PASSED + 1))
            log_info "✓ $msg"
            return 0
        else
            TESTS_FAILED=$((TESTS_FAILED + 1))
            log_error "✗ $msg"
            return 1
        fi
    else
        log_warn "jq not available, skipping JSON validation"
        TESTS_PASSED=$((TESTS_PASSED + 1))
        return 0
    fi
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="${3:-String should contain: $needle}"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ "$haystack" == *"$needle"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ $msg"
        return 0
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ $msg"
        return 1
    fi
}

# Test setup
setup_test_env() {
    local test_name="$1"
    local tmp_dir
    tmp_dir=$(mktemp -d "/tmp/publisher-test-${test_name}-XXXXXX")
    echo "$tmp_dir"
}

cleanup_test_env() {
    local tmp_dir="$1"
    if [[ -d "$tmp_dir" ]]; then
        rm -rf "$tmp_dir"
    fi
}

# Test cases
test_publisher_script_exists() {
    local test_name="script_exists"
    log_info "Running test: $test_name"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ -f "$GHX_SCRIPT" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ GHX entry script exists"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ GHX entry script not found: $GHX_SCRIPT"
    fi
}

test_publisher_script_executable() {
    local test_name="script_executable"
    log_info "Running test: $test_name"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ -x "$GHX_SCRIPT" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ GHX entry script is executable"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ GHX entry script is not executable"
    fi
}

test_publisher_help() {
    local test_name="help_output"
    log_info "Running test: $test_name"
    
    local output
    output=$(bash "$GHX_SCRIPT" --help 2>&1 || true)
    
    assert_contains "$output" "Usage:" "Help text shows usage"
    assert_contains "$output" "ghx.sh" "Help text mentions script name"
}

test_publisher_missing_batch() {
    local test_name="missing_batch"
    log_info "Running test: $test_name"

    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    local bin_dir="$tmp_dir/bin"

    # Provide a stub gh on PATH so check_dependencies/gh auth status passes
    mkdir -p "$bin_dir"
    cat > "$bin_dir/gh" << 'INNEREOF'
#!/usr/bin/env bash
# Minimal gh stub for tests: always report auth as OK.
if [[ "$1" == "auth" && "$2" == "status" ]]; then
  echo "github.com"
  echo "  ✓ Logged in to github.com"
  exit 0
fi
# Default: succeed without doing anything.
exit 0
INNEREOF
    chmod +x "$bin_dir/gh"
    export PATH="$bin_dir:$PATH"

    mkdir -p "$output_dir"
    export GITHUB_OUTPUT_DIR="$output_dir"

    cd "$tmp_dir"

    # Run publisher without batch file and expect error
    local output
    output=$(bash "$GHX_SCRIPT" batch run --batch=/nonexistent/publish-batch.json 2>&1 || true)

    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ "$output" == *"Error"* ]] || [[ "$output" == *"error"* ]] || [[ "$output" == *"not found"* ]] || [[ "$output" == *"No such file"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher correctly handles missing batch file"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher should error on missing batch file"
        log_error "Output: $output"
    fi

    cleanup_test_env "$tmp_dir"
}

test_publisher_invalid_json() {
    local test_name="invalid_json"
    log_info "Running test: $test_name"

    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    local bin_dir="$tmp_dir/bin"

    # Provide a stub gh on PATH so check_dependencies/gh auth status passes
    mkdir -p "$bin_dir"
    cat > "$bin_dir/gh" << 'INNEREOF'
#!/usr/bin/env bash
# Minimal gh stub for tests: always report auth as OK.
if [[ "$1" == "auth" && "$2" == "status" ]]; then
  echo "github.com"
  echo "  ✓ Logged in to github.com"
  exit 0
fi
# Default: succeed without doing anything.
exit 0
INNEREOF
    chmod +x "$bin_dir/gh"
    export PATH="$bin_dir:$PATH"

    # Create invalid JSON batch file
    mkdir -p "$output_dir"
    echo "{ invalid json" > "$output_dir/publish-batch.json"

    export GITHUB_OUTPUT_DIR="$output_dir"

    cd "$tmp_dir"

    # Run publisher and expect failure
    local output
    output=$(bash "$GHX_SCRIPT" batch run --batch="$output_dir/publish-batch.json" 2>&1 || true)

    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ "$output" == *"parse error"* ]] || [[ "$output" == *"invalid"* ]] || [[ "$output" == *"Error"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher correctly rejects invalid JSON"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher should reject invalid JSON"
    fi

    cleanup_test_env "$tmp_dir"
}

test_publisher_valid_batch() {
    local test_name="valid_batch"
    log_info "Running test: $test_name"

    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    local bin_dir="$tmp_dir/bin"

    # Create valid batch file with proper schema
    mkdir -p "$output_dir"
    cat > "$output_dir/publish-batch.json" << 'EOF'
{
  "version": "1.0",
  "pr_ref": "owner/repo#123",
  "actions": [
    {
      "type": "post_comment"
    }
  ]
}
EOF

    # Provide a stub gh on PATH so check_dependencies/gh auth status passes in hermetic CI
    mkdir -p "$bin_dir"
    cat > "$bin_dir/gh" << 'INNEREOF'
#!/usr/bin/env bash
# Minimal gh stub for tests: always report auth as OK.
if [[ "$1" == "auth" && "$2" == "status" ]]; then
  echo "github.com"
  echo "  ✓ Logged in to github.com"
  exit 0
fi
# Default: succeed without doing anything.
exit 0
INNEREOF
    chmod +x "$bin_dir/gh"
    export PATH="$bin_dir:$PATH"
    export GITHUB_OUTPUT_DIR="$output_dir"

    cd "$tmp_dir"

    # Test dry-run mode (should succeed and not crash)
    local output
    local status=0
    if output=$(bash "$GHX_SCRIPT" batch run --dry-run --batch="$output_dir/publish-batch.json" 2>&1); then
        status=0
    else
        status=$?
    fi

    TESTS_RUN=$((TESTS_RUN + 1))
    # In dry-run mode, it should succeed and not emit syntax errors
    if [[ $status -eq 0 && "$output" != *"syntax error"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher handles valid batch in dry-run mode"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher should handle valid batch (status=$status, output: $output)"
    fi

    cleanup_test_env "$tmp_dir"
}

test_body_file_stdin() {
    local test_name="body_file_stdin"
    log_info "Running test: $test_name"

    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    local bin_dir="$tmp_dir/bin"
    local captured_body_file="$tmp_dir/captured-body.txt"

    mkdir -p "$output_dir"
    mkdir -p "$bin_dir"

    # Provide a gh stub that captures posted comment body.
    cat > "$bin_dir/gh" << 'INNEREOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "auth" && "${2:-}" == "status" ]]; then
  echo "github.com"
  echo "  ✓ Logged in to github.com"
  exit 0
fi

if [[ "${1:-}" == "api" ]]; then
  endpoint="${2:-}"
  shift 2 || true

  method="GET"
  body=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -X)
        shift
        method="${1:-GET}"
        ;;
      -f)
        shift
        if [[ "${1:-}" == body=* ]]; then
          body="${1#body=}"
        fi
        ;;
      -q|--jq|-F|--input)
        # Consume value for flags that have one.
        if [[ "$1" != "--input" ]]; then
          shift
        fi
        ;;
    esac
    shift || true
  done

  if [[ "$endpoint" == repos/*/issues/*/comments && "$method" == "GET" ]]; then
    # find_existing_comment path
    echo "[]"
    exit 0
  fi

  if [[ "$endpoint" == repos/*/issues/*/comments && "$method" == "POST" ]]; then
    # create comment path
    if [[ -n "${GHX_CAPTURED_BODY_FILE:-}" ]]; then
      printf '%s' "$body" > "${GHX_CAPTURED_BODY_FILE}"
    fi
    echo "12345"
    exit 0
  fi

  echo "{}"
  exit 0
fi

exit 0
INNEREOF
    chmod +x "$bin_dir/gh"
    export PATH="$bin_dir:$PATH"
    export GITHUB_OUTPUT_DIR="$output_dir"

    cd "$tmp_dir"

    local output
    local status=0
    if output=$(cat << 'EOF' | GHX_CAPTURED_BODY_FILE="$captured_body_file" bash "$GHX_SCRIPT" pr comment --pr=owner/repo#123 --body-file - 2>&1
## Summary
line one
line two
EOF
); then
        status=0
    else
        status=$?
    fi

    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ $status -eq 0 ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher accepts --body-file - from stdin"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher should accept --body-file - (status=$status, output: $output)"
    fi

    assert_file_exists "$captured_body_file" "Captured body should exist"
    local captured
    captured=$(cat "$captured_body_file" 2>/dev/null || true)
    assert_contains "$captured" "## Summary" "Captured body contains stdin markdown"
    assert_contains "$captured" "line two" "Captured body contains multiline stdin content"
    assert_file_exists "$output_dir/publish-results.json" "publish-results.json should be generated"
    assert_json_valid "$output_dir/publish-results.json" "publish-results.json should be valid JSON"

    cleanup_test_env "$tmp_dir"
}

test_reply_review_multiline_messages() {
    local test_name="reply_review_multiline"
    log_info "Running test: $test_name (regression test for issue #551)"

    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    local bin_dir="$tmp_dir/bin"

    # Create batch file with reply_review action containing multi-word/multiline messages
    mkdir -p "$output_dir"
    cat > "$output_dir/publish-batch.json" << 'EOF'
{
  "version": "1.0",
  "pr_ref": "owner/repo#123",
  "actions": [
    {
      "type": "reply_review",
      "params": {
        "replies": [
          {
            "comment_id": 123456,
            "status": "fixed",
            "message": "This is a multi-word message with spaces and punctuation.",
            "action_taken": "Updated the function to use line-safe iteration."
          },
          {
            "comment_id": 789012,
            "status": "deferred",
            "message": "Another reply with\nnewlines and\ttabs.",
            "action_taken": "Will handle in a follow-up PR."
          }
        ]
      }
    }
  ]
}
EOF

    # Provide a stub gh on PATH
    mkdir -p "$bin_dir"
    cat > "$bin_dir/gh" << 'INNEREOF'
#!/usr/bin/env bash
# Minimal gh stub for tests
if [[ "$1" == "auth" && "$2" == "status" ]]; then
  echo "github.com"
  echo "  ✓ Logged in to github.com"
  exit 0
fi
# For reply_review, simulate success
exit 0
INNEREOF
    chmod +x "$bin_dir/gh"
    export PATH="$bin_dir:$PATH"
    export GITHUB_OUTPUT_DIR="$output_dir"

    cd "$tmp_dir"

    # Run in dry-run mode and check for jq parse errors
    local output
    local status=0
    if output=$(bash "$GHX_SCRIPT" batch run --dry-run --batch="$output_dir/publish-batch.json" 2>&1); then
        status=0
    else
        status=$?
    fi

    TESTS_RUN=$((TESTS_RUN + 1))
    # Should succeed and NOT have jq parse errors (regression test for word-splitting bug)
    if [[ $status -eq 0 && "$output" != *"jq parse error"* && "$output" != *"parse error"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ reply_review handles multi-word messages without jq parse errors"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ reply_review should handle multi-word messages (status=$status)"
        if [[ "$output" == *"jq parse error"* ]]; then
            log_error "  Found 'jq parse error' in output - word-splitting bug present!"
        fi
        log_error "  Output: $output"
    fi

    cleanup_test_env "$tmp_dir"
}

# Main test runner
main() {
    log_info "=== Publisher Script Tests ==="
    log_info "Test directory: $TEST_DIR"
    log_info "Repository root: $REPO_ROOT"
    log_info ""
    
    # Run tests
    test_publisher_script_exists
    test_publisher_script_executable
    test_publisher_help
    test_publisher_missing_batch
    test_publisher_invalid_json
    test_publisher_valid_batch
    test_body_file_stdin
    test_reply_review_multiline_messages
    
    # Summary
    echo ""
    log_info "=== Test Summary ==="
    log_info "Tests run: $TESTS_RUN"
    log_info "Tests passed: $TESTS_PASSED"
    log_info "Tests failed: $TESTS_FAILED"
    
    if [[ $TESTS_FAILED -gt 0 ]]; then
        exit 1
    else
        exit 0
    fi
}

# Run main if executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
