package githubpr

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"

	"github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/config"
	"github.com/go-git/go-git/v5/plumbing"
	"github.com/go-git/go-git/v5/plumbing/object"
	"github.com/go-git/go-git/v5/plumbing/transport/http"
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

// ApplyPatch applies a patch file to the workspace.
func (g *GitClient) ApplyPatch(patchPath string) error {
	// Verify patch file exists
	if _, err := os.Stat(patchPath); err != nil {
		return fmt.Errorf("patch file not found: %w", err)
	}

	// Use git apply command with proper exec.Command to avoid injection
	checkCmd := exec.Command("git", "apply", "--check", patchPath)
	checkCmd.Dir = g.WorkspaceDir
	if output, err := checkCmd.CombinedOutput(); err != nil {
		return fmt.Errorf("patch check failed: %w (the workspace may not be a git repository or patch may not apply): %s", err, strings.TrimSpace(string(output)))
	}

	applyCmd := exec.Command("git", "apply", patchPath)
	applyCmd.Dir = g.WorkspaceDir
	if output, err := applyCmd.CombinedOutput(); err != nil {
		return fmt.Errorf("failed to apply patch: %w: %s", err, strings.TrimSpace(string(output)))
	}

	return nil
}

// CreateBranch creates a new branch or checks out existing one.
func (g *GitClient) CreateBranch(branchName string) error {
	repo, err := git.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Check if branch already exists
	_, err = repo.Branch(branchName)
	if err == nil {
		// Branch exists, checkout it
		worktree, err := repo.Worktree()
		if err != nil {
			return fmt.Errorf("failed to get worktree: %w", err)
		}

		err = worktree.Checkout(&git.CheckoutOptions{
			Branch: plumbing.NewBranchReferenceName(branchName),
		})
		if err != nil {
			return fmt.Errorf("failed to checkout branch %s: %w", branchName, err)
		}

		return nil
	}

	// Branch doesn't exist, create it
	worktree, err := repo.Worktree()
	if err != nil {
		return fmt.Errorf("failed to get worktree: %w", err)
	}

	// Create and checkout new branch
	err = worktree.Checkout(&git.CheckoutOptions{
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
	repo, err := git.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return "", fmt.Errorf("failed to open repository: %w", err)
	}

	worktree, err := repo.Worktree()
	if err != nil {
		return "", fmt.Errorf("failed to get worktree: %w", err)
	}

	// Stage all changes
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
	commit, err := worktree.Commit(message, &git.CommitOptions{
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
	repo, err := git.PlainOpen(g.WorkspaceDir)
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
	err = remote.Push(&git.PushOptions{
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
	repo, err := git.PlainOpen(g.WorkspaceDir)
	if err != nil {
		if err == git.ErrRepositoryNotExists {
			return fmt.Errorf("workspace is not a git repository")
		}
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Only validate that it's a git repository, don't check for clean workspace
	// Users may have uncommitted or untracked files that aren't part of this PR
	_ = repo
	return nil
}
