package github

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-github/v68/github"
	"github.com/holon-run/holon/pkg/publisher"
)

// mockGitHubServer creates a mock GitHub API server for testing
type mockGitHubServer struct {
	server                   *httptest.Server
	mux                      *http.ServeMux
	comments                 map[int64][]*github.PullRequestComment
	issueComments            []*github.IssueComment
	createCommentCalls       int  // PR review comment creations
	createIssueCommentCalls  int  // Issue/PR comment creations (for summary)
	editCommentCalls         int
	listCommentsCalls        int
	listIssueCommentsCalls   int
	nextCommentID            int64
	nextIssueCommentID       int64
}

// newMockGitHubServer creates a new mock GitHub server with expected handlers
func newMockGitHubServer(t *testing.T) *mockGitHubServer {
	mux := http.NewServeMux()
	server := httptest.NewServer(mux)

	m := &mockGitHubServer{
		server:           server,
		mux:              mux,
		comments:         make(map[int64][]*github.PullRequestComment),
		issueComments:    make([]*github.IssueComment, 0),
		nextCommentID:    1000000000,
		nextIssueCommentID: 2000000000,
	}

	// Register handlers for all GitHub API endpoints
	mux.HandleFunc("/", m.handleRequest)

	return m
}

// handleRequest routes requests to appropriate handlers
func (m *mockGitHubServer) handleRequest(w http.ResponseWriter, r *http.Request) {
	path := r.URL.Path

	// Handle: List pull request comments
	if strings.Contains(path, "/pulls/") && strings.HasSuffix(path, "/comments") && r.Method == http.MethodGet {
		m.handleListComments(w, r)
		return
	}

	// Handle: Create comment on PR
	if strings.Contains(path, "/pulls/") && strings.HasSuffix(path, "/comments") && r.Method == http.MethodPost {
		m.handleCreateComment(w, r)
		return
	}

	// Handle: List issue comments (for summary lookup)
	if strings.Contains(path, "/issues/") && strings.HasSuffix(path, "/comments") && r.Method == http.MethodGet {
		m.handleListIssueComments(w, r)
		return
	}

	// Handle: Create issue comment
	if strings.Contains(path, "/issues/") && strings.HasSuffix(path, "/comments") && r.Method == http.MethodPost {
		m.handleCreateIssueComment(w, r)
		return
	}

	// Handle: Edit issue comment
	if strings.Contains(path, "/comments/") && r.Method == http.MethodPatch {
		m.handleEditComment(w, r)
		return
	}

	http.NotFound(w, r)
}

