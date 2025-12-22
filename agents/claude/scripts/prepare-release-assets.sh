#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)

VERSION="${1:-}"
if [ -z "${VERSION}" ]; then
  VERSION=$(node -e "const p=require('${ROOT_DIR}/package.json'); console.log(p.version || '0.0.0')" 2>/dev/null || echo "0.0.0")
fi

# Platform/architecture suffix for the agent bundle filename.
# Defaults to linux-amd64-glibc but can be overridden via AGENT_PLATFORM_SUFFIX.
PLATFORM="${2:-linux-amd64-glibc}"

BUNDLE_DIR="${ROOT_DIR}/dist/agent-bundles"

# Find the bundle file for this version and platform suffix
BUNDLE_FILE=$(find "${BUNDLE_DIR}" -name "agent-bundle-agent-claude-${VERSION}-${PLATFORM}.tar.gz" | head -n1)

if [ -z "${BUNDLE_FILE}" ]; then
  echo "Error: Bundle file not found for version ${VERSION} and platform suffix ${PLATFORM}" >&2
  echo "Expected pattern: agent-bundle-agent-claude-${VERSION}-${PLATFORM}.tar.gz" >&2
  echo "Available files:" >&2
  ls -la "${BUNDLE_DIR}" 2>/dev/null | sed 's/^/  /' >&2 || echo "  No bundle directory found" >&2
  exit 1
fi

echo "Found bundle: ${BUNDLE_FILE}" >&2

# Create release assets with standardized names
RELEASE_DIR="${BUNDLE_DIR}/release"
mkdir -p "${RELEASE_DIR}"

RELEASE_BUNDLE="${RELEASE_DIR}/holon-agent-claude-${VERSION}.tar.gz"
RELEASE_CHECKSUM="${RELEASE_DIR}/holon-agent-claude-${VERSION}.tar.gz.sha256"

# Copy bundle with release name
cp "${BUNDLE_FILE}" "${RELEASE_BUNDLE}"

# Generate checksum
# Note: The checksum file records only the bundle's basename, not a path.
# When verifying with `sha256sum -c`, ensure the bundle and checksum files
# are in the same directory (e.g., download both into one folder first).
# Note: The checksum file records only the bundle's basename, not a path.
# When verifying with `sha256sum -c`, ensure the bundle and checksum files
# are in the same directory (e.g., download both into one folder first).
cd "${RELEASE_DIR}"
sha256sum "$(basename "${RELEASE_BUNDLE}")" > "$(basename "${RELEASE_CHECKSUM}")"

# Send informational messages to stderr
echo "Release assets prepared:" >&2
echo "  Bundle: ${RELEASE_BUNDLE}" >&2
echo "  Checksum: ${RELEASE_CHECKSUM}" >&2
echo "" >&2
echo "To verify checksum:" >&2
echo "  sha256sum -c \"$(basename \"${RELEASE_CHECKSUM}\")\"" >&2

# Output only the environment variables to stdout
echo "bundle_file=${RELEASE_BUNDLE}"
echo "checksum_file=${RELEASE_CHECKSUM}"