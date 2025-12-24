package workspace

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
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

	// Build git clone arguments based on history mode
	args := p.buildCloneArgs(req)

	// Execute git clone
	cmd := exec.CommandContext(ctx, "git", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return PrepareResult{}, fmt.Errorf("git clone failed: %v, output: %s", err, string(out))
	}

	// Verify the clone was created successfully
	if !IsGitRepo(req.Dest) {
		return PrepareResult{}, fmt.Errorf("git clone succeeded but destination is not a git repository")
	}

	// Checkout the requested ref, or HEAD if no ref specified
	// We always do a checkout since we used --no-checkout during clone
	checkoutRef := req.Ref
	if checkoutRef == "" {
		checkoutRef = "HEAD"
	}
	if err := checkoutRefContext(ctx, req.Dest, checkoutRef); err != nil {
		// Checkout failed - log a note but don't fail
		result.Notes = append(result.Notes, fmt.Sprintf("Warning: failed to checkout ref '%s': %v", checkoutRef, err))
	}

	// Handle submodules if requested
	if req.Submodules == SubmodulesRecursive {
		if err := p.initSubmodules(ctx, req.Dest); err != nil {
			// Submodule init failed - log a note but don't fail
			result.Notes = append(result.Notes, fmt.Sprintf("Warning: failed to initialize submodules: %v", err))
		}
	}

	// Get HEAD SHA
	headSHA, err := getHeadSHAContext(ctx, req.Dest)
	if err != nil {
		result.Notes = append(result.Notes, fmt.Sprintf("Warning: failed to get HEAD SHA: %v", err))
	} else {
		result.HeadSHA = headSHA
	}

	// Determine history status
	result.HasHistory = req.History != HistoryNone
	result.IsShallow = req.History == HistoryShallow || isShallowCloneContext(ctx, req.Dest)

	// Write workspace manifest
	if err := WriteManifest(req.Dest, result); err != nil {
		return PrepareResult{}, fmt.Errorf("failed to write workspace manifest: %w", err)
	}

	return result, nil
}

// Cleanup removes the workspace directory
func (p *GitClonePreparer) Cleanup(dest string) error {
	return os.RemoveAll(dest)
}

// buildCloneArgs constructs the git clone command arguments based on the request
func (p *GitClonePreparer) buildCloneArgs(req PrepareRequest) []string {
	args := []string{"clone", "--quiet", "--no-checkout"}

	// Handle history mode
	switch req.History {
	case HistoryShallow:
		args = append(args, "--depth", "1")
		// Note: --local is incompatible with --depth for creating true shallow clones
		// When shallow is requested, we don't use --local even for local repos
	case HistoryNone:
		// For history none, we use depth 1 to create a minimal clone.
		// While this creates a git repository, it has effectively no history
		// beyond the single commit. For true "no history" behavior without a
		// git repository at all, use the snapshot strategy instead.
		args = append(args, "--depth", "1")
	case HistoryFull:
		// Full history is the default for git clone
		// When source is a local repo, git clone --local is more efficient
		if IsGitRepo(req.Source) {
			args = append(args, "--local")
		}
	}

	// Source and destination
	args = append(args, req.Source, req.Dest)

	return args
}

// initSubmodules initializes git submodules recursively
func (p *GitClonePreparer) initSubmodules(ctx context.Context, dir string) error {
	cmd := exec.CommandContext(ctx, "git", "-C", dir, "submodule", "update", "--init", "--recursive", "--quiet")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("submodule init failed: %v, output: %s", err, string(out))
	}
	return nil
}
