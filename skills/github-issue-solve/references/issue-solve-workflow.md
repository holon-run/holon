# Issue-Solve Workflow

Detailed workflow for solving GitHub issues and creating pull requests.

## Context

This guide applies when only issue context is detected (no PR exists yet).

## Workflow

When issue context is detected (no PR):

1. **Analyze the issue**: Read `issue.json` and `comments.json` (if present)
2. **Implement the solution**: Make code changes to address the issue
3. **Commit changes**:
   ```bash
   git checkout -b feature/issue-<number>
   git add .
   git commit -m "Feature: <brief description>"
   git push -u origin feature/issue-<number>
   ```
4. **Draft output artifacts before publish**:
   - Write an initial `${GITHUB_OUTPUT_DIR}/summary.md` (implementation/testing summary used for PR body)
   - Write `${GITHUB_OUTPUT_DIR}/manifest.json` with execution metadata
5. **Publish via direct `gh` (mandatory)**:
   ```bash
   ISSUE_NUMBER=<issue number>
   HEAD_BRANCH="$(git branch --show-current)"
   BASE_BRANCH="${BASE_BRANCH:-main}"
   PR_TITLE="Fix #${ISSUE_NUMBER}: <short title>"

   EXISTING_PR_NUMBER="$(gh pr list --head "$HEAD_BRANCH" --json number --jq '.[0].number // empty')"

   if [ -n "$EXISTING_PR_NUMBER" ]; then
     gh pr edit "$EXISTING_PR_NUMBER" --title "$PR_TITLE" --body-file "${GITHUB_OUTPUT_DIR}/summary.md" --base "$BASE_BRANCH"
     PR_NUMBER="$EXISTING_PR_NUMBER"
   else
     gh pr create --base "$BASE_BRANCH" --head "$HEAD_BRANCH" --title "$PR_TITLE" --body-file "${GITHUB_OUTPUT_DIR}/summary.md"
     PR_NUMBER="$(gh pr list --head "$HEAD_BRANCH" --json number --jq '.[0].number // empty')"
   fi

   PR_URL="$(gh pr view "$PR_NUMBER" --json url --jq .url)"
   ```
6. **Finalize outputs after publish**:
   - Update `${GITHUB_OUTPUT_DIR}/summary.md` and `${GITHUB_OUTPUT_DIR}/manifest.json`
   - Record publish result fields (`pr_number`, `pr_url`, branch/ref)
   - If publish fails, mark failure and include actionable error details

## Completion Criteria (Mandatory)

Do not mark the run successful unless a PR was actually created or updated.

- `gh` publish commands (`gh pr create`/`gh pr edit`) are mandatory for completion.
- A successful run must include publish result data (`pr_number` and/or `pr_url`) in `summary.md` and `manifest.json`.
- If publishing fails, mark the run as failed and record the actionable error details.

## Output Files

### Required Outputs

1. **`${GITHUB_OUTPUT_DIR}/summary.md`**: Human-readable summary of your analysis and actions taken
   - This will be used as the PR body

2. **`${GITHUB_OUTPUT_DIR}/manifest.json`**: Execution metadata and status

## Best Practices

- **Branch naming**: Use descriptive names like `feature/issue-<number>` or `fix/issue-<number>`
- **Commit messages**: Be concise and descriptive (e.g., "Feature: Add test coverage for skill mode")
- **PR titles**: Reference the issue (e.g., "Feature: Add non-LLM test coverage for skill mode (#520)")
- **PR body**: Include `${GITHUB_OUTPUT_DIR}/summary.md` which explains the changes
- **Testing**: Run tests before pushing to ensure the changes work
