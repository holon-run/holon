package redact

import (
	"os"
	"strings"
	"testing"
)

func TestRedactEnvKeyValues(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		contains string
		mode     Mode
	}{
		{
			name:     "redact API key",
			input:    "API_KEY=sk_test_12345",
			contains: "***REDACTED***",
			mode:     ModeBasic,
		},
		{
			name:     "redact token",
			input:    "AUTH_TOKEN=ghp_1234567890abcdef",
			contains: "***REDACTED***",
			mode:     ModeBasic,
		},
		{
			name:     "redact password",
			input:    "PASSWORD=secret123",
			contains: "***REDACTED***",
			mode:     ModeBasic,
		},
		{
			name:     "redact quoted value",
			input:    `MY_SECRET="super_secret_value"`,
			contains: "***REDACTED***",
			mode:     ModeBasic,
		},
		{
			name:     "redact custom key",
			input:    "MY_CUSTOM_KEY=value123",
			contains: "***REDACTED***",
			mode:     ModeBasic,
		},
		{
			name:     "no redaction in off mode",
			input:    "API_KEY=sk_test_12345",
			contains: "sk_test_12345",
			mode:     ModeOff,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := New(Config{
				Mode:       tt.mode,
				CustomKeys: "MY_CUSTOM_KEY",
			})
			result, _ := r.Redact([]byte(tt.input))
			resultStr := string(result)
			if !strings.Contains(resultStr, tt.contains) {
				t.Errorf("Expected result to contain %q, got %q", tt.contains, resultStr)
			}
		})
	}
}

func TestRedactHTTPHeaderValues(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		mode     Mode
		contains string
		exact    string
	}{
		{
			name:     "redact Authorization header",
			input:    "Authorization: Bearer ghp_1234567890",
			mode:     ModeBasic,
			contains: "***REDACTED***",
		},
		{
			name:     "redact X-API-Key header",
			input:    "X-API-Key: sk_test_12345",
			mode:     ModeBasic,
			contains: "***REDACTED***",
		},
		{
			name:     "redact Cookie header",
			input:    "Cookie: session=abc123; token=xyz789",
			mode:     ModeBasic,
			contains: "***REDACTED***",
		},
		{
			name:     "preserve lowercase header name",
			input:    "authorization: Bearer ghp_1234567890",
			mode:     ModeBasic,
			contains: "***REDACTED***",
			exact:    "authorization: ***REDACTED***",
		},
		{
			name:     "no redaction in off mode",
			input:    "Authorization: Bearer ghp_1234567890",
			mode:     ModeOff,
			contains: "ghp_1234567890",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := New(Config{Mode: tt.mode})
			result, _ := r.Redact([]byte(tt.input))
			resultStr := string(result)
			if !strings.Contains(resultStr, tt.contains) {
				t.Errorf("Expected result to contain %q, got %q", tt.contains, resultStr)
			}
			if tt.exact != "" && !strings.Contains(resultStr, tt.exact) {
				t.Errorf("Expected result to contain exact header %q, got %q", tt.exact, resultStr)
			}
		})
	}
}

func TestRedactURLQueryParams(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		mode     Mode
		contains string
	}{
		{
			name:     "redact token param",
			input:    "https://api.example.com?token=secret123",
			mode:     ModeBasic,
			contains: "***REDACTED***",
		},
		{
			name:     "redact api_key param",
			input:    "https://api.example.com?api_key=sk_test_12345",
			mode:     ModeBasic,
			contains: "***REDACTED***",
		},
		{
			name:     "redact multiple params",
			input:    "https://api.example.com?token=abc&other=value",
			mode:     ModeBasic,
			contains: "***REDACTED***",
		},
		{
			name:     "no redaction in off mode",
			input:    "https://api.example.com?token=secret123",
			mode:     ModeOff,
			contains: "secret123",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := New(Config{Mode: tt.mode})
			result, _ := r.Redact([]byte(tt.input))
			resultStr := string(result)
			if !strings.Contains(resultStr, tt.contains) {
				t.Errorf("Expected result to contain %q, got %q", tt.contains, resultStr)
			}
		})
	}
}

func TestRedactPEMBlocks(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		mode     Mode
		contains string
	}{
		{
			name: "redact RSA private key",
			input: `-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA2a2j9z8/lYmN3g8x9lKLKR...
-----END RSA PRIVATE KEY-----`,
			mode:     ModeBasic,
			contains: "REDACTED",
		},
		{
			name: "redact certificate",
			input: `-----BEGIN CERTIFICATE-----
MIIDXTCCAkWgAwIBAgIJAKL0UG+mRKqzMA0GCSqGSIb3DQEBCwUAMEU...
-----END CERTIFICATE-----`,
			mode:     ModeBasic,
			contains: "REDACTED",
		},
		{
			name: "no redaction in off mode",
			input: `-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA2a2j9z8/lYmN3g8x9lKLKR...
-----END RSA PRIVATE KEY-----`,
			mode:     ModeOff,
			contains: "RSA PRIVATE KEY",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := New(Config{Mode: tt.mode})
			result, _ := r.Redact([]byte(tt.input))
			resultStr := string(result)
			if !strings.Contains(resultStr, tt.contains) {
				t.Errorf("Expected result to contain %q, got %q", tt.contains, resultStr)
			}
		})
	}
}

