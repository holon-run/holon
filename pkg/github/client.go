package github

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/google/go-github/v68/github"
	"golang.org/x/oauth2"
)

const (
	// DefaultBaseURL is the default GitHub API base URL
	DefaultBaseURL = "https://api.github.com"

	// TokenEnv is the environment variable for GitHub token
	TokenEnv = "GITHUB_TOKEN"

	// HolonTokenEnv is the environment variable for Holon-specific GitHub token
	// This has higher priority than GITHUB_TOKEN to allow overriding CI's GITHUB_TOKEN
	// with a token that has higher permissions (e.g., holonbot app token).
	HolonTokenEnv = "HOLON_GITHUB_TOKEN"

	// DefaultTimeout is the default HTTP timeout
	DefaultTimeout = 30 * time.Second
)

// ghAuthToken retrieves the GitHub token from the gh CLI.
// It runs `gh auth token` and returns the token output.
// Returns an empty string if gh is not available or the command fails.
func ghAuthToken() string {
	// Check if gh command exists
	_, err := exec.LookPath("gh")
	if err != nil {
		return ""
	}

	// Run gh auth token
	cmd := exec.Command("gh", "auth", "token")
	output, err := cmd.Output()
	if err != nil {
		// gh auth token failed (likely not authenticated)
		return ""
	}

	// Trim whitespace and return
	token := strings.TrimSpace(string(output))
	return token
}

// GetTokenFromEnv retrieves a GitHub token from environment variables or gh CLI.
// Priority order (highest to lowest):
// 1. HOLON_GITHUB_TOKEN - Holon-specific token (allows overriding CI's GITHUB_TOKEN)
// 2. GITHUB_TOKEN - standard GitHub token (automatically set in CI environments)
// 3. gh auth token - fallback to gh CLI token
//
// Returns the token and a boolean indicating whether the token came from gh CLI.
func GetTokenFromEnv() (string, bool) {
	// Check HOLON_GITHUB_TOKEN first (highest priority)
	// This allows overriding the CI's GITHUB_TOKEN with a higher-permission token
	token := os.Getenv(HolonTokenEnv)
	if token != "" {
		return token, false
	}

	// Check standard GITHUB_TOKEN (automatically set in GitHub Actions CI)
	token = os.Getenv(TokenEnv)
	if token != "" {
		return token, false
	}

	// Fallback to gh CLI
	token = ghAuthToken()
	if token != "" {
		return token, true
	}

	return "", false
}

// ClientOption configures a Client
type ClientOption func(*Client)

// WithBaseURL sets a custom base URL for the GitHub API
func WithBaseURL(baseURL string) ClientOption {
	return func(c *Client) {
		c.baseURL = baseURL
	}
}

// WithTimeout sets a custom HTTP timeout
func WithTimeout(timeout time.Duration) ClientOption {
	return func(c *Client) {
		c.timeout = timeout
	}
}

// WithHTTPClient sets a custom HTTP client
func WithHTTPClient(client *http.Client) ClientOption {
	return func(c *Client) {
		c.httpClient = client
	}
}

// WithRateLimitTracking enables rate limit tracking
func WithRateLimitTracking(enabled bool) ClientOption {
	return func(c *Client) {
		if enabled && c.rateLimitTracker == nil {
			c.rateLimitTracker = NewRateLimitTracker()
		}
	}
}

// WithRetryConfig configures retry behavior
func WithRetryConfig(config *RetryConfig) ClientOption {
	return func(c *Client) {
		c.retryConfig = config
	}
}

// Client is a unified GitHub API client that supports both direct HTTP and go-github.
//
// The client provides:
// - Direct HTTP access via NewRequest/Do methods
// - Lazy-loaded go-github client via GitHubClient() for advanced operations
// - Automatic rate limit tracking (when enabled via WithRateLimitTracking)
// - Retry logic with exponential backoff (when configured via WithRetryConfig)
//
// Example:
//
//	client := github.NewClient(token,
//	    github.WithRateLimitTracking(true),
//	    github.WithRetryConfig(github.DefaultRetryConfig()),
//	)
type Client struct {
	token             string
	baseURL           string
	httpClient        *http.Client
	timeout           time.Duration
	githubClient      *github.Client // Lazy-loaded go-github client
	rateLimitTracker  *RateLimitTracker
	retryConfig       *RetryConfig
}

