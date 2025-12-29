package git

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	holonGit "github.com/holon-run/holon/pkg/git"
	holonlog "github.com/holon-run/holon/pkg/log"
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

	// Stage the applied changes
	if err := client.AddAll(ctx); err != nil {
		return false, fmt.Errorf("failed to stage changes after patch: %w", err)
	}

	return true, nil
}

// CreateBranch creates a new branch or checks out an existing one.
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
// Returns the commit hash if successful.
func (g *GitClient) CommitChanges(ctx context.Context, message string) (string, error) {
	client := holonGit.NewClient(g.WorkspaceDir)

	// Stage all changes.
	// Note: ApplyPatch already runs `git add -A`, so this can be redundant in that flow.
	// We still stage here so CommitChanges is safe to call independently of ApplyPatch.
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

// Push pushes the current branch to the specified remote.
func (g *GitClient) Push(branchName, remoteName string) error {
	client := holonGit.NewClient(g.WorkspaceDir)

	pushCtx := context.Background()

	// Configure credentials if token is provided
	if g.Token != "" {
		if err := g.configureGitCredentials(pushCtx); err != nil {
			return fmt.Errorf("failed to configure git credentials: %w", err)
		}
	}

	// Push using system git
	if err := client.Push(pushCtx, holonGit.PushOptions{
		Remote:     remoteName,
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

	// Configure git to use the token via a generic HTTP extra header
	// Note: This persists credentials in .git/config. A more secure approach would use
	// GIT_ASKPASS with temporary helpers, which is deferred to a follow-up improvement.
	authHeader := fmt.Sprintf("Authorization: Bearer %s", g.Token)

	_, err := client.ExecCommand(ctx, "config", "--local", "http.extraheader", authHeader)
	if err != nil {
		// Non-fatal error - continue with push attempt
		holonlog.Warn("failed to configure git credential helper", "error", err)
	}

	return nil
}

// EnsureRepository ensures the workspace is a Git repository.
func (g *GitClient) EnsureRepository() error {
	client := holonGit.NewClient(g.WorkspaceDir)

	if !client.IsRepo(context.Background()) {
		return fmt.Errorf("workspace is not a git repository")
	}

	return nil
}
