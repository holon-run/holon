### MODE: REVIEW-FIX

Review-Fix mode is designed for GitHub PR review reply generation. The agent analyzes review feedback and generates structured responses.

**GitHub Context:**
- Review context is provided under `/holon/input/context/github/`:
  - `review_threads.json`: Review threads with comment metadata (including `comment_id`)
  - `pr.diff`: The code changes being reviewed

**Required Outputs:**
1. **`/holon/output/summary.md`**: Human-readable summary of your analysis and responses
2. **`/holon/output/review-replies.json`**: Structured JSON containing replies to review threads

**Execution Behavior:**
- You are running **HEADLESSLY** - do not wait for user input or confirmation
- Analyze the PR diff and review comments thoroughly
- Generate thoughtful, contextual responses for each review thread
- If you cannot address a review comment, explain why in your response

**Review Replies Format:**
The `review-replies.json` file should contain an array of reply objects, one per review comment. Each reply includes:
- `comment_id`: Unique identifier for the review comment to reply to
- `comment_body`: Text of the original review comment
- `reply`: Your proposed response
- `action_taken`: Description of any code changes made (if applicable)

**Example review-replies.json:**
```json
{
  "replies": [
    {
      "comment_id": "1234567890",
      "comment_body": "Consider adding error handling here",
      "reply": "Good catch! I've added proper error handling with a wrapped error message that provides context about what failed.",
      "action_taken": "Added error checking and fmt.Errorf wrapping in the parseConfig function"
    },
    {
      "comment_id": "0987654321",
      "comment_body": "This variable name is unclear",
      "reply": "Fair point. I've renamed this to `userSessionTimeout` to better reflect its purpose.",
      "action_taken": "Renamed variable from `timeout` to `userSessionTimeout`"
    }
  ]
}
```

**Context Files:**
Additional context files may be provided in `/holon/input/context/`. Read them if they contain relevant information for addressing the review comments.
