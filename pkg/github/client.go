package github

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
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

	// LegacyTokenEnv is the legacy environment variable for GitHub token
	LegacyTokenEnv = "HOLON_GITHUB_TOKEN"

	// DefaultTimeout is the default HTTP timeout
	DefaultTimeout = 30 * time.Second
)

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

// NewClientFromEnv creates a new client using token from environment variables
func NewClientFromEnv(opts ...ClientOption) (*Client, error) {
	token := os.Getenv(TokenEnv)
	if token == "" {
		token = os.Getenv(LegacyTokenEnv)
	}
	if token == "" {
		return nil, fmt.Errorf("%s or %s environment variable is required", TokenEnv, LegacyTokenEnv)
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
		tc := oauth2.NewClient(ctx, ts)
		c.githubClient = github.NewClient(tc)

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
