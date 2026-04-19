# Issue-Solve Workflow

Detailed execution workflow for `github-issue-solve`.

## 1) Context intake

1. If `${GITHUB_CONTEXT_DIR}/manifest.json` exists, read it first.
2. When a manifest exists:
   - confirm `kind=issue` and `success=true`
   - locate available artifacts from `manifest.artifacts[]`
   - build analysis context only from artifacts with `status=present`
3. If no manifest exists, collect context directly:
   - `gh issue view <issue_number> --repo <owner/repo> --json ...`
   - `gh api repos/<owner>/<repo>/issues/<issue_number>/comments --paginate`

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

Publish with direct `gh` commands:
- `gh pr create --body-file`
- `gh pr edit --body-file`

After publish, verify PR identity:
- `pr_number`
- `pr_url`

## 5) Output finalization

Write/update:
- `${GITHUB_OUTPUT_DIR}/summary.md`
- `${GITHUB_OUTPUT_DIR}/manifest.json`

## Completion criteria

The run is complete only when:
1. Code changes are implemented for issue intent.
2. Changes are committed and pushed.
3. A PR was created or updated and can be verified (`pr_number`, `pr_url`).
4. Outputs include verification details and final status.
