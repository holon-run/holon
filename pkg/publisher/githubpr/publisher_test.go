package githubpr

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	holonGit "github.com/holon-run/holon/pkg/git"
	"github.com/holon-run/holon/pkg/publisher"
)

func TestPRPublisherName(t *testing.T) {
	p := NewPRPublisher()
	if got := p.Name(); got != "github-pr" {
		t.Errorf("PRPublisher.Name() = %v, want %v", got, "github-pr")
	}
}

func TestPRPublisherValidate(t *testing.T) {
	tests := []struct {
		name    string
		req     publisher.PublishRequest
		wantErr bool
		errMsg  string
	}{
		{
			name: "valid request",
			req: publisher.PublishRequest{
				Target: "holon-run/holon",
				Artifacts: map[string]string{
					"diff.patch":  "/tmp/diff.patch",
					"summary.md":  "/tmp/summary.md",
				},
			},
			wantErr: false,
		},
		{
			name: "valid request with base branch",
			req: publisher.PublishRequest{
				Target: "holon-run/holon:develop",
				Artifacts: map[string]string{
					"diff.patch":  "/tmp/diff.patch",
					"summary.md":  "/tmp/summary.md",
				},
			},
			wantErr: false,
		},
		{
			name: "invalid target format",
			req: publisher.PublishRequest{
				Target: "invalid-format",
				Artifacts: map[string]string{
					"diff.patch":  "/tmp/diff.patch",
					"summary.md":  "/tmp/summary.md",
				},
			},
			wantErr: true,
			errMsg:  "invalid target format",
		},
		{
			name: "missing diff.patch artifact",
			req: publisher.PublishRequest{
				Target: "holon-run/holon",
				Artifacts: map[string]string{
					"summary.md": "/tmp/summary.md",
				},
			},
			wantErr: true,
			errMsg:  "required artifact 'diff.patch' not found",
		},
		{
			name: "missing summary.md artifact",
			req: publisher.PublishRequest{
				Target: "holon-run/holon",
				Artifacts: map[string]string{
					"diff.patch": "/tmp/diff.patch",
				},
			},
			wantErr: true,
			errMsg:  "required artifact 'summary.md' not found",
		},
		{
			name: "incomplete repository reference",
			req: publisher.PublishRequest{
				Target: "/repo",
				Artifacts: map[string]string{
					"diff.patch": "/tmp/diff.patch",
					"summary.md": "/tmp/summary.md",
				},
			},
			wantErr: true,
			errMsg:  "invalid target format",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			p := NewPRPublisher()
			err := p.Validate(tt.req)
			if (err != nil) != tt.wantErr {
				t.Errorf("PRPublisher.Validate() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if tt.wantErr && tt.errMsg != "" {
				if err == nil {
					t.Errorf("PRPublisher.Validate() expected error containing %q, got nil", tt.errMsg)
				} else if err.Error() != tt.errMsg && !strings.Contains(err.Error(), tt.errMsg) {
					t.Errorf("PRPublisher.Validate() error = %q, want error containing %q", err.Error(), tt.errMsg)
				}
			}
		})
	}
}

func TestPRPublisherBuildConfig(t *testing.T) {
	p := NewPRPublisher()

	tests := []struct {
		name     string
		manifest map[string]interface{}
		want     PRPublisherConfig
	}{
		{
			name:     "empty manifest",
			manifest: map[string]interface{}{},
			want:     PRPublisherConfig{},
		},
		{
			name: "with metadata",
			manifest: map[string]interface{}{
				"metadata": map[string]interface{}{
					"branch":   "custom/branch",
					"title":    "Custom Title",
					"issue_id": "123",
				},
			},
			want: PRPublisherConfig{
				BranchName: "custom/branch",
				Title:      "Custom Title",
				IssueID:    "123",
			},
		},
		{
			name: "with goal issue_id",
			manifest: map[string]interface{}{
				"goal": map[string]interface{}{
					"issue_id": "456",
				},
			},
			want: PRPublisherConfig{
				IssueID: "456",
			},
		},
		{
			name: "metadata overrides goal",
			manifest: map[string]interface{}{
				"metadata": map[string]interface{}{
					"issue_id": "789",
				},
				"goal": map[string]interface{}{
					"issue_id": "456",
				},
			},
			want: PRPublisherConfig{
				IssueID: "789",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := p.buildConfig(tt.manifest)
			if got.BranchName != tt.want.BranchName {
				t.Errorf("buildConfig() BranchName = %v, want %v", got.BranchName, tt.want.BranchName)
			}
			if got.Title != tt.want.Title {
				t.Errorf("buildConfig() Title = %v, want %v", got.Title, tt.want.Title)
			}
			if got.IssueID != tt.want.IssueID {
				t.Errorf("buildConfig() IssueID = %v, want %v", got.IssueID, tt.want.IssueID)
			}
		})
	}
}

