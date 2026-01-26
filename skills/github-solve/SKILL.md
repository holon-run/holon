# SKILL: github-solve

## Environment and Paths

This skill uses environment variables to maintain flexibility across different environments (Holon, local development, CI/CD, etc.):

### Key Environment Variables

- **`GITHUB_OUTPUT_DIR`**: Output directory for artifacts and context
  - **Default**: `/holon/output` (Holon environment)
  - **Custom**: Set to any directory (e.g., `./output`, `/tmp/github-work`)
  - **Usage**: All generated files (artifacts, results) go here

- **`GITHUB_CONTEXT_DIR`**: Directory for collected GitHub context
  - **Default**: `${GITHUB_OUTPUT_DIR}/github-context`
  - **Custom**: Override to control where context is stored

- **`GITHUB_TOKEN`**: GitHub authentication token (required for publishing)
  - **Required for**: Creating PRs, posting comments, replying to reviews
  - **Not required for**: Context collection (read-only operations)

- **`HOLON_GITHUB_BOT_LOGIN`**: Bot login name for idempotency checks
  - **Default**: `holonbot[bot]`
  - **Purpose**: Skip bot's own comments/replies when checking for duplicates

### Path Examples

```bash
# Holon environment (default)
export GITHUB_OUTPUT_DIR=/holon/output

# Local development
export GITHUB_OUTPUT_DIR=./output
export GITHUB_TOKEN=ghp_xxx

# CI/CD environment
export GITHUB_OUTPUT_DIR=${PWD}/artifacts
export GITHUB_TOKEN=${{ secrets.GITHUB_TOKEN }}

# Then use scripts (they respect GITHUB_OUTPUT_DIR)
./scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
```

**Note**: This documentation uses `${GITHUB_OUTPUT_DIR}` as a placeholder. Replace it with your actual output directory or set the environment variable.

## Minimal Input Payload

When no pre-populated GitHub context is available (i.e., `/holon/input/context/github/` is empty or missing), you **MUST** first collect context using the skill's built-in collection script.

The minimal input payload required is:
- **`/holon/input/payload.json`** (optional): Contains task metadata with GitHub reference
  ```json
  {
    "ref": "holon-run/holon#502",
    "repo": "holon-run/holon",
    "type": "issue|pr",
    "trigger_comment_id": 123456  // optional
  }
  ```

The agent must extract the `ref` field from `payload.json` and pass it as a command-line argument to the collection script. If `payload.json` doesn't exist or `ref` is not provided, check for:
1. Command-line arguments or environment variables with the reference
2. Fallback to requesting the reference from the user

## Context Collection (When No Pre-Populated Context)

When `/holon/input/context/github/` is empty or missing required files:

1. **Run the collection script**:
   ```bash
   /holon/workspace/skills/github-solve/scripts/collect.sh "<ref>" [repo_hint]
   ```

   Where `<ref>` is one of:
   - `holon-run/holon#502` - owner/repo#number format
   - `502` - numeric (requires repo_hint)
   - `https://github.com/holon-run/holon/issues/502` - full URL

2. **Configure with environment variables** (optional):
   ```bash
   export GITHUB_CONTEXT_DIR=${GITHUB_OUTPUT_DIR}/github-context  # default
   export TRIGGER_COMMENT_ID=123456  # if provided in payload
   export INCLUDE_DIFF=true          # for PRs
   export INCLUDE_CHECKS=true        # for PRs
   export UNRESOLVED_ONLY=true       # for PR review threads
   ```

3. **Copy collected context** to input location (for compatibility):
   ```bash
   mkdir -p /holon/input/context/github
   cp -r ${GITHUB_OUTPUT_DIR}/github-context/github/* /holon/input/context/github/
   ```

4. **Proceed with task** using collected context

The collection script fetches:
- **For issues**: `issue.json`, `comments.json`
- **For PRs**: `pr.json`, `review_threads.json`, `comments.json`, `pr.diff`, `check_runs.json`, `test-failure-logs.txt`

All collected context is persisted under `${GITHUB_OUTPUT_DIR}/github-context/` for audit/debug.

