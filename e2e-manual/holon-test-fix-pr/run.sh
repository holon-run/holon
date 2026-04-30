#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-holon-run/holon-test}"
WORKFLOW="${WORKFLOW:-holon-solve-smoke.yml}"
MODEL="${MODEL:-anthropic/claude-sonnet-4-6}"
RUN_ID="${RUN_ID:-$(date +%Y%m%d-%H%M%S)}"
LOCAL_REPO="${LOCAL_REPO:-/tmp/holon-fix-e2e-$RUN_ID}"
BRANCH="${BRANCH:-e2e/fix-$RUN_ID}"

gh repo clone "$REPO" "$LOCAL_REPO"
(
  cd "$LOCAL_REPO"
  git config user.name "${GIT_USER_NAME:-holon-e2e}"
  git config user.email "${GIT_USER_EMAIL:-holon-e2e@example.com}"
  git checkout -b "$BRANCH"
  mkdir -p docs
  cat >"docs/e2e-fix-$RUN_ID.md" <<EOT
# E2E Fix Fixture

This sentence contains a TODO marker and should be rewritten clearly.
EOT
  git add "docs/e2e-fix-$RUN_ID.md"
  git commit -m "test: add fix fixture $RUN_ID"
  git push -u origin "$BRANCH"
)

body_file="$(mktemp)"
cat >"$body_file" <<EOT
E2E fix fixture for Holon.

Run id: $RUN_ID
EOT
pr_url="$(gh pr create --repo "$REPO" --head "$BRANCH" --base main --title "E2E fix fixture $RUN_ID" --body-file "$body_file")"
rm -f "$body_file"

pr_number="${pr_url##*/}"
head_before="$(gh pr view "$pr_number" --repo "$REPO" --json headRefOid --jq .headRefOid)"
goal='Fix the target pull request by removing the TODO marker wording from the fixture markdown file, rewriting the sentence clearly, committing the change, and pushing it to the PR branch. Do not open another PR.'

gh workflow run "$WORKFLOW" \
  --repo "$REPO" \
  -f ref="$pr_url" \
  -f goal="$goal" \
  -f model="$MODEL"

sleep 5
run_json="$(gh run list --repo "$REPO" --workflow "$WORKFLOW" --limit 1 --json databaseId,url,status,conclusion,createdAt)"
run_url="$(printf '%s' "$run_json" | jq -r '.[0].url')"

cat <<EOT
Triggered PR fix e2e.
- pr: $pr_url
- run: $run_url
- head before: $head_before
- local repo: $LOCAL_REPO

Watch:
  gh run watch --repo "$REPO" "$(printf '%s' "$run_json" | jq -r '.[0].databaseId')" --interval 10
EOT
