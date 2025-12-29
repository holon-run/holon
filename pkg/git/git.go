// Package git provides a shared utility layer for git operations.
// It wraps system git commands, providing a consistent API for use across
// workspace preparers and publishers. The design allows for future migration
// to go-git or a hybrid approach without changing the consumer API.
package git

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

// Client represents a git client for operations on a repository.
type Client struct {
	// Dir is the working directory of the git repository.
	Dir string

	// AuthToken is the optional authentication token for push operations.
	AuthToken string

	// Options provides optional git configuration.
	Options *ClientOptions
}

// ClientOptions holds configuration for git operations.
type ClientOptions struct {
	// UserName is the git user name for commits.
	UserName string

	// UserEmail is the git user email for commits.
	UserEmail string

	// Quiet suppresses output from git commands.
	Quiet bool

	// DryRun logs commands without executing them.
	DryRun bool
}

// DefaultClientOptions returns the default client options.
func DefaultClientOptions() *ClientOptions {
	return &ClientOptions{
		UserName:  "Holon Bot",
		UserEmail: "bot@holon.run",
		Quiet:     true,
		DryRun:    false,
	}
}

// NewClient creates a new git client for the given directory.
func NewClient(dir string) *Client {
	return &Client{
		Dir:     dir,
		Options: DefaultClientOptions(),
	}
}

// NewClientWithToken creates a new git client with authentication.
func NewClientWithToken(dir, token string) *Client {
	return &Client{
		Dir:       dir,
		AuthToken: token,
		Options:   DefaultClientOptions(),
	}
}

// RepositoryInfo holds information about a git repository.
type RepositoryInfo struct {
	// HEAD is the current commit SHA.
	HEAD string

	// IsShallow indicates if the repository is a shallow clone.
	IsShallow bool

	// Branch is the current branch name (empty if detached HEAD).
	Branch string

	// Clean indicates if the working directory has no changes.
	Clean bool
}

// CloneOptions specifies options for cloning a repository.
type CloneOptions struct {
	// Source is the repository URL or path to clone from.
	Source string

	// Dest is the destination directory.
	Dest string

	// Ref is the reference to checkout after clone (optional).
	Ref string

	// Depth specifies shallow clone depth (0 for full history).
	Depth int

	// Local indicates to use --local for local repositories.
	Local bool

	// Submodules indicates whether to initialize submodules.
	Submodules bool

	// Quiet suppresses output.
	Quiet bool
}

// CloneResult holds the result of a clone operation.
type CloneResult struct {
	// HEAD is the checked out commit SHA.
	HEAD string

	// Branch is the checked out branch name.
	Branch string

	// IsShallow indicates if the clone is shallow.
	IsShallow bool
}

// execCommand executes a git command with proper error handling.
func (c *Client) execCommand(ctx context.Context, args ...string) ([]byte, error) {
	return c.execCommandWithDir(ctx, c.Dir, args...)
}

// ExecCommand is a safe wrapper to allow callers to run arbitrary git commands.
func (c *Client) ExecCommand(ctx context.Context, args ...string) ([]byte, error) {
	return c.execCommand(ctx, args...)
}

// execCommandWithDir executes a git command in a specific directory.
func (c *Client) execCommandWithDir(ctx context.Context, dir string, args ...string) ([]byte, error) {
	if c.Options != nil && c.Options.DryRun {
		return nil, fmt.Errorf("dry run: git %s (dir: %s)", strings.Join(args, " "), dir)
	}

	cmdArgs := []string{"-C", dir}
	cmdArgs = append(cmdArgs, args...)

	cmd := exec.CommandContext(ctx, "git", cmdArgs...)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return output, fmt.Errorf("git %s failed: %w: %s", strings.Join(args, " "), err, strings.TrimSpace(string(output)))
	}

	return output, nil
}

// quietFlag returns the --quiet flag if enabled.
func (c *Client) quietFlag() string {
	if c.Options != nil && c.Options.Quiet {
		return "--quiet"
	}
	return ""
}

