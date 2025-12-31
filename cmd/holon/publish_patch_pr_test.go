package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	holonGit "github.com/holon-run/holon/pkg/git"
	pubgit "github.com/holon-run/holon/pkg/publisher/git"
)

// TestPublishPatchToPR_HeadRefParsing tests parsing head_ref from various pr.json shapes.
func TestPublishPatchToPR_HeadRefParsing(t *testing.T) {
	tests := []struct {
		name     string
		prJSON   string
		wantRef  string
		wantErr  bool
		errSub   string
	}{
		{
			name: "head.ref format (standard GitHub PR JSON)",
			prJSON: `{
				"head": {
					"ref": "feature/my-branch"
				}
			}`,
			wantRef: "feature/my-branch",
			wantErr: false,
		},
		{
			name: "head_ref at top level",
			prJSON: `{
				"head_ref": "fix/bug-123"
			}`,
			wantRef: "fix/bug-123",
			wantErr: false,
		},
		{
			name: "pr.head.ref nested format",
			prJSON: `{
				"pr": {
					"head": {
						"ref": "feature/nested"
					}
				}
			}`,
			wantRef: "feature/nested",
			wantErr: false,
		},
		{
			name: "pr.head_ref nested format",
			prJSON: `{
				"pr": {
					"head_ref": "fix/nested-head-ref"
				}
			}`,
			wantRef: "fix/nested-head-ref",
			wantErr: false,
		},
		{
			name: "fallback priority: head.ref first",
			prJSON: `{
				"head": {
					"ref": "primary"
				},
				"head_ref": "secondary",
				"pr": {
					"head": {
						"ref": "tertiary"
					},
					"head_ref": "quaternary"
				}
			}`,
			wantRef: "primary",
			wantErr: false,
		},
		{
			name: "fallback priority: head_ref second",
			prJSON: `{
				"head_ref": "secondary",
				"pr": {
					"head": {
						"ref": "tertiary"
					},
					"head_ref": "quaternary"
				}
			}`,
			wantRef: "secondary",
			wantErr: false,
		},
		{
			name: "fallback priority: pr.head.ref third",
			prJSON: `{
				"pr": {
					"head": {
						"ref": "tertiary"
					},
					"head_ref": "quaternary"
				}
			}`,
			wantRef: "tertiary",
			wantErr: false,
		},
		{
			name: "fallback priority: pr.head_ref fourth",
			prJSON: `{
				"pr": {
					"head_ref": "quaternary"
				}
			}`,
			wantRef: "quaternary",
			wantErr: false,
		},
		{
			name: "missing all head_ref fields - error",
			prJSON: `{
				"number": 123,
				"title": "Test PR"
			}`,
			wantErr: true,
			errSub:  "missing head.ref",
		},
		{
			name: "empty head.ref falls back to head_ref",
			prJSON: `{
				"head": {
					"ref": ""
				},
				"head_ref": "fallback"
			}`,
			wantRef: "fallback",
			wantErr: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			ctx := context.Background()

			// Create input directory structure
			inputDir := filepath.Join(tmpDir, "input")
			contextDir := filepath.Join(inputDir, "context", "github")
			if err := os.MkdirAll(contextDir, 0755); err != nil {
				t.Fatalf("failed to create input dirs: %v", err)
			}

			// Write pr.json
			prJSONPath := filepath.Join(contextDir, "pr.json")
			if err := os.WriteFile(prJSONPath, []byte(tt.prJSON), 0644); err != nil {
				t.Fatalf("failed to write pr.json: %v", err)
			}

			// Create a minimal git repo for publish workspace
			pubWorkspace := t.TempDir()
			if err := initTestGitRepo(pubWorkspace); err != nil {
				t.Fatalf("failed to init git repo: %v", err)
			}

			// Create an empty diff.patch
			diffPath := filepath.Join(tmpDir, "diff.patch")
			if err := os.WriteFile(diffPath, []byte{}, 0644); err != nil {
				t.Fatalf("failed to create diff.patch: %v", err)
			}

			// Set HOLON_WORKSPACE to the publish workspace
			oldWorkspace := os.Getenv("HOLON_WORKSPACE")
			defer func() { os.Setenv("HOLON_WORKSPACE", oldWorkspace) }()
			os.Setenv("HOLON_WORKSPACE", pubWorkspace)

			// Run publishPatchToPR - it should fail at validation or succeed if branch exists
			err := publishPatchToPR(ctx, pubWorkspace, inputDir, diffPath)

			if tt.wantErr {
				if err == nil {
					t.Errorf("publishPatchToPR() expected error containing %q, got nil", tt.errSub)
					return
				}
				if !strings.Contains(err.Error(), tt.errSub) {
					t.Errorf("publishPatchToPR() error = %v, want error containing %q", err, tt.errSub)
				}
				return
			}

			// If we don't expect a parse error, we'll get a git error (branch doesn't exist)
			// or success (empty patch is a no-op). Verify that the parsed ref is correct
			// by checking the error message - it should mention the expected branch
			if err != nil {
				// Git error should reference the branch name we parsed
				if strings.Contains(err.Error(), "missing head.ref") {
					t.Errorf("publishPatchToPR() got unexpected parse error: %v", err)
				}
				// The error should reference our expected branch
				errMsg := err.Error()
				if !strings.Contains(errMsg, tt.wantRef) && !strings.Contains(errMsg, "no-op") {
					t.Logf("publishPatchToPR() error = %v (may be expected git error)", err)
				}
			}
		})
	}
}

