---
name: ghx
description: "GitHub enhanced operations skill. Provides unified context collection and publishing commands for PR/issue workflows."
---

# GHX Skill

`ghx` is the shared GitHub operations layer used by higher-level skills.

It provides two capabilities:
- Context collection with a stable artifact manifest
- Publishing for PR/review/comment workflows

## Environment Variables

- `GITHUB_OUTPUT_DIR`: artifact directory (caller-provided; defaults to a temp dir when unset)
- `GITHUB_CONTEXT_DIR`: context directory (default `${GITHUB_OUTPUT_DIR}/github-context`)
- `GITHUB_TOKEN` / `GH_TOKEN`: token used by `gh`
- `DRY_RUN`: preview mode for publish commands (`true|false`)

## Commands

Use `scripts/ghx.sh` as the entrypoint.

### Context collection

```bash
scripts/ghx.sh context collect holon-run/holon#123
scripts/ghx.sh context collect 123 holon-run/holon
```

### Publish commands

```bash
scripts/ghx.sh review publish --pr=holon-run/holon#123 --body-file=review.md --comments-file=review.json
scripts/ghx.sh pr create --repo=holon-run/holon --title="Title" --body-file=summary.md --head=feature/x --base=main
scripts/ghx.sh pr update --pr=holon-run/holon#123 --title="New title" --body-file=summary.md
scripts/ghx.sh pr comment --pr=holon-run/holon#123 --body-file=summary.md
```

### Intent mode (ghx internal)

```bash
scripts/ghx.sh intent run --intent=${GITHUB_OUTPUT_DIR}/publish-intent.json
```

External skills should prefer direct capability commands and avoid coupling to `publish-intent.json` internals.

## Contract Boundary

- Public contract for other skills:
  - Call `ghx.sh` commands.
  - Read `${GITHUB_CONTEXT_DIR}/manifest.json` for context artifacts.
  - Read `${GITHUB_OUTPUT_DIR}/publish-results.json` for publish execution results.
- Internal contract inside ghx:
  - `publish-intent.json` action schema and internal script layout may evolve.

## Context Output Contract

`context collect` writes:
- `${GITHUB_CONTEXT_DIR}/manifest.json`
- Context files under `${GITHUB_CONTEXT_DIR}/github/` (for example `pr.json`, `pr.diff`, `comments.json`)

`manifest.json` is the source of truth for collected context:

```json
{
  "schema_version": "2.0",
  "provider": "ghx",
  "kind": "pr|issue",
  "ref": "owner/repo#123",
  "success": true,
  "artifacts": [
    {
      "id": "pr_metadata",
      "path": "github/pr.json",
      "required_for": ["review"],
      "status": "present|missing|error",
      "format": "json|text",
      "description": "What this artifact contains"
    }
  ],
  "notes": []
}
```

Consumers must use `artifacts[]` instead of assuming fixed context filenames.

## Publish Output Contract

Publish commands write `${GITHUB_OUTPUT_DIR}/publish-results.json` with per-action status and summary totals.

## Notes

- Prefer `ghx` for multi-step or error-prone GitHub operations.
- `context collect` also prints a human-readable artifact summary; this is informational only.