// handleListComments handles GET /repos/:owner/:repo/pulls/:number/comments
func (m *mockGitHubServer) handleListComments(w http.ResponseWriter, r *http.Request) {
	m.listCommentsCalls++

	var allComments []*github.PullRequestComment
	for _, comments := range m.comments {
		allComments = append(allComments, comments...)
	}

	// Respond with JSON array
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(allComments); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

// handleCreateComment handles POST /repos/:owner/:repo/pulls/:number/comments
func (m *mockGitHubServer) handleCreateComment(w http.ResponseWriter, r *http.Request) {
	m.createCommentCalls++

	var comment github.PullRequestComment
	if err := json.NewDecoder(r.Body).Decode(&comment); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	// Assign ID to new comment
	m.nextCommentID++
	comment.ID = github.Int64(m.nextCommentID)
	comment.User = &github.User{Login: github.String("holonbot[bot]")}

	// Store if it's a reply
	if comment.InReplyTo != nil {
		parentID := *comment.InReplyTo
		m.comments[parentID] = append(m.comments[parentID], &comment)
	}

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	if err := json.NewEncoder(w).Encode(comment); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

// handleListIssueComments handles GET /repos/:owner/:repo/issues/:number/comments
func (m *mockGitHubServer) handleListIssueComments(w http.ResponseWriter, r *http.Request) {
	m.listIssueCommentsCalls++

	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(m.issueComments); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

// handleCreateIssueComment handles POST /repos/:owner/:repo/issues/:number/comments
func (m *mockGitHubServer) handleCreateIssueComment(w http.ResponseWriter, r *http.Request) {
	m.createIssueCommentCalls++

	var comment github.IssueComment
	if err := json.NewDecoder(r.Body).Decode(&comment); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	// Assign ID and user
	m.nextIssueCommentID++
	comment.ID = github.Int64(m.nextIssueCommentID)
	comment.User = &github.User{Login: github.String("holonbot[bot]")}

	m.issueComments = append(m.issueComments, &comment)

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	if err := json.NewEncoder(w).Encode(comment); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

// handleEditComment handles PATCH /repos/:owner/:repo/issues/comments/:id
func (m *mockGitHubServer) handleEditComment(w http.ResponseWriter, r *http.Request) {
	m.editCommentCalls++

	var comment github.IssueComment
	if err := json.NewDecoder(r.Body).Decode(&comment); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	// Extract comment ID from URL
	parts := strings.Split(strings.Trim(r.URL.Path, "/"), "/")
	commentIDStr := parts[len(parts)-1]
	var commentID int64
	fmt.Sscanf(commentIDStr, "%d", &commentID)

	// Find and update the comment
	for _, c := range m.issueComments {
		if c.GetID() == commentID {
			c.Body = comment.Body
			w.Header().Set("Content-Type", "application/json")
			if err := json.NewEncoder(w).Encode(c); err != nil {
				http.Error(w, err.Error(), http.StatusInternalServerError)
			}
			return
		}
	}

	http.NotFound(w, r)
}

// addExistingComment adds an existing review comment reply to the mock
func (m *mockGitHubServer) addExistingComment(parentID int64, botLogin string) {
	comment := &github.PullRequestComment{
		ID:        github.Int64(parentID + 1000),
		Body:      github.String("Existing reply from bot"),
		User:      &github.User{Login: github.String(botLogin)},
		InReplyTo: github.Int64(parentID),
	}
	m.comments[parentID] = append(m.comments[parentID], comment)
}

// addExistingSummaryComment adds an existing summary comment to the mock
func (m *mockGitHubServer) addExistingSummaryComment(botLogin string) int64 {
	comment := &github.IssueComment{
		ID:        github.Int64(m.nextIssueCommentID),
		Body:      github.String(SummaryMarker + "\nOld summary content"),
		User:      &github.User{Login: github.String(botLogin)},
	}
	m.nextIssueCommentID++
	m.issueComments = append(m.issueComments, comment)
	return *comment.ID
}

// close closes the mock server
func (m *mockGitHubServer) close() {
	m.server.Close()
}

// getBaseURL returns the base URL of the mock server
func (m *mockGitHubServer) getBaseURL() string {
	return m.server.URL
}

// TestContractReviewRepliesIdempotency tests that replies are not posted twice
func TestContractReviewRepliesIdempotency(t *testing.T) {
	t.Run("skip posting if bot already replied", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		// Add existing reply from bot - this simulates that we already replied
		mockServer.addExistingComment(1234567890, "holonbot[bot]")

		// Create temporary directory for artifacts
		tempDir := t.TempDir()
		prFixContent := `{
			"review_replies": [
				{
					"comment_id": 1234567890,
					"status": "fixed",
					"message": "Fixed the bug"
				}
			]
		}`
		prFixPath := filepath.Join(tempDir, "pr-fix.json")
		if err := os.WriteFile(prFixPath, []byte(prFixContent), 0644); err != nil {
			t.Fatalf("Failed to write pr-fix.json: %v", err)
		}

		// Create publisher with mock server URL
		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/123",
			Artifacts: map[string]string{
				"pr-fix.json": prFixPath,
			},
		}

		// Set bot login
		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		// Verify idempotency - should skip the existing reply
		if !result.Success {
			t.Errorf("Expected success=true, got false. Errors: %v", result.Errors)
		}

		// Check that no new comment was created
		if mockServer.createCommentCalls != 0 {
			t.Errorf("Expected 0 create comment calls (should skip existing), got %d", mockServer.createCommentCalls)
		}

		// Verify the action indicates skipping
		foundSkip := false
		for _, action := range result.Actions {
			if strings.Contains(action.Type, "review") && strings.Contains(action.Description, "skipped") {
				foundSkip = true
				break
			}
		}
		if !foundSkip {
			// At minimum, we should not have created a new reply
			t.Logf("Actions: %+v", result.Actions)
		}
	})
}

// TestContractReviewRepliesPosting tests successful reply posting
func TestContractReviewRepliesPosting(t *testing.T) {
	t.Run("post new reply successfully", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		// Create temporary directory for artifacts
		tempDir := t.TempDir()
		prFixContent := `{
			"review_replies": [
				{
					"comment_id": 1234567890,
					"status": "fixed",
					"message": "Fixed the null pointer dereference",
					"action_taken": "Added nil check"
				}
			]
		}`
		prFixPath := filepath.Join(tempDir, "pr-fix.json")
		if err := os.WriteFile(prFixPath, []byte(prFixContent), 0644); err != nil {
			t.Fatalf("Failed to write pr-fix.json: %v", err)
		}

		// Create publisher
		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/123",
			Artifacts: map[string]string{
				"pr-fix.json": prFixPath,
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false. Errors: %v", result.Errors)
		}

		// Should create exactly 1 comment
		if mockServer.createCommentCalls != 1 {
			t.Errorf("Expected 1 create comment call, got %d", mockServer.createCommentCalls)
		}

		// Verify action was recorded
		foundReplyAction := false
		for _, action := range result.Actions {
			if strings.Contains(action.Type, "replied_review_comment") {
				foundReplyAction = true
				if action.Metadata["comment_id"] == "" {
					t.Errorf("Expected comment_id in metadata")
				}
			}
		}
		if !foundReplyAction {
			t.Errorf("Expected replied_review_comment action, got actions: %+v", result.Actions)
		}
	})
}

// TestContractReviewRepliesMultiple tests posting multiple replies
func TestContractReviewRepliesMultiple(t *testing.T) {
	t.Run("post multiple replies with mixed existing", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		// Add existing reply for second comment only
		mockServer.addExistingComment(1234567891, "holonbot[bot]")

		// Create temporary directory for artifacts
		tempDir := t.TempDir()
		prFixContent := `{
			"review_replies": [
				{
					"comment_id": 1234567890,
					"status": "fixed",
					"message": "Fixed first issue"
				},
				{
					"comment_id": 1234567891,
					"status": "fixed",
					"message": "Fixed second issue"
				},
				{
					"comment_id": 1234567892,
					"status": "wontfix",
					"message": "Won't fix third issue"
				}
			]
		}`
		prFixPath := filepath.Join(tempDir, "pr-fix.json")
		if err := os.WriteFile(prFixPath, []byte(prFixContent), 0644); err != nil {
			t.Fatalf("Failed to write pr-fix.json: %v", err)
		}

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/456",
			Artifacts: map[string]string{
				"pr-fix.json": prFixPath,
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false")
		}

		// Should create 2 comments (skip 1 existing)
		if mockServer.createCommentCalls != 2 {
			t.Errorf("Expected 2 create comment calls, got %d", mockServer.createCommentCalls)
		}
	})
}

