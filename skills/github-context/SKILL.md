---
name: github-context
description: "Shared GitHub context collection skill used by other skills. Collects issue/PR metadata, comments, files, diffs, review threads, commits, and check runs into a standard layout under ${GITHUB_CONTEXT_DIR}/github/."
---

# GitHub Context Skill

Reusable GitHub collector used by `github-solve`, `github-review`, and other skills. Produces a consistent context bundle for issues and pull requests.

## Environment and Paths

- **`GITHUB_CONTEXT_DIR`**: Output directory for collected context  
  - Default: `/holon/output/github-context` if the path exists; otherwise a temp dir `/tmp/holon-ghctx-*`
- **`MANIFEST_PROVIDER`** / **`COLLECT_PROVIDER`**: Provider name written to `manifest.json` (default: `github-context`)
- **`TRIGGER_COMMENT_ID`**: Comment ID to flag with `is_trigger` in comments/review threads
- **`INCLUDE_DIFF`** (default: `true`): Fetch PR diff
- **`INCLUDE_CHECKS`** (default: `true`): Fetch check runs and workflow logs
- **`INCLUDE_THREADS`** (default: `true`): Fetch review comment threads
- **`INCLUDE_FILES`** (default: `true`): Fetch changed files list
- **`INCLUDE_COMMITS`** (default: `true`): Fetch commits for the PR
- **`MAX_FILES`** (default: `200`): Limit for files collected when `INCLUDE_FILES=true`

## Usage

Use the skill runner (Holon or host) to invoke `scripts/collect.sh` with the reference; examples assume the script is on PATH or referenced relative to the skill directory:

```bash
collect.sh "holon-run/holon#123"
collect.sh 123 holon-run/holon
INCLUDE_CHECKS=false MAX_FILES=50 collect.sh https://github.com/holon-run/holon/pull/123
```

## Output Contract

Artifacts are written to `${GITHUB_CONTEXT_DIR}/github/`:

- `issue.json` (issues) or `pr.json` (PRs)
- `comments.json`
- `review_threads.json` (when `INCLUDE_THREADS=true` and PR)
- `files.json` (when `INCLUDE_FILES=true` and PR)
- `pr.diff` (when `INCLUDE_DIFF=true` and PR)
- `commits.json` (when `INCLUDE_COMMITS=true` and PR)
- `check_runs.json` and `test-failure-logs.txt` (when `INCLUDE_CHECKS=true` and PR)
- `manifest.json` (written at `${GITHUB_CONTEXT_DIR}/manifest.json`)

## Integration Notes

- Wrapper skills (`github-solve`, `github-review`) set their own defaults and `MANIFEST_PROVIDER` before delegating to this collector.
- The helper library lives at `scripts/lib/helpers.sh` and provides parsing, dependency checks, fetching helpers, and manifest writing. Source it instead of copying.
