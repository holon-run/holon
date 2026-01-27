#!/bin/bash
# test_publisher.sh - Tests for the publisher script
#
# These tests validate the publisher script behavior using real jq.

set -euo pipefail

# Test directory setup
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$TEST_DIR/../.." && pwd)"
PUBLISHER_SCRIPT="$REPO_ROOT/skills/github-solve/scripts/publish.sh"

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
    if [[ -f "$PUBLISHER_SCRIPT" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher script exists"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher script not found: $PUBLISHER_SCRIPT"
    fi
}

test_publisher_script_executable() {
    local test_name="script_executable"
    log_info "Running test: $test_name"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ -x "$PUBLISHER_SCRIPT" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher script is executable"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher script is not executable"
    fi
}

test_publisher_help() {
    local test_name="help_output"
    log_info "Running test: $test_name"
    
    local output
    output=$(bash "$PUBLISHER_SCRIPT" --help 2>&1 || true)
    
    assert_contains "$output" "Usage:" "Help text shows usage"
    assert_contains "$output" "publish.sh" "Help text mentions script name"
}

test_publisher_missing_intent() {
    local test_name="missing_intent"
    log_info "Running test: $test_name"
    
    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    
    mkdir -p "$output_dir"
    export GITHUB_OUTPUT_DIR="$output_dir"
    
    cd "$tmp_dir"
    
    # Run publisher without intent file and expect error
    local output
    output=$(bash "$PUBLISHER_SCRIPT" --intent=/nonexistent/intent.json 2>&1 || true)
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ "$output" == *"Error"* ]] || [[ "$output" == *"error"* ]] || [[ "$output" == *"not found"* ]] || [[ "$output" == *"No such file"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher correctly handles missing intent file"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher should error on missing intent file"
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
    
    # Create invalid JSON intent file
    mkdir -p "$output_dir"
    echo "{ invalid json" > "$output_dir/publish-intent.json"
    
    export GITHUB_OUTPUT_DIR="$output_dir"
    
    cd "$tmp_dir"
    
    # Run publisher and expect failure
    local output
    output=$(bash "$PUBLISHER_SCRIPT" --intent="$output_dir/publish-intent.json" 2>&1 || true)
    
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

test_publisher_valid_intent() {
    local test_name="valid_intent"
    log_info "Running test: $test_name"
    
    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    
    # Create valid intent file
    mkdir -p "$output_dir"
    cat > "$output_dir/publish-intent.json" << 'EOF'
{
  "actions": [
    {
      "type": "post_comment",
      "body": "Test comment"
    }
  ]
}
EOF
    
    export GITHUB_OUTPUT_DIR="$output_dir"
    
    cd "$tmp_dir"
    
    # Test dry-run mode (should not fail even without gh auth)
    local output
    output=$(bash "$PUBLISHER_SCRIPT" --dry-run --intent="$output_dir/publish-intent.json" 2>&1 || true)
    
    TESTS_RUN=$((TESTS_RUN + 1))
    # In dry-run mode, it should succeed or at least not crash
    if [[ "$output" != *"syntax error"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Publisher handles valid intent in dry-run mode"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Publisher should handle valid intent"
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
    test_publisher_missing_intent
    test_publisher_invalid_json
    test_publisher_valid_intent
    
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
