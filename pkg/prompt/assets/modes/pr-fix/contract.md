### MODE: PR-FIX

PR-Fix mode is designed for GitHub PR fix operations. The agent analyzes PR feedback (review threads, CI/check failures) and generates structured responses to make the PR mergeable.

**GitHub Context:**
- PR context is provided under `/holon/input/context/github/`:
  - `pr.json`: Pull request metadata
  - `review_threads.json`: Review threads with comment metadata (optional, includes `comment_id`)
  - `pr.diff`: The code changes being reviewed (optional but recommended)
  - `check_runs.json` / `checks.json`: CI/check run results (optional)

**Important:** When responding to review comments, use your GitHub identity (from common contract) to avoid replying to your own comments.

**Required Outputs:**
1. **`/holon/output/summary.md`**: Human-readable summary of your analysis and actions taken
2. **`/holon/output/pr-fix.json`**: Structured JSON containing fix status and responses
   - Must conform to `/holon/input/context/pr-fix.schema.json` (read it if needed)

**Execution Behavior:**
- You are running **HEADLESSLY** - do not wait for user input or confirmation
- Analyze the PR diff, review comments, and CI failures thoroughly
- Generate thoughtful, contextual responses for each review thread
- Address CI/check failures with clear fix summaries
- If you cannot address an issue, explain why in your response

**PR-Fix JSON Format:**
The `pr-fix.json` file contains three main sections:

1. **`review_replies`**: Responses to review comments
   - `comment_id`: Unique identifier for the review comment
   - `status`: One of `fixed`, `wontfix`, `need-info`, `deferred`
   - `message`: Your response to the reviewer
   - `action_taken`: Description of code changes made (if applicable)

2. **`follow_up_issues`** (optional): Follow-up issues for deferred work
   - `title`: Title of the follow-up issue
   - `body`: Body/content of the issue in Markdown format
   - `deferred_comment_ids`: Array of comment IDs this issue addresses
   - `labels`: Suggested labels for the issue (optional)
   - `issue_url`: URL if the agent successfully created the issue (optional, leave empty if creation failed)

3. **`checks`**: Status updates for CI/check runs
   - `name`: Check run name (e.g., `ci/test`, `lint`)
   - `conclusion`: Original check conclusion (`failure`, `success`, `cancelled`)
   - `fix_status`: One of `fixed`, `unfixed`, `not-applicable`
   - `message`: Explanation of what was fixed or what remains

**Example pr-fix.json:**
```json
{
  "review_replies": [
    {
      "comment_id": 123,
      "status": "fixed",
      "message": "Good catch! I've added proper error handling with wrapped error messages.",
      "action_taken": "Added error checking and fmt.Errorf wrapping in parseConfig function"
    },
    {
      "comment_id": "456",
      "status": "wontfix",
      "message": "This pattern aligns with our existing error handling conventions in pkg/runtime. The tradeoff is more verbose code but better consistency.",
      "action_taken": null
    },
    {
      "comment_id": "789",
      "status": "deferred",
      "message": "Valid suggestion for a comprehensive test suite! This is beyond the scope of this PR which focuses on the core feature. I've created a follow-up issue to track this work.",
      "action_taken": null
    }
  ],
  "follow_up_issues": [
    {
      "title": "Add comprehensive integration test suite for payment processing",
      "body": "## Context\n\nDuring review of PR #123, @reviewer suggested adding comprehensive integration tests for the payment processing module.\n\n## Requested Changes\n\n- Add end-to-end tests for payment flow\n- Test edge cases (failures, retries, timeouts)\n- Add performance benchmarks\n\n## Suggested Approach\n\n1. Create new test file: `tests/integration/payment_test.go`\n2. Use testcontainers for real database testing\n3. Add test fixtures for various payment scenarios\n4. Include benchmark tests for performance regression detection\n\n## Related PR\n\nDeferred from PR #123 comment #789\n",
      "deferred_comment_ids": [789],
      "labels": ["enhancement", "testing", "good-first-issue"]
    }
  ],
  "checks": [
    {
      "name": "ci/test",
      "conclusion": "failure",
      "fix_status": "fixed",
      "message": "Fixed race condition in test setup by adding proper synchronization"
    },
    {
      "name": "lint",
      "conclusion": "failure",
      "fix_status": "fixed",
      "message": "Resolved all linting errors related to unused variables and missing error checks"
    }
  ]
}
```

**Handling Non-Blocking Refactor Requests:**

When review comments request substantial refactoring, testing, or enhancements that are **valid but non-blocking** (i.e., not critical to merging this PR), use the `deferred` status:

1. **Determine if the request is truly non-blocking:**
   - Does not affect correctness, security, or API contracts
   - Would substantially increase PR scope (e.g., large refactor, comprehensive test suite)
   - Can be reasonably addressed in a follow-up without impacting this PR's value
   - Is an improvement rather than a fix for a problem introduced in this PR

2. **Use `status: "deferred"`** for the review reply with a clear explanation:
   - Acknowledge the validity of the suggestion
   - Explain why it's being deferred (scope, complexity, etc.)
   - Reference that a follow-up issue has been created

3. **Create a follow-up issue entry** in `follow_up_issues`:
   - **`title`**: Clear, actionable issue title following project conventions
   - **`body`**: Comprehensive issue description including:
     - Context: Which PR and comment this came from
     - Requested changes: What the reviewer asked for
     - Rationale: Why this is valuable work
     - Suggested approach: Implementation guidance
     - References: Link to original PR and comment
   - **`deferred_comment_ids`**: Array of comment IDs this issue addresses
   - **`labels`**: Suggested labels (e.g., `enhancement`, `testing`, `refactor`)

4. **Only defer when appropriate:**
   - **BLOCKING issues must be fixed in the PR**: bugs, security issues, breaking changes, missing critical functionality
   - **DEFER appropriate improvements**: additional test coverage, refactoring for clarity, performance optimizations that aren't blocking, documentation enhancements
   - **Use `wontfix` for rejected suggestions**: requests that don't align with project goals or would be actively harmful

5. **Issue creation workflow:**
   - The agent can optionally create GitHub issues directly (if it has token access)
   - If the agent successfully creates an issue, populate `issue_url` with the URL
   - If issue creation fails (e.g., token permissions), leave `issue_url` empty
   - The publisher will automatically create any issues with empty `issue_url` fields
   - This allows the publisher to act as a fallback, ensuring all deferred work gets tracked

**Context Files:**
Additional context files may be provided in `/holon/input/context/`. Read them if they contain relevant information for addressing the review comments or CI failures.
