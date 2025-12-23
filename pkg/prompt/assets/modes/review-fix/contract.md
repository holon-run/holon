### MODE: REVIEW-FIX

Review-Fix mode is designed for GitHub PR review reply generation. The agent analyzes review feedback and generates structured responses.

**GitHub Context:**
- Review context is provided under `/holon/input/context/github/`:
  - `issue.json`: Pull request metadata and issue details
  - `comments.json`: Review comments and thread discussions (may be named `review_threads.json` in some contexts)
  - `pr.diff`: The code changes being reviewed (may also be named `diff.patch`)

**Required Outputs:**
1. **`/holon/output/summary.md`**: Human-readable summary of your analysis and responses
2. **`/holon/output/review-replies.json`**: Structured JSON containing replies to review threads

**Execution Behavior:**
- You are running **HEADLESSLY** - do not wait for user input or confirmation
- Analyze the PR diff and review comments thoroughly
- Generate thoughtful, contextual responses for each review thread
- If you cannot address a review comment, explain why in your response

**Review Replies Format:**
The `review-replies.json` file should contain an object with replies keyed by `comment_id` for GitHub thread reply functionality. Each reply includes:
- `comment_id`: Unique identifier for the specific review comment (required for thread reply)
- `comment_body`: Text of the original review comment
- `reply`: Your proposed response
- `action_taken`: Description of any code changes made (if applicable)

**Example review-replies.json:**
```json
{
  "replies": {
    "1234567890": {
      "comment_id": "1234567890",
      "comment_body": "Consider adding error handling here",
      "reply": "Good catch! I've added proper error handling with a wrapped error message that provides context about what failed.",
      "action_taken": "Added error checking and fmt.Errorf wrapping in the parseConfig function"
    },
    "0987654321": {
      "comment_id": "0987654321",
      "comment_body": "This variable name is unclear",
      "reply": "Fair point. I've renamed this to `userSessionTimeout` to better reflect its purpose.",
      "action_taken": "Renamed variable from `timeout` to `userSessionTimeout`"
    }
  }
}
```

**Important Notes:**
- The `comment_id` field is required for GitHub's thread reply functionality
- Use the exact `comment_id` from the input `comments.json` or `review_threads.json`
- Replies are keyed by `comment_id` in the `replies` object for O(1) lookup
- Each reply object must contain the `comment_id` for publisher compatibility

**Context Files:**
Additional context files may be provided in `/holon/input/context/`. Read them if they contain relevant information for addressing the review comments.
