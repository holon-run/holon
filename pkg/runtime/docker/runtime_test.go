package docker

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/holon-run/holon/pkg/workspace"
)

func TestNewRuntime(t *testing.T) {
	rt, err := NewRuntime()
	if err != nil {
		t.Skipf("Skipping integration test: Docker daemon not reachable or client error: %v", err)
	}
	if rt.cli == nil {
		t.Error("Expected non-nil docker client")
	}
}

// TestRunHolon_ConfigAssembly tests the pure configuration assembly logic
// without requiring Docker daemon by using the extracted functions
func TestRunHolon_ConfigAssembly(t *testing.T) {
	// Create temporary test files
	tmpDir, err := os.MkdirTemp("", "holon-config-test-*")
	if err != nil {
		t.Fatalf("Failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

	specFile := filepath.Join(tmpDir, "spec.yaml")
	if err := os.WriteFile(specFile, []byte("test: spec"), 0644); err != nil {
		t.Fatalf("Failed to create test spec file: %v", err)
	}

	outDir := filepath.Join(tmpDir, "output")
	if err := os.MkdirAll(outDir, 0755); err != nil {
		t.Fatalf("Failed to create output dir: %v", err)
	}

	contextDir := filepath.Join(tmpDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		t.Fatalf("Failed to create context dir: %v", err)
	}

	promptFile := filepath.Join(tmpDir, "system.md")
	if err := os.WriteFile(promptFile, []byte("# System Prompt"), 0644); err != nil {
		t.Fatalf("Failed to create test prompt file: %v", err)
	}

	userPromptFile := filepath.Join(tmpDir, "user.md")
	if err := os.WriteFile(userPromptFile, []byte("# User Prompt"), 0644); err != nil {
		t.Fatalf("Failed to create test user prompt file: %v", err)
	}

	// Test mount configuration assembly
	t.Run("mount assembly", func(t *testing.T) {
		// Create input directory structure
		inputDir := t.TempDir()
		if err := os.WriteFile(filepath.Join(inputDir, "spec.yaml"), []byte("test"), 0644); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(filepath.Join(inputDir, "context"), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(filepath.Join(inputDir, "prompts"), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(inputDir, "prompts", "system.md"), []byte("# System Prompt"), 0644); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(inputDir, "prompts", "user.md"), []byte("# User Prompt"), 0644); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: "/tmp/workspace-snapshot",
			InputPath:   inputDir,
			OutDir:      outDir,
		}

		mounts := BuildContainerMounts(cfg)

		// Verify we get expected number of mounts (workspace, input, output)
		expectedMountCount := 3
		if len(mounts) != expectedMountCount {
			t.Errorf("Expected %d mounts, got %d", expectedMountCount, len(mounts))
		}

		// Verify mount targets are correct
		targets := make(map[string]string)
		for _, m := range mounts {
			targets[m.Target] = m.Source
		}

		expectedTargets := map[string]string{
			"/holon/workspace": "/tmp/workspace-snapshot",
			"/holon/input":     inputDir,
			"/holon/output":    outDir,
		}

		for target, expectedSource := range expectedTargets {
			actualSource, exists := targets[target]
			if !exists {
				t.Errorf("Missing mount for target: %s", target)
			} else if actualSource != expectedSource {
				t.Errorf("Mount target %s: expected source %s, got %s", target, expectedSource, actualSource)
			}
		}
	})

	// Test environment variable assembly
	t.Run("env assembly", func(t *testing.T) {
		cfg := &EnvConfig{
			UserEnv: map[string]string{
				"ANTHROPIC_API_KEY": "test-key-123",
				"DEBUG":             "true",
				"CUSTOM_VAR":        "custom-value",
			},
			HostUID: 1000,
			HostGID: 1000,
		}

		env := BuildContainerEnv(cfg)

		// Verify we get expected number of env vars
		expectedEnvCount := 6 // 3 user vars + HOST_UID + HOST_GID + GIT_CONFIG_NOSYSTEM
		if len(env) != expectedEnvCount {
			t.Errorf("Expected %d env vars, got %d", expectedEnvCount, len(env))
		}

		// Verify specific env vars
		envSet := make(map[string]bool)
		for _, e := range env {
			envSet[e] = true
		}

		expectedEnv := []string{
			"ANTHROPIC_API_KEY=test-key-123",
			"DEBUG=true",
			"CUSTOM_VAR=custom-value",
			"HOST_UID=1000",
			"HOST_GID=1000",
			"GIT_CONFIG_NOSYSTEM=1",
		}

		for _, expectedVar := range expectedEnv {
			if !envSet[expectedVar] {
				t.Errorf("Missing expected env var: %s", expectedVar)
			}
		}
	})

	// Test mount target validation
	t.Run("mount target validation", func(t *testing.T) {
		// Create input directory structure
		inputDir := t.TempDir()
		if err := os.WriteFile(filepath.Join(inputDir, "spec.yaml"), []byte("test"), 0644); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(filepath.Join(inputDir, "context"), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(filepath.Join(inputDir, "prompts"), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(inputDir, "prompts", "system.md"), []byte("# System Prompt"), 0644); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(inputDir, "prompts", "user.md"), []byte("# User Prompt"), 0644); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: "/tmp/snapshot-test",
			InputPath:   inputDir,
			OutDir:      outDir,
		}

		// Should pass validation
		if err := ValidateMountTargets(cfg); err != nil {
			t.Errorf("Expected no validation error, got: %v", err)
		}

		// Test invalid configuration
		invalidCfg := &MountConfig{
			SnapshotDir: "", // Empty snapshot dir should fail
			InputPath:   inputDir,
			OutDir:      outDir,
		}

		if err := ValidateMountTargets(invalidCfg); err == nil {
			t.Error("Expected validation error for empty snapshot dir")
		}
	})
}