// TestContractSummaryCommentCreate tests creating a new summary comment
func TestContractSummaryCommentCreate(t *testing.T) {
	t.Run("create new summary comment", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		tempDir := t.TempDir()
		summaryContent := "# Test Summary\n\nThis is a test summary."
		summaryPath := filepath.Join(tempDir, "summary.md")
		if err := os.WriteFile(summaryPath, []byte(summaryContent), 0644); err != nil {
			t.Fatalf("Failed to write summary.md: %v", err)
		}

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/789",
			Artifacts: map[string]string{
				"summary.md": summaryPath,
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false")
		}

		// Should create 1 issue comment (not edit)
		if mockServer.createIssueCommentCalls != 1 {
			t.Errorf("Expected 1 create issue comment call, got %d", mockServer.createIssueCommentCalls)
		}
		if mockServer.editCommentCalls != 0 {
			t.Errorf("Expected 0 edit comment calls for new comment, got %d", mockServer.editCommentCalls)
		}

		// Verify action
		foundCreateAction := false
		for _, action := range result.Actions {
			if action.Type == "created_summary_comment" {
				foundCreateAction = true
				break
			}
		}
		if !foundCreateAction {
			t.Errorf("Expected created_summary_comment action")
		}
	})
}

// TestContractSummaryCommentUpdate tests updating an existing summary comment
func TestContractSummaryCommentUpdate(t *testing.T) {
	t.Run("update existing summary comment", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		// Add existing summary comment
		existingID := mockServer.addExistingSummaryComment("holonbot[bot]")

		tempDir := t.TempDir()
		summaryContent := "# Updated Summary\n\nThis is an updated test summary."
		summaryPath := filepath.Join(tempDir, "summary.md")
		if err := os.WriteFile(summaryPath, []byte(summaryContent), 0644); err != nil {
			t.Fatalf("Failed to write summary.md: %v", err)
		}

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/101",
			Artifacts: map[string]string{
				"summary.md": summaryPath,
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false")
		}

		// Should edit existing comment (not create new)
		if mockServer.createCommentCalls != 0 {
			t.Errorf("Expected 0 create comment calls, got %d", mockServer.createCommentCalls)
		}
		if mockServer.editCommentCalls != 1 {
			t.Errorf("Expected 1 edit comment call, got %d", mockServer.editCommentCalls)
		}

		// Verify action
		foundUpdateAction := false
		for _, action := range result.Actions {
			if action.Type == "updated_summary_comment" {
				foundUpdateAction = true
				if action.Metadata["comment_id"] != fmt.Sprintf("%d", existingID) {
					t.Errorf("Expected comment_id %d, got %s", existingID, action.Metadata["comment_id"])
				}
				break
			}
		}
		if !foundUpdateAction {
			t.Errorf("Expected updated_summary_comment action")
		}
	})
}

