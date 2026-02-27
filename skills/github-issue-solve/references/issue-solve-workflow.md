# Issue-Solve Workflow

Detailed execution workflow for `github-issue-solve`.

## 1) Context intake (manifest-first)

1. Read `${GITHUB_CONTEXT_DIR}/manifest.json`.
2. Confirm `kind=issue` and `success=true`.
3. Locate available artifacts from `manifest.artifacts[]`:
   - preferred ids: `issue_metadata`, `comments`
4. Build analysis context only from artifacts with `status=present`.

If required context is missing, record explicit limitations in `summary.md`.

## 2) Solution planning

1. Extract requested outcome and constraints from issue content.
2. Convert into an implementation checklist.
3. Choose the smallest change set that satisfies acceptance criteria.

## 3) Implementation and verification

1. Create/switch branch (`feature/issue-<number>` or `fix/issue-<number>`).
2. Implement code changes.
3. Run relevant validation commands.
4. Commit and push.

Never claim success without commit and push.

## 4) PR publish

Preferred publish path:
- `ghx` PR commands (`pr create` / `pr update`) with `--body-file`.

Fallback path:
- direct `gh pr create` / `gh pr edit`.

After publish, verify PR identity:
- `pr_number`
- `pr_url`

## 5) Output finalization

Write/update:
- `${GITHUB_OUTPUT_DIR}/summary.md`
- `${GITHUB_OUTPUT_DIR}/manifest.json`

When `ghx` publish is used, keep `${GITHUB_OUTPUT_DIR}/publish-results.json` for audit/debug.

## Completion criteria

The run is complete only when:
1. Code changes are implemented for issue intent.
2. Changes are committed and pushed.
3. A PR was created or updated and can be verified (`pr_number`, `pr_url`).
4. Outputs include verification details and final status.