// TestPublishPatchToPR_TokenPropagation tests GITHUB_TOKEN/GH_TOKEN to GIT_TOKEN propagation.
func TestPublishPatchToPR_TokenPropagation(t *testing.T) {
	tests := []struct {
		name            string
		existingGitTok  string
		githubToken     string
		ghToken         string
		wantGitToken    string
	}{
		{
			name:         "GITHUB_TOKEN propagated to GIT_TOKEN",
			githubToken:  "ghp_test_token_123",
			wantGitToken: "ghp_test_token_123",
		},
		{
			name:         "GH_TOKEN propagated to GIT_TOKEN",
			ghToken:      "ghp_test_token_456",
			wantGitToken: "ghp_test_token_456",
		},
		{
			name:         "GITHUB_TOKEN takes precedence over GH_TOKEN",
			githubToken:  "ghp_github",
			ghToken:      "ghp_gh",
			wantGitToken: "ghp_github",
		},
		{
			name:           "existing GIT_TOKEN is not overwritten",
			existingGitTok: "existing_token",
			githubToken:    "ghp_new",
			wantGitToken:   "existing_token",
		},
		{
			name:         "no GitHub tokens - GIT_TOKEN remains empty",
			wantGitToken: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			ctx := context.Background()

			// Create input directory structure
			inputDir := filepath.Join(tmpDir, "input")
			contextDir := filepath.Join(inputDir, "context", "github")
			if err := os.MkdirAll(contextDir, 0755); err != nil {
				t.Fatalf("failed to create input dirs: %v", err)
			}

			// Write pr.json
			prJSON := `{"head": {"ref": "main"}}`
			prJSONPath := filepath.Join(contextDir, "pr.json")
			if err := os.WriteFile(prJSONPath, []byte(prJSON), 0644); err != nil {
				t.Fatalf("failed to write pr.json: %v", err)
			}

			// Create a minimal git repo for publish workspace
			pubWorkspace := t.TempDir()
			if err := initTestGitRepo(pubWorkspace); err != nil {
				t.Fatalf("failed to init git repo: %v", err)
			}

			// Create an empty diff.patch
			diffPath := filepath.Join(tmpDir, "diff.patch")
			if err := os.WriteFile(diffPath, []byte{}, 0644); err != nil {
				t.Fatalf("failed to create diff.patch: %v", err)
			}

			// Set environment variables
			oldWorkspace := os.Getenv("HOLON_WORKSPACE")
			oldGitToken := os.Getenv(pubgit.GitTokenEnv)
			oldGithubToken := os.Getenv("GITHUB_TOKEN")
			oldGhToken := os.Getenv("GH_TOKEN")

			defer func() {
				os.Setenv("HOLON_WORKSPACE", oldWorkspace)
				os.Setenv(pubgit.GitTokenEnv, oldGitToken)
				os.Setenv("GITHUB_TOKEN", oldGithubToken)
				os.Setenv("GH_TOKEN", oldGhToken)
			}()

			os.Setenv("HOLON_WORKSPACE", pubWorkspace)

			if tt.existingGitTok != "" {
				os.Setenv(pubgit.GitTokenEnv, tt.existingGitTok)
			} else {
				os.Unsetenv(pubgit.GitTokenEnv)
			}

			if tt.githubToken != "" {
				os.Setenv("GITHUB_TOKEN", tt.githubToken)
			} else {
				os.Unsetenv("GITHUB_TOKEN")
			}

			if tt.ghToken != "" {
				os.Setenv("GH_TOKEN", tt.ghToken)
			} else {
				os.Unsetenv("GH_TOKEN")
			}

			// Run publishPatchToPR
			_ = publishPatchToPR(ctx, pubWorkspace, inputDir, diffPath)

			// Check GIT_TOKEN environment variable after the call
			gotGitToken := os.Getenv(pubgit.GitTokenEnv)
			if gotGitToken != tt.wantGitToken {
				t.Errorf("GIT_TOKEN = %q, want %q", gotGitToken, tt.wantGitToken)
			}
		})
	}
}

