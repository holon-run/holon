# PR-Fix Workflow

Detailed execution workflow for `github-pr-fix`.

## 1) Context intake (manifest-first)

1. Read `${GITHUB_CONTEXT_DIR}/manifest.json`.
2. Confirm `kind=pr` and collection status.
3. Resolve available artifacts by id/path/status, typically:
   - `pr_metadata`
   - `review_threads`
   - `comments`
   - `check_runs`
   - `diff`
   - `commits`

Use only artifacts marked `status=present`.  
If critical artifacts are missing, document limitations before remediation.

## 2) Triage and prioritization

Identify all actionable problems, then fix in order:
1. build/compile blockers
2. failing tests/regressions
3. type/import/module issues
4. lint/style issues

Treat large non-blocking refactors as defer candidates.

## 3) Fix and verify

1. Apply targeted code fixes.
2. Run relevant verification commands.
3. Commit and push fixes to the existing PR branch.

Do not publish replies before push completes.

## 4) Publish replies

Preferred:
- Use `ghx` publish commands for review-thread replies and related comments.

Fallback:
- Use direct `gh api` reply operations only when `ghx` publish is unavailable.

Always capture per-action status in `${GITHUB_OUTPUT_DIR}/publish-results.json`.

## 5) Finalize outputs

Write/update:
- `${GITHUB_OUTPUT_DIR}/summary.md`
- `${GITHUB_OUTPUT_DIR}/manifest.json`
- `${GITHUB_OUTPUT_DIR}/publish-results.json`

## Completion criteria

Run is successful only when:
1. Required fixes are committed and pushed.
2. Replies planned for this run are published.
3. `publish-results.json` contains no failed required reply actions.
