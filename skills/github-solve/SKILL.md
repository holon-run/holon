---
name: github-solve
description: GitHub issue and PR workflow automation for collecting context, fixing issues, and publishing changes. Use when working with GitHub issues, pull requests, or code reviews that require: (1) Analyzing and fixing PR review comments, (2) Creating PRs from issues, (3) Publishing changes via GitHub API, (4) Collecting GitHub context (issues, PRs, reviews, CI logs)
---

# GitHub Solve Skill

Automation skill for GitHub issue and pull request workflows.

## Environment and Paths

This skill uses environment variables to stay portable across Holon, local shells, and CI.

### Key Environment Variables

- **`GITHUB_OUTPUT_DIR`**: Output directory for artifacts and publish results
  - **Default**: `/holon/output` when the path exists (Holon container); otherwise a temp dir under `/tmp/holon-ghout-*`
  - **Custom**: Set to any directory (e.g., `./output`, `/tmp/github-work`)

- **`GITHUB_CONTEXT_DIR`**: Directory for collected GitHub context
  - **Default**: `${GITHUB_OUTPUT_DIR}/github-context` if `/holon/output` exists; otherwise a temp dir under `/tmp/holon-ghctx-*`

- **`GITHUB_TOKEN` / `GH_TOKEN`**: GitHub authentication token
  - Required for publishing; also used for collection if gh auth is not already logged in

