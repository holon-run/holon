package workspace

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	"github.com/holon-run/holon/pkg/git"
)

// GitClonePreparer prepares a workspace using git clone
// This creates a self-contained git repository that works inside containers
// without host-path references or alternates
type GitClonePreparer struct {
	name string
}

// NewGitClonePreparer creates a new git-clone preparer
func NewGitClonePreparer() *GitClonePreparer {
	return &GitClonePreparer{
		name: "git-clone",
	}
}

// Name returns the strategy name
func (p *GitClonePreparer) Name() string {
	return p.name
}

// Validate checks if the request is valid for this preparer
func (p *GitClonePreparer) Validate(req PrepareRequest) error {
	if req.Source == "" {
		return fmt.Errorf("source cannot be empty")
	}
	if req.Dest == "" {
		return fmt.Errorf("dest cannot be empty")
	}
	return nil
}

// Prepare creates a workspace using git clone
func (p *GitClonePreparer) Prepare(ctx context.Context, req PrepareRequest) (PrepareResult, error) {
	if err := p.Validate(req); err != nil {
		return PrepareResult{}, fmt.Errorf("validation failed: %w", err)
	}

	result := NewPrepareResult(p.Name())
	result.Source = req.Source
	result.Ref = req.Ref

	// Clean destination if requested
	if req.CleanDest {
		if err := os.RemoveAll(req.Dest); err != nil {
			return PrepareResult{}, fmt.Errorf("failed to clean destination: %w", err)
		}
	}

	// Create parent directory if it doesn't exist
	if err := os.MkdirAll(filepath.Dir(req.Dest), 0755); err != nil {
		return PrepareResult{}, fmt.Errorf("failed to create parent directory: %w", err)
	}

	// Build git clone options based on history mode
	opts := p.buildCloneOptions(ctx, req)

	// Execute git clone
	cloneResult, err := git.Clone(ctx, opts)
	if err != nil {
		return PrepareResult{}, fmt.Errorf("git clone failed: %w", err)
	}

	// Verify the clone was created successfully
	client := git.NewClient(req.Dest)
	if !client.IsRepo(ctx) {
		return PrepareResult{}, fmt.Errorf("git clone succeeded but destination is not a git repository")
	}

	// Get HEAD SHA from clone result or fallback to client
	headSHA := cloneResult.HEAD
	if headSHA == "" {
		headSHA, err = client.GetHeadSHA(ctx)
		if err != nil {
			result.Notes = append(result.Notes, fmt.Sprintf("Warning: failed to get HEAD SHA: %v", err))
		}
	}
	result.HeadSHA = headSHA

	// Determine history status
	result.HasHistory = req.History != HistoryNone
	result.IsShallow = req.History == HistoryShallow || cloneResult.IsShallow

	return result, nil
}

// Cleanup removes the workspace directory
func (p *GitClonePreparer) Cleanup(dest string) error {
	return os.RemoveAll(dest)
}

// buildCloneOptions constructs the git clone options based on the request
func (p *GitClonePreparer) buildCloneOptions(ctx context.Context, req PrepareRequest) git.CloneOptions {
	opts := git.CloneOptions{
		Source: req.Source,
		Dest:   req.Dest,
		Ref:    req.Ref,
		Quiet:  true,
	}

	// Handle history mode
	switch req.History {
	case HistoryShallow:
		opts.Depth = 1
		// Note: --local is incompatible with --depth for creating true shallow clones
		// When shallow is requested, we don't use --local even for local repos
	case HistoryNone:
		// For history none, we use depth 1 to create a minimal clone.
		// While this creates a git repository, it has effectively no history
		// beyond the single commit. For true "no history" behavior without a
		// git repository at all, use the snapshot strategy instead.
		opts.Depth = 1
	case HistoryFull:
		// Full history is the default for git clone
		// When source is a local repo, git clone --local is more efficient
		client := git.NewClient(req.Source)
		if client.IsRepo(ctx) {
			opts.Local = true
		}
	}

	// Handle submodules
	if req.Submodules == SubmodulesRecursive {
		opts.Submodules = true
	}

	return opts
}
