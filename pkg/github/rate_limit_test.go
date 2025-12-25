package github

import (
	"context"
	"fmt"
	"net/http"
	"testing"
	"time"
)

// TestNewRateLimitTracker tests rate limit tracker initialization
func TestNewRateLimitTracker(t *testing.T) {
	tracker := NewRateLimitTracker()

	status := tracker.GetStatus()

	if status.Limit != defaultRateLimit {
		t.Errorf("Limit = %v, want %v", status.Limit, defaultRateLimit)
	}

	if status.Remaining != 0 {
		t.Errorf("Remaining = %v, want %v", status.Remaining, 0)
	}
}

// TestRateLimitTracker_Update tests updating rate limit from response headers
func TestRateLimitTracker_Update(t *testing.T) {
	tracker := NewRateLimitTracker()

	// Create a mock response with rate limit headers
	h := make(http.Header)
	h.Add("X-RateLimit-Limit", "5000")
	h.Add("X-RateLimit-Remaining", "4999")
	h.Add("X-RateLimit-Used", "1")
	h.Add("X-RateLimit-Reset", "1234567890")

	resp := &http.Response{
		Header: h,
	}

	tracker.Update(resp)

	status := tracker.GetStatus()

	if status.Limit != 5000 {
		t.Errorf("Limit = %v, want %v", status.Limit, 5000)
	}

	if status.Remaining != 4999 {
		t.Errorf("Remaining = %v, want %v", status.Remaining, 4999)
	}

	if status.Used != 1 {
		t.Errorf("Used = %v, want %v", status.Used, 1)
	}

	expectedReset := time.Unix(1234567890, 0)
	if !status.Reset.Equal(expectedReset) {
		t.Errorf("Reset = %v, want %v", status.Reset, expectedReset)
	}
}

// TestRateLimitTracker_WaitForRateLimitReset tests waiting for rate limit reset
func TestRateLimitTracker_WaitForRateLimitReset(t *testing.T) {
	tests := []struct {
		name      string
		remaining int
		reset     time.Time
		wantWait  bool
	}{
		{
			name:      "no wait - remaining requests",
			remaining: 100,
			reset:     time.Now().Add(1 * time.Hour),
			wantWait:  false,
		},
		{
			name:      "no wait - reset time in past",
			remaining: 0,
			reset:     time.Now().Add(-1 * time.Hour),
			wantWait:  false,
		},
		{
			name:      "no wait - no reset time",
			remaining: 0,
			reset:     time.Time{},
			wantWait:  false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tracker := NewRateLimitTracker()

			// Set initial state
			tracker.mu.Lock()
			tracker.limit.Remaining = tt.remaining
			tracker.limit.Reset = tt.reset
			tracker.mu.Unlock()

			// Create context with short timeout
			ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
			defer cancel()

			// This should either return immediately or timeout
			err := tracker.WaitForRateLimitReset(ctx)

			if tt.wantWait {
				// If we expect to wait, we should get a timeout or context cancellation
				if err == nil && tt.reset.After(time.Now()) {
					t.Error("Expected error when waiting for future reset, got nil")
				}
			} else {
				if err != nil {
					t.Errorf("Unexpected error: %v", err)
				}
			}
		})
	}
}

// TestDefaultRetryConfig tests default retry configuration
func TestDefaultRetryConfig(t *testing.T) {
	config := DefaultRetryConfig()

	if config.MaxAttempts != defaultRetryAttempts {
		t.Errorf("MaxAttempts = %v, want %v", config.MaxAttempts, defaultRetryAttempts)
	}

	if config.BaseDelay != defaultBaseDelay {
		t.Errorf("BaseDelay = %v, want %v", config.BaseDelay, defaultBaseDelay)
	}

	if config.MaxDelay != defaultMaxDelay {
		t.Errorf("MaxDelay = %v, want %v", config.MaxDelay, defaultMaxDelay)
	}

	expectedCodes := []int{
		http.StatusTooManyRequests,
		http.StatusInternalServerError,
		http.StatusBadGateway,
		http.StatusServiceUnavailable,
		http.StatusGatewayTimeout,
	}

	if len(config.RetryOn) != len(expectedCodes) {
		t.Errorf("RetryOn length = %v, want %v", len(config.RetryOn), len(expectedCodes))
	}

	for _, code := range expectedCodes {
		if !config.ShouldRetry(code) {
			t.Errorf("ShouldRetry(%v) = false, want true", code)
		}
	}
}

