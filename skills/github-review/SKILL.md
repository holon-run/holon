---
name: github-review
description: "Automated PR code review skill that collects context, performs AI-powered analysis, and publishes structured reviews with inline comments. Use when Claude needs to review pull requests: (1) Analyzing code changes for correctness/security/performance issues, (2) Generating review findings with inline comments, (3) Publishing reviews via GitHub API. Supports one-shot review and CI integration."
---

# GitHub Review Skill

Automated code review skill for pull requests. Collects PR context, performs AI-powered code review, and publishes structured reviews with inline comments.

**Prerequisites:** This skill requires the `github-context` skill to collect PR data and must be distributed together with it.

## Environment and Paths

This skill uses environment variables to stay portable across Holon, local shells, and CI. It delegates context collection to the shared `github-context` skill; no absolute install paths are assumed. Agents should invoke `github-context` when context is missing, then invoke `github-publish` (or the included wrapper) to publish.

### Key Environment Variables

- **`GITHUB_OUTPUT_DIR`**: Where this skill writes artifacts  
  - Default: `/holon/output` if present; otherwise a temp dir `/tmp/holon-ghreview-*`
- **`GITHUB_CONTEXT_DIR`**: Where `github-context` writes collected PR data  
  - Default: `${GITHUB_OUTPUT_DIR}/github-context`
- **`GITHUB_TOKEN` / `GH_TOKEN`**: Token used when invoking `github-context` / `github-publish` (scopes: `repo` or `public_repo`)
- Publishing options (e.g., inline limits) can be passed to `github-publish` (`MAX_INLINE`, `POST_EMPTY`, etc.) before calling it.

### Path Examples

```bash
# Holon container (default picked up automatically)
export GITHUB_OUTPUT_DIR=/holon/output

# Local development (keeps workspace clean by defaulting to /tmp if unset)
export GITHUB_OUTPUT_DIR=./output
export GITHUB_TOKEN=ghp_xxx

# CI/CD environment
export GITHUB_OUTPUT_DIR=${PWD}/artifacts
export GITHUB_TOKEN=${{ secrets.GITHUB_TOKEN }}
export MAX_INLINE=10
```

## Workflow

This skill follows a three-step workflow and assumes `github-context` is co-installed (same skill bundle) for collection. Agents collect via `github-context`, generate artifacts, then publish via `github-publish` (or the provided wrapper).

### 1. Collect Context
Collect PR information via the `github-context` skill (review-friendly defaults recommended):
- PR metadata (title, description, author, stats)
- Changed files list with full diff
- Existing review threads (to avoid duplicates)
- PR comments and commit history

### 2. Perform Review
Agent analyzes the collected context and generates:
- `review.md` - Human-readable review summary
- `review.json` - Structured findings with path/line/severity/message
- `summary.md` - Brief process summary

Agent follows review guidelines in `prompts/review.md`.

### 3. Publish Review
Use `github-publish` (or this skill’s `publish.sh` wrapper) with the produced artifacts (`review.md`, `review.json`, `summary.md`, `manifest.json`) to post the PR review and inline comments.

## Usage

### Basic Usage

- Collect PR context with `github-context` (agent-triggered if missing).
- Run `github-review` to produce `review.md`, `review.json`, `summary.md`.
- Publish via `github-publish` (or this skill’s `publish.sh`) using those artifacts.

### Advanced Options

```bash
# Preview review without posting (dry-run)
export DRY_RUN=true
holon --skill github-review holon-run/holon#123

# Limit inline comments
export MAX_INLINE=10
holon --skill github-review holon-run/holon#123

# Post review even if no findings
export POST_EMPTY=true
holon --skill github-review holon-run/holon#123

# Combine options
export MAX_INLINE=15 POST_EMPTY=true
holon --skill github-review holon-run/holon#123
```

## Scripts

Wrapper scripts remain for convenience (`collect.sh` delegates to `github-context`; `publish.sh` can post a review). Agents may call them when available; the recommended flow is still: call `github-context` to collect, then `github-publish` to post.

## Agent Prompts

**`prompts/review.md`**: Review guidelines and output format for agents

Agents should read this file to understand review priorities, what to skip, and how to structure findings.

