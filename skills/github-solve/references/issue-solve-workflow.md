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
4. **Create PR**:
   ```bash
   gh pr create \
     --title "Feature: <title>" \
     --body-file ${GITHUB_OUTPUT_DIR}/summary.md \
     --base main
   ```
5. **Document**: Write `${GITHUB_OUTPUT_DIR}/summary.md` explaining what was done

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
