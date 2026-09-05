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

- `GITHUB_OUTPUT_DIR`: optional caller-provided output artifacts directory.
- `GITHUB_CONTEXT_DIR`: context directory (default `${GITHUB_OUTPUT_DIR}/github-context`).
- Do not create fixed output files unless the caller or an integration explicitly
  requires them.

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
- Follow the developer agent's recorded worktree, branch, and PR preferences;
  ask the operator when a relevant preference is not recorded.
- Run relevant verification commands before publish.

### 3. Commit and push

- Commit only intentional changes.
- Push branch to remote before PR publish.

### 4. Publish PR

Use raw `gh` commands. Pass the PR body directly, or use a caller-requested
body file when the integration needs one:

```bash
gh pr create --repo <owner/repo> --title "<title>" --body "<body>" --head <branch> --base <base>
gh pr edit <pr_number> --repo <owner/repo> --title "<title>" --body "<body>"
```

Publish completion is mandatory; do not report success without a real PR side effect.

### 5. Finalize delivery

Report the result in the normal agent delivery. If a caller explicitly requires
machine-readable output, write only the requested artifacts under
`${GITHUB_OUTPUT_DIR}` and include their paths in the delivery.

## Delivery Standards

- Keep scope aligned with issue intent; avoid unrelated refactors.
- State assumptions explicitly when requirements are ambiguous.
- Include concrete verification results (commands + outcomes).
- If full verification is impossible, report what was attempted and why it is incomplete.

## Failure Rules

Mark run as failed if any of the following is true:
- no meaningful code change was produced for the issue intent
- commit/push was not completed
- PR create/update failed or PR URL cannot be verified

Do not report success from artifacts alone. A summary or manifest is optional
evidence, not a substitute for the actual commit, push, or PR side effect.
