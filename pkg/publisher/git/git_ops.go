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

	// AuthorName is the git author name for commits
	AuthorName string

	// AuthorEmail is the git author email for commits
	AuthorEmail string
}

// NewGitClient creates a new Git client.
func NewGitClient(workspaceDir, token, authorName, authorEmail string) *GitClient {
	return &GitClient{
		WorkspaceDir: workspaceDir,
		Token:        token,
		AuthorName:   authorName,
		AuthorEmail:  authorEmail,
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

	// Determine author name and email (use defaults if not configured)
	authorName := g.AuthorName
	if authorName == "" {
		authorName = "Holon Bot"
	}
	authorEmail := g.AuthorEmail
	if authorEmail == "" {
		authorEmail = "bot@holon.run"
	}

	// Configure git user.name and user.email BEFORE committing
	// This sets both author and committer identity.
	// Use --local flag to only set repository-level config (doesn't affect global config).
	// Use git config --get to check if already set, preserving user's existing config if present.
	currentName, _ := client.ExecCommand(ctx, "config", "--local", "--get", "user.name")
	if strings.TrimSpace(string(currentName)) == "" {
		if _, err := client.ExecCommand(ctx, "config", "--local", "user.name", authorName); err != nil {
			return "", fmt.Errorf("failed to configure git user.name: %w", err)
		}
	}
	currentEmail, _ := client.ExecCommand(ctx, "config", "--local", "--get", "user.email")
	if strings.TrimSpace(string(currentEmail)) == "" {
		if _, err := client.ExecCommand(ctx, "config", "--local", "user.email", authorEmail); err != nil {
			return "", fmt.Errorf("failed to configure git user.email: %w", err)
		}
	}

	// Commit changes
	commitSHA, err := client.CommitWith(ctx, holonGit.CommitOptions{
		Message: message,
		Author: &holonGit.CommitAuthor{
			Name:  authorName,
			Email: authorEmail,
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

	// Validate token is not empty
	if g.Token == "" {
		return fmt.Errorf("git token is empty: please set GIT_TOKEN or repository token")
	}

	// Configure git credentials by updating the remote URL to include the token.
	// This is more reliable than http.extraheader for git push operations.
	// Format: https://x-access-token:TOKEN@github.com/owner/repo.git

	// Get the current remote URL
	remoteURL, err := client.ExecCommand(ctx, "config", "--local", "--get", "remote.origin.url")
	if err != nil {
		return fmt.Errorf("failed to get remote URL: %w", err)
	}

	currentURL := strings.TrimSpace(string(remoteURL))
	holonlog.Debug("current remote URL", "url", currentURL)

	// Check if URL already has token embedded
	// Look for the pattern "://credentials@" which indicates embedded auth
	if strings.Contains(currentURL, "://") && strings.Contains(currentURL, "@") {
		// Find the position of "://" and "@" to verify there's something between them
		schemeEnd := strings.Index(currentURL, "://")
		atPos := strings.Index(currentURL, "@")
		if schemeEnd != -1 && atPos != -1 && atPos > schemeEnd+3 {
			// Has embedded credentials
			holonlog.Debug("remote URL already has embedded credentials", "url_prefix", safeTruncate(currentURL, 30))
			return nil
		}
	}

	// Embed token in URL
	// Support any HTTPS/HTTP GitHub URL (not just github.com)
	var tokenEmbeddedURL string
	if strings.HasPrefix(currentURL, "https://") {
		// HTTPS URL: https://HOST/PATH -> https://x-access-token:TOKEN@HOST/PATH
		// This works for github.com, GitHub Enterprise, and compatible services
		tokenEmbeddedURL = strings.Replace(currentURL, "https://", fmt.Sprintf("https://x-access-token:%s@", g.Token), 1)
	} else if strings.HasPrefix(currentURL, "http://") {
		// HTTP URL (less common, but supported)
		tokenEmbeddedURL = strings.Replace(currentURL, "http://", fmt.Sprintf("http://x-access-token:%s@", g.Token), 1)
	} else if strings.HasPrefix(currentURL, "git@") {
		// SSH URL: git@github.com:owner/repo.git
		// Parse and convert to HTTPS with embedded token
		sshPart := strings.TrimPrefix(currentURL, "git@")
		hostAndPath := strings.SplitN(sshPart, ":", 2)
		if len(hostAndPath) != 2 {
			return fmt.Errorf("unsupported SSH remote URL format: %s", currentURL)
		}
		host := hostAndPath[0]
		repoPath := strings.TrimSuffix(hostAndPath[1], ".git")
		tokenEmbeddedURL = fmt.Sprintf("https://x-access-token:%s@%s/%s.git", g.Token, host, repoPath)
		holonlog.Debug("converted SSH URL to HTTPS with token", "host", host, "repo_path", repoPath)
	} else {
		return fmt.Errorf("unsupported remote URL format: %s (expected HTTPS, HTTP, or SSH)", currentURL)
	}

	holonlog.Debug("updating remote URL with embedded token", "url_prefix", safeTruncate(tokenEmbeddedURL, 40))

	// Update the remote URL
	_, err = client.ExecCommand(ctx, "config", "--local", "remote.origin.url", tokenEmbeddedURL)
	if err != nil {
		return fmt.Errorf("failed to update remote URL with token: %w", err)
	}

	// Verify the URL was updated
	verifyURL, err := client.ExecCommand(ctx, "config", "--local", "--get", "remote.origin.url")
	if err != nil {
		holonlog.Warn("failed to verify remote URL update", "error", err)
	} else {
		verifyStr := strings.TrimSpace(string(verifyURL))
		// Truncate for logging to avoid exposing full token
		holonlog.Debug("remote URL updated successfully", "url_prefix", safeTruncate(verifyStr, 50))
	}

	return nil
}

// safeTruncate safely truncates a string to the specified length for logging.
// If the string is shorter than maxLength, returns the entire string.
func safeTruncate(s string, maxLength int) string {
	if len(s) < maxLength {
		return s
	}
	return s[:maxLength] + "..."
}

// EnsureRepository ensures the workspace is a Git repository.
func (g *GitClient) EnsureRepository() error {
	client := holonGit.NewClient(g.WorkspaceDir)

	if !client.IsRepo(context.Background()) {
		return fmt.Errorf("workspace is not a git repository")
	}

	return nil
}
