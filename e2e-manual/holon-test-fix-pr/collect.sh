#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-holon-run/holon-test}"
RUN_ID="${1:-}"
OUT="${OUT:-artifacts}"

if [ -z "$RUN_ID" ]; then
  echo "usage: $0 <github-actions-run-id>" >&2
  exit 2
fi

mkdir -p "$OUT"
gh run view "$RUN_ID" --repo "$REPO" --json url,status,conclusion,jobs > "$OUT/run.json"
gh run download "$RUN_ID" --repo "$REPO" --dir "$OUT"
echo "Collected evidence under $OUT"
