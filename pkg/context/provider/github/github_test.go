package github

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/holon-run/holon/pkg/context/collector"
)

func TestParseRef(t *testing.T) {
	tests := []struct {
		name      string
		ref       string
		repoHint  string
		wantOwner string
		wantRepo  string
		wantNum   int
		wantErr   bool
	}{
		{
			name:      "parse URL with pull",
			ref:       "https://github.com/owner/repo/pull/123",
			wantOwner: "owner",
			wantRepo:  "repo",
			wantNum:   123,
			wantErr:   false,
		},
		{
			name:      "parse URL with issues",
			ref:       "https://github.com/owner/repo/issues/456",
			wantOwner: "owner",
			wantRepo:  "repo",
			wantNum:   456,
			wantErr:   false,
		},
		{
			name:      "parse owner/repo#number format",
			ref:       "owner/repo#789",
			wantOwner: "owner",
			wantRepo:  "repo",
			wantNum:   789,
			wantErr:   false,
		},
		{
			name:      "parse #number with repo hint",
			ref:       "#42",
			repoHint:  "hint/repo",
			wantOwner: "hint",
			wantRepo:  "repo",
			wantNum:   42,
			wantErr:   false,
		},
		{
			name:      "parse plain number with repo hint",
			ref:       "99",
			repoHint:  "another/hint",
			wantOwner: "another",
			wantRepo:  "hint",
			wantNum:   99,
			wantErr:   false,
		},
		{
			name:    "invalid URL - missing parts",
			ref:     "https://github.com/owner",
			wantErr: true,
		},
		{
			name:    "invalid ref format",
			ref:     "invalid-format",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			owner, repo, num, err := ParseRef(tt.ref, tt.repoHint)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParseRef() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if owner != tt.wantOwner {
					t.Errorf("ParseRef() owner = %v, want %v", owner, tt.wantOwner)
				}
				if repo != tt.wantRepo {
					t.Errorf("ParseRef() repo = %v, want %v", repo, tt.wantRepo)
				}
				if num != tt.wantNum {
					t.Errorf("ParseRef() num = %v, want %v", num, tt.wantNum)
				}
			}
		})
	}
}

func TestParseRepo(t *testing.T) {
	tests := []struct {
		name      string
		repo      string
		wantOwner string
		wantName  string
		wantErr   bool
	}{
		{
			name:      "parse owner/repo format",
			repo:      "owner/repo",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse github.com/owner/repo format",
			repo:      "github.com/owner/repo",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse https://github.com/owner/repo format",
			repo:      "https://github.com/owner/repo",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse owner/repo.git format",
			repo:      "owner/repo.git",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse https://github.com/owner/repo.git format",
			repo:      "https://github.com/owner/repo.git",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:    "invalid - missing repo",
			repo:    "owner",
			wantErr: true,
		},
		{
			name:    "invalid - empty string",
			repo:    "",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			owner, name, err := parseRepo(tt.repo)
			if (err != nil) != tt.wantErr {
				t.Errorf("parseRepo() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if owner != tt.wantOwner {
					t.Errorf("parseRepo() owner = %v, want %v", owner, tt.wantOwner)
				}
				if name != tt.wantName {
					t.Errorf("parseRepo() name = %v, want %v", name, tt.wantName)
				}
			}
		})
	}
}

