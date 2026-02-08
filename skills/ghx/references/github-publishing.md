# GHX Publishing Guide

`ghx` provides a unified publishing surface for GitHub operations.

## Modes

1. Intent mode (batch actions)

```bash
scripts/ghx.sh intent run --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
```

2. Direct mode (single action)

```bash
scripts/ghx.sh pr comment --pr=owner/repo#123 --body-file=summary.md
scripts/ghx.sh pr reply-reviews --pr=owner/repo#123 --pr-fix-json=pr-fix.json
scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json
```

## Intent Schema

`publish-intent.json`:

```json
{
  "version": "1.0",
  "pr_ref": "owner/repo#123",
  "actions": [
    {
      "type": "post_comment",
      "description": "optional",
      "params": {
        "body": "summary.md"
      }
    }
  ]
}
```

Supported action types:
- `create_pr`
- `update_pr`
- `post_comment`
- `reply_review`
- `post_review`

## Output

Execution writes `${GITHUB_OUTPUT_DIR}/publish-results.json`:
- per-action status
- summary totals
- overall status

## Notes

- `gh` and `jq` are required.
- `gh auth status` must pass.
- Use `DRY_RUN=true` for preview mode.
