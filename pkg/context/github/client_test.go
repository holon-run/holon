package github

import (
	"testing"
)

func TestParseRepoFromURL(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		wantOwner string
		wantRepo  string
		wantErr bool
	}{
		{
			name:      "full https URL",
			input:     "https://github.com/holon-run/holon",
			wantOwner: "holon-run",
			wantRepo:  "holon",
			wantErr:   false,
		},
		{
			name:      "http URL",
			input:     "http://github.com/holon-run/holon",
			wantOwner: "holon-run",
			wantRepo:  "holon",
			wantErr:   false,
		},
		{
			name:      "github.com path",
			input:     "github.com/holon-run/holon",
			wantOwner: "holon-run",
			wantRepo:  "holon",
			wantErr:   false,
		},
		{
			name:      "owner/repo format",
			input:     "holon-run/holon",
			wantOwner: "holon-run",
			wantRepo:  "holon",
			wantErr:   false,
		},
		{
			name:      "with .git suffix",
			input:     "https://github.com/holon-run/holon.git",
			wantOwner: "holon-run",
			wantRepo:  "holon",
			wantErr:   false,
		},
		{
			name:      "with trailing slash",
			input:     "https://github.com/holon-run/holon/",
			wantOwner: "holon-run",
			wantRepo:  "holon",
			wantErr:   false,
		},
		{
			name:    "invalid format",
			input:   "invalid",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			owner, repo, err := ParseRepoFromURL(tt.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParseRepoFromURL() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if owner != tt.wantOwner {
					t.Errorf("ParseRepoFromURL() owner = %v, want %v", owner, tt.wantOwner)
				}
				if repo != tt.wantRepo {
					t.Errorf("ParseRepoFromURL() repo = %v, want %v", repo, tt.wantRepo)
				}
			}
		})
	}
}

func TestParsePRNumber(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		want    int
		wantErr bool
	}{
		{
			name:    "plain number",
			input:   "123",
			want:    123,
			wantErr: false,
		},
		{
			name:    "number with hash",
			input:   "#456",
			want:    456,
			wantErr: false,
		},
		{
			name:    "full GitHub URL",
			input:   "https://github.com/holon-run/holon/pull/789",
			want:    789,
			wantErr: false,
		},
		{
			name:    "invalid format",
			input:   "not-a-number",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParsePRNumber(tt.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParsePRNumber() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr && got != tt.want {
				t.Errorf("ParsePRNumber() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestGroupCommentsIntoThreads(t *testing.T) {
	client := NewClient("")

	// Test case: simple thread with one top-level comment and one reply
	comments := []map[string]interface{}{
		{
			"id":       float64(1),
			"body":     "Top-level comment",
			"path":     "file.go",
			"line":     float64(10),
			"diff_hunk": "@@ -1,1 +1,1 @@",
			"html_url": "https://github.com/owner/repo/pull/1#discussion_r1",
			"user": map[string]interface{}{
				"login": "user1",
			},
			"created_at": "2023-01-01T00:00:00Z",
			"updated_at": "2023-01-01T00:00:00Z",
		},
		{
			"id":             float64(2),
			"body":           "Reply to comment",
			"in_reply_to_id": float64(1),
			"html_url":       "https://github.com/owner/repo/pull/1#discussion_r2",
			"user": map[string]interface{}{
				"login": "user2",
			},
			"created_at": "2023-01-01T01:00:00Z",
			"updated_at": "2023-01-01T01:00:00Z",
		},
	}

	threads := client.groupCommentsIntoThreads(comments)

	if len(threads) != 1 {
		t.Fatalf("Expected 1 thread, got %d", len(threads))
	}

	thread := threads[0]
	if thread.CommentID != 1 {
		t.Errorf("Expected thread ID 1, got %d", thread.CommentID)
	}
	if thread.Body != "Top-level comment" {
		t.Errorf("Expected body 'Top-level comment', got '%s'", thread.Body)
	}
	if len(thread.Replies) != 1 {
		t.Fatalf("Expected 1 reply, got %d", len(thread.Replies))
	}
	if thread.Replies[0].CommentID != 2 {
		t.Errorf("Expected reply ID 2, got %d", thread.Replies[0].CommentID)
	}
	if thread.Replies[0].Body != "Reply to comment" {
		t.Errorf("Expected reply body 'Reply to comment', got '%s'", thread.Replies[0].Body)
	}
}
