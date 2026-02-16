#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="${REPO:-holon-run/holon-test}"
PORT="${PORT:-18089}"
RUN_ID="${RUN_ID:-$(date +%Y%m%d-%H%M%S)}"
PREFIX="[AUTONOMY-E2E-$RUN_ID]"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/artifacts/run-$RUN_ID}"
AGENT_HOME="${AGENT_HOME:-/tmp/holon-serve-autonomy-agent-$RUN_ID}"
HOLON_BIN="${HOLON_BIN:-$(cd "$ROOT_DIR/../.." && pwd)/bin/holon}"
WAIT_PLAN_SECONDS="${WAIT_PLAN_SECONDS:-900}"
WAIT_PR_SECONDS="${WAIT_PR_SECONDS:-1800}"
WAIT_MERGE_SECONDS="${WAIT_MERGE_SECONDS:-2400}"
POLL_INTERVAL_SECONDS="${POLL_INTERVAL_SECONDS:-15}"
RPC_READY_TIMEOUT_SECONDS="${RPC_READY_TIMEOUT_SECONDS:-180}"
MESSAGE_SEND_TIMEOUT_SECONDS="${MESSAGE_SEND_TIMEOUT_SECONDS:-180}"
MESSAGE_SEND_RETRY_SECONDS="${MESSAGE_SEND_RETRY_SECONDS:-20}"

mkdir -p "$OUT_DIR"

if [[ ! -x "$HOLON_BIN" ]]; then
  echo "error: holon binary not found: $HOLON_BIN (run 'make build')" >&2
  exit 1
fi
if ! gh auth status >/dev/null 2>&1; then
  echo "error: gh auth is required" >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "error: docker is required" >&2
  exit 1
fi
if lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "error: port $PORT is already in use; set PORT=<free-port> and retry" >&2
  exit 1
fi

ANTHROPIC_AUTH_TOKEN="${ANTHROPIC_AUTH_TOKEN:-$(jq -r '.env.ANTHROPIC_AUTH_TOKEN // empty' ~/.claude/settings.json 2>/dev/null || true)}"
ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-$(jq -r '.env.ANTHROPIC_BASE_URL // empty' ~/.claude/settings.json 2>/dev/null || true)}"
if [[ -z "$ANTHROPIC_AUTH_TOKEN" || -z "$ANTHROPIC_BASE_URL" ]]; then
  echo "error: missing ANTHROPIC_AUTH_TOKEN or ANTHROPIC_BASE_URL" >&2
  exit 1
fi

if [[ -z "${HOLON_GITHUB_TOKEN:-}" ]]; then
  HOLON_GITHUB_TOKEN="$(gh auth token)"
fi
if [[ -z "$HOLON_GITHUB_TOKEN" ]]; then
  echo "error: HOLON_GITHUB_TOKEN is empty" >&2
  exit 1
fi

echo "[1/8] building local agent bundle"
(cd "$ROOT_DIR/../.." && ./agents/claude/scripts/build-bundle.sh >/dev/null)
AGENT_BUNDLE="$(cd "$ROOT_DIR/../.." && ls -t agents/claude/dist/agent-bundles/agent-bundle-*.tar.gz | head -n 1)"
if [[ -z "$AGENT_BUNDLE" || ! -f "$AGENT_BUNDLE" ]]; then
  echo "error: local agent bundle not found" >&2
  exit 1
fi
AGENT_BUNDLE="$(cd "$(dirname "$AGENT_BUNDLE")" && pwd)/$(basename "$AGENT_BUNDLE")"

ROLE_FILE="$AGENT_HOME/ROLE.md"
PROMPT_FILE="$OUT_DIR/prompt.md"
RUN_META="$OUT_DIR/run-meta.env"
SERVE_LOG="$OUT_DIR/serve.log"
SERVE_PID=""

mkdir -p "$AGENT_HOME"