// TestComposedImageTagGeneration verifies that the tag generation is stable and valid
func TestComposedImageTagGeneration(t *testing.T) {
	// Test data
	testCases := []struct {
		name         string
		baseImage    string
		bundleDigest string
	}{
		{
			name:         "standard bundle",
			baseImage:    "golang:1.22",
			bundleDigest: "bundle-a",
		},
		{
			name:         "same bundle should produce same tag",
			baseImage:    "golang:1.22",
			bundleDigest: "bundle-a",
		},
		{
			name:         "different base image",
			baseImage:    "python:3.11",
			bundleDigest: "bundle-a",
		},
		{
			name:         "different bundle digest",
			baseImage:    "golang:1.22",
			bundleDigest: "bundle-b",
		},
	}

	// Generate tags for each test case
	tags := make(map[string]string)
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			// Generate tag using the same logic as buildComposedImage
			tag := composeImageTag(tc.baseImage, tc.bundleDigest)

			t.Logf("Generated tag for %s + %s: %s", tc.baseImage, tc.bundleDigest, tag)

			// Verify tag format
			if !strings.HasPrefix(tag, "holon-composed-") {
				t.Errorf("Tag should start with 'holon-composed-', got: %s", tag)
			}

			// Verify tag contains valid hex characters only after prefix
			hashPart := strings.TrimPrefix(tag, "holon-composed-")
			if len(hashPart) != 24 { // 12 bytes = 24 hex characters
				t.Errorf("Hash part should be 24 characters, got: %d", len(hashPart))
			}

			// Store for consistency check
			key := tc.baseImage + ":" + tc.bundleDigest
			if existingTag, exists := tags[key]; exists {
				if existingTag != tag {
					t.Errorf("Inconsistent tag generation: same inputs produced different tags: %s vs %s", existingTag, tag)
				}
			} else {
				tags[key] = tag
			}

			// Verify tag doesn't contain invalid characters (only check the hash part)
			hashPart = strings.TrimPrefix(tag, "holon-composed-")
			for _, r := range hashPart {
				if !((r >= 'a' && r <= 'f') || (r >= '0' && r <= '9')) {
					t.Errorf("Tag hash part contains invalid character '%c': %s", r, tag)
				}
			}
		})
	}

	// Verify that different inputs produce different tags
	uniqueTags := make(map[string]bool)
	for _, tag := range tags {
		uniqueTags[tag] = true
	}

	if len(uniqueTags) != len(tags) {
		t.Errorf("Different inputs should produce different tags. Got %d unique tags for %d input combinations", len(uniqueTags), len(tags))
	}
}