// NewClient creates a new GitHub API client with the given token
func NewClient(token string, opts ...ClientOption) *Client {
	c := &Client{
		token:    token,
		baseURL:  DefaultBaseURL,
		timeout:  DefaultTimeout,
		httpClient: &http.Client{
			Timeout: DefaultTimeout,
		},
	}

	for _, opt := range opts {
		opt(c)
	}

	// Update HTTP client timeout if set
	c.httpClient.Timeout = c.timeout

	return c
}

// NewClientFromEnv creates a new client using token from environment variables or gh CLI.
// It checks HOLON_GITHUB_TOKEN and GITHUB_TOKEN environment variables first.
// If those are empty, it attempts to use `gh auth token` as a fallback.
func NewClientFromEnv(opts ...ClientOption) (*Client, error) {
	token, fromGh := GetTokenFromEnv()
	if token == "" {
		return nil, fmt.Errorf("%s or %s environment variable is required (or use 'gh auth login')", TokenEnv, HolonTokenEnv)
	}

	if fromGh {
		// Log that we're using gh CLI for authentication (without exposing the token)
		fmt.Fprintln(os.Stderr, "Using GitHub token from 'gh auth token' (GITHUB_TOKEN not set)")
	}

	return NewClient(token, opts...), nil
}

// GetToken returns the client's authentication token
func (c *Client) GetToken() string {
	return c.token
}

// SetToken updates the client's authentication token
func (c *Client) SetToken(token string) {
	c.token = token
	// Invalidate cached github client
	c.githubClient = nil
}

// SetBaseURL updates the base URL for the GitHub API
func (c *Client) SetBaseURL(baseURL string) {
	c.baseURL = baseURL
	// Invalidate cached github client
	c.githubClient = nil
}

// GetRateLimitStatus returns the current rate limit status
func (c *Client) GetRateLimitStatus() (RateLimitStatus, error) {
	if c.rateLimitTracker == nil {
		return RateLimitStatus{}, nil
	}
	return c.rateLimitTracker.GetStatus(), nil
}

// GitHubClient returns the underlying go-github client (lazy-loaded)
func (c *Client) GitHubClient() *github.Client {
	if c.githubClient == nil {
		ctx := context.Background()
		ts := oauth2.StaticTokenSource(&oauth2.Token{AccessToken: c.token})

		// Use custom HTTP client if provided (e.g., for VCR recording)
		// We need to wrap the oauth2 transport with the custom client's transport
		if c.httpClient != nil && c.httpClient.Transport != nil {
			// Create oauth2 transport that uses the custom transport (e.g., VCR recorder)
			tc := &http.Client{
				Transport: &oauth2.Transport{
					Source: ts,
					Base:   c.httpClient.Transport,
				},
				Timeout: c.httpClient.Timeout,
			}
			c.githubClient = github.NewClient(tc)
		} else {
			// Create default oauth2 client
			tc := oauth2.NewClient(ctx, ts)
			c.githubClient = github.NewClient(tc)
		}

		// Set custom base URL if configured (for testing)
		if c.baseURL != DefaultBaseURL && c.baseURL != "" {
			baseURL := c.baseURL
			// Ensure trailing slash for go-github (only if len > 0 to avoid index out of bounds)
			if len(baseURL) > 0 && baseURL[len(baseURL)-1] != '/' {
				baseURL += "/"
			}
			parsedURL, err := url.Parse(baseURL)
			if err != nil {
				// Log the error but don't fail - the client will use default base URL
				// In a testing context, an invalid URL should be caught early
				baseURL = DefaultBaseURL
				parsedURL, _ = url.Parse(baseURL)
			}
			c.githubClient.BaseURL = parsedURL
		}
	}
	return c.githubClient
}

