#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOT
Usage: $0 --state-dir <path> [--out <path>]
EOT
}

STATE_DIR=""
OUT_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --state-dir)
      STATE_DIR="$2"
      shift 2
      ;;
    --out)
      OUT_DIR="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown arg: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$STATE_DIR" ]]; then
  echo "error: --state-dir is required" >&2
  usage
  exit 1
fi

if [[ ! -d "$STATE_DIR" ]]; then
  echo "error: state dir does not exist: $STATE_DIR" >&2
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/artifacts/collect-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$OUT_DIR"

copy_if_exists() {
  local src="$1"
  local dst="$2"
  if [[ -f "$src" ]]; then
    cp "$src" "$dst"
  fi
}

copy_if_exists "$STATE_DIR/events.ndjson" "$OUT_DIR/events.ndjson"
copy_if_exists "$STATE_DIR/decisions.ndjson" "$OUT_DIR/decisions.ndjson"
copy_if_exists "$STATE_DIR/actions.ndjson" "$OUT_DIR/actions.ndjson"
copy_if_exists "$STATE_DIR/runtime-state.json" "$OUT_DIR/runtime-state.json"
copy_if_exists "$STATE_DIR/serve-state.json" "$OUT_DIR/serve-state.json"
copy_if_exists "$STATE_DIR/controller-runtime/output/evidence/execution.log" "$OUT_DIR/execution.log"
copy_if_exists "$STATE_DIR/controller-runtime/output/manifest.json" "$OUT_DIR/manifest.json"

cat > "$OUT_DIR/summary.txt" <<EOT
state_dir=$STATE_DIR
collected_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)

files:
$(ls -1 "$OUT_DIR")
EOT

echo "evidence collected at: $OUT_DIR"
