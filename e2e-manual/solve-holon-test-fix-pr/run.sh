#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUN_ID="$(date +%Y%m%d-%H%M%S)"
REPO="${REPO:-holon-run/holon-test}"
OUT_DIR="${OUT_DIR:-/tmp/holon-solve-fix-e2e-$RUN_ID}"
AGENT_HOME="${AGENT_HOME:-/tmp/holon-solve-fix-agent-$RUN_ID}"
OUTPUT_DIR="${OUTPUT_DIR:-$OUT_DIR/output}"
LOCAL_REPO="${LOCAL_REPO:-/tmp/holon-solve-fix-repo-$RUN_ID}"
BRANCH_NAME="${BRANCH_NAME:-e2e/fix-$RUN_ID}"
RUN_TIMEOUT_SECONDS="${RUN_TIMEOUT_SECONDS:-1800}"
HOLON_BIN="${HOLON_BIN:-$(cd "$ROOT_DIR/../.." && pwd)/bin/holon}"

mkdir -p "$OUT_DIR" "$OUTPUT_DIR"

if [[ ! -x "$HOLON_BIN" ]]; then
  echo "error: holon binary not found: $HOLON_BIN. run 'make build' first." >&2
  exit 1
fi
if ! gh auth status >/dev/null 2>&1; then
  echo "error: gh auth is required" >&2
  exit 1
fi

ANTHROPIC_AUTH_TOKEN="${ANTHROPIC_AUTH_TOKEN:-$(jq -r '.env.ANTHROPIC_AUTH_TOKEN // empty' ~/.claude/settings.json 2>/dev/null || true)}"
ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-$(jq -r '.env.ANTHROPIC_BASE_URL // empty' ~/.claude/settings.json 2>/dev/null || true)}"
if [[ -z "$ANTHROPIC_AUTH_TOKEN" || -z "$ANTHROPIC_BASE_URL" ]]; then
  echo "error: missing ANTHROPIC_AUTH_TOKEN or ANTHROPIC_BASE_URL" >&2
  exit 1
fi

if [[ -z "${AGENT_BUNDLE:-}" ]]; then
  echo "[1/9] building local agent bundle"
  (cd "$ROOT_DIR/../.." && ./agents/claude/scripts/build-bundle.sh)
  AGENT_BUNDLE="$(cd "$ROOT_DIR/../.." && ls -t agents/claude/dist/agent-bundles/agent-bundle-*.tar.gz | head -n 1)"
fi
if [[ -n "${AGENT_BUNDLE:-}" && -f "${AGENT_BUNDLE:-}" ]]; then
  AGENT_BUNDLE="$(cd "$(dirname "$AGENT_BUNDLE")" && pwd)/$(basename "$AGENT_BUNDLE")"
fi
if [[ ! -f "$AGENT_BUNDLE" ]]; then
  echo "error: agent bundle not found: $AGENT_BUNDLE" >&2
  exit 1
fi

RUN_LOG="$OUT_DIR/solve.log"
RUN_META="$OUT_DIR/run-meta.env"
PR_BODY_FILE="$OUT_DIR/pr-body.md"
COMMENT_BODY_FILE="$OUT_DIR/pr-comment.md"

echo "[2/9] cloning target repo"
gh repo clone "$REPO" "$LOCAL_REPO"

echo "[3/9] preparing fixture PR branch"
(
  cd "$LOCAL_REPO"
  git config user.name "${GIT_USER_NAME:-holon-e2e}"
  git config user.email "${GIT_USER_EMAIL:-holon-e2e@example.com}"
  git checkout -b "$BRANCH_NAME"
  mkdir -p docs
  FIXTURE_FILE="docs/e2e-fix-$RUN_ID.md"
  cat >"$FIXTURE_FILE" <<EOT
# E2E Fix Fixture

this line has TODO marker and awkward wording.
EOT
  git add "$FIXTURE_FILE"
  git commit -m "test: add fix fixture $RUN_ID"
  git push -u origin "$BRANCH_NAME"
)

cat >"$PR_BODY_FILE" <<EOT
E2E fix fixture PR.

- scenario: holon solve pr with github-pr-fix
- run id: $RUN_ID
EOT

