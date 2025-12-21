#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)

NAME="${BUNDLE_NAME:-agent-claude}"
VERSION="${BUNDLE_VERSION:-}"
if [ -z "${VERSION}" ]; then
  VERSION=$(node -e "const p=require('${ROOT_DIR}/package.json'); console.log(p.version || '0.0.0')" 2>/dev/null || echo "0.0.0")
fi
PLATFORM="${BUNDLE_PLATFORM:-linux}"
ARCH="${BUNDLE_ARCH:-amd64}"
LIBC="${BUNDLE_LIBC:-glibc}"
NODE_VERSION="${BUNDLE_NODE_VERSION:-}"
if [ -z "${NODE_VERSION}" ]; then
  NODE_VERSION="20"
fi

WORK_DIR=$(mktemp -d)
trap 'rm -rf "${WORK_DIR}"' EXIT

BUNDLE_OUTPUT_DIR="${WORK_DIR}/bundles"
BUNDLE_ARCHIVE="${BUNDLE_OUTPUT_DIR}/agent-bundle-${NAME}-${VERSION}-${PLATFORM}-${ARCH}-${LIBC}.tar.gz"

BUNDLE_OUTPUT_DIR="${BUNDLE_OUTPUT_DIR}" \
BUNDLE_NODE_VERSION="${NODE_VERSION}" \
BUNDLE_PLATFORM="${PLATFORM}" \
BUNDLE_ARCH="${ARCH}" \
BUNDLE_LIBC="${LIBC}" \
  "${SCRIPT_DIR}/build-bundle.sh"

if [ ! -f "${BUNDLE_ARCHIVE}" ]; then
  echo "Bundle archive not found: ${BUNDLE_ARCHIVE}" >&2
  exit 1
fi

BUNDLE_EXTRACT="${WORK_DIR}/bundle"
mkdir -p "${BUNDLE_EXTRACT}"
tar -xzf "${BUNDLE_ARCHIVE}" -C "${BUNDLE_EXTRACT}"

HOLON_DIR="${WORK_DIR}/holon"
INPUT_DIR="${HOLON_DIR}/input"
WORKSPACE_DIR="${HOLON_DIR}/workspace"
OUTPUT_DIR="${HOLON_DIR}/output"

mkdir -p "${INPUT_DIR}" "${WORKSPACE_DIR}" "${OUTPUT_DIR}"
cat > "${INPUT_DIR}/spec.yaml" <<'SPEC'
version: "v1"
kind: Holon
metadata:
  name: "bundle-verify"
context:
  workspace: "/holon/workspace"
goal:
  description: "Verify that the agent bundle can start and write outputs."
output:
  artifacts:
    - path: "manifest.json"
      required: true
    - path: "summary.md"
      required: false
SPEC

echo "Bundle verification workspace" > "${WORKSPACE_DIR}/README.md"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required to verify the bundle runtime" >&2
  exit 1
fi

IMAGE="${BUNDLE_VERIFY_IMAGE:-node:${NODE_VERSION}-bookworm-slim}"
RUN_SCRIPT="${BUNDLE_VERIFY_RUN_SCRIPT:-/holon/agent/bin/agent --probe}"

set +e
DOCKER_OUTPUT=$(docker run --rm \
  -v "${INPUT_DIR}:/holon/input" \
  -v "${WORKSPACE_DIR}:/holon/workspace" \
  -v "${OUTPUT_DIR}:/holon/output" \
  -v "${BUNDLE_EXTRACT}:/holon/agent" \
  --entrypoint /bin/sh \
  "${IMAGE}" -c "${RUN_SCRIPT}" 2>&1)
EXIT_CODE=$?
set -e

if [ ! -f "${OUTPUT_DIR}/manifest.json" ]; then
  echo "Bundle verification failed: manifest.json not found." >&2
  echo "Adapter output:" >&2
  echo "${DOCKER_OUTPUT}" >&2
  exit 1
fi

if [ "${EXIT_CODE}" -ne 0 ]; then
  echo "Bundle verification: adapter exited with code ${EXIT_CODE}."
fi

echo "Bundle verification complete."