- **`HOLON_GITHUB_BOT_LOGIN`**: Bot login name for idempotency checks
  - **Default**: `holonbot[bot]`
  - **Purpose**: Skip bot's own comments/replies when checking for duplicates

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
```

**Note**: This documentation uses `${GITHUB_OUTPUT_DIR}` as a placeholder.

## Minimal Input Payload

When no pre-populated GitHub context is available (i.e., `/holon/input/context/github/` is empty or missing), you **MUST** first collect context using the skill's built-in collection script.

The minimal input payload required is:
- **`/holon/input/payload.json`** (optional): Contains task metadata with GitHub reference
  ```json
  {
    "ref": "holon-run/holon#502",
    "repo": "holon-run/holon",
    "type": "issue|pr",
    "trigger_comment_id": 123456
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
   # Optional overrides; otherwise defaults to /holon/output/github-context or /tmp/holon-ghctx-*
   export GITHUB_CONTEXT_DIR=${GITHUB_OUTPUT_DIR}/github-context
   export TRIGGER_COMMENT_ID=123456
   export INCLUDE_DIFF=true          # for PRs
   export INCLUDE_CHECKS=true        # for PRs
   # UNRESOLVED_ONLY is deprecated (GitHub API lacks unresolved state on /pulls/{n}/comments)
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
  - Requires `gh` CLI to be authenticated
  - Requires `jq` for JSON processing
  - Usage: `collect.sh <ref> [repo_hint]`

- **`scripts/lib/helpers.sh`**: Reusable helper functions
  - `check_gh_cli()`, `check_jq()`: Verify dependencies
  - `parse_ref()`: Parse GitHub reference into owner/repo/number
  - `determine_ref_type()`: Check if a number is a PR or issue
  - `fetch_*`: Various fetch functions for GitHub data
  - `verify_context_files()`: Validate required files exist
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
    - `--from=N`: Start from reply N
    - `--pr=OWNER/REPO#NUMBER`: Target PR
    - `--bot-login=NAME`: Bot login name for idempotency

### GitHub Publishing

- **`scripts/publish.sh`**: Unified publishing script
  - Supports batch mode (declarative intent file)
  - Creates PRs, posts comments, replies to reviews
  - Handles idempotency and error recovery
  - Usage: `publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json`

For detailed GitHub publishing guide, see [references/github-publishing.md](references/github-publishing.md).

## Context Detection

This skill adapts behavior based on the GitHub context provided:

- **PR Context** (`/holon/input/context/github/pr.json` exists):
  - PR-fix mode: Analyze PR feedback, review threads, and CI failures
  - Generate structured responses to make the PR mergeable
  - See [references/pr-fix-workflow.md](references/pr-fix-workflow.md) for detailed workflow

- **Issue Context** (only `/holon/input/context/github/issue.json` exists):
  - Issue-solve mode: Implement the feature or fix described in the issue
  - Create a pull request with the changes
  - See [references/issue-solve-workflow.md](references/issue-solve-workflow.md) for detailed workflow

## GitHub Context Files

When GitHub context is provided (either pre-populated or collected), files are available under `/holon/input/context/github/`:

### PR Context Files
- `pr.json`: Pull request metadata including reviews
- `review_threads.json`: Review threads with line-specific comments (includes `comment_id`)
- `comments.json`: PR discussion comments
- `pr.diff`: The code changes being reviewed
- `check_runs.json`: CI/check run metadata
- `test-failure-logs.txt`: Complete workflow logs for failed tests

### Issue Context Files
- `issue.json`: Issue metadata
- `comments.json`: Issue comments

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
- `gh pr create` - Create pull requests
- `gh api` - Make API calls

You are responsible for **all git operations**:
- `git checkout -b` - Create feature branches
- `git add` / `git commit` - Commit changes
- `git push` - Push branches to remote

For detailed workflows including git operations, see [references/issue-solve-workflow.md](references/issue-solve-workflow.md) or [references/pr-fix-workflow.md](references/pr-fix-workflow.md).

## Output Contract

### Required Outputs

1. **`${GITHUB_OUTPUT_DIR}/summary.md`**: Human-readable summary of your analysis and actions taken

2. **`${GITHUB_OUTPUT_DIR}/manifest.json`**: Execution metadata and status

### Conditional Outputs

#### For PR Context (PR-fix mode)

3. **`${GITHUB_OUTPUT_DIR}/pr-fix.json`**: Structured JSON containing fix status and responses
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

**Note**: For PR-fix mode, you should **push your changes to the PR branch** directly (not create a new PR).

## Reference Documentation

Detailed workflow guides and best practices are available in the `references/` directory:

- **[pr-fix-workflow.md](references/pr-fix-workflow.md)**: Complete workflow for fixing issues in pull requests
  - Error triage and priority order
  - Environment setup and verification requirements
  - Test failure diagnosis
  - Handling non-blocking refactor requests
  - Posting review replies

- **[issue-solve-workflow.md](references/issue-solve-workflow.md)**: Complete workflow for solving GitHub issues
  - Issue analysis and solution implementation
  - Output file formats
  - PR creation and publishing

- **[github-publishing.md](references/github-publishing.md)**: Complete guide for GitHub publishing
  - Workflow and responsibilities (Agent vs Script)
  - Action types (create_pr, update_pr, post_comment, reply_review)
  - Usage examples for Issue→PR and PR-fix workflows
  - Environment variables, options, and output format
  - Best practices and troubleshooting

- **[diagnostics.md](references/diagnostics.md)**: Diagnostic confidence levels and best practices
  - Confidence levels for CI failure diagnosis
  - Common contract rules

## Workflow Quick Reference

### PR-Fix Mode

1. Analyze PR feedback, review threads, and CI failures
2. Identify and fix errors in priority order (build → test → import → lint)
3. Commit and push changes to the PR branch:
   ```bash
   git add .
   git commit -m "Fix: <description>"
   git push
   ```
4. Generate `${GITHUB_OUTPUT_DIR}/pr-fix.json` with fix status and responses
5. Use `scripts/reply-reviews.sh` to post replies
6. For full workflow details, see [references/pr-fix-workflow.md](references/pr-fix-workflow.md)

**IMPORTANT**: Commit your code fixes BEFORE replying to reviews. This ensures reviewers can see your actual fixes when reading your replies.

### Issue-Solve Mode

1. Collect context (if not pre-populated): Run `scripts/collect.sh "<ref>"`
2. Analyze the issue and implement solution
3. Create feature branch and commit changes:
   ```bash
   git checkout -b feature/issue-<number>
   git add .
   git commit -m "Feature: <description>"
   git push -u origin feature/issue-<number>
   ```
4. Generate `${GITHUB_OUTPUT_DIR}/summary.md` with explanation
5. Create PR using `gh pr create`
6. For full workflow details, see [references/issue-solve-workflow.md](references/issue-solve-workflow.md)