func TestTokenEnvVars(t *testing.T) {
	// Test that both environment variable names are documented
	if TokenEnv != "GITHUB_TOKEN" {
		t.Errorf("TokenEnv = %v, want %v", TokenEnv, "GITHUB_TOKEN")
	}
	if HolonTokenEnv != "HOLON_GITHUB_TOKEN" {
		t.Errorf("HolonTokenEnv = %v, want %v", HolonTokenEnv, "HOLON_GITHUB_TOKEN")
	}
}

func TestPublishWithoutToken(t *testing.T) {
	// Ensure no token is set in environment
	os.Unsetenv("GITHUB_TOKEN")
	os.Unsetenv("HOLON_GITHUB_TOKEN")

	// Create a temporary git repository for testing
	tempDir := t.TempDir()

	// Initialize git repository
	if err := exec.Command("git", "init", tempDir).Run(); err != nil {
		t.Fatalf("Failed to init git repo: %v", err)
	}

	// Configure git user
	if err := exec.Command("git", "-C", tempDir, "config", "user.email", "test@example.com").Run(); err != nil {
		t.Fatalf("Failed to configure git email: %v", err)
	}
	if err := exec.Command("git", "-C", tempDir, "config", "user.name", "Test User").Run(); err != nil {
		t.Fatalf("Failed to configure git name: %v", err)
	}

	// Create initial commit
	testFile := filepath.Join(tempDir, "README.md")
	if err := os.WriteFile(testFile, []byte("# Test"), 0o644); err != nil {
		t.Fatalf("Failed to write test file: %v", err)
	}
	if err := exec.Command("git", "-C", tempDir, "add", ".").Run(); err != nil {
		t.Fatalf("Failed to add files: %v", err)
	}
	if err := exec.Command("git", "-C", tempDir, "commit", "-m", "Initial commit").Run(); err != nil {
		t.Fatalf("Failed to commit: %v", err)
	}

	// Create output directory for artifacts
	outDir := filepath.Join(tempDir, "output")
	if err := os.Mkdir(outDir, 0o755); err != nil {
		t.Fatalf("Failed to create output dir: %v", err)
	}

	// Create test artifact files
	diffFile := filepath.Join(outDir, "diff.patch")
	summaryFile := filepath.Join(outDir, "summary.md")

	if err := os.WriteFile(diffFile, []byte("diff --git a/test.txt b/test.txt\nindex 123..456 100644\n--- a/test.txt\n+++ b/test.txt\n@@ -1 +1 @@\n-old\n+new"), 0o644); err != nil {
		t.Fatalf("Failed to write diff file: %v", err)
	}
	if err := os.WriteFile(summaryFile, []byte("# Test PR\n\nThis is a test PR."), 0o644); err != nil {
		t.Fatalf("Failed to write summary file: %v", err)
	}

	// Set HOLON_WORKSPACE to our test repository
	t.Setenv("HOLON_WORKSPACE", tempDir)

	p := NewPRPublisher()
	req := publisher.PublishRequest{
		Target: "holon-run/holon",
		Artifacts: map[string]string{
			"diff.patch":  diffFile,
			"summary.md":  summaryFile,
		},
		Manifest: map[string]interface{}{},
	}

	_, err := p.Publish(req)
	if err == nil {
		t.Error("Expected error when token is not set")
	}
	// The error should mention authentication/credential issues, which may include:
	// - GitHub token env vars
	// - git authentication failure
	// - push failure due to missing credentials
	// Note: If gh CLI provides a token, the test may fail at push instead
	if !strings.Contains(err.Error(), "GITHUB_TOKEN") && !strings.Contains(err.Error(), "HOLON_GITHUB_TOKEN") &&
	   !strings.Contains(err.Error(), "authentication") && !strings.Contains(err.Error(), "credentials") &&
	   !strings.Contains(err.Error(), "push") {
		t.Logf("Warning: Error should mention authentication/credential issues, got: %v", err)
	}
}