func TestMkdirTempOutsideWorkspace_DoesNotNest(t *testing.T) {
	ws := t.TempDir()
	tmpInside := filepath.Join(ws, "tmp")
	if err := os.MkdirAll(tmpInside, 0o755); err != nil {
		t.Fatalf("mkdir tmp: %v", err)
	}
	t.Setenv("TMPDIR", tmpInside)

	dir, err := workspace.MkdirTempOutsideWorkspace(ws, "holon-test-*")
	if err != nil {
		t.Fatalf("mkdirTempOutsideWorkspace: %v", err)
	}
	defer os.RemoveAll(dir)

	// Use filepath.Abs for verification since cleanAbs is now private
	absWs, err := filepath.Abs(ws)
	if err != nil {
		t.Fatalf("abs workspace: %v", err)
	}
	absDir, err := filepath.Abs(dir)
	if err != nil {
		t.Fatalf("abs dir: %v", err)
	}

	// Check that dir is not a subpath of workspace
	rel, err := filepath.Rel(absWs, absDir)
	if err != nil {
		t.Fatalf("rel: %v", err)
	}
	rel = filepath.Clean(rel)
	if rel == "." || (!strings.HasPrefix(rel, ".."+string(filepath.Separator)) && rel != "..") {
		t.Fatalf("expected snapshot dir to be outside workspace:\nworkspace=%s\ndir=%s\nrel=%s", absWs, absDir, rel)
	}
}

