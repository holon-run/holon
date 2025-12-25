package github

import (
	"testing"

	"github.com/holon-run/holon/pkg/publisher"
)

func TestGitHubPublisher_Name(t *testing.T) {
	p := NewGitHubPublisher()
	if got := p.Name(); got != "github" {
		t.Errorf("GitHubPublisher.Name() = %v, want %v", got, "github")
	}
}

func TestGitHubPublisher_Validate(t *testing.T) {
	p := NewGitHubPublisher()

	tests := []struct {
		name    string
		target  string
		wantErr bool
	}{
		{
			name:    "valid target - owner/repo/pr/123",
			target:  "holon-run/holon/pr/123",
			wantErr: false,
		},
		{
			name:    "valid target - owner/repo#123",
			target:  "holon-run/holon#123",
			wantErr: false,
		},
		{
			name:    "invalid target - missing PR number",
			target:  "holon-run/holon/pr",
			wantErr: true,
		},
		{
			name:    "invalid target - no separator",
			target:  "holon-run-holon-123",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := publisher.PublishRequest{
				Target: tt.target,
			}
			err := p.Validate(req)
			if (err != nil) != tt.wantErr {
				t.Errorf("GitHubPublisher.Validate() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

func TestFormatReviewReply(t *testing.T) {
	tests := []struct {
		name     string
		reply    ReviewReply
		contains []string
	}{
		{
			name: "fixed status",
			reply: ReviewReply{
				CommentID: 123,
				Status:    "fixed",
				Message:   "Fixed the bug",
			},
			contains: []string{"✅", "FIXED", "Fixed the bug"},
		},
		{
			name: "wontfix status",
			reply: ReviewReply{
				CommentID: 124,
				Status:    "wontfix",
				Message:   "Not applicable",
			},
			contains: []string{"⚠️", "WONTFIX", "Not applicable"},
		},
		{
			name: "need-info status",
			reply: ReviewReply{
				CommentID: 125,
				Status:    "need-info",
				Message:   "Please clarify",
			},
			contains: []string{"❓", "NEED-INFO", "Please clarify"},
		},
		{
			name: "fixed with action taken",
			reply: ReviewReply{
				CommentID: 126,
				Status:    "fixed",
				Message:   "Fixed the issue",
				ActionTaken: func() *string {
					s := "Updated the code"
					return &s
				}(),
			},
			contains: []string{"✅", "FIXED", "Fixed the issue", "Action taken", "Updated the code"},
		},
		{
			name: "unknown status",
			reply: ReviewReply{
				CommentID: 127,
				Status:    "unknown",
				Message:   "Some status",
			},
			contains: []string{"📝", "UNKNOWN", "Some status"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := formatReviewReply(tt.reply)
			for _, substr := range tt.contains {
				if !contains(got, substr) {
					t.Errorf("formatReviewReply() = %v, does not contain %v", got, substr)
				}
			}
		})
	}
}

func TestGetBotLogin(t *testing.T) {
	// Test default bot login
	t.Setenv(BotLoginEnv, "")
	if got := getBotLogin(); got != DefaultBotLogin {
		t.Errorf("getBotLogin() = %v, want %v", got, DefaultBotLogin)
	}

	// Test custom bot login
	customLogin := "mybot[bot]"
	t.Setenv(BotLoginEnv, customLogin)
	if got := getBotLogin(); got != customLogin {
		t.Errorf("getBotLogin() = %v, want %v", got, customLogin)
	}
}

// contains checks if a string contains a substring
func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(s) > len(substr) && containsHelper(s, substr))
}

func containsHelper(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
