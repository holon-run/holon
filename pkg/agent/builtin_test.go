package agent

import (
	"os"
	"strings"
	"testing"
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

func TestBuiltinAgent_AutoInstallDisabled(t *testing.T) {
	// Save original value
	origValue := os.Getenv("HOLON_NO_AUTO_INSTALL")
	defer os.Setenv("HOLON_NO_AUTO_INSTALL", origValue)

	// Test with auto-install disabled
	os.Setenv("HOLON_NO_AUTO_INSTALL", "1")
	if !IsAutoInstallDisabled() {
		t.Error("Auto-install should be disabled when HOLON_NO_AUTO_INSTALL=1")
	}

	// Test with auto-install enabled
	os.Unsetenv("HOLON_NO_AUTO_INSTALL")
	if IsAutoInstallDisabled() {
		t.Error("Auto-install should be enabled when HOLON_NO_AUTO_INSTALL is unset")
	}
}

func TestDefaultBuiltinAgent_Consistency(t *testing.T) {
	// Test that DefaultBuiltinAgent returns consistent data
	agent1 := DefaultBuiltinAgent()
	agent2 := DefaultBuiltinAgent()

	if agent1.Name != agent2.Name {
		t.Error("DefaultBuiltinAgent() should return consistent agent name")
	}

	if agent1.Version != agent2.Version {
		t.Error("DefaultBuiltinAgent() should return consistent agent version")
	}

	if agent1.URL != agent2.URL {
		t.Error("DefaultBuiltinAgent() should return consistent agent URL")
	}

	if agent1.Checksum != agent2.Checksum {
		t.Error("DefaultBuiltinAgent() should return consistent agent checksum")
	}
}