func TestGenerateDeterministicTitle(t *testing.T) {
	p := NewPRPublisher()

	t.Run("returns empty when title already exists in manifest", func(t *testing.T) {
		inDir := t.TempDir()
		manifest := map[string]interface{}{
			"metadata": map[string]interface{}{
				"title": "Existing Title",
			},
		}

		req := publisher.PublishRequest{
			InputDir: inDir,
			Manifest: manifest,
		}
		title, err := p.generateDeterministicTitle(req)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if title != "" {
			t.Errorf("expected empty title when already set, got %q", title)
		}
	})

	t.Run("returns empty when InputDir is not set", func(t *testing.T) {
		manifest := map[string]interface{}{}

		req := publisher.PublishRequest{
			InputDir: "", // Empty InputDir
			Manifest: manifest,
		}
		title, err := p.generateDeterministicTitle(req)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if title != "" {
			t.Errorf("expected empty title when InputDir is empty, got %q", title)
		}
	})

	t.Run("returns empty when no context manifest exists", func(t *testing.T) {
		inDir := t.TempDir()
		manifest := map[string]interface{}{}

		req := publisher.PublishRequest{
			InputDir: inDir,
			Manifest: manifest,
		}
		title, err := p.generateDeterministicTitle(req)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if title != "" {
			t.Errorf("expected empty title without context, got %q", title)
		}
	})

	t.Run("generates issue title from context", func(t *testing.T) {
		inDir := t.TempDir()
		contextDir := filepath.Join(inDir, "context", "github")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		// Write context manifest
		contextManifest := `{
  "provider": "github",
  "kind": "issue",
  "ref": "holon-run/holon#123",
  "owner": "holon-run",
  "repo": "holon",
  "number": 123
}`
		if err := os.WriteFile(filepath.Join(inDir, "context", "manifest.json"), []byte(contextManifest), 0o644); err != nil {
			t.Fatalf("failed to write context manifest: %v", err)
		}

		// Write issue.json
		issueData := `{
  "number": 123,
  "title": "Add deterministic PR titles"
}`
		if err := os.WriteFile(filepath.Join(contextDir, "issue.json"), []byte(issueData), 0o644); err != nil {
			t.Fatalf("failed to write issue.json: %v", err)
		}

		manifest := map[string]interface{}{}
		req := publisher.PublishRequest{
			InputDir: inDir,
			Manifest: manifest,
		}
		title, err := p.generateDeterministicTitle(req)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		expected := "Fix: Add deterministic PR titles"
		if title != expected {
			t.Errorf("expected title %q, got %q", expected, title)
		}
	})

	t.Run("generates PR fix title from context", func(t *testing.T) {
		inDir := t.TempDir()
		contextDir := filepath.Join(inDir, "context", "github")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		// Write context manifest
		contextManifest := `{
  "provider": "github",
  "kind": "pr",
  "ref": "holon-run/holon#45",
  "owner": "holon-run",
  "repo": "holon",
  "number": 45
}`
		if err := os.WriteFile(filepath.Join(inDir, "context", "manifest.json"), []byte(contextManifest), 0o644); err != nil {
			t.Fatalf("failed to write context manifest: %v", err)
		}

		// Write pr.json
		prData := `{
  "number": 45,
  "title": "Refactor title generation"
}`
		if err := os.WriteFile(filepath.Join(contextDir, "pr.json"), []byte(prData), 0o644); err != nil {
			t.Fatalf("failed to write pr.json: %v", err)
		}

		manifest := map[string]interface{}{}
		req := publisher.PublishRequest{
			InputDir: inDir,
			Manifest: manifest,
		}
		title, err := p.generateDeterministicTitle(req)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		expected := "Address review comments on #45: Refactor title generation"
		if title != expected {
			t.Errorf("expected title %q, got %q", expected, title)
		}
	})

	t.Run("returns empty for non-GitHub provider", func(t *testing.T) {
		inDir := t.TempDir()
		contextDir := filepath.Join(inDir, "context")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		// Write context manifest for non-GitHub provider
		contextManifest := `{
  "provider": "gitlab",
  "kind": "issue",
  "ref": "123",
  "owner": "owner",
  "repo": "repo",
  "number": 123
}`
		if err := os.WriteFile(filepath.Join(contextDir, "manifest.json"), []byte(contextManifest), 0o644); err != nil {
			t.Fatalf("failed to write context manifest: %v", err)
		}

		manifest := map[string]interface{}{}
		req := publisher.PublishRequest{
			InputDir: inDir,
			Manifest: manifest,
		}
		title, err := p.generateDeterministicTitle(req)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if title != "" {
			t.Errorf("expected empty title for non-GitHub provider, got %q", title)
		}
	})

	t.Run("handles missing issue.json gracefully", func(t *testing.T) {
		inDir := t.TempDir()
		contextDir := filepath.Join(inDir, "context")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		// Write context manifest without issue.json
		contextManifest := `{
  "provider": "github",
  "kind": "issue",
  "ref": "holon-run/holon#123",
  "owner": "holon-run",
  "repo": "holon",
  "number": 123
}`
		if err := os.WriteFile(filepath.Join(contextDir, "manifest.json"), []byte(contextManifest), 0o644); err != nil {
			t.Fatalf("failed to write context manifest: %v", err)
		}

		manifest := map[string]interface{}{}
		req := publisher.PublishRequest{
			InputDir: inDir,
			Manifest: manifest,
		}
		_, err := p.generateDeterministicTitle(req)
		if err == nil {
			t.Error("expected error when issue.json is missing")
		}
	})

	t.Run("handles empty issue title", func(t *testing.T) {
		inDir := t.TempDir()
		contextDir := filepath.Join(inDir, "context", "github")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		// Write context manifest
		contextManifest := `{
  "provider": "github",
  "kind": "issue",
  "ref": "holon-run/holon#123",
  "owner": "holon-run",
  "repo": "holon",
  "number": 123
}`
		if err := os.WriteFile(filepath.Join(inDir, "context", "manifest.json"), []byte(contextManifest), 0o644); err != nil {
			t.Fatalf("failed to write context manifest: %v", err)
		}

		// Write issue.json with empty title
		issueData := `{
  "number": 123,
  "title": ""
}`
		if err := os.WriteFile(filepath.Join(contextDir, "issue.json"), []byte(issueData), 0o644); err != nil {
			t.Fatalf("failed to write issue.json: %v", err)
		}

		manifest := map[string]interface{}{}
		req := publisher.PublishRequest{
			InputDir: inDir,
			Manifest: manifest,
		}
		_, err := p.generateDeterministicTitle(req)
		if err == nil {
			t.Error("expected error when issue title is empty")
		}
	})
}