// TestGetGitConfig tests the getGitConfig helper function
func TestGetGitConfig(t *testing.T) {
	// Skip test on Windows as it relies on Unix shell scripts
	if runtime.GOOS == "windows" {
		t.Skip("Skipping test on Windows - requires Unix shell")
	}

	// Save original PATH for restoring in each test
	originalPath := os.Getenv("PATH")

	tests := []struct {
		name           string
		key            string
		setupFunc      func()
		expectedResult string
		expectedError  bool
		errorContains  string
	}{
		{
			name: "successful git config retrieval",
			key:  "user.name",
			setupFunc: func() {
				// Create a mock git command that returns a known value
				tempDir := t.TempDir()
				mockGit := filepath.Join(tempDir, "git")
				script := `#!/bin/bash
if [ "$1" = "config" ] && [ "$2" = "--get" ] && [ "$3" = "user.name" ]; then
	echo "Test User"
	exit 0
fi
exit 1`
				if err := os.WriteFile(mockGit, []byte(script), 0755); err != nil {
					t.Fatalf("Failed to create mock git: %v", err)
				}

				// Prepend temp dir to PATH to use our mock git
				// Use list.ListSeparator for cross-platform compatibility
				pathSeparator := string(filepath.ListSeparator)
				t.Setenv("PATH", tempDir+pathSeparator+originalPath)
				// Cleanup handled by t.TempDir()
			},
			expectedResult: "Test User",
			expectedError:  false,
		},
		{
			name: "git config with leading/trailing whitespace",
			key:  "user.email",
			setupFunc: func() {
				tempDir := t.TempDir()
				mockGit := filepath.Join(tempDir, "git")
				script := `#!/bin/bash
if [ "$1" = "config" ] && [ "$2" = "--get" ] && [ "$3" = "user.email" ]; then
	echo "  test@example.com  "
	exit 0
fi
exit 1`
				if err := os.WriteFile(mockGit, []byte(script), 0755); err != nil {
					t.Fatalf("Failed to create mock git: %v", err)
				}

				pathSeparator := string(filepath.ListSeparator)
				t.Setenv("PATH", tempDir+pathSeparator+originalPath)
			},
			expectedResult: "test@example.com",
			expectedError:  false,
		},
		{
			name: "git config with multiline output",
			key:  "user.name",
			setupFunc: func() {
				tempDir := t.TempDir()
				mockGit := filepath.Join(tempDir, "git")
				script := `#!/bin/bash
if [ "$1" = "config" ] && [ "$2" = "--get" ] && [ "$3" = "user.name" ]; then
	echo -e "Test User\\n\\n"
	exit 0
fi
exit 1`
				if err := os.WriteFile(mockGit, []byte(script), 0755); err != nil {
					t.Fatalf("Failed to create mock git: %v", err)
				}

				pathSeparator := string(filepath.ListSeparator)
				t.Setenv("PATH", tempDir+pathSeparator+originalPath)
			},
			expectedResult: "Test User",
			expectedError:  false,
		},
		{
			name: "git command not found",
			key:  "user.name",
			setupFunc: func() {
				// Set PATH to empty directory so git is not found
				tempDir := t.TempDir()
				t.Setenv("PATH", tempDir)
			},
			expectedResult: "",
			expectedError:  true,
			errorContains:  "executable file not found",
		},
		{
			name: "git config key not found",
			key:  "nonexistent.key",
			setupFunc: func() {
				tempDir := t.TempDir()
				mockGit := filepath.Join(tempDir, "git")
				script := `#!/bin/bash
if [ "$1" = "config" ] && [ "$2" = "--get" ] && [ "$3" = "nonexistent.key" ]; then
	exit 1
fi
exit 1`
				if err := os.WriteFile(mockGit, []byte(script), 0755); err != nil {
					t.Fatalf("Failed to create mock git: %v", err)
				}

				pathSeparator := string(filepath.ListSeparator)
				t.Setenv("PATH", tempDir+pathSeparator+originalPath)
			},
			expectedResult: "",
			expectedError:  true,
		},
		{
			name: "git command exits with error",
			key:  "user.name",
			setupFunc: func() {
				tempDir := t.TempDir()
				mockGit := filepath.Join(tempDir, "git")
				script := `#!/bin/bash
echo "git: error" >&2
exit 128`
				if err := os.WriteFile(mockGit, []byte(script), 0755); err != nil {
					t.Fatalf("Failed to create mock git: %v", err)
				}

				pathSeparator := string(filepath.ListSeparator)
				t.Setenv("PATH", tempDir+pathSeparator+originalPath)
			},
			expectedResult: "",
			expectedError:  true,
		},
		{
			name: "empty git config value",
			key:  "user.name",
			setupFunc: func() {
				tempDir := t.TempDir()
				mockGit := filepath.Join(tempDir, "git")
				script := `#!/bin/bash
if [ "$1" = "config" ] && [ "$2" = "--get" ] && [ "$3" = "user.name" ]; then
	echo ""
	exit 0
fi
exit 1`
				if err := os.WriteFile(mockGit, []byte(script), 0755); err != nil {
					t.Fatalf("Failed to create mock git: %v", err)
				}

				pathSeparator := string(filepath.ListSeparator)
				t.Setenv("PATH", tempDir+pathSeparator+originalPath)
			},
			expectedResult: "",
			expectedError:  false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Setup test environment
			if tt.setupFunc != nil {
				tt.setupFunc()
				// No cleanup needed - t.Setenv handles restoration automatically
			}

			// Test the function
			result, err := getGitConfig(tt.key)

			// Check expectations
			if tt.expectedError {
				if err == nil {
					t.Errorf("getGitConfig(%q) expected error but got none", tt.key)
				} else if tt.errorContains != "" && !strings.Contains(err.Error(), tt.errorContains) {
					t.Errorf("getGitConfig(%q) expected error containing %q, got %q", tt.key, tt.errorContains, err.Error())
				}
			} else {
				if err != nil {
					t.Errorf("getGitConfig(%q) unexpected error: %v", tt.key, err)
				}
			}

			if result != tt.expectedResult {
				t.Errorf("getGitConfig(%q) = %q, want %q", tt.key, result, tt.expectedResult)
			}
		})
	}
}

