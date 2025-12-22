package agent

import (
	"os"
	"strings"
)

// BuiltinAgent represents the default builtin agent configuration
type BuiltinAgent struct {
	Name     string `json:"name"`
	Version  string `json:"version"`
	URL      string `json:"url"`
	Checksum string `json:"checksum"`
}

// DefaultBuiltinAgent returns the builtin agent configuration
// This can be updated to point to new versions as needed
func DefaultBuiltinAgent() *BuiltinAgent {
	// TODO: Update this to point to the actual release when ready
	// For now, using a placeholder that will be replaced with real values
	return &BuiltinAgent{
		Name:     "claude-agent",
		Version:  "agent-claude-v0.2.0",
		URL:      "https://github.com/holon-run/holon/releases/download/agent-claude-v0.2.0/holon-agent-claude-0.2.0.tar.gz",
		Checksum: "5cde0cbaa9f3e7f210b2484185c7ad554bc53a55cbebf8c00fae4fbc04a5b04f",
	}
}

// IsAutoInstallDisabled checks if the auto-install feature is disabled
func IsAutoInstallDisabled() bool {
	disabled := os.Getenv("HOLON_NO_AUTO_INSTALL")
	return strings.ToLower(disabled) == "1" || strings.ToLower(disabled) == "true"
}

