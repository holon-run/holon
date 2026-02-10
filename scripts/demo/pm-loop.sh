#!/usr/bin/env bash

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <owner/repo>"
  exit 1
fi

if [[ -z "${PM_GITHUB_TOKEN:-}" || -z "${DEV_GITHUB_TOKEN:-}" ]]; then
  echo "PM_GITHUB_TOKEN and DEV_GITHUB_TOKEN must be set"
  exit 1
fi

repo="$1"
root=".holon"
pm_state="$root/pm-state"
dev_state="$root/dev-state"
pm_ws="$root/pm-workspace"
dev_ws="$root/dev-workspace"

mkdir -p "$pm_state" "$dev_state" "$pm_ws" "$dev_ws"

echo "starting PM serve on :8787"
HOLON_GITHUB_TOKEN="$PM_GITHUB_TOKEN" \
  holon serve \
  --repo "$repo" \
  --webhook-port 8787 \
  --state-dir "$pm_state" \
  --controller-workspace "$pm_ws" \
  --controller-skill skills/github-controller \
  --controller-role pm \
  --tick-interval 5m &
pm_pid=$!

echo "starting Dev serve on :8788"
HOLON_GITHUB_TOKEN="$DEV_GITHUB_TOKEN" \
  holon serve \
  --repo "$repo" \
  --webhook-port 8788 \
  --state-dir "$dev_state" \
  --controller-workspace "$dev_ws" \
  --controller-skill skills/github-controller \
  --controller-role dev &
dev_pid=$!

cleanup() {
  kill "$pm_pid" "$dev_pid" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "PM PID=$pm_pid DEV PID=$dev_pid"
echo "forward webhooks to:"
echo "  http://127.0.0.1:8787/ingress/github/webhook"
echo "  http://127.0.0.1:8788/ingress/github/webhook"

wait
