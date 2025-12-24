package publisher

import "time"

// PublishRequest contains the input parameters for a publish operation.
type PublishRequest struct {
	// Target is the destination for publishing (e.g., repo URL, PR number, branch name)
	Target string

	// OutputDir is the path to the Holon output directory containing artifacts
	OutputDir string

	// Manifest is the execution manifest from the Holon run
	Manifest map[string]interface{}

	// Artifacts is a map of artifact names to their file paths
	Artifacts map[string]string
}

// PublishAction represents a single action taken during publishing.
type PublishAction struct {
	// Type is the kind of action performed
	// Examples: "posted_summary_comment", "replied_review_comment", "applied_patch",
	// "pushed_branch", "created_commit"
	Type string `json:"type"`

	// Description provides human-readable details about the action
	Description string `json:"description"`

	// Metadata contains additional action-specific information
	Metadata map[string]string `json:"metadata,omitempty"`
}

// PublishError represents an error that occurred during publishing.
type PublishError struct {
	// Message is the error message
	Message string `json:"message"`

	// Action is the action that failed (if applicable)
	Action string `json:"action,omitempty"`

	// Details contains additional error context
	Details map[string]string `json:"details,omitempty"`
}

// PublishResult contains the outcome of a publish operation.
type PublishResult struct {
	// Provider is the name of the publisher that handled this request
	Provider string `json:"provider"`

	// Target is the destination that was published to
	Target string `json:"target"`

	// PublishedAt is the timestamp when publishing completed
	PublishedAt time.Time `json:"published_at"`

	// Actions is a list of actions taken during publishing
	Actions []PublishAction `json:"actions"`

	// Errors contains any errors that occurred during publishing
	Errors []PublishError `json:"errors,omitempty"`

	// Success indicates whether the overall publish operation succeeded
	Success bool `json:"success"`
}

// Publisher is the interface for publishing Holon outputs to external systems.
type Publisher interface {
	// Publish sends Holon outputs to the target system.
	// It returns a PublishResult describing what actions were taken and any errors.
	Publish(req PublishRequest) (PublishResult, error)

	// Name returns the provider name (e.g., "github", "git", "gitlab")
	Name() string

	// Validate checks if the request is valid for this publisher.
	// Returns nil if valid, or an error describing what's invalid.
	Validate(req PublishRequest) error
}
