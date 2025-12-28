package preflight

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

// CheckLevel represents the severity level of a preflight check
type CheckLevel int

const (
	// LevelError indicates a critical failure that prevents execution
	LevelError CheckLevel = iota
	// LevelWarn indicates a warning that should be addressed but doesn't block execution
	LevelWarn
	// LevelInfo indicates informational output
	LevelInfo
)

// CheckResult represents the result of a single preflight check
type CheckResult struct {
	Name    string     // Check name
	Level   CheckLevel // Severity level
	Message string     // Human-readable message
	Error   error      // Underlying error (if any)
}

// Check represents a single preflight check
type Check interface {
	// Name returns the check name
	Name() string
	// Run executes the check and returns a CheckResult
	Run(ctx context.Context) CheckResult
}

// Checker runs a collection of preflight checks
type Checker struct {
	checks   []Check
	skipped  bool
	quiet    bool
}

// Config configures the preflight checker
type Config struct {
	// Skip skips all preflight checks
	Skip bool
	// Quiet suppresses info-level messages
	Quiet bool
	// RequireDocker checks if docker is available (default: true)
	RequireDocker bool
	// RequireGit checks if git is available (default: true)
	RequireGit bool
	// RequireGitHubToken checks if GitHub token is available (default: false)
	RequireGitHubToken bool
	// RequireAnthropicToken checks if Anthropic token is available (default: false)
	RequireAnthropicToken bool
	// WorkspacePath is the workspace path for permission checks
	WorkspacePath string
	// OutputPath is the output path for permission checks
	OutputPath string
}

// NewChecker creates a new preflight checker with the given configuration
func NewChecker(cfg Config) *Checker {
	c := &Checker{
		skipped: cfg.Skip,
		quiet:   cfg.Quiet,
	}

	// Add checks based on configuration
	if cfg.RequireDocker {
		c.checks = append(c.checks, &DockerCheck{})
	}
	if cfg.RequireGit {
		c.checks = append(c.checks, &GitCheck{})
	}
	if cfg.RequireGitHubToken {
		c.checks = append(c.checks, &GitHubTokenCheck{})
	}
	if cfg.RequireAnthropicToken {
		c.checks = append(c.checks, &AnthropicTokenCheck{})
	}
	if cfg.WorkspacePath != "" {
		c.checks = append(c.checks, &WorkspaceCheck{
			Path: cfg.WorkspacePath,
		})
	}
	if cfg.OutputPath != "" {
		c.checks = append(c.checks, &OutputCheck{
			Path: cfg.OutputPath,
		})
	}

	return c
}

// Run executes all registered checks and returns an error if any critical checks fail
func (c *Checker) Run(ctx context.Context) error {
	if c.skipped {
		holonlog.Info("preflight checks skipped")
		return nil
	}

	holonlog.Progress("running preflight checks")

	var errors []error
	var warnings []string

	for _, check := range c.checks {
		result := check.Run(ctx)

		switch result.Level {
		case LevelError:
			holonlog.Error("preflight check failed", "check", result.Name, "message", result.Message)
			if result.Error != nil {
				errors = append(errors, result.Error)
			} else {
				errors = append(errors, fmt.Errorf("%s: %s", result.Name, result.Message))
			}
		case LevelWarn:
			holonlog.Warn("preflight check warning", "check", result.Name, "message", result.Message)
			warnings = append(warnings, fmt.Sprintf("%s: %s", result.Name, result.Message))
		case LevelInfo:
			if !c.quiet {
				holonlog.Info("preflight check", "check", result.Name, "message", result.Message)
			}
		}
	}

	// Log warnings summary
	if len(warnings) > 0 {
		holonlog.Info("preflight warnings", "count", len(warnings))
	}

	// If there are errors, return a combined error
	if len(errors) > 0 {
		var errMsgs []string
		for _, err := range errors {
			errMsgs = append(errMsgs, err.Error())
		}
		return fmt.Errorf("preflight checks failed:\n  - %s", strings.Join(errMsgs, "\n  - "))
	}

	holonlog.Progress("preflight checks passed")
	return nil
}

// DockerCheck checks if docker is installed and the daemon is reachable
type DockerCheck struct{}

func (c *DockerCheck) Name() string {
	return "docker"
}

func (c *DockerCheck) Run(ctx context.Context) CheckResult {
	// Check if docker command exists
	_, err := exec.LookPath("docker")
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: "docker command not found. Please install Docker from https://docs.docker.com/get-docker/",
			Error:   err,
		}
	}

	// Check if docker daemon is reachable
	// Use a timeout context to avoid hanging
	checkCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	cmd := exec.CommandContext(checkCtx, "docker", "info")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: "docker daemon is not running or not accessible. Please start Docker Desktop or the Docker daemon.",
			Error:   fmt.Errorf("docker info failed: %w: %s", err, string(output)),
		}
	}

	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: "docker is available and daemon is running",
	}
}

