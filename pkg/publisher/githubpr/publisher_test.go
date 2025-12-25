package githubpr

import (
	"os"
	"strings"
	"testing"

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
					"branch": "custom/branch",
					"title":  "Custom Title",
					"issue":  "123",
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
					"issue": "789",
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
	if LegacyTokenEnv != "HOLON_GITHUB_TOKEN" {
		t.Errorf("LegacyTokenEnv = %v, want %v", LegacyTokenEnv, "HOLON_GITHUB_TOKEN")
	}
}

func TestPublishWithoutToken(t *testing.T) {
	// Ensure no token is set in environment
	os.Unsetenv("GITHUB_TOKEN")
	os.Unsetenv("HOLON_GITHUB_TOKEN")

	p := NewPRPublisher()
	req := publisher.PublishRequest{
		Target: "holon-run/holon",
		Artifacts: map[string]string{
			"diff.patch": "/tmp/diff.patch",
			"summary.md": "/tmp/summary.md",
		},
	}

	_, err := p.Publish(req)
	if err == nil {
		t.Error("Expected error when token is not set")
	}
	if !strings.Contains(err.Error(), "GITHUB_TOKEN") && !strings.Contains(err.Error(), "HOLON_GITHUB_TOKEN") {
		t.Errorf("Error should mention required token env vars, got: %v", err)
	}
}