## Transition Behavior

- **Primary**: Always attempt to collect context using the skill's `scripts/collect.sh` first when pre-populated context is missing
- **Fallback**: If the script fails, check if `/holon/input/context/github/` has any files that were populated by the host (legacy behavior)
- **Error**: If neither method provides context, report the error clearly in `manifest.json` and exit

## Skill Scripts

This skill includes helper scripts in `scripts/`:

### Context Collection

- **`scripts/collect.sh`**: Main context collection script
  - Fetches issue/PR metadata, comments, diffs, and CI logs using `gh` CLI
  - Requires `gh` CLI to be authenticated (container environment typically provides this)
  - Requires `jq` for JSON processing
  - Usage: `collect.sh <ref> [repo_hint]`

- **`scripts/lib/helpers.sh`**: Reusable helper functions
  - `check_gh_cli()`, `check_jq()`: Verify required dependencies are available
  - `parse_ref()`: Parse GitHub reference into owner/repo/number
  - `determine_ref_type()`: Check if a number is a PR or issue
  - `fetch_issue_metadata()`, `fetch_pr_metadata()`: Get issue/PR details (includes reviews for PRs)
  - `fetch_issue_comments()`, `fetch_pr_comments()`, `fetch_pr_review_threads()`: Get comments
  - `fetch_pr_diff()`: Get PR diff
  - `fetch_pr_check_runs()`, `fetch_workflow_logs()`: Get CI status and logs
  - `verify_context_files()`: Validate required files exist and are non-empty
  - `write_manifest()`: Write collection manifest

### Review Reply Posting

- **`scripts/reply-reviews.sh`**: Post formatted replies to PR review comments
  - Reads `pr-fix.json` and posts replies with proper formatting
  - Implements idempotency checks to avoid duplicate replies
  - Requires `gh` CLI to be authenticated
  - Usage examples:
    ```bash
    # Preview replies without posting (dry-run)
    reply-reviews.sh --dry-run --pr=owner/repo#123

    # Post all replies
    reply-reviews.sh --pr=owner/repo#123

    # Resume from specific reply index (for error recovery)
    reply-reviews.sh --from=5 --pr=owner/repo#123
    ```
  - Options:
    - `--dry-run`: Show what would be posted without actually posting
    - `--from=N`: Start from reply N (useful if script fails midway)
    - `--pr=OWNER/REPO#NUMBER`: Target PR (auto-detected from git if not specified)
    - `--bot-login=NAME`: Bot login name for idempotency (default: holonbot[bot])
  - Environment variables:
    - `PR_FIX_JSON`: Path to pr-fix.json (default: ./pr-fix.json)
    - `HOLON_GITHUB_BOT_LOGIN`: Bot login name (default: holonbot[bot])
  - Output:
    - `reply-results.json`: JSON with results of each reply attempt
    - Console summary: total, posted, skipped, failed counts

## Context Detection

This skill adapts behavior based on the GitHub context provided:

- **PR Context** (`/holon/input/context/github/pr.json` exists):
  - PR-fix mode: Analyze PR feedback, review threads, and CI failures
  - Generate structured responses to make the PR mergeable

- **Issue Context** (only `/holon/input/context/github/issue.json` exists):
  - Issue-solve mode: Implement the feature or fix described in the issue
  - Create a pull request with the changes

## GitHub Context Files

When GitHub context is provided (either pre-populated or collected), files are available under `/holon/input/context/github/`:

### PR Context Files
- `pr.json`: Pull request metadata including reviews (review submissions without line-specific comments)
- `review_threads.json`: Review threads with line-specific comments (optional, includes `comment_id`)
- `comments.json`: PR discussion comments (optional)
- `pr.diff`: The code changes being reviewed (optional but recommended)
- `check_runs.json`: CI/check run metadata (optional)
- `test-failure-logs.txt`: Complete workflow logs for failed tests (optional, downloaded when checks fail)

### Issue Context Files
- `issue.json`: Issue metadata
- `comments.json`: Issue comments (optional)

