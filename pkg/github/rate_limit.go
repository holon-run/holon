package github

import (
	"context"
	"math/rand"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	// Default rate limit limits
	defaultRateLimit     = 5000
	defaultRetryAttempts = 3
	defaultBaseDelay     = 1 * time.Second
	defaultMaxDelay      = 60 * time.Second
)

// RateLimitStatus represents the current rate limit status
type RateLimitStatus struct {
	Limit     int       `json:"limit"`
	Remaining int       `json:"remaining"`
	Reset     time.Time `json:"reset"`
	Used      int       `json:"used"`
}

// RateLimitTracker tracks rate limit information from GitHub API responses
type RateLimitTracker struct {
	mu     sync.RWMutex
	limit  RateLimitStatus
}

// NewRateLimitTracker creates a new rate limit tracker
func NewRateLimitTracker() *RateLimitTracker {
	return &RateLimitTracker{
		limit: RateLimitStatus{
			Limit: defaultRateLimit,
		},
	}
}

// Update updates the rate limit status from HTTP response headers
func (r *RateLimitTracker) Update(resp *http.Response) {
	r.mu.Lock()
	defer r.mu.Unlock()

	// Parse rate limit headers
	if limit := resp.Header.Get("X-RateLimit-Limit"); limit != "" {
		if val, err := strconv.Atoi(limit); err == nil {
			r.limit.Limit = val
		}
	}

	if remaining := resp.Header.Get("X-RateLimit-Remaining"); remaining != "" {
		if val, err := strconv.Atoi(remaining); err == nil {
			r.limit.Remaining = val
		}
	}

	if reset := resp.Header.Get("X-RateLimit-Reset"); reset != "" {
		if val, err := strconv.ParseInt(reset, 10, 64); err == nil {
			r.limit.Reset = time.Unix(val, 0)
		}
	}

	if used := resp.Header.Get("X-RateLimit-Used"); used != "" {
		if val, err := strconv.Atoi(used); err == nil {
			r.limit.Used = val
		}
	}
}

// GetStatus returns a copy of the current rate limit status
func (r *RateLimitTracker) GetStatus() RateLimitStatus {
	r.mu.RLock()
	defer r.mu.RUnlock()

	return RateLimitStatus{
		Limit:     r.limit.Limit,
		Remaining: r.limit.Remaining,
		Reset:     r.limit.Reset,
		Used:      r.limit.Used,
	}
}

// WaitForRateLimitReset waits until the rate limit resets if necessary
func (r *RateLimitTracker) WaitForRateLimitReset(ctx context.Context) error {
	r.mu.RLock()
	reset := r.limit.Reset
	remaining := r.limit.Remaining
	r.mu.RUnlock()

	// If we have remaining requests, no need to wait
	if remaining > 0 {
		return nil
	}

	// If no reset time, we can't wait
	if reset.IsZero() {
		return nil
	}

	// Calculate wait duration
	now := time.Now()
	if reset.Before(now) {
		// Already reset, no need to wait
		return nil
	}

	waitDuration := reset.Sub(now)
	if waitDuration <= 0 {
		return nil
	}

	// Wait or check context
	select {
	case <-time.After(waitDuration):
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// RetryConfig defines retry behavior for failed requests
type RetryConfig struct {
	MaxAttempts int           // Maximum number of retry attempts
	BaseDelay    time.Duration // Base delay between retries
	MaxDelay     time.Duration // Maximum delay between retries
	RetryOn      []int         // HTTP status codes to retry on
}

// DefaultRetryConfig returns the default retry configuration
func DefaultRetryConfig() *RetryConfig {
	return &RetryConfig{
		MaxAttempts: defaultRetryAttempts,
		BaseDelay:   defaultBaseDelay,
		MaxDelay:    defaultMaxDelay,
		RetryOn: []int{
			http.StatusTooManyRequests,                // 429
			http.StatusInternalServerError,             // 500
			http.StatusBadGateway,                     // 502
			http.StatusServiceUnavailable,             // 503
			http.StatusGatewayTimeout,                 // 504
		},
	}
}

// ShouldRetry returns true if the request should be retried based on the status code
func (rc *RetryConfig) ShouldRetry(statusCode int) bool {
	for _, code := range rc.RetryOn {
		if code == statusCode {
			return true
		}
	}
	return false
}

// GetDelay calculates the delay for a given retry attempt with exponential backoff
func (rc *RetryConfig) GetDelay(attempt int) time.Duration {
	// Exponential backoff with jitter
	delay := rc.BaseDelay * time.Duration(1<<uint(attempt))

	// Add jitter to avoid thundering herd (±10%)
	jitter := time.Duration(float64(delay) * 0.1 * (rand.Float64()*2 - 1))
	delay += jitter

	// Ensure non-negative and cap at max delay
	if delay < 0 {
		delay = rc.BaseDelay
	}
	if delay > rc.MaxDelay {
		delay = rc.MaxDelay
	}

	return delay
}

// IsRetryableError checks if an error is retryable
func IsRetryableError(err error) bool {
	if err == nil {
		return false
	}

	// Check if it's a timeout error
	if strings.Contains(err.Error(), "timeout") ||
		strings.Contains(err.Error(), "deadline exceeded") {
		return true
	}

	// Check if it's a connection error
	if strings.Contains(err.Error(), "connection refused") ||
		strings.Contains(err.Error(), "connection reset") {
		return true
	}

	return false
}
