package workspace

import (
	"context"
	"time"
)

// PrepareRequest contains the parameters for a workspace preparation operation
type PrepareRequest struct {
	// Source is the origin of the workspace content
	// Examples: local path "/path/to/repo", remote URL "https://github.com/owner/repo.git"
	Source string

	// Ref is the git reference to checkout (branch, tag, or SHA)
	// Examples: "main", "v1.0.0", "abc123def", "" (HEAD)
	Ref string

	// Dest is the host directory where the workspace will be created
	// This directory will be mounted into the container at /holon/workspace
	Dest string

	// History specifies how much git history to include
	History HistoryMode

	// Submodules specifies how to handle git submodules
	Submodules SubmoduleMode

	// CleanDest indicates whether to clean the destination directory before preparation
	// If true, any existing content at Dest will be removed
	CleanDest bool
}

// HistoryMode defines how much git history to include in the workspace
type HistoryMode string

const (
	// HistoryFull includes complete git history
	HistoryFull HistoryMode = "full"
	// HistoryShallow creates a shallow clone with reduced history
	HistoryShallow HistoryMode = "shallow"
	// HistoryNone creates a workspace without git history (e.g., copy only)
	HistoryNone HistoryMode = "none"
)

// SubmoduleMode defines how to handle git submodules
type SubmoduleMode string

const (
	// SubmodulesNone does not initialize any submodules
	SubmodulesNone SubmoduleMode = "none"
	// SubmodulesRecursive initializes submodules recursively
	SubmodulesRecursive SubmoduleMode = "recursive"
)

// PrepareResult contains the outcome of a workspace preparation operation
type PrepareResult struct {
	// Strategy is the name of the strategy that handled this request
	Strategy string `json:"strategy"`

	// Source is the origin that was used
	Source string `json:"source"`

	// Ref is the git reference that was checked out
	Ref string `json:"ref"`

	// HeadSHA is the commit SHA of the workspace after preparation
	HeadSHA string `json:"head_sha"`

	// CreatedAt is the timestamp when preparation completed
	CreatedAt time.Time `json:"created_at"`

	// HasHistory indicates whether git history is available
	HasHistory bool `json:"has_history"`

	// IsShallow indicates whether the git repository is shallow
	IsShallow bool `json:"is_shallow"`

	// Notes contains any additional information about the preparation
	Notes []string `json:"notes,omitempty"`

	// BuiltinSkillsCommit is the git commit SHA of embedded builtin skills
	BuiltinSkillsCommit string `json:"builtin_skills_commit,omitempty"`
}

// Manifest contains workspace metadata
// The manifest is written to the output directory (not the workspace) by the runtime
type Manifest struct {
	// Strategy is the name of the strategy used
	Strategy string `json:"strategy"`

	// Source is the origin of the workspace content
	Source string `json:"source"`

	// Ref is the git reference that was checked out
	Ref string `json:"ref,omitempty"`

	// HeadSHA is the commit SHA of the workspace
	HeadSHA string `json:"head_sha,omitempty"`

	// CreatedAt is the timestamp when the workspace was prepared
	CreatedAt time.Time `json:"created_at"`

	// HasHistory indicates whether git history is available
	HasHistory bool `json:"has_history"`

	// IsShallow indicates whether the git repository is shallow
	IsShallow bool `json:"is_shallow"`

	// Notes contains any additional information
	Notes []string `json:"notes,omitempty"`

	// BuiltinSkillsCommit is the git commit SHA of embedded builtin skills
	BuiltinSkillsCommit string `json:"builtin_skills_commit,omitempty"`
}

// Preparer is the interface for preparing workspaces
type Preparer interface {
	// Prepare creates a workspace directory at Dest with the requested content
	Prepare(ctx context.Context, req PrepareRequest) (PrepareResult, error)

	// Name returns the strategy name (e.g., "git-clone", "snapshot", "existing")
	Name() string

	// Validate checks if the request is valid for this preparer
	// Returns nil if valid, or an error describing what's invalid
	Validate(req PrepareRequest) error

	// Cleanup performs any necessary cleanup after workspace use
	// For most strategies, this is a simple directory removal
	// Returns an error if cleanup fails
	Cleanup(dest string) error
}