## Important Notes

- When responding to review comments, use your GitHub identity (from common contract) to avoid replying to your own comments
- You are running **HEADLESSLY** - do not wait for user input or confirmation

## Capabilities

You MAY use these commands directly via `gh` CLI:
- `gh issue view` - View issue details
- `gh issue comment` - Comment on issues
- `gh pr view` - View PR details
- `gh pr comment` - Comment on PRs
- `gh issue edit` - Edit issue metadata (labels, assignees)
- `gh api` - Make read API calls

You MUST NOT use these directly (use artifacts instead):
- `git push` - Code pushing must go through artifacts
- `gh pr create` - PR creation must go through artifacts
- `gh pr merge` - PR merging must go through artifacts

## Output Contract

### Required Outputs

1. **`${GITHUB_OUTPUT_DIR}/summary.md`**: Human-readable summary of your analysis and actions taken

2. **`${GITHUB_OUTPUT_DIR}/manifest.json`**: Execution metadata and status

### Conditional Outputs

#### For PR Context (PR-fix mode)

3. **`${GITHUB_OUTPUT_DIR}/pr-fix.json`**: Structured JSON containing fix status and responses
   - Format:
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

#### For Issue Context (Issue-solve mode)

3. **`${GITHUB_OUTPUT_DIR}/diff.patch`**: Git-compatible patch with code changes

4. **`${GITHUB_OUTPUT_DIR}/pr-fix.json`** (if creating PR): PR creation metadata
   - `title`: PR title
   - `body`: PR body (references issue)
   - `branch`: Suggested branch name

## PR-Fix Mode Behavior

When PR context is detected:

### Error Triage (Priority Order)
1. **Build/compile failures** (blocks all tests)
2. **Runtime test failures**
3. **Import/module resolution errors**
4. **Lint/style warnings**

You MUST identify all errors first, then fix in this order. Do not fix lower-priority issues while higher-priority failures remain.

### Environment Setup (Before Claiming "Fixed")
- Verify required tools are available (build/test runners, package managers, compilers)
- If tools or dependencies are missing, attempt at least three setup paths:
  1. Project-recommended install commands
  2. Alternate install method (package manager, global install)
  3. Inspect CI workflow/config files for canonical setup steps
- If setup still fails, attempt a build/compile step (if possible) and report the failure

### Verification Requirements
- You may mark `fix_status: "fixed"` only if you ran a build/test command successfully
- If you cannot run tests, run the most relevant build/compile command and report that result
- If you made changes but cannot complete verification, use `fix_status: "unverified"` and document every attempt
- If you cannot address the issue or made no meaningful progress, use `fix_status: "unfixed"`
- Never claim success based on reasoning or syntax checks alone

### Test Failure Diagnosis

When CI tests fail, follow this workflow:

1. **Check for test logs**: Look for `context/github/test-failure-logs.txt`
2. **Read the logs**: Use grep to find specific test failures
3. **Analyze the failure**: What error/assertion failed? What file/line is failing?
4. **Determine relevance**: Check if modified files relate to the failure by comparing against `pr.diff`

**Using logs:**
```bash
# Find all failing tests
grep -E "(FAIL|FAIL:|FAILED)" /holon/input/context/github/test-failure-logs.txt

# Search for a specific test name
grep "TestRunner_Run_EnvVariablePrecedence" /holon/input/context/github/test-failure-logs.txt

# Show context around a failure
grep -A 20 "FAIL:" /holon/input/context/github/test-failure-logs.txt
```

### Handling Non-Blocking Refactor Requests

When review comments request substantial refactoring that is **valid but non-blocking**:

1. **Determine if truly non-blocking:**
   - Does not affect correctness, security, or API contracts
   - Would substantially increase PR scope (large refactor, comprehensive test suite)
   - Can be reasonably addressed in a follow-up without impacting this PR's value
   - Is an improvement rather than a fix for a problem introduced in this PR

2. **Use `status: "deferred"`** with clear explanation:
   - Acknowledge the validity of the suggestion
   - Explain why it's being deferred (scope, complexity, etc.)
   - Reference that a follow-up issue has been created

