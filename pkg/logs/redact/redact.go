// Package redact provides log redaction functionality to prevent secret leakage in CI logs.
package redact

import (
	"fmt"
	"math"
	"os"
	"regexp"
	"strings"
)

// Mode represents the redaction mode.
type Mode string

const (
	// ModeOff disables redaction.
	ModeOff Mode = "off"
	// ModeBasic enables basic redaction (default in CI).
	ModeBasic Mode = "basic"
	// ModeAggressive enables aggressive redaction with high-entropy detection.
	ModeAggressive Mode = "aggressive"

	// minEntropyCandidateLen is the minimum token length considered for entropy-based redaction.
	minEntropyCandidateLen = 20
)

// DefaultMode is the default redaction mode when running in CI.
// It's "basic" when CI is detected, "off" otherwise.
var DefaultMode = func() Mode {
	if isCI() {
		return ModeBasic
	}
	return ModeOff
}()

// Redactor handles log redaction.
type Redactor struct {
	mode           Mode
	customKeys     []string
	replacement    string
}

// Config holds configuration for a Redactor.
type Config struct {
	Mode        Mode   // Redaction mode: off, basic, aggressive
	CustomKeys  string // Comma-separated list of custom key patterns (e.g., "MY_API_KEY,SECRET_TOKEN")
	Replacement string // Replacement string (default: "***REDACTED***")
}

// New creates a new Redactor with the given configuration.
func New(cfg Config) *Redactor {
	mode := cfg.Mode
	if mode == "" {
		mode = DefaultMode
	}

	replacement := cfg.Replacement
	if replacement == "" {
		replacement = "***REDACTED***"
	}

	customKeys := []string{}
	if cfg.CustomKeys != "" {
		for _, key := range strings.Split(cfg.CustomKeys, ",") {
			key = strings.TrimSpace(key)
			if key != "" {
				customKeys = append(customKeys, key)
			}
		}
	}

	return &Redactor{
		mode:        mode,
		customKeys:  customKeys,
		replacement: replacement,
	}
}

// RedactFile reads a file, redacts sensitive content, and writes it back.
func (r *Redactor) RedactFile(path string) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("failed to read file %s: %w", path, err)
	}

	redacted, err := r.Redact(data)
	if err != nil {
		return fmt.Errorf("failed to redact content from %s: %w", path, err)
	}

	if err := os.WriteFile(path, redacted, 0644); err != nil {
		return fmt.Errorf("failed to write redacted content to %s: %w", path, err)
	}

	return nil
}

// Redact redacts sensitive content from the input data.
func (r *Redactor) Redact(data []byte) ([]byte, error) {
	if r.mode == ModeOff {
		return data, nil
	}

	content := string(data)
	var err error

	// Apply all redaction passes
	content = r.redactPEMBlocks(content)
	content = r.redactEnvKeyValues(content)
	content = r.redactHTTPHeaderValues(content)
	content = r.redactURLQueryParams(content)

	if r.mode == ModeAggressive {
		content, err = r.redactHighEntropyStrings(content)
		if err != nil {
			return nil, fmt.Errorf("failed to redact high-entropy strings: %w", err)
		}
		content = r.redactKnownPrefixes(content)
	}

	return []byte(content), nil
}

// redactEnvKeyValues redacts environment variable key-value pairs.
func (r *Redactor) redactEnvKeyValues(content string) string {
	// Common secret patterns in env keys
	patterns := []string{
		`_TOKEN`, `_KEY`, `_SECRET`, `_PASSWORD`,
		`_API_KEY`, `_AUTH_TOKEN`, `_AUTHORIZATION`,
	}

	// Add custom key patterns
	for _, key := range r.customKeys {
		patterns = append(patterns, key)
	}

	// Also check for exact matches (no underscore)
	exactPatterns := []string{
		`API_KEY`, `AUTH_TOKEN`, `PASSWORD`, `APIKEY`, `SECRET`,
	}

	// Pattern for: KEY=VALUE or KEY="VALUE" or KEY='VALUE'
	// Match env-like assignments in logs
	for _, pattern := range patterns {
		// Convert glob pattern to regex (e.g., *_TOKEN -> \w+_TOKEN)
		regexPattern := strings.ReplaceAll(regexp.QuoteMeta(pattern), "\\_", "\\w*_")
		regexPattern = `(\w+` + regexPattern + `)\s*=\s*[\'"]?([^\'"\s\n]+)[\'"]?`

		re := regexp.MustCompile(regexPattern)
		content = re.ReplaceAllString(content, fmt.Sprintf("$1=%s", r.replacement))
	}

	// Handle exact matches
	for _, pattern := range exactPatterns {
		re := regexp.MustCompile(`(`+pattern+`)\s*=\s*[\'"]?([^\'"\s\n]+)[\'"]?`)
		content = re.ReplaceAllString(content, fmt.Sprintf("$1=%s", r.replacement))
	}

	return content
}