cleanup() {
  if [[ -n "$SERVE_PID" ]] && kill -0 "$SERVE_PID" 2>/dev/null; then
    kill "$SERVE_PID" 2>/dev/null || true
    sleep 1
    kill -9 "$SERVE_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "[2/8] initializing agent home"
"$HOLON_BIN" agent init --agent-home "$AGENT_HOME" --template serve-controller >/dev/null

cat >"$ROLE_FILE" <<EOT
# ROLE: Autonomous Project Driver

You are a long-running autonomous project operator.

Primary responsibilities:
1. Convert high-level goals into milestone-driven plans.
2. Create parent issue and child issues with clear acceptance criteria.
3. Use @holonbot trigger comments on execution issues to delegate implementation.
4. Follow up execution status, review PR outcomes, and drive issues to done.
5. Prefer safe, incremental delivery and explicit status updates.

Execution rules:
- Work against repository: $REPO
- Use run prefix: $PREFIX
- Keep all created issues/PRs traceable by including the prefix in title/body.
- Do not perform destructive repo admin operations.
EOT

cat >"$PROMPT_FILE" <<EOT
Repository: $REPO

Goal:
Design and execute a small autonomous delivery cycle for this repo.

Required actions:
1) Create exactly one parent tracking issue with title starting with "$PREFIX Parent".
2) Create at least two child issues with titles starting with "$PREFIX Child" and link them from parent.
3) Pick one child issue and trigger implementation by posting a comment that includes "@holonbot".
4) Track resulting PR activity and post progress updates in the parent issue.
5) If a generated PR is clearly safe and CI passes, merge it; otherwise explain blockers in the parent issue.

Execution constraints (mandatory):
- Do NOT clone/fetch repository locally.
- Use GitHub CLI (gh) directly against "$REPO" for issue/comment/PR operations.
- Each Bash command MUST be a single operation:
  no &&, no ||, no pipes |, no redirects > <, no subshells.
- If a command fails, issue a new single command for the next step.

Output discipline:
- Keep all titles/messages in English.
- Include concise acceptance criteria in each child issue.
- Keep actions incremental and auditable.
EOT

{
  echo "RUN_ID=$RUN_ID"
  echo "REPO=$REPO"
  echo "PREFIX=$PREFIX"
  echo "PORT=$PORT"
  echo "OUT_DIR=$OUT_DIR"
  echo "AGENT_HOME=$AGENT_HOME"
  echo "ROLE_FILE=$ROLE_FILE"
  echo "PROMPT_FILE=$PROMPT_FILE"
  echo "AGENT_BUNDLE=$AGENT_BUNDLE"
} >"$RUN_META"

echo "[3/8] starting holon serve"
ANTHROPIC_AUTH_TOKEN="$ANTHROPIC_AUTH_TOKEN" \
ANTHROPIC_BASE_URL="$ANTHROPIC_BASE_URL" \
HOLON_GITHUB_TOKEN="$HOLON_GITHUB_TOKEN" \
HOLON_AGENT="$AGENT_BUNDLE" \
"$HOLON_BIN" serve \
  --agent-home "$AGENT_HOME" \
  --webhook-port "$PORT" \
  --no-subscriptions \
  --controller-warmup-best-effort \
  --log-level debug \
  >"$SERVE_LOG" 2>&1 &
SERVE_PID=$!
echo "SERVE_PID=$SERVE_PID" >>"$RUN_META"

sleep 3
if ! kill -0 "$SERVE_PID" 2>/dev/null; then
  echo "error: serve exited early; see $SERVE_LOG" >&2
  exit 1
fi

RPC_URL="http://127.0.0.1:$PORT/rpc"
echo "[4/8] waiting for rpc readiness"
ELAPSED=0
while (( ELAPSED <= RPC_READY_TIMEOUT_SECONDS )); do
  if ! kill -0 "$SERVE_PID" 2>/dev/null; then
    echo "error: serve exited while waiting rpc readiness; see $SERVE_LOG" >&2
    exit 1
  fi
  if curl -fsS \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":1,"method":"holon/status","params":{}}' \
    "$RPC_URL" >/dev/null 2>&1; then
    break
  fi
  sleep 2
  ELAPSED=$((ELAPSED + 2))