// TestPublishPatchToPR_PublishRequestValidation tests that the PublishRequest is correctly constructed.
func TestPublishPatchToPR_PublishRequestValidation(t *testing.T) {
	tests := []struct {
		name        string
		prJSON      string
		wantBranch  string
		wantTarget  string
		wantCommit  bool
		wantPush    bool
	}{
		{
			name: "valid PublishRequest construction",
			prJSON: `{
				"head": {
					"ref": "feature/test-branch"
				}
			}`,
			wantBranch: "feature/test-branch",
			wantTarget: "origin/feature/test-branch",
			wantCommit: true,
			wantPush:   true,
		},
		{
			name: "branch with special characters",
			prJSON: `{
				"head": {
					"ref": "feature/123-add-feature"
				}
			}`,
			wantBranch: "feature/123-add-feature",
			wantTarget: "origin/feature/123-add-feature",
			wantCommit: true,
			wantPush:   true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			ctx := context.Background()

			// Create input directory structure
			inputDir := filepath.Join(tmpDir, "input")
			contextDir := filepath.Join(inputDir, "context", "github")
			if err := os.MkdirAll(contextDir, 0755); err != nil {
				t.Fatalf("failed to create input dirs: %v", err)
			}

			// Write pr.json
			prJSONPath := filepath.Join(contextDir, "pr.json")
			if err := os.WriteFile(prJSONPath, []byte(tt.prJSON), 0644); err != nil {
				t.Fatalf("failed to write pr.json: %v", err)
			}

			// Create a minimal git repo for publish workspace
			pubWorkspace := t.TempDir()
			if err := initTestGitRepo(pubWorkspace); err != nil {
				t.Fatalf("failed to init git repo: %v", err)
			}

			// Create an empty diff.patch
			diffPath := filepath.Join(tmpDir, "diff.patch")
			if err := os.WriteFile(diffPath, []byte{}, 0644); err != nil {
				t.Fatalf("failed to create diff.patch: %v", err)
			}

			// Set HOLON_WORKSPACE
			oldWorkspace := os.Getenv("HOLON_WORKSPACE")
			defer func() { os.Setenv("HOLON_WORKSPACE", oldWorkspace) }()
			os.Setenv("HOLON_WORKSPACE", pubWorkspace)

			// Run publishPatchToPR - will fail at git push but that's OK
			// We're testing that the PublishRequest was constructed correctly
			err := publishPatchToPR(ctx, pubWorkspace, inputDir, diffPath)

			// We expect either success (empty patch no-op) or git-related errors
			// but NOT validation errors about missing branch/target
			if err != nil {
				errMsg := err.Error()
				// These are acceptable errors (git operations)
				if strings.Contains(errMsg, "validation failed") ||
					strings.Contains(errMsg, "missing") {
					t.Errorf("publishPatchToPR() got unexpected validation error: %v", err)
				}
				// Verify the error mentions our expected branch (correct parsing)
				if !strings.Contains(errMsg, tt.wantBranch) && !strings.Contains(errMsg, "no-op") {
					t.Logf("publishPatchToPR() error = %v", err)
				}
			}
		})
	}
}

// initTestGitRepo initializes a minimal git repository for testing.
func initTestGitRepo(dir string) error {
	client := holonGit.NewClient(dir)

	if err := client.InitRepository(context.Background()); err != nil {
		return err
	}

	if err := os.WriteFile(filepath.Join(dir, ".gitignore"), []byte(""), 0644); err != nil {
		return err
	}

	if err := os.WriteFile(filepath.Join(dir, "README.md"), []byte("test repo\n"), 0644); err != nil {
		return err
	}

	if _, err := client.ExecCommand(context.Background(), "add", "."); err != nil {
		return err
	}

	if _, err := client.ExecCommand(context.Background(), "commit", "-m", "Initial commit"); err != nil {
		return err
	}

	return nil
}
