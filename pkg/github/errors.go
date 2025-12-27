package github

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
)

// APIError represents a GitHub API error response
type APIError struct {
	StatusCode int
	Message    string
	Errors     []APIErrorDetail `json:"errors,omitempty"`
	// Rate limit information when rate limited
	RateLimit *RateLimitInfo
}

// APIErrorDetail represents individual error details from GitHub
type APIErrorDetail struct {
	Resource string `json:"resource"`
	Field    string `json:"field"`
	Code     string `json:"code"`
	Message  string `json:"message"`
}

// RateLimitInfo contains rate limit information from response headers
type RateLimitInfo struct {
	Limit     int
	Remaining int
	Reset     int64 // Unix timestamp
	Used      int
}

// Error returns the error message
func (e *APIError) Error() string {
	if e.Message != "" {
		return fmt.Sprintf("GitHub API error (status %d): %s", e.StatusCode, e.Message)
	}
	return fmt.Sprintf("GitHub API error (status %d)", e.StatusCode)
}

// IsRateLimitError returns true if the error is a rate limit error
func IsRateLimitError(err error) bool {
	if apiErr, ok := err.(*APIError); ok {
		// Check for rate limit error (403 with RateLimit info, or 429)
		if apiErr.StatusCode == http.StatusTooManyRequests {
			return true
		}
		if apiErr.StatusCode == http.StatusForbidden && apiErr.RateLimit != nil {
			return true
		}
	}
	return false
}

// IsNotFoundError returns true if the error is a not found error
func IsNotFoundError(err error) bool {
	// Check our custom APIError type
	if apiErr, ok := err.(*APIError); ok {
		return apiErr.StatusCode == http.StatusNotFound
	}

	// Check go-github's ErrorResponse type by checking the error message
	// go-github returns errors with status code in the message
	if err != nil && strings.Contains(err.Error(), "404") {
		return true
	}

	return false
}

// IsAuthenticationError returns true if the error is an authentication error
func IsAuthenticationError(err error) bool {
	if apiErr, ok := err.(*APIError); ok {
		// Exclude rate limit errors (they're not auth errors)
		if IsRateLimitError(err) {
			return false
		}
		return apiErr.StatusCode == http.StatusUnauthorized ||
			apiErr.StatusCode == http.StatusForbidden
	}
	return false
}

// parseErrorResponse parses an error response from GitHub
func parseErrorResponse(statusCode int, body []byte) *APIError {
	var apiErr APIError
	apiErr.StatusCode = statusCode

	// Try to parse as GitHub error response
	var githubErr struct {
		Message string          `json:"message"`
		Errors  []APIErrorDetail `json:"errors"`
	}
	if err := json.Unmarshal(body, &githubErr); err == nil {
		apiErr.Message = githubErr.Message
		apiErr.Errors = githubErr.Errors
	} else {
		// If parsing fails, use the body as the message
		apiErr.Message = string(body)
	}

	return &apiErr
}
