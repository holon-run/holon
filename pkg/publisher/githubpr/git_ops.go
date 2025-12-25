package githubpr

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"

	"github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/plumbing"
	"github.com/go-git/go-git/v5/plumbing/object"
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

	// Use git apply command (more reliable than go-git for complex patches)
	cmd := fmt.Sprintf("cd %s && git apply --check %s", g.WorkspaceDir, patchPath)
	if err := runCommand(cmd); err != nil {
		return fmt.Errorf("patch check failed: %w (the workspace may not be a git repository or patch may not apply)", err)
	}

	cmd = fmt.Sprintf("cd %s && git apply %s", g.WorkspaceDir, patchPath)
	if err := runCommand(cmd); err != nil {
		return fmt.Errorf("failed to apply patch: %w", err)
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
	// Configure the remote URL with token
	repo, err := git.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return fmt.Errorf("failed to open repository: %w", err)
	}

	// Get or set the remote auth
	_, err = repo.Remote("origin")
	if err != nil {
		return fmt.Errorf("failed to get remote: %w", err)
	}

	// Use git push with token (more reliable for auth)
	// The token should be set via GIT_ASKPASS or embedded in URL
	cmd := fmt.Sprintf("cd %s && git push https://x-access-token:%s@github.com/ $(git config --get remote.origin.url | sed 's/.*github.com[:/]//') %s",
		g.WorkspaceDir, g.Token, branchName)

	if err := runCommand(cmd); err != nil {
		return fmt.Errorf("failed to push branch: %w", err)
	}

	return nil
}

// GetCurrentBranch returns the current branch name.
func (g *GitClient) GetCurrentBranch() (string, error) {
	repo, err := git.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return "", fmt.Errorf("failed to open repository: %w", err)
	}

	head, err := repo.Head()
	if err != nil {
		return "", fmt.Errorf("failed to get HEAD: %w", err)
	}

	if !head.Name().IsBranch() {
		return "", fmt.Errorf("HEAD is not on a branch")
	}

	return head.Name().Short(), nil
}

// EnsureCleanWorkspace ensures the workspace is a Git repository and is clean.
func (g *GitClient) EnsureCleanWorkspace() error {
	repo, err := git.PlainOpen(g.WorkspaceDir)
	if err != nil {
		if err == git.ErrRepositoryNotExists {
			return fmt.Errorf("workspace is not a git repository")
		}
		return fmt.Errorf("failed to open repository: %w", err)
	}

	worktree, err := repo.Worktree()
	if err != nil {
		return fmt.Errorf("failed to get worktree: %w", err)
	}

	status, err := worktree.Status()
	if err != nil {
		return fmt.Errorf("failed to get status: %w", err)
	}

	if !status.IsClean() {
		return fmt.Errorf("workspace has uncommitted changes")
	}

	return nil
}

// GetCurrentRemoteURL gets the current remote URL for the repository.
func (g *GitClient) GetCurrentRemoteURL() (string, error) {
	repo, err := git.PlainOpen(g.WorkspaceDir)
	if err != nil {
		return "", fmt.Errorf("failed to open repository: %w", err)
	}

	remote, err := repo.Remote("origin")
	if err != nil {
		return "", fmt.Errorf("failed to get remote: %w", err)
	}

	if remote == nil || len(remote.Config().URLs) == 0 {
		return "", fmt.Errorf("no remote URL configured")
	}

	return remote.Config().URLs[0], nil
}

// FetchAndMergeBase fetches from origin and merges with base branch.
func (g *GitClient) FetchAndMergeBase(baseBranch string) error {
	// Fetch latest from origin
	cmd := fmt.Sprintf("cd %s && git fetch origin %s", g.WorkspaceDir, baseBranch)
	if err := runCommand(cmd); err != nil {
		return fmt.Errorf("failed to fetch from origin: %w", err)
	}

	// Merge base branch into current branch
	cmd = fmt.Sprintf("cd %s && git merge origin/%s -m 'Merge base branch %s'", g.WorkspaceDir, baseBranch, baseBranch)
	if err := runCommand(cmd); err != nil {
		return fmt.Errorf("failed to merge base branch: %w", err)
	}

	return nil
}

// runCommand executes a shell command.
func runCommand(cmd string) error {
	// Parse command into parts
	parts := strings.Fields(cmd)
	if len(parts) == 0 {
		return fmt.Errorf("empty command")
	}

	// Handle shell operators like cd and pipes
	if strings.Contains(cmd, "cd ") && strings.Contains(cmd, "&&") {
		// Execute using bash
		execCmd := exec.Command("bash", "-c", cmd)
		output, err := execCmd.CombinedOutput()
		if err != nil {
			return fmt.Errorf("command failed: %s\noutput: %s", err, string(output))
		}
		return nil
	}

	// Simple command execution
	execCmd := exec.Command(parts[0], parts[1:]...)
	output, err := execCmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("command failed: %s\noutput: %s", err, string(output))
	}
	return nil
}
