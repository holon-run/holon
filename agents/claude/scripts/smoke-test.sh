#!/usr/bin/env bash
set -euo pipefail

# Smoke test script for agent bundle verification
# This script verifies that the built agent bundle has the correct structure
# and can execute successfully (syntax check + basic execution test).
#
# Usage: ./scripts/smoke-test.sh [bundle_path]
#   bundle_path: Path to the bundle tar.gz (optional, auto-detected if not provided)

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)

NAME="${BUNDLE_NAME:-agent-claude}"
VERSION="${BUNDLE_VERSION:-}"
if [ -z "${VERSION}" ]; then
  VERSION=$(node -e "const p=require('${ROOT_DIR}/package.json'); console.log(p.version || '0.0.0')" 2>/dev/null || echo "0.0.0")
  # Warn if using fallback version
  if [ "${VERSION}" = "0.0.0" ]; then
    echo "WARNING: Using fallback version 0.0.0 - version could not be detected from package.json" >&2
    echo "         Set BUNDLE_VERSION explicitly to override." >&2
  fi
fi
PLATFORM="${BUNDLE_PLATFORM:-linux}"
ARCH="${BUNDLE_ARCH:-amd64}"
LIBC="${BUNDLE_LIBC:-glibc}"

BUNDLE_OUTPUT_DIR="${BUNDLE_OUTPUT_DIR:-${ROOT_DIR}/dist/agent-bundles}"

# Determine bundle path from argument or auto-detect
if [ -n "${1:-}" ]; then
  BUNDLE_PATH="$1"
else
  BUNDLE_PATH="${BUNDLE_OUTPUT_DIR}/agent-bundle-${NAME}-${VERSION}-${PLATFORM}-${ARCH}-${LIBC}.tar.gz"
fi

echo "Running bundle smoke test..."
echo "Bundle: ${BUNDLE_PATH}"

# Check if bundle exists
if [ ! -f "${BUNDLE_PATH}" ]; then
  echo "ERROR: Bundle not found at ${BUNDLE_PATH}" >&2
  echo "Please run 'npm run bundle' first." >&2
  exit 1
fi

# Create temporary directory for extraction
TEST_DIR=$(mktemp -d)
trap 'rm -rf "${TEST_DIR}"' EXIT

# Extract bundle
echo "Extracting bundle..."
tar -xzf "${BUNDLE_PATH}" -C "${TEST_DIR}"

# Verify critical files exist
echo "Verifying critical files..."
REQUIRED_FILES=(
  "package.json"
  "manifest.json"
  "dist/agent.js"
  "bin/agent"
)

for file in "${REQUIRED_FILES[@]}"; do
  if [ ! -f "${TEST_DIR}/${file}" ]; then
    echo "ERROR: Required file not found in bundle: ${file}" >&2
    exit 1
  fi
  echo "  ✓ ${file}"
done

# Verify package.json has type: module (catches ES module bug)
echo "Verifying package.json configuration..."
if ! grep -q '"type": "module"' "${TEST_DIR}/package.json"; then
  echo "ERROR: package.json missing 'type: module' (required for ES modules)" >&2
  exit 1
fi
echo "  ✓ package.json has type: module"

# Verify bin/agent is executable
echo "Verifying executable permissions..."
if [ ! -x "${TEST_DIR}/bin/agent" ]; then
  echo "ERROR: bin/agent is not executable" >&2
  exit 1
fi
echo "  ✓ bin/agent is executable"

# Verify manifest.json structure
echo "Verifying manifest.json structure..."
if ! node -e "JSON.parse(require('fs').readFileSync('${TEST_DIR}/manifest.json', 'utf8'))" 2>/dev/null; then
  echo "ERROR: manifest.json is not valid JSON" >&2
  exit 1
fi
echo "  ✓ manifest.json is valid JSON"

# Try to run the agent (syntax check)
echo "Running Node.js syntax check on dist/agent.js..."
if ! node -c "${TEST_DIR}/dist/agent.js" 2>/dev/null; then
  echo "ERROR: Syntax check failed for dist/agent.js" >&2
  exit 1
fi
echo "  ✓ dist/agent.js syntax is valid"

# Try to execute agent with --probe (basic execution test)
# The --probe mode validates that the agent can start and write outputs
# Expected output format: "Probe completed" string indicates successful execution
echo "Running agent probe test..."

# Check if we're already running in a Holon container environment
IN_HOLON_CONTAINER=0
if [ -d "/holon/workspace" ] && [ -d "/holon/input" ] && [ -d "/holon/output" ]; then
  IN_HOLON_CONTAINER=1
fi

# Check if docker is available for probe test (only needed if not already in container)
if [ ${IN_HOLON_CONTAINER} -eq 1 ]; then
  echo "  ℹ Running in Holon container environment"
  # Use existing Holon environment
  set +e
  PROBE_OUTPUT=$(cd "${TEST_DIR}" && NODE_ENV=production node dist/agent.js --probe 2>&1)
  PROBE_EXIT_CODE=$?
  set -e

  if [ ${PROBE_EXIT_CODE} -ne 0 ]; then
    echo "ERROR: Agent probe test failed with exit code ${PROBE_EXIT_CODE}" >&2
    echo "Output:" >&2
    echo "${PROBE_OUTPUT}" >&2
    exit 1
  fi

  # Check if probe wrote the expected completion message
  if ! echo "${PROBE_OUTPUT}" | grep -q "Probe completed"; then
    echo "ERROR: Agent probe did not complete successfully" >&2
    echo "Output:" >&2
    echo "${PROBE_OUTPUT}" >&2
    exit 1
  fi

  echo "  ✓ Agent probe test passed"