// NewRequest creates a new HTTP request with proper authentication
func (c *Client) NewRequest(ctx context.Context, method, url string, body io.Reader) (*http.Request, error) {
	req, err := http.NewRequestWithContext(ctx, method, url, body)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	c.setHeaders(req)
	return req, nil
}

// Do sends an HTTP request and returns the response
func (c *Client) Do(req *http.Request, result interface{}) (*ClientResponse, error) {
	var lastErr error

	// Check for rate limiting before request
	if c.rateLimitTracker != nil {
		if err := c.rateLimitTracker.WaitForRateLimitReset(req.Context()); err != nil {
			return nil, fmt.Errorf("rate limit wait failed: %w", err)
		}
	}

	// Retry logic
	maxAttempts := 1
	if c.retryConfig != nil {
		maxAttempts = c.retryConfig.MaxAttempts
	}

	for attempt := 0; attempt < maxAttempts; attempt++ {
		if attempt > 0 {
			// Wait before retry
			delay := c.retryConfig.GetDelay(attempt - 1)
			select {
			case <-time.After(delay):
			case <-req.Context().Done():
				return nil, req.Context().Err()
			}
		}

		resp, err := c.httpClient.Do(req)
		if err != nil {
			lastErr = err

			// Check if we should retry
			if c.retryConfig != nil && IsRetryableError(err) && attempt < maxAttempts-1 {
				continue
			}

			return nil, fmt.Errorf("request failed: %w", err)
		}

		// Update rate limit tracking
		if c.rateLimitTracker != nil {
			c.rateLimitTracker.Update(resp)
		}

		// Handle error responses
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			body, _ := io.ReadAll(resp.Body)
			resp.Body.Close()

			apiErr := parseErrorResponse(resp.StatusCode, body)

			// Check for rate limiting
			if resp.StatusCode == http.StatusForbidden && resp.Header.Get("X-RateLimit-Remaining") == "0" {
				// Extract rate limit info
				if reset := resp.Header.Get("X-RateLimit-Reset"); reset != "" {
					if resetInt, err := strconv.ParseInt(reset, 10, 64); err == nil {
						limit := 0
						if c.rateLimitTracker != nil {
							limit = c.rateLimitTracker.GetStatus().Limit
						}
						apiErr.RateLimit = &RateLimitInfo{
							Limit:     limit,
							Remaining: 0,
							Reset:     resetInt,
						}
					}
				}
			}

			// Check if we should retry
			if c.retryConfig != nil && c.retryConfig.ShouldRetry(resp.StatusCode) && attempt < maxAttempts-1 {
				continue
			}

			return nil, apiErr
		}

		// Wrap response
		clientResp := &ClientResponse{
			Response: resp,
			client:   c,
		}

		// Decode result if provided
		if result != nil {
			if err := clientResp.DecodeJSON(result); err != nil {
				return nil, fmt.Errorf("failed to decode response: %w", err)
			}
		}

		return clientResp, nil
	}

	return nil, fmt.Errorf("max retries exceeded: %w", lastErr)
}

// setHeaders sets common headers for GitHub API requests
func (c *Client) setHeaders(req *http.Request) {
	req.Header.Set("Accept", "application/vnd.github.v3+json")
	if c.token != "" {
		req.Header.Set("Authorization", "token "+c.token)
	}
}

// ClientResponse wraps an HTTP response with additional methods
type ClientResponse struct {
	*http.Response
	client    *Client
	closed    bool  // Track if body was already closed
	closeOnce sync.Once
}

// DecodeJSON decodes the response body as JSON
func (r *ClientResponse) DecodeJSON(v interface{}) error {
	defer r.Close()
	return json.NewDecoder(r.Response.Body).Decode(v)
}

// ReadAll reads the entire response body
func (r *ClientResponse) ReadAll() ([]byte, error) {
	defer r.Close()
	return io.ReadAll(r.Response.Body)
}

// Close closes the response body (idempotent)
func (r *ClientResponse) Close() error {
	var err error
	r.closeOnce.Do(func() {
		if r.Response != nil && r.Response.Body != nil {
			err = r.Response.Body.Close()
			r.closed = true
		}
	})
	return err
}
