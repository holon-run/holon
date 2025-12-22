package github

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestWriteContext(t *testing.T) {
	// Create temporary directory for test output
	tmpDir, err := os.MkdirTemp("", "holon-context-test-*")
	if err != nil {
		t.Fatalf("Failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

	// Create test data
	prInfo := &PRInfo{
		Number:     123,
		Title:      "Test PR",
		Body:       "This is a test PR",
		State:      "open",
		URL:        "https://github.com/owner/repo/pull/123",
		BaseRef:    "main",
		HeadRef:    "feature",
		BaseSHA:    "abc123",
		HeadSHA:    "def456",
		Author:     "testuser",
		CreatedAt:  time.Now(),
		UpdatedAt:  time.Now(),
		Repository: "owner/repo",
	}

	reviewThreads := []ReviewThread{
		{
			CommentID: 1,
			URL:       "https://github.com/owner/repo/pull/123#discussion_r1",
			Path:      "file.go",
			Line:      10,
			Side:      "RIGHT",
			DiffHunk:  "@@ -1,1 +1,1 @@\n-old\n+new",
			Body:      "Please fix this",
			Author:    "reviewer",
			CreatedAt: time.Now(),
			UpdatedAt: time.Now(),
			Resolved:  false,
			Replies: []Reply{
				{
					CommentID:   2,
					URL:         "https://github.com/owner/repo/pull/123#discussion_r2",
					Body:        "I'll fix it",
					Author:      "testuser",
					CreatedAt:   time.Now(),
					UpdatedAt:   time.Now(),
					InReplyToID: 1,
				},
			},
		},
	}

	diff := `diff --git a/file.go b/file.go
index abc123..def456 100644
--- a/file.go
+++ b/file.go
@@ -1,1 +1,1 @@
-old
+new`

	// Write context
	err = WriteContext(tmpDir, prInfo, reviewThreads, diff)
	if err != nil {
		t.Fatalf("WriteContext failed: %v", err)
	}

	// Verify files were created
	githubDir := filepath.Join(tmpDir, "github")

	// Check pr.json
	prJSONPath := filepath.Join(githubDir, "pr.json")
	if _, err := os.Stat(prJSONPath); os.IsNotExist(err) {
		t.Errorf("pr.json not created")
	} else {
		data, err := os.ReadFile(prJSONPath)
		if err != nil {
			t.Errorf("Failed to read pr.json: %v", err)
		}
		var pr PRInfo
		if err := json.Unmarshal(data, &pr); err != nil {
			t.Errorf("Failed to parse pr.json: %v", err)
		}
		if pr.Number != 123 {
			t.Errorf("Expected PR number 123, got %d", pr.Number)
		}
	}

	// Check review_threads.json
	threadsJSONPath := filepath.Join(githubDir, "review_threads.json")
	if _, err := os.Stat(threadsJSONPath); os.IsNotExist(err) {
		t.Errorf("review_threads.json not created")
	} else {
		data, err := os.ReadFile(threadsJSONPath)
		if err != nil {
			t.Errorf("Failed to read review_threads.json: %v", err)
		}
		var threads []ReviewThread
		if err := json.Unmarshal(data, &threads); err != nil {
			t.Errorf("Failed to parse review_threads.json: %v", err)
		}
		if len(threads) != 1 {
			t.Errorf("Expected 1 thread, got %d", len(threads))
		}
		if threads[0].CommentID != 1 {
			t.Errorf("Expected thread ID 1, got %d", threads[0].CommentID)
		}
		if len(threads[0].Replies) != 1 {
			t.Errorf("Expected 1 reply, got %d", len(threads[0].Replies))
		}
	}

	// Check pr.diff
	diffPath := filepath.Join(githubDir, "pr.diff")
	if _, err := os.Stat(diffPath); os.IsNotExist(err) {
		t.Errorf("pr.diff not created")
	} else {
		data, err := os.ReadFile(diffPath)
		if err != nil {
			t.Errorf("Failed to read pr.diff: %v", err)
		}
		if string(data) != diff {
			t.Errorf("Diff content mismatch")
		}
	}

	// Check review.md
	reviewMDPath := filepath.Join(githubDir, "review.md")
	if _, err := os.Stat(reviewMDPath); os.IsNotExist(err) {
		t.Errorf("review.md not created")
	} else {
		data, err := os.ReadFile(reviewMDPath)
		if err != nil {
			t.Errorf("Failed to read review.md: %v", err)
		}
		content := string(data)
		if !strings.Contains(content, "Test PR") {
			t.Errorf("review.md should contain PR title")
		}
		if !strings.Contains(content, "Please fix this") {
			t.Errorf("review.md should contain comment body")
		}
		if !strings.Contains(content, "I'll fix it") {
			t.Errorf("review.md should contain reply body")
		}
	}
}

