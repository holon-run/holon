package docker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRedactLogs_RedactsSensitiveValues(t *testing.T) {
	t.Setenv("HOLON_LOG_REDACT", "basic")

	outDir := t.TempDir()
	logPath := filepath.Join(outDir, "execution.log")
	content := strings.Join([]string{
		"ANTHROPIC_API_KEY=sk-test-123456",
		"Authorization: Bearer ghp_abcdef123456",
		"https://api.example.com?token=secret-token-value",
	}, "\n")
	if err := os.WriteFile(logPath, []byte(content), 0o644); err != nil {
		t.Fatalf("failed to write execution log: %v", err)
	}

	if err := redactLogs(outDir); err != nil {
		t.Fatalf("redactLogs() error = %v", err)
	}

	redacted, err := os.ReadFile(logPath)
	if err != nil {
		t.Fatalf("failed to read redacted log: %v", err)
	}
	redactedStr := string(redacted)

	for _, secret := range []string{"sk-test-123456", "ghp_abcdef123456", "secret-token-value"} {
		if strings.Contains(redactedStr, secret) {
			t.Fatalf("redacted log still contains secret %q: %s", secret, redactedStr)
		}
	}
	if !strings.Contains(redactedStr, "***REDACTED***") {
		t.Fatalf("redacted log missing replacement marker: %s", redactedStr)
	}
}

func TestRedactLogs_RedactsNestedLogFiles(t *testing.T) {
	t.Setenv("HOLON_LOG_REDACT", "basic")

	outDir := t.TempDir()
	nestedDir := filepath.Join(outDir, "evidence")
	if err := os.MkdirAll(nestedDir, 0o755); err != nil {
		t.Fatalf("failed to create nested log dir: %v", err)
	}
	logPath := filepath.Join(nestedDir, "agent.log")
	if err := os.WriteFile(logPath, []byte("X_API_KEY=my-sensitive-key"), 0o644); err != nil {
		t.Fatalf("failed to write nested log: %v", err)
	}

	if err := redactLogs(outDir); err != nil {
		t.Fatalf("redactLogs() error = %v", err)
	}

	redacted, err := os.ReadFile(logPath)
	if err != nil {
		t.Fatalf("failed to read nested redacted log: %v", err)
	}
	if strings.Contains(string(redacted), "my-sensitive-key") {
		t.Fatalf("nested log still contains sensitive value: %s", string(redacted))
	}
}
