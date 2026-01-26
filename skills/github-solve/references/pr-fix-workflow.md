# PR-Fix Workflow

Detailed workflow for fixing issues in pull requests.

## Context

This guide applies when PR context is detected (review comments, test failures, CI issues).

## Error Triage (Priority Order)

You MUST identify all errors first, then fix in this order. Do not fix lower-priority issues while higher-priority failures remain.

1. **Build/compile failures** (blocks all tests)
2. **Runtime test failures**
3. **Import/module resolution errors**
4. **Lint/style warnings**

## Environment Setup

Before claiming "Fixed", verify required tools are available (build/test runners, package managers, compilers).

If tools or dependencies are missing, attempt at least three setup paths:

1. Project-recommended install commands
2. Alternate install method (package manager, global install)
3. Inspect CI workflow/config files for canonical setup steps

If setup still fails, attempt a build/compile step (if possible) and report the failure.

## Verification Requirements

- You may mark `fix_status: "fixed"` only if you ran a build/test command successfully
- If you cannot run tests, run the most relevant build/compile command and report that result
- If you made changes but cannot complete verification, use `fix_status: "unverified"` and document every attempt
- If you cannot address the issue or made no meaningful progress, use `fix_status: "unfixed"`
- Never claim success based on reasoning or syntax checks alone

## Test Failure Diagnosis

When CI tests fail, follow this workflow:

1. **Check for test logs**: Look for `context/github/test-failure-logs.txt`
2. **Read the logs**: Use grep to find specific test failures
3. **Analyze the failure**: What error/assertion failed? What file/line is failing?
4. **Determine relevance**: Check if modified files relate to the failure by comparing against `pr.diff`

### Using Logs

```bash
# Find all failing tests
grep -E "(FAIL|FAIL:|FAILED)" /holon/input/context/github/test-failure-logs.txt

# Search for a specific test name
grep "TestRunner_Run_EnvVariablePrecedence" /holon/input/context/github/test-failure-logs.txt

# Show context around a failure
grep -A 20 "FAIL:" /holon/input/context/github/test-failure-logs.txt
```

## Handling Non-Blocking Refactor Requests

When review comments request substantial refactoring that is **valid but non-blocking**:

### 1. Determine if Truly Non-Blocking

A refactor request is non-blocking if it:
- Does not affect correctness, security, or API contracts
- Would substantially increase PR scope (large refactor, comprehensive test suite)
- Can be reasonably addressed in a follow-up without impacting this PR's value
- Is an improvement rather than a fix for a problem introduced in this PR

### 2. Use `status: "deferred"` with Clear Explanation

- Acknowledge the validity of the suggestion
- Explain why it's being deferred (scope, complexity, etc.)
- Reference that a follow-up issue has been created

### 3. Create a Follow-Up Issue Entry

Add to `follow_up_issues` array in `pr-fix.json`:

```json
{
  "title": "Clear, actionable issue title",
  "body": "Comprehensive issue description including context, requested changes, rationale, and suggested approach",
  "deferred_comment_ids": [123, 456],
  "labels": ["enhancement", "testing", "refactor"]
}
```

### 4. Defer vs Fix Guidelines

**BLOCKING issues must be fixed:**
- Bugs
- Security issues
- Breaking changes
- Missing critical functionality

**DEFER appropriate improvements:**
- Additional test coverage
- Refactoring for clarity
- Performance optimizations

**Use `wontfix` for rejected suggestions:**
- Requests that don't align with project goals

## Posting Review Replies

After generating `${GITHUB_OUTPUT_DIR}/pr-fix.json` with review replies:

### 1. Use the Reply Script

```bash
# Navigate to skill directory
cd /holon/workspace/skills/github-solve

# Preview replies (recommended before posting)
scripts/reply-reviews.sh --dry-run

# Post replies to PR
scripts/reply-reviews.sh --pr=owner/repo#123
```

### 2. Reply Format

Automatically applied by script:
- Emoji based on status: ✅ fixed, ⚠️ wontfix, ❓ need-info, 🔜 deferred
- Status label in uppercase
- Your message
- Optional "Action taken" section

### 3. Idempotency

- Script checks if bot already replied to each comment
- Skips already-responded comments
- Supports `--from=N` to resume from specific index

### 4. Output Files

- `reply-results.json`: Detailed results for each reply attempt
- Console summary: total, posted, skipped, failed counts

## pr-fix.json Format

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
