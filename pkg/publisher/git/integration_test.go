package git

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/holon-run/holon/pkg/publisher"
)

// TestIntegration tests the full git publisher workflow.
// This test creates a real git repository and applies a patch.
func TestIntegration(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	// Check if git is available
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not found in PATH, skipping integration test")
	}

	// Set workspace environment variable
	oldWorkspace := os.Getenv(WorkspaceEnv)
	t.Cleanup(func() {
		os.Setenv(WorkspaceEnv, oldWorkspace)
	})

	t.Run("apply patch only", func(t *testing.T) {
		tmpDir := t.TempDir()
		workspaceDir := filepath.Join(tmpDir, "workspace")
		outputDir := filepath.Join(tmpDir, "output")

		// Create workspace directory
		if err := os.MkdirAll(workspaceDir, 0755); err != nil {
			t.Fatalf("failed to create workspace: %v", err)
		}

		// Create output directory
		if err := os.MkdirAll(outputDir, 0755); err != nil {
			t.Fatalf("failed to create output directory: %v", err)
		}

		// Initialize git repository
		if err := initRealGitRepo(workspaceDir); err != nil {
			t.Fatalf("failed to init git repo: %v", err)
		}

		// Create an initial file
		initialFile := filepath.Join(workspaceDir, "test.txt")
		if err := os.WriteFile(initialFile, []byte("initial content\n"), 0644); err != nil {
			t.Fatalf("failed to create initial file: %v", err)
		}

		// Commit the initial file
		if err := gitCommit(workspaceDir, "initial commit"); err != nil {
			t.Fatalf("failed to commit initial file: %v", err)
		}

		// Create a patch file that modifies test.txt
		patchContent := `diff --git a/test.txt b/test.txt
index 94c1a57..1a3c6d8 100644
--- a/test.txt
+++ b/test.txt
@@ -1 +1 @@
-initial content
+modified content
`
		patchFile := filepath.Join(outputDir, "diff.patch")
		if err := os.WriteFile(patchFile, []byte(patchContent), 0644); err != nil {
			t.Fatalf("failed to create patch file: %v", err)
		}

		// Set workspace environment variable
		os.Setenv(WorkspaceEnv, workspaceDir)

		// Test: Apply patch without branch or commit
		p := NewPublisher()
		req := publisher.PublishRequest{
			Target:    "origin/main",
			OutputDir: outputDir,
			Artifacts: map[string]string{
				"diff.patch": patchFile,
			},
		}

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() failed: %v", err)
		}
		if !result.Success {
			t.Fatalf("Publish() returned success=false: %+v", result)
		}

		// Verify patch was applied
		content, err := os.ReadFile(initialFile)
		if err != nil {
			t.Fatalf("failed to read modified file: %v", err)
		}
		if string(content) != "modified content\n" {
			t.Errorf("file content = %q, want %q", string(content), "modified content\n")
		}

		// Verify result actions
		if len(result.Actions) != 1 {
			t.Errorf("got %d actions, want 1", len(result.Actions))
		} else if result.Actions[0].Type != "applied_patch" {
			t.Errorf("action type = %s, want applied_patch", result.Actions[0].Type)
		}
	})

	t.Run("apply patch with branch and commit", func(t *testing.T) {
		tmpDir := t.TempDir()
		workspaceDir := filepath.Join(tmpDir, "workspace")
		outputDir := filepath.Join(tmpDir, "output")

		// Create workspace directory
		if err := os.MkdirAll(workspaceDir, 0755); err != nil {
			t.Fatalf("failed to create workspace: %v", err)
		}

		// Create output directory
		if err := os.MkdirAll(outputDir, 0755); err != nil {
			t.Fatalf("failed to create output directory: %v", err)
		}

		// Initialize git repository
		if err := initRealGitRepo(workspaceDir); err != nil {
			t.Fatalf("failed to init git repo: %v", err)
		}

		// Create an initial file
		initialFile := filepath.Join(workspaceDir, "test.txt")
		if err := os.WriteFile(initialFile, []byte("initial content\n"), 0644); err != nil {
			t.Fatalf("failed to create initial file: %v", err)
		}

		// Commit the initial file
		if err := gitCommit(workspaceDir, "initial commit"); err != nil {
			t.Fatalf("failed to commit initial file: %v", err)
		}

		// Create a patch file that modifies test.txt
		patchContent := `diff --git a/test.txt b/test.txt
index 94c1a57..1a3c6d8 100644
--- a/test.txt
+++ b/test.txt
@@ -1 +1 @@
-initial content
+modified content
`
		patchFile := filepath.Join(outputDir, "diff.patch")
		if err := os.WriteFile(patchFile, []byte(patchContent), 0644); err != nil {
			t.Fatalf("failed to create patch file: %v", err)
		}

		// Set workspace environment variable
		os.Setenv(WorkspaceEnv, workspaceDir)

		// Test: Apply patch with branch and commit
		p := NewPublisher()
		req := publisher.PublishRequest{
			Target:    "origin/main",
			OutputDir: outputDir,
			Manifest: map[string]interface{}{
				"metadata": map[string]interface{}{
					"branch": "test-branch",
					"commit": true,
				},
			},
			Artifacts: map[string]string{
				"diff.patch": patchFile,
			},
		}

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() with branch/commit failed: %v", err)
		}
		if !result.Success {
			t.Fatalf("Publish() with branch/commit returned success=false: %+v", result)
		}

		// Verify patch was applied
		content, err := os.ReadFile(initialFile)
		if err != nil {
			t.Fatalf("failed to read modified file: %v", err)
		}
		if string(content) != "modified content\n" {
			t.Errorf("file content = %q, want %q", string(content), "modified content\n")
		}

		// Verify actions (created_branch, applied_patch, committed)
		if len(result.Actions) != 3 {
			t.Errorf("got %d actions, want 3", len(result.Actions))
		} else {
			if result.Actions[0].Type != "created_branch" {
				t.Errorf("action[0] type = %s, want created_branch", result.Actions[0].Type)
			}
			if result.Actions[1].Type != "applied_patch" {
				t.Errorf("action[1] type = %s, want applied_patch", result.Actions[1].Type)
			}
			if result.Actions[2].Type != "committed" {
				t.Errorf("action[2] type = %s, want committed", result.Actions[2].Type)
			}
		}

		// Verify we're on the test branch
		currentBranch, err := gitGetCurrentBranch(workspaceDir)
		if err != nil {
			t.Fatalf("failed to get current branch: %v", err)
		}
		if currentBranch != "test-branch" {
			t.Errorf("current branch = %s, want test-branch", currentBranch)
		}

		// Verify commit was created
		commits, err := gitLog(workspaceDir)
		if err != nil {
			t.Fatalf("failed to get git log: %v", err)
		}
		if len(commits) < 1 {
			t.Errorf("expected at least 1 commit, got %d", len(commits))
		}
	})

	t.Run("no op without patch", func(t *testing.T) {
		tmpDir := t.TempDir()
		outputDir := filepath.Join(tmpDir, "output")

		// Create output directory
		if err := os.MkdirAll(outputDir, 0755); err != nil {
			t.Fatalf("failed to create output directory: %v", err)
		}

		// Test: No-op with no patch
		p := NewPublisher()
		req := publisher.PublishRequest{
			Target:    "origin/main",
			OutputDir: outputDir,
			Artifacts: map[string]string{},
		}

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() without patch failed: %v", err)
		}
		if !result.Success {
			t.Fatalf("Publish() without patch returned success=false: %+v", result)
		}
		if len(result.Actions) != 1 || result.Actions[0].Type != "no_op" {
			t.Errorf("expected no_op action, got: %+v", result.Actions)
		}
	})

	t.Run("push requires commit validation", func(t *testing.T) {
		tmpDir := t.TempDir()
		workspaceDir := filepath.Join(tmpDir, "workspace")
		outputDir := filepath.Join(tmpDir, "output")

		// Create workspace directory
		if err := os.MkdirAll(workspaceDir, 0755); err != nil {
			t.Fatalf("failed to create workspace: %v", err)
		}

		// Create output directory
		if err := os.MkdirAll(outputDir, 0755); err != nil {
			t.Fatalf("failed to create output directory: %v", err)
		}

		// Initialize git repository
		if err := initRealGitRepo(workspaceDir); err != nil {
			t.Fatalf("failed to init git repo: %v", err)
		}

		// Set workspace environment variable
		os.Setenv(WorkspaceEnv, workspaceDir)

		// Test: push without commit should fail validation
		p := NewPublisher()
		req := publisher.PublishRequest{
			Target:    "origin/main",
			OutputDir: outputDir,
			Manifest: map[string]interface{}{
				"metadata": map[string]interface{}{
					"push": true,
				},
			},
			Artifacts: map[string]string{},
		}

		err := p.Validate(req)
		if err == nil {
			t.Fatal("Validate() with push without commit should fail, got nil error")
		}
		if !strings.Contains(err.Error(), "push requires commit") {
			t.Errorf("expected error about push requiring commit, got: %v", err)
		}
	})
}