func TestFetchCheckRuns(t *testing.T) {
	tests := []struct {
		name         string
		ref          string
		maxResults   int
		responseBody string
		wantCount    int
		wantErr      bool
	}{
		{
			name:       "fetch check runs successfully",
			ref:        "abc123",
			maxResults: 0, // no limit
			responseBody: `{
				"total_count": 2,
				"check_runs": [
					{
						"id": 1,
						"name": "test",
						"head_sha": "abc123",
						"status": "completed",
						"conclusion": "success",
						"started_at": "2024-01-01T00:00:00Z",
						"completed_at": "2024-01-01T00:01:00Z",
						"details_url": "https://example.com/details",
						"app": {
							"slug": "github-actions"
						},
						"check_suite": {
							"id": 123
						},
						"output": {
							"title": "Test Summary",
							"summary": "All tests passed",
							"text": "Detailed output"
						}
					},
					{
						"id": 2,
						"name": "lint",
						"head_sha": "abc123",
						"status": "completed",
						"conclusion": "failure",
						"output": {}
					}
				]
			}`,
			wantCount: 2,
			wantErr:   false,
		},
		{
			name:       "fetch with max results limit",
			ref:        "abc123",
			maxResults: 1,
			responseBody: `{
				"total_count": 2,
				"check_runs": [
					{
						"id": 1,
						"name": "test",
						"head_sha": "abc123",
						"status": "completed",
						"conclusion": "success"
					},
					{
						"id": 2,
						"name": "lint",
						"head_sha": "abc123",
						"status": "completed",
						"conclusion": "failure"
					}
				]
			}`,
			wantCount: 1,
			wantErr:   false,
		},
		{
			name:       "empty check runs response",
			ref:        "abc123",
			maxResults: 0,
			responseBody: `{
				"total_count": 0,
				"check_runs": []
			}`,
			wantCount: 0,
			wantErr:   false,
		},
		{
			name:         "API error",
			ref:          "abc123",
			maxResults:   0,
			responseBody: `{"message": "Not Found"}`,
			wantCount:    0,
			wantErr:      true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create test server
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				// Verify request
				if r.URL.Path != "/repos/owner/repo/commits/"+tt.ref+"/check-runs" {
					t.Errorf("unexpected path: %s", r.URL.Path)
				}
				if r.Method != http.MethodGet {
					t.Errorf("unexpected method: %s", r.Method)
				}

				// Check auth header - accept both "token" and "Bearer" formats
				auth := r.Header.Get("Authorization")
				if auth != "token test-token" && auth != "Bearer test-token" {
					t.Errorf("unexpected authorization header: %s", auth)
				}

				if tt.wantErr {
					w.WriteHeader(http.StatusNotFound)
				} else {
					w.WriteHeader(http.StatusOK)
				}
				w.Header().Set("Content-Type", "application/json")
				w.Write([]byte(tt.responseBody))
			}))
			defer server.Close()

			// Create client with test server URL
			client := NewClient("test-token")
			client.SetBaseURL(server.URL)

			// Fetch check runs
			checkRuns, err := client.FetchCheckRuns(context.Background(), "owner", "repo", tt.ref, tt.maxResults)

			// Verify results
			if (err != nil) != tt.wantErr {
				t.Errorf("FetchCheckRuns() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if !tt.wantErr && len(checkRuns) != tt.wantCount {
				t.Errorf("FetchCheckRuns() got %d check runs, want %d", len(checkRuns), tt.wantCount)
			}

			if !tt.wantErr && len(checkRuns) > 0 {
				// Verify first check run
				if checkRuns[0].Name != "test" {
					t.Errorf("expected name 'test', got '%s'", checkRuns[0].Name)
				}
				if checkRuns[0].Conclusion != "success" {
					t.Errorf("expected conclusion 'success', got '%s'", checkRuns[0].Conclusion)
				}
				// Only verify these fields if we expect them to be present (first test case)
				if tt.name == "fetch check runs successfully" {
					if checkRuns[0].AppSlug != "github-actions" {
						t.Errorf("expected app_slug 'github-actions', got '%s'", checkRuns[0].AppSlug)
					}
					if checkRuns[0].Output.Title != "Test Summary" {
						t.Errorf("expected output title 'Test Summary', got '%s'", checkRuns[0].Output.Title)
					}
				}
			}
		})
	}
}

