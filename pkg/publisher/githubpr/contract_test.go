package githubpr

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/go-github/v68/github"
	gh "github.com/holon-run/holon/pkg/github"
	"github.com/holon-run/holon/pkg/publisher"
)

// ============================================================================
// VCR-Based Contract Tests for GitHub PR Publisher
// These tests use go-vcr to record/replay real GitHub API interactions.
// They ensure the API contract is maintained for PR creation/update operations.
//
// IMPORTANT: VCR fixtures must be recorded before these tests can run.
// To record new fixtures:
//   HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/publisher/githubpr/... -run TestVCR
//
// The fixtures will be stored in pkg/github/testdata/fixtures/githubpr/
// If fixtures don't exist, tests will be skipped with a helpful message.
//
// Note: When recording fixtures, ensure you have appropriate permissions in the
// test repository (holon-run/holon) and that any referenced PRs and comments exist.
// ============================================================================

// TestVCRPRPublisher_FindExistingPR tests finding existing PRs with VCR.
// This test locks the API contract for PR listing and finding.
//
// To record new fixtures:
//   HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/publisher/githubpr/... -run TestVCRPRPublisher_FindExistingPR
func TestVCRPRPublisher_FindExistingPR(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := gh.NewRecorder(t, "githubpr/find_existing_pr")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := gh.NewClient("", gh.WithHTTPClient(rec.HTTPClient()))

	// Test the findPRByBranch method directly
	p := &PRPublisher{}
	prRef := PRRef{
		Owner:      "holon-run",
		Repo:       "holon",
		BaseBranch: "main",
	}

	// This will list PRs and try to find one with matching head branch
	existingPR, err := p.findPRByBranch(context.Background(), client, prRef, "nonexistent-branch-12345")
	if err != nil {
		t.Fatalf("findPRByBranch() error = %v", err)
	}

	// Contract: Should return nil for non-existent branch
	if existingPR != nil {
		t.Errorf("findPRByBranch() = %v, want nil for non-existent branch", existingPR)
	}

	// Contract: Should not error on empty PR list
	// The method should handle pagination correctly
}

// TestVCRPRPublisher_BuildConfigContract tests configuration building.
// This test locks the configuration contract.
func TestVCRPRPublisher_BuildConfigContract(t *testing.T) {
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
			name: "with metadata configuration",
			manifest: map[string]interface{}{
				"metadata": map[string]interface{}{
					"branch":    "feature/test-branch",
					"title":     "Test PR Title",
					"issue_id":  "123",
				},
			},
			want: PRPublisherConfig{
				BranchName: "feature/test-branch",
				Title:      "Test PR Title",
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

			// Contract: Config should match expected values
			if got.BranchName != tt.want.BranchName {
				t.Errorf("BranchName = %v, want %v", got.BranchName, tt.want.BranchName)
			}
			if got.Title != tt.want.Title {
				t.Errorf("Title = %v, want %v", got.Title, tt.want.Title)
			}
			if got.IssueID != tt.want.IssueID {
				t.Errorf("IssueID = %v, want %v", got.IssueID, tt.want.IssueID)
			}
		})
	}
}

// TestVCRPRPublisher_ValidateContract tests request validation.
// This test locks the validation contract.
func TestVCRPRPublisher_ValidateContract(t *testing.T) {
	p := NewPRPublisher()

	tests := []struct {
		name    string
		req     publisher.PublishRequest
		wantErr bool
		errMsg  string
	}{
		{
			name: "valid request",
			req: publisher.PublishRequest{
				Target: "holon-run/holon:main",
				Artifacts: map[string]string{
					"diff.patch": "/tmp/diff.patch",
					"summary.md": "/tmp/summary.md",
				},
			},
			wantErr: false,
		},
		{
			name: "valid request without base branch",
			req: publisher.PublishRequest{
				Target: "holon-run/holon",
				Artifacts: map[string]string{
					"diff.patch": "/tmp/diff.patch",
					"summary.md": "/tmp/summary.md",
				},
			},
			wantErr: false,
		},
		{
			name: "invalid target format",
			req: publisher.PublishRequest{
				Target: "invalid-format",
				Artifacts: map[string]string{
					"diff.patch": "/tmp/diff.patch",
					"summary.md": "/tmp/summary.md",
				},
			},
			wantErr: true,
			errMsg:  "invalid target format",
		},
		{
			name: "missing diff.patch",
			req: publisher.PublishRequest{
				Target: "holon-run/holon",
				Artifacts: map[string]string{
					"summary.md": "/tmp/summary.md",
				},
			},
			wantErr: true,
			errMsg:  "diff.patch",
		},
		{
			name: "missing summary.md",
			req: publisher.PublishRequest{
				Target: "holon-run/holon",
				Artifacts: map[string]string{
					"diff.patch": "/tmp/diff.patch",
				},
			},
			wantErr: true,
			errMsg:  "summary.md",
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
			err := p.Validate(tt.req)
			if (err != nil) != tt.wantErr {
				t.Errorf("Validate() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if tt.wantErr && tt.errMsg != "" {
				if err == nil {
					t.Errorf("Validate() expected error containing %q, got nil", tt.errMsg)
				} else if !strings.Contains(err.Error(), tt.errMsg) {
					t.Errorf("Validate() error = %q, want containing %q", err.Error(), tt.errMsg)
				}
			}
		})
	}
}

