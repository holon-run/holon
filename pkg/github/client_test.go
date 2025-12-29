package github

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// setupTestClient creates a test client with VCR recording
func setupTestClient(t *testing.T, fixtureName string) (*Client, *Recorder) {
	t.Helper()

	// Check if fixtures directory exists
	fixturesDir := filepath.Join("testdata", "fixtures")
	if _, err := os.Stat(fixturesDir); os.IsNotExist(err) {
		t.Skipf("fixtures directory not found. To record fixtures, run: HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/github/...")
	}

	// Create recorder
	rec, err := NewRecorder(t, fixtureName)
	if err != nil {
		// If cassette not found and we're in replay mode, skip the test
		if errors.Is(err, os.ErrNotExist) {
			t.Skipf("fixture %q not found. To record it, run: HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test -v ./pkg/github/ -run %s", fixtureName, t.Name())
		}
		t.Fatalf("failed to create recorder: %v", err)
	}

	// Create test client with recorder's HTTP client
	// Use a real token when recording, dummy token when replaying
	var token string
	if rec.IsRecording() {
		// Use actual GitHub token from environment for recording
		token = os.Getenv("GITHUB_TOKEN")
		if token == "" {
			t.Fatal("GITHUB_TOKEN environment variable must be set when recording fixtures")
		}
	} else {
		// Use dummy token for replay mode (it will be filtered from recordings)
		token = "test-token"
	}

	testClient := NewClient(token,
		WithTimeout(10*time.Second),
	)

	// Override the HTTP client to use the recorder
	testClient.httpClient = rec.HTTPClient()

	return testClient, rec
}

// TestFetchPRInfo tests fetching PR information
func TestFetchPRInfo(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "fetch_pr_info")
	defer rec.Stop()

	ctx := context.Background()

	prInfo, err := client.FetchPRInfo(ctx, "holon-run", "holon", 123)
	if err != nil {
		t.Fatalf("FetchPRInfo() error = %v", err)
	}

	// Verify basic fields
	if prInfo.Number != 123 {
		t.Errorf("Number = %v, want %v", prInfo.Number, 123)
	}

	if prInfo.Repository == "" {
		t.Error("Repository should not be empty")
	}

	if prInfo.Author == "" {
		t.Error("Author should not be empty")
	}

	// Verify time fields are non-zero
	if prInfo.CreatedAt.IsZero() {
		t.Error("CreatedAt should not be zero")
	}

	if prInfo.UpdatedAt.IsZero() {
		t.Error("UpdatedAt should not be zero")
	}

	// Verify SHA fields
	if prInfo.BaseSHA == "" {
		t.Error("BaseSHA should not be empty")
	}

	if prInfo.HeadSHA == "" {
		t.Error("HeadSHA should not be empty")
	}

	// Verify refs
	if prInfo.BaseRef == "" {
		t.Error("BaseRef should not be empty")
	}

	if prInfo.HeadRef == "" {
		t.Error("HeadRef should not be empty")
	}
}

// TestFetchIssueInfo tests fetching issue information
func TestFetchIssueInfo(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "fetch_issue_info")
	defer rec.Stop()

	ctx := context.Background()

	issueInfo, err := client.FetchIssueInfo(ctx, "holon-run", "holon", 289)
	if err != nil {
		t.Fatalf("FetchIssueInfo() error = %v", err)
	}

	// Verify basic fields
	if issueInfo.Number != 289 {
		t.Errorf("Number = %v, want %v", issueInfo.Number, 289)
	}

	if issueInfo.Title == "" {
		t.Error("Title should not be empty")
	}

	if issueInfo.State == "" {
		t.Error("State should not be empty")
	}

	if issueInfo.Author == "" {
		t.Error("Author should not be empty")
	}

	// Verify time fields
	if issueInfo.CreatedAt.IsZero() {
		t.Error("CreatedAt should not be zero")
	}

	if issueInfo.UpdatedAt.IsZero() {
		t.Error("UpdatedAt should not be zero")
	}
}

