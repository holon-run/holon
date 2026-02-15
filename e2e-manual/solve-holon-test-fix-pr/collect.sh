#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOT
Usage: $0 --run-dir <path> [--out <path>]
EOT
}

RUN_DIR=""
OUT_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-dir)
      RUN_DIR="$2"
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

if [[ -z "$RUN_DIR" || ! -d "$RUN_DIR" ]]; then
  echo "error: valid --run-dir is required" >&2
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

copy_if_exists "$RUN_DIR/run-meta.env" "$OUT_DIR/run-meta.env"
copy_if_exists "$RUN_DIR/solve.log" "$OUT_DIR/solve.log"
copy_if_exists "$RUN_DIR/pr-body.md" "$OUT_DIR/pr-body.md"
copy_if_exists "$RUN_DIR/pr-comment.md" "$OUT_DIR/pr-comment.md"

OUTPUT_DIR=""
LOCAL_REPO=""
AGENT_HOME=""
if [[ -f "$RUN_DIR/run-meta.env" ]]; then
  OUTPUT_DIR="$(grep '^OUTPUT_DIR=' "$RUN_DIR/run-meta.env" | head -n1 | cut -d= -f2-)"
  LOCAL_REPO="$(grep '^LOCAL_REPO=' "$RUN_DIR/run-meta.env" | head -n1 | cut -d= -f2-)"
  AGENT_HOME="$(grep '^AGENT_HOME=' "$RUN_DIR/run-meta.env" | head -n1 | cut -d= -f2-)"
fi

if [[ -n "$OUTPUT_DIR" && -d "$OUTPUT_DIR" ]]; then
  find "$OUTPUT_DIR" -maxdepth 3 -type f | sort >"$OUT_DIR/output-files.txt"
  copy_if_exists "$OUTPUT_DIR/manifest.json" "$OUT_DIR/manifest.json"
  copy_if_exists "$OUTPUT_DIR/summary.md" "$OUT_DIR/summary.md"
  copy_if_exists "$OUTPUT_DIR/publish-results.json" "$OUT_DIR/publish-results.json"
fi

if [[ -n "$LOCAL_REPO" && -d "$LOCAL_REPO/.git" ]]; then
  (
    cd "$LOCAL_REPO"
    git status --short --branch >"$OUT_DIR/local-repo-status.txt"
    git log --oneline -n 10 >"$OUT_DIR/local-repo-log.txt"
    git diff --stat >"$OUT_DIR/local-repo-diffstat.txt" || true
  )
fi

if [[ -n "$AGENT_HOME" && -d "$AGENT_HOME" ]]; then
  find "$AGENT_HOME" -maxdepth 3 -type f | sort >"$OUT_DIR/agent-home-files.txt"
  copy_if_exists "$AGENT_HOME/agent.yaml" "$OUT_DIR/agent.yaml"
  copy_if_exists "$AGENT_HOME/ROLE.md" "$OUT_DIR/ROLE.md"
fi

cat >"$OUT_DIR/summary.txt" <<EOT
run_dir=$RUN_DIR
collected_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
files:
$(ls -1 "$OUT_DIR")
EOT

echo "evidence collected at: $OUT_DIR"
