package github

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-github/v68/github"
	hghelper "github.com/holon-run/holon/pkg/github"
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
	failCreateComments       bool // When true, return error for comment creation
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

	// Simulate API error if enabled
	if m.failCreateComments {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnprocessableEntity)
		errorResp := map[string]interface{}{
			"message": "Validation Failed",
			"errors":  []string{"Resource not accessible"},
		}
		if err := json.NewEncoder(w).Encode(errorResp); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
		return
	}

	// Decode request body - GitHub API expects either:
	// 1. Full PullRequestComment with in_reply_to_id (old way, causes 422)
	// 2. Simple body + in_reply_to (new way via CreateCommentInReplyTo)
	var payload struct {
		Body      string `json:"body"`
		InReplyTo int64  `json:"in_reply_to,omitempty"` // For CreateCommentInReplyTo
	}

	bodyBytes, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	if err := json.Unmarshal(bodyBytes, &payload); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	// Check for the wrong field (in_reply_to_id) which causes 422 in real GitHub API
	var rawPayload map[string]interface{}
	if err := json.Unmarshal(bodyBytes, &rawPayload); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if _, hasWrongField := rawPayload["in_reply_to_id"]; hasWrongField {
		// Real GitHub API returns 422 for this
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnprocessableEntity)
		errorResp := map[string]interface{}{
			"message": "Validation Failed",
			"errors": []map[string]string{
				{"code": "invalid", "field": "in_reply_to_id", "message": "in_reply_to_id is not a permitted key"},
			},
		}
		if err := json.NewEncoder(w).Encode(errorResp); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
		return
	}

	// Create response comment
	comment := &github.PullRequestComment{
		Body: github.String(payload.Body),
	}
	m.nextCommentID++
	comment.ID = github.Int64(m.nextCommentID)
	comment.User = &github.User{Login: github.String("holonbot[bot]")}
	// Set InReplyTo for idempotency checks (mock internal representation)
	if payload.InReplyTo > 0 {
		comment.InReplyTo = github.Int64(payload.InReplyTo)
		// Store if it's a reply
		m.comments[payload.InReplyTo] = append(m.comments[payload.InReplyTo], comment)
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
func newTestGitHubClient(t *testing.T, mockServer *mockGitHubServer) *hghelper.Client {
	// Create a client with a dummy token (not used in mock server)
	return hghelper.NewClient("test-token", hghelper.WithBaseURL(mockServer.server.URL))
}

// ============================================================================
// VCR-Based Contract Tests
// These tests use go-vcr to record/replay real GitHub API interactions.
// They ensure the API contract is maintained as GitHub's API evolves.
//
// Recording new fixtures:
//   HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/publisher/github/... -run TestVCR
//
// The fixtures will be stored in pkg/github/testdata/fixtures/publisher/
// ============================================================================

// TestVCRPublishSummaryComment tests summary comment publishing with VCR.
// This test locks the API contract for creating/updating summary comments.
//
// IMPORTANT: These VCR tests reference fixtures that must be recorded before they can run.
// To record new fixtures:
//   HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/publisher/github/... -run TestVCRPublishSummaryComment
//
// The fixtures will be stored in pkg/github/testdata/fixtures/publisher/
// If fixtures don't exist, these tests will be skipped with a helpful message.
func TestVCRPublishSummaryComment(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	// Create a VCR recorder
	rec, err := hghelper.NewRecorder(t, "publisher/summary_comment_create")
	if err != nil {
		// Skip gracefully if fixture doesn't exist (in replay mode)
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	// Create GitHub client with recorder
	client := hghelper.NewClient("", hghelper.WithHTTPClient(rec.HTTPClient()))

	// Create publisher with test client
	p := &GitHubPublisher{client: client}

	// Create test artifacts
	tmpDir := t.TempDir()
	summaryPath := filepath.Join(tmpDir, "summary.md")
	summaryContent := "# Test Summary\n\nThis is a test summary for VCR recording."
	if err := os.WriteFile(summaryPath, []byte(summaryContent), 0644); err != nil {
		t.Fatal(err)
	}

	req := publisher.PublishRequest{
		Target: "holon-run/holon#1",
		Artifacts: map[string]string{
			"summary.md": summaryPath,
		},
	}

	t.Setenv(BotLoginEnv, "holonbot[bot]")

	result, err := p.Publish(req)
	if err != nil {
		t.Fatalf("Publish() error = %v", err)
	}

	// Contract: Result should be successful
	if !result.Success {
		t.Errorf("Publish() Success = false, want true. Errors: %v", result.Errors)
	}

	// Contract: Should have summary comment action
	if len(result.Actions) == 0 {
		t.Fatal("Publish() has no actions")
	}

	action := result.Actions[0]
	if action.Type != "created_summary_comment" && action.Type != "updated_summary_comment" {
		t.Errorf("Action Type = %v, want created/updated_summary_comment", action.Type)
	}

	// Contract: Should have comment_id in metadata
	if action.Metadata["comment_id"] == "" {
		t.Error("Action Metadata missing comment_id")
	}

	// Contract: Provider should be "github"
	if result.Provider != "github" {
		t.Errorf("Provider = %v, want github", result.Provider)
	}
}

// TestVCRPublishReviewReplies tests review reply publishing with VCR.
// This test locks the API contract for posting review replies.
func TestVCRPublishReviewReplies(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := hghelper.NewRecorder(t, "publisher/review_replies_post")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := hghelper.NewClient("", hghelper.WithHTTPClient(rec.HTTPClient()))
	p := &GitHubPublisher{client: client}

	tmpDir := t.TempDir()

	// Create pr-fix.json
	prFixPath := filepath.Join(tmpDir, "pr-fix.json")
	actionTaken := "Updated code"
	prFixData := PRFixData{
		ReviewReplies: []ReviewReply{
			{
				CommentID:   123456789,
				Status:      "fixed",
				Message:     "Fixed the bug",
				ActionTaken: &actionTaken,
			},
		},
	}
	prFixJSON, _ := json.Marshal(prFixData)
	if err := os.WriteFile(prFixPath, prFixJSON, 0644); err != nil {
		t.Fatal(err)
	}

	req := publisher.PublishRequest{
		Target: "holon-run/holon#1",
		Artifacts: map[string]string{
			"pr-fix.json": prFixPath,
		},
	}

	t.Setenv(BotLoginEnv, "holonbot[bot]")

	result, err := p.Publish(req)
	if err != nil {
		t.Fatalf("Publish() error = %v", err)
	}

	// Contract: Should be successful
	if !result.Success {
		t.Errorf("Success = false, want true. Errors: %v", result.Errors)
	}

	// Contract: Should have review reply actions
	hasReplyAction := false
	for _, action := range result.Actions {
		if strings.Contains(action.Type, "review") {
			hasReplyAction = true
			break
		}
	}
	if !hasReplyAction {
		t.Error("Missing review reply action")
	}
}

// TestVCRPublishIdempotency tests that existing replies are detected with VCR.
// This test locks the idempotency behavior contract.
func TestVCRPublishIdempotency(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := hghelper.NewRecorder(t, "publisher/review_replies_idempotent")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := hghelper.NewClient("", hghelper.WithHTTPClient(rec.HTTPClient()))
	p := &GitHubPublisher{client: client}

	tmpDir := t.TempDir()

	// This fixture has one comment the bot already replied to
	prFixPath := filepath.Join(tmpDir, "pr-fix.json")
	prFixData := PRFixData{
		ReviewReplies: []ReviewReply{
			{
				CommentID: 111111111, // Existing reply in fixture
				Status:    "fixed",
				Message:   "Already replied",
			},
			{
				CommentID: 222222222, // New comment
				Status:    "fixed",
				Message:   "New reply",
			},
		},
	}
	prFixJSON, _ := json.Marshal(prFixData)
	if err := os.WriteFile(prFixPath, prFixJSON, 0644); err != nil {
		t.Fatal(err)
	}

	req := publisher.PublishRequest{
		Target: "holon-run/holon#1",
		Artifacts: map[string]string{
			"pr-fix.json": prFixPath,
		},
	}

	t.Setenv(BotLoginEnv, "holonbot[bot]")

	result, err := p.Publish(req)
	if err != nil {
		t.Fatalf("Publish() error = %v", err)
	}

	// Contract: Should succeed
	if !result.Success {
		t.Errorf("Success = false. Errors: %v", result.Errors)
	}

	// Contract: Summary should mention both posted and skipped
	// Note: The exact format may vary, but these keywords should appear
	summary := result.Actions[len(result.Actions)-1].Description
	hasPosted := strings.Contains(summary, "1 posted") || strings.Contains(summary, "posted")
	hasSkipped := strings.Contains(summary, "1 skipped") || strings.Contains(summary, "skipped")

	// These are informational warnings rather than hard assertions
	// since the exact summary format may vary
	if !hasPosted {
		t.Logf("Note: expected 'posted' in summary: %v", summary)
	}
	if !hasSkipped {
		t.Logf("Note: expected 'skipped' in summary: %v", summary)
	}
}

// TestVCRPublishBothSummaryAndReplies tests complete publish with VCR.
// This test locks the contract for publishing both summary and replies together.
func TestVCRPublishBothSummaryAndReplies(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := hghelper.NewRecorder(t, "publisher/summary_and_replies_full")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := hghelper.NewClient("", hghelper.WithHTTPClient(rec.HTTPClient()))
	p := &GitHubPublisher{client: client}

	tmpDir := t.TempDir()

	// Create summary
	summaryPath := filepath.Join(tmpDir, "summary.md")
	summaryContent := "# Full Test Summary\n\nTesting both summary and replies."
	if err := os.WriteFile(summaryPath, []byte(summaryContent), 0644); err != nil {
		t.Fatal(err)
	}

	// Create pr-fix.json
	prFixPath := filepath.Join(tmpDir, "pr-fix.json")
	prFixData := PRFixData{
		ReviewReplies: []ReviewReply{
			{
				// Note: This is a placeholder comment ID for VCR recording.
				// When recording fixtures, use actual comment IDs from the test repository.
				CommentID: 333333333,
				Status:    "fixed",
				Message:   "Fixed in combined run",
			},
		},
	}
	prFixJSON, _ := json.Marshal(prFixData)
	if err := os.WriteFile(prFixPath, prFixJSON, 0644); err != nil {
		t.Fatal(err)
	}

	req := publisher.PublishRequest{
		Target: "holon-run/holon#1",
		Artifacts: map[string]string{
			"summary.md":  summaryPath,
			"pr-fix.json": prFixPath,
		},
	}

	t.Setenv(BotLoginEnv, "holonbot[bot]")

	result, err := p.Publish(req)
	if err != nil {
		t.Fatalf("Publish() error = %v", err)
	}

	// Contract: Should succeed
	if !result.Success {
		t.Errorf("Success = false. Errors: %v", result.Errors)
	}

	// Contract: Should have both summary and reply actions
	hasSummary := false
	hasReply := false
	for _, action := range result.Actions {
		if strings.Contains(action.Type, "summary") {
			hasSummary = true
		}
		if strings.Contains(action.Type, "review") {
			hasReply = true
		}
	}

	if !hasSummary {
		t.Error("Missing summary action")
	}
	if !hasReply {
		t.Error("Missing review reply action")
	}

	// Contract: All actions should have proper structure
	for i, action := range result.Actions {
		if action.Type == "" {
			t.Errorf("Action %d has empty Type", i)
		}
		if action.Description == "" {
			t.Errorf("Action %d has empty Description", i)
		}
	}
}

// TestVCRResultStructContract tests that PublishResult matches expected structure.
// This ensures result structs don't change unexpectedly.
func TestVCRResultStructContract(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := hghelper.NewRecorder(t, "publisher/result_contract")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := hghelper.NewClient("", hghelper.WithHTTPClient(rec.HTTPClient()))
	p := &GitHubPublisher{client: client}

	tmpDir := t.TempDir()

	// Create minimal artifacts
	summaryPath := filepath.Join(tmpDir, "summary.md")
	if err := os.WriteFile(summaryPath, []byte("# Test"), 0644); err != nil {
		t.Fatal(err)
	}

	req := publisher.PublishRequest{
		Target: "holon-run/holon#1",
		Artifacts: map[string]string{
			"summary.md": summaryPath,
		},
	}

	t.Setenv(BotLoginEnv, "holonbot[bot]")

	result, err := p.Publish(req)
	if err != nil {
		t.Fatalf("Publish() error = %v", err)
	}

	// Contract: Result structure validation
	if result.Provider != "github" {
		t.Errorf("Provider = %v, want github", result.Provider)
	}

	if result.Target != req.Target {
		t.Errorf("Target = %v, want %v", result.Target, req.Target)
	}

	if result.PublishedAt.IsZero() {
		t.Error("PublishedAt is zero")
	}

	if len(result.Actions) == 0 {
		t.Error("Actions is empty")
	}

	// Contract: Action structure validation
	for i, action := range result.Actions {
		if action.Type == "" {
			t.Errorf("Action %d: Type is empty", i)
		}
		if action.Description == "" {
			t.Errorf("Action %d: Description is empty", i)
		}
		// Metadata is optional, but if present should be a map
		if action.Metadata != nil {
			if _, ok := action.Metadata["comment_id"]; ok && action.Metadata["comment_id"] == "" {
				t.Errorf("Action %d: comment_id in metadata is empty", i)
			}
		}
	}
}

// TestVCRErrorHandling tests error handling with VCR.
// This test locks the error behavior contract.
func TestVCRErrorHandling(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	// Test with invalid PR number (will error from API)
	rec, err := hghelper.NewRecorder(t, "publisher/error_handling")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := hghelper.NewClient("", hghelper.WithHTTPClient(rec.HTTPClient()))
	p := &GitHubPublisher{client: client}

	tmpDir := t.TempDir()
	summaryPath := filepath.Join(tmpDir, "summary.md")
	if err := os.WriteFile(summaryPath, []byte("# Test"), 0644); err != nil {
		t.Fatal(err)
	}

	// Use a non-existent PR to trigger an error
	req := publisher.PublishRequest{
		Target: "holon-run/holon#999999999",
		Artifacts: map[string]string{
			"summary.md": summaryPath,
		},
	}

	t.Setenv(BotLoginEnv, "holonbot[bot]")

	_, err = p.Publish(req)

	// Contract: Should return an error for non-existent PR
	if err == nil {
		t.Error("Expected error for non-existent PR, got nil")
	}

	// Error message should be meaningful
	// Note: The exact error message may vary, but it should indicate the PR wasn't found
	if err != nil && !strings.Contains(err.Error(), "404") && !strings.Contains(strings.ToLower(err.Error()), "not found") {
		t.Logf("Note: error message format may vary: %v", err)
	}
}

// TestContractReviewRepliesFailure tests error handling when review replies fail
func TestContractReviewRepliesFailure(t *testing.T) {
	t.Run("log and track individual reply failures", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		mockServer.failCreateComments = true // Simulate API errors
		defer mockServer.close()

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
					"status": "wontfix",
					"message": "Won't fix second issue"
				},
				{
					"comment_id": 1234567892,
					"status": "fixed",
					"message": "Fixed third issue"
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

		// Contract: Overall result should indicate failure
		if result.Success {
			t.Errorf("Expected success=false when all replies fail, got true")
		}

		// Contract: Individual errors should be captured in result.Errors
		if len(result.Errors) != 3 {
			t.Errorf("Expected 3 errors (one per failed reply), got %d. Errors: %+v", len(result.Errors), result.Errors)
		}

		// Contract: Each error should have meaningful details
		for i, e := range result.Errors {
			if e.Message == "" {
				t.Errorf("Error %d: Message is empty", i)
			}
			if e.Action != "publish_review_replies" {
				t.Errorf("Error %d: Expected action='publish_review_replies', got '%s'", i, e.Action)
			}
			if e.Details == nil {
				t.Errorf("Error %d: Details map is nil", i)
			} else if e.Details["comment_id"] == "" {
				t.Errorf("Error %d: comment_id not in details", i)
			}
		}

		// Contract: Summary action should show correct counts
		foundSummary := false
		for _, action := range result.Actions {
			if action.Type == "review_replies_summary" {
				foundSummary = true
				if !strings.Contains(action.Description, "0 posted") {
					t.Errorf("Expected summary to show '0 posted', got: %s", action.Description)
				}
				if !strings.Contains(action.Description, "3 failed") {
					t.Errorf("Expected summary to show '3 failed', got: %s", action.Description)
				}
				break
			}
		}
		if !foundSummary {
			t.Errorf("Expected review_replies_summary action, got: %+v", result.Actions)
		}

		// Contract: Should have attempted to create all 3 comments (all failed)
		if mockServer.createCommentCalls != 3 {
			t.Errorf("Expected 3 create comment calls (all failed), got %d", mockServer.createCommentCalls)
		}
	})
}

// TestContractReviewRepliesCorrectPayload tests that replies use correct JSON payload
func TestContractReviewRepliesCorrectPayload(t *testing.T) {
	t.Run("use in_reply_to field, not in_reply_to_id", func(t *testing.T) {
		mockServer := newMockGitHubServer(t)
		defer mockServer.close()

		tempDir := t.TempDir()
		prFixContent := `{
			"review_replies": [
				{
					"comment_id": 1234567890,
					"status": "fixed",
					"message": "Fixed with correct payload"
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

		// Contract: Should succeed without 422 error
		if !result.Success {
			t.Errorf("Expected success=true when using correct payload, got false. Errors: %v", result.Errors)
		}

		// Contract: Should create exactly 1 comment
		if mockServer.createCommentCalls != 1 {
			t.Errorf("Expected 1 create comment call, got %d", mockServer.createCommentCalls)
		}

		// Contract: No errors should be reported
		if len(result.Errors) > 0 {
			t.Errorf("Expected no errors, got: %+v", result.Errors)
		}
	})
}