// Clone clones a repository.
func Clone(ctx context.Context, opts CloneOptions) (*CloneResult, error) {
	// Build clone arguments
	args := []string{"clone"}

	if opts.Quiet {
		args = append(args, "--quiet")
	}

	args = append(args, "--no-checkout")

	// Handle depth for shallow clone
	if opts.Depth > 0 {
		args = append(args, "--depth", fmt.Sprintf("%d", opts.Depth))
	}

	// Use --local for local repositories
	if opts.Local {
		args = append(args, "--local")
	}

	// Source and destination
	args = append(args, opts.Source, opts.Dest)

	// Execute git clone
	cmd := exec.CommandContext(ctx, "git", args...)
	if output, err := cmd.CombinedOutput(); err != nil {
		return nil, fmt.Errorf("git clone failed: %w: %s", err, strings.TrimSpace(string(output)))
	}

	// Verify the clone was created successfully
	client := NewClient(opts.Dest)
	if _, err := client.execCommand(ctx, "rev-parse", "--git-dir"); err != nil {
		return nil, fmt.Errorf("git clone succeeded but destination is not a git repository")
	}

	// Determine ref to checkout
	checkoutRef := opts.Ref
	if checkoutRef == "" {
		checkoutRef = "HEAD"
	}

	// Checkout the requested ref
	if err := client.Checkout(ctx, checkoutRef); err != nil {
		// Checkout failed - this is a warning, not a failure
		// The caller can decide if this is critical
		return nil, fmt.Errorf("checkout failed: %w", err)
	}

	// Handle submodules if requested
	if opts.Submodules {
		if err := client.InitSubmodules(ctx); err != nil {
			return nil, fmt.Errorf("submodule init failed: %w", err)
		}
	}

	// Get repository info
	info, err := client.GetRepositoryInfo(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to get repository info: %w", err)
	}

	return &CloneResult{
		HEAD:      info.HEAD,
		Branch:    info.Branch,
		IsShallow: info.IsShallow,
	}, nil
}

// IsRepo checks if the directory is a git repository.
func (c *Client) IsRepo(ctx context.Context) bool {
	_, err := c.execCommand(ctx, "rev-parse", "--git-dir")
	return err == nil
}

// GetRepositoryInfo returns information about the repository.
func (c *Client) GetRepositoryInfo(ctx context.Context) (*RepositoryInfo, error) {
	info := &RepositoryInfo{}

	// Get HEAD SHA
	headSHA, err := c.execCommand(ctx, "rev-parse", "HEAD")
	if err != nil {
		return nil, fmt.Errorf("failed to get HEAD SHA: %w", err)
	}
	info.HEAD = strings.TrimSpace(string(headSHA))

	// Check if shallow
	shallowOutput, err := c.execCommand(ctx, "rev-parse", "--is-shallow-repository")
	if err == nil {
		info.IsShallow = strings.TrimSpace(string(shallowOutput)) == "true"
	}

	// Get current branch
	branch, err := c.execCommand(ctx, "rev-parse", "--abbrev-ref", "HEAD")
	if err == nil {
		info.Branch = strings.TrimSpace(string(branch))
	}

	// Check if working directory is clean
	_, err = c.execCommand(ctx, "diff", "--quiet")
	if err == nil {
		info.Clean = true
	} else {
		// diff --quiet returns exit code 1 if there are differences
		info.Clean = false
	}

	return info, nil
}

// GetHeadSHA returns the current HEAD SHA.
func (c *Client) GetHeadSHA(ctx context.Context) (string, error) {
	output, err := c.execCommand(ctx, "rev-parse", "HEAD")
	if err != nil {
		return "", fmt.Errorf("failed to get HEAD SHA: %w", err)
	}
	return strings.TrimSpace(string(output)), nil
}

// IsShallowClone checks if the repository is a shallow clone.
func (c *Client) IsShallowClone(ctx context.Context) (bool, error) {
	output, err := c.execCommand(ctx, "rev-parse", "--is-shallow-repository")
	if err != nil {
		return false, fmt.Errorf("failed to check shallow status: %w", err)
	}
	return strings.TrimSpace(string(output)) == "true", nil
}

// Checkout checks out a reference (branch, tag, or commit).
func (c *Client) Checkout(ctx context.Context, ref string) error {
	args := []string{"checkout"}
	if q := c.quietFlag(); q != "" {
		args = append(args, q)
	}
	if ref != "" {
		args = append(args, ref)
	}
	_, err := c.execCommand(ctx, args...)
	return err
}

// AddWorktree adds a new worktree at the given path, optionally checking out a ref.
func (c *Client) AddWorktree(ctx context.Context, path, ref string, detach bool) error {
	args := []string{"worktree", "add"}
	if q := c.quietFlag(); q != "" {
		args = append(args, q)
	}
	if detach {
		args = append(args, "--detach")
	}
	args = append(args, path)
	if ref != "" {
		args = append(args, ref)
	}

	_, err := c.execCommand(ctx, args...)
	if err != nil {
		return fmt.Errorf("failed to add worktree at %s: %w", path, err)
	}
	return nil
}