done
if (( ELAPSED > RPC_READY_TIMEOUT_SECONDS )); then
  echo "error: rpc endpoint not ready within ${RPC_READY_TIMEOUT_SECONDS}s ($RPC_URL)" >&2
  exit 1
fi

echo "[5/8] sending autonomous goal to main session"
TURN_OUT="$OUT_DIR/turn-start.txt"
MESSAGE_ELAPSED=0
while true; do
  if "$HOLON_BIN" message send \
    --rpc "$RPC_URL" \
    --timeout "${MESSAGE_SEND_TIMEOUT_SECONDS}s" \
    --thread main \
    --message "$(cat "$PROMPT_FILE")" \
    >"$TURN_OUT" 2>&1; then
    break
  fi
  if ! kill -0 "$SERVE_PID" 2>/dev/null; then
    echo "error: serve exited while sending first message; see $SERVE_LOG" >&2
    cat "$TURN_OUT" >&2
    exit 1
  fi
  MESSAGE_ELAPSED=$((MESSAGE_ELAPSED + MESSAGE_SEND_RETRY_SECONDS))
  if (( MESSAGE_ELAPSED >= MESSAGE_SEND_TIMEOUT_SECONDS )); then
    echo "error: failed to send first message within ${MESSAGE_SEND_TIMEOUT_SECONDS}s" >&2
    cat "$TURN_OUT" >&2
    exit 1
  fi
  sleep "$MESSAGE_SEND_RETRY_SECONDS"
done

if ! grep -q "turn started:" "$TURN_OUT"; then
  echo "error: failed to start turn" >&2
  cat "$TURN_OUT" >&2
  exit 1
fi
TURN_ID="$(sed -n 's/^turn started: //p' "$TURN_OUT" | head -n1 | tr -d '\r')"
TURN_ID="$(echo "$TURN_ID" | sed -n 's/.*turn_id=\([^ ]*\).*/\1/p')"
echo "TURN_ID=$TURN_ID" >>"$RUN_META"

ACTOR_LOGIN="$(gh api user -q '.login')"
echo "ACTOR_LOGIN=$ACTOR_LOGIN" >>"$RUN_META"

echo "[6/8] polling for planning issues"
ISSUE_JSON="$OUT_DIR/issues.json"
PARENT_COUNT=0
CHILD_COUNT=0
ELAPSED=0
while (( ELAPSED <= WAIT_PLAN_SECONDS )); do
  if ! kill -0 "$SERVE_PID" 2>/dev/null; then
    echo "error: serve exited while waiting for planning issues; see $SERVE_LOG" >&2
    exit 1
  fi
  gh issue list --repo "$REPO" --state all --limit 100 --search "$PREFIX in:title" --json number,title,url,state,author,labels,createdAt,updatedAt >"$ISSUE_JSON"
  PARENT_COUNT="$(jq '[.[] | select(.title | startswith("'"$PREFIX"' Parent"))] | length' "$ISSUE_JSON")"
  CHILD_COUNT="$(jq '[.[] | select(.title | startswith("'"$PREFIX"' Child"))] | length' "$ISSUE_JSON")"
  if (( PARENT_COUNT >= 1 && CHILD_COUNT >= 2 )); then
    break
  fi
  sleep "$POLL_INTERVAL_SECONDS"
  ELAPSED=$((ELAPSED + POLL_INTERVAL_SECONDS))
done

echo "PARENT_COUNT=$PARENT_COUNT" >>"$RUN_META"
echo "CHILD_COUNT=$CHILD_COUNT" >>"$RUN_META"

if (( PARENT_COUNT < 1 || CHILD_COUNT < 2 )); then
  echo "error: expected parent>=1 and child>=2 issues; got parent=$PARENT_COUNT child=$CHILD_COUNT" >&2
  exit 1
fi

