package issue

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestCollector_Collect(t *testing.T) {
	// This test requires a real GitHub token to run against the API.
	// It's marked as a manual test to avoid running in CI without credentials.
	t.Skip("Skipping integration test - requires GITHUB_TOKEN")

	ctx := context.Background()

	// Create a temporary output directory
	tmpDir, err := os.MkdirTemp("", "issue-collector-test-*")
	if err != nil {
		t.Fatalf("Failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

	// Use a known issue for testing
	collector := NewCollector(CollectorConfig{
		Owner:     "holon-run",
		Repo:      "holon",
		IssueNum:  169,
		Token:     os.Getenv("GITHUB_TOKEN"),
		OutputDir: tmpDir,
	})

	if collector.config.Token == "" {
		t.Skip("Skipping test - GITHUB_TOKEN not set")
	}

	// Run collection
	err = collector.Collect(ctx)
	if err != nil {
		t.Fatalf("Collect() failed: %v", err)
	}

	// Verify output files exist
	githubDir := filepath.Join(tmpDir, "github")
	issuePath := filepath.Join(githubDir, "issue.json")
	commentsPath := filepath.Join(githubDir, "issue_comments.json")

	if _, err := os.Stat(issuePath); os.IsNotExist(err) {
		t.Errorf("issue.json was not created")
	}

	if _, err := os.Stat(commentsPath); os.IsNotExist(err) {
		t.Errorf("issue_comments.json was not created")
	}

	// Verify issue.json structure
	issueData, err := os.ReadFile(issuePath)
	if err != nil {
		t.Fatalf("Failed to read issue.json: %v", err)
	}

	var issueInfo map[string]interface{}
	if err := json.Unmarshal(issueData, &issueInfo); err != nil {
		t.Fatalf("Failed to parse issue.json: %v", err)
	}

	// Check required fields
	if _, ok := issueInfo["number"]; !ok {
		t.Error("issue.json missing 'number' field")
	}
	if _, ok := issueInfo["title"]; !ok {
		t.Error("issue.json missing 'title' field")
	}
	if _, ok := issueInfo["body"]; !ok {
		t.Error("issue.json missing 'body' field")
	}

	// Verify issue_comments.json structure
	commentsData, err := os.ReadFile(commentsPath)
	if err != nil {
		t.Fatalf("Failed to read issue_comments.json: %v", err)
	}

	var comments []interface{}
	if err := json.Unmarshal(commentsData, &comments); err != nil {
		t.Fatalf("Failed to parse issue_comments.json: %v", err)
	}

	// Comments should be an array (may be empty)
	if comments == nil {
		t.Error("issue_comments.json should be an array")
	}
}

func TestNewCollector(t *testing.T) {
	config := CollectorConfig{
		Owner:     "test-owner",
		Repo:      "test-repo",
		IssueNum:  123,
		Token:     "test-token",
		OutputDir: "/tmp/test",
	}

	collector := NewCollector(config)

	if collector == nil {
		t.Fatal("NewCollector() returned nil")
	}

	if collector.config.Owner != config.Owner {
		t.Errorf("Owner = %v, want %v", collector.config.Owner, config.Owner)
	}

	if collector.config.Repo != config.Repo {
		t.Errorf("Repo = %v, want %v", collector.config.Repo, config.Repo)
	}

	if collector.config.IssueNum != config.IssueNum {
		t.Errorf("IssueNum = %v, want %v", collector.config.IssueNum, config.IssueNum)
	}

	if collector.config.Token != config.Token {
		t.Errorf("Token = %v, want %v", collector.config.Token, config.Token)
	}

	if collector.config.OutputDir != config.OutputDir {
		t.Errorf("OutputDir = %v, want %v", collector.config.OutputDir, config.OutputDir)
	}
}