// TestIsGitRepo tests the workspace.IsGitRepo helper function
func TestIsGitRepo(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Skipping test on Windows - requires Unix shell")
	}

	tests := []struct {
		name     string
		setupDir func(t *testing.T) string
		want     bool
	}{
		{
			name: "git repository",
			setupDir: func(t *testing.T) string {
				dir := t.TempDir()
				// Initialize a git repository
				if err := runCmd(dir, "git", "init"); err != nil {
					t.Fatalf("git init failed: %v", err)
				}
				if err := runCmd(dir, "git", "config", "user.email", "test@example.com"); err != nil {
					t.Fatalf("git config failed: %v", err)
				}
				if err := runCmd(dir, "git", "config", "user.name", "Test User"); err != nil {
					t.Fatalf("git config failed: %v", err)
				}
				// Create and commit a file
				testFile := filepath.Join(dir, "test.txt")
				if err := os.WriteFile(testFile, []byte("test"), 0644); err != nil {
					t.Fatalf("write test file failed: %v", err)
				}
				if err := runCmd(dir, "git", "add", "test.txt"); err != nil {
					t.Fatalf("git add failed: %v", err)
				}
				if err := runCmd(dir, "git", "commit", "-m", "initial"); err != nil {
					t.Fatalf("git commit failed: %v", err)
				}
				return dir
			},
			want: true,
		},
		{
			name: "non-git directory",
			setupDir: func(t *testing.T) string {
				return t.TempDir()
			},
			want: false,
		},
		{
			name: "subdirectory of git repository",
			setupDir: func(t *testing.T) string {
				dir := t.TempDir()
				// Initialize a git repository
				if err := runCmd(dir, "git", "init"); err != nil {
					t.Fatalf("git init failed: %v", err)
				}
				if err := runCmd(dir, "git", "config", "user.email", "test@example.com"); err != nil {
					t.Fatalf("git config failed: %v", err)
				}
				if err := runCmd(dir, "git", "config", "user.name", "Test User"); err != nil {
					t.Fatalf("git config failed: %v", err)
				}
				// Create a subdirectory
				subdir := filepath.Join(dir, "subdir")
				if err := os.MkdirAll(subdir, 0755); err != nil {
					t.Fatalf("mkdir failed: %v", err)
				}
				return subdir
			},
			want: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := tt.setupDir(t)
			got := workspace.IsGitRepo(dir)
			if got != tt.want {
				t.Errorf("IsGitRepo(%q) = %v, want %v", dir, got, tt.want)
			}
		})
	}
}