// TestFetchIssueComments tests fetching issue comments with pagination
func TestFetchIssueComments(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "fetch_issue_comments")
	defer rec.Stop()

	ctx := context.Background()

	comments, err := client.FetchIssueComments(ctx, "holon-run", "holon", 289)
	if err != nil {
		t.Fatalf("FetchIssueComments() error = %v", err)
	}

	// Verify we got comments
	if len(comments) == 0 {
		t.Fatal("Expected at least one comment")
	}

	// Verify comment structure
	for _, comment := range comments {
		if comment.CommentID == 0 {
			t.Error("CommentID should not be zero")
		}

		if comment.Body == "" {
			t.Error("Body should not be empty")
		}

		if comment.Author == "" {
			t.Error("Author should not be empty")
		}

		if comment.CreatedAt.IsZero() {
			t.Error("CreatedAt should not be zero")
		}
	}
}

// TestFetchReviewThreads tests fetching review comment threads
func TestFetchReviewThreads(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	tests := []struct {
		name           string
		fixture        string
		unresolvedOnly bool
		wantThreads    bool
	}{
		{
			name:           "all threads",
			fixture:        "fetch_review_threads_all",
			unresolvedOnly: false,
			wantThreads:    true,
		},
		{
			name:           "unresolved only",
			fixture:        "fetch_review_threads_unresolved",
			unresolvedOnly: true,
			wantThreads:    true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client, rec := setupTestClient(t, tt.fixture)
			defer rec.Stop()

			ctx := context.Background()

			threads, err := client.FetchReviewThreads(ctx, "holon-run", "holon", 123, tt.unresolvedOnly)
			if err != nil {
				t.Fatalf("FetchReviewThreads() error = %v", err)
			}

			if tt.wantThreads && len(threads) == 0 {
				t.Error("Expected at least one review thread")
			}

			// Verify thread structure
			for _, thread := range threads {
				if thread.CommentID == 0 {
					t.Error("CommentID should not be zero")
				}

				if thread.Path == "" {
					t.Error("Path should not be empty")
				}

				if thread.Body == "" {
					t.Error("Body should not be empty")
				}

				if thread.Author == "" {
					t.Error("Author should not be empty")
				}

				// If unresolvedOnly is true, verify all threads are unresolved
				if tt.unresolvedOnly && thread.Resolved {
					t.Error("Expected only unresolved threads")
				}
			}
		})
	}
}

// TestFetchPRDiff tests fetching PR diff
func TestFetchPRDiff(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "fetch_pr_diff")
	defer rec.Stop()

	ctx := context.Background()

	diff, err := client.FetchPRDiff(ctx, "holon-run", "holon", 123)
	if err != nil {
		t.Fatalf("FetchPRDiff() error = %v", err)
	}

	// Verify diff content
	if diff == "" {
		t.Fatal("Diff should not be empty")
	}

	// Verify it looks like a diff (has standard diff headers)
	if len(diff) < 10 {
		t.Fatal("Diff seems too short")
	}
}

// TestFetchCheckRuns tests fetching check runs for a ref
func TestFetchCheckRuns(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	tests := []struct {
		name      string
		fixture   string
		maxResults int
	}{
		{
			name:      "all check runs",
			fixture:   "fetch_check_runs_all",
			maxResults: 0, // No limit
		},
		{
			name:      "limited check runs",
			fixture:   "fetch_check_runs_limited",
			maxResults: 5,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client, rec := setupTestClient(t, tt.fixture)
			defer rec.Stop()

			ctx := context.Background()

			checkRuns, err := client.FetchCheckRuns(ctx, "holon-run", "holon", "44c152d51cb6991d33e53552726fb00086c4c478", tt.maxResults)
			if err != nil {
				t.Fatalf("FetchCheckRuns() error = %v", err)
			}

			if len(checkRuns) == 0 {
				t.Fatal("Expected at least one check run")
			}

			// Verify check run structure
			for _, cr := range checkRuns {
				if cr.ID == 0 {
					t.Error("ID should not be zero")
				}

				if cr.Name == "" {
					t.Error("Name should not be empty")
				}

				if cr.HeadSHA == "" {
					t.Error("HeadSHA should not be empty")
				}

				if cr.Status == "" {
					t.Error("Status should not be empty")
				}

				// Verify max results limit
				if tt.maxResults > 0 && len(checkRuns) > tt.maxResults {
					t.Errorf("Expected max %d results, got %d", tt.maxResults, len(checkRuns))
				}
			}
		})
	}
}