elif ! command -v docker >/dev/null 2>&1; then
  echo "  ⚠ WARNING: Docker not available and not in Holon container, skipping agent probe test"
  echo "    The agent probe test requires either:"
  echo "    - A Holon container environment (/holon/workspace, /holon/input, /holon/output)"
  echo "    - Docker to simulate the Holon container environment"
  echo "    Run 'npm run verify-bundle' for a full Docker-based verification"
else
  # Create minimal Holon environment structure required for probe mode
  # The agent's --probe mode expects /holon/workspace, /holon/input/spec.yaml, and /holon/output
  PROBE_HOLON_DIR=$(mktemp -d)
  trap 'rm -rf "${TEST_DIR}" "${PROBE_HOLON_DIR}"' EXIT

  PROBE_INPUT_DIR="${PROBE_HOLON_DIR}/input"
  PROBE_WORKSPACE_DIR="${PROBE_HOLON_DIR}/workspace"
  PROBE_OUTPUT_DIR="${PROBE_HOLON_DIR}/output"
  mkdir -p "${PROBE_INPUT_DIR}" "${PROBE_WORKSPACE_DIR}" "${PROBE_OUTPUT_DIR}"

  # Create minimal spec.yaml required by agent
  cat > "${PROBE_INPUT_DIR}/spec.yaml" <<'SPEC'
version: "v1"
kind: Holon
metadata:
  name: "smoke-test-probe"
context:
  workspace: "/holon/workspace"
goal:
  description: "Smoke test probe validation"
output:
  artifacts:
    - path: "manifest.json"
      required: true
SPEC

  # Create a minimal workspace file
  echo "Smoke test workspace" > "${PROBE_WORKSPACE_DIR}/README.md"

  # Determine Node version to use
  NODE_VERSION="${BUNDLE_NODE_VERSION:-20}"
  IMAGE="${BUNDLE_VERIFY_IMAGE:-node:${NODE_VERSION}-bookworm-slim}"

  # Run agent probe test in Docker container
  # This simulates the actual Holon container environment
  set +e
  PROBE_OUTPUT=$(docker run --rm \
    -v "${PROBE_INPUT_DIR}:/holon/input:ro" \
    -v "${PROBE_WORKSPACE_DIR}:/holon/workspace:ro" \
    -v "${PROBE_OUTPUT_DIR}:/holon/output" \
    -v "${TEST_DIR}:/holon/agent:ro" \
    --entrypoint /bin/sh \
    "${IMAGE}" -c "cd /holon/agent && NODE_ENV=production node dist/agent.js --probe" 2>&1)
  PROBE_EXIT_CODE=$?
  set -e

  if [ ${PROBE_EXIT_CODE} -ne 0 ]; then
    echo "ERROR: Agent probe test failed with exit code ${PROBE_EXIT_CODE}" >&2
    echo "Output:" >&2
    echo "${PROBE_OUTPUT}" >&2
    exit 1
  fi

  # Check if probe wrote the expected completion message
  # Note: The "Probe completed" string is the expected success indicator from the agent's --probe mode
  if ! echo "${PROBE_OUTPUT}" | grep -q "Probe completed"; then
    echo "ERROR: Agent probe did not complete successfully" >&2
    echo "Output:" >&2
    echo "${PROBE_OUTPUT}" >&2
    exit 1
  fi

  # Verify manifest.json was written
  if [ ! -f "${PROBE_OUTPUT_DIR}/manifest.json" ]; then
    echo "ERROR: Agent probe did not write manifest.json" >&2
    exit 1
  fi

  # Clean up probe environment
  rm -rf "${PROBE_HOLON_DIR}"

  # Reset trap to only clean up TEST_DIR
  trap 'rm -rf "${TEST_DIR}"' EXIT

  echo "  ✓ Agent probe test passed"
fi

# Verify node_modules are included (check for a critical dependency)
echo "Verifying dependencies are bundled..."
if [ ! -d "${TEST_DIR}/node_modules" ]; then
  echo "ERROR: node_modules directory not found in bundle" >&2
  exit 1
fi

# Check for the Claude Agent SDK
if [ ! -d "${TEST_DIR}/node_modules/@anthropic-ai/claude-agent-sdk" ]; then
  echo "ERROR: @anthropic-ai/claude-agent-sdk not found in node_modules" >&2
  exit 1
fi
echo "  ✓ Dependencies bundled correctly"

echo ""
echo "✅ Bundle smoke test passed!"
echo ""
echo "Bundle summary:"
echo "  Path: ${BUNDLE_PATH}"
echo "  Size: $(du -h "${BUNDLE_PATH}" | cut -f1)"
echo "  Required files: ${#REQUIRED_FILES[@]} present"
echo "  Type: ES module (type: module)"
echo "  Dependencies: Bundled"
