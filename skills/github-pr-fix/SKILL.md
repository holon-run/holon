---
name: github-pr-fix
description: "Fix a GitHub pull request by addressing feedback or CI failures, pushing changes, and publishing replies."
---

# GitHub PR Fix Skill

## Summary

Use this skill when you need to remediate an existing pull request: diagnose what is broken, apply targeted fixes, push commits, and reply on review threads.

## When To Use

- Fixing CI failures on an existing PR
- Addressing requested changes or review feedback
- Updating the PR branch and publishing review replies

## Do Not Use

- Opening a brand-new PR from an issue
- Performing a review-only pass without code changes
- Project-wide triage or roadmap planning

## Prerequisites

- `gh` CLI authentication is required.
- `GITHUB_TOKEN`/`GH_TOKEN` must allow PR read-write operations.

## Runtime Paths

- `GITHUB_OUTPUT_DIR`: output artifacts directory (caller-provided preferred; otherwise temp dir).
- `GITHUB_CONTEXT_DIR`: context directory (default `${GITHUB_OUTPUT_DIR}/github-context`).

## Inputs (Manifest-First)

Preferred input when already available:
- `${GITHUB_CONTEXT_DIR}/manifest.json`

Optional inputs:
- Any artifact listed as `status=present` in `manifest.artifacts[]`.

If no manifest is provided, collect PR context directly with `gh`:

```bash
gh pr view <pr_number> --repo <owner/repo> --json number,title,body,state,url,baseRefName,headRefName,headRefOid,author,createdAt,updatedAt,mergeable,reviews,changedFiles,additions,deletions
gh pr view <pr_number> --repo <owner/repo> --json files
gh api repos/<owner>/<repo>/issues/<pr_number>/comments --paginate
gh api graphql -f query='
  query($owner:String!, $repo:String!, $number:Int!) {
    repository(owner:$owner, name:$repo) {
      pullRequest(number:$number) {
        reviewThreads(first:100) {
          nodes {
            isResolved
            comments(first:100) {
              nodes { id body path line author { login } }
            }
          }
        }
      }
    }
  }' -F owner=<owner> -F repo=<repo> -F number=<pr_number>
```

Do not assume fixed `github/*.json` files.
Resolve available context through artifact metadata (`id`, `path`, `status`, `description`).

## Workflow

### 1. Collect context

- If `${GITHUB_CONTEXT_DIR}/manifest.json` exists, use it.
- Otherwise, collect PR metadata, files, comments, and review threads directly with `gh`.

### 2. Diagnose and prioritize

Prioritize in this order:
1. Build/compile failures
2. Failing tests and runtime regressions
3. Type/import/module errors
4. Lint/style issues
5. Non-blocking refactor suggestions

Use existing review threads/comments to avoid duplicate or stale responses.

### 3. Implement fixes

- Apply minimal targeted fixes for blocking issues first.
- Run relevant verification commands.
- Commit and push before posting review replies.

### 4. Publish review replies

Use direct `gh api` reply operations:

```bash
gh api repos/<owner>/<repo>/pulls/<pr_number>/comments \
  -X POST \
  -F in_reply_to=<comment_id> \
  -f body='Thanks, fixed in the latest commit.'
```

### 5. Finalize outputs

Required outputs under `${GITHUB_OUTPUT_DIR}`:
- `summary.md`
- `manifest.json`

## Remediation Standards

- Do not mark issues fixed without verification evidence.
- If verification is partial, state exact limits and risk.
- Defer non-blocking large refactors with explicit rationale.
- Keep review replies concrete: what changed, where, and any remaining risk.

## Output Contract

### `summary.md`

Must include:
- PR reference and diagnosis summary
- fixes applied
- verification commands and outcomes
- reply publish result summary
- deferred/follow-up items

### `manifest.json`

Execution metadata for this skill, including:
- `provider: "github-pr-fix"`
- PR reference
- fix/reply counters
- `status` (`completed|partial|failed`)

## Completion Criteria

A successful run requires all of the following:
1. Blocking fixes are committed and pushed to the PR branch.
2. Replies planned for this run are published successfully.
3. `summary.md` records what changed, which replies were posted, and any remaining risks.

If replies are planned but not published, the run is not successful.
