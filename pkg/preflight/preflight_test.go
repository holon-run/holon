package preflight

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestDockerCheck(t *testing.T) {
	check := &DockerCheck{}
	ctx := context.Background()

	result := check.Run(ctx)

	if result.Name != "docker" {
		t.Errorf("expected name 'docker', got '%s'", result.Name)
	}

	// Docker check should be info level if docker is available, error otherwise
	// We can't assert the exact level in tests since docker may or may not be installed
	if result.Level != LevelError && result.Level != LevelInfo {
		t.Errorf("expected LevelError or LevelInfo, got %v", result.Level)
	}

	t.Logf("DockerCheck result: level=%d, message=%s", result.Level, result.Message)
}

func TestGitCheck(t *testing.T) {
	check := &GitCheck{}
	ctx := context.Background()

	result := check.Run(ctx)

	if result.Name != "git" {
		t.Errorf("expected name 'git', got '%s'", result.Name)
	}

	// Git should be available in test environment
	if result.Level != LevelError && result.Level != LevelInfo {
		t.Errorf("expected LevelError or LevelInfo, got %v", result.Level)
	}

	t.Logf("GitCheck result: level=%d, message=%s", result.Level, result.Message)
}

func TestGitHubTokenCheck(t *testing.T) {
	check := &GitHubTokenCheck{}
	ctx := context.Background()

	result := check.Run(ctx)

	if result.Name != "github-token" {
		t.Errorf("expected name 'github-token', got '%s'", result.Name)
	}

	// GitHub token may or may not be set in test environment
	t.Logf("GitHubTokenCheck result: level=%d, message=%s", result.Level, result.Message)
}

func TestAnthropicTokenCheck(t *testing.T) {
	check := &AnthropicTokenCheck{}
	ctx := context.Background()

	// Save original values
	origKey := os.Getenv("ANTHROPIC_API_KEY")
	origAuthToken := os.Getenv("ANTHROPIC_AUTH_TOKEN")
	defer func() {
		if origKey != "" {
			os.Setenv("ANTHROPIC_API_KEY", origKey)
		} else {
			os.Unsetenv("ANTHROPIC_API_KEY")
		}
		if origAuthToken != "" {
			os.Setenv("ANTHROPIC_AUTH_TOKEN", origAuthToken)
		} else {
			os.Unsetenv("ANTHROPIC_AUTH_TOKEN")
		}
	}()

	// Test with no token - unset both
	os.Unsetenv("ANTHROPIC_API_KEY")
	os.Unsetenv("ANTHROPIC_AUTH_TOKEN")
	result := check.Run(ctx)

	if result.Level != LevelError {
		t.Errorf("expected LevelError when token not set, got %v", result.Level)
	}

	// Test with legacy ANTHROPIC_API_KEY set (should return warning for deprecated usage)
	os.Unsetenv("ANTHROPIC_AUTH_TOKEN")
	os.Unsetenv("ANTHROPIC_API_KEY")
	os.Setenv("ANTHROPIC_API_KEY", "test-key-12345")
	result = check.Run(ctx)

	if result.Level != LevelWarn {
		t.Errorf("expected LevelWarn when legacy token set, got %v", result.Level)
	}

	// Test with new ANTHROPIC_AUTH_TOKEN set (should return info level)
	os.Unsetenv("ANTHROPIC_API_KEY")
	os.Setenv("ANTHROPIC_AUTH_TOKEN", "test-token-12345")
	result = check.Run(ctx)

	if result.Level != LevelInfo {
		t.Errorf("expected LevelInfo when AUTH_TOKEN set, got %v", result.Level)
	}

	// Test priority: when both are set, ANTHROPIC_AUTH_TOKEN should take precedence (info, not warn)
	os.Setenv("ANTHROPIC_API_KEY", "test-key-12345")
	os.Setenv("ANTHROPIC_AUTH_TOKEN", "test-token-12345")
	result = check.Run(ctx)

	if result.Level != LevelInfo {
		t.Errorf("expected LevelInfo when both tokens set (AUTH_TOKEN takes precedence), got %v", result.Level)
	}
}

func TestWorkspaceCheck(t *testing.T) {
	ctx := context.Background()

	// Test with temp directory
	tempDir := t.TempDir()
	check := &WorkspaceCheck{Path: tempDir}
	result := check.Run(ctx)

	if result.Name != "workspace" {
		t.Errorf("expected name 'workspace', got '%s'", result.Name)
	}

	if result.Level != LevelInfo {
		t.Errorf("expected LevelInfo for valid directory, got %v: %s", result.Level, result.Message)
	}

	// Test with non-existent path
	check = &WorkspaceCheck{Path: "/nonexistent/path/that/does/not/exist"}
	result = check.Run(ctx)

	if result.Level != LevelError {
		t.Errorf("expected LevelError for non-existent path, got %v", result.Level)
	}
}