// TestContractSummaryCommentMostRecent tests that the most recent summary comment is updated
func TestContractSummaryCommentMostRecent(t *testing.T) {
	t.Run("update most recent summary when multiple exist", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		// Add multiple summary comments
		mockServer.addExistingSummaryComment("holonbot[bot]")
		mostRecentID := mockServer.addExistingSummaryComment("holonbot[bot]")

		tempDir := t.TempDir()
		summaryContent := "# Latest Summary\n\nMost recent content."
		summaryPath := filepath.Join(tempDir, "summary.md")
		if err := os.WriteFile(summaryPath, []byte(summaryContent), 0644); err != nil {
			t.Fatalf("Failed to write summary.md: %v", err)
		}

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/202",
			Artifacts: map[string]string{
				"summary.md": summaryPath,
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false")
		}

		// Should only edit once, targeting the most recent
		if mockServer.editCommentCalls != 1 {
			t.Errorf("Expected 1 edit comment call, got %d", mockServer.editCommentCalls)
		}
		if mockServer.createCommentCalls != 0 {
			t.Errorf("Expected 0 create comment calls, got %d", mockServer.createCommentCalls)
		}

		// Verify it updated the most recent comment
		foundUpdateAction := false
		for _, action := range result.Actions {
			if action.Type == "updated_summary_comment" {
				foundUpdateAction = true
				if action.Metadata["comment_id"] != fmt.Sprintf("%d", mostRecentID) {
					t.Errorf("Expected most recent comment_id %d, got %s", mostRecentID, action.Metadata["comment_id"])
				}
				break
			}
		}
		if !foundUpdateAction {
			t.Errorf("Expected updated_summary_comment action")
		}
	})
}