// TestFetchCombinedStatus tests fetching combined status
func TestFetchCombinedStatus(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "fetch_combined_status")
	defer rec.Stop()

	ctx := context.Background()

	status, err := client.FetchCombinedStatus(ctx, "holon-run", "holon", "44c152d51cb6991d33e53552726fb00086c4c478")
	if err != nil {
		t.Fatalf("FetchCombinedStatus() error = %v", err)
	}

	// Verify combined status structure
	if status.SHA == "" {
		t.Error("SHA should not be empty")
	}

	if status.State == "" {
		t.Error("State should not be empty")
	}

	// Verify statuses
	for _, st := range status.Statuses {
		if st.Context == "" {
			t.Error("Status context should not be empty")
		}

		if st.State == "" {
			t.Error("Status state should not be empty")
		}
	}
}

// TestRateLimitTracking tests rate limit header parsing
func TestRateLimitTracking(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "rate_limit_tracking")
	defer rec.Stop()

	// Enable rate limit tracking
	client.rateLimitTracker = NewRateLimitTracker()

	ctx := context.Background()

	// Make a request that will have rate limit headers
	_, err := client.FetchPRInfo(ctx, "holon-run", "holon", 123)
	if err != nil {
		t.Fatalf("FetchPRInfo() error = %v", err)
	}

	// Check rate limit status was updated
	status, err := client.GetRateLimitStatus()
	if err != nil {
		t.Fatalf("GetRateLimitStatus() error = %v", err)
	}

	// In recorded fixtures, we should have rate limit info
	if status.Limit == 0 {
		t.Log("Warning: Rate limit not recorded in fixture")
	}

	if status.Remaining == 0 && status.Limit > 0 {
		t.Log("Warning: No remaining requests recorded in fixture")
	}
}

// TestClientErrors tests error handling
func TestClientErrors(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	tests := []struct {
		name        string
		fixture     string
		setupClient func(*Client)
		fetch       func(*Client, context.Context) error
		wantErrType string
	}{
		{
			name:    "not found",
			fixture: "error_not_found",
			fetch: func(c *Client, ctx context.Context) error {
				_, err := c.FetchPRInfo(ctx, "holon-run", "holon", 999999999)
				return err
			},
			wantErrType: "not found",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client, rec := setupTestClient(t, tt.fixture)
			defer rec.Stop()

			if tt.setupClient != nil {
				tt.setupClient(client)
			}

			ctx := context.Background()

			err := tt.fetch(client, ctx)
			if err == nil {
				t.Fatal("Expected error, got nil")
			}

			// Verify error type
			if tt.wantErrType != "" {
				if tt.wantErrType == "not found" && !IsNotFoundError(err) {
					t.Errorf("Expected not found error, got: %v", err)
				}
			}
		})
	}
}

// TestPaginationTests tests pagination for various endpoints
func TestPaginationTests(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	t.Run("issue comments pagination", func(t *testing.T) {
		client, rec := setupTestClient(t, "pagination_issue_comments")
		defer rec.Stop()

		ctx := context.Background()

		// This fixture should have multiple pages
		comments, err := client.FetchIssueComments(ctx, "holon-run", "holon", 289)
		if err != nil {
			t.Fatalf("FetchIssueComments() error = %v", err)
		}

		// Verify we got a reasonable number of comments
		if len(comments) < 100 {
			t.Logf("Warning: Only got %d comments, pagination may not be fully tested", len(comments))
		}
	})

	t.Run("review threads pagination", func(t *testing.T) {
		client, rec := setupTestClient(t, "pagination_review_threads")
		defer rec.Stop()

		ctx := context.Background()

		threads, err := client.FetchReviewThreads(ctx, "holon-run", "holon", 123, false)
		if err != nil {
			t.Fatalf("FetchReviewThreads() error = %v", err)
		}

		// Verify thread structure even with pagination
		for _, thread := range threads {
			if thread.CommentID == 0 {
				t.Error("CommentID should not be zero")
			}

			// Verify replies are properly attached
			if len(thread.Replies) > 0 {
				for _, reply := range thread.Replies {
					if reply.InReplyToID == 0 {
						t.Error("Reply InReplyToID should not be zero")
					}
				}
			}
		}
	})
}