3. **Create a follow-up issue entry** in `follow_up_issues`:
   - **`title`**: Clear, actionable issue title
   - **`body`**: Comprehensive issue description including context, requested changes, rationale, and suggested approach
   - **`deferred_comment_ids`**: Array of comment IDs this issue addresses
   - **`labels`**: Suggested labels (e.g., `enhancement`, `testing`, `refactor`)

4. **Only defer when appropriate:**
   - **BLOCKING issues must be fixed**: bugs, security issues, breaking changes, missing critical functionality
   - **DEFER appropriate improvements**: additional test coverage, refactoring for clarity, performance optimizations
   - **Use `wontfix` for rejected suggestions**: requests that don't align with project goals

### Posting Review Replies

After generating `${GITHUB_OUTPUT_DIR}/pr-fix.json` with review replies:

1. **Use the skill's reply script**:
   ```bash
   # Navigate to skill directory
   cd /holon/workspace/skills/github-solve

   # Preview replies (recommended before posting)
   scripts/reply-reviews.sh --dry-run

   # Post replies to PR
   scripts/reply-reviews.sh --pr=owner/repo#123
   ```

2. **Reply format** (automatically applied by script):
   - Emoji based on status: ✅ fixed, ⚠️ wontfix, ❓ need-info, 🔜 deferred
   - Status label in uppercase
   - Your message
   - Optional "Action taken" section

3. **Idempotency**:
   - Script checks if bot already replied to each comment
   - Skips already-responded comments
   - Supports `--from=N` to resume from specific index

4. **Output files**:
   - `reply-results.json`: Detailed results for each reply attempt
   - Console summary: total, posted, skipped, failed counts

## Issue-Solve Mode Behavior

When only issue context is detected (no PR):

1. **Analyze the issue**: Read `issue.json` and `comments.json` (if present)
2. **Implement the solution**: Make code changes to address the issue
3. **Generate diff**: Create `${GITHUB_OUTPUT_DIR}/diff.patch` with your changes
4. **Document**: Write `${GITHUB_OUTPUT_DIR}/summary.md` explaining what was done
5. **PR metadata** (optional): Create `${GITHUB_OUTPUT_DIR}/pr-fix.json` with PR title, body, and branch name

## GitHub Publishing

The github-solve skill includes built-in GitHub publishing capabilities that allow agents to create PRs, post comments, and reply to reviews directly from within the container.

### Overview

The publishing system uses a **unified publish.sh script** that supports:

1. **Batch Mode** (available): Execute multiple actions from a declarative intent file
2. **Direct Command Mode** (planned): Execute single actions with command-line arguments - **Not yet implemented in Phase 1**

**Note**: Direct Command Mode is planned for future phases. Currently, only Batch Mode (--intent) is supported.

### Workflow and Responsibilities

When using the publishing system, responsibilities are clearly divided between the **agent** and the **script**:

#### Agent Responsibilities (Code and Git Operations)

The agent is responsible for all **creative and intelligent** work:

1. **Create feature branch** (if needed):
   ```bash
   git checkout -b feature/issue-503
   ```

2. **Edit code directly** (modify files in workspace):
   ```bash
   # Edit files directly, don't generate patches
   vim cmd/holon/solve.go
   vim skills/github-solve/scripts/publish.sh
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
   cd /holon/workspace/skills/github-solve
   ./scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
   ```

#### Script Responsibilities (GitHub API Operations)

The `publish.sh` script is responsible for all **operational and repetitive** work:

1. **Validate intent** - Check file structure and required fields
2. **Call GitHub API** - Create PRs, post comments, reply to reviews
3. **Handle idempotency** - Avoid duplicate operations
4. **Format output** - Generate publish-results.json
5. **Error handling** - Graceful failure with clear messages

#### Why This Division?

