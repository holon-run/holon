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
- Runtime-provided repository or path-specific review instructions.

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
When the runtime provides repository or path-specific review instructions,
treat them as authoritative project context for this review. Do not discover,
parse, or match path-specific instruction files inside this skill; that context
assembly belongs to the caller/runtime.

## Workflow

### 1. Collect context

- If `${GITHUB_CONTEXT_DIR}/manifest.json` exists, use it.
- Otherwise, collect PR metadata, files, diff, comments, and review-thread context directly with `gh`.
- Record which runtime-provided repository/path instructions are available.
  If the runtime exposes instruction names, paths, or match metadata, preserve
  that metadata in the execution summary.

### 2. Perform review

Generate:
- `${GITHUB_OUTPUT_DIR}/review.md`
- `${GITHUB_OUTPUT_DIR}/review.json`
- `${GITHUB_OUTPUT_DIR}/summary.md`
- `${GITHUB_OUTPUT_DIR}/review-publish.json` after a successful publish, or when
  publishing is skipped because an equivalent same-head review/comment already
  exists
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

### Runtime-provided project context

- Apply any runtime-provided repository or path-specific review instructions to
  matching files as binding project context.
- If runtime-provided instructions conflict with generic review heuristics,
  follow the repository/path instruction unless it would hide a correctness,
  security, or safety issue.
- If instruction metadata is unavailable, do not infer that no project-specific
  instructions exist; state the coverage limitation in `summary.md`.

### Line-level risk scan

Review each changed file and materially changed hunk before deciding there are
no findings. For every relevant hunk, explicitly consider:
- control-flow and lifecycle regressions, including missed wake/sleep,
  retry, cancellation, cleanup, or state-transition paths
- trust-boundary or provenance mistakes, including implicit trust elevation,
  mixed operator/external input, missing authorization checks, or secret leaks
- error handling gaps, including swallowed errors, ambiguous retries,
  unchecked fallbacks, panics, `unwrap`/`expect`, or misleading success states
- data-shape and compatibility changes, including schema drift, missing
  migration behavior, broken serialization, or public contract changes
- concurrency and ordering risks, including races, stale reads, duplicate
  side effects, non-idempotent publishes, or hidden background work
- resource and performance risks, including unbounded loops, large context
  expansion, unnecessary network calls, or avoidable memory growth
- test and observability gaps that would let a high-impact regression pass
  silently

Use this scan to find concrete issues, not to produce checklist noise. Only
publish findings that are supported by the diff or directly relevant surrounding
code.

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
- repository/path instruction coverage, including instruction metadata when
  provided by the runtime
- number of findings and publish outcome
- explicit degradation/failure reason when context is insufficient

## Degradation Rules

- If core review artifacts are missing (for example `pr_metadata`, `diff` and `files` both unavailable), do not fabricate certainty.
- If runtime-provided instruction metadata is expected but unavailable, continue
  the review using the available PR context, but record that limitation in
  `summary.md`.
- Either:
  - produce summary-only review with explicit limitations and no inline comments, or
  - fail with clear reason in `summary.md`.

## Publishing Guardrails

- Publish at most one review per execution round.
- Treat review/comment publishing as a single-shot external side effect: choose
  one publish surface, either one PR review or one PR comment, not both.
- Capture the target PR `headRefOid` before publishing and use it as the
  deduplication key.
- Before any publish or retry, check existing reviews/comments by Holon or the
  current GitHub actor for the same head SHA. If an equivalent review/comment
  already exists, skip publishing and record the existing URL/status.
- A successful primary publish is terminal; immediately write the publish result
  to `${GITHUB_OUTPUT_DIR}/review-publish.json` and do not run alternate publish
  paths.
- Before fallback publish, require both conditions: no local
  `review-publish.json` success marker exists, and no equivalent Holon review or
  comment exists for the same head SHA.
- If a publish command result is ambiguous, verify existing reviews/comments for
  the same head SHA before retrying. Do not retry blindly.

## Configuration

- `DRY_RUN=true`: preview only.
- `MAX_INLINE=N`: cap inline comments.
- `POST_EMPTY=true`: allow posting empty review.
