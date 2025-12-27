package git

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

// GitClient handles Git operations.
type GitClient struct {
	// WorkspaceDir is the path to the Git workspace
	WorkspaceDir string

	// Token is the optional authentication token for push operations
	Token string
}

// NewGitClient creates a new Git client.
func NewGitClient(workspaceDir, token string) *GitClient {
	return &GitClient{
		WorkspaceDir: workspaceDir,
		Token:        token,
	}
}

// ApplyPatch applies a patch file to the workspace using git apply --3way.
// Returns true if patch was applied, false if patch file was empty/missing.
func (g *GitClient) ApplyPatch(ctx context.Context, patchPath string) (bool, error) {
	// Check if patch file exists
	info, err := os.Stat(patchPath)
	if err != nil {
		if os.IsNotExist(err) {
			// Patch file doesn't exist - this is a no-op case
			return false, nil
		}
		return false, fmt.Errorf("failed to check patch file: %w", err)
	}

	// Check if patch file is empty
	if info.Size() == 0 {
		// Empty patch file - this is a no-op case
		return false, nil
	}

	// Guard against whitespace-only patches (git apply treats them as invalid)
	payload, err := os.ReadFile(patchPath)
	if err != nil {
		return false, fmt.Errorf("failed to read patch file: %w", err)
	}
	if strings.TrimSpace(string(payload)) == "" {
		return false, nil
	}

	client := holonGit.NewClient(g.WorkspaceDir)

	// Use git apply command with --3way for better merge behavior
	if err := client.ApplyCheck(ctx, patchPath, true); err != nil {
		return false, fmt.Errorf("patch check failed: %w (the workspace may not be a git repository or patch may not apply)", err)
	}

	if err := client.Apply(ctx, holonGit.ApplyOptions{
		PatchPath: patchPath,
		ThreeWay:  true,
	}); err != nil {
		return false, fmt.Errorf("failed to apply patch: %w", err)
	}

	// Stage the applied changes so go-git can see them
	if err := client.AddAll(ctx); err != nil {
		return false, fmt.Errorf("failed to stage changes after patch: %w", err)
	}

	return true, nil
}

// CreateBranch creates a new branch or checks out an existing one.
func (g *GitClient) CreateBranch(branchName string) error {
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Check if branch already exists
	_, err = repo.Branch(branchName)
	if err == nil {
		// Branch exists, check it out
		worktree, err := repo.Worktree()
		if err != nil {
			return fmt.Errorf("failed to get worktree: %w", err)
		}

		err = worktree.Checkout(&gogit.CheckoutOptions{
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
// Returns the commit hash if successful.
func (g *GitClient) CommitChanges(message string) (string, error) {
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return "", fmt.Errorf("failed to open repository: %w", err)
	}

	worktree, err := repo.Worktree()
	if err != nil {
		return "", fmt.Errorf("failed to get worktree: %w", err)
	}

	// Stage all changes.
	// Note: ApplyPatch already runs `git add -A`, so this can be redundant in that flow.
	// We still stage here so CommitChanges is safe to call independently of ApplyPatch.
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

// Push pushes the current branch to the specified remote.
func (g *GitClient) Push(branchName, remoteName string) error {
	repo, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Get the remote to push to
	remote, err := repo.Remote(remoteName)
	if err != nil {
		return fmt.Errorf("failed to get remote '%s': %w", remoteName, err)
	}

	// Create push options
	opts := &gogit.PushOptions{
		RemoteName: remoteName,
		RefSpecs:   []config.RefSpec{config.RefSpec("refs/heads/" + branchName + ":refs/heads/" + branchName)},
	}

	// Add authentication if token is provided
	if g.Token != "" {
		opts.Auth = &http.BasicAuth{
			Username: "x-access-token", // Generic token auth convention
			Password: g.Token,
		}
	}

	// Push
	err = remote.Push(opts)
	if err != nil {
		return fmt.Errorf("failed to push branch: %w", err)
	}

	return nil
}

// EnsureRepository ensures the workspace is a Git repository.
func (g *GitClient) EnsureRepository() error {
	_, err := gogit.PlainOpen(g.WorkspaceDir)
	if err != nil {
		if err == gogit.ErrRepositoryNotExists {
			return fmt.Errorf("workspace is not a git repository")
		}
		return fmt.Errorf("failed to open repository: %w", err)
	}

	return nil
}
