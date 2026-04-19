---
name: github-issue-solve
description: "Solve a GitHub issue by collecting context, implementing a fix, and opening or updating a pull request."
---

# GitHub Issue Solve Skill

## Summary

Use this skill when you need to turn a GitHub issue into a concrete code change and publish the result as a pull request.

## When To Use

- Fixing a GitHub issue end-to-end
- Collecting issue context and comments with raw `gh` commands
- Implementing code changes and opening or updating a PR

## Do Not Use

- Reviewing an existing PR without making changes
- Replying to review feedback on an existing PR
- Project-wide planning or backlog triage

## Prerequisites

- `gh` CLI authentication is required.
- `GITHUB_TOKEN`/`GH_TOKEN` must allow issue/PR read-write operations.

## Runtime Paths

- `GITHUB_OUTPUT_DIR`: output artifacts directory (caller-provided preferred; otherwise temp dir).
- `GITHUB_CONTEXT_DIR`: context directory (default `${GITHUB_OUTPUT_DIR}/github-context`).

## Inputs (Manifest-First)

Preferred input when already available:
- `${GITHUB_CONTEXT_DIR}/manifest.json`

Optional inputs:
- Any artifact listed as `status=present` in `manifest.artifacts[]`.

If no manifest is provided, collect issue metadata and comments directly with `gh`:

```bash
gh issue view <issue_number> --repo <owner/repo> --json number,title,body,state,url,author,createdAt,updatedAt,labels
gh api repos/<owner>/<repo>/issues/<issue_number>/comments --paginate
```

Do not assume fixed file names under `github/`.
Resolve usable inputs from `manifest.artifacts[]` by `id`/`path`/`status`/`description`.

## Workflow

### 1. Collect context

- If `${GITHUB_CONTEXT_DIR}/manifest.json` exists, use it.
- Otherwise, collect the issue body and comments directly with `gh`.

### 2. Analyze and implement

- Extract acceptance criteria and constraints from issue metadata and discussion.
- Implement minimal complete changes for the requested outcome.
- Use deterministic branch naming (`feature/issue-<number>` or `fix/issue-<number>`).
- Run relevant verification commands before publish.

### 3. Commit and push

- Commit only intentional changes.
- Push branch to remote before PR publish.

### 4. Publish PR

Use raw `gh` commands with `--body-file`:

```bash
gh pr create --repo <owner/repo> --title "<title>" --body-file <summary.md> --head <branch> --base <base>
gh pr edit <pr_number> --repo <owner/repo> --title "<title>" --body-file <summary.md>
```

Publish completion is mandatory; do not report success without a real PR side effect.

### 5. Finalize outputs

Required outputs under `${GITHUB_OUTPUT_DIR}`:
- `summary.md`
- `manifest.json`

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
