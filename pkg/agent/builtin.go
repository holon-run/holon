package agent

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/holon-run/holon/pkg/agent/resolver"
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
		Version:  "v0.1.0",
		URL:      "https://github.com/holon-run/claude-agent/releases/download/v0.1.0/agent-bundle.tar.gz",
		Checksum: "sha256=0000000000000000000000000000000000000000000000000000000000000000", // placeholder
	}
}

// IsAutoInstallDisabled checks if the auto-install feature is disabled
func IsAutoInstallDisabled() bool {
	disabled := os.Getenv("HOLON_NO_AUTO_INSTALL")
	return strings.ToLower(disabled) == "1" || strings.ToLower(disabled) == "true"
}

// BuiltinResolver resolves the builtin default agent
type BuiltinResolver struct {
	agent *BuiltinAgent
}

// NewBuiltinResolver creates a new builtin resolver
func NewBuiltinResolver() *BuiltinResolver {
	return &BuiltinResolver{
		agent: DefaultBuiltinAgent(),
	}
}

// CanResolve returns true if this is a request for the default agent
func (r *BuiltinResolver) CanResolve(ref string) bool {
	// Resolve empty string (no agent specified) and "default" alias
	return strings.TrimSpace(ref) == "" || strings.TrimSpace(ref) == "default"
}

// Resolve resolves the builtin agent to a local bundle path
func (r *BuiltinResolver) Resolve(ctx context.Context, cacheDir string) (string, error) {
	if IsAutoInstallDisabled() {
		return "", fmt.Errorf("auto-install is disabled (HOLON_NO_AUTO_INSTALL=1)")
	}

	// Create a resolver registry to handle the download
	registry := resolver.NewRegistry(cacheDir)

	// Resolve the builtin agent URL
	return registry.Resolve(ctx, r.agent.URL+"#sha256="+r.agent.Checksum)
}

// GetInfo returns information about the builtin agent
func (r *BuiltinResolver) GetInfo() *BuiltinAgent {
	return r.agent
}