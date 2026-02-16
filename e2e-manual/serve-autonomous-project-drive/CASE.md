# serve-autonomous-project-drive

## Purpose

Validate a full autonomous project-driving loop via `holon serve` + `holon message send`:

- Agent receives a high-level project objective.
- Agent creates a parent issue and child issues in GitHub.
- Agent triggers execution via `@holonbot` on at least one child issue.
- Agent tracks execution and drives toward PR creation/merge.

This case focuses on real-world operational behavior instead of unit-level protocol checks.

## Preconditions

- `gh auth status` succeeds and has write access to target repo.
- `bin/holon` exists (`make build` if missing).
- Docker daemon is running.
- `~/.claude/settings.json` contains:
  - `.env.ANTHROPIC_AUTH_TOKEN`
  - `.env.ANTHROPIC_BASE_URL`
- Target repo has `holon-trigger`/`holonbot` workflow enabled (default: `holon-run/holon-test`).

## Steps

1. Build local agent bundle and start `holon serve` with a temporary `agent_home`.
2. Send one high-level instruction to `main` thread using `holon message send`.
3. Poll GitHub for:
   - parent issue creation
   - child issue creation
   - `@holonbot` trigger comment on a child issue
4. Continue polling for PR creation and merge status.
5. Collect run logs and GitHub evidence.

Use `run.sh` for the scripted flow.

## Expected

- A unique `[AUTONOMY-E2E-<id>]` parent issue is created.
- At least 2 child issues are created and linked.
- At least one child issue contains a `@holonbot` trigger comment.
- At least one PR referencing the run prefix is created.
- Stretch goal: one such PR is merged.

## Pass / Fail Criteria

- Pass:
  - parent issue exists
  - >= 2 child issues exist
  - `@holonbot` trigger comment exists
  - PR created (merge preferred, but optional for infra pass)
- Fail (`infra-fail`):
  - `serve` cannot stay up / RPC unreachable
  - message send fails to create turn
  - GitHub write operations cannot proceed due auth/pipeline outage
- Fail (`agent-fail`):
  - infra works but agent does not complete planning/decomposition/trigger behavior

## Evidence to Capture

- `serve.log`
- `run-meta.env`
- `prompt.md`
- issue/PR snapshot files from GitHub polling
- `agent_home` key files (`agent.yaml`, `ROLE.md`, `state/subscription-status.json` if present)

Use `collect.sh` to gather artifacts.

## Known Failure Modes

- Agent performs planning but does not actually publish issues/PRs.
- `@holonbot` trigger comment exists but workflow queue is delayed.
- Workflow run fails for unrelated repo CI constraints.