// GitCheck checks if git is installed
type GitCheck struct{}

func (c *GitCheck) Name() string {
	return "git"
}

func (c *GitCheck) Run(ctx context.Context) CheckResult {
	// Check if git command exists
	_, err := exec.LookPath("git")
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: "git command not found. Please install Git from https://git-scm.com/downloads",
			Error:   err,
		}
	}

	// Check git version
	cmd := exec.Command("git", "--version")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelWarn,
			Message: "git is installed but may not be working correctly",
			Error:   err,
		}
	}

	version := strings.TrimSpace(string(output))
	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: fmt.Sprintf("git is available (%s)", version),
	}
}

// GitHubTokenCheck checks if GitHub token is available
type GitHubTokenCheck struct{}

func (c *GitHubTokenCheck) Name() string {
	return "github-token"
}

func (c *GitHubTokenCheck) Run(ctx context.Context) CheckResult {
	// Check environment variables
	token := os.Getenv("GITHUB_TOKEN")
	if token == "" {
		token = os.Getenv("GH_TOKEN")
	}
	if token == "" {
		token = os.Getenv("HOLON_GITHUB_TOKEN")
	}

	// If no token in env, try gh CLI
	if token == "" {
		_, err := exec.LookPath("gh")
		if err == nil {
			cmd := exec.Command("gh", "auth", "token")
			output, err := cmd.Output()
			if err == nil {
				token = strings.TrimSpace(string(output))
				if token != "" {
					return CheckResult{
						Name:    c.Name(),
						Level:   LevelInfo,
						Message: "GitHub token available (from gh auth token)",
					}
				}
			}
		}
	}

	if token == "" {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: "GitHub token not found. Set GITHUB_TOKEN environment variable, or run 'gh auth login' to authenticate with gh CLI",
			Error:   fmt.Errorf("no GitHub token found"),
		}
	}

	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: "GitHub token available (from environment)",
	}
}

// AnthropicTokenCheck checks if Anthropic API key is available
type AnthropicTokenCheck struct{}

func (c *AnthropicTokenCheck) Name() string {
	return "anthropic-token"
}

func (c *AnthropicTokenCheck) Run(ctx context.Context) CheckResult {
	// Check environment variables with priority: ANTHROPIC_AUTH_TOKEN > ANTHROPIC_API_KEY
	authToken := os.Getenv("ANTHROPIC_AUTH_TOKEN")
	apiKey := os.Getenv("ANTHROPIC_API_KEY")

	var token string
	var usedLegacyVar bool

	if authToken != "" {
		token = authToken
	} else if apiKey != "" {
		token = apiKey
		usedLegacyVar = true
	}

	if token == "" {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: "Anthropic auth token not found. Set ANTHROPIC_AUTH_TOKEN environment variable",
			Error:   fmt.Errorf("no Anthropic auth token found"),
		}
	}

	if usedLegacyVar {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelWarn,
			Message: "Anthropic auth token available (using ANTHROPIC_API_KEY; consider migrating to ANTHROPIC_AUTH_TOKEN)",
		}
	}

	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: "Anthropic auth token available",
	}
}

// WorkspaceCheck checks if workspace path is accessible
type WorkspaceCheck struct {
	Path string
}

func (c *WorkspaceCheck) Name() string {
	return "workspace"
}

func (c *WorkspaceCheck) Run(ctx context.Context) CheckResult {
	if c.Path == "" {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelInfo,
			Message: "no workspace path specified",
		}
	}

	// Resolve absolute path
	absPath, err := filepath.Abs(c.Path)
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: fmt.Sprintf("failed to resolve workspace path: %s", c.Path),
			Error:   err,
		}
	}

	// Check if path exists
	info, err := os.Stat(absPath)
	if err != nil {
		if os.IsNotExist(err) {
			return CheckResult{
				Name:    c.Name(),
				Level:   LevelError,
				Message: fmt.Sprintf("workspace path does not exist: %s", absPath),
				Error:   err,
			}
		}
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: fmt.Sprintf("cannot access workspace path: %s", absPath),
			Error:   err,
		}
	}

	// Check if it's a directory
	if !info.IsDir() {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: fmt.Sprintf("workspace path is not a directory: %s", absPath),
			Error:   fmt.Errorf("not a directory"),
		}
	}

	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: fmt.Sprintf("workspace is accessible: %s", absPath),
	}
}

// OutputCheck checks if output path is writable
type OutputCheck struct {
	Path string
}

func (c *OutputCheck) Name() string {
	return "output"
}