echo "[4/9] opening fixture PR"
PR_URL="$(gh pr create --repo "$REPO" --head "$BRANCH_NAME" --base main --title "E2E fix fixture $RUN_ID" --body-file "$PR_BODY_FILE")"
PR_NUMBER="${PR_URL##*/}"

cat >"$COMMENT_BODY_FILE" <<'EOT'
Please address the following:
1. Remove the "TODO marker" wording.
2. Rewrite the sentence into clear professional wording.
3. Commit and push the fix.
EOT
gh pr comment "$PR_NUMBER" --repo "$REPO" --body-file "$COMMENT_BODY_FILE" >/dev/null

HEAD_BEFORE="$(gh pr view "$PR_NUMBER" --repo "$REPO" --json headRefOid --jq .headRefOid)"

{
  echo "RUN_ID=$RUN_ID"
  echo "REPO=$REPO"
  echo "PR_URL=$PR_URL"
  echo "PR_NUMBER=$PR_NUMBER"
  echo "HEAD_BEFORE=$HEAD_BEFORE"
  echo "OUT_DIR=$OUT_DIR"
  echo "OUTPUT_DIR=$OUTPUT_DIR"
  echo "AGENT_HOME=$AGENT_HOME"
  echo "AGENT_BUNDLE=$AGENT_BUNDLE"
  echo "LOCAL_REPO=$LOCAL_REPO"
  echo "BRANCH_NAME=$BRANCH_NAME"
  echo "RUN_TIMEOUT_SECONDS=$RUN_TIMEOUT_SECONDS"
} >"$RUN_META"

echo "[5/9] running holon solve pr (github-pr-fix) with explicit --workspace"
set +e
(
  export ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL
  "$HOLON_BIN" solve "$PR_URL" \
  --agent "$AGENT_BUNDLE" \
  --agent-home "$AGENT_HOME" \
  --workspace "$LOCAL_REPO" \
  --skills github-pr-fix \
  --output "$OUTPUT_DIR" \
  --cleanup none \
  --assistant-output none \
  --log-level debug \
  >"$RUN_LOG" 2>&1
) &
RUN_PID=$!
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

if [[ "$RUN_EXIT" -ne 0 ]]; then
  echo "error: holon solve fix failed (exit=$RUN_EXIT). see $RUN_LOG" >&2
  exit "$RUN_EXIT"
fi

MANIFEST="$OUTPUT_DIR/manifest.json"
if [[ ! -f "$MANIFEST" ]]; then
  echo "error: manifest missing: $MANIFEST" >&2
  exit 1
fi
STATUS="$(jq -r '.status // empty' "$MANIFEST")"
OUTCOME="$(jq -r '.outcome // empty' "$MANIFEST")"
if [[ "$STATUS" != "completed" || "$OUTCOME" != "success" ]]; then
  echo "error: unexpected manifest status/outcome: $STATUS/$OUTCOME" >&2
  exit 1
fi

echo "[6/9] validating PR head movement"
sleep 5
HEAD_AFTER="$(gh pr view "$PR_NUMBER" --repo "$REPO" --json headRefOid --jq .headRefOid)"
echo "HEAD_AFTER=$HEAD_AFTER" >>"$RUN_META"
if [[ "$HEAD_BEFORE" == "$HEAD_AFTER" ]]; then
  echo "error: expected branch head to move after fix; sha unchanged: $HEAD_AFTER" >&2
  exit 1
fi

echo "[7/9] validating publish-results for replies"
if [[ -f "$OUTPUT_DIR/publish-results.json" ]]; then
  FAILED_REPLIES="$(jq '[.actions[] | select((.type=="reply_review" or .type=="reply_review_comment") and .status!="ok")] | length' "$OUTPUT_DIR/publish-results.json" 2>/dev/null || echo 0)"
  echo "FAILED_REPLIES=$FAILED_REPLIES" >>"$RUN_META"
fi

echo "[8/9] summary"
echo "PR: $PR_URL"
echo "head before: $HEAD_BEFORE"
echo "head after:  $HEAD_AFTER"

echo "[9/9] done"
cat <<EOT
Run succeeded.
- out dir: $OUT_DIR
- pr: $PR_URL
- log: $RUN_LOG

Collect evidence:
  ./e2e-manual/solve-holon-test-fix-pr/collect.sh --run-dir "$OUT_DIR" --out "$OUT_DIR/collected"
EOT
