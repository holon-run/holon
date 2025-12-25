package git

import (
	"os"
	"strings"
	"testing"

	"github.com/holon-run/holon/pkg/publisher"
)

func TestPublisherName(t *testing.T) {
	p := NewPublisher()
	if got := p.Name(); got != "git" {
		t.Errorf("Publisher.Name() = %v, want %v", got, "git")
	}
}

func TestPublisherValidate(t *testing.T) {
	// Create a temporary git repository for testing
	tmpDir := t.TempDir()

	// Initialize a git repository
	if err := initGitRepo(tmpDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	// Set workspace environment variable
	oldWorkspace := os.Getenv(WorkspaceEnv)
	t.Cleanup(func() {
		os.Setenv(WorkspaceEnv, oldWorkspace)
	})
	os.Setenv(WorkspaceEnv, tmpDir)

	tests := []struct {
		name    string
		req     publisher.PublishRequest
		wantErr bool
		errMsg  string
	}{
		{
			name: "valid request with diff.patch",
			req: publisher.PublishRequest{
				Target: "origin/main",
				Artifacts: map[string]string{
					"diff.patch": tmpDir + "/diff.patch",
				},
			},
			wantErr: false,
		},
		{
			name: "valid request without diff.patch",
			req: publisher.PublishRequest{
				Target:    "origin/main",
				Artifacts: map[string]string{},
			},
			wantErr: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			p := NewPublisher()
			err := p.Validate(tt.req)
			if (err != nil) != tt.wantErr {
				t.Errorf("Publisher.Validate() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if tt.wantErr && tt.errMsg != "" {
				if err == nil {
					t.Errorf("Publisher.Validate() expected error containing %q, got nil", tt.errMsg)
				} else if !strings.Contains(err.Error(), tt.errMsg) {
					t.Errorf("Publisher.Validate() error = %q, want error containing %q", err.Error(), tt.errMsg)
				}
			}
		})
	}
}

func TestPublisherBuildConfig(t *testing.T) {
	p := NewPublisher()

	tests := []struct {
		name     string
		manifest map[string]interface{}
		want     GitPublisherConfig
	}{
		{
			name:     "empty manifest",
			manifest: map[string]interface{}{},
			want:     GitPublisherConfig{},
		},
		{
			name: "with metadata",
			manifest: map[string]interface{}{
				"metadata": map[string]interface{}{
					"branch":         "custom/branch",
					"commit_message": "Custom commit message",
					"remote":         "upstream",
					"commit":         true,
					"push":           true,
				},
			},
			want: GitPublisherConfig{
				Branch:        "custom/branch",
				CommitMessage: "Custom commit message",
				Remote:        "upstream",
				Commit:        true,
				Push:          true,
			},
		},
		{
			name: "partial metadata",
			manifest: map[string]interface{}{
				"metadata": map[string]interface{}{
					"branch": "feature/test",
					"commit": true,
				},
			},
			want: GitPublisherConfig{
				Branch: "feature/test",
				Commit: true,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := p.buildConfig(tt.manifest)
			if got.Branch != tt.want.Branch {
				t.Errorf("buildConfig() Branch = %v, want %v", got.Branch, tt.want.Branch)
			}
			if got.CommitMessage != tt.want.CommitMessage {
				t.Errorf("buildConfig() CommitMessage = %v, want %v", got.CommitMessage, tt.want.CommitMessage)
			}
			if got.Remote != tt.want.Remote {
				t.Errorf("buildConfig() Remote = %v, want %v", got.Remote, tt.want.Remote)
			}
			if got.Commit != tt.want.Commit {
				t.Errorf("buildConfig() Commit = %v, want %v", got.Commit, tt.want.Commit)
			}
			if got.Push != tt.want.Push {
				t.Errorf("buildConfig() Push = %v, want %v", got.Push, tt.want.Push)
			}
		})
	}
}

func TestApplyPatchNoOp(t *testing.T) {
	tmpDir := t.TempDir()
	client := NewGitClient(tmpDir, "")

	// Test with non-existent patch file
	applied, err := client.ApplyPatch(tmpDir + "/nonexistent.patch")
	if err != nil {
		t.Errorf("ApplyPatch() with non-existent file should return no error, got: %v", err)
	}
	if applied {
		t.Error("ApplyPatch() with non-existent file should return applied=false")
	}

	// Test with empty patch file
	emptyPatch := tmpDir + "/empty.patch"
	if err := os.WriteFile(emptyPatch, []byte{}, 0644); err != nil {
		t.Fatalf("failed to create empty patch file: %v", err)
	}

	applied, err = client.ApplyPatch(emptyPatch)
	if err != nil {
		t.Errorf("ApplyPatch() with empty file should return no error, got: %v", err)
	}
	if applied {
		t.Error("ApplyPatch() with empty file should return applied=false")
	}
}

func TestEnvVarNames(t *testing.T) {
	// Test that environment variable names are correctly defined
	if WorkspaceEnv != "HOLON_WORKSPACE" {
		t.Errorf("WorkspaceEnv = %v, want %v", WorkspaceEnv, "HOLON_WORKSPACE")
	}
	if GitTokenEnv != "GIT_TOKEN" {
		t.Errorf("GitTokenEnv = %v, want %v", GitTokenEnv, "GIT_TOKEN")
	}
	if DefaultRemote != "origin" {
		t.Errorf("DefaultRemote = %v, want %v", DefaultRemote, "origin")
	}
	if DefaultCommitMessage != "Apply changes from Holon" {
		t.Errorf("DefaultCommitMessage = %v, want %v", DefaultCommitMessage, "Apply changes from Holon")
	}
}

func TestPublishNoOpWithoutDiff(t *testing.T) {
	tmpDir := t.TempDir()

	// Initialize a git repository
	if err := initGitRepo(tmpDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	// Set workspace environment variable
	oldWorkspace := os.Getenv(WorkspaceEnv)
	t.Cleanup(func() {
		os.Setenv(WorkspaceEnv, oldWorkspace)
	})
	os.Setenv(WorkspaceEnv, tmpDir)

	p := NewPublisher()
	req := publisher.PublishRequest{
		Target:    "origin/main",
		Artifacts: map[string]string{},
	}

	result, err := p.Publish(req)
	if err != nil {
		t.Errorf("Publish() without diff.patch should succeed, got: %v", err)
	}
	if !result.Success {
		t.Errorf("Publish() without diff.patch should return success=true")
	}
	if len(result.Actions) != 1 {
		t.Errorf("Publish() without diff.patch should return 1 action, got %d", len(result.Actions))
	}
	if result.Actions[0].Type != "no_op" {
		t.Errorf("Publish() without diff.patch should return no_op action, got %s", result.Actions[0].Type)
	}
}

func TestPublishNoOpWithEmptyDiff(t *testing.T) {
	tmpDir := t.TempDir()

	// Initialize a git repository
	if err := initGitRepo(tmpDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	// Create empty diff.patch file
	emptyPatch := tmpDir + "/diff.patch"
	if err := os.WriteFile(emptyPatch, []byte{}, 0644); err != nil {
		t.Fatalf("failed to create empty patch file: %v", err)
	}

	// Set workspace environment variable
	oldWorkspace := os.Getenv(WorkspaceEnv)
	t.Cleanup(func() {
		os.Setenv(WorkspaceEnv, oldWorkspace)
	})
	os.Setenv(WorkspaceEnv, tmpDir)

	p := NewPublisher()
	req := publisher.PublishRequest{
		Target: "origin/main",
		Artifacts: map[string]string{
			"diff.patch": emptyPatch,
		},
	}

	result, err := p.Publish(req)
	if err != nil {
		t.Errorf("Publish() with empty diff.patch should succeed, got: %v", err)
	}
	if !result.Success {
		t.Errorf("Publish() with empty diff.patch should return success=true")
	}
	if len(result.Actions) != 1 {
		t.Errorf("Publish() with empty diff.patch should return 1 action, got %d", len(result.Actions))
	}
	if result.Actions[0].Type != "no_op" {
		t.Errorf("Publish() with empty diff.patch should return no_op action, got %s", result.Actions[0].Type)
	}
}

// initGitRepo initializes a git repository in the given directory.
func initGitRepo(dir string) error {
	// Create .git directory
	gitDir := dir + "/.git"
	if err := os.MkdirAll(gitDir+"/objects", 0755); err != nil {
		return err
	}
	if err := os.MkdirAll(gitDir+"/refs/heads", 0755); err != nil {
		return err
	}

	// Create HEAD file
	headFile := gitDir + "/HEAD"
	if err := os.WriteFile(headFile, []byte("ref: refs/heads/main\n"), 0644); err != nil {
		return err
	}

	// Create config file
	configFile := gitDir + "/config"
	configContent := `[core]
	repositoryformatversion = 0
	filemode = true
	bare = false
`
	if err := os.WriteFile(configFile, []byte(configContent), 0644); err != nil {
		return err
	}

	return nil
}