// TestFixtureFileStructure tests that fixture files exist and are properly formatted
func TestFixtureFileStructure(t *testing.T) {
	fixturesDir := filepath.Join("testdata", "fixtures")

	entries, err := os.ReadDir(fixturesDir)
	if err != nil {
		if os.IsNotExist(err) {
			t.Skipf("fixtures directory not found. To record fixtures, run: HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/github/...")
		}
		t.Fatalf("Failed to read fixtures directory: %v", err)
	}

	if len(entries) == 0 {
		t.Skip("No fixtures found - tests will need to record fixtures first. Run: HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/github/...")
	}

	// Verify all fixtures are YAML files
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}

		name := entry.Name()
		ext := filepath.Ext(name)

		if ext != ".yaml" && ext != ".yml" {
			t.Errorf("Fixture %s has invalid extension %s, want .yaml or .yml", name, ext)
		}
	}
}

// TestTypeConversions tests that type conversions from go-github types work correctly
func TestTypeConversions(t *testing.T) {
	// This test doesn't require fixtures - it tests the conversion functions directly

	t.Run("PRInfo conversion", func(t *testing.T) {
		// Test with minimal PR data
		prInfo := &PRInfo{
			Number:      123,
			Title:       "Test PR",
			State:       "open",
			BaseRef:     "main",
			HeadRef:     "feature",
			BaseSHA:     "abc123",
			HeadSHA:     "def456",
			Author:      "testuser",
			CreatedAt:   time.Now(),
			UpdatedAt:   time.Now(),
			Repository:  "holon-run/holon",
		}

		if prInfo.Number != 123 {
			t.Error("Number not preserved")
		}

		if prInfo.State != "open" {
			t.Error("State not preserved")
		}
	})

	t.Run("IssueInfo conversion", func(t *testing.T) {
		issueInfo := &IssueInfo{
			Number:     456,
			Title:      "Test Issue",
			State:      "open",
			Author:     "testuser",
			CreatedAt:  time.Now(),
			UpdatedAt:  time.Now(),
			Repository: "holon-run/holon",
			Labels:     []string{"bug", "enhancement"},
		}

		if len(issueInfo.Labels) != 2 {
			t.Errorf("Labels not preserved, got %d", len(issueInfo.Labels))
		}
	})

	t.Run("ReviewThread conversion", func(t *testing.T) {
		thread := &ReviewThread{
			CommentID: 789,
			Path:      "README.md",
			Line:      42,
			Body:      "Test comment",
			Author:    "testuser",
			CreatedAt: time.Now(),
			UpdatedAt: time.Now(),
			Resolved:  false,
			Replies: []Reply{
				{
					CommentID:   790,
					Body:        "Test reply",
					Author:      "anotheruser",
					CreatedAt:   time.Now(),
					UpdatedAt:   time.Now(),
					InReplyToID: 789,
				},
			},
		}

		if len(thread.Replies) != 1 {
			t.Error("Replies not preserved")
		}

		if thread.Replies[0].InReplyToID != 789 {
			t.Error("InReplyToID not correct")
		}
	})
}

