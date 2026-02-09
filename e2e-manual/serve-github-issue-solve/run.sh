#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-holon-run/holon-test}"
PORT="${PORT:-18080}"
ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/artifacts/run-$(date +%Y%m%d-%H%M%S)}"
STATE_DIR="${STATE_DIR:-/tmp/holon-serve-e2e-$(date +%s)}"

mkdir -p "$OUT_DIR"

if [[ ! -x "./bin/holon" ]]; then
  echo "error: ./bin/holon not found. run 'make build' first." >&2
  exit 1
fi

if ! gh auth status >/dev/null 2>&1; then
  echo "error: gh auth is required" >&2
  exit 1
fi

ANTHROPIC_AUTH_TOKEN="${ANTHROPIC_AUTH_TOKEN:-$(jq -r '.env.ANTHROPIC_AUTH_TOKEN // empty' ~/.claude/settings.json)}"
ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-$(jq -r '.env.ANTHROPIC_BASE_URL // empty' ~/.claude/settings.json)}"

if [[ -z "$ANTHROPIC_AUTH_TOKEN" || -z "$ANTHROPIC_BASE_URL" ]]; then
  echo "error: missing ANTHROPIC_AUTH_TOKEN or ANTHROPIC_BASE_URL" >&2
  exit 1
fi

SERVE_LOG="$OUT_DIR/serve.log"
WEBHOOK_LOG="$OUT_DIR/webhook-forward.log"
RUN_META="$OUT_DIR/run-meta.env"

echo "STATE_DIR=$STATE_DIR" | tee -a "$RUN_META"
echo "REPO=$REPO" | tee -a "$RUN_META"
echo "PORT=$PORT" | tee -a "$RUN_META"

echo "[1/4] starting holon serve"
ANTHROPIC_AUTH_TOKEN="$ANTHROPIC_AUTH_TOKEN" \
ANTHROPIC_BASE_URL="$ANTHROPIC_BASE_URL" \
./bin/holon serve --repo "$REPO" --webhook-port "$PORT" --state-dir "$STATE_DIR" --log-level debug \
  >"$SERVE_LOG" 2>&1 &
SERVE_PID=$!
echo "SERVE_PID=$SERVE_PID" | tee -a "$RUN_META"

sleep 2

if ! kill -0 "$SERVE_PID" 2>/dev/null; then
  echo "error: serve exited early; see $SERVE_LOG" >&2
  exit 1
fi

echo "[2/4] starting webhook forward"
gh webhook forward \
  --repo="$REPO" \
  --events=issues,issue_comment,pull_request,pull_request_review,pull_request_review_comment \
  --url="http://127.0.0.1:$PORT/ingress/github/webhook" \
  >"$WEBHOOK_LOG" 2>&1 &
WEBHOOK_PID=$!
echo "WEBHOOK_PID=$WEBHOOK_PID" | tee -a "$RUN_META"

sleep 3

echo "[3/4] creating test issue"
ISSUE_TITLE="E2E serve issue solve $(date +%H%M%S)"
ISSUE_BODY_FILE="$OUT_DIR/issue-body.md"
cat > "$ISSUE_BODY_FILE" <<'EOT'
E2E serve manual test.
Please create a tiny docs update PR.
EOT

ISSUE_URL=$(gh issue create --repo "$REPO" --title "$ISSUE_TITLE" --body-file "$ISSUE_BODY_FILE")
ISSUE_NUMBER="${ISSUE_URL##*/}"
echo "ISSUE_URL=$ISSUE_URL" | tee -a "$RUN_META"
echo "ISSUE_NUMBER=$ISSUE_NUMBER" | tee -a "$RUN_META"

echo "[4/4] posting trigger comment"
COMMENT_BODY_FILE="$OUT_DIR/comment-body.md"
cat > "$COMMENT_BODY_FILE" <<'EOT'
@holonbot
please solve this issue and open a PR.
EOT
COMMENT_URL=$(gh issue comment --repo "$REPO" "$ISSUE_NUMBER" --body-file "$COMMENT_BODY_FILE")
echo "COMMENT_URL=$COMMENT_URL" | tee -a "$RUN_META"

echo "Waiting 30s for event ingestion..."
sleep 30

cat <<EOT

Run started.
- state dir: $STATE_DIR
- output dir: $OUT_DIR
- serve log: $SERVE_LOG
- webhook log: $WEBHOOK_LOG

To stop background processes:
  kill $SERVE_PID $WEBHOOK_PID

Then run:
  ./e2e-manual/serve-github-issue-solve/collect.sh --state-dir "$STATE_DIR" --out "$OUT_DIR/collected"
EOT
