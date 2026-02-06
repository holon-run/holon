---
name: github-issue-solve
description: "Solve GitHub issues by implementing features or fixes and creating pull requests. Use when: (1) Implementing features described in issues, (2) Fixing bugs reported in issues, (3) Creating PRs from issue requirements."
---

# GitHub Issue Solve Skill

Automation skill for solving GitHub issues by implementing solutions and creating pull requests.

## Purpose

This skill helps you:
1. Analyze GitHub issues and understand requirements
2. Implement features or fixes in the codebase
3. Create feature branches and commit changes
4. Create pull requests with proper descriptions

## Prerequisites

This skill depends on (co-installed and callable by the agent):
- **`github-context`**: Agent should invoke it to collect issue metadata and comments when context is missing
- **`github-publish`**: Agent should invoke it to publish PRs from the produced artifacts

## Environment & Paths

- **`GITHUB_OUTPUT_DIR`**: Where this skill writes artifacts  
  - Default: `/holon/output` if present; otherwise a temp dir `/tmp/holon-ghissue-*`
- **`GITHUB_CONTEXT_DIR`**: Where `github-context` writes collected data  
  - Default: `${GITHUB_OUTPUT_DIR}/github-context`
- **`GITHUB_TOKEN` / `GH_TOKEN`**: Token used for GitHub operations (scopes: `repo` or `public_repo`)

## Inputs & Outputs

- **Inputs** (agent should obtain via `github-context`): `${GITHUB_CONTEXT_DIR}/github/issue.json`, `comments.json`
- **Outputs** (agent writes under `${GITHUB_OUTPUT_DIR}`):
  - `summary.md`
  - `manifest.json`
  - Optional `publish-intent.json` for `github-publish`

## Workflow

### 1. Context Collection

If context is not pre-populated, call the `github-context` skill’s collector with the issue reference. After collection, context is under `${GITHUB_CONTEXT_DIR}/github/`.

### 2. Analyze Issue

Read the collected context:
- `${GITHUB_CONTEXT_DIR}/github/issue.json`: Issue metadata (title, body, labels, assignees)
- `${GITHUB_CONTEXT_DIR}/github/comments.json`: Discussion comments

Understand:
- What feature or fix is requested
- Any specific requirements or constraints
- Related issues or PRs mentioned

### 3. Implement Solution

Create a feature branch and implement the solution:

```bash
# Create feature branch
git checkout -b feature/issue-<number>

# Make your changes to the codebase
# ... implement the feature or fix ...

# Commit changes
git add .
git commit -m "Feature: <description>"

# Push to remote
git push -u origin feature/issue-<number>
```

### 4. Generate Artifacts

Create the required output files:

#### `${GITHUB_OUTPUT_DIR}/summary.md`

Human-readable summary of your work:
- Issue reference and description
- What was implemented
- Key changes made
- Testing performed

#### `${GITHUB_OUTPUT_DIR}/manifest.json`

Execution metadata:
```json
{
  "provider": "github-issue-solve",
  "issue_ref": "holon-run/holon#502",
  "branch": "feature/issue-502",
  "status": "completed|failed",
  "commits": ["abc123", "def456"]
}
```

### 5. Create Pull Request

Produce `${GITHUB_OUTPUT_DIR}/publish-intent.json` and invoke the `github-publish` skill to create the PR:
```json
{
  "actions": [
    {
      "type": "create_pr",
      "base": "main",
      "head": "feature/issue-502",
      "title": "Fix: <issue title>",
      "body": "Closes #502\n\n<description of changes>",
      "draft": false
    }
  ]
}
```

Run `github-publish` with this intent file (scripts/publish.sh in that skill bundle).

## Output Contract

### Required Outputs

1. **`${GITHUB_OUTPUT_DIR}/summary.md`**: Human-readable summary
   - Issue reference and description
   - Implementation details
   - Changes made
   - Testing performed

2. **`${GITHUB_OUTPUT_DIR}/manifest.json`**: Execution metadata
   ```json
   {
     "provider": "github-issue-solve",
     "issue_ref": "holon-run/holon#502",
     "branch": "feature/issue-502",
     "status": "completed|failed",
     "commits": ["abc123"],
     "pr_number": 123
   }
   ```

### Optional Outputs

3. **`${GITHUB_OUTPUT_DIR}/publish-intent.json`**: PR creation intent (for `github-publish`)

## Git Operations

You are responsible for all git operations:

```bash
# Create feature branch
git checkout -b feature/issue-<number>

# Stage changes
git add .

# Commit with descriptive message
git commit -m "Feature: <description>"

# Push to remote
git push -u origin feature/issue-<number>
```

## GitHub CLI Operations

You MAY use these commands:
- `gh issue view <number>` - View issue details
- `gh issue comment <number>` - Comment on issues
- `gh pr create` - Create pull requests (or use `github-publish` skill)

## Important Notes

- You are running **HEADLESSLY** - do not wait for user input or confirmation
- Create feature branches following the pattern `feature/issue-<number>` or `fix/issue-<number>`
- Write clear commit messages describing what was changed
- Include "Closes #<number>" in PR body to auto-link the issue
- Run tests if available before creating the PR

## Reference Documentation

See [references/issue-solve-workflow.md](references/issue-solve-workflow.md) for detailed workflow guide.