// redactHTTPHeaderValues redacts HTTP header values.
func (r *Redactor) redactHTTPHeaderValues(content string) string {
	// Sensitive HTTP headers
	headers := []string{
		"Authorization", "X-API-Key", "X-Auth-Token",
		"X-GitHub-Token", "X-Api-Key", "Authentication",
		"Cookie", "Set-Cookie", "Proxy-Authorization",
	}

	for _, header := range headers {
		// Match: Header: value (case insensitive) while preserving the original header casing in output.
		re := regexp.MustCompile(`(?i)(^|\n)\s*(` + regexp.QuoteMeta(header) + `)\s*:\s*[^\n\r]+`)
		content = re.ReplaceAllString(content, fmt.Sprintf("$1$2: %s", r.replacement))
	}

	return content
}

// redactURLQueryParams redacts sensitive URL query parameters.
func (r *Redactor) redactURLQueryParams(content string) string {
	// Sensitive query param names
	params := []string{
		"token", "key", "secret", "password", "api_key",
		"access_token", "refresh_token", "auth_token",
		"apikey", "authorization",
	}

	for _, param := range params {
		// Match: ?param=value or &param=value or &param=value&
		// Handle various URL formats
		re := regexp.MustCompile(`([?&])` + regexp.QuoteMeta(param) + `=[^&\s#\'"]+`)
		content = re.ReplaceAllString(content, fmt.Sprintf("$1%s=%s", param, r.replacement))
	}

	return content
}

// redactPEMBlocks redacts PEM-encoded private keys and certificates.
func (r *Redactor) redactPEMBlocks(content string) string {
	// Match PEM blocks: -----BEGIN ...----- to -----END ...-----
	// The label can contain spaces, letters, numbers, and common symbols
	re := regexp.MustCompile(`-----BEGIN [A-Za-z0-9+/\s-]+-----[\s\S]*?-----END [A-Za-z0-9+/\s-]+-----`)
	return re.ReplaceAllString(content, fmt.Sprintf("-----BEGIN REDACTED-----\n%s\n-----END REDACTED-----", r.replacement))
}

// redactKnownPrefixes redacts strings with known secret prefixes.
func (r *Redactor) redactKnownPrefixes(content string) string {
	// Known API key/token prefixes with their typical lengths
	patterns := []struct {
		prefix   string
		minLen   int
		maxLen   int
		pattern  string
	}{
		// GitHub tokens
		{prefix: "ghp_", minLen: 36, maxLen: 40, pattern: "ghp_[A-Za-z0-9_]{32,36}"},
		{prefix: "gho_", minLen: 36, maxLen: 40, pattern: "gho_[A-Za-z0-9_]{32,36}"},
		{prefix: "ghu_", minLen: 36, maxLen: 40, pattern: "ghu_[A-Za-z0-9_]{32,36}"},
		{prefix: "ghs_", minLen: 36, maxLen: 40, pattern: "ghs_[A-Za-z0-9_]{32,36}"},
		{prefix: "ghr_", minLen: 36, maxLen: 40, pattern: "ghr_[A-Za-z0-9_]{32,36}"},
		// Stripe
		{prefix: "sk_live_", minLen: 40, maxLen: 50, pattern: "sk_live_[A-Za-z0-9_]{32,40}"},
		{prefix: "sk_test_", minLen: 40, maxLen: 50, pattern: "sk_test_[A-Za-z0-9_]{32,40}"},
		{prefix: "sk-", minLen: 30, maxLen: 50, pattern: "sk-[A-Za-z0-9_]{26,46}"},
		// Anthropic
		{prefix: "anthropic_", minLen: 40, maxLen: 60, pattern: "anthropic_[A-Za-z0-9_]{30,50}"},
		// HuggingFace
		{prefix: "hf_", minLen: 30, maxLen: 50, pattern: "hf_[A-Za-z0-9_]{26,46}"},
		// AWS
		{prefix: "AKIA", minLen: 20, maxLen: 20, pattern: "AKIA[A-Z0-9]{16}"},
		// Slack
		{prefix: "xoxb-", minLen: 30, maxLen: 50, pattern: "xoxb-[A-Za-z0-9\\-]{26,46}"},
		{prefix: "xoxp-", minLen: 30, maxLen: 50, pattern: "xoxp-[A-Za-z0-9\\-]{26,46}"},
		// Google OAuth
		{prefix: "ya29.", minLen: 50, maxLen: 200, pattern: "ya29\\.[A-Za-z0-9_\\-]{46,196}"},
	}

	result := content
	for _, p := range patterns {
		re := regexp.MustCompile(p.pattern)
		result = re.ReplaceAllString(result, p.prefix+r.replacement)
	}

	return result
}

