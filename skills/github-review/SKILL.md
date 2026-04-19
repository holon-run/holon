---
name: github-review
description: "Review a GitHub pull request by collecting PR context, analyzing risks, and publishing one structured review."
---

# GitHub Review Skill

## Summary

Use this skill when you need to review a pull request, identify the highest-signal findings, and publish one structured GitHub review.

## When To Use

- Reviewing an open pull request for correctness, regressions, or safety issues
- Publishing a review summary plus optional inline comments
- Working directly from raw GitHub CLI and API data

## Do Not Use

- Implementing fixes on the PR branch
- Opening a new PR from an issue
- Project-wide prioritization or PM analysis

## Prerequisites

- `gh` CLI authentication is required.
- `GITHUB_TOKEN`/`GH_TOKEN` needs permissions to read PR data and publish reviews/comments.

## Runtime Paths

- `GITHUB_OUTPUT_DIR`: output artifacts directory (caller-provided preferred; otherwise temp dir).
- `GITHUB_CONTEXT_DIR`: context directory (default `${GITHUB_OUTPUT_DIR}/github-context`).

## Inputs (Manifest-First)

Preferred input when already available:
- `${GITHUB_CONTEXT_DIR}/manifest.json`

Optional inputs:
- Any context artifact listed as `status=present` in `manifest.json`.

If no manifest is provided, collect PR context directly with `gh`:

```bash
gh pr view <pr_number> --repo <owner/repo> --json number,title,body,state,url,baseRefName,headRefName,headRefOid,author,createdAt,updatedAt,mergeable,reviews,changedFiles,additions,deletions
gh pr view <pr_number> --repo <owner/repo> --json files
gh pr diff <pr_number> --repo <owner/repo>
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

This skill must not assume fixed context filenames.
Use `manifest.artifacts[]` (`id`, `path`, `status`, `description`) to determine available context.

## Workflow

### 1. Collect context

- If `${GITHUB_CONTEXT_DIR}/manifest.json` exists, use it.
- Otherwise, collect PR metadata, files, diff, comments, and review-thread context directly with `gh`.

### 2. Perform review

Generate:
- `${GITHUB_OUTPUT_DIR}/review.md`
- `${GITHUB_OUTPUT_DIR}/review.json`
- `${GITHUB_OUTPUT_DIR}/summary.md`
- Optional `${GITHUB_OUTPUT_DIR}/manifest.json` (execution metadata)

### 3. Publish review

Use `gh api` with a JSON payload file:

```bash
gh api repos/<owner>/<repo>/pulls/<pr_number>/reviews -X POST --input <review-payload.json>
```

## Review Standards

### Scope and priority

Review focus order:
1. Correctness bugs
2. Security/safety issues
3. Performance/scalability risks
4. API compatibility and error handling
5. High-impact maintainability issues

### Incremental-first

- Prioritize newly introduced changes (new commits and new diff hunks).
- Expand scope only when needed to validate correctness or safety.

### Historical deduplication

- Check existing review threads/comments before raising findings.
- Do not repeat already-raised issues unless there is new evidence or changed impact.
- If re-raising, explain the delta briefly.

### Keep signal high

- Avoid low-value style nitpicks unless they affect behavior/maintainability.
- Keep feedback concise, specific, and actionable.
- Prefer fewer high-impact findings over exhaustive noise.

## Output Contract

### `review.md`

Human-readable review summary containing:
- conclusion-first summary
- key findings ordered by severity
- actionable recommendations

### `review.json`

Structured inline findings:

```json
[
  {
    "path": "path/to/file.go",
    "line": 42,
    "severity": "error|warn|nit",
    "message": "Issue description",
    "suggestion": "Optional concrete fix"
  }
]
```

Severity semantics:
- `error`: must-fix before merge
- `warn`: should-fix
- `nit`: optional improvement

### `summary.md`

Short execution summary:
- reviewed ref/head
- context coverage summary from manifest
- number of findings and publish outcome
- explicit degradation/failure reason when context is insufficient

## Degradation Rules

- If core review artifacts are missing (for example `pr_metadata`, `diff` and `files` both unavailable), do not fabricate certainty.
- Either:
  - produce summary-only review with explicit limitations and no inline comments, or
  - fail with clear reason in `summary.md`.

## Publishing Guardrails

- Publish at most one review per execution round.
- A successful primary publish is terminal; do not run alternate publish paths.
- Before fallback publish, check whether an equivalent Holon review already exists for the same head SHA and skip if present.

## Configuration

- `DRY_RUN=true`: preview only.
- `MAX_INLINE=N`: cap inline comments.
- `POST_EMPTY=true`: allow posting empty review.