func TestGenerateIssueTitle(t *testing.T) {
	p := NewPRPublisher()

	t.Run("generates correct title format", func(t *testing.T) {
		outDir := t.TempDir()
		contextDir := filepath.Join(outDir, "context", "github")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		issueData := `{
  "number": 42,
  "title": "Test issue title"
}`
		if err := os.WriteFile(filepath.Join(contextDir, "issue.json"), []byte(issueData), 0o644); err != nil {
			t.Fatalf("failed to write issue.json: %v", err)
		}

		// Pass the context directory, not outDir
		title, err := p.generateIssueTitle(filepath.Join(outDir, "context"))
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		expected := "Fix: Test issue title"
		if title != expected {
			t.Errorf("expected %q, got %q", expected, title)
		}
	})
}

func TestGeneratePRFixTitle(t *testing.T) {
	p := NewPRPublisher()

	t.Run("generates correct title format", func(t *testing.T) {
		outDir := t.TempDir()
		contextDir := filepath.Join(outDir, "context", "github")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		prData := `{
  "number": 78,
  "title": "Original PR title"
}`
		if err := os.WriteFile(filepath.Join(contextDir, "pr.json"), []byte(prData), 0o644); err != nil {
			t.Fatalf("failed to write pr.json: %v", err)
		}

		// Pass the context directory, not outDir
		title, err := p.generatePRFixTitle(filepath.Join(outDir, "context"))
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		expected := "Address review comments on #78: Original PR title"
		if title != expected {
			t.Errorf("expected %q, got %q", expected, title)
		}
	})

	t.Run("handles empty PR title", func(t *testing.T) {
		outDir := t.TempDir()
		contextDir := filepath.Join(outDir, "context", "github")
		if err := os.MkdirAll(contextDir, 0o755); err != nil {
			t.Fatalf("failed to create context dir: %v", err)
		}

		prData := `{
  "number": 78,
  "title": ""
}`
		if err := os.WriteFile(filepath.Join(contextDir, "pr.json"), []byte(prData), 0o644); err != nil {
			t.Fatalf("failed to write pr.json: %v", err)
		}

		// Pass the context directory, not outDir
		title, err := p.generatePRFixTitle(filepath.Join(outDir, "context"))
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		expected := "Address review comments on #78: pull request"
		if title != expected {
			t.Errorf("expected %q, got %q", expected, title)
		}
	})
}