// TestCreateClone tests the GitClonePreparer workspace preparation
func TestCreateClone(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Skipping test on Windows - requires Unix shell")
	}

	// Create a temporary git repository
	sourceRepo := t.TempDir()
	if err := runCmd(sourceRepo, "git", "init"); err != nil {
		t.Skipf("git not available: %v", err)
	}
	if err := runCmd(sourceRepo, "git", "config", "user.email", "test@example.com"); err != nil {
		t.Fatalf("git config failed: %v", err)
	}
	if err := runCmd(sourceRepo, "git", "config", "user.name", "Test User"); err != nil {
		t.Fatalf("git config failed: %v", err)
	}

	// Create and commit a file
	testFile := filepath.Join(sourceRepo, "test.txt")
	if err := os.WriteFile(testFile, []byte("test content"), 0644); err != nil {
		t.Fatalf("write test file failed: %v", err)
	}
	if err := runCmd(sourceRepo, "git", "add", "test.txt"); err != nil {
		t.Fatalf("git add failed: %v", err)
	}
	if err := runCmd(sourceRepo, "git", "commit", "-m", "initial commit"); err != nil {
		t.Fatalf("git commit failed: %v", err)
	}

	// Test creating a clone using the GitClonePreparer
	clonePath := filepath.Join(t.TempDir(), "clone")
	preparer := workspace.NewGitClonePreparer()
	result, err := preparer.Prepare(context.Background(), workspace.PrepareRequest{
		Source:  sourceRepo,
		Dest:    clonePath,
		History: workspace.HistoryFull,
	})
	if err != nil {
		t.Fatalf("GitClonePreparer.Prepare failed: %v", err)
	}
	_ = result // Use result to avoid unused variable warning

	// Verify clone was created
	if _, err := os.Stat(clonePath); err != nil {
		t.Errorf("clone directory not created: %v", err)
	}

	// Verify the file exists in the clone
	cloneFile := filepath.Join(clonePath, "test.txt")
	content, err := os.ReadFile(cloneFile)
	if err != nil {
		t.Errorf("failed to read file in clone: %v", err)
	}
	if string(content) != "test content" {
		t.Errorf("file content mismatch: got %q, want %q", string(content), "test content")
	}

	// Verify .git is a directory (not a file) in the clone - this is the key difference from worktree
	gitEntry := filepath.Join(clonePath, ".git")
	info, err := os.Lstat(gitEntry)
	if err != nil {
		t.Errorf("failed to stat .git in clone: %v", err)
	}
	if info.Mode()&os.ModeDir == 0 {
		t.Error(".git in clone should be a directory, not a file")
	}

	// Verify clone has its own object database (no alternates file)
	alternatesFile := filepath.Join(clonePath, ".git", "objects", "info", "alternates")
	if _, err := os.Stat(alternatesFile); err == nil {
		t.Error("clone should not have alternates file (should have own object database)")
	}

	// Verify workspace.manifest.json was created
	manifestPath := filepath.Join(clonePath, "workspace.manifest.json")
	if _, err := os.Stat(manifestPath); err != nil {
		t.Errorf("workspace.manifest.json not created: %v", err)
	}

	// Test git operations work correctly in the clone (this works in containers too!)
	// Make a change, stage it, and verify it's tracked
	modifiedFile := filepath.Join(clonePath, "test.txt")
	if err := os.WriteFile(modifiedFile, []byte("modified content"), 0644); err != nil {
		t.Fatalf("failed to modify file in clone: %v", err)
	}
	// Stage the change
	if err := runCmd(clonePath, "git", "add", "test.txt"); err != nil {
		t.Fatalf("git add in clone failed: %v", err)
	}
	// Verify the change is staged
	out, err := exec.Command("git", "-C", clonePath, "diff", "--cached", "--name-only").CombinedOutput()
	if err != nil {
		t.Fatalf("git diff --cached failed: %v", err)
	}
	if !strings.Contains(string(out), "test.txt") {
		t.Errorf("expected test.txt to be staged, got: %s", string(out))
	}

	// Test cleanup: remove the clone and verify cleanup
	if err := preparer.Cleanup(clonePath); err != nil {
		t.Errorf("failed to remove clone: %v", err)
	}
	// Verify clone directory was removed
	if _, err := os.Stat(clonePath); err == nil {
		t.Error("clone directory still exists after removal")
	}

	// Verify source repo is still intact
	sourceContent, err := os.ReadFile(testFile)
	if err != nil {
		t.Errorf("failed to read source file after clone cleanup: %v", err)
	}
	if string(sourceContent) != "test content" {
		t.Errorf("source file content changed: got %q, want %q", string(sourceContent), "test content")
	}
}
// runCmd is a helper to run a command in a directory
func runCmd(dir string, name string, args ...string) error {
	cmd := exec.Command(name, args...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("command %s %v failed: %v, output: %s", name, args, err, string(out))
	}
	return nil
}
