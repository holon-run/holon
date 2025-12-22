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

// PRContext contains all PR context information
type PRContext struct {
	PR             PRInfo          `json:"pr"`
	ReviewThreads  []ReviewThread  `json:"review_threads"`
	UnresolvedOnly bool            `json:"unresolved_only"`
	CollectedAt    time.Time       `json:"collected_at"`
}