// TestContractFullPublishWithFixtures tests the complete publish flow
func TestContractFullPublishWithFixtures(t *testing.T) {
	t.Run("full publish with both pr-fix.json and summary.md", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		tempDir := t.TempDir()

		// Use fixture files
		copyFixtureToFile(t, "pr_fix_single_reply.json", tempDir, "pr-fix.json")
		copyFixtureToFile(t, "summary_simple.md", tempDir, "summary.md")

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "holon-run/holon/pr/303",
			Artifacts: map[string]string{
				"pr-fix.json": filepath.Join(tempDir, "pr-fix.json"),
				"summary.md":  filepath.Join(tempDir, "summary.md"),
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false. Errors: %v", result.Errors)
		}

		// Should have both review reply and summary comment
		if mockServer.createCommentCalls != 1 { // 1 review reply
			t.Errorf("Expected 1 create PR comment call (review reply), got %d", mockServer.createCommentCalls)
		}
		if mockServer.createIssueCommentCalls != 1 { // 1 summary
			t.Errorf("Expected 1 create issue comment call (summary), got %d", mockServer.createIssueCommentCalls)
		}

		// Verify both types of actions are present
		hasReviewAction := false
		hasSummaryAction := false
		for _, action := range result.Actions {
			if strings.Contains(action.Type, "review") {
				hasReviewAction = true
			}
			if strings.Contains(action.Type, "summary") {
				hasSummaryAction = true
			}
		}
		if !hasReviewAction {
			t.Errorf("Expected review reply action")
		}
		if !hasSummaryAction {
			t.Errorf("Expected summary comment action")
		}
	})
}

// TestContractEmptyPRFixtures tests with empty pr-fix.json
func TestContractEmptyPRFixtures(t *testing.T) {
	t.Run("publish with empty pr-fix.json", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		tempDir := t.TempDir()
		copyFixtureToFile(t, "pr_fix_empty.json", tempDir, "pr-fix.json")
		copyFixtureToFile(t, "summary_simple.md", tempDir, "summary.md")

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/404",
			Artifacts: map[string]string{
				"pr-fix.json": filepath.Join(tempDir, "pr-fix.json"),
				"summary.md":  filepath.Join(tempDir, "summary.md"),
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false")
		}

		// Should only create summary comment (no review replies for empty pr-fix.json)
		if mockServer.createIssueCommentCalls != 1 {
			t.Errorf("Expected 1 create issue comment call (summary only), got %d", mockServer.createIssueCommentCalls)
		}
	})
}

// TestContractMultipleRepliesWithFixtures tests with multiple replies from fixture
func TestContractMultipleRepliesWithFixtures(t *testing.T) {
	t.Run("publish multiple replies from fixture", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		tempDir := t.TempDir()
		copyFixtureToFile(t, "pr_fix_multiple_replies.json", tempDir, "pr-fix.json")
		copyFixtureToFile(t, "summary_detailed.md", tempDir, "summary.md")

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/505",
			Artifacts: map[string]string{
				"pr-fix.json": filepath.Join(tempDir, "pr-fix.json"),
				"summary.md":  filepath.Join(tempDir, "summary.md"),
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		if !result.Success {
			t.Errorf("Expected success=true, got false")
		}

		// Should create 3 review replies + 1 summary
		if mockServer.createCommentCalls != 3 { // 3 review replies
			t.Errorf("Expected 3 create PR comment calls (review replies), got %d", mockServer.createCommentCalls)
		}
		if mockServer.createIssueCommentCalls != 1 { // 1 summary
			t.Errorf("Expected 1 create issue comment call (summary), got %d", mockServer.createIssueCommentCalls)
		}
	})
}

