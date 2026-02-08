#!/bin/bash
# test_collector.sh - Tests for the collector script
#
# These tests validate the collector script behavior using real gh/jq tools
# but without requiring network access (using local mode and fixtures).

set -euo pipefail

# Test directory setup
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$TEST_DIR/../.." && pwd)"
FIXTURES_DIR="$TEST_DIR/fixtures"
COLLECTOR_SCRIPT="$REPO_ROOT/skills/ghx/scripts/collect.sh"

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

assert_file_not_empty() {
    local file="$1"
    local msg="${2:-File should not be empty: $file}"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ -s "$file" ]]; then
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
    tmp_dir=$(mktemp -d "/tmp/collector-test-${test_name}-XXXXXX")
    echo "$tmp_dir"
}

cleanup_test_env() {
    local tmp_dir="$1"
    if [[ -d "$tmp_dir" ]]; then
        rm -rf "$tmp_dir"
    fi
}

# Test cases
test_collector_script_exists() {
    local test_name="script_exists"
    log_info "Running test: $test_name"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ -f "$COLLECTOR_SCRIPT" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Collector script exists"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Collector script not found: $COLLECTOR_SCRIPT"
    fi
}

test_collector_script_executable() {
    local test_name="script_executable"
    log_info "Running test: $test_name"
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ -x "$COLLECTOR_SCRIPT" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Collector script is executable"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Collector script is not executable"
    fi
}

test_collector_help() {
    local test_name="help_output"
    log_info "Running test: $test_name"

    local output
    output=$(bash "$COLLECTOR_SCRIPT" 2>&1 || true)

    assert_contains "$output" "Usage:" "Usage text is shown"
    assert_contains "$output" "collect.sh" "Help text mentions script name"
}

test_collector_invalid_ref() {
    local test_name="invalid_ref"
    log_info "Running test: $test_name"
    
    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    
    export GITHUB_CONTEXT_DIR="$output_dir"
    
    # Run collector with invalid ref and expect failure
    local output
    local exit_code=0
    output=$(bash "$COLLECTOR_SCRIPT" "invalid-ref-format" 2>&1) || exit_code=$?
    
    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ $exit_code -ne 0 ]] || [[ "$output" == *"Error"* ]] || [[ "$output" == *"error"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Collector correctly rejects invalid ref"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Collector should reject invalid ref"
    fi
    
    cleanup_test_env "$tmp_dir"
}

test_collector_missing_jq() {
    local test_name="missing_jq"
    log_info "Running test: $test_name"

    local tmp_dir
    tmp_dir=$(setup_test_env "$test_name")
    local output_dir="$tmp_dir/output"
    local bin_dir="$tmp_dir/bin"

    # Create a PATH with a shadowed jq that fails
    mkdir -p "$bin_dir"
    # Create fake gh that does nothing
    cat > "$bin_dir/gh" << 'INNEREOF'
#!/bin/sh
exit 0
INNEREOF
    chmod +x "$bin_dir/gh"
    # Create a jq stub that always fails
    cat > "$bin_dir/jq" << 'INNEREOF'
#!/bin/sh
echo "jq: command not found" >&2
exit 1
INNEREOF
    chmod +x "$bin_dir/jq"

    local old_path="$PATH"
    # Prepend our stubbed bin to PATH to shadow the real jq
    export PATH="$bin_dir:$PATH"
    export GITHUB_CONTEXT_DIR="$output_dir"

    # Run collector and expect dependency error
    local output
    output=$(bash "$COLLECTOR_SCRIPT" "owner/repo#123" 2>&1 || true)

    TESTS_RUN=$((TESTS_RUN + 1))
    if [[ "$output" == *"jq"* ]] || [[ "$output" == *"Missing"* ]] || [[ "$output" == *"dependencies"* ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        log_info "✓ Collector correctly detects missing jq"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        log_error "✗ Collector should detect missing jq"
        log_error "Output: $output"
    fi

    export PATH="$old_path"
    cleanup_test_env "$tmp_dir"
}

test_collector_parsing_urls() {
    local test_name="parse_urls"
    log_info "Running test: $test_name"
    
    # Test that the collector script can at least parse various URL formats
    # without actually fetching from GitHub (we'll test that it rejects them cleanly)
    
    local refs=(
        "https://github.com/owner/repo/issues/123"
        "owner/repo#456"
        "789"
    )
    
    for ref in "${refs[@]}"; do
        local tmp_dir
        tmp_dir=$(setup_test_env "$test_name")
        local output_dir="$tmp_dir/output"
        
        export GITHUB_CONTEXT_DIR="$output_dir"
        
        # Run collector - it should either succeed (if network available) or fail gracefully
        local output
        output=$(bash "$COLLECTOR_SCRIPT" "$ref" 2>&1 || true)
        
        # We're mainly checking it doesn't crash with a script error
        TESTS_RUN=$((TESTS_RUN + 1))
        if [[ "$output" != *"syntax error"* ]] && [[ "$output" != *"unexpected"* ]]; then
            TESTS_PASSED=$((TESTS_PASSED + 1))
            log_info "✓ Collector handles ref format: $ref"
        else
            TESTS_FAILED=$((TESTS_FAILED + 1))
            log_error "✗ Collector script error on ref: $ref"
        fi
        
        cleanup_test_env "$tmp_dir"
    done
}

# Main test runner
main() {
    log_info "=== Collector Script Tests ==="
    log_info "Test directory: $TEST_DIR"
    log_info "Repository root: $REPO_ROOT"
    log_info ""
    
    # Run tests
    test_collector_script_exists
    test_collector_script_executable
    test_collector_help
    test_collector_invalid_ref
    test_collector_missing_jq
    test_collector_parsing_urls
    
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