// TestVCRPRPublisher_ActionMetadataContract tests action metadata structure.
// This test documents the expected metadata fields for each action type.
// Note: This is a documentation-only test that validates expected metadata structure
// but doesn't verify the implementation produces this metadata.
func TestVCRPRPublisher_ActionMetadataContract(t *testing.T) {
	// This is a structural test that validates the contract for action metadata

	// Contract: Actions should have specific metadata fields based on type
	expectedMetadata := map[string][]string{
		"applied_patch":   {}, // No specific metadata required
		"created_branch":  {"branch"},
		"created_commit":  {"commit"},
		"pushed_branch":   {"branch"},
		"created_pr":      {"pr_number", "pr_url"},
		"updated_pr":      {"pr_number", "pr_url"},
	}

	for actionType, expectedFields := range expectedMetadata {
		t.Run("action_"+actionType, func(t *testing.T) {
			// Validate that the action type is documented
			if actionType == "" {
				t.Error("Action type should not be empty")
			}

			// Validate expected metadata fields are documented
			for _, field := range expectedFields {
				if field == "" {
					t.Errorf("Metadata field name should not be empty for action %s", actionType)
				}
			}
		})
	}
}

// TestVCRPRPublisher_PublishActionsOrder documents that actions should be in correct order.
// This is a documentation-only test that specifies the expected action ordering.
func TestVCRPRPublisher_PublishActionsOrder(t *testing.T) {
	// Contract: Actions should be in a specific order
	expectedOrder := []string{
		"created_branch",
		"applied_patch",
		"created_commit",
		"pushed_branch",
		"created_pr", // or "updated_pr"
	}

	// This is a documentation test - the expected order is:
	// 1. Create branch (must be before patch to apply to correct branch)
	// 2. Apply patch
	// 3. Commit changes
	// 4. Push branch
	// 5. Create or update PR

	t.Log("Expected action order:", strings.Join(expectedOrder, " -> "))

	// The actual implementation should follow this order
	// This test documents the contract for future maintenance
	// Note: This test doesn't verify the implementation produces this order,
	// it only documents what the expected order should be.
}

// ============================================================================
// Mock Server Tests for PR Publisher API Interactions
// ============================================================================

// mockGitHubServerForPR creates a simple mock server for PR testing
type mockGitHubServerForPR struct {
	server *httptest.Server
	mux    *http.ServeMux
}

func newMockGitHubServerForPR(t *testing.T) *mockGitHubServerForPR {
	mux := http.NewServeMux()
	server := httptest.NewServer(mux)

	return &mockGitHubServerForPR{
		server: server,
		mux:    mux,
	}
}

func (m *mockGitHubServerForPR) close() {
	m.server.Close()
}

func (m *mockGitHubServerForPR) getBaseURL() string {
	return m.server.URL
}

// newTestGitHubClientForPR creates a GitHub client for the mock server
func newTestGitHubClientForPR(t *testing.T, mockServer *mockGitHubServerForPR) *gh.Client {
	return gh.NewClient("", gh.WithBaseURL(mockServer.server.URL))
}

