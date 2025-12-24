package workspace

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

// SnapshotPreparer prepares a workspace by copying files without git history
// This is useful for cases where history is not needed or when the source is not a git repo
type SnapshotPreparer struct {
	name string
}

// NewSnapshotPreparer creates a new snapshot preparer
func NewSnapshotPreparer() *SnapshotPreparer {
	return &SnapshotPreparer{
		name: "snapshot",
	}
}

// Name returns the strategy name
func (p *SnapshotPreparer) Name() string {
	return p.name
}

// Validate checks if the request is valid for this preparer
func (p *SnapshotPreparer) Validate(req PrepareRequest) error {
	if req.Source == "" {
		return fmt.Errorf("source cannot be empty")
	}
	if req.Dest == "" {
		return fmt.Errorf("dest cannot be empty")
	}
	// Check that source exists
	if _, err := os.Stat(req.Source); os.IsNotExist(err) {
		return fmt.Errorf("source does not exist: %s", req.Source)
	}
	return nil
}

// Prepare creates a workspace by copying files
func (p *SnapshotPreparer) Prepare(ctx context.Context, req PrepareRequest) (PrepareResult, error) {
	if err := p.Validate(req); err != nil {
		return PrepareResult{}, fmt.Errorf("validation failed: %w", err)
	}

	result := NewPrepareResult(p.Name())
	result.Source = req.Source
	result.Ref = req.Ref
	result.HasHistory = false
	result.IsShallow = false

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

	// Create destination directory
	if err := os.MkdirAll(req.Dest, 0755); err != nil {
		return PrepareResult{}, fmt.Errorf("failed to create destination directory: %w", err)
	}

	// Copy the source directory
	if err := copyDir(req.Source, req.Dest); err != nil {
		return PrepareResult{}, fmt.Errorf("failed to copy directory: %w", err)
	}

	// If source was a git repo, try to get the HEAD SHA before we lose the git dir
	sourceIsGit := IsGitRepo(req.Source)
	headSHA := ""
	if sourceIsGit {
		// Try to get HEAD SHA from source
		if sha, err := getHeadSHAContext(ctx, req.Source); err == nil {
			headSHA = sha
			result.HeadSHA = sha
		}
	}

	// Remove .git directory if it was copied (to ensure it's a true snapshot)
	gitDir := filepath.Join(req.Dest, ".git")
	if _, err := os.Stat(gitDir); err == nil {
		if err := os.RemoveAll(gitDir); err != nil {
			result.Notes = append(result.Notes, fmt.Sprintf("Warning: failed to remove .git directory: %v", err))
		}
	}

	// If the source was a git repo, initialize a minimal git repository
	// This enables git operations inside the container even without history
	if sourceIsGit {
		if err := p.initMinimalGit(ctx, req.Dest, headSHA); err != nil {
			result.Notes = append(result.Notes, fmt.Sprintf("Warning: failed to initialize minimal git: %v", err))
		}
	}

	// Handle ref checkout - this won't work without history, but we can note it
	if req.Ref != "" && req.Ref != "HEAD" {
		result.Notes = append(result.Notes, fmt.Sprintf("Note: ref '%s' was not checked out (no history available)", req.Ref))
	}

	// Write workspace manifest
	if err := WriteManifest(req.Dest, result); err != nil {
		return PrepareResult{}, fmt.Errorf("failed to write workspace manifest: %w", err)
	}

	return result, nil
}

// Cleanup removes the workspace directory
func (p *SnapshotPreparer) Cleanup(dest string) error {
	return os.RemoveAll(dest)
}

// initMinimalGit initializes a minimal git repository for a snapshot
// This allows git commands to work inside the container even without history
func (p *SnapshotPreparer) initMinimalGit(ctx context.Context, dir string, headSHA string) error {
	// Initialize git repo
	cmd := exec.CommandContext(ctx, "git", "-C", dir, "init")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git init failed: %v, output: %s", err, string(out))
	}

	// Configure user (required for commits)
	cmd = exec.CommandContext(ctx, "git", "-C", dir, "config", "user.email", "holon@holon.run")
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("git config user.email failed: %w", err)
	}

	cmd = exec.CommandContext(ctx, "git", "-C", dir, "config", "user.name", "Holon")
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("git config user.name failed: %w", err)
	}

	// Add all files
	cmd = exec.CommandContext(ctx, "git", "-C", dir, "add", "-A")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git add failed: %v, output: %s", err, string(out))
	}

	// Create initial commit
	if headSHA != "" {
		// Try to preserve the original commit message if available
		cmd = exec.CommandContext(ctx, "git", "-C", dir, "commit", "-m", fmt.Sprintf("Holon snapshot\n\nOriginal commit: %s", headSHA))
	} else {
		cmd = exec.CommandContext(ctx, "git", "-C", dir, "commit", "-m", "Holon snapshot")
	}

	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git commit failed: %v, output: %s", err, string(out))
	}

	return nil
}
