#!/usr/bin/env bash
#
# Holon test wrapper with structured output using gotestfmt
#
# Usage:
#   ./scripts/test.sh [packages...] [-- extra-go-test-args...]
#
# Examples:
#   ./scripts/test.sh                    # Test all packages
#   ./scripts/test.sh ./pkg/...          # Test specific packages
#   ./scripts/test.sh -- -run TestFoo    # Pass extra args to go test
#   ./scripts/test.sh -- -v -count=1     # Multiple extra args after --
#
# Environment Variables:
#   GOTESTFMT_OPTS  - Additional options for gotestfmt (e.g., "-nofail")
#   GO_TEST_OPTS    - Additional options for go test (e.g., "-race")

set -euo pipefail

# Colors for terminal output
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# Default packages to test
DEFAULT_PKGS="./..."

# Determine if we should use gotestfmt based on its availability
# Default to enabled and fall back to plain go test if gotestfmt is not installed
use_gotestfmt=true

# Parse command line arguments
packages=()
go_test_args=()

# Check if gotestfmt is available
if ! command -v gotestfmt &> /dev/null; then
    echo -e "${YELLOW}Warning: gotestfmt not found, falling back to plain go test output${NC}"
    echo -e "${YELLOW}Install gotestfmt: go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest${NC}"
    use_gotestfmt=false
fi

# Parse arguments - everything after -- is a go test arg, everything before is a package
# Packages must contain '/' or start with './' or '.' to be recognized as such
while [[ $# -gt 0 ]]; do
    case $1 in
        --)
            shift
            go_test_args+=("$@")
            break
            ;;
        *)
            # Check if argument looks like a package path (contains '/' or starts with '.')
            if [[ "$1" == *"/"* ]] || [[ "$1" == "."* ]]; then
                packages+=("$1")
            else
                # Treat as a go test argument (e.g., -v, -run, -count, etc.)
                go_test_args+=("$1")
            fi
            shift
            ;;
    esac
done

# If no packages specified, test all
if [ ${#packages[@]} -eq 0 ]; then
    packages=("$DEFAULT_PKGS")
fi

# Build go test command
go_test_cmd=(go test)
go_test_cmd+=("${packages[@]}")

# Add JSON output format if using gotestfmt
if [ "$use_gotestfmt" = true ]; then
    go_test_cmd+=("-json")
fi

# Add GO_TEST_OPTS if specified
if [ -n "${GO_TEST_OPTS:-}" ]; then
    # Split GO_TEST_OPTS by spaces and add to command
    read -ra opts <<< "$GO_TEST_OPTS"
    go_test_cmd+=("${opts[@]}")
fi

# Add any extra test arguments from command line
go_test_cmd+=("${go_test_args[@]}")

# Add verbose flag if not already specified (gotestfmt handles this well)
# Check go_test_cmd for -v or -verbose flag
if [[ ! " ${go_test_cmd[*]} " =~ " -v " ]] && [[ ! " ${go_test_cmd[*]} " =~ " -verbose " ]]; then
    go_test_cmd+=("-v")
fi

# Print what we're running
echo -e "${GREEN}Running:${NC} ${go_test_cmd[*]}"

# Run tests and pipe through gotestfmt if available
if [ "$use_gotestfmt" = true ]; then
    # Use gotestfmt for structured output
    "${go_test_cmd[@]}" 2>&1 | gotestfmt ${GOTESTFMT_OPTS:+"$GOTESTFMT_OPTS"}
else
    # Fallback to plain output
    "${go_test_cmd[@]}"
fi
