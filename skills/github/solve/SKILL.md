# SKILL: github/solve

## Context Detection

This skill adapts behavior based on the GitHub context provided:

- **PR Context** (`/holon/input/context/github/pr.json` exists):
  - PR-fix mode: Analyze PR feedback, review threads, and CI failures
  - Generate structured responses to make the PR mergeable

- **Issue Context** (only `/holon/input/context/github/issue.json` exists):
  - Issue-solve mode: Implement the feature or fix described in the issue
  - Create a pull request with the changes

## GitHub Context Files

When GitHub context is provided, files are available under `/holon/input/context/github/`:

### PR Context Files
- `pr.json`: Pull request metadata
- `review_threads.json`: Review threads with comment metadata (optional, includes `comment_id`)
- `pr.diff`: The code changes being reviewed (optional but recommended)
- `check_runs.json`: CI/check run metadata (optional)
- `test-failure-logs.txt`: Complete workflow logs for failed tests (optional, downloaded when checks fail)

### Issue Context Files
- `issue.json`: Issue metadata
- `comments.json`: Issue comments (optional)

## Important Notes

- When responding to review comments, use your GitHub identity (from common contract) to avoid replying to your own comments
- You are running **HEADLESSLY** - do not wait for user input or confirmation

## Capabilities

You MAY use these commands directly via `gh` CLI:
- `gh issue view` - View issue details
- `gh issue comment` - Comment on issues
- `gh pr view` - View PR details
- `gh pr comment` - Comment on PRs
- `gh issue edit` - Edit issue metadata (labels, assignees)
- `gh api` - Make read API calls

You MUST NOT use these directly (use artifacts instead):
- `git push` - Code pushing must go through artifacts
- `gh pr create` - PR creation must go through artifacts
- `gh pr merge` - PR merging must go through artifacts

## Output Contract

### Required Outputs

1. **`/holon/output/summary.md`**: Human-readable summary of your analysis and actions taken

2. **`/holon/output/manifest.json`**: Execution metadata and status

### Conditional Outputs

#### For PR Context (PR-fix mode)

3. **`/holon/output/pr-fix.json`**: Structured JSON containing fix status and responses
   - Format:
     ```json
     {
       "review_replies": [
         {
           "comment_id": 123,
           "status": "fixed|wontfix|need-info|deferred",
           "message": "Response to reviewer",
           "action_taken": "Description of code changes"
         }
       ],
       "follow_up_issues": [
         {
           "title": "Issue title",
           "body": "Issue body in Markdown",
           "deferred_comment_ids": [123],
           "labels": ["enhancement", "testing"]
         }
       ],
       "checks": [
         {
           "name": "ci/test",
           "conclusion": "failure",
           "fix_status": "fixed|unfixed|unverified|not-applicable",
           "message": "Explanation of what was fixed"
         }
       ]
     }
     ```

#### For Issue Context (Issue-solve mode)

3. **`/holon/output/diff.patch`**: Git-compatible patch with code changes

4. **`/holon/output/pr-fix.json`** (if creating PR): PR creation metadata
   - `title`: PR title
   - `body`: PR body (references issue)
   - `branch`: Suggested branch name

## PR-Fix Mode Behavior

When PR context is detected:

### Error Triage (Priority Order)
1. **Build/compile failures** (blocks all tests)
2. **Runtime test failures**
3. **Import/module resolution errors**
4. **Lint/style warnings**

You MUST identify all errors first, then fix in this order. Do not fix lower-priority issues while higher-priority failures remain.

### Environment Setup (Before Claiming "Fixed")
- Verify required tools are available (build/test runners, package managers, compilers)
- If tools or dependencies are missing, attempt at least three setup paths:
  1. Project-recommended install commands
  2. Alternate install method (package manager, global install)
  3. Inspect CI workflow/config files for canonical setup steps
- If setup still fails, attempt a build/compile step (if possible) and report the failure

### Verification Requirements
- You may mark `fix_status: "fixed"` only if you ran a build/test command successfully
- If you cannot run tests, run the most relevant build/compile command and report that result
- If you made changes but cannot complete verification, use `fix_status: "unverified"` and document every attempt
- If you cannot address the issue or made no meaningful progress, use `fix_status: "unfixed"`
- Never claim success based on reasoning or syntax checks alone

### Test Failure Diagnosis

When CI tests fail, follow this workflow:

1. **Check for test logs**: Look for `context/github/test-failure-logs.txt`
2. **Read the logs**: Use grep to find specific test failures
3. **Analyze the failure**: What error/assertion failed? What file/line is failing?
4. **Determine relevance**: Check if modified files relate to the failure by comparing against `pr.diff`

**Using logs:**
```bash
# Find all failing tests
grep -E "(FAIL|FAIL:|FAILED)" /holon/input/context/github/test-failure-logs.txt

# Search for a specific test name
grep "TestRunner_Run_EnvVariablePrecedence" /holon/input/context/github/test-failure-logs.txt

# Show context around a failure
grep -A 20 "FAIL:" /holon/input/context/github/test-failure-logs.txt
```

### Handling Non-Blocking Refactor Requests

When review comments request substantial refactoring that is **valid but non-blocking**:

1. **Determine if truly non-blocking:**
   - Does not affect correctness, security, or API contracts
   - Would substantially increase PR scope (large refactor, comprehensive test suite)
   - Can be reasonably addressed in a follow-up without impacting this PR's value
   - Is an improvement rather than a fix for a problem introduced in this PR

2. **Use `status: "deferred"`** with clear explanation:
   - Acknowledge the validity of the suggestion
   - Explain why it's being deferred (scope, complexity, etc.)
   - Reference that a follow-up issue has been created

3. **Create a follow-up issue entry** in `follow_up_issues`:
   - **`title`**: Clear, actionable issue title
   - **`body`**: Comprehensive issue description including context, requested changes, rationale, and suggested approach
   - **`deferred_comment_ids`**: Array of comment IDs this issue addresses
   - **`labels`**: Suggested labels (e.g., `enhancement`, `testing`, `refactor`)

4. **Only defer when appropriate:**
   - **BLOCKING issues must be fixed**: bugs, security issues, breaking changes, missing critical functionality
   - **DEFER appropriate improvements**: additional test coverage, refactoring for clarity, performance optimizations
   - **Use `wontfix` for rejected suggestions**: requests that don't align with project goals

## Issue-Solve Mode Behavior

When only issue context is detected (no PR):

1. **Analyze the issue**: Read `issue.json` and `comments.json` (if present)
2. **Implement the solution**: Make code changes to address the issue
3. **Generate diff**: Create `/holon/output/diff.patch` with your changes
4. **Document**: Write `/holon/output/summary.md` explaining what was done
5. **PR metadata** (optional): Create `/holon/output/pr-fix.json` with PR title, body, and branch name

## Diagnostic Confidence Levels

When diagnosing CI failures, communicate your confidence:

- **High**: Root cause is clearly identified, all evidence points to the same conclusion
- **Medium**: Root cause is likely but not 100% certain, some evidence supports diagnosis
- **Low**: Significant conflicting evidence exists (e.g., tests pass locally but fail in CI)

When confidence is **low** or fix_status is **"not-applicable"**:
1. Document all conflicting evidence
2. List alternative explanations
3. Request specific investigation
4. Consider `fix_status: "unverified"` instead of "not-applicable"

## Common Contract Rules

Use the common contract rules without modification. The common contract provides:
- Sandbox environment rules and physics
- Developer role expectations
- Output artifact requirements
- Testing and verification guidelines
