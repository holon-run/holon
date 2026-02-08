# GHX Context Collection

Collect GitHub context into `${GITHUB_CONTEXT_DIR}` (default `${GITHUB_OUTPUT_DIR}/github-context`).

## Command

```bash
scripts/ghx.sh context collect <ref> [repo_hint]
```

`ref` supports:
- `OWNER/REPO#NUMBER`
- GitHub issue/PR URL
- numeric ID with `repo_hint`

## Outputs

- `${GITHUB_CONTEXT_DIR}/github/pr.json` for PR refs
- `${GITHUB_CONTEXT_DIR}/github/issue.json` for issue refs
- `${GITHUB_CONTEXT_DIR}/github/comments.json`
- `${GITHUB_CONTEXT_DIR}/manifest.json`
- Optional PR extras based on env toggles (`INCLUDE_DIFF`, `INCLUDE_CHECKS`, `INCLUDE_THREADS`, `INCLUDE_FILES`, `INCLUDE_COMMITS`)