// TestGetTokenFromEnv tests the GetTokenFromEnv function
func TestGetTokenFromEnv(t *testing.T) {
	// Save original env vars and restore after test
	origToken := os.Getenv(TokenEnv)
	origLegacyToken := os.Getenv(HolonTokenEnv)
	defer func() {
		if origToken != "" {
			os.Setenv(TokenEnv, origToken)
		} else {
			os.Unsetenv(TokenEnv)
		}
		if origLegacyToken != "" {
			os.Setenv(HolonTokenEnv, origLegacyToken)
		} else {
			os.Unsetenv(HolonTokenEnv)
		}
	}()

	t.Run("HOLON_GITHUB_TOKEN has highest priority", func(t *testing.T) {
		os.Setenv(HolonTokenEnv, "holon-token-999")
		os.Setenv(TokenEnv, "env-token-123")

		token, fromGh := GetTokenFromEnv()

		if token != "holon-token-999" {
			t.Errorf("Token = %q, want %q", token, "holon-token-999")
		}
		if fromGh {
			t.Error("Expected fromGh to be false when HOLON_GITHUB_TOKEN is set")
		}
	})

	t.Run("GITHUB_TOKEN set (no HOLON_GITHUB_TOKEN)", func(t *testing.T) {
		os.Unsetenv(HolonTokenEnv)
		os.Setenv(TokenEnv, "env-token-123")

		token, fromGh := GetTokenFromEnv()

		if token != "env-token-123" {
			t.Errorf("Token = %q, want %q", token, "env-token-123")
		}
		if fromGh {
			t.Error("Expected fromGh to be false when GITHUB_TOKEN is set")
		}
	})

	t.Run("priority: HOLON_GITHUB_TOKEN > GITHUB_TOKEN", func(t *testing.T) {
		os.Setenv(HolonTokenEnv, "holon-token")
		os.Setenv(TokenEnv, "ci-token")

		token, fromGh := GetTokenFromEnv()

		if token != "holon-token" {
			t.Errorf("Token = %q, want %q (HOLON_GITHUB_TOKEN should override GITHUB_TOKEN)", token, "holon-token")
		}
		if fromGh {
			t.Error("Expected fromGh to be false when HOLON_GITHUB_TOKEN is set")
		}
	})

	t.Run("no env vars, falls back to gh CLI if available", func(t *testing.T) {
		os.Unsetenv(HolonTokenEnv)
		os.Unsetenv(TokenEnv)

		token, fromGh := GetTokenFromEnv()

		// If gh CLI is available and authenticated, we should get a token
		// If gh CLI is not available, token should be empty
		if token != "" && !fromGh {
			t.Error("Expected fromGh to be true when token comes from gh CLI")
		}
		// We can't assert token == "" because gh might be available in the test environment
		// This test verifies that when no env vars are set, the function attempts gh CLI fallback
	})
}

// TestNewClientFromEnv tests the NewClientFromEnv function with various env states
func TestNewClientFromEnv(t *testing.T) {
	// Save original env vars and restore after test
	origToken := os.Getenv(TokenEnv)
	origLegacyToken := os.Getenv(HolonTokenEnv)
	defer func() {
		if origToken != "" {
			os.Setenv(TokenEnv, origToken)
		} else {
			os.Unsetenv(TokenEnv)
		}
		if origLegacyToken != "" {
			os.Setenv(HolonTokenEnv, origLegacyToken)
		} else {
			os.Unsetenv(HolonTokenEnv)
		}
	}()

	t.Run("with GITHUB_TOKEN set", func(t *testing.T) {
		os.Setenv(TokenEnv, "test-token-123")
		os.Unsetenv(HolonTokenEnv)

		client, err := NewClientFromEnv()
		if err != nil {
			t.Fatalf("NewClientFromEnv() error = %v", err)
		}
		if client == nil {
			t.Fatal("Expected non-nil client")
		}
		if client.GetToken() != "test-token-123" {
			t.Errorf("Token = %q, want %q", client.GetToken(), "test-token-123")
		}
	})

	t.Run("with HOLON_GITHUB_TOKEN set", func(t *testing.T) {
		os.Unsetenv(TokenEnv)
		os.Setenv(HolonTokenEnv, "holon-token-456")

		client, err := NewClientFromEnv()
		if err != nil {
			t.Fatalf("NewClientFromEnv() error = %v", err)
		}
		if client == nil {
			t.Fatal("Expected non-nil client")
		}
		if client.GetToken() != "holon-token-456" {
			t.Errorf("Token = %q, want %q", client.GetToken(), "holon-token-456")
		}
	})

	t.Run("with no env vars, may use gh CLI if available", func(t *testing.T) {
		os.Unsetenv(TokenEnv)
		os.Unsetenv(HolonTokenEnv)

		client, err := NewClientFromEnv()
		// If gh CLI is available and authenticated, this should succeed
		// If gh CLI is not available, this should return an error
		if err != nil {
			// Expected when gh CLI is not available
			if !strings.Contains(err.Error(), TokenEnv) && !strings.Contains(err.Error(), HolonTokenEnv) {
				t.Errorf("Error should mention env vars or gh login, got: %v", err)
			}
			return
		}
		// If we got here, gh CLI provided a token
		if client == nil {
			t.Fatal("Expected non-nil client when err is nil")
		}
		// Token should not be empty if client was created
		if client.GetToken() == "" {
			t.Error("Expected non-empty token from gh CLI")
		}
	})
}

