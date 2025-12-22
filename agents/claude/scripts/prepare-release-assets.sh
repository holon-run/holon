#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)

VERSION="${1:-}"
if [ -z "${VERSION}" ]; then
  VERSION=$(node -e "const p=require('${ROOT_DIR}/package.json'); console.log(p.version || '0.0.0')" 2>/dev/null || echo "0.0.0")
fi

BUNDLE_DIR="${ROOT_DIR}/dist/agent-bundles"

# Find the bundle file for this version
BUNDLE_FILE=$(find "${BUNDLE_DIR}" -name "agent-bundle-agent-claude-${VERSION}-linux-amd64-glibc.tar.gz" | head -n1)

if [ -z "${BUNDLE_FILE}" ]; then
  echo "Error: Bundle file not found for version ${VERSION}"
  echo "Expected: agent-bundle-agent-claude-${VERSION}-linux-amd64-glibc.tar.gz"
  echo "Available files:"
  ls -la "${BUNDLE_DIR}" 2>/dev/null || echo "  No bundle directory found"
  exit 1
fi

echo "Found bundle: ${BUNDLE_FILE}"

# Create release assets with standardized names
RELEASE_DIR="${BUNDLE_DIR}/release"
mkdir -p "${RELEASE_DIR}"

RELEASE_BUNDLE="${RELEASE_DIR}/holon-agent-claude-${VERSION}.tar.gz"
RELEASE_CHECKSUM="${RELEASE_DIR}/holon-agent-claude-${VERSION}.tar.gz.sha256"

# Copy bundle with release name
cp "${BUNDLE_FILE}" "${RELEASE_BUNDLE}"

# Generate checksum
cd "${RELEASE_DIR}"
sha256sum "$(basename "${RELEASE_BUNDLE}")" > "$(basename "${RELEASE_CHECKSUM}")"

echo "Release assets prepared:"
echo "  Bundle: ${RELEASE_BUNDLE}"
echo "  Checksum: ${RELEASE_CHECKSUM}"

# Output paths for GitHub Actions to consume
echo "bundle_file=${RELEASE_BUNDLE}"
echo "checksum_file=${RELEASE_CHECKSUM}"