// initRealGitRepo initializes a real git repository using git commands.
func initRealGitRepo(dir string) error {
	commands := [][]string{
		{"git", "init"},
		{"git", "config", "user.name", "Test User"},
		{"git", "config", "user.email", "test@example.com"},
	}

	for _, cmd := range commands {
		c := exec.Command(cmd[0], cmd[1:]...)
		c.Dir = dir
		if output, err := c.CombinedOutput(); err != nil {
			return fmt.Errorf("git command %v failed: %w: %s", cmd, err, string(output))
		}
	}
	return nil
}

// gitCommit stages and commits all changes.
func gitCommit(dir, message string) error {
	cmd := exec.Command("git", "add", ".")
	cmd.Dir = dir
	if output, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git add failed: %w: %s", err, string(output))
	}

	cmd = exec.Command("git", "commit", "-m", message)
	cmd.Dir = dir
	if output, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git commit failed: %w: %s", err, string(output))
	}

	return nil
}

// gitGetCurrentBranch returns the current branch name.
func gitGetCurrentBranch(dir string) (string, error) {
	cmd := exec.Command("git", "branch", "--show-current")
	cmd.Dir = dir
	output, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("git branch --show-current failed: %w", err)
	}
	return strings.TrimSpace(string(output)), nil
}

// gitLog returns the list of commit messages.
func gitLog(dir string) ([]string, error) {
	cmd := exec.Command("git", "log", "--oneline")
	cmd.Dir = dir
	output, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("git log failed: %w", err)
	}
	lines := strings.Split(strings.TrimSpace(string(output)), "\n")
	return lines, nil
}