func TestOutputCheck(t *testing.T) {
	ctx := context.Background()

	// Test with temp directory
	tempDir := t.TempDir()
	check := &OutputCheck{Path: tempDir}
	result := check.Run(ctx)

	if result.Name != "output" {
		t.Errorf("expected name 'output', got '%s'", result.Name)
	}

	if result.Level != LevelInfo {
		t.Errorf("expected LevelInfo for writable directory, got %v: %s", result.Level, result.Message)
	}

	// Test with directory that doesn't exist (should create it)
	newDir := filepath.Join(tempDir, "new-output-dir")
	check = &OutputCheck{Path: newDir}
	result = check.Run(ctx)

	if result.Level != LevelInfo {
		t.Errorf("expected LevelInfo for creatable directory, got %v: %s", result.Level, result.Message)
	}

	// Verify directory was created
	if _, err := os.Stat(newDir); err != nil {
		t.Errorf("expected directory to be created: %v", err)
	}
}

func TestNetworkCheck(t *testing.T) {
	check := &NetworkCheck{}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	result := check.Run(ctx)

	// Network check is best-effort, so warn or info are acceptable
	if result.Level != LevelWarn && result.Level != LevelInfo {
		t.Errorf("expected LevelWarn or LevelInfo, got %v", result.Level)
	}

	t.Logf("NetworkCheck result: level=%d, message=%s", result.Level, result.Message)
}

func TestDiskSpaceCheck(t *testing.T) {
	tempDir := t.TempDir()
	check := &DiskSpaceCheck{Path: tempDir}
	ctx := context.Background()

	result := check.Run(ctx)

	if result.Level != LevelWarn && result.Level != LevelInfo {
		t.Errorf("expected LevelWarn or LevelInfo, got %v", result.Level)
	}

	t.Logf("DiskSpaceCheck result: level=%d, message=%s", result.Level, result.Message)
}

func TestChecker(t *testing.T) {
	tempDir := t.TempDir()

	cfg := Config{
		RequireDocker:         false, // Disable for CI
		RequireGit:            true,
		RequireGitHubToken:    false,
		RequireAnthropicToken: false,
		WorkspacePath:         tempDir,
		OutputPath:            tempDir,
	}

	checker := NewChecker(cfg)
	ctx := context.Background()

	err := checker.Run(ctx)

	// Should succeed since we only require git and valid paths
	if err != nil {
		t.Errorf("expected success, got error: %v", err)
	}
}

func TestCheckerSkip(t *testing.T) {
	cfg := Config{
		Skip: true,
	}

	checker := NewChecker(cfg)
	ctx := context.Background()

	err := checker.Run(ctx)

	// Should succeed immediately since checks are skipped
	if err != nil {
		t.Errorf("expected success when skipped, got error: %v", err)
	}
}

func TestCheckerWithMissingGit(t *testing.T) {
	// Save original PATH
	origPath := os.Getenv("PATH")
	defer os.Setenv("PATH", origPath)

	// Set PATH to empty to make commands unavailable
	os.Setenv("PATH", "")

	cfg := Config{
		RequireGit:    true,
		RequireDocker: false,
		Skip:          false,
	}

	checker := NewChecker(cfg)
	ctx := context.Background()

	err := checker.Run(ctx)

	// Should fail since git is required but not in PATH
	if err == nil {
		t.Error("expected error when git is required but not found")
	}

	t.Logf("Expected error: %v", err)
}

func TestCheckerWithInvalidWorkspace(t *testing.T) {
	cfg := Config{
		RequireGit:            false,
		RequireDocker:         false,
		RequireGitHubToken:    false,
		RequireAnthropicToken: false,
		WorkspacePath:         "/nonexistent/workspace/path",
	}

	checker := NewChecker(cfg)
	ctx := context.Background()

	err := checker.Run(ctx)

	// Should fail since workspace doesn't exist
	if err == nil {
		t.Error("expected error when workspace path is invalid")
	}

	t.Logf("Expected error: %v", err)
}

func TestCheckerWithInvalidOutput(t *testing.T) {
	// Create a temporary file (not directory) to test invalid output path
	tempFile, err := os.CreateTemp("", "holon-test-file-*")
	if err != nil {
		t.Fatal(err)
	}
	defer os.Remove(tempFile.Name())
	tempFile.Close()

	cfg := Config{
		RequireGit:            false,
		RequireDocker:         false,
		RequireGitHubToken:    false,
		RequireAnthropicToken: false,
		OutputPath:            tempFile.Name(), // File instead of directory
	}

	checker := NewChecker(cfg)
	ctx := context.Background()

	err = checker.Run(ctx)

	// Should fail since output path is a file, not a directory
	if err == nil {
		t.Error("expected error when output path is a file")
	}

	t.Logf("Expected error: %v", err)
}