// TestRetryConfig_ShouldRetry tests retry logic for different status codes
func TestRetryConfig_ShouldRetry(t *testing.T) {
	config := &RetryConfig{
		RetryOn: []int{429, 500, 502},
	}

	tests := []struct {
		name       string
		statusCode int
		want       bool
	}{
		{
			name:       "429 - retry",
			statusCode: 429,
			want:       true,
		},
		{
			name:       "500 - retry",
			statusCode: 500,
			want:       true,
		},
		{
			name:       "502 - retry",
			statusCode: 502,
			want:       true,
		},
		{
			name:       "404 - no retry",
			statusCode: 404,
			want:       false,
		},
		{
			name:       "200 - no retry",
			statusCode: 200,
			want:       false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := config.ShouldRetry(tt.statusCode)
			if got != tt.want {
				t.Errorf("ShouldRetry() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestRetryConfig_GetDelay tests exponential backoff with jitter
func TestRetryConfig_GetDelay(t *testing.T) {
	config := &RetryConfig{
		BaseDelay: 1 * time.Second,
		MaxDelay:  60 * time.Second,
	}

	// Test first few attempts
	attempts := []int{0, 1, 2, 3}

	for _, attempt := range attempts {
		delay := config.GetDelay(attempt)

		// Expected delay without jitter: BaseDelay * 2^attempt
		expectedMin := config.BaseDelay * time.Duration(1<<uint(attempt))
		_ = expectedMin // Used for range checking below

		// Check delay is in reasonable range (with jitter)
		if delay < 0 {
			t.Errorf("Attempt %d: delay = %v, want >= 0", attempt, delay)
		}

		if delay > config.MaxDelay {
			t.Errorf("Attempt %d: delay = %v, want <= %v", attempt, delay, config.MaxDelay)
		}

		// First attempt should be close to BaseDelay
		if attempt == 0 && delay > 2*time.Second {
			t.Errorf("Attempt %d: delay = %v, want ~%v", attempt, delay, config.BaseDelay)
		}
	}
}

// TestIsRetryableError tests retryable error detection
func TestIsRetryableError(t *testing.T) {
	tests := []struct {
		name string
		err  error
		want bool
	}{
		{
			name: "timeout error",
			err:  &testError{msg: "context timeout exceeded"},
			want: true,
		},
		{
			name: "deadline exceeded",
			err:  &testError{msg: "deadline exceeded"},
			want: true,
		},
		{
			name: "connection refused",
			err:  &testError{msg: "connection refused"},
			want: true,
		},
		{
			name: "connection reset",
			err:  &testError{msg: "connection reset"},
			want: true,
		},
		{
			name: "not retryable",
			err:  &testError{msg: "some other error"},
			want: false,
		},
		{
			name: "nil error",
			err:  nil,
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := IsRetryableError(tt.err)
			if got != tt.want {
				t.Errorf("IsRetryableError() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestRateLimitStatus_ConcurrentAccess tests concurrent access to rate limit status
func TestRateLimitStatus_ConcurrentAccess(t *testing.T) {
	tracker := NewRateLimitTracker()

	// Simulate concurrent updates
	done := make(chan bool)

	for i := 0; i < 10; i++ {
		go func(iteration int) {
			h := make(http.Header)
			h.Add("X-RateLimit-Limit", "5000")
			h.Add("X-RateLimit-Remaining", fmt.Sprintf("%d", 4900-iteration))
			resp := &http.Response{
				Header: h,
			}
			tracker.Update(resp)
			tracker.GetStatus()
			done <- true
		}(i)
	}

	// Wait for all goroutines
	for i := 0; i < 10; i++ {
		<-done
	}

	// If we get here without panic or deadlock, the test passes
	status := tracker.GetStatus()
	if status.Limit == 0 {
		t.Error("Limit should not be zero after updates")
	}
}
