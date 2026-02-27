---
name: github-issue-solve
description: "Solve GitHub issues by implementing changes and creating or updating a pull request."
---

# GitHub Issue Solve Skill

`github-issue-solve` focuses on delivery: understand issue intent, implement code changes, and publish a PR.

## Prerequisites

- `gh` CLI authentication is required.
- Prefer `ghx` for context collection and PR publishing commands.
- `GITHUB_TOKEN`/`GH_TOKEN` must allow issue/PR read-write operations.

## Runtime Paths

- `GITHUB_OUTPUT_DIR`: output artifacts directory (caller-provided preferred; otherwise temp dir).
- `GITHUB_CONTEXT_DIR`: context directory (default `${GITHUB_OUTPUT_DIR}/github-context`).

## Inputs (Manifest-First)

Required input:
- `${GITHUB_CONTEXT_DIR}/manifest.json` produced by `ghx context collect`.

Optional inputs:
- Any artifact listed as `status=present` in `manifest.artifacts[]`.

Do not assume fixed file names under `github/`.  
Resolve usable inputs from `manifest.artifacts[]` by `id`/`path`/`status`/`description`.

## Workflow

### 1. Collect context

Preferred:
- `skills/ghx/scripts/ghx.sh context collect <issue_ref>`

Fallback:
- Direct `gh` collection only if `ghx` is unavailable; still produce equivalent manifest contract.

### 2. Analyze and implement

- Extract acceptance criteria and constraints from issue metadata and discussion.
- Implement minimal complete changes for the requested outcome.
- Use deterministic branch naming (`feature/issue-<number>` or `fix/issue-<number>`).
- Run relevant verification commands before publish.

### 3. Commit and push

- Commit only intentional changes.
- Push branch to remote before PR publish.

### 4. Publish PR

Preferred:
- Use `ghx` PR capability commands (`pr create` / `pr update`) with `--body-file`.

Fallback:
- Use `gh pr create` / `gh pr edit` directly if `ghx` publish path is unavailable.

Publish completion is mandatory; do not report success without a real PR side effect.

### 5. Finalize outputs

Required outputs under `${GITHUB_OUTPUT_DIR}`:
- `summary.md`
- `manifest.json`

Optional:
- `publish-results.json` (when publish executed via `ghx`)

## Delivery Standards

- Keep scope aligned with issue intent; avoid unrelated refactors.
- State assumptions explicitly when requirements are ambiguous.
- Include concrete verification results (commands + outcomes).
- If full verification is impossible, report what was attempted and why it is incomplete.

## Output Contract

### `summary.md`

Must include:
- issue reference and interpreted requirements
- key code changes
- verification performed and outcomes
- PR publish result (`pr_number`, `pr_url`, branch)
- explicit blockers or follow-ups (if any)

### `manifest.json`

Execution metadata for this skill, including:
- `provider: "github-issue-solve"`
- issue reference
- branch
- publish result fields (`pr_number`, `pr_url`)
- `status` (`completed|failed`)

## Failure Rules

Mark run as failed if any of the following is true:
- no meaningful code change was produced for the issue intent
- commit/push was not completed
- PR create/update failed or PR URL cannot be verified

Do not report success from artifacts alone.

## Notes

- This skill defines issue-solving behavior and completion criteria.
- `ghx` defines context artifact semantics via `manifest.json`.
