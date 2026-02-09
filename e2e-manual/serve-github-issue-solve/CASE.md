# serve-github-issue-solve

## Purpose

Validate `holon serve` end-to-end for GitHub issue solve trigger:

- GitHub webhook event is ingested by serve.
- Event is forwarded into controller runtime.
- Controller attempts issue-solve workflow from issue intent events without relying on `@holonbot` mention.

## Preconditions

- `gh` CLI is installed and authenticated (`gh auth status` succeeds).
- `bin/holon` exists (`make build` if missing).
- `~/.claude/settings.json` contains:
  - `.env.ANTHROPIC_AUTH_TOKEN`
  - `.env.ANTHROPIC_BASE_URL`
- Test repository is writable (default: `holon-run/holon-test`).
- Docker daemon is running.

## Steps

1. Start serve and webhook forwarder.
2. Create a test issue in target repo.
3. Post a plain follow-up comment (no bot mention).
4. Observe state files and runtime logs.

Use `run.sh` for the scripted flow.

## Expected

- `events.ndjson` receives `github.issue.opened` and `github.issue.comment.created` from user actions.
- `decisions.ndjson` and `actions.ndjson` are updated for ingested events.
- Controller runtime evidence log is generated.
- Optional success goal: issue-solve execution produces a PR.

## Pass / Fail Criteria

- Pass (infra):
  - webhook accepted
  - events are persisted
  - controller runtime starts and reads event channel
- Fail (`infra-fail`):
  - webhook cannot reach serve
  - no events written
  - controller runtime does not start
- Fail (`agent-fail`):
  - infra passes but no solve behavior (no meaningful decision/action toward PR)

## Evidence to Capture

- `events.ndjson`
- `decisions.ndjson`
- `actions.ndjson`
- `controller-runtime/output/evidence/execution.log`
- Trigger issue URL and comment URL

Use `collect.sh` to bundle evidence into `artifacts/`.

## Known Failure Modes

- Anthropic env missing: controller cannot operate model.
- `gh webhook forward` not running: no GitHub events reach local serve.
- Agent drifts to generic shell workflow instead of skill-guided solve.
- If `@holonbot` is used in comments, repo GitHub Actions may run in parallel and interfere with this local test.
