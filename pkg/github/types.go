package github

import "time"

// PRInfo contains basic pull request information
type PRInfo struct {
	Number      int       `json:"number"`
	Title       string    `json:"title"`
	Body        string    `json:"body"`
	State       string    `json:"state"`
	URL         string    `json:"url"`
	BaseRef     string    `json:"base_ref"`
	HeadRef     string    `json:"head_ref"`
	BaseSHA     string    `json:"base_sha"`
	HeadSHA     string    `json:"head_sha"`
	Author      string    `json:"author"`
	CreatedAt   time.Time `json:"created_at"`
	UpdatedAt   time.Time `json:"updated_at"`
	Repository  string    `json:"repository"`
	MergeCommit string    `json:"merge_commit_sha,omitempty"`
}

// ReviewThread represents a review comment thread
type ReviewThread struct {
	CommentID   int64     `json:"comment_id"`
	URL         string    `json:"url"`
	Path        string    `json:"path"`
	Line        int       `json:"line,omitempty"`
	Side        string    `json:"side,omitempty"`
	StartLine   int       `json:"start_line,omitempty"`
	StartSide   string    `json:"start_side,omitempty"`
	DiffHunk    string    `json:"diff_hunk"`
	Body        string    `json:"body"`
	Author      string    `json:"author"`
	CreatedAt   time.Time `json:"created_at"`
	UpdatedAt   time.Time `json:"updated_at"`
	Resolved    bool      `json:"resolved"`
	InReplyToID int64     `json:"in_reply_to_id,omitempty"`
	Position    int       `json:"position,omitempty"`
	Replies     []Reply   `json:"replies,omitempty"`
}

// Reply represents a reply to a review comment
type Reply struct {
	CommentID   int64     `json:"comment_id"`
	URL         string    `json:"url"`
	Body        string    `json:"body"`
	Author      string    `json:"author"`
	CreatedAt   time.Time `json:"created_at"`
	UpdatedAt   time.Time `json:"updated_at"`
	InReplyToID int64     `json:"in_reply_to_id"`
}

// IssueInfo contains basic issue information
type IssueInfo struct {
	Number     int       `json:"number"`
	Title      string    `json:"title"`
	Body       string    `json:"body"`
	State      string    `json:"state"`
	URL        string    `json:"url"`
	Author     string    `json:"author"`
	Assignee   string    `json:"assignee,omitempty"`
	CreatedAt  time.Time `json:"created_at"`
	UpdatedAt  time.Time `json:"updated_at"`
	Repository string    `json:"repository"`
	Labels     []string  `json:"labels,omitempty"`
}

// IssueComment represents a comment on an issue
type IssueComment struct {
	CommentID int64     `json:"comment_id"`
	URL       string    `json:"url"`
	Body      string    `json:"body"`
	Author    string    `json:"author"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

// CheckRun represents a GitHub check run
type CheckRun struct {
	ID           int64          `json:"id"`
	Name         string         `json:"name"`
	HeadSHA      string         `json:"head_sha"`
	Status       string         `json:"status"`          // queued, in_progress, completed
	Conclusion   string         `json:"conclusion"`      // success, failure, neutral, cancelled, skipped, timed_out, action_required
	StartedAt    *time.Time     `json:"started_at,omitempty"`
	CompletedAt  *time.Time     `json:"completed_at,omitempty"`
	DetailsURL   string         `json:"details_url,omitempty"`
	AppSlug      string         `json:"app_slug,omitempty"`          // e.g., "github-actions"
	CheckSuiteID int64          `json:"check_suite_id,omitempty"`
	Output       CheckRunOutput `json:"output,omitempty"`
}

// CheckRunOutput represents the output of a check run
type CheckRunOutput struct {
	Title   string `json:"title,omitempty"`
	Summary string `json:"summary,omitempty"`
	Text    string `json:"text,omitempty"`
}

// CheckRunsResponse is the API response for check runs
type CheckRunsResponse struct {
	TotalCount int        `json:"total_count"`
	CheckRuns  []CheckRun `json:"check_runs"`
}

// CombinedStatus represents the combined status for a ref
type CombinedStatus struct {
	SHA        string    `json:"sha"`
	State      string    `json:"state"` // pending, success, failure, error
	TotalCount int       `json:"total_count"`
	Statuses   []Status  `json:"statuses"`
}

// Status represents an individual status
type Status struct {
	ID          int64      `json:"id"`
	Context     string     `json:"context"`      // e.g., "ci/travis-ci", "coverage/coveralls"
	State       string     `json:"state"`        // pending, success, failure, error
	TargetURL   string     `json:"target_url,omitempty"`
	Description string     `json:"description,omitempty"`
	CreatedAt   time.Time  `json:"created_at"`
	UpdatedAt   time.Time  `json:"updated_at"`
}

// NewPullRequest contains information for creating a new pull request
type NewPullRequest struct {
	Title               string `json:"title"`
	Head                string `json:"head"`
	Base                string `json:"base"`
	Body                string `json:"body"`
	MaintainerCanModify bool   `json:"maintainer_can_modify"`
}

// ActorInfo represents the authenticated GitHub user or app
type ActorInfo struct {
	Login     string `json:"login"`              // Username or app name
	Type      string `json:"type"`               // "User" or "App"
	Source    string `json:"source,omitempty"`   // "token" or "app"
	AppSlug   string `json:"app_slug,omitempty"` // App slug if type is "App"
}
