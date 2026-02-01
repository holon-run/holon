---
name: github-publish
description: "Shared publishing skill for GitHub operations. Provides scripts and utilities for creating PRs, posting comments, and replying to reviews with idempotency and error recovery."
---

# GitHub Publish Skill

Foundational skill providing GitHub publishing capabilities for other skills.

## Why Use This Skill?

Instead of manually calling `gh pr create` or `gh pr comment`, this skill provides:

- **Declarative Intent**: Define what to publish in JSON format
- **Idempotency**: Automatically detects and skips duplicate comments/replies
- **Batch Operations**: Create PRs, post comments, and reply to reviews in one call
- **Error Recovery**: Resume from specific operations if failures occur
- **Consistent Formatting**: Standardized markdown formatting for all published content

## Environment Variables

- **`GITHUB_OUTPUT_DIR`**: Directory containing artifacts to publish
  - Default: `/holon/output` when the path exists; otherwise a temp dir under `/tmp/holon-ghpub-*`
- **`GITHUB_TOKEN` / `GH_TOKEN`**: GitHub authentication token (required). Scopes: `repo` for private repos (or `public_repo` for public).
- **`HOLON_GITHUB_BOT_LOGIN`**: Bot login name for idempotency checks
  - Default: `holonbot[bot]`
  - Purpose: Skip bot's own comments/replies when checking for duplicates
- **`DRY_RUN`**: Preview operations without actually publishing (default: false)

## Usage

### Declarative Publishing

Create a `publish-intent.json` file describing what to publish:

```json
{
  "actions": [
    {
      "type": "create_pr",
      "base": "main",
      "head": "feature/new-feature",
      "title": "Add new feature",
      "body": "Description of changes",
      "draft": false
    },
    {
      "type": "post_comment",
      "target": "holon-run/holon#123",
      "body": "Comment content"
    },
    {
      "type": "reply_review",
      "pr": "holon-run/holon#123",
      "comment_id": 456,
      "body": "Reply to review comment"
    }
  ]
}
```

Then run the publish script:

```bash
scripts/publish.sh --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
```

### Direct Script Usage

```bash
# Preview without publishing
DRY_RUN=true scripts/publish.sh --intent=publish-intent.json

# Publish with custom bot login
HOLON_GITHUB_BOT_LOGIN=mybot[bot] scripts/publish.sh --intent=publish-intent.json
```

## Action Types

### create_pr
Creates a new pull request.

**Required fields:**
- `base`: Target branch
- `head`: Source branch
- `title`: PR title
- `body`: PR description

**Optional fields:**
- `draft`: Create as draft PR (default: false)

### post_comment
Posts a comment on an issue or PR.

**Required fields:**
- `target`: Issue/PR reference (e.g., `owner/repo#123`)
- `body`: Comment content

### reply_review
Replies to a PR review comment.

**Required fields:**
- `pr`: PR reference (e.g., `owner/repo#123`)
- `comment_id`: Review comment ID
- `body`: Reply content

## Output Contract

The publish script generates:

- **`${GITHUB_OUTPUT_DIR}/publish-result.json`**: Results of all publish operations
  ```json
  {
    "actions": [
      {
        "type": "create_pr",
        "status": "success|skipped|failed",
        "pr_number": 123,
        "pr_url": "https://github.com/owner/repo/pull/123",
        "message": "Created PR #123"
      }
    ]
  }
  ```

## Idempotency

The publish script implements idempotency checks:

- **Comments**: Checks if bot has already posted identical comment
- **Review replies**: Checks if bot has already replied to the review comment
- **PRs**: Checks if PR already exists for the branch

Duplicate operations are skipped with status `"skipped"`.

## Error Recovery

If publishing fails partway through:

1. Check `publish-result.json` to see which actions succeeded
2. Remove successful actions from `publish-intent.json`
3. Re-run the publish script

## Integration

This skill is used by:
- `github-issue-solve`: Creating PRs from issues
- `github-pr-fix`: Replying to review comments
- Other skills that need to publish to GitHub

## Reference Documentation

See [references/github-publishing.md](references/github-publishing.md) for detailed publishing guide including:
- Workflow and responsibilities (Agent vs Script)
- Complete action type specifications
- Usage examples
- Best practices and troubleshooting