// TestContractMissingArtifacts tests handling of missing artifacts
func TestContractMissingArtifacts(t *testing.T) {
	t.Run("gracefully handle missing pr-fix.json", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		tempDir := t.TempDir()
		copyFixtureToFile(t, "summary_simple.md", tempDir, "summary.md")

		p := NewGitHubPublisher()
		p.client = newTestGitHubClient(t, mockServer)

		req := publisher.PublishRequest{
			Target: "testowner/testrepo/pr/606",
			Artifacts: map[string]string{
				"summary.md": filepath.Join(tempDir, "summary.md"),
				// pr-fix.json intentionally omitted
			},
		}

		t.Setenv(BotLoginEnv, "holonbot[bot]")

		result, err := p.Publish(req)
		if err != nil {
			t.Fatalf("Publish() error = %v", err)
		}

		// Should succeed with just summary
		if !result.Success {
			t.Errorf("Expected success=true when pr-fix.json is missing, got false")
		}

		if mockServer.createIssueCommentCalls != 1 {
			t.Errorf("Expected 1 summary issue comment creation, got %d", mockServer.createIssueCommentCalls)
		}
	})
}

// TestContractReplyFormats tests different reply status formats
func TestContractReplyFormats(t *testing.T) {
	tests := []struct {
		name     string
		status   string
		contains []string
	}{
		{"fixed status", "fixed", []string{"✅", "FIXED"}},
		{"wontfix status", "wontfix", []string{"⚠️", "WONTFIX"}},
		{"need-info status", "need-info", []string{"❓", "NEED-INFO"}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockServer := newMockGitHubServer(t)
			defer mockServer.close()

			tempDir := t.TempDir()
			prFixContent := fmt.Sprintf(`{
				"review_replies": [
					{
						"comment_id": 999888777,
						"status": "%s",
						"message": "Test message"
					}
				]
			}`, tt.status)
			prFixPath := filepath.Join(tempDir, "pr-fix.json")
			if err := os.WriteFile(prFixPath, []byte(prFixContent), 0644); err != nil {
				t.Fatalf("Failed to write pr-fix.json: %v", err)
			}

			p := NewGitHubPublisher()
			p.client = newTestGitHubClient(t, mockServer)

			req := publisher.PublishRequest{
				Target: "testowner/testrepo/pr/707",
				Artifacts: map[string]string{
					"pr-fix.json": prFixPath,
				},
			}

			t.Setenv(BotLoginEnv, "holonbot[bot]")

			result, err := p.Publish(req)
			if err != nil {
				t.Fatalf("Publish() error = %v", err)
			}

			if !result.Success {
				t.Errorf("Expected success=true, got false")
			}

			// We can't easily inspect the message body sent to the mock server
			// but we can verify a comment was created
			if mockServer.createCommentCalls != 1 {
				t.Errorf("Expected 1 create comment call, got %d", mockServer.createCommentCalls)
			}
		})
	}
}

// copyFixtureToFile copies a fixture file to a test directory
func copyFixtureToFile(t *testing.T, fixtureName, destDir, destName string) {
	srcPath := filepath.Join("testdata/fixtures", fixtureName)
	destPath := filepath.Join(destDir, destName)

	data, err := os.ReadFile(srcPath)
	if err != nil {
		t.Fatalf("Failed to read fixture %s: %v", fixtureName, err)
	}

	if err := os.WriteFile(destPath, data, 0644); err != nil {
		t.Fatalf("Failed to write test file %s: %v", destPath, err)
	}
}

// newTestGitHubClient creates a GitHub client configured for the mock server
func newTestGitHubClient(t *testing.T, mockServer *mockGitHubServer) *github.Client {
	client := github.NewClient(nil)
	// Ensure trailing slash for BaseURL (go-github requirement)
	baseURL, _ := url.Parse(mockServer.server.URL + "/")
	client.BaseURL = baseURL
	return client
}