func TestFetchCombinedStatus(t *testing.T) {
	tests := []struct {
		name         string
		ref          string
		responseBody string
		wantState    string
		wantCount    int
		wantErr      bool
	}{
		{
			name: "fetch combined status successfully",
			ref:  "abc123",
			responseBody: `{
				"state": "success",
				"sha": "abc123",
				"total_count": 2,
				"statuses": [
					{
						"id": 1,
						"context": "ci/travis-ci",
						"state": "success",
						"target_url": "https://travis-ci.org/owner/repo/builds/123",
						"description": "The build passed",
						"created_at": "2024-01-01T00:00:00Z",
						"updated_at": "2024-01-01T00:01:00Z"
					},
					{
						"id": 2,
						"context": "coverage/coveralls",
						"state": "pending",
						"description": "Coverage report pending",
						"created_at": "2024-01-01T00:02:00Z",
						"updated_at": "2024-01-01T00:02:00Z"
					}
				]
			}`,
			wantState: "success",
			wantCount: 2,
			wantErr:   false,
		},
		{
			name: "empty status response",
			ref:  "abc123",
			responseBody: `{
				"state": "pending",
				"sha": "abc123",
				"total_count": 0,
				"statuses": []
			}`,
			wantState: "pending",
			wantCount: 0,
			wantErr:   false,
		},
		{
			name:         "API error",
			ref:          "abc123",
			responseBody: `{"message": "Not Found"}`,
			wantState:    "",
			wantCount:    0,
			wantErr:      true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create test server
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				// Verify request
				if r.URL.Path != "/repos/owner/repo/commits/"+tt.ref+"/status" {
					t.Errorf("unexpected path: %s", r.URL.Path)
				}
				if r.Method != http.MethodGet {
					t.Errorf("unexpected method: %s", r.Method)
				}

				// Check auth header - accept both "token" and "Bearer" formats
				auth := r.Header.Get("Authorization")
				if auth != "token test-token" && auth != "Bearer test-token" {
					t.Errorf("unexpected authorization header: %s", auth)
				}

				if tt.wantErr {
					w.WriteHeader(http.StatusNotFound)
				} else {
					w.WriteHeader(http.StatusOK)
				}
				w.Header().Set("Content-Type", "application/json")
				w.Write([]byte(tt.responseBody))
			}))
			defer server.Close()

			// Create client with test server URL
			client := NewClient("test-token")
			client.SetBaseURL(server.URL)

			// Fetch combined status
			status, err := client.FetchCombinedStatus(context.Background(), "owner", "repo", tt.ref)

			// Verify results
			if (err != nil) != tt.wantErr {
				t.Errorf("FetchCombinedStatus() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if !tt.wantErr {
				if status.State != tt.wantState {
					t.Errorf("FetchCombinedStatus() got state %s, want %s", status.State, tt.wantState)
				}
				if len(status.Statuses) != tt.wantCount {
					t.Errorf("FetchCombinedStatus() got %d statuses, want %d", len(status.Statuses), tt.wantCount)
				}
			}

			if !tt.wantErr && len(status.Statuses) > 0 {
				// Verify first status
				if status.Statuses[0].Context != "ci/travis-ci" {
					t.Errorf("expected context 'ci/travis-ci', got '%s'", status.Statuses[0].Context)
				}
				if status.Statuses[0].State != "success" {
					t.Errorf("expected state 'success', got '%s'", status.Statuses[0].State)
				}
				if status.Statuses[0].TargetURL != "https://travis-ci.org/owner/repo/builds/123" {
					t.Errorf("unexpected target_url: %s", status.Statuses[0].TargetURL)
				}
				if status.Statuses[0].Description != "The build passed" {
					t.Errorf("expected description 'The build passed', got '%s'", status.Statuses[0].Description)
				}
			}
		})
	}
}

func TestCheckRunMarshaling(t *testing.T) {
	startedAt := time.Date(2024, 1, 1, 0, 0, 0, 0, time.UTC)
	completedAt := time.Date(2024, 1, 1, 0, 1, 0, 0, time.UTC)

	checkRun := CheckRun{
		ID:           123,
		Name:         "test",
		HeadSHA:      "abc123",
		Status:       "completed",
		Conclusion:   "success",
		StartedAt:    &startedAt,
		CompletedAt:  &completedAt,
		DetailsURL:   "https://example.com/details",
		AppSlug:      "github-actions",
		CheckSuiteID: 456,
		Output: CheckRunOutput{
			Title:   "Test Summary",
			Summary: "All tests passed",
			Text:    "Detailed output",
		},
	}

	// Marshal to JSON
	data, err := json.Marshal(checkRun)
	if err != nil {
		t.Fatalf("failed to marshal: %v", err)
	}

	// Unmarshal back
	var unmarshaled CheckRun
	if err := json.Unmarshal(data, &unmarshaled); err != nil {
		t.Fatalf("failed to unmarshal: %v", err)
	}

	// Verify fields
	if unmarshaled.ID != checkRun.ID {
		t.Errorf("ID mismatch: got %d, want %d", unmarshaled.ID, checkRun.ID)
	}
	if unmarshaled.Name != checkRun.Name {
		t.Errorf("Name mismatch: got %s, want %s", unmarshaled.Name, checkRun.Name)
	}
	if unmarshaled.Conclusion != checkRun.Conclusion {
		t.Errorf("Conclusion mismatch: got %s, want %s", unmarshaled.Conclusion, checkRun.Conclusion)
	}
	if unmarshaled.AppSlug != checkRun.AppSlug {
		t.Errorf("AppSlug mismatch: got %s, want %s", unmarshaled.AppSlug, checkRun.AppSlug)
	}
	if unmarshaled.Output.Title != checkRun.Output.Title {
		t.Errorf("Output.Title mismatch: got %s, want %s", unmarshaled.Output.Title, checkRun.Output.Title)
	}
}

