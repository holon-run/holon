# Script Reference

This document describes the runner-facing wrapper scripts for github-review. Agents should not invoke these directly; runners may use them or call `ghx` for posting.

## collect.sh - Context Collection Script

### Purpose

Fetches all necessary PR context for code review.

### Usage

```bash
collect.sh <pr_ref> [repo_hint]
```

### Parameters

- `pr_ref` (required): PR reference in any format:
  - Numeric: `123`
  - Short form: `owner/repo#123`
  - Full URL: `https://github.com/owner/repo/pull/123`
- `repo_hint` (optional): Repository hint for ambiguous numeric refs

### What It Collects

`collect.sh` delegates to `ghx` collection and writes a manifest-driven context contract:

1. `manifest.json` (schema `2.0`) with:
   - normalized ref (`owner/repo#number`)
   - collection success status
   - `artifacts[]` entries (`id`, `path`, `status`, `description`, `required_for`)
   - diagnostic `notes[]`
2. Context files under `github/` referenced by `manifest.artifacts[]` (for example `pr.json`, `files.json`, `pr.diff`, `review_threads.json`, `comments.json`, `commits.json`, `check_runs.json`)

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_OUTPUT_DIR` | Caller-provided output dir; else `/tmp/holon-ghreview-*` | Output directory for artifacts |
| `GITHUB_CONTEXT_DIR` | `${GITHUB_OUTPUT_DIR}/github-context` | Context subdirectory |
| `MAX_FILES` | `100` | Maximum files to fetch (prevents overwhelming context) |
| `INCLUDE_THREADS` | `true` | Include existing review threads |
| `INCLUDE_DIFF` | `true` | Include `pr.diff` |
| `INCLUDE_FILES` | `true` | Include `files.json` |
| `INCLUDE_COMMITS` | `true` | Include `commits.json` |
| `INCLUDE_CHECKS` | `false` | Include `check_runs.json` + logs when true |

### Output Files

All files are written to `GITHUB_CONTEXT_DIR` (default: `GITHUB_OUTPUT_DIR/github-context/`):

- `manifest.json` - canonical artifact contract for consumers
- `github/*` - artifact files listed in `manifest.artifacts[]`

### Requirements

- `gh` CLI must be installed and authenticated
- `jq` must be installed for JSON processing
- `GITHUB_TOKEN` or `GH_TOKEN` must be set with appropriate scopes

### Examples

```bash
# Basic usage
collect.sh holon-run/holon#123

# With custom output directory
GITHUB_OUTPUT_DIR=./review collect.sh 123

# Limit files for large PRs
MAX_FILES=50 collect.sh "owner/repo#456"
```

---

## GHX Review Publish

### Purpose

Posts a single PR review with inline comments using GitHub API, based on agent-generated artifacts.

### Usage

```bash
# Preview without posting
DRY_RUN=true skills/ghx/scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json

# Publish with limits
MAX_INLINE=10 POST_EMPTY=false skills/ghx/scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json
```

### Options

- `--dry-run` or `DRY_RUN=true`: Preview review body and inline comments without posting
- `--max-inline=N` or `MAX_INLINE`: Limit inline comments (default 20)
- `--post-empty` or `POST_EMPTY=true`: Post even when `review.json` is empty
- `--pr=OWNER/REPO#NUMBER`: Target PR reference

### Required artifacts (in `${GITHUB_OUTPUT_DIR}`; defaults to temp when unset)
- `review.md`: Review summary/body
- `review.json`: Structured findings with `path`/`line`/`severity`/`message` (and optional `suggestion`)
- Optional `github-context/manifest.json`: collection metadata for audit/debug context

### Output
- Updates GitHub with one review (event=COMMENT) plus inline comments (up to `MAX_INLINE`).
- Writes `summary.md` describing what was posted.

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_OUTPUT_DIR` | Temp dir when unset | Directory containing review artifacts |
| `DRY_RUN` | `false` | Preview without posting |
| `MAX_INLINE` | `20` | Maximum inline comments to post |
| `POST_EMPTY` | `false` | Post review even with no findings |

### Input Files (Agent-Generated)

The publish command expects these artifacts in `GITHUB_OUTPUT_DIR`:

- `review.md` - Human-readable review summary
- `review.json` - Structured findings with inline comments:
  ```json
  [
    {
      "path": "src/file.ts",
      "line": 42,
      "severity": "error",
      "message": "Null pointer dereference",
      "suggestion": "Add null check"
    }
  ]
  ```
- `summary.md` - Brief process summary

### Publishing Behavior

1. **Creates PR review** via GitHub API
2. **Posts inline comments** for findings with path+line information
3. **Limits inline comments** via `MAX_INLINE` (most important findings first)
4. **Skips posting** if `POST_EMPTY=false` and no findings
5. **Dry-run mode** previews without posting

### Examples

```bash
# Preview review
DRY_RUN=true skills/ghx/scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json

# Limit inline comments
MAX_INLINE=10 skills/ghx/scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json

# Post even if no findings
POST_EMPTY=true skills/ghx/scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json

# Use batch mode for multi-action publish
skills/ghx/scripts/ghx.sh batch run --batch=publish-batch.json
```

---

## Workflow Integration

### CI/CD Integration

```yaml
name: PR Review
on:
  pull_request:
    types: [opened, synchronize]

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Collect context
        run: |
          GITHUB_OUTPUT_DIR=${PWD}/context \
          skills/github-review/scripts/collect.sh "${{ github.repository }}#${{ github.event.pull_request.number }}"

      - name: Run review
        uses: holon-run/holon@main
        with:
          skill: github-review
          args: "${{ github.repository }}#${{ github.event.pull_request.number }}"
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          MAX_INLINE: 20

      - name: Publish review
        run: |
          skills/ghx/scripts/ghx.sh review publish --pr="${{ github.repository }}#${{ github.event.pull_request.number }}" --body-file=review.md --comments-file=review.json
        env:
          GITHUB_OUTPUT_DIR: ${PWD}/context
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

### Manual Workflow

```bash
# 1. Collect
GITHUB_OUTPUT_DIR=./review collect.sh "owner/repo#123"

# 2. Agent performs review (reads ./review/github-context/)
#    Agent writes to ./review/review.md and review.json

# 3. Publish
skills/ghx/scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json
```

## Error Handling

Both scripts include error handling:

- Missing dependencies (`gh`, `jq`) → Clear error message
- Invalid PR reference → Usage help
- Authentication failure → Check token message
- Missing artifacts → Fails fast with clear error

Scripts exit with non-zero status on error for reliable CI integration.