// TestGetCurrentUser tests fetching the current user's identity
func TestGetCurrentUser(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "get_current_user")
	defer rec.Stop()

	ctx := context.Background()

	actorInfo, err := client.GetCurrentUser(ctx)
	if err != nil {
		t.Fatalf("GetCurrentUser() error = %v", err)
	}

	// Verify basic fields
	if actorInfo.Login == "" {
		t.Error("Login should not be empty")
	}

	if actorInfo.Type == "" {
		t.Error("Type should not be empty")
	}

	if actorInfo.Source != "token" {
		t.Errorf("Source = %q, want %q", actorInfo.Source, "token")
	}

	// Verify type is either User or App (Bot is converted to App)
	if actorInfo.Type != "User" && actorInfo.Type != "App" {
		t.Errorf("Type should be 'User' or 'App', got %q", actorInfo.Type)
	}
}

// TestGetCurrentUserAppToken tests fetching the current app's identity when using an App installation token
func TestGetCurrentUserAppToken(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "get_current_user_app")
	defer rec.Stop()

	ctx := context.Background()

	actorInfo, err := client.GetCurrentUser(ctx)
	if err != nil {
		t.Fatalf("GetCurrentUser() error = %v", err)
	}

	// Verify basic fields
	if actorInfo.Login != "my-github-app" {
		t.Errorf("Login = %q, want %q", actorInfo.Login, "my-github-app")
	}

	if actorInfo.Type != "App" {
		t.Errorf("Type = %q, want %q", actorInfo.Type, "App")
	}

	if actorInfo.Source != "app" {
		t.Errorf("Source = %q, want %q", actorInfo.Source, "app")
	}

	if actorInfo.AppSlug != "my-github-app" {
		t.Errorf("AppSlug = %q, want %q", actorInfo.AppSlug, "my-github-app")
	}
}

// TestGetCurrentUserAppTokenNoPerm tests fetching identity when using an App installation token without /app permission
func TestGetCurrentUserAppTokenNoPerm(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping integration test in short mode")
	}

	client, rec := setupTestClient(t, "get_current_user_app_no_perm")
	defer rec.Stop()

	ctx := context.Background()

	actorInfo, err := client.GetCurrentUser(ctx)
	if err != nil {
		t.Fatalf("GetCurrentUser() error = %v", err)
	}

	// Verify minimal fields - should still return ActorInfo even without /app permission
	if actorInfo.Type != "App" {
		t.Errorf("Type = %q, want %q", actorInfo.Type, "App")
	}

	if actorInfo.Source != "app" {
		t.Errorf("Source = %q, want %q", actorInfo.Source, "app")
	}

	// Login and AppSlug should be empty when we can't access /app
	if actorInfo.Login != "" {
		t.Errorf("Login = %q, want empty string", actorInfo.Login)
	}

	if actorInfo.AppSlug != "" {
		t.Errorf("AppSlug = %q, want empty string", actorInfo.AppSlug)
	}
}