func TestCombinedStatusMarshaling(t *testing.T) {
	createdAt := time.Date(2024, 1, 1, 0, 0, 0, 0, time.UTC)
	updatedAt := time.Date(2024, 1, 1, 0, 1, 0, 0, time.UTC)

	status := CombinedStatus{
		SHA:        "abc123",
		State:      "success",
		TotalCount: 1,
		Statuses: []Status{
			{
				ID:          1,
				Context:     "ci/travis-ci",
				State:       "success",
				TargetURL:   "https://travis-ci.org",
				Description: "Build passed",
				CreatedAt:   createdAt,
				UpdatedAt:   updatedAt,
			},
		},
	}

	// Marshal to JSON
	data, err := json.Marshal(status)
	if err != nil {
		t.Fatalf("failed to marshal: %v", err)
	}

	// Unmarshal back
	var unmarshaled CombinedStatus
	if err := json.Unmarshal(data, &unmarshaled); err != nil {
		t.Fatalf("failed to unmarshal: %v", err)
	}

	// Verify fields
	if unmarshaled.SHA != status.SHA {
		t.Errorf("SHA mismatch: got %s, want %s", unmarshaled.SHA, status.SHA)
	}
	if unmarshaled.State != status.State {
		t.Errorf("State mismatch: got %s, want %s", unmarshaled.State, status.State)
	}
	if len(unmarshaled.Statuses) != len(status.Statuses) {
		t.Errorf("Statuses count mismatch: got %d, want %d", len(unmarshaled.Statuses), len(status.Statuses))
	}
	if len(unmarshaled.Statuses) > 0 {
		if unmarshaled.Statuses[0].Context != status.Statuses[0].Context {
			t.Errorf("Context mismatch: got %s, want %s", unmarshaled.Statuses[0].Context, status.Statuses[0].Context)
		}
	}
}

