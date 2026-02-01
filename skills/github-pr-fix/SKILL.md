---
name: github-pr-fix
description: "Fix pull requests based on review feedback and CI failures. Use when: (1) Addressing PR review comments, (2) Fixing CI/test failures, (3) Resolving merge conflicts, (4) Implementing requested changes."
---

# GitHub PR Fix Skill

Automation skill for fixing pull requests based on review feedback, CI failures, and other issues.

## Purpose

This skill helps you:
1. Analyze PR review comments and CI failures
2. Fix code issues in priority order (build → test → lint)
3. Push fixes to the PR branch
4. Reply to review comments with fix status

## Prerequisites

This skill depends on (co-installed and callable by the agent):
- **`github-context`**: Agent should invoke it to collect PR metadata, reviews, diffs, and CI logs
- **`github-publish`**: Agent should invoke it to post review replies from the produced artifacts

## Environment & Paths

- **`GITHUB_OUTPUT_DIR`**: Where this skill writes artifacts  
  - Default: `/holon/output` if present; otherwise a temp dir `/tmp/holon-ghprfix-*`
- **`GITHUB_CONTEXT_DIR`**: Where `github-context` writes collected data  
  - Default: `${GITHUB_OUTPUT_DIR}/github-context`
- **`GITHUB_TOKEN` / `GH_TOKEN`**: Token for GitHub operations (scopes: `repo` or `public_repo`)
- **`HOLON_GITHUB_BOT_LOGIN`**: Bot login for idempotency (default `holonbot[bot]`)

## Inputs & Outputs

- **Inputs** (agent obtains via `github-context`): `${GITHUB_CONTEXT_DIR}/github/pr.json`, `review_threads.json`, `check_runs.json`, `pr.diff`, etc.
- **Outputs** (agent writes under `${GITHUB_OUTPUT_DIR}`):
  - `pr-fix.json` (reply plan)
  - `summary.md`
  - `manifest.json`

### 1. Context Collection

If context is not pre-populated, invoke the `github-context` skill with PR options (e.g., INCLUDE_DIFF= true, INCLUDE_CHECKS=true, INCLUDE_THREADS=true, INCLUDE_FILES=true).

### 2. Analyze PR Feedback

Read the collected context:
- `${GITHUB_CONTEXT_DIR}/github/pr.json`: PR metadata and reviews
- `${GITHUB_CONTEXT_DIR}/github/review_threads.json`: Review comments with line numbers
- `${GITHUB_CONTEXT_DIR}/github/check_runs.json`: CI check results
- `${GITHUB_CONTEXT_DIR}/github/test-failure-logs.txt`: Failed test logs
- `${GITHUB_CONTEXT_DIR}/github/pr.diff`: Code changes

Identify issues in priority order:
1. **Build errors** - Blocking, must fix first
2. **Test failures** - High priority
3. **Import/type errors** - Medium priority
4. **Lint issues** - Lower priority
5. **Refactor requests** - Non-blocking, can defer

### 3. Fix Issues

Check out the PR branch and fix issues:

```bash
# Checkout PR branch
git checkout <pr-branch>

# Make fixes
# ... fix code issues ...

# Commit changes
git add .
git commit -m "Fix: <description>"

# Push to PR branch
git push
```

**IMPORTANT**: Commit your code fixes BEFORE replying to reviews. This ensures reviewers can see your actual fixes when reading your replies.

### 4. Generate Artifacts

Create the required output files:

#### `${GITHUB_OUTPUT_DIR}/pr-fix.json`

Structured JSON with fix status and responses:

```json
{
  "review_replies": [
    {
      "comment_id": 123,
      "status": "fixed|wontfix|need-info|deferred",
      "message": "Response to reviewer",
      "action_taken": "Description of code changes"
    }
  ],
  "follow_up_issues": [
    {
      "title": "Issue title",
      "body": "Issue body in Markdown",
      "deferred_comment_ids": [123],
      "labels": ["enhancement", "testing"]
    }
  ],
  "checks": [
    {
      "name": "ci/test",
      "conclusion": "failure",
      "fix_status": "fixed|unfixed|unverified|not-applicable",
      "message": "Explanation of what was fixed"
    }
  ]
}
```

#### `${GITHUB_OUTPUT_DIR}/summary.md`

Human-readable summary:
- PR reference
- Issues identified
- Fixes applied
- Review responses

#### `${GITHUB_OUTPUT_DIR}/manifest.json`

Execution metadata:
```json
{
  "provider": "github-pr-fix",
  "pr_ref": "holon-run/holon#123",
  "status": "completed|partial|failed",
  "fixes_applied": 5,
  "reviews_replied": 3
}
```

### 5. Reply to Reviews

Use `${GITHUB_OUTPUT_DIR}/pr-fix.json` to produce `publish-intent.json` with `reply_review` actions, then invoke the `github-publish` skill to post replies. `reply-reviews.sh` is removed; publishing is unified via `github-publish`.

## Output Contract

### Required Outputs

1. **`${GITHUB_OUTPUT_DIR}/summary.md`**: Human-readable summary
   - PR reference and description
   - Issues identified
   - Fixes applied
   - Review responses

2. **`${GITHUB_OUTPUT_DIR}/manifest.json`**: Execution metadata

3. **`${GITHUB_OUTPUT_DIR}/pr-fix.json`**: Structured fix status and responses

## Git Operations

You are responsible for all git operations:

```bash
# Checkout PR branch
git checkout <pr-branch>

# Stage changes
git add .

# Commit with descriptive message
git commit -m "Fix: <description>"

# Push to PR branch (NOT a new branch)
git push
```

**Note**: Push changes to the existing PR branch, do NOT create a new PR.

## Scripts

### reply-reviews.sh

Post formatted replies to PR review comments.

**Usage:**
```bash
# Preview without posting
reply-reviews.sh --dry-run --pr=owner/repo#123

# Post all replies
reply-reviews.sh --pr=owner/repo#123

# Resume from specific reply
reply-reviews.sh --from=5 --pr=owner/repo#123
```

**Options:**
- `--dry-run`: Show what would be posted without actually posting
- `--from=N`: Start from reply N (for error recovery)
- `--pr=OWNER/REPO#NUMBER`: Target PR
- `--bot-login=NAME`: Bot login name for idempotency

## Important Notes

- You are running **HEADLESSLY** - do not wait for user input or confirmation
- Fix issues in priority order: build → test → import → lint
- Commit fixes BEFORE replying to reviews
- Use idempotency - the script will skip duplicate replies
- For non-blocking refactor requests, consider deferring to follow-up issues

## Reference Documentation

- **[pr-fix-workflow.md](references/pr-fix-workflow.md)**: Complete workflow guide
  - Error triage and priority order
  - Environment setup and verification
  - Test failure diagnosis
  - Handling refactor requests
  - Posting review replies

- **[diagnostics.md](references/diagnostics.md)**: Diagnostic confidence levels
  - Confidence levels for CI failure diagnosis
  - Common contract rules
