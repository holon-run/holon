#!/bin/bash
# Script to help record GitHub API test fixtures
#
# Usage:
#   ./record-fixtures.sh                    # Record all fixtures
#   ./record-fixtures.sh -run TestFetchPR   # Record specific test
#   ./record-fixtures.sh -list              # List available tests
#
# Prerequisites:
#   - GITHUB_TOKEN environment variable set
#   - Write access to pkg/github/testdata/fixtures/

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check if GITHUB_TOKEN is set
if [ -z "$GITHUB_TOKEN" ]; then
    echo -e "${RED}Error: GITHUB_TOKEN environment variable not set${NC}"
    echo "Please set your GitHub token:"
    echo "  export GITHUB_TOKEN=your_token_here"
    exit 1
fi

# Parse arguments
TEST_FILTER=""
LIST_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        -run|--run)
            TEST_FILTER="$2"
            shift 2
            ;;
        -list|--list)
            LIST_ONLY=true
            shift
            ;;
        -h|--help)
            echo "GitHub Helper Fixture Recording Script"
            echo ""
            echo "Usage:"
            echo "  $0 [options]"
            echo ""
            echo "Options:"
            echo "  -run, --run TEST    Record specific test (e.g., TestFetchPRInfo)"
            echo "  -list, --list       List available tests"
            echo "  -h, --help          Show this help message"
            echo ""
            echo "Examples:"
            echo "  $0                              # Record all fixtures"
            echo "  $0 -run TestFetchPRInfo         # Record specific test"
            echo "  $0 -run TestFetch               # Record all matching tests"
            echo ""
            echo "Prerequisites:"
            echo "  - GITHUB_TOKEN environment variable set"
            echo "  - Write access to pkg/github/testdata/fixtures/"
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            echo "Use -h or --help for usage information"
            exit 1
            ;;
    esac
done

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG_DIR="$(dirname "$SCRIPT_DIR")"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"

# List available tests if requested
if [ "$LIST_ONLY" = true ]; then
    echo "Available tests in pkg/github:"
    echo ""
    go test -list ".*" "$PKG_DIR" 2>/dev/null | grep -E "^Test" || true
    exit 0
fi

# Create fixtures directory if it doesn't exist
mkdir -p "$FIXTURES_DIR"

# Set recording mode
export HOLON_VCR_MODE=record

# Print what we're about to do
echo -e "${GREEN}Recording GitHub API fixtures${NC}"
echo ""
echo "Settings:"
echo "  Mode:         record"

# Display token safely (avoid bash 4.2+ negative index syntax)
token_prefix="${GITHUB_TOKEN:0:10}"
token_len=${#GITHUB_TOKEN}
if [ "$token_len" -gt 4 ]; then
    token_suffix="${GITHUB_TOKEN:$((token_len - 4)):4}"
else
    token_suffix="$GITHUB_TOKEN"
fi
echo "  Token:        ${token_prefix}...${token_suffix}"
echo "  Fixtures dir: $FIXTURES_DIR"
echo ""

if [ -n "$TEST_FILTER" ]; then
    echo "  Test filter:  $TEST_FILTER"
fi

echo ""
echo -e "${YELLOW}Warning: This will make real API calls to GitHub${NC}"
echo ""
read -p "Continue? (y/N) " -n 1 -r response
echo ""

if [[ ! $response =~ ^[Yy]$ ]]; then
    echo "Aborted"
    exit 0
fi

echo ""
echo -e "${GREEN}Starting fixture recording...${NC}"
echo ""

# Run the tests without using eval to avoid command injection
if [ -n "$TEST_FILTER" ]; then
    go test -v "$PKG_DIR" -run "$TEST_FILTER"
else
    go test -v "$PKG_DIR"
fi

# Check if fixtures were created
echo ""
echo -e "${GREEN}Recording complete!${NC}"
echo ""

# Count fixtures
FIXTURE_COUNT=$(find "$FIXTURES_DIR" -name "*.yaml" -o -name "*.yml" 2>/dev/null | wc -l)

echo "Fixture summary:"
echo "  Total fixtures: $FIXTURE_COUNT"
echo ""

# List new/modified fixtures
echo "Recent fixtures:"
ls -lt "$FIXTURES_DIR"/*.yaml 2>/dev/null | head -10 || echo "  No fixtures found"

echo ""
echo -e "${GREEN}Done!${NC}"
echo ""
echo "Next steps:"
echo "  1. Review the fixtures in $FIXTURES_DIR"
echo "  2. Run tests in replay mode to verify:"
echo "     go test -v $PKG_DIR"
echo "  3. Commit the fixtures to version control"
