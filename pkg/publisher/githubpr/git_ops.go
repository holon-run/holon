package githubpr

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	gogit "github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/config"
	"github.com/go-git/go-git/v5/plumbing"
	"github.com/go-git/go-git/v5/plumbing/object"
	"github.com/go-git/go-git/v5/plumbing/transport/http"

	holonGit "github.com/holon-run/holon/pkg/git"
)

// GitClient handles Git operations for PR creation.
type GitClient struct {
	// WorkspaceDir is the path to the Git workspace
	WorkspaceDir string

	// Token is the GitHub authentication token
	Token string
}

// NewGitClient creates a new Git client.
func NewGitClient(workspaceDir, token string) *GitClient {
	return &GitClient{
		WorkspaceDir: workspaceDir,
		Token:        token,
	}
}

// ApplyPatch applies a patch file to the workspace and stages all changes.
func (g *GitClient) ApplyPatch(ctx context.Context, patchPath string) error {
	// Verify patch file exists
	info, err := os.Stat(patchPath)
	if err != nil {
		return fmt.Errorf("patch file not found: %w", err)
	}
	if info.Size() == 0 {
		return nil
	}

	// Guard against whitespace-only patches (git apply treats them as invalid)
	payload, err := os.ReadFile(patchPath)
	if err != nil {
		return fmt.Errorf("failed to read patch file: %w", err)
	}
	if strings.TrimSpace(string(payload)) == "" {
		return nil
	}

	client := holonGit.NewClient(g.WorkspaceDir)

	// Use git apply command with proper exec.Command to avoid injection
	if err := client.ApplyCheck(ctx, patchPath, false); err != nil {
		return fmt.Errorf("patch check failed: %w (the workspace may not be a git repository or patch may not apply)", err)
	}

	if err := client.Apply(ctx, holonGit.ApplyOptions{
		PatchPath: patchPath,
		ThreeWay:  false,
	}); err != nil {
		return fmt.Errorf("failed to apply patch: %w", err)
	}

	// IMPORTANT: Stage changes immediately after applying patch
	// This ensures all patch changes are tracked and preserved for subsequent Git operations.
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return fmt.Errorf("failed to open repository: %w", err)
	}

	worktree, err := repo.Worktree()
	if err != nil {
		return fmt.Errorf("failed to get worktree: %w", err)
	}

	if _, err := worktree.Add("."); err != nil {
		return fmt.Errorf("failed to stage changes after patch: %w", err)
	}

	return nil
}

// CreateBranch creates a new branch or checks out existing one.
func (g *GitClient) CreateBranch(branchName string) error {
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Clean working tree before branch operations to avoid "worktree contains unstaged changes" errors
	// This is particularly important in CI environments where file metadata or permissions may differ
	worktree, err := repo.Worktree()
	if err != nil {
		return fmt.Errorf("failed to get worktree: %w", err)
	}

	// Reset any unstaged changes to ensure clean working tree
	// Use Force to discard all changes (similar to git reset --hard HEAD)
	err = worktree.Reset(&gogit.ResetOptions{
		Mode: gogit.HardReset,
	})
	if err != nil {
		// Log warning but continue - reset failure shouldn't block branch creation
		// This might happen if there are no changes to reset
		fmt.Printf("Warning: failed to reset worktree (continuing anyway): %v\n", err)
	}

	// Check if branch already exists
	_, err = repo.Branch(branchName)
	if err == nil {
		// Branch exists, checkout it
		err = worktree.Checkout(&gogit.CheckoutOptions{
			Branch: plumbing.NewBranchReferenceName(branchName),
		})
		if err != nil {
			return fmt.Errorf("failed to checkout branch %s: %w", branchName, err)
		}

		return nil
	}

	// Branch doesn't exist, create it
	// Create and checkout new branch
	err = worktree.Checkout(&gogit.CheckoutOptions{
		Branch: plumbing.NewBranchReferenceName(branchName),
		Create: true,
	})
	if err != nil {
		return fmt.Errorf("failed to create branch %s: %w", branchName, err)
	}

	return nil
}

// CommitChanges commits all changes with the given message.
func (g *GitClient) CommitChanges(message string) (string, error) {
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return "", fmt.Errorf("failed to open repository: %w", err)
	}

	worktree, err := repo.Worktree()
	if err != nil {
		return "", fmt.Errorf("failed to get worktree: %w", err)
	}

	// Force stage all changes (including untracked files)
	_, err = worktree.Add(".")
	if err != nil {
		return "", fmt.Errorf("failed to stage changes: %w", err)
	}

	// Check if there are any changes to commit
	status, err := worktree.Status()
	if err != nil {
		return "", fmt.Errorf("failed to get status: %w", err)
	}

	if status.IsClean() {
		return "", fmt.Errorf("no changes to commit")
	}

	// Commit changes
	commit, err := worktree.Commit(message, &gogit.CommitOptions{
		Author: &object.Signature{
			Name:  "Holon Bot",
			Email: "bot@holon.run",
			When:  time.Now(),
		},
	})
	if err != nil {
		return "", fmt.Errorf("failed to commit: %w", err)
	}

	return commit.String(), nil
}

// Push pushes the current branch to remote.
func (g *GitClient) Push(branchName string) error {
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Get the remote to push to
	remote, err := repo.Remote("origin")
	if err != nil {
		return fmt.Errorf("failed to get remote: %w", err)
	}

	// Create auth callback using the token
	auth := &http.BasicAuth{
		Username: "x-access-token", // GitHub requires this for token auth
		Password: g.Token,
	}

	// Push using go-git to avoid command injection
	err = remote.Push(&gogit.PushOptions{
		RemoteName: "origin",
		RefSpecs:   []config.RefSpec{config.RefSpec("refs/heads/" + branchName + ":refs/heads/" + branchName)},
		Auth:       auth,
	})
	if err != nil {
		return fmt.Errorf("failed to push branch: %w", err)
	}

	return nil
}

// EnsureCleanWorkspace ensures the workspace is a Git repository.
func (g *GitClient) EnsureCleanWorkspace() error {
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		if err == gogit.ErrRepositoryNotExists {
			return fmt.Errorf("workspace is not a git repository")
		}
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Only validate that it's a git repository, don't check for clean workspace
	// Users may have uncommitted or untracked files that aren't part of this PR
	_ = repo
	return nil
}
