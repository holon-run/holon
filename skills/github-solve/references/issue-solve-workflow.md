# Issue-Solve Workflow

Detailed workflow for solving GitHub issues and creating pull requests.

## Context

This guide applies when only issue context is detected (no PR exists yet).

## Workflow

When issue context is detected (no PR):

1. **Analyze the issue**: Read `issue.json` and `comments.json` (if present)
2. **Implement the solution**: Make code changes to address the issue
3. **Generate diff**: Create `${GITHUB_OUTPUT_DIR}/diff.patch` with your changes
4. **Document**: Write `${GITHUB_OUTPUT_DIR}/summary.md` explaining what was done
5. **PR metadata** (optional): Create `${GITHUB_OUTPUT_DIR}/pr-fix.json` with PR title, body, and branch name

## Output Files

### Required Outputs

1. **`${GITHUB_OUTPUT_DIR}/summary.md`**: Human-readable summary of your analysis and actions taken

2. **`${GITHUB_OUTPUT_DIR}/manifest.json`**: Execution metadata and status

### Optional Outputs (for PR creation)

3. **`${GITHUB_OUTPUT_DIR}/diff.patch`**: Git-compatible patch with code changes

4. **`${GITHUB_OUTPUT_DIR}/pr-fix.json`**: PR creation metadata
   ```json
   {
     "title": "PR title",
     "body": "PR body (references issue)",
     "branch": "suggested-branch-name"
   }
   ```

## Publishing

For creating PRs and publishing changes, see [github-publishing.md](github-publishing.md).
