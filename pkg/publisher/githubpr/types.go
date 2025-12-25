package githubpr

import "time"

// PRRef represents a parsed GitHub repository reference.
// Supports formats: "owner/repo", "owner/repo:base_branch"
type PRRef struct {
	Owner       string
	Repo        string
	BaseBranch  string // Optional: defaults to "main"
}

// PRPublisherConfig contains configuration for PR publishing.
type PRPublisherConfig struct {
	// BranchName is the name of the branch to create/use for the PR
	BranchName string

	// Title is the PR title (if empty, derived from summary.md)
	Title string

	// CommitMessage is the commit message (if empty, derived from summary.md)
	CommitMessage string

	// IssueID is the optional issue number to reference in the PR
	IssueID string

	// DryRun if true, validates without making changes
	DryRun bool
}

// PublishResult contains the outcome of a PR publish operation.
type PublishResult struct {
	// Branch created/updated
	Branch string `json:"branch"`

	// Commit created
	Commit string `json:"commit"`

	// PR created or updated
	PRNumber int    `json:"pr_number,omitempty"`
	PRURL    string `json:"pr_url,omitempty"`

	// Whether PR was created or updated
	IsNew bool `json:"is_new"`

	// Timestamp of operation
	Timestamp time.Time `json:"timestamp"`

	// Success status
	Success bool `json:"success"`
}