func TestRedactKnownPrefixes(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		mode     Mode
		contains string
	}{
		{
			name:     "redact Stripe key",
			input:    "sk_live_51MzN7hLkdJ8hN2vP3nR4tG7fK9jL0mN2",
			mode:     ModeAggressive,
			contains: "REDACTED",
		},
		{
			name:     "redact Anthropic key",
			input:    "anthropic_1234567890abcdefghijklmnopqrstuvwxyz",
			mode:     ModeAggressive,
			contains: "REDACTED",
		},
		{
			name:     "redact AWS access key",
			input:    "AKIAIOSFODNN7EXAMPLE",
			mode:     ModeAggressive,
			contains: "REDACTED",
		},
		{
			name:     "no redaction in basic mode",
			input:    "sk_live_51MzN7hLkdJ8hN2vP3nR4tG7fK9jL0mN2",
			mode:     ModeBasic,
			contains: "sk_live_",
		},
		{
			name:     "no redaction in off mode",
			input:    "sk_live_51MzN7hLkdJ8hN2vP3nR4tG7fK9jL0mN2",
			mode:     ModeOff,
			contains: "sk_live_",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := New(Config{Mode: tt.mode})
			result := r.redactKnownPrefixes(tt.input)
			if !strings.Contains(result, tt.contains) {
				t.Errorf("Expected result to contain %q, got %q", tt.contains, result)
			}
		})
	}
}

func TestRedact(t *testing.T) {
	tests := []struct {
		name        string
		input       string
		mode        Mode
		mustContain []string
		mustNotContain []string
	}{
		{
			name: "comprehensive redaction in basic mode",
			input: `API_KEY=sk_test_12345
Authorization: Bearer ghp_token123
https://api.example.com?token=secret
Normal text should remain`,
			mode:     ModeBasic,
			mustContain: []string{"Normal text should remain"},
			mustNotContain: []string{"sk_test_12345", "ghp_token123"},
		},
		{
			name: "aggressive mode redacts known prefixes",
			input: `Stripe key: sk_live_51MzN7hLkdJ8hN2vP3nR4tG7fK9jL0mN2
Normal text stays`,
			mode:     ModeAggressive,
			mustContain: []string{"Normal text stays", "REDACTED"},
			mustNotContain: []string{"sk_live_51MzN7hLkdJ8hN2vP3nR4tG7fK9jL0mN2"},
		},
		{
			name: "off mode doesn't redact",
			input: `API_KEY=sk_test_12345
Authorization: Bearer ghp_token123`,
			mode:     ModeOff,
			mustContain: []string{"sk_test_12345", "ghp_token123"},
			mustNotContain: []string{"REDACTED"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := New(Config{Mode: tt.mode})
			result, err := r.Redact([]byte(tt.input))
			if err != nil {
				t.Fatalf("Redact() error = %v", err)
			}

			resultStr := string(result)

			for _, s := range tt.mustContain {
				if !strings.Contains(resultStr, s) {
					t.Errorf("Expected result to contain %q, got %q", s, resultStr)
				}
			}

			for _, s := range tt.mustNotContain {
				if strings.Contains(resultStr, s) {
					t.Errorf("Expected result not to contain %q, got %q", s, resultStr)
				}
			}
		})
	}
}

func TestIsHighEntropy(t *testing.T) {
	r := New(Config{Mode: ModeAggressive})

	tests := []struct {
		name     string
		input    string
		expected bool
	}{
		{
			name:     "random string is high entropy",
			input:    "aB3xY9mK2pL7qR4tN8vZ1wX5cD6",
			expected: true,
		},
		{
			name:     "natural language is low entropy",
			input:    "hello world this is a test",
			expected: false,
		},
		{
			name:     "repeated pattern is low entropy",
			input:    "abcabcabcabcabcabcabc",
			expected: false,
		},
		{
			name:     "short string is not checked",
			input:    "short",
			expected: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := r.isHighEntropy(tt.input)
			if result != tt.expected {
				t.Errorf("isHighEntropy(%q) = %v, want %v", tt.input, result, tt.expected)
			}
		})
	}
}

func TestIsLikelyFalsePositive(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		expected bool
	}{
		{
			name:     "path is false positive",
			input:    "/path/to/some/file.txt",
			expected: true,
		},
		{
			name:     "URL is false positive",
			input:    "https://example.com/path",
			expected: true,
		},
		{
			name:     "random token is not false positive",
			input:    "aB3xY9mK2pL7qR4tN8vZ1wX5cD6",
			expected: false,
		},
		{
			name:     "lowercase word is false positive",
			input:    "hello",
			expected: true,
		},
		{
			name:     "uppercase acronym is false positive",
			input:    "API",
			expected: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := isLikelyFalsePositive(tt.input)
			if result != tt.expected {
				t.Errorf("isLikelyFalsePositive(%q) = %v, want %v", tt.input, result, tt.expected)
			}
		})
	}
}