## Output Contract

### Required Inputs (from collection script)

1. **`${GITHUB_CONTEXT_DIR}/github/pr.json`**: PR metadata
2. **`${GITHUB_CONTEXT_DIR}/github/files.json`**: Changed files list
3. **`${GITHUB_CONTEXT_DIR}/github/pr.diff`**: Full diff of changes

### Optional Inputs (from collection script)

4. **`${GITHUB_CONTEXT_DIR}/github/review_threads.json`**: Existing review comments
5. **`${GITHUB_CONTEXT_DIR}/github/comments.json`**: PR discussion comments
6. **`${GITHUB_CONTEXT_DIR}/github/commits.json`**: Commit history
7. **`${GITHUB_CONTEXT_DIR}/github/check_runs.json`**: Check runs (when `INCLUDE_CHECKS=true`)

### Required Outputs (from agent)

1. **`${GITHUB_OUTPUT_DIR}/review.md`**: Human-readable review summary
   - Overall summary of the PR
   - Key findings by severity
   - Detailed feedback organized by category
   - Positive notes and recommendations

2. **`${GITHUB_OUTPUT_DIR}/review.json`**: Structured review findings
   ```json
   [
     {
       "path": "path/to/file.go",
       "line": 42,
       "severity": "error|warn|nit",
       "message": "Clear description of the issue",
       "suggestion": "Specific suggestion for fixing (optional)"
     }
   ]
   ```

3. **`${GITHUB_OUTPUT_DIR}/summary.md`**: Brief summary of the review process
   - PR reference and metadata
   - Number of findings
   - Review outcomes

### Optional Outputs (from agent)

4. **`${GITHUB_OUTPUT_DIR}/manifest.json`**: Execution metadata
   ```json
   {
     "provider": "github-review",
     "pr_ref": "holon-run/holon#123",
     "findings_count": 5,
     "inline_comments_count": 3,
     "status": "completed|completed_with_empty|failed"
   }
   ```

## Context Files

When context is collected, the following files are available under `${GITHUB_CONTEXT_DIR}/github/`:

- `pr.json`: Pull request metadata
- `files.json`: List of changed files
- `pr.diff`: Full diff of changes
- `review_threads.json`: Existing review comments (if any)
- `comments.json`: PR discussion comments (if any)
- `commits.json`: Commit history (if any)

## Integration Examples

### Holon Skill Mode

```yaml
# .github/workflows/pr-review.yml
name: PR Review
on:
  pull_request:
    types: [opened, synchronize]

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - name: Run Holon review
        uses: holon-run/holon@main
        with:
          skill: github-review
          args: ${{ github.repository }}#${{ github.event.pull_request.number }}
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          MAX_INLINE: 20
```

### Manual Review

```bash
# Review a PR manually
export GITHUB_TOKEN=ghp_xxx
export GITHUB_OUTPUT_DIR=./my-review
mkdir -p $GITHUB_OUTPUT_DIR

# Collect context
scripts/collect.sh "holon-run/holon#123"

# Perform review (agent reads context, generates findings)
# ... agent processes ...

# Preview review
scripts/publish.sh --dry-run

# Publish review
scripts/publish.sh
```

## Important Notes

- **Idempotency**: The skill fetches existing review threads to avoid duplicating comments
- **Rate limits**: GitHub API has rate limits; the skill uses pagination and batching appropriately
- **Large PRs**: Use `MAX_FILES` to limit context collection for PRs with many changed files
- **Inline limits**: Use `MAX_INLINE` to avoid overwhelming reviewers with too many comments
- **Silent success**: By default, the skill doesn't post reviews when no findings are found (use `POST_EMPTY=true` to change this)
- **Headless operation**: The skill runs without user interaction; all configuration is via environment variables

## Capabilities

You MAY use these commands directly via `gh` CLI:
- `gh pr view` - View PR details
- `gh pr diff` - Get PR diff
- `gh api` - Make API calls

You MUST NOT use these directly (use artifacts instead):
- `gh pr review` - Use `scripts/publish.sh` instead

## Reference Documentation

See [prompts/review.md](prompts/review.md) for detailed review guidelines and instructions.