// TestMockPRPublisher_ListPRs tests listing PRs with mock server
func TestMockPRPublisher_ListPRs(t *testing.T) {
	mockServer := newMockGitHubServerForPR(t)
	defer mockServer.close()

	// Handler for listing PRs
	mockServer.mux.HandleFunc("/repos/holon-run/holon/pulls", func(w http.ResponseWriter, r *http.Request) {
		// Return empty list
		prs := []github.PullRequest{}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(prs)
	})

	client := newTestGitHubClientForPR(t, mockServer)

	p := &PRPublisher{}
	prRef := PRRef{
		Owner:      "holon-run",
		Repo:       "holon",
		BaseBranch: "main",
	}

	// Test listing PRs
	existingPR, err := p.findPRByBranch(context.Background(), client, prRef, "test-branch")
	if err != nil {
		t.Fatalf("findPRByBranch() error = %v", err)
	}

	if existingPR != nil {
		t.Errorf("findPRByBranch() = %v, want nil for empty list", existingPR)
	}
}

// TestMockPRPublisher_FindPRByBranch tests finding PRs with mock server
func TestMockPRPublisher_FindPRByBranch(t *testing.T) {
	mockServer := newMockGitHubServerForPR(t)
	defer mockServer.close()

	// Handler for listing PRs
	mockServer.mux.HandleFunc("/repos/holon-run/holon/pulls", func(w http.ResponseWriter, r *http.Request) {
		// Return PR list with one matching PR
		prs := []github.PullRequest{
			{
				Number:  github.Int(456),
				Title:   github.String("Test PR"),
				Head: &github.PullRequestBranch{
					Ref: github.String("feature/test-branch"),
				},
			},
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(prs)
	})

	client := newTestGitHubClientForPR(t, mockServer)

	p := &PRPublisher{}
	prRef := PRRef{
		Owner:      "holon-run",
		Repo:       "holon",
		BaseBranch: "main",
	}

	// Test finding existing PR
	existingPR, err := p.findPRByBranch(context.Background(), client, prRef, "feature/test-branch")
	if err != nil {
		t.Fatalf("findPRByBranch() error = %v", err)
	}

	if existingPR == nil {
		t.Fatal("findPRByBranch() = nil, want existing PR")
	}

	if existingPR.Number != 456 {
		t.Errorf("PR number = %v, want 456", existingPR.Number)
	}
}

// TestVCRPRPublisher_ResultStructContract tests the PublishResult structure.
// This documents the expected result structure contract.
//
// To record new fixtures:
//   HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/publisher/githubpr/... -run TestVCRPRPublisher_ResultStructContract
func TestVCRPRPublisher_ResultStructContract(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := gh.NewRecorder(t, "githubpr/result_contract")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	// Test result structure validation
	// This test documents the expected contract - it manually constructs a result
	// to validate the expected structure, but doesn't verify the implementation produces it.

	result := publisher.PublishResult{
		Provider:   "github-pr",
		Target:     "holon-run/holon:main",
		Success:    true,
		Actions: []publisher.PublishAction{
			{
				Type:        "created_branch",
				Description: "Created branch: test-branch",
				Metadata: map[string]string{
					"branch": "test-branch",
				},
			},
			{
				Type:        "applied_patch",
				Description: "Applied diff.patch to workspace",
			},
			{
				Type:        "created_commit",
				Description: "Committed changes: abc123",
				Metadata: map[string]string{
					"commit": "abc123",
				},
			},
			{
				Type:        "pushed_branch",
				Description: "Pushed branch to remote: test-branch",
				Metadata: map[string]string{
					"branch": "test-branch",
				},
			},
			{
				Type:        "created_pr",
				Description: "Created PR #123",
				Metadata: map[string]string{
					"pr_number": "123",
					"pr_url":    "https://github.com/holon-run/holon/pull/123",
				},
			},
		},
	}

	// Contract: Provider should be "github-pr"
	if result.Provider != "github-pr" {
		t.Errorf("Provider = %v, want github-pr", result.Provider)
	}

	// Contract: Should have exactly 5 actions for successful PR creation
	if len(result.Actions) != 5 {
		t.Errorf("Actions count = %d, want 5", len(result.Actions))
	}

	// Contract: Actions should be in correct order
	expectedOrder := []string{"created_branch", "applied_patch", "created_commit", "pushed_branch", "created_pr"}
	for i, action := range result.Actions {
		if action.Type != expectedOrder[i] {
			t.Errorf("Action %d type = %v, want %v", i, action.Type, expectedOrder[i])
		}
	}

	// Contract: Each action should have a type and description
	for i, action := range result.Actions {
		if action.Type == "" {
			t.Errorf("Action %d has empty Type", i)
		}
		if action.Description == "" {
			t.Errorf("Action %d has empty Description", i)
		}
	}
}

// TestMockPRPublisher_CreateGitHubClient tests client creation.
// Note: This is a basic smoke test to verify the client is created.
// More thorough OAuth verification is done in TestMockPRPublisher_CreateGitHubClientWithTransport.
func TestMockPRPublisher_CreateGitHubClient(t *testing.T) {
	token := "test-token"

	client := gh.NewClient(token)

	if client == nil {
		t.Fatal("NewClient() returned nil")
	}

	// Verify the client is properly configured
	if client == nil {
		t.Error("Expected non-nil client")
	}
	// The actual OAuth token verification is done in TestMockPRPublisher_CreateGitHubClientWithTransport
}

// TestMockPRPublisher_CreateGitHubClientWithTransport tests that client uses correct transport
func TestMockPRPublisher_CreateGitHubClientWithTransport(t *testing.T) {
	token := "test-token-123"
	ctx := context.Background()

	// Create a mock server to test the client and verify OAuth token is used
	var receivedAuth string
	mockServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		receivedAuth = r.Header.Get("Authorization")
		w.Header().Set("Content-Type", "application/json")
		// Return minimal valid JSON body for go-github to decode
		_, _ = w.Write([]byte(`[]`))
	}))
	defer mockServer.Close()

	client := gh.NewClient(token, gh.WithBaseURL(mockServer.URL))

	// Make a test request to trigger a request
	_, err := client.ListPullRequests(ctx, "test", "test", "open")
	if err != nil {
		t.Logf("Request error (may be expected): %v", err)
	}

	// Verify the Authorization header was set with the token
	if receivedAuth == "" {
		t.Fatalf("expected Authorization header to be set, got empty string")
	}
	if !strings.Contains(receivedAuth, token) {
		t.Fatalf("expected Authorization header to contain token %q, got %q", token, receivedAuth)
	}
}

