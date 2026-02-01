# GitHub Publishing

Complete guide for GitHub publishing in the github-publish skill.

## Table of Contents

1. [Overview](#overview)
2. [Workflow and Responsibilities](#workflow-and-responsibilities)
3. [publish-intent.json Format](#publish-intentjson-format)
4. [Action Types](#action-types)
5. [Usage Examples](#usage-examples)
6. [Direct Command Mode (Planned for Future Phases)](#direct-command-mode-planned-for-future-phases)
7. [Environment Variables](#environment-variables)
8. [Options](#options)
9. [Output](#output)
10. [Authentication](#authentication)
11. [Best Practices](#best-practices)
12. [Troubleshooting](#troubleshooting)
13. [Dependencies](#dependencies)

## Overview

The publishing system uses a **unified publish.sh script** that supports:

1. **Batch Mode** (available): Execute multiple actions from a declarative intent file
2. **Direct Command Mode** (planned): Execute single actions with command-line arguments - **Not yet implemented in Phase 1**

**Note**: Direct Command Mode is planned for future phases. Currently, only Batch Mode (--intent) is supported.

## Workflow and Responsibilities

When using the publishing system, responsibilities are clearly divided between the **agent** and the **script**.

### Agent Responsibilities (Code and Git Operations)

The agent is responsible for all **creative and intelligent** work:

1. **Create feature branch** (if needed):
   ```bash
   git checkout -b feature/issue-503
   ```

2. **Edit code directly** (modify files in workspace):
   ```bash
   # Edit files directly, don't generate patches
   vim cmd/holon/solve.go
   vim skills/github-publish/scripts/publish.sh
   ```

3. **Test and iterate**:
   ```bash
   go test ./...
   ./scripts/publish.sh --dry-run --intent=/tmp/test.json
   ```

4. **Commit changes**:
   ```bash
   git add .
   git commit -m "Feature: GitHub publishing"
   ```

5. **Generate artifacts**:
   - `${GITHUB_OUTPUT_DIR}/publish-intent.json` - Publishing instructions
   - `${GITHUB_OUTPUT_DIR}/summary.md` - Main description document (used as PR body or comment)
   - `${GITHUB_OUTPUT_DIR}/pr-fix.json` - Review replies (for PR-fix mode)
   - Optional: Additional comment files (e.g., `ci-note.md`, `fix-summary.md`)

6. **Execute publishing script**:
   ```bash
   cd /holon/workspace/skills/github-publish
   ./scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
   ```

### Script Responsibilities (GitHub API Operations)

The `publish.sh` script is responsible for all **operational and repetitive** work:

1. **Validate intent** - Check file structure and required fields
2. **Call GitHub API** - Create PRs, post comments, reply to reviews
3. **Handle idempotency** - Avoid duplicate operations
4. **Format output** - Generate publish-results.json
5. **Error handling** - Graceful failure with clear messages

### Why This Division?

| Responsibility | Agent | Script |
|----------------|-------|--------|
| **Code editing** | ✅ Intelligent, iterative | ❌ Can't make decisions |
| **Git operations** | ✅ Simple, reliable | ❌ Unnecessary complexity |
| **Testing** | ✅ Needs interpretation | ❌ Can't analyze results |
| **API calls** | ❌ Error-prone, repetitive | ✅ Idempotent, reliable |
| **Formatting** | ❌ Inconsistent | ✅ Standardized |

- **Agent excels at**: Understanding requirements, making decisions, iterating on code
- **Script excels at**: Repetitive tasks, API calls, error handling, consistency

### Complete Workflow Example

```bash
# ===== AGENT DOES THIS =====
# 1. Analyze the issue
cat /holon/input/context/github/issue.json

# 2. Create branch
git checkout -b feature/issue-503

# 3. Implement solution (edit files directly)
vim pkg/publisher/github/publisher.go

# 4. Test
go test ./pkg/publisher/github/

# 5. Commit
git add .
git commit -m "Feature: Add GitHub publishing to skill"

# 6. Generate summary (main description document)
cat > ${GITHUB_OUTPUT_DIR}/summary.md <<EOF
## Summary

Implements GitHub publishing in github-publish skill

## Changes
- Add publish.sh: Unified publishing script
- Add lib/publish.sh: Action implementations
- Update SKILL.md: Documentation

## Testing
✅ Intent validation working
✅ Dry-run mode working
✅ Path flexibility verified

Resolves #503
EOF

# 7. Generate publish intent
cat > ${GITHUB_OUTPUT_DIR}/publish-intent.json <<EOF
{
  "version": "1.0",
  "pr_ref": "holon-run/holon#503",
  "actions": [
    {
      "type": "create_pr",
      "params": {
        "title": "Feature: GitHub skill publishing",
        "body": "summary.md",
        "head": "feature/issue-503",
        "base": "main"
      }
    }
  ]
}
EOF

# 8. (Optional) Add extra comment if needed
# Example: CI fix explanation, important notes, etc.
# cat > ${GITHUB_OUTPUT_DIR}/ci-note.md <<EOF
# ## CI 修复说明
#
# 之前的 CI 失败已修复，可以重新运行检查。
# EOF
# Then add post_comment action to intent if needed

# 9. Test publishing (dry-run)
cd /holon/workspace/skills/github-solve
./scripts/publish.sh --dry-run --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json

# ===== SCRIPT DOES THIS =====
# 10. Execute actual publishing
./scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json

# Output: publish-results.json shows what was done
```

## publish-intent.json Format

For batch mode, create a `${GITHUB_OUTPUT_DIR}/publish-intent.json` file:

```json
{
  "version": "1.0",
  "pr_ref": "holon-run/holon#123",
  "actions": [
    {
      "type": "create_pr|update_pr|post_comment|reply_review|post_review",
      "description": "Human-readable description",
      "params": { }
    }
  ]
}
```

### File Usage Guidelines

**summary.md** - Primary description document:
- Used as PR body for create_pr action
- Contains main description, changes, testing info
- **Required for most workflows**

**Additional comment files** (optional):
- Created only when extra context is needed
- Examples: `ci-note.md`, `fix-summary.md`, `implementation-notes.md`
- Used with post_comment action
- **Agent decides when needed**

**When to use what**:

| Scenario | Files Needed | post_comment? |
|----------|--------------|---------------|
| **Issue→PR** (standard) | `summary.md` only | ❌ No (summary is PR body) |
| **Issue→PR** (needs extra context) | `summary.md` + `extra.md` | ✅ Yes (for extra.md) |
| **PR-fix** (standard) | `pr-fix.json` + `fix-summary.md` | ✅ Yes (to summarize fixes) |
| **PR-fix** (minimal) | `pr-fix.json` only | ❌ No (if replies are enough) |

## Action Types

### 1. create_pr - Create a Pull Request

Creates a new PR from a feature branch.

**Parameters**:
- `title` (required): PR title
- `body` (required): PR description (file path or inline markdown)
- `head` (required): Feature branch name
- `base` (required): Target branch (e.g., "main")
- `draft` (optional): Create as draft (default: false)
- `labels` (optional): Array of label strings

**Example**:
```json
{
  "type": "create_pr",
  "description": "Create PR for GitHub publishing feature",
  "params": {
    "title": "Feature: GitHub skill publishing",
    "body": "summary.md",
    "head": "feature/github-publishing",
    "base": "main",
    "draft": false,
    "labels": ["enhancement", "documentation"]
  }
}
```

**Note**: The `body` parameter typically references `summary.md`, which contains the main description. Use separate comment files only when additional context is needed.

### 2. update_pr - Update an Existing PR

Updates an existing PR's metadata.

**Parameters**:
- `pr_number` (required): PR number to update
- `title` (optional): New PR title
- `body` (optional): New PR description (file path or inline)
- `state` (optional): New state ("open" or "closed")

**Example**:
```json
{
  "type": "update_pr",
  "description": "Mark PR as ready",
  "params": {
    "pr_number": 123,
    "title": "Feature: GitHub skill publishing - ready for review"
  }
}
```

### 3. post_comment - Post a PR-Level Comment

Posts or updates a PR-level comment with idempotency.

**Parameters**:
- `body` (required): Comment content (file path or inline markdown)
- `marker` (optional): Unique identifier for idempotency (default: "holon-publish-marker")

**Example**:
```json
{
  "type": "post_comment",
  "description": "Post implementation summary",
  "params": {
    "body": "summary.md"
  }
}
```

**Idempotency**: The comment includes a hidden marker (`<!-- holon-publish-marker -->`). Re-running will update the existing comment instead of creating duplicates.

### 4. reply_review - Reply to Review Comments

Posts formatted replies to PR review comments.

**Parameters**:
- `replies_file` (recommended): Path to pr-fix.json file
- `replies` (optional): Array of inline reply objects

**Using pr-fix.json (Recommended)**:
```json
{
  "type": "reply_review",
  "description": "Respond to review feedback",
  "params": {
    "replies_file": "pr-fix.json"
  }
}
```

**pr-fix.json Format**:
```json
{
  "review_replies": [
    {
      "comment_id": 123456,
      "status": "fixed|deferred|wontfix",
      "message": "Explanation of the fix",
      "action_taken": "Updated file X line Y"
    }
  ]
}
```

### 5. post_review - Post a PR Review with Inline Comments

Posts a single PR review (`event=COMMENT`) with optional inline comments.

**Parameters**:
- `body` (required): Review body (inline markdown or path relative to output dir, e.g., `review.md`)
- `comments_file` (optional): Path to findings file (default: `review.json`)
- `max_inline` (optional): Limit inline comments (default: 20)
- `post_empty` (optional): Post even when there are zero findings (default: false)
- `commit_id` (optional): PR head SHA (auto-fetched if omitted)

**Example**:
```json
{
  "type": "post_review",
  "description": "Post code review with inline comments",
  "params": {
    "body": "review.md",
    "comments_file": "review.json",
    "max_inline": 15,
    "post_empty": false
  }
}
```

## Usage Examples

### Issue→PR Workflow

```bash
# 1. Implement solution and create branch
git checkout -b feature/issue-503
# ... make changes ...

# 2. Write summary (main description document)
cat > ${GITHUB_OUTPUT_DIR}/summary.md <<EOF
## Summary

Implements GitHub publishing in github-publish skill

## Changes
- Add publish.sh: Unified publishing script
- Add lib/publish.sh: Action implementations

## Testing
✅ All tests passing

Resolves #503
EOF

# 3. Write publish intent (use summary as PR body)
cat > ${GITHUB_OUTPUT_DIR}/publish-intent.json <<EOF
{
  "version": "1.0",
  "pr_ref": "holon-run/holon#503",
  "actions": [
    {
      "type": "create_pr",
      "params": {
        "title": "Feature: GitHub publishing",
        "body": "summary.md",
        "head": "feature/issue-503",
        "base": "main"
      }
    }
  ]
}
EOF

# 4. Execute publishing
cd /holon/workspace/skills/github-solve
./scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
```

**When to add extra comment**: Only add `post_comment` action when you need to:
- Provide additional context beyond the PR description
- Explain CI fixes or troubleshooting steps
- Add stage markers in long-running PR discussions
- Mention specific users or teams

### PR-Fix Workflow

**IMPORTANT**: Commit your code fixes BEFORE replying to reviews. This ensures reviewers can see your actual fixes when reading your replies.

```bash
# 1. Fix issues based on review feedback
# ... make code changes ...

# 2. Commit and push your fixes
git add .
git commit -m "Fix: Address review feedback"
git push

# 3. Generate pr-fix.json with review responses
cat > ${GITHUB_OUTPUT_DIR}/pr-fix.json <<EOF
{
  "review_replies": [
    {
      "comment_id": 123456,
      "status": "fixed",
      "message": "Fixed the validation issue",
      "action_taken": "Updated helpers.sh:123"
    }
  ]
}
EOF

# 4. Write publish intent
cat > ${GITHUB_OUTPUT_DIR}/publish-intent.json <<EOF
{
  "version": "1.0",
  "pr_ref": "holon-run/holon#507",
  "actions": [
    {
      "type": "reply_review",
      "params": {
        "replies_file": "pr-fix.json"
      }
    },
    {
      "type": "post_comment",
      "params": {
        "body": "summary.md"
      }
    }
  ]
}
EOF

# 5. Execute publishing
cd /holon/workspace/skills/github-solve
./scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
```

## Direct Command Mode (Planned for Future Phases)

**Status**: ⚠️ Not yet implemented - Planned for Phase 2+

Direct Command Mode will allow simple operations without an intent file:

```bash
# Future examples (not yet available):
./scripts/publish.sh create-pr \
    --title "Feature: GitHub publishing" \
    --body summary.md \
    --head feature/github-publishing \
    --base main

./scripts/publish.sh comment --body summary.md

./scripts/publish.sh reply-reviews --pr-fix-json pr-fix.json
```

**Current Workaround**: Use batch mode with intent file for all operations.

## Environment Variables

- `GITHUB_OUTPUT_DIR`: Output directory (default: `/holon/output`)
- `GITHUB_TOKEN`: GitHub authentication token (required)
- `HOLON_GITHUB_BOT_LOGIN`: Bot login name for idempotency (default: holonbot[bot])

## Options

- `--dry-run`: Show what would be done without executing
- `--from=N`: Start from action N (for resume capability)
- `--pr=OWNER/REPO#NUM`: Target PR reference (auto-detected from intent if not specified)

## Output

All publishing operations generate `${GITHUB_OUTPUT_DIR}/publish-results.json`:

```json
{
  "version": "1.0",
  "pr_ref": "holon-run/holon#507",
  "executed_at": "2026-01-26T12:00:00Z",
  "dry_run": false,
  "actions": [
    {
      "index": 0,
      "type": "reply_review",
      "status": "completed",
      "result": {
        "total": 30,
        "posted": 30,
        "skipped": 0,
        "failed": 0
      }
    }
  ],
  "summary": {
    "total": 1,
    "completed": 1,
    "failed": 0
  },
  "overall_status": "success"
}
```

## Authentication

The publish.sh script requires `GITHUB_TOKEN` to be set:

```bash
export GITHUB_TOKEN="ghp_xxx"  # Or via container mount
```

For production use, the token should be provided via:
- Container secrets mount
- Environment variable injection
- GitHub Actions secrets

## Best Practices

1. **Always test with --dry-run first**:
   ```bash
   ./scripts/publish.sh --dry-run --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
   ```

2. **Use file references for long content**:
   ```json
   {
     "params": {
       "body": "pr-description.md"  // Better than inline markdown
     }
   }
   ```

3. **Leverage idempotency**: All actions can be safely re-run. Use this for debugging and recovery.

4. **Combine actions strategically**:
   - Issue→PR: create_pr + post_comment
   - PR-fix: reply_review + post_comment
   - Complex: create_pr + update_pr + post_comment

5. **Review publish-results.json**: After execution, check results to verify success and diagnose issues.

## Troubleshooting

**Error: "Missing required field"**
- Ensure all required parameters are provided
- Check that file references point to existing files

**Error: "Branch not found"**
- Create the feature branch before calling publish.sh
- Or add `"create_branch": true` to create_pr params

**Error: "gh auth failed"**
- Ensure GITHUB_TOKEN is set and valid
- Run `gh auth status` to verify authentication

**Partial failures**:
- Check publish-results.json for failed actions
- Use `--from=N` to resume from failed action
- Fix the issue and re-run

## Dependencies

The publishing system requires:
- `gh` CLI - GitHub API operations
- `jq` - JSON processing
- `git` - Version control operations (for create_pr)