| Responsibility | Agent | Script |
|----------------|-------|--------|
| **Code editing** | ✅ Intelligent, iterative | ❌ Can't make decisions |
| **Git operations** | ✅ Simple, reliable | ❌ Unnecessary complexity |
| **Testing** | ✅ Needs interpretation | ❌ Can't analyze results |
| **API calls** | ❌ Error-prone, repetitive | ✅ Idempotent, reliable |
| **Formatting** | ❌ Inconsistent | ✅ Standardized |

- **Agent excels at**: Understanding requirements, making decisions, iterating on code
- **Script excels at**: Repetitive tasks, API calls, error handling, consistency

#### Complete Workflow Example

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

# 5. Generate summary (main description document)
cat > ${GITHUB_OUTPUT_DIR}/summary.md <<EOF
## Summary

Implements GitHub publishing in github-solve skill

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

# 6. Generate publish intent
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

# 7. (Optional) Add extra comment if needed
# Example: CI fix explanation, important notes, etc.
# cat > ${GITHUB_OUTPUT_DIR}/ci-note.md <<EOF
# ## CI 修复说明
#
# 之前的 CI 失败已修复，可以重新运行检查。
# EOF
# Then add post_comment action to intent if needed

# 8. Test publishing (dry-run)
cd /holon/workspace/skills/github-solve
./scripts/publish.sh --dry-run --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json

# ===== SCRIPT DOES THIS =====
# 9. Execute actual publishing
./scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json

# Output: publish-results.json shows what was done
```

### publish-intent.json Format

For batch mode, create a `${GITHUB_OUTPUT_DIR}/publish-intent.json` file:

```json
{
  "version": "1.0",
  "pr_ref": "holon-run/holon#123",
  "actions": [
    {
      "type": "create_pr|update_pr|post_comment|reply_review",
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

### Action Types

#### 1. create_pr - Create a Pull Request

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

#### 2. update_pr - Update an Existing PR

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

#### 3. post_comment - Post a PR-Level Comment

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

#### 4. reply_review - Reply to Review Comments

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

### Usage Examples

#### Issue→PR Workflow

```bash
# 1. Implement solution and create branch
git checkout -b feature/issue-503
# ... make changes ...

# 2. Write summary (main description document)
cat > ${GITHUB_OUTPUT_DIR}/summary.md <<EOF
## Summary

Implements GitHub publishing in github-solve skill

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

#### PR-Fix Workflow

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

### Direct Command Mode (Planned for Future Phases)

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

### Environment Variables

- `GITHUB_OUTPUT_DIR`: Output directory (default: `/holon/output`)
- `GITHUB_TOKEN`: GitHub authentication token (required)
- `HOLON_GITHUB_BOT_LOGIN`: Bot login name for idempotency (default: holonbot[bot])

### Options

- `--dry-run`: Show what would be done without executing
- `--from=N`: Start from action N (for resume capability)
- `--pr=OWNER/REPO#NUM`: Target PR reference (auto-detected from intent if not specified)

### Output

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

### Authentication

The publish.sh script requires `GITHUB_TOKEN` to be set:

```bash
export GITHUB_TOKEN="ghp_xxx"  # Or via container mount
```

For production use, the token should be provided via:
- Container secrets mount
- Environment variable injection
- GitHub Actions secrets

### Best Practices

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

### Troubleshooting

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

### Dependencies

The publishing system requires:
- `gh` CLI - GitHub API operations
- `jq` - JSON processing
- `git` - Version control operations (for create_pr)

## Diagnostic Confidence Levels

When diagnosing CI failures, communicate your confidence:

- **High**: Root cause is clearly identified, all evidence points to the same conclusion
- **Medium**: Root cause is likely but not 100% certain, some evidence supports diagnosis
- **Low**: Significant conflicting evidence exists (e.g., tests pass locally but fail in CI)

When confidence is **low** or fix_status is **"not-applicable"**:
1. Document all conflicting evidence
2. List alternative explanations
3. Request specific investigation
4. Consider `fix_status: "unverified"` instead of "not-applicable"

## Common Contract Rules

Use the common contract rules without modification. The common contract provides:
- Sandbox environment rules and physics
- Developer role expectations
- Output artifact requirements
- Testing and verification guidelines
