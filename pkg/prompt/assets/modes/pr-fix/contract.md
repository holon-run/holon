### MODE: PR-FIX

PR-Fix mode is designed for GitHub PR fix operations. The agent analyzes PR feedback (review threads, CI/check failures) and generates structured responses to make the PR mergeable.

**GitHub Context:**
- PR context is provided under `/holon/input/context/github/`:
  - `pr.json`: Pull request metadata
  - `review_threads.json`: Review threads with comment metadata (optional, includes `comment_id`)
  - `pr.diff`: The code changes being reviewed (optional but recommended)
  - `check_runs.json` / `checks.json`: CI/check run results (optional)

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
The `pr-fix.json` file contains two main sections:

1. **`review_replies`**: Responses to review comments
   - `comment_id`: Unique identifier for the review comment
   - `status`: One of `fixed`, `wontfix`, `need-info`
   - `message`: Your response to the reviewer
   - `action_taken`: Description of code changes made (if applicable)

2. **`checks`**: Status updates for CI/check runs
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

**Context Files:**
Additional context files may be provided in `/holon/input/context/`. Read them if they contain relevant information for addressing the review comments or CI failures.
