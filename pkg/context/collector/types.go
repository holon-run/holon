package collector

import (
	"context"
	"time"
)

// Kind represents the type of context to collect
type Kind string

const (
	// KindIssue represents an issue context
	KindIssue Kind = "issue"
	// KindPR represents a pull request context
	KindPR Kind = "pr"
)

// CollectRequest contains the parameters for a context collection operation
type CollectRequest struct {
	// Kind is the type of context to collect (issue or pr)
	Kind Kind

	// Ref is the reference to the issue or PR
	// Examples: "123", "#123", "owner/repo#123", "https://github.com/owner/repo/issues/123"
	Ref string

	// RepoHint is an optional repository hint in the format "owner/repo"
	// Used when Ref doesn't contain the repository (e.g., just "#123")
	RepoHint string

	// OutputDir is the directory where collected context will be written
	OutputDir string

	// Options contains provider-specific options
	Options Options
}

// Options contains provider-specific collection options
type Options struct {
	// Token is the authentication token for the provider
	Token string

	// IncludeDiff indicates whether to include the PR diff (for PRs)
	IncludeDiff bool

	// UnresolvedOnly indicates whether to only collect unresolved review threads (for PRs)
	UnresolvedOnly bool

	// IncludeChecks indicates whether to fetch CI/check results (for PRs)
	IncludeChecks bool

	// ChecksOnlyFailed indicates whether to only include failed checks (for PRs)
	ChecksOnlyFailed bool

	// ChecksMax is the maximum number of check runs to fetch (for PRs)
	// 0 or negative means no limit
	ChecksMax int

	// TriggerCommentID is the ID of the comment that triggered holon (for marking is_trigger)
	TriggerCommentID int64

	// TriggerGoalHint is the trailing text from a free-form trigger (e.g., "@holonbot fix this bug" -> "fix this bug")
	TriggerGoalHint string
}

// FileInfo represents metadata about a collected file
type FileInfo struct {
	// Path is the relative path from the output directory
	Path string `json:"path"`

	// ContentType is the optional content type (e.g., "application/json", "text/plain")
	ContentType string `json:"content_type,omitempty"`

	// Description is a human-readable description of the file
	Description string `json:"description,omitempty"`
}

// CollectResult contains the outcome of a context collection operation
type CollectResult struct {
	// Provider is the name of the provider that handled this request
	Provider string `json:"provider"`

	// Kind is the type of context that was collected
	Kind Kind `json:"kind"`

	// Ref is the reference that was collected
	Ref string `json:"ref"`

	// Owner is the repository owner (e.g., "holon-run")
	Owner string `json:"owner"`

	// Repo is the repository name (e.g., "holon")
	Repo string `json:"repo"`

	// Number is the issue or PR number
	Number int `json:"number"`

	// CollectedAt is the timestamp when collection completed
	CollectedAt time.Time `json:"collected_at"`

	// Files is a list of files that were written
	Files []FileInfo `json:"files"`

	// Success indicates whether the overall collection operation succeeded
	Success bool `json:"success"`

	// Error contains an error message if the operation failed
	Error string `json:"error,omitempty"`
}

// Collector is the interface for collecting context from various sources
type Collector interface {
	// Collect gathers context and writes it to the output directory
	Collect(ctx context.Context, req CollectRequest) (CollectResult, error)

	// Name returns the provider name (e.g., "github", "gitlab")
	Name() string

	// Validate checks if the request is valid for this collector
	// Returns nil if valid, or an error describing what's invalid
	Validate(req CollectRequest) error
}