// RemoveWorktree removes an existing worktree.
func (c *Client) RemoveWorktree(ctx context.Context, path string, force bool) error {
	args := []string{"worktree", "remove"}
	if q := c.quietFlag(); q != "" {
		args = append(args, q)
	}
	if force {
		args = append(args, "--force")
	}
	args = append(args, path)

	_, err := c.execCommand(ctx, args...)
	if err != nil {
		return fmt.Errorf("failed to remove worktree at %s: %w", path, err)
	}
	return nil
}

// Init initializes a new git repository.
func (c *Client) Init(ctx context.Context) error {
	_, err := c.execCommand(ctx, "init")
	return err
}

// SetConfig sets a git configuration value.
func (c *Client) SetConfig(ctx context.Context, key, value string) error {
	_, err := c.execCommand(ctx, "config", key, value)
	return err
}

// InitRepository initializes a git repository with default configuration.
func (c *Client) InitRepository(ctx context.Context) error {
	if err := c.Init(ctx); err != nil {
		return fmt.Errorf("failed to init repository: %w", err)
	}

	// Set default user config
	userName := c.Options.UserName
	if userName == "" {
		userName = "Holon"
	}
	if err := c.SetConfig(ctx, "user.name", userName); err != nil {
		return fmt.Errorf("failed to set user.name: %w", err)
	}

	userEmail := c.Options.UserEmail
	if userEmail == "" {
		userEmail = "holon@holon.run"
	}
	if err := c.SetConfig(ctx, "user.email", userEmail); err != nil {
		return fmt.Errorf("failed to set user.email: %w", err)
	}

	return nil
}

// Add stages files for commit.
func (c *Client) Add(ctx context.Context, args ...string) error {
	addArgs := append([]string{"add"}, args...)
	_, err := c.execCommand(ctx, addArgs...)
	return err
}

// AddAll stages all changes.
func (c *Client) AddAll(ctx context.Context) error {
	return c.Add(ctx, "-A")
}

// Commit creates a commit with the given message.
func (c *Client) Commit(ctx context.Context, message string) (string, error) {
	args := []string{"commit", "-m", message}
	_, err := c.execCommand(ctx, args...)
	if err != nil {
		return "", fmt.Errorf("commit failed: %w", err)
	}

	// Return the commit SHA
	headSHA, err := c.GetHeadSHA(ctx)
	if err != nil {
		return "", err
	}

	return headSHA, nil
}

// ApplyOptions specifies options for applying a patch.
type ApplyOptions struct {
	// PatchPath is the path to the patch file.
	PatchPath string

	// ThreeWay enables 3-way merge.
	ThreeWay bool

	// CheckOnly validates the patch without applying it.
	CheckOnly bool
}

// Apply applies a patch file to the workspace.
func (c *Client) Apply(ctx context.Context, opts ApplyOptions) error {
	// Check if patch file exists
	if _, err := os.Stat(opts.PatchPath); err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("patch file not found: %s", opts.PatchPath)
		}
		return fmt.Errorf("failed to check patch file: %w", err)
	}

	// Build apply arguments
	args := []string{"apply"}
	if opts.ThreeWay {
		args = append(args, "--3way")
	}
	if opts.CheckOnly {
		args = append(args, "--check")
	}
	args = append(args, opts.PatchPath)

	_, err := c.execCommand(ctx, args...)
	if err != nil {
		return fmt.Errorf("git apply failed: %w", err)
	}

	return nil
}

// ApplyCheck checks if a patch can be applied without conflicts.
func (c *Client) ApplyCheck(ctx context.Context, patchPath string, threeWay bool) error {
	return c.Apply(ctx, ApplyOptions{
		PatchPath: patchPath,
		ThreeWay:  threeWay,
		CheckOnly: true,
	})
}

// InitSubmodules initializes git submodules.
func (c *Client) InitSubmodules(ctx context.Context) error {
	args := []string{"submodule", "update", "--init", "--recursive"}
	if q := c.quietFlag(); q != "" {
		args = append(args, q)
	}
	_, err := c.execCommand(ctx, args...)
	return err
}

// Branch creates a new branch or checks out an existing one.
func (c *Client) Branch(ctx context.Context, name string, create bool) error {
	args := []string{"checkout"}
	if q := c.quietFlag(); q != "" {
		args = append(args, q)
	}
	if create {
		args = append(args, "-b", name)
	} else {
		args = append(args, name)
	}
	_, err := c.execCommand(ctx, args...)
	return err
}

