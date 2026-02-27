# GHX Publishing Guide

`ghx` provides a unified publishing surface for GitHub operations.

Boundary:
- External skills should call `ghx` capability commands and rely on `publish-results.json`.
- `publish-batch.json` is a public batch schema for `ghx.sh batch run`.

## Modes

1. Batch mode (multiple actions)

```bash
scripts/ghx.sh batch run --batch=${GITHUB_OUTPUT_DIR}/publish-batch.json
```

2. Direct mode (single action)

```bash
scripts/ghx.sh pr comment --pr=owner/repo#123 --body-file=summary.md
scripts/ghx.sh review publish --pr=owner/repo#123 --body-file=review.md --comments-file=review.json
```

## Text Payload Safety (Required)

When publishing markdown/json content, do not inline large text directly in shell arguments.
Always write payloads to files first, then pass file flags.

Required patterns:
- `ghx`: `--body-file`, `--comments-file`
- raw `gh`: `--body-file`
- `--body-file=-` is supported when you want to stream body content from stdin.

Reason:
- Avoid shell escaping/newline truncation/backtick interpolation errors.
- Improve reproducibility and debugging (payload can be inspected as a file).

Example:

```bash
cat > /tmp/review.md <<'EOF'
## Review Summary
Multi-line content with markdown/code fences.
EOF
scripts/ghx.sh pr comment --pr=owner/repo#123 --body-file=/tmp/review.md
```

Stdin example (no temp file):

```bash
scripts/ghx.sh pr comment --pr=owner/repo#123 --body-file=- <<'EOF'
## Review Summary
Multiline content streamed from stdin.
EOF
```

Mode selection:
- Use direct mode for one publish action.
- Use batch mode when one run needs multiple publish actions.

## Batch Schema (Public)

`publish-batch.json`:

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

Notes:
- `version` is optional; default is `1.0`.
- Action parameters are read from `params`, and legacy inline action fields are also accepted.

Top-level fields:
- `version` (optional): schema version, currently `1.0`
- `pr_ref` (required): `owner/repo#number`
- `actions` (required): ordered list of actions

Action fields:
- `type` (required): action type
- `description` (optional): human-readable note
- `params` (optional): action payload object

Supported action types:
- `create_pr`
- `update_pr`
- `post_comment`
- `reply_review`
- `post_review`

Third-party skill guidance:
- You can use this batch schema directly when needed.
- For single actions, prefer direct commands instead of batch files.
- Do not depend on undocumented fields outside this schema.

## Output

Execution writes `${GITHUB_OUTPUT_DIR}/publish-results.json`:
- per-action status
- summary totals
- overall status

## Notes

- `gh` and `jq` are required.
- `gh auth status` must pass.
- Use `DRY_RUN=true` for preview mode.
