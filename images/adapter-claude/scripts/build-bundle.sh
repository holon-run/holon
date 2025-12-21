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
  NODE_VERSION=$(node -v 2>/dev/null | sed 's/^v//')
fi
NODE_VERSION="${NODE_VERSION:-unknown}"

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

EMBED_RUNTIME="${BUNDLE_EMBED_RUNTIME:-false}"
RUNTIME_EMBEDDED=false
RUNTIME_DIR="${BUNDLE_RUNTIME_DIR:-}"

WORK_DIR=$(mktemp -d)
trap 'rm -rf "${WORK_DIR}"' EXIT
STAGE_DIR="${WORK_DIR}/stage"
BUNDLE_DIR="${WORK_DIR}/bundle"
mkdir -p "${STAGE_DIR}" "${BUNDLE_DIR}"

download_node_runtime() {
  local version=$1
  local platform=$2
  local arch=$3
  local libc=$4
  local node_arch=""

  if [ "${platform}" != "linux" ] || [ "${libc}" != "glibc" ]; then
    echo "Embedded runtime download only supports linux glibc bundles." >&2
    exit 1
  fi

  case "${arch}" in
    amd64)
      node_arch="x64"
      ;;
    arm64)
      node_arch="arm64"
      ;;
    *)
      echo "Unsupported BUNDLE_ARCH for runtime download: ${arch}" >&2
      exit 1
      ;;
  esac

  if [ -z "${version}" ] || [ "${version}" = "unknown" ]; then
    echo "BUNDLE_NODE_VERSION is required for runtime download." >&2
    exit 1
  fi

  local base_url="${BUNDLE_NODE_DIST_BASE:-https://nodejs.org/dist}"
  local filename="node-v${version}-${platform}-${node_arch}.tar.xz"
  local url="${base_url}/v${version}/${filename}"
  local download_dir="${WORK_DIR}/runtime-download"

  mkdir -p "${download_dir}"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "${url}" -o "${download_dir}/${filename}"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "${download_dir}/${filename}" "${url}"
  else
    echo "curl or wget is required to download the Node runtime." >&2
    exit 1
  fi

  if tar --help 2>/dev/null | grep -q -- '-J'; then
    tar -C "${download_dir}" -xJf "${download_dir}/${filename}"
  else
    echo "tar does not support -J; cannot extract .tar.xz runtime." >&2
    exit 1
  fi

  RUNTIME_DIR="${download_dir}/node-v${version}-${platform}-${node_arch}"
  if [ ! -d "${RUNTIME_DIR}" ]; then
    echo "Downloaded runtime directory not found: ${RUNTIME_DIR}" >&2
    exit 1
  fi
}

if [ "${EMBED_RUNTIME}" = "true" ] || [ "${EMBED_RUNTIME}" = "1" ]; then
  if [ -z "${RUNTIME_DIR}" ]; then
    download_node_runtime "${NODE_VERSION}" "${PLATFORM}" "${ARCH}" "${LIBC}"
  fi
  if [ ! -x "${RUNTIME_DIR}/bin/node" ]; then
    echo "Node runtime missing bin/node in ${RUNTIME_DIR}" >&2
    exit 1
  fi
  RUNTIME_EMBEDDED=true
fi

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

if [ "${RUNTIME_EMBEDDED}" = "true" ]; then
  mkdir -p "${BUNDLE_DIR}/runtime"
  cp -R "${RUNTIME_DIR}/." "${BUNDLE_DIR}/runtime/"
fi

cat > "${BUNDLE_DIR}/bin/agent" <<'ENTRYPOINT'
#!/usr/bin/env sh
set -eu

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
if [ -x "${ROOT_DIR}/runtime/bin/node" ]; then
  NODE_BIN="${ROOT_DIR}/runtime/bin/node"
else
  NODE_BIN="${NODE_BIN:-node}"
fi

exec "${NODE_BIN}" "${ROOT_DIR}/dist/adapter.js" "$@"
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
    "version": "${NODE_VERSION}",
    "embedded": ${RUNTIME_EMBEDDED}
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
  if ! tar -C "${BUNDLE_DIR}" -cf "${tmp_archive}" ${compress_flag} .; then
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