func TestVerifyContextFiles(t *testing.T) {
	// Helper function to create test context files
	createTestFiles := func(dir string, files map[string]string) error {
		for path, content := range files {
			fullPath := filepath.Join(dir, path)
			if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
				return err
			}
			if err := os.WriteFile(fullPath, []byte(content), 0644); err != nil {
				return err
			}
		}
		return nil
	}

	tests := []struct {
		name        string
		kind        collector.Kind
		files       map[string]string // path -> content (empty string = empty file)
		wantErr     bool
		errContains string
	}{
		{
			name: "valid PR context - all files non-empty",
			kind: collector.KindPR,
			files: map[string]string{
				"github/pr.json":             `{"number": 123}`,
				"github/review_threads.json": `[]`,
				"github/review.md":           "# Review",
				"pr-fix.schema.json":         `{}`,
			},
			wantErr: false,
		},
		{
			name: "valid issue context - all files non-empty",
			kind: collector.KindIssue,
			files: map[string]string{
				"github/issue.json":    `{"number": 123}`,
				"github/comments.json": `[]`,
				"github/issue.md":      "# Issue",
			},
			wantErr: false,
		},
		{
			name: "PR context - missing pr.json",
			kind: collector.KindPR,
			files: map[string]string{
				"github/review_threads.json": `[]`,
				"github/review.md":           "# Review",
				"pr-fix.schema.json":         `{}`,
			},
			wantErr:     true,
			errContains: "context file missing",
		},
		{
			name: "PR context - empty pr.json",
			kind: collector.KindPR,
			files: map[string]string{
				"github/pr.json":             "",
				"github/review_threads.json": `[]`,
				"github/review.md":           "# Review",
				"pr-fix.schema.json":         `{}`,
			},
			wantErr:     true,
			errContains: "context file is empty",
		},
		{
			name: "PR context - missing review.md",
			kind: collector.KindPR,
			files: map[string]string{
				"github/pr.json":             `{"number": 123}`,
				"github/review_threads.json": `[]`,
				"pr-fix.schema.json":         `{}`,
			},
			wantErr:     true,
			errContains: "context file missing",
		},
		{
			name: "PR context - empty review.md",
			kind: collector.KindPR,
			files: map[string]string{
				"github/pr.json":             `{"number": 123}`,
				"github/review_threads.json": `[]`,
				"github/review.md":           "",
				"pr-fix.schema.json":         `{}`,
			},
			wantErr:     true,
			errContains: "context file is empty",
		},
		{
			name: "PR context - missing pr-fix.schema.json",
			kind: collector.KindPR,
			files: map[string]string{
				"github/pr.json":             `{"number": 123}`,
				"github/review_threads.json": `[]`,
				"github/review.md":           "# Review",
			},
			wantErr:     true,
			errContains: "context file missing",
		},
		{
			name: "issue context - missing issue.json",
			kind: collector.KindIssue,
			files: map[string]string{
				"github/comments.json": `[]`,
				"github/issue.md":      "# Issue",
			},
			wantErr:     true,
			errContains: "context file missing",
		},
		{
			name: "issue context - empty issue.json",
			kind: collector.KindIssue,
			files: map[string]string{
				"github/issue.json":    "",
				"github/comments.json": `[]`,
				"github/issue.md":      "# Issue",
			},
			wantErr:     true,
			errContains: "context file is empty",
		},
		{
			name: "issue context - missing issue.md",
			kind: collector.KindIssue,
			files: map[string]string{
				"github/issue.json":    `{"number": 123}`,
				"github/comments.json": `[]`,
			},
			wantErr:     true,
			errContains: "context file missing",
		},
		{
			name: "network error simulation - all files empty",
			kind: collector.KindPR,
			files: map[string]string{
				"github/pr.json":             "",
				"github/review_threads.json": "",
				"github/review.md":           "",
				"pr-fix.schema.json":         "",
			},
			wantErr:     true,
			errContains: "empty",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create temp directory
			tmpDir, err := os.MkdirTemp("", "holon-test-verify-*")
			if err != nil {
				t.Fatalf("failed to create temp dir: %v", err)
			}
			defer os.RemoveAll(tmpDir)

			// Create test files
			if err := createTestFiles(tmpDir, tt.files); err != nil {
				t.Fatalf("failed to create test files: %v", err)
			}

			// Run verification
			err = verifyContextFiles(tmpDir, tt.kind)

			// Check results
			if (err != nil) != tt.wantErr {
				t.Errorf("verifyContextFiles() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if tt.wantErr && tt.errContains != "" {
				if err == nil {
					t.Errorf("expected error containing %q, got nil", tt.errContains)
					return
				}
				if !strings.Contains(err.Error(), tt.errContains) {
					t.Errorf("expected error containing %q, got %q", tt.errContains, err.Error())
				}
			}
		})
	}
}

func TestVerifyContextFilesNetworkFailureSimulation(t *testing.T) {
	// This test simulates what happens when GitHub API calls fail
	// and result in empty context files being written

	tmpDir, err := os.MkdirTemp("", "holon-test-network-*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

	// Simulate failed API response by creating empty files
	// This is what could happen if the token is invalid or network fails
	githubDir := filepath.Join(tmpDir, "github")
	if err := os.MkdirAll(githubDir, 0755); err != nil {
		t.Fatalf("failed to create github dir: %v", err)
	}

	// Create empty files (simulating failed fetch that wrote empty files)
	emptyFiles := []string{
		"github/pr.json",
		"github/review_threads.json",
		"github/review.md",
	}
	for _, file := range emptyFiles {
		path := filepath.Join(tmpDir, file)
		if err := os.WriteFile(path, []byte(""), 0644); err != nil {
			t.Fatalf("failed to create empty file: %v", err)
		}
	}
	// Also create empty schema file
	if err := os.WriteFile(filepath.Join(tmpDir, "pr-fix.schema.json"), []byte(""), 0644); err != nil {
		t.Fatalf("failed to create empty schema file: %v", err)
	}

	// Verify should fail with empty file error
	err = verifyContextFiles(tmpDir, collector.KindPR)
	if err == nil {
		t.Error("expected error for empty context files, got nil")
		return
	}

	// Check that error message mentions checking token and network
	if !strings.Contains(err.Error(), "check token and network connectivity") {
		t.Errorf("expected error message to mention checking token/network, got: %v", err)
	}
}