// TestGitClient_CommitChangesWithoutPreconfiguredGit tests that CommitChanges
// properly auto-configures git when no configuration exists. This simulates
// a fresh clone scenario like GitHub Actions where no git config is set initially.
// This was the bug scenario from issue #383.
func TestGitClient_CommitChangesWithoutPreconfiguredGit(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test")
	}

	ctx := context.Background()
	tmpDir := t.TempDir()

	// Initialize git WITHOUT configuring user.name/email
	// This simulates a fresh clone scenario (like GitHub Actions)
	if err := exec.Command("git", "init", tmpDir).Run(); err != nil {
		t.Fatalf("Failed to init git repo: %v", err)
	}

	// Create initial commit (without git config, this will use global config or fail)
	testFile := filepath.Join(tmpDir, "README.md")
	if err := os.WriteFile(testFile, []byte("# Test"), 0o644); err != nil {
		t.Fatalf("Failed to write test file: %v", err)
	}

	// Configure git minimally for initial commit (using global or setting values)
	// In CI, there might be no global config either
	if err := exec.Command("git", "-C", tmpDir, "config", "user.name", "Initial User").Run(); err != nil {
		t.Fatalf("Failed to configure git name for initial commit: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "config", "user.email", "initial@example.com").Run(); err != nil {
		t.Fatalf("Failed to configure git email for initial commit: %v", err)
	}

	if err := exec.Command("git", "-C", tmpDir, "add", ".").Run(); err != nil {
		t.Fatalf("Failed to add files: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "commit", "-m", "Initial commit").Run(); err != nil {
		t.Fatalf("Failed to commit: %v", err)
	}

	// Now REMOVE the git config to simulate a fresh clone where git config is not set
	// This is the key scenario from the bug
	if err := exec.Command("git", "-C", tmpDir, "config", "--unset", "user.name").Run(); err != nil {
		t.Logf("warning: failed to unset git user.name: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "config", "--unset", "user.email").Run(); err != nil {
		t.Logf("warning: failed to unset git user.email: %v", err)
	}

	// Verify git config is not set
	cmd := exec.Command("git", "-C", tmpDir, "config", "--local", "--get", "user.name")
	if output, _ := cmd.Output(); len(output) > 0 {
		t.Logf("Warning: git config still has user.name after unset: %s", string(output))
	}

	// Create GitClient without author info (should use defaults)
	gitClient := NewGitClient(tmpDir, "", "", "")

	// Make a change to commit
	if err := os.WriteFile(testFile, []byte("# Test\n\nNew content"), 0o644); err != nil {
		t.Fatalf("Failed to modify test file: %v", err)
	}

	// This should succeed by auto-configuring git during CommitChanges
	// Before fix PR #382: would fail with "Committer identity unknown"
	// After fix: should succeed with default "Holon Bot <bot@holon.run>"
	sha, err := gitClient.CommitChanges(ctx, "test commit from GitClient")
	if err != nil {
		t.Fatalf("CommitChanges() failed without pre-configured git: %v", err)
	}
	if sha == "" {
		t.Error("CommitChanges() returned empty SHA")
	}

	// Verify git config was set after CommitChanges
	client := holonGit.NewClient(tmpDir)
	name, err := client.ConfigGet(ctx, "user.name")
	if err != nil {
		t.Fatalf("ConfigGet user.name failed: %v", err)
	}
	if name != "Holon Bot" {
		t.Errorf("user.name = %q, want 'Holon Bot'", name)
	}

	email, err := client.ConfigGet(ctx, "user.email")
	if err != nil {
		t.Fatalf("ConfigGet user.email failed: %v", err)
	}
	if email != "bot@holon.run" {
		t.Errorf("user.email = %q, want 'bot@holon.run'", email)
	}
}

// TestGitClient_CommitChanges_PreservesExistingConfig tests that CommitChanges
// preserves existing git configuration instead of overwriting it.
func TestGitClient_CommitChanges_PreservesExistingConfig(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test")
	}

	ctx := context.Background()
	tmpDir := t.TempDir()

	// Initialize git with custom configuration
	if err := exec.Command("git", "init", tmpDir).Run(); err != nil {
		t.Fatalf("Failed to init git repo: %v", err)
	}

	// Set custom git config
	if err := exec.Command("git", "-C", tmpDir, "config", "user.name", "Custom User").Run(); err != nil {
		t.Fatalf("Failed to configure git name: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "config", "user.email", "custom@example.com").Run(); err != nil {
		t.Fatalf("Failed to configure git email: %v", err)
	}

	// Create initial commit
	testFile := filepath.Join(tmpDir, "README.md")
	if err := os.WriteFile(testFile, []byte("# Test"), 0o644); err != nil {
		t.Fatalf("Failed to write test file: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "add", ".").Run(); err != nil {
		t.Fatalf("Failed to add files: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "commit", "-m", "Initial commit").Run(); err != nil {
		t.Fatalf("Failed to commit: %v", err)
	}

	// Create GitClient without author info
	gitClient := NewGitClient(tmpDir, "", "", "")

	// Make a change to commit
	if err := os.WriteFile(testFile, []byte("# Test\n\nNew content"), 0o644); err != nil {
		t.Fatalf("Failed to modify test file: %v", err)
	}

	// CommitChanges should preserve existing config
	sha, err := gitClient.CommitChanges(ctx, "test commit")
	if err != nil {
		t.Fatalf("CommitChanges() failed: %v", err)
	}
	if sha == "" {
		t.Error("CommitChanges() returned empty SHA")
	}

	// Verify existing config was preserved
	client := holonGit.NewClient(tmpDir)
	name, err := client.ConfigGet(ctx, "user.name")
	if err != nil {
		t.Fatalf("ConfigGet user.name failed: %v", err)
	}
	if name != "Custom User" {
		t.Errorf("user.name = %q, want 'Custom User' (existing config should be preserved)", name)
	}

	email, err := client.ConfigGet(ctx, "user.email")
	if err != nil {
		t.Fatalf("ConfigGet user.email failed: %v", err)
	}
	if email != "custom@example.com" {
		t.Errorf("user.email = %q, want 'custom@example.com' (existing config should be preserved)", email)
	}
}

// TestGitClient_CommitChanges_WithCustomAuthor tests that CommitChanges
// uses custom author info when provided.
func TestGitClient_CommitChanges_WithCustomAuthor(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test")
	}

	ctx := context.Background()
	tmpDir := t.TempDir()

	// Initialize git without configuration
	if err := exec.Command("git", "init", tmpDir).Run(); err != nil {
		t.Fatalf("Failed to init git repo: %v", err)
	}

	// Create initial commit (with temp config)
	if err := exec.Command("git", "-C", tmpDir, "config", "user.name", "Initial").Run(); err != nil {
		t.Fatalf("Failed to configure git: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "config", "user.email", "initial@example.com").Run(); err != nil {
		t.Fatalf("Failed to configure git: %v", err)
	}

	testFile := filepath.Join(tmpDir, "README.md")
	if err := os.WriteFile(testFile, []byte("# Test"), 0o644); err != nil {
		t.Fatalf("Failed to write test file: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "add", ".").Run(); err != nil {
		t.Fatalf("Failed to add files: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "commit", "-m", "Initial").Run(); err != nil {
		t.Fatalf("Failed to commit: %v", err)
	}

	// Remove config to simulate fresh clone
	if err := exec.Command("git", "-C", tmpDir, "config", "--unset", "user.name").Run(); err != nil {
		t.Logf("Warning: failed to unset git config user.name: %v", err)
	}
	if err := exec.Command("git", "-C", tmpDir, "config", "--unset", "user.email").Run(); err != nil {
		t.Logf("Warning: failed to unset git config user.email: %v", err)
	}

	// Create GitClient with custom author info
	gitClient := NewGitClient(tmpDir, "", "My Author", "myauthor@example.com")

	// Make a change
	if err := os.WriteFile(testFile, []byte("# Test\n\nNew content"), 0o644); err != nil {
		t.Fatalf("Failed to modify test file: %v", err)
	}

	// CommitChanges should use custom author
	sha, err := gitClient.CommitChanges(ctx, "test commit with custom author")
	if err != nil {
		t.Fatalf("CommitChanges() failed: %v", err)
	}
	if sha == "" {
		t.Error("CommitChanges() returned empty SHA")
	}

	// Verify git config was set with custom values
	client := holonGit.NewClient(tmpDir)
	name, err := client.ConfigGet(ctx, "user.name")
	if err != nil {
		t.Fatalf("ConfigGet user.name failed: %v", err)
	}
	if name != "My Author" {
		t.Errorf("user.name = %q, want 'My Author'", name)
	}

	email, err := client.ConfigGet(ctx, "user.email")
	if err != nil {
		t.Fatalf("ConfigGet user.email failed: %v", err)
	}
	if email != "myauthor@example.com" {
		t.Errorf("user.email = %q, want 'myauthor@example.com'", email)
	}
}
