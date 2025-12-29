package githubpr

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	holonGit "github.com/holon-run/holon/pkg/git"
	holonlog "github.com/holon-run/holon/pkg/log"
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
	if err := client.AddAll(ctx); err != nil {
		return fmt.Errorf("failed to stage changes after patch: %w", err)
	}

	return nil
}

// CreateBranch creates a new branch or checks out existing one.
//
// NOTE: This function performs destructive git operations (reset --hard, clean -fd)
// to ensure the worktree is clean before branch creation. This is safe for publish
// workflows where the workspace should be in a clean state, but could discard
// uncommitted changes if called in other contexts.
func (g *GitClient) CreateBranch(ctx context.Context, branchName string) error {
	gitClient := holonGit.NewClient(g.WorkspaceDir)

	holonlog.Debug("preparing to create branch", "branch", branchName, "workspace", g.WorkspaceDir)

	// Reset working tree
	if _, err := gitClient.ExecCommand(ctx, "reset", "--hard", "HEAD"); err != nil {
		// Preserve previous lenient behavior: log warning and continue.
		holonlog.Warn("failed to reset worktree", "branch", branchName, "error", err)
	} else {
		holonlog.Debug("reset worktree successfully", "branch", branchName)
	}

	// Clean untracked files
	if _, err := gitClient.ExecCommand(ctx, "clean", "-fd"); err != nil {
		holonlog.Warn("failed to clean untracked files", "error", err)
	}

	// Verify working tree is clean
	if !gitClient.IsClean(ctx) {
		// Log diagnostic information
		diagnostics := gitClient.DiagnoseWorkingTree(ctx)
		holonlog.Error("working tree still dirty after reset and clean",
			"branch", branchName,
			"diagnostics", diagnostics)
		return fmt.Errorf("working tree is dirty after reset and clean, cannot create branch")
	}

	// Check if branch already exists
	branchExists := false
	if _, err := gitClient.ExecCommand(ctx, "rev-parse", "--verify", "refs/heads/"+branchName); err == nil {
		branchExists = true
	}

	if branchExists {
		// Checkout existing branch
		holonlog.Debug("checking out existing branch", "branch", branchName)
		if _, err := gitClient.ExecCommand(ctx, "checkout", branchName); err != nil {
			return fmt.Errorf("failed to checkout branch %s: %w", branchName, err)
		}
		return nil
	}

	// Create and checkout new branch
	holonlog.Debug("creating new branch", "branch", branchName)
	if _, err := gitClient.ExecCommand(ctx, "checkout", "-b", branchName); err != nil {
		return fmt.Errorf("failed to create branch %s: %w", branchName, err)
	}

	holonlog.Info("created branch successfully", "branch", branchName)
	return nil
}

// CommitChanges commits all changes with the given message.
func (g *GitClient) CommitChanges(ctx context.Context, message string) (string, error) {
	client := holonGit.NewClient(g.WorkspaceDir)

	// Force stage all changes (including untracked files)
	if err := client.AddAll(ctx); err != nil {
		return "", fmt.Errorf("failed to stage changes: %w", err)
	}

	// Check if there are any changes to commit
	if client.IsClean(ctx) {
		return "", fmt.Errorf("no changes to commit")
	}

	// Commit changes
	commitSHA, err := client.CommitWith(ctx, holonGit.CommitOptions{
		Message: message,
		Author: &holonGit.CommitAuthor{
			Name:  "Holon Bot",
			Email: "bot@holon.run",
			When:  time.Now(),
		},
	})
	if err != nil {
		return "", fmt.Errorf("failed to commit: %w", err)
	}

	return commitSHA, nil
}

// Push pushes the current branch to remote.
func (g *GitClient) Push(branchName string) error {
	client := holonGit.NewClient(g.WorkspaceDir)

	// Configure git credentials for push
	pushCtx := context.Background()

	if err := g.configureGitCredentials(pushCtx); err != nil {
		return fmt.Errorf("failed to configure git credentials: %w", err)
	}

	// Push using system git with auth configured
	if err := client.Push(pushCtx, holonGit.PushOptions{
		Remote:     "origin",
		Branch:     branchName,
		SetUpstream: true,
	}); err != nil {
		return fmt.Errorf("failed to push branch: %w", err)
	}

	return nil
}

// configureGitCredentials configures git to use the token for authentication.
func (g *GitClient) configureGitCredentials(ctx context.Context) error {
	client := holonGit.NewClient(g.WorkspaceDir)

	// Configure git to use the token via the extraheader
	// This is a common pattern for GitHub authentication.
	// Note: This persists credentials in .git/config. A more secure approach would use
	// GIT_ASKPASS with temporary helpers, which is deferred to a follow-up improvement.
	authHeader := fmt.Sprintf("Authorization: Bearer %s", g.Token)

	_, err := client.ExecCommand(ctx, "config", "--local", "http.https://github.com/.extraheader", authHeader)
	if err != nil {
		// Non-fatal error - continue with push attempt
		holonlog.Warn("failed to configure git credential helper", "error", err)
	}

	return nil
}

// EnsureCleanWorkspace ensures the workspace is a Git repository.
func (g *GitClient) EnsureCleanWorkspace() error {
	client := holonGit.NewClient(g.WorkspaceDir)

	if !client.IsRepo(context.Background()) {
		return fmt.Errorf("workspace is not a git repository")
	}

	return nil
}