// RemoteGetConfig retrieves a global/system git configuration value.
// This function does not use a repository directory and reads from global/system config.
// Use client.ConfigGet() for repository-specific configuration.
func RemoteGetConfig(ctx context.Context, key string) (string, error) {
	cmd := exec.Command("git", "config", "--get", key)
	output, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("git config --get %s failed: %w", key, err)
	}
	return strings.TrimSpace(string(output)), nil
}

// CommitAuthor represents the author of a commit.
type CommitAuthor struct {
	Name  string
	Email string
	When  time.Time
}

// CommitOptions specifies options for creating a commit.
type CommitOptions struct {
	// Message is the commit message.
	Message string

	// Author is the commit author (defaults to config).
	Author *CommitAuthor

	// AllowEmpty allows creating a commit with no changes.
	AllowEmpty bool
}

// CommitWith creates a commit with options.
func (c *Client) CommitWith(ctx context.Context, opts CommitOptions) (string, error) {
	args := []string{"commit"}

	if opts.AllowEmpty {
		args = append(args, "--allow-empty")
	}

	// Set author if specified
	if opts.Author != nil {
		args = append(args, "--author", fmt.Sprintf("%s <%s>", opts.Author.Name, opts.Author.Email))
	}

	args = append(args, "-m", opts.Message)

	output, err := c.execCommand(ctx, args...)
	if err != nil {
		return "", fmt.Errorf("commit failed: %w: %s", err, strings.TrimSpace(string(output)))
	}

	// Return the commit SHA
	return c.GetHeadSHA(ctx)
}

// PushOptions specifies options for pushing to a remote.
type PushOptions struct {
	// Remote is the remote name (default: "origin").
	Remote string

	// Branch is the branch to push.
	Branch string

	// Force enables force push.
	Force bool

	// SetUpstream sets the upstream branch.
	SetUpstream bool
}

// Push pushes commits to a remote repository.
// Note: This requires git credentials to be configured for authentication.
func (c *Client) Push(ctx context.Context, opts PushOptions) error {
	if opts.Remote == "" {
		opts.Remote = "origin"
	}
	if opts.Branch == "" {
		return fmt.Errorf("branch name is required for push")
	}

	args := []string{"push"}

	if opts.Force {
		args = append(args, "--force")
	}

	if opts.SetUpstream {
		args = append(args, "-u")
	}

	args = append(args, opts.Remote, opts.Branch)

	_, err := c.execCommand(ctx, args...)
	if err != nil {
		return fmt.Errorf("push failed: %w", err)
	}

	return nil
}

// IsClean returns true if the working directory has no uncommitted changes.
func (c *Client) IsClean(ctx context.Context) bool {
	info, err := c.GetRepositoryInfo(ctx)
	if err != nil {
		return false
	}

	// Check for untracked files
	untrackedOutput, err := c.execCommand(ctx, "ls-files", "--others", "--exclude-standard")
	if err == nil && len(untrackedOutput) > 0 {
		return false
	}

	// Check for staged changes using git diff --cached --quiet
	// If there are staged changes, the command returns exit code 1
	_, err = c.execCommand(ctx, "diff", "--cached", "--quiet")
	if err != nil {
		// Exit code 1 means there are staged changes
		return false
	}

	return info.Clean
}

// HasChanges returns true if there are uncommitted changes.
func (c *Client) HasChanges(ctx context.Context) bool {
	return !c.IsClean(ctx)
}

// ConfigGet gets a git configuration value.
func (c *Client) ConfigGet(ctx context.Context, key string) (string, error) {
	output, err := c.execCommand(ctx, "config", "--get", key)
	if err != nil {
		return "", fmt.Errorf("git config --get %s failed: %w", key, err)
	}
	return strings.TrimSpace(string(output)), nil
}

// SetRemote ensures that a remote with the given name points to the provided URL.
// If the remote already exists, its URL is updated. If it does not exist, the
// remote is created with the given URL.
func (c *Client) SetRemote(ctx context.Context, name, url string) error {
	// List existing remotes to determine whether the named remote already exists.
	output, err := c.execCommand(ctx, "remote")
	if err != nil {
		return fmt.Errorf("failed to list git remotes: %w", err)
	}

	exists := false
	for _, remote := range strings.Fields(string(output)) {
		if remote == name {
			exists = true
			break
		}
	}

	if exists {
		_, err = c.execCommand(ctx, "remote", "set-url", name, url)
		if err != nil {
			return fmt.Errorf("failed to set remote %s to %s: %w", name, url, err)
		}
	} else {
		_, err = c.execCommand(ctx, "remote", "add", name, url)
		if err != nil {
			return fmt.Errorf("failed to add remote %s with URL %s: %w", name, url, err)
		}
	}

	return nil
}

