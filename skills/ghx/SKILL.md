---
name: ghx
description: "GitHub enhanced operations skill. Provides unified context collection and publishing commands for PR/issue workflows."
---

# GHX Skill

`ghx` is the shared GitHub operations layer used by higher-level skills.

It combines two capabilities:
- Context collection (issue/PR metadata, comments, diffs, threads, checks)
- Publishing (PR creation/updates, comments, review replies, inline review posting)

## Environment Variables

- `GITHUB_OUTPUT_DIR`: artifact directory (default `/holon/output` if available)
- `GITHUB_CONTEXT_DIR`: context directory (default `${GITHUB_OUTPUT_DIR}/github-context`)
- `GITHUB_TOKEN` / `GH_TOKEN`: GitHub token for `gh`
- `HOLON_GITHUB_BOT_LOGIN`: bot login for idempotency checks (default `holonbot[bot]`)
- `DRY_RUN`: preview mode (`true|false`)

## Commands

Use `scripts/ghx.sh` as the primary entrypoint.

### Context

```bash
scripts/ghx.sh context collect holon-run/holon#123
scripts/ghx.sh context collect 123 holon-run/holon
```

### Publish from intent

```bash
scripts/ghx.sh intent run --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
```

### Direct publish commands

```bash
scripts/ghx.sh review publish --pr=holon-run/holon#123 --body-file=review.md --comments-file=review.json
scripts/ghx.sh pr create --repo=holon-run/holon --title="Title" --body-file=summary.md --head=feature/x --base=main
scripts/ghx.sh pr update --pr=holon-run/holon#123 --title="New title" --body-file=summary.md
scripts/ghx.sh pr comment --pr=holon-run/holon#123 --body-file=summary.md
scripts/ghx.sh pr reply-reviews --pr=holon-run/holon#123 --pr-fix-json=pr-fix.json
```

## Output Contract

### Context artifacts

Under `${GITHUB_CONTEXT_DIR}`:
- `github/pr.json` or `github/issue.json`
- `github/comments.json`
- Optional: `github/pr.diff`, `github/files.json`, `github/review_threads.json`, `github/check_runs.json`, `github/commits.json`
- `manifest.json`

### Publish artifacts

Under `${GITHUB_OUTPUT_DIR}`:
- `publish-results.json`

## Notes

- `ghx` is capability-oriented: upper-level skills should focus on final outputs and outcomes.
- Prefer `ghx` commands for multi-step or error-prone GitHub operations.
