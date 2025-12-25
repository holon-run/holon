package github

import (
	"net/http"
	"testing"
)

// TestAPIError_Error tests error message formatting
func TestAPIError_Error(t *testing.T) {
	tests := []struct {
		name    string
		err     *APIError
		wantMsg string
	}{
		{
			name: "error with message",
			err: &APIError{
				StatusCode: 404,
				Message:    "Not found",
			},
			wantMsg: "GitHub API error (status 404): Not found",
		},
		{
			name: "error without message",
			err: &APIError{
				StatusCode: 500,
			},
			wantMsg: "GitHub API error (status 500)",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.err.Error()
			if got != tt.wantMsg {
				t.Errorf("APIError.Error() = %v, want %v", got, tt.wantMsg)
			}
		})
	}
}

// TestIsRateLimitError tests rate limit error detection
func TestIsRateLimitError(t *testing.T) {
	tests := []struct {
		name string
		err  error
		want bool
	}{
		{
			name: "429 too many requests",
			err: &APIError{
				StatusCode: http.StatusTooManyRequests,
			},
			want: true,
		},
		{
			name: "403 with rate limit info",
			err: &APIError{
				StatusCode: http.StatusForbidden,
				RateLimit: &RateLimitInfo{
					Limit:     5000,
					Remaining: 0,
					Reset:     1234567890,
				},
			},
			want: true,
		},
		{
			name: "403 without rate limit info",
			err: &APIError{
				StatusCode: http.StatusForbidden,
			},
			want: false,
		},
		{
			name: "404 not found",
			err: &APIError{
				StatusCode: http.StatusNotFound,
			},
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
			got := IsRateLimitError(tt.err)
			if got != tt.want {
				t.Errorf("IsRateLimitError() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestIsNotFoundError tests not found error detection
func TestIsNotFoundError(t *testing.T) {
	tests := []struct {
		name string
		err  error
		want bool
	}{
		{
			name: "404 not found",
			err: &APIError{
				StatusCode: http.StatusNotFound,
			},
			want: true,
		},
		{
			name: "403 forbidden",
			err: &APIError{
				StatusCode: http.StatusForbidden,
			},
			want: false,
		},
		{
			name: "nil error",
			err:  nil,
			want: false,
		},
		{
			name: "non-APIError",
			err:  &testError{msg: "not an API error"},
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := IsNotFoundError(tt.err)
			if got != tt.want {
				t.Errorf("IsNotFoundError() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestIsAuthenticationError tests authentication error detection
func TestIsAuthenticationError(t *testing.T) {
	tests := []struct {
		name string
		err  error
		want bool
	}{
		{
			name: "401 unauthorized",
			err: &APIError{
				StatusCode: http.StatusUnauthorized,
			},
			want: true,
		},
		{
			name: "403 forbidden without rate limit",
			err: &APIError{
				StatusCode: http.StatusForbidden,
			},
			want: true,
		},
		{
			name: "403 with rate limit info",
			err: &APIError{
				StatusCode: http.StatusForbidden,
				RateLimit: &RateLimitInfo{
					Limit:     5000,
					Remaining: 0,
				},
			},
			want: false,
		},
		{
			name: "404 not found",
			err: &APIError{
				StatusCode: http.StatusNotFound,
			},
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
			got := IsAuthenticationError(tt.err)
			if got != tt.want {
				t.Errorf("IsAuthenticationError() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestParseErrorResponse tests error response parsing
func TestParseErrorResponse(t *testing.T) {
	tests := []struct {
		name       string
		statusCode int
		body       []byte
		wantMsg    string
		wantErrors bool
	}{
		{
			name:       "GitHub error response",
			statusCode: 422,
			body:       []byte(`{"message":"Validation failed","errors":[{"resource":"Issue","field":"title","code":"missing"}]}`),
			wantMsg:    "Validation failed",
			wantErrors: true,
		},
		{
			name:       "plain text error",
			statusCode: 500,
			body:       []byte("Internal server error"),
			wantMsg:    "Internal server error",
			wantErrors: false,
		},
		{
			name:       "empty body",
			statusCode: 503,
			body:       []byte(""),
			wantMsg:    "",
			wantErrors: false,
		},
		{
			name:       "invalid JSON",
			statusCode: 400,
			body:       []byte("not json"),
			wantMsg:    "not json",
			wantErrors: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := parseErrorResponse(tt.statusCode, tt.body)

			if got.StatusCode != tt.statusCode {
				t.Errorf("StatusCode = %v, want %v", got.StatusCode, tt.statusCode)
			}

			if got.Message != tt.wantMsg {
				t.Errorf("Message = %v, want %v", got.Message, tt.wantMsg)
			}

			if tt.wantErrors && len(got.Errors) == 0 {
				t.Error("Expected errors to be parsed")
			}
		})
	}
}

// testError is a helper for testing error detection
type testError struct {
	msg string
}

func (e *testError) Error() string {
	return e.msg
}