// FileStatus represents the status of a single file in the working tree.
type FileStatus struct {
	// Path is the file path.
	Path string

	// Status is the human-readable status (e.g., "modified", "added", "deleted").
	Status string

	// StatusCode is the raw status code from git status.
	StatusCode string
}

// GetWorkingTreeStatus returns detailed status information about the working tree.
// It parses git status --porcelain output to return file-level status.
func (c *Client) GetWorkingTreeStatus(ctx context.Context) ([]FileStatus, error) {
	output, err := c.execCommand(ctx, "status", "--porcelain")
	if err != nil {
		return nil, fmt.Errorf("failed to get working tree status: %w", err)
	}

	return parseFileStatus(string(output)), nil
}

// parseFileStatus parses git status --porcelain output into FileStatus entries.
func parseFileStatus(output string) []FileStatus {
	var statuses []FileStatus
	lines := strings.Split(output, "\n")

	for _, line := range lines {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}

		if len(line) >= 3 {
			statusCode := line[0:2]
			pathInfo := line[3:]

			// Handle renamed/copied files in porcelain format: "R  old_name -> new_name".
			// For programmatic use, we treat the new name as the canonical path.
			filePath := pathInfo
			if strings.Contains(pathInfo, " -> ") {
				parts := strings.SplitN(pathInfo, " -> ", 2)
				if len(parts) == 2 {
					filePath = parts[1]
				}
			}

			statuses = append(statuses, FileStatus{
				Path:       filePath,
				Status:     decodeSimpleStatusCode(statusCode),
				StatusCode: statusCode,
			})
		}
	}

	return statuses
}

// decodeSimpleStatusCode converts simple porcelain status to human-readable string.
func decodeSimpleStatusCode(code string) string {
	switch code {
	case " M":
		return "modified (worktree)"
	case "M ":
		return "modified (index)"
	case "MM":
		return "modified (both)"
	case "A ":
		return "added (index)"
	case " D":
		return "deleted (worktree)"
	case "D ":
		return "deleted (index)"
	case "AM":
		return "added (index), modified (worktree)"
	case "??":
		return "untracked"
	case "R ":
		return "renamed (index)"
	case "C ":
		return "copied (index)"
	case "UU":
		return "both modified"
	case "AA":
		return "both added"
	case "DD":
		return "both deleted"
	case "DU":
		return "deleted (ours), modified (theirs)"
	case "UD":
		return "modified (ours), deleted (theirs)"
	// Additional unmerged states
	case "AU":
		return "added (ours), unmerged"
	case "UA":
		return "added (theirs), unmerged"
	// Additional states
	case " A":
		return "added (worktree)"
	case "AD":
		return "added (index), deleted (worktree)"
	case "MD":
		return "modified (index), deleted (worktree)"
	case " R":
		return "renamed (worktree)"
	case " C":
		return "copied (worktree)"
	default:
		return fmt.Sprintf("unknown_%s", code)
	}
}

// DiagnoseWorkingTree returns detailed diagnostic information about the working tree state.
// It returns a formatted string with file-by-file status information.
func (c *Client) DiagnoseWorkingTree(ctx context.Context) string {
	var builder strings.Builder

	builder.WriteString("=== Working Tree Diagnostics ===\n")
	builder.WriteString(fmt.Sprintf("Workspace: %s\n", c.Dir))

	// Check if clean
	if c.IsClean(ctx) {
		builder.WriteString("Status: CLEAN\n")
		return builder.String()
	}

	builder.WriteString("Status: DIRTY\n")

	// Get detailed status
	statuses, err := c.GetWorkingTreeStatus(ctx)
	if err != nil {
		builder.WriteString(fmt.Sprintf("Error getting status: %v\n", err))
		return builder.String()
	}

	if len(statuses) == 0 {
		builder.WriteString("No changes detected\n")
		return builder.String()
	}

	builder.WriteString(fmt.Sprintf("Files with changes: %d\n", len(statuses)))
	for _, status := range statuses {
		builder.WriteString(fmt.Sprintf("  - %s: %s\n", status.Path, status.Status))
	}

	// Get diff stat
	diffOutput, err := c.execCommand(ctx, "diff", "--stat")
	if err == nil && len(diffOutput) > 0 {
		builder.WriteString("\nDiff Stat:\n")
		builder.WriteString(string(diffOutput))
	}

	return builder.String()
}
