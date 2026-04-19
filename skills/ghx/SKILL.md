---
name: ghx
description: "Guidance for safe, reliable GitHub CLI workflows across issues, pull requests, and reviews."
---

# GHX Skill

## Summary

Use this skill as shared guidance for raw `gh` and `gh api` usage in GitHub issue, pull request, and review workflows.
It focuses on safe command patterns, payload handling, and common collection/publish recipes.

This skill is guidance-only. It does not provide scripts, wrappers, or executable entrypoints.

## When To Use

- Collecting issue or PR context directly from GitHub with `gh`
- Publishing PR bodies, issue comments, or review replies safely
- Looking for reusable GitHub CLI patterns without depending on repo-local scripts
- Standardizing GitHub CLI safety rules across multiple skills

## Do Not Use

- As a command or executable tool
- When a task needs Holon runtime internals rather than GitHub CLI guidance
- As a replacement for the task-specific logic in `github-review`, `github-pr-fix`, or `github-issue-solve`

## Prerequisites

- `gh` CLI authentication is required.
- `GITHUB_TOKEN` or `GH_TOKEN` must have the scopes required by the target repository operations.
- `jq` is recommended when you need structured filtering or JSON post-processing.

## Safety Rules

### Payload handling

1. Never inline large markdown or JSON payloads on the command line.
2. Always pass long bodies through files:
   - `gh issue create --body-file <file>`
   - `gh issue comment --body-file <file>`
   - `gh pr create --body-file <file>`
   - `gh pr edit --body-file <file>`
3. Use `--body-file -` only when stdin is clearly available and intentional.

Good:

```bash
cat > /tmp/comment.md <<'EOF'
## Summary
Detailed markdown with quotes, backticks, and newlines.
EOF
gh issue comment 123 --repo owner/repo --body-file /tmp/comment.md
```

Bad:

```bash
gh issue comment 123 --repo owner/repo --body "## Summary
Detailed markdown with quotes, backticks, and newlines."
```

### Raw JSON payloads

- Prefer `gh api --input <file>` for complex request bodies.
- When passing many scalar fields, prefer repeated `-f key=value` flags over hand-built shell strings.

## Common Recipes

### Collect issue context

```bash
gh issue view 123 --repo owner/repo --json number,title,body,state,url,author,createdAt,updatedAt,labels
gh api repos/owner/repo/issues/123/comments --paginate
```

### Collect PR context

```bash
gh pr view 123 --repo owner/repo --json number,title,body,state,url,baseRefName,headRefName,headRefOid,author,createdAt,updatedAt,mergeable,reviews,changedFiles,additions,deletions
gh pr view 123 --repo owner/repo --json files
gh pr diff 123 --repo owner/repo
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
  }' -F owner=owner -F repo=repo -F number=123
```

### Create or update a PR

```bash
gh pr create --repo owner/repo --title "Title" --body-file /tmp/pr-body.md --head feature/x --base main
gh pr edit 123 --repo owner/repo --title "Updated title" --body-file /tmp/pr-body.md
```

### Post a PR-level comment

```bash
gh api repos/owner/repo/issues/123/comments -X POST --input /tmp/comment.json
```

Example payload:

```json
{
  "body": "Comment body"
}
```

### Reply to a review comment

```bash
gh api repos/owner/repo/pulls/123/comments -X POST \
  -F in_reply_to=456789 \
  -f body='Thanks, fixed in the latest commit.'
```

### Post a review with inline comments

Use `gh api` with a JSON payload file so body text and inline comments stay structured and reproducible.

## Notes

- Prefer explicit repository targeting via `--repo owner/repo` in automation.
- Prefer machine-readable `--json` output when subsequent steps need structured data.
- When collecting many records, remember `--paginate` for REST endpoints.
