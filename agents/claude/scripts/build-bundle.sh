#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)
REPO_ROOT=$(cd "${ROOT_DIR}/../.." && pwd)

DEFAULT_OUTPUT_DIR="${ROOT_DIR}/dist/agent-bundles"
OUTPUT_DIR="${BUNDLE_OUTPUT_DIR:-${DEFAULT_OUTPUT_DIR}}"

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
  NODE_VERSION="unknown"
fi

ENGINE_NAME="${BUNDLE_ENGINE_NAME:-claude-code}"
ENGINE_SDK="${BUNDLE_ENGINE_SDK:-@anthropic-ai/claude-agent-sdk}"
ENGINE_SDK_VERSION="${BUNDLE_ENGINE_SDK_VERSION:-}"
if [ -z "${ENGINE_SDK_VERSION}" ]; then
  ENGINE_SDK_VERSION=$(node - "${ROOT_DIR}" "${ENGINE_SDK}" <<'NODE'
const fs = require("fs");
const path = require("path");
const root = process.argv[2];
const name = process.argv[3];
let version = "";
try {
  const lock = JSON.parse(fs.readFileSync(path.join(root, "package-lock.json"), "utf8"));
  const nodePath = `node_modules/${name}`;
  if (lock.packages && lock.packages[nodePath]?.version) {
    version = lock.packages[nodePath].version;
  } else if (lock.dependencies && lock.dependencies[name]?.version) {
    version = lock.dependencies[name].version;
  }
} catch {}
if (!version) {
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
    version = (pkg.dependencies || {})[name] || "";
  } catch {}
}
process.stdout.write(version || "unknown");
NODE
)
fi

WORK_DIR=$(mktemp -d)
trap 'rm -rf "${WORK_DIR}"' EXIT
STAGE_DIR="${WORK_DIR}/stage"
BUNDLE_DIR="${WORK_DIR}/bundle"
mkdir -p "${STAGE_DIR}" "${BUNDLE_DIR}"

# Copy sources to a staging directory for a clean build.
tar -C "${ROOT_DIR}" -cf - \
  --exclude "./node_modules" \
  --exclude "./dist" \
  --exclude "./bundle-out" \
  --exclude "./.git" \
  . | tar -C "${STAGE_DIR}" -xf -

pushd "${STAGE_DIR}" >/dev/null
npm ci
npm run build
npm prune --omit=dev
popd >/dev/null

if [ ! -d "${STAGE_DIR}/dist" ]; then
  echo "dist/ not found after build" >&2
  exit 1
fi
if [ ! -d "${STAGE_DIR}/node_modules" ]; then
  echo "node_modules/ not found after build" >&2
  exit 1
fi

mkdir -p "${BUNDLE_DIR}/bin"
cp -R "${STAGE_DIR}/dist" "${BUNDLE_DIR}/dist"
cp -R "${STAGE_DIR}/node_modules" "${BUNDLE_DIR}/node_modules"

# Copy package.json to ensure ES modules work correctly
cp "${STAGE_DIR}/package.json" "${BUNDLE_DIR}/package.json"
if [ ! -f "${BUNDLE_DIR}/package.json" ]; then
  echo "package.json not found in bundle" >&2
  exit 1
fi

cat > "${BUNDLE_DIR}/bin/agent" <<'ENTRYPOINT'
#!/usr/bin/env sh
set -eu

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
NODE_BIN="${NODE_BIN:-node}"

exec "${NODE_BIN}" "${ROOT_DIR}/dist/agent.js" "$@"
ENTRYPOINT
chmod +x "${BUNDLE_DIR}/bin/agent"

cat > "${BUNDLE_DIR}/manifest.json" <<MANIFEST_EOF
{
  "bundleVersion": "1",
  "name": "${NAME}",
  "version": "${VERSION}",
  "entry": "bin/agent",
  "platform": "${PLATFORM}",
  "arch": "${ARCH}",
  "libc": "${LIBC}",
  "engine": {
    "name": "${ENGINE_NAME}",
    "sdk": "${ENGINE_SDK}",
    "sdkVersion": "${ENGINE_SDK_VERSION}"
  },
  "runtime": {
    "type": "node",
    "version": "${NODE_VERSION}"
  },
  "env": {
    "NODE_ENV": "production"
  },
  "capabilities": {
    "needsNetwork": true,
    "needsGit": true
  }
}
MANIFEST_EOF

EXT="tar.gz"
TAR_COMPRESS_FLAG="-z"

mkdir -p "${OUTPUT_DIR}"

create_archive() {
  local archive_path=$1
  local compress_flag=$2
  local tmp_archive="${archive_path}.tmp"
  rm -f "${tmp_archive}"
  if ! tar -C "${BUNDLE_DIR}" -cf "${tmp_archive}" "${compress_flag}" .; then
    rm -f "${tmp_archive}"
    return 1
  fi
  mv "${tmp_archive}" "${archive_path}"
}

ARCHIVE_NAME="agent-bundle-${NAME}-${VERSION}-${PLATFORM}-${ARCH}-${LIBC}.${EXT}"
ARCHIVE_PATH="${OUTPUT_DIR}/${ARCHIVE_NAME}"
if ! create_archive "${ARCHIVE_PATH}" "${TAR_COMPRESS_FLAG}"; then
  exit 1
fi

echo "Bundle created: ${ARCHIVE_PATH}"