PARENT_NUMBER="$(jq -r '[.[] | select(.title | startswith("'"$PREFIX"' Parent"))] | sort_by(.number) | last | .number // empty' "$ISSUE_JSON")"
echo "PARENT_NUMBER=$PARENT_NUMBER" >>"$RUN_META"

echo "[7/8] checking @holonbot trigger comment"
TRIGGER_FOUND=0
TRIGGER_CHILD=""
for n in $(jq -r '.[] | select(.title | startswith("'"$PREFIX"' Child")) | .number' "$ISSUE_JSON"); do
  COMMENT_JSON="$OUT_DIR/comments-$n.json"
  gh issue view --repo "$REPO" "$n" --comments --json comments >"$COMMENT_JSON"
  if jq -e '.comments[]? | select(.author.login == "'"$ACTOR_LOGIN"'" and (.body | contains("@holonbot")))' "$COMMENT_JSON" >/dev/null; then
    TRIGGER_FOUND=1
    TRIGGER_CHILD="$n"
    break
  fi
done

echo "TRIGGER_FOUND=$TRIGGER_FOUND" >>"$RUN_META"
echo "TRIGGER_CHILD=$TRIGGER_CHILD" >>"$RUN_META"
if (( TRIGGER_FOUND != 1 )); then
  echo "error: no @holonbot trigger comment from $ACTOR_LOGIN found on child issues" >&2
  exit 1
fi

echo "[8/8] polling for related PR creation"
PR_JSON="$OUT_DIR/pulls.json"
PR_COUNT=0
ELAPSED=0
while (( ELAPSED <= WAIT_PR_SECONDS )); do
  if ! kill -0 "$SERVE_PID" 2>/dev/null; then
    echo "error: serve exited while waiting for PR creation; see $SERVE_LOG" >&2
    exit 1
  fi
  gh pr list --repo "$REPO" --state all --limit 100 --search "$PREFIX in:title" --json number,title,url,state,isDraft,createdAt,updatedAt,mergedAt >"$PR_JSON"
  PR_COUNT="$(jq 'length' "$PR_JSON")"
  if (( PR_COUNT >= 1 )); then
    break
  fi
  sleep "$POLL_INTERVAL_SECONDS"
  ELAPSED=$((ELAPSED + POLL_INTERVAL_SECONDS))
done

echo "PR_COUNT=$PR_COUNT" >>"$RUN_META"
if (( PR_COUNT < 1 )); then
  echo "error: no PR created with prefix $PREFIX" >&2
  exit 1
fi

MERGED_COUNT="$(jq '[.[] | select(.mergedAt != null)] | length' "$PR_JSON")"
echo "MERGED_COUNT_INITIAL=$MERGED_COUNT" >>"$RUN_META"

if (( MERGED_COUNT < 1 )); then
  echo "[8/8] waiting for merge (best effort)"
  ELAPSED=0
  while (( ELAPSED <= WAIT_MERGE_SECONDS )); do
    gh pr list --repo "$REPO" --state all --limit 100 --search "$PREFIX in:title" --json number,title,url,state,isDraft,createdAt,updatedAt,mergedAt >"$PR_JSON"
    MERGED_COUNT="$(jq '[.[] | select(.mergedAt != null)] | length' "$PR_JSON")"
    if (( MERGED_COUNT >= 1 )); then
      break
    fi
    sleep "$POLL_INTERVAL_SECONDS"
    ELAPSED=$((ELAPSED + POLL_INTERVAL_SECONDS))
  done
fi

echo "MERGED_COUNT_FINAL=$MERGED_COUNT" >>"$RUN_META"

echo "run completed"
cat <<EOT
Run succeeded.
- repo: $REPO
- prefix: $PREFIX
- parent issue: https://github.com/$REPO/issues/$PARENT_NUMBER
- pr count: $PR_COUNT
- merged count: $MERGED_COUNT
- out dir: $OUT_DIR

Collect evidence:
  ./e2e-manual/serve-autonomous-project-drive/collect.sh --run-dir "$OUT_DIR" --out "$OUT_DIR/collected"

Background serve process:
  kill $SERVE_PID
EOT