func TestRedactFromEnv(t *testing.T) {
	tests := []struct {
		name          string
		envMode       string
		envKeys       string
		envReplacement string
		input         string
		expectedMode  Mode
	}{
		{
			name:          "basic mode from env",
			envMode:       "basic",
			envKeys:       "",
			envReplacement: "",
			input:         "API_KEY=secret123",
			expectedMode:  ModeBasic,
		},
		{
			name:          "aggressive mode from env",
			envMode:       "aggressive",
			envKeys:       "",
			envReplacement: "",
			input:         "API_KEY=secret123",
			expectedMode:  ModeAggressive,
		},
		{
			name:          "off mode from env",
			envMode:       "off",
			envKeys:       "",
			envReplacement: "",
			input:         "API_KEY=secret123",
			expectedMode:  ModeOff,
		},
		{
			name:          "custom keys from env",
			envMode:       "basic",
			envKeys:       "MY_KEY,ANOTHER_KEY",
			envReplacement: "",
			input:         "MY_KEY=value123",
			expectedMode:  ModeBasic,
		},
		{
			name:          "custom replacement from env",
			envMode:       "basic",
			envKeys:       "",
			envReplacement: "[HIDDEN]",
			input:         "API_KEY=secret123",
			expectedMode:  ModeBasic,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Set env vars
			if tt.envMode != "" {
				os.Setenv("HOLON_LOG_REDACT", tt.envMode)
				defer os.Unsetenv("HOLON_LOG_REDACT")
			}
			if tt.envKeys != "" {
				os.Setenv("HOLON_LOG_REDACT_KEYS", tt.envKeys)
				defer os.Unsetenv("HOLON_LOG_REDACT_KEYS")
			}
			if tt.envReplacement != "" {
				os.Setenv("HOLON_LOG_REDACT_REPLACEMENT", tt.envReplacement)
				defer os.Unsetenv("HOLON_LOG_REDACT_REPLACEMENT")
			}

			r := RedactFromEnv()
			if r.mode != tt.expectedMode {
				t.Errorf("RedactFromEnv() mode = %v, want %v", r.mode, tt.expectedMode)
			}
		})
	}
}

func TestIsCI(t *testing.T) {
	// Save original env
	origCI := os.Getenv("CI")
	origGitHubActions := os.Getenv("GITHUB_ACTIONS")
	defer func() {
		if origCI != "" {
			os.Setenv("CI", origCI)
		} else {
			os.Unsetenv("CI")
		}
		if origGitHubActions != "" {
			os.Setenv("GITHUB_ACTIONS", origGitHubActions)
		} else {
			os.Unsetenv("GITHUB_ACTIONS")
		}
	}()

	// Test CI detection
	t.Run("detects GitHub Actions", func(t *testing.T) {
		os.Setenv("GITHUB_ACTIONS", "true")
		if !isCI() {
			t.Error("Expected isCI() to return true in GitHub Actions")
		}
	})

	t.Run("detects generic CI", func(t *testing.T) {
		os.Unsetenv("GITHUB_ACTIONS")
		os.Setenv("CI", "true")
		if !isCI() {
			t.Error("Expected isCI() to return true with CI=true")
		}
	})

	t.Run("returns false when not in CI", func(t *testing.T) {
		os.Unsetenv("CI")
		os.Unsetenv("GITHUB_ACTIONS")
		if isCI() {
			t.Error("Expected isCI() to return false when not in CI")
		}
	})
}

func TestRedactFile(t *testing.T) {
	// Create temp file with content
	tmpDir := t.TempDir()
	testFile := tmpDir + "/test.log"
	content := "API_KEY=sk_test_12345\nPASSWORD=secret"

	err := os.WriteFile(testFile, []byte(content), 0644)
	if err != nil {
		t.Fatalf("Failed to write test file: %v", err)
	}

	r := New(Config{Mode: ModeBasic})
	err = r.RedactFile(testFile)
	if err != nil {
		t.Fatalf("RedactFile() error = %v", err)
	}

	// Read back and check
	result, err := os.ReadFile(testFile)
	if err != nil {
		t.Fatalf("Failed to read redacted file: %v", err)
	}

	resultStr := string(result)
	if strings.Contains(resultStr, "sk_test_12345") {
		t.Error("Expected API key to be redacted")
	}
	if strings.Contains(resultStr, "secret") {
		t.Error("Expected password to be redacted")
	}
	if !strings.Contains(resultStr, "***REDACTED***") {
		t.Error("Expected redaction marker in result")
	}
}
