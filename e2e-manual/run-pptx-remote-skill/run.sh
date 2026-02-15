#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUN_ID="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="${OUT_DIR:-/tmp/holon-run-pptx-e2e-$RUN_ID}"
AGENT_HOME="${AGENT_HOME:-/tmp/holon-run-pptx-agent-$(date +%s)}"
WORKSPACE="${WORKSPACE:-/tmp/holon-run-pptx-workspace-$RUN_ID}"
INPUT_DIR="${INPUT_DIR:-$OUT_DIR/input}"
OUTPUT_DIR="${OUTPUT_DIR:-$OUT_DIR/output}"
SKILL_REF="${SKILL_REF:-ghpath:anthropics/skills/skills/pptx@main}"
RUN_TIMEOUT_SECONDS="${RUN_TIMEOUT_SECONDS:-900}"
HOLON_BIN="${HOLON_BIN:-$(cd "$ROOT_DIR/../.." && pwd)/bin/holon}"

mkdir -p "$OUT_DIR" "$WORKSPACE" "$INPUT_DIR" "$OUTPUT_DIR"

if [[ ! -x "$HOLON_BIN" ]]; then
  echo "error: holon binary not found: $HOLON_BIN. run 'make build' first." >&2
  exit 1
fi

ANTHROPIC_AUTH_TOKEN="${ANTHROPIC_AUTH_TOKEN:-$(jq -r '.env.ANTHROPIC_AUTH_TOKEN // empty' ~/.claude/settings.json 2>/dev/null || true)}"
ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-$(jq -r '.env.ANTHROPIC_BASE_URL // empty' ~/.claude/settings.json 2>/dev/null || true)}"

if [[ -z "$ANTHROPIC_AUTH_TOKEN" || -z "$ANTHROPIC_BASE_URL" ]]; then
  echo "error: missing ANTHROPIC_AUTH_TOKEN or ANTHROPIC_BASE_URL" >&2
  exit 1
fi

if [[ -z "${AGENT_BUNDLE:-}" ]]; then
  echo "[1/4] building local agent bundle"
  ./agents/claude/scripts/build-bundle.sh
  AGENT_BUNDLE="$(ls -t agents/claude/dist/agent-bundles/agent-bundle-*.tar.gz | head -n 1)"
fi

if [[ -n "${AGENT_BUNDLE:-}" && -f "${AGENT_BUNDLE:-}" ]]; then
  AGENT_BUNDLE="$(cd "$(dirname "$AGENT_BUNDLE")" && pwd)/$(basename "$AGENT_BUNDLE")"
fi

if [[ ! -f "$AGENT_BUNDLE" ]]; then
  echo "error: agent bundle not found: $AGENT_BUNDLE" >&2
  exit 1
fi

RUN_LOG="$OUT_DIR/run.log"
RUN_META="$OUT_DIR/run-meta.env"
GOAL_FILE="$OUT_DIR/goal.txt"

cat >"$GOAL_FILE" <<'EOT'
Use the pptx skill to create a polished presentation file named "holon-run-e2e.pptx" in the current workspace root.

Requirements:
1. Topic: "Holon Run E2E Integration Test".
2. 6-8 slides.
3. Include sections: overview, test scope, setup, execution steps, observed results, risks and next actions.
4. Keep content concise and practical.
5. After creation, print the exact file path you wrote.
EOT

{
  echo "OUT_DIR=$OUT_DIR"
  echo "AGENT_HOME=$AGENT_HOME"
  echo "WORKSPACE=$WORKSPACE"
  echo "INPUT_DIR=$INPUT_DIR"
  echo "OUTPUT_DIR=$OUTPUT_DIR"
  echo "AGENT_BUNDLE=$AGENT_BUNDLE"
  echo "SKILL_REF=$SKILL_REF"
  echo "RUN_TIMEOUT_SECONDS=$RUN_TIMEOUT_SECONDS"
  echo "HOLON_BIN=$HOLON_BIN"
} >"$RUN_META"

echo "[2/4] running holon run with remote pptx skill"
set +e
(
  cd /tmp
  ANTHROPIC_AUTH_TOKEN="$ANTHROPIC_AUTH_TOKEN" \
  ANTHROPIC_BASE_URL="$ANTHROPIC_BASE_URL" \
  "$HOLON_BIN" run \
    --agent "$AGENT_BUNDLE" \
    --agent-home "$AGENT_HOME" \
    --workspace "$WORKSPACE" \
    --input "$INPUT_DIR" \
    --output "$OUTPUT_DIR" \
    --skills "$SKILL_REF" \
    --goal "$(cat "$GOAL_FILE")" \
    --log-level debug \
    >"$RUN_LOG" 2>&1
) &
RUN_PID=$!
echo "RUN_PID=$RUN_PID" >>"$RUN_META"

SECONDS_WAITED=0
TIMED_OUT=0
while kill -0 "$RUN_PID" 2>/dev/null; do
  if (( SECONDS_WAITED >= RUN_TIMEOUT_SECONDS )); then
    TIMED_OUT=1
    break
  fi
  sleep 2
  SECONDS_WAITED=$((SECONDS_WAITED + 2))
done

if (( TIMED_OUT == 1 )); then
  echo "warning: holon run timed out after ${RUN_TIMEOUT_SECONDS}s; killing pid $RUN_PID" >&2
  kill "$RUN_PID" 2>/dev/null || true
  sleep 2
  kill -9 "$RUN_PID" 2>/dev/null || true
  RUN_EXIT=124
else
  wait "$RUN_PID"
  RUN_EXIT=$?
fi
set -e

echo "RUN_EXIT=$RUN_EXIT" >>"$RUN_META"

echo "[3/4] validating outputs"
PPT_PATH="$(find "$WORKSPACE" -maxdepth 2 -type f -name '*.pptx' | head -n 1 || true)"
if [[ -z "$PPT_PATH" ]]; then
  echo "error: no .pptx artifact found under workspace: $WORKSPACE" >&2
  echo "hint: check run log at $RUN_LOG" >&2
  exit 1
fi

if [[ ! -f "$AGENT_HOME/agent.yaml" ]]; then
  echo "error: expected auto-initialized agent home file missing: $AGENT_HOME/agent.yaml" >&2
  exit 1
fi

echo "PPT_PATH=$PPT_PATH" >>"$RUN_META"
echo "PPT_SIZE=$(wc -c <"$PPT_PATH")" >>"$RUN_META"

if [[ "$RUN_EXIT" -ne 0 ]]; then
  echo "error: holon run exited with non-zero status: $RUN_EXIT" >&2
  exit "$RUN_EXIT"
fi

echo "[4/4] done"
cat <<EOT
Run succeeded.
- out dir: $OUT_DIR
- agent home: $AGENT_HOME
- workspace: $WORKSPACE
- ppt file: $PPT_PATH
- run log: $RUN_LOG

Collect evidence:
  ./e2e-manual/run-pptx-remote-skill/collect.sh --out "$OUT_DIR/collected" --run-dir "$OUT_DIR"
EOT
