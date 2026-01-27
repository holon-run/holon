# Script Reference

This document provides detailed reference for the github-review skill scripts.

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

1. **PR Metadata** (`github/pr.json`)
   - Title, description, author
   - Addition, deletion, modification counts
   - Created at, updated at, state

2. **Changed Files** (`github/files.json`)
   - List of modified files with statistics
   - Configurable limit via `MAX_FILES`

3. **Full Diff** (`github/pr.diff`)
   - Complete diff of all changes
   - Used for code review analysis

4. **Review Threads** (`github/review_threads.json`)
   - Existing review comments
   - Used to avoid duplicating feedback

5. **Discussion Comments** (`github/comments.json`)
   - PR conversation history
   - Provides additional context

6. **Commit History** (`github/commits.json`)
   - Commit messages and metadata
   - Helps understand evolution of changes

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_OUTPUT_DIR` | `/holon/output` if present, else `/tmp/holon-ghreview-*` | Output directory for artifacts |
| `GITHUB_CONTEXT_DIR` | `${GITHUB_OUTPUT_DIR}/github-review-context` | Context subdirectory |
| `MAX_FILES` | `100` | Maximum files to fetch (prevents overwhelming context) |
| `INCLUDE_THREADS` | `true` | Include existing review threads |

### Output Files

All files written to `GITHUB_CONTEXT_DIR` (default: `GITHUB_OUTPUT_DIR/github-review-context/`):

- `github/pr.json` - PR metadata
- `github/files.json` - Changed files list
- `github/pr.diff` - Full diff
- `github/review_threads.json` - Existing reviews
- `github/comments.json` - PR comments
- `github/commits.json` - Commit history
- `manifest.json` - Collection metadata

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

## publish.sh - Review Publishing Script

### Purpose

Publishes structured PR reviews with inline comments via GitHub API.

### Usage

```bash
# Execute from intent file
publish.sh --intent=/holon/output/publish-intent.json

# Direct command mode
publish.sh create-pr --title "Review" --body-file review.md --head fix/x --base main
publish.sh comment --body-file summary.md
```

### Commands

#### create-pr - Create a new PR review

```bash
publish.sh create-pr \
  --title "Code Review Results" \
  --body-file review.md \
  --head fix/review-branch \
  --base main
```

**Parameters:**
- `--title` (required): Review title
- `--body` or `--body-file` (required): Review body (file or inline)
- `--head` (required): Head branch name
- `--base` (required): Base branch name
- `--draft`: Create as draft PR (default: false)
- `--labels`: Comma-separated labels

#### update-pr - Update existing review

```bash
publish.sh update-pr \
  --pr-number 123 \
  --body-file updated-review.md
```

#### comment - Post PR-level comment

```bash
publish.sh comment \
  --body-file summary.md
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_OUTPUT_DIR` | `/holon/output` | Directory containing review artifacts |
| `DRY_RUN` | `false` | Preview without posting |
| `MAX_INLINE` | `20` | Maximum inline comments to post |
| `POST_EMPTY` | `false` | Post review even with no findings |

### Input Files (Agent-Generated)

The script expects these artifacts in `GITHUB_OUTPUT_DIR`:

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

1. **Creates PR review** using `gh pr review` command
2. **Posts inline comments** for findings with path+line information
3. **Limits inline comments** via `MAX_INLINE` (most important findings first)
4. **Skips posting** if `POST_EMPTY=false` and no findings
5. **Dry-run mode** previews without posting

### Examples

```bash
# Preview review
DRY_RUN=true publish.sh create-pr --title "Review" --body-file review.md --head fix/x --base main

# Limit inline comments
MAX_INLINE=10 publish.sh create-pr ...

# Post even if no findings
POST_EMPTY=true publish.sh create-pr ...

# Use intent file
publish.sh --intent=/holon/output/publish-intent.json
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
          /holon/workspace/skills/github-review/scripts/collect.sh "${{ github.repository }}#${{ github.event.pull_request.number }}"

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
          cd /holon/workspace/skills/github-review
          ./scripts/publish.sh
        env:
          GITHUB_OUTPUT_DIR: ${PWD}/context
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

### Manual Workflow

```bash
# 1. Collect
GITHUB_OUTPUT_DIR=./review collect.sh "owner/repo#123"

# 2. Agent performs review (reads ./review/github-review-context/)
#    Agent writes to ./review/review.md and review.json

# 3. Publish
cd review/github-review/scripts
./publish.sh
```

## Error Handling

Both scripts include error handling:

- Missing dependencies (`gh`, `jq`) → Clear error message
- Invalid PR reference → Usage help
- Authentication failure → Check token message
- Missing artifacts → Fails fast with clear error

Scripts exit with non-zero status on error for reliable CI integration.