// redactHighEntropyStrings redacts strings that appear to be high-entropy secrets.
// This is a best-effort heuristic and may have false positives.
func (r *Redactor) redactHighEntropyStrings(content string) (string, error) {
	// Look for potential secrets (strings that look like base64, hex, or random strings)
	// Look for:
	// - Base64-like strings (20+ chars)
	// - Mixed alphanumeric with symbols (16+ chars)

	// Regex to find potential secrets
	potentialSecretRe := regexp.MustCompile(
		fmt.Sprintf(`\b[A-Za-z0-9_\-\.]{%d,}\b`, minEntropyCandidateLen),
	)

	lines := strings.Split(content, "\n")
	var redactedLines []string

	for _, line := range lines {
		matches := potentialSecretRe.FindAllString(line, -1)

		for _, match := range matches {
			// Skip if it's a common word or path
			if isLikelyFalsePositive(match) {
				continue
			}

			// Check for high entropy
			if r.isHighEntropy(match) {
				line = strings.ReplaceAll(line, match, r.replacement)
			}
		}

		redactedLines = append(redactedLines, line)
	}

	return strings.Join(redactedLines, "\n"), nil
}

// isHighEntropy calculates Shannon entropy of a string to determine if it looks like a secret.
func (r *Redactor) isHighEntropy(s string) bool {
	if len(s) < minEntropyCandidateLen {
		return false
	}

	// Calculate character frequency
	freq := make(map[rune]float64)
	for _, ch := range s {
		freq[ch]++
	}

	// Calculate Shannon entropy
	entropy := 0.0
	for _, count := range freq {
		p := count / float64(len(s))
		if p > 0 {
			entropy -= p * math.Log2(p)
		}
	}

	// Threshold: entropy > 4.0 suggests high randomness (like a secret)
	// Most natural language has entropy < 3.5
	return entropy > 4.0
}

// isLikelyFalsePositive checks if a string is likely a false positive (not a secret).
func isLikelyFalsePositive(s string) bool {
	// Check if it's a path
	if strings.Contains(s, "/") || strings.Contains(s, "\\") {
		return true
	}

	// Check if it's a URL
	if strings.HasPrefix(s, "http://") || strings.HasPrefix(s, "https://") {
		return true
	}

	// Check if it's all lowercase (likely a word)
	if s == strings.ToLower(s) && len(s) < 30 {
		return true
	}

	// Check if it's all uppercase (likely an acronym)
	if s == strings.ToUpper(s) && len(s) < 20 {
		return true
	}

	// Check if it looks like base64 but contains mostly lowercase letters
	lowerCount := 0
	for _, ch := range s {
		if ch >= 'a' && ch <= 'z' {
			lowerCount++
		}
	}
	if float64(lowerCount)/float64(len(s)) > 0.7 {
		return true
	}

	return false
}

// isCI detects if running in a CI environment.
func isCI() bool {
	// Check common CI environment variables
	ciEnvs := []struct {
		name  string
		truthy string
	}{
		{"CI", ""},
		{"GITHUB_ACTIONS", "true"},
		{"GITLAB_CI", "true"},
		{"TRAVIS", "true"},
		{"CIRCLECI", "true"},
		{"JENKINS_URL", ""},
		{"BUILDKITE", "true"},
		{"BITBUCKET_BUILD_NUMBER", ""},
		{"AZURE_PIPELINES", "true"},
	}

	for _, env := range ciEnvs {
		if _, exists := os.LookupEnv(env.name); exists {
			// For CI env, check if it's set to a truthy value
			val := os.Getenv(env.name)
			if env.truthy == "" || val == env.truthy || val == "1" || val == "true" {
				return true
			}
		}
	}

	return false
}

// RedactFromEnv creates a Redactor from environment variables.
// Uses HOLON_LOG_REDACT for mode and HOLON_LOG_REDACT_KEYS for custom keys.
func RedactFromEnv() *Redactor {
	modeStr := os.Getenv("HOLON_LOG_REDACT")
	mode := Mode(modeStr)
	if modeStr == "" {
		mode = DefaultMode
	}

	// Validate mode
	switch mode {
	case ModeOff, ModeBasic, ModeAggressive:
		// Valid modes
	default:
		mode = DefaultMode
	}

	customKeys := os.Getenv("HOLON_LOG_REDACT_KEYS")
	replacement := os.Getenv("HOLON_LOG_REDACT_REPLACEMENT")

	return New(Config{
		Mode:        mode,
		CustomKeys:  customKeys,
		Replacement: replacement,
	})
}
