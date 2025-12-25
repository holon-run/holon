package github

// PRFixData represents the parsed pr-fix.json file.
type PRFixData struct {
	ReviewReplies   []ReviewReply     `json:"review_replies"`
	FollowUpIssues  []FollowUpIssue   `json:"follow_up_issues,omitempty"`
	Checks          []CheckRun        `json:"checks"`
}

// FollowUpIssue represents a follow-up issue for deferred work.
type FollowUpIssue struct {
	Title               string   `json:"title"`
	Body                string   `json:"body"`
	DeferredCommentIDs  []int64  `json:"deferred_comment_ids"`
	Labels              []string `json:"labels,omitempty"`
	IssueURL            string   `json:"issue_url,omitempty"`
}

// ReviewReply represents a single review comment reply from pr-fix.json.
type ReviewReply struct {
	CommentID   int64   `json:"comment_id"`
	Status      string  `json:"status"` // "fixed", "wontfix", "need-info", "deferred"
	Message     string  `json:"message"`
	ActionTaken *string `json:"action_taken,omitempty"`
}

// CheckRun represents a CI/check run status update from pr-fix.json.
type CheckRun struct {
	Name       string `json:"name"`
	Conclusion string `json:"conclusion"` // "failure", "success", "cancelled"
	FixStatus  string `json:"fix_status"` // "fixed", "unfixed", "not-applicable"
	Message    string `json:"message"`
}

// PRRef represents a parsed GitHub PR reference.
// Supports formats: "owner/repo/pr/123", "owner/repo#123", "owner/repo/pull/123"
type PRRef struct {
	Owner    string
	Repo     string
	PRNumber int
}

// PublishResult contains the outcome of a GitHub publish operation.
type PublishResult struct {
	// Summary comment result
	SummaryComment CommentResult `json:"summary_comment"`

	// Review replies results
	ReviewReplies ReviewRepliesResult `json:"review_replies"`

	// Overall success
	Success bool `json:"success"`
}

// CommentResult represents the result of posting/updating a summary comment.
type CommentResult struct {
	Posted    bool   `json:"posted"`
	Updated   bool   `json:"updated,omitempty"` // true if an existing comment was updated
	CommentID int64  `json:"comment_id,omitempty"`
	Error     string `json:"error,omitempty"`
}

// ReviewRepliesResult represents the aggregated results of posting review replies.
type ReviewRepliesResult struct {
	Total   int              `json:"total"`
	Posted  int              `json:"posted"`
	Skipped int              `json:"skipped"`
	Failed  int              `json:"failed"`
	Details []ReplyResult    `json:"details,omitempty"`
}

// ReplyResult represents the result of posting a single review reply.
type ReplyResult struct {
	CommentID int64  `json:"comment_id"`
	Status    string `json:"status"` // "posted", "skipped", "failed"
	Reason    string `json:"reason,omitempty"`
}

// FollowUpIssuesResult represents the result of handling follow-up issues.
type FollowUpIssuesResult struct {
	Created       []FollowUpIssueDetail `json:"created,omitempty"`
	CreatedCount  int                   `json:"created_count"`
	DeferredCount int                   `json:"deferred_count"`
}

// FollowUpIssueDetail represents a created follow-up issue.
type FollowUpIssueDetail struct {
	Title    string `json:"title"`
	IssueURL string `json:"issue_url"`
}