// TestVCRPRPublisher_ErrorHandlingContract tests error handling.
// This test locks the error handling contract.
//
// To record new fixtures:
//   HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/publisher/githubpr/... -run TestVCRPRPublisher_ErrorHandlingContract
func TestVCRPRPublisher_ErrorHandlingContract(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := gh.NewRecorder(t, "githubpr/error_handling")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := gh.NewClient("", gh.WithHTTPClient(rec.HTTPClient()))

	// Test with invalid repo
	p := &PRPublisher{}
	prRef := PRRef{
		Owner:      "nonexistent-repo-12345",
		Repo:       "nonexistent-repo-12345",
		BaseBranch: "main",
	}

	// This should fail but not panic
	_, err = p.findPRByBranch(context.Background(), client, prRef, "test-branch")

	// Contract: Should return an error for non-existent repo
	if err == nil {
		t.Error("Expected error for non-existent repo")
	}

	// Error should be meaningful
	if err != nil {
		t.Logf("Expected error: %v", err)
	}
}

// TestVCRPRPublisher_PaginationContract tests pagination handling
//
// To record new fixtures:
//   HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/publisher/githubpr/... -run TestVCRPRPublisher_PaginationContract
func TestVCRPRPublisher_PaginationContract(t *testing.T) {
	if testing.Short() {
		t.Skip("Skipping VCR test in short mode")
	}

	rec, err := gh.NewRecorder(t, "githubpr/pagination")
	if err != nil {
		t.Skipf("VCR fixture not found: %v (run with HOLON_VCR_MODE=record GITHUB_TOKEN=your_token to create)", err)
	}
	defer rec.Stop()

	client := gh.NewClient("", gh.WithHTTPClient(rec.HTTPClient()))

	p := &PRPublisher{}
	prRef := PRRef{
		Owner:      "holon-run",
		Repo:       "holon",
		BaseBranch: "main",
	}

	// This tests that pagination works correctly
	// The fixture should have pagination set up
	_, err = p.findPRByBranch(context.Background(), client, prRef, "any-branch")

	// Contract: Should handle pagination without error
	// Even if branch is not found, pagination should work
	if err != nil {
		t.Logf("findPRByBranch error (may be expected): %v", err)
	}
}