func (c *OutputCheck) Run(ctx context.Context) CheckResult {
	if c.Path == "" {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelInfo,
			Message: "no output path specified (will use temp directory)",
		}
	}

	// Resolve absolute path
	absPath, err := filepath.Abs(c.Path)
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: fmt.Sprintf("failed to resolve output path: %s", c.Path),
			Error:   err,
		}
	}

	// Check if path exists
	info, err := os.Stat(absPath)
	if err != nil {
		if os.IsNotExist(err) {
			// Try to create the directory
			if err := os.MkdirAll(absPath, 0755); err != nil {
				return CheckResult{
					Name:    c.Name(),
					Level:   LevelError,
					Message: fmt.Sprintf("cannot create output directory: %s", absPath),
					Error:   err,
				}
			}
			// Successfully created, now check if we can write to it
		} else {
			return CheckResult{
				Name:    c.Name(),
				Level:   LevelError,
				Message: fmt.Sprintf("cannot access output path: %s", absPath),
				Error:   err,
			}
		}
	} else {
		// Path exists, check if it's a directory
		if !info.IsDir() {
			return CheckResult{
				Name:    c.Name(),
				Level:   LevelError,
				Message: fmt.Sprintf("output path is not a directory: %s", absPath),
				Error:   fmt.Errorf("not a directory"),
			}
		}
	}

	// Check if directory is writable by creating a temporary file
	testFile := filepath.Join(absPath, fmt.Sprintf(".holon-write-test-%d", os.Getpid()))
	f, err := os.Create(testFile)
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelError,
			Message: fmt.Sprintf("output directory is not writable: %s", absPath),
			Error:   err,
		}
	}
	f.Close()
	_ = os.Remove(testFile)

	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: fmt.Sprintf("output directory is writable: %s", absPath),
	}
}

// NetworkCheck performs a basic network connectivity check
// This is best-effort and may not catch all network issues
type NetworkCheck struct {
	URL string // URL to check (default: https://google.com)
}

func (c *NetworkCheck) Name() string {
	return "network"
}

func (c *NetworkCheck) Run(ctx context.Context) CheckResult {
	url := c.URL
	if url == "" {
		url = "https://www.google.com/"
	}

	// Create a request with timeout
	checkCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	req, err := http.NewRequestWithContext(checkCtx, http.MethodHead, url, nil)
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelWarn,
			Message: "failed to create network check request",
			Error:   err,
		}
	}

	client := &http.Client{
		Timeout: 5 * time.Second,
	}
	resp, err := client.Do(req)
	if err != nil {
		// Network check is best-effort, so just warn
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelWarn,
			Message: "network may be unavailable or restricted (this may affect agent bundle downloads)",
			Error:   err,
		}
	}
	defer resp.Body.Close()
	// Drain body
	_, err = io.Copy(io.Discard, resp.Body)
	if err != nil {
		// Log the error but don't fail - this is just body draining
		holonlog.Debug("failed to drain response body", "error", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 400 {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelWarn,
			Message: fmt.Sprintf("network check returned unexpected status: %d", resp.StatusCode),
			Error:   fmt.Errorf("HTTP %d", resp.StatusCode),
		}
	}

	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: "network connectivity appears functional",
	}
}

// DiskSpaceCheck checks if there's sufficient disk space
// Note: MinBytes is for future use; the check uses a fixed 10MB test file for practical verification
type DiskSpaceCheck struct {
	Path     string
	MinBytes int64 // Minimum required bytes (for future use; currently writes a 10MB test file)
}

func (c *DiskSpaceCheck) Name() string {
	return "disk-space"
}

func (c *DiskSpaceCheck) Run(ctx context.Context) CheckResult {
	path := c.Path
	if path == "" {
		path = os.TempDir()
	}

	// MinBytes is reserved for future use
	// Currently, we use a fixed 10MB test size for practical disk space verification
	_ = c.MinBytes

	// Try to write a 10MB test file to verify disk space
	// This works cross-platform on Windows, Unix, and macOS
	testFile := filepath.Join(path, fmt.Sprintf(".holon-dspace-test-%d", os.Getpid()))
	f, err := os.Create(testFile)
	if err != nil {
		return CheckResult{
			Name:    c.Name(),
			Level:   LevelWarn,
			Message: "cannot verify disk space (write test failed)",
			Error:   err,
		}
	}

	// Try to allocate 10MB
	chunk := make([]byte, 1024*1024) // 1MB chunks
	for i := 0; i < 10; i++ {
		_, err := f.Write(chunk)
		if err != nil {
			f.Close()
			_ = os.Remove(testFile)
			return CheckResult{
				Name:    c.Name(),
				Level:   LevelWarn,
				Message: "low disk space detected",
				Error:   err,
			}
		}
	}
	f.Close()
	_ = os.Remove(testFile)

	return CheckResult{
		Name:    c.Name(),
		Level:   LevelInfo,
		Message: "disk space appears sufficient",
	}
}
