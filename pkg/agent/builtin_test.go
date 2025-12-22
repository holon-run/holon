package agent

import (
	"context"
	"os"
	"strings"
	"testing"

	"github.com/holon-run/holon/pkg/agent/resolver"
)

func TestDefaultBuiltinAgent(t *testing.T) {
	agent := DefaultBuiltinAgent()
	if agent == nil {
		t.Fatal("DefaultBuiltinAgent() returned nil")
	}

	if agent.Name == "" {
		t.Error("Agent name should not be empty")
	}

	if agent.Version == "" {
		t.Error("Agent version should not be empty")
	}

	if agent.URL == "" {
		t.Error("Agent URL should not be empty")
	}

	if !strings.HasPrefix(agent.URL, "http://") && !strings.HasPrefix(agent.URL, "https://") {
		t.Error("Agent URL should start with http:// or https://")
	}

	if agent.Checksum == "" {
		t.Error("Agent checksum should not be empty")
	}
}

func TestIsAutoInstallDisabled(t *testing.T) {
	// Save original value
	origValue := os.Getenv("HOLON_NO_AUTO_INSTALL")
	defer os.Setenv("HOLON_NO_AUTO_INSTALL", origValue)

	// Test with unset environment variable
	os.Unsetenv("HOLON_NO_AUTO_INSTALL")
	if IsAutoInstallDisabled() {
		t.Error("Auto-install should be enabled when HOLON_NO_AUTO_INSTALL is unset")
	}

	// Test with "1"
	os.Setenv("HOLON_NO_AUTO_INSTALL", "1")
	if !IsAutoInstallDisabled() {
		t.Error("Auto-install should be disabled when HOLON_NO_AUTO_INSTALL=1")
	}

	// Test with "true"
	os.Setenv("HOLON_NO_AUTO_INSTALL", "true")
	if !IsAutoInstallDisabled() {
		t.Error("Auto-install should be disabled when HOLON_NO_AUTO_INSTALL=true")
	}

	// Test with "TRUE" (case insensitive)
	os.Setenv("HOLON_NO_AUTO_INSTALL", "TRUE")
	if !IsAutoInstallDisabled() {
		t.Error("Auto-install should be disabled when HOLON_NO_AUTO_INSTALL=TRUE")
	}

	// Test with "0"
	os.Setenv("HOLON_NO_AUTO_INSTALL", "0")
	if IsAutoInstallDisabled() {
		t.Error("Auto-install should be enabled when HOLON_NO_AUTO_INSTALL=0")
	}

	// Test with "false"
	os.Setenv("HOLON_NO_AUTO_INSTALL", "false")
	if IsAutoInstallDisabled() {
		t.Error("Auto-install should be enabled when HOLON_NO_AUTO_INSTALL=false")
	}
}

func TestBuiltinResolver_CanResolve(t *testing.T) {
	// Save original value
	origValue := os.Getenv("HOLON_NO_AUTO_INSTALL")
	defer os.Setenv("HOLON_NO_AUTO_INSTALL", origValue)

	// Test with auto-install enabled
	os.Unsetenv("HOLON_NO_AUTO_INSTALL")
	r := NewBuiltinResolver()

	testCases := []struct {
		ref      string
		expected bool
	}{
		{"", true},
		{"default", true},
		{"Default", true}, // case insensitive due to TrimSpace
		{"default ", true},
		{" default", true},
		{"  default  ", true},
		{"custom-agent", false},
		{"http://example.com/agent.tar.gz", false},
		{"/path/to/agent.tar.gz", false},
		{"default-agent", false},
	}

	for _, tc := range testCases {
		result := r.CanResolve(tc.ref)
		if result != tc.expected {
			t.Errorf("CanResolve(%q) = %v, expected %v", tc.ref, result, tc.expected)
		}
	}
}

func TestBuiltinResolver_CanResolve_WithAutoInstallDisabled(t *testing.T) {
	// Save original value
	origValue := os.Getenv("HOLON_NO_AUTO_INSTALL")
	defer os.Setenv("HOLON_NO_AUTO_INSTALL", origValue)

	// Disable auto-install
	os.Setenv("HOLON_NO_AUTO_INSTALL", "1")
	r := NewBuiltinResolver()

	testCases := []struct {
		ref      string
		expected bool
	}{
		{"", false},
		{"default", false},
		{"custom-agent", false},
	}

	for _, tc := range testCases {
		result := r.CanResolve(tc.ref)
		if result != tc.expected {
			t.Errorf("CanResolve(%q) with auto-install disabled = %v, expected %v", tc.ref, result, tc.expected)
		}
	}
}

func TestBuiltinResolver_GetInfo(t *testing.T) {
	r := NewBuiltinResolver()
	info := r.GetInfo()

	if info == nil {
		t.Fatal("GetInfo() returned nil")
	}

	if info != DefaultBuiltinAgent() {
		t.Error("GetInfo() should return the same agent as DefaultBuiltinAgent()")
	}
}

func TestResolverRegistry_WithBuiltinResolver(t *testing.T) {
	// Create a temporary directory for cache
	tmpDir := t.TempDir()
	registry := resolver.NewRegistry(tmpDir)

	// Test that empty string resolves (or attempts to resolve) to builtin agent
	ctx := context.Background()

	// This will likely fail since we're using a placeholder URL, but it should
	// attempt to resolve through the builtin resolver and give us a meaningful error
	_, err := registry.Resolve(ctx, "")
	if err == nil {
		t.Error("Expected error when resolving builtin agent with placeholder URL")
	}

	// Check that the error is related to downloading, not to "no resolver found"
	if strings.Contains(err.Error(), "no resolver found") {
		t.Errorf("Expected download-related error, got: %v", err)
	}

	// Test that "default" also resolves through builtin resolver
	_, err = registry.Resolve(ctx, "default")
	if err == nil {
		t.Error("Expected error when resolving builtin agent with placeholder URL")
	}

	if strings.Contains(err.Error(), "no resolver found") {
		t.Errorf("Expected download-related error, got: %v", err)
	}
}