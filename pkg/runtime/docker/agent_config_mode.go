package docker

import (
	"fmt"
	"strings"
)

// AgentConfigMode represents the mode for mounting agent-specific configuration
// from the host into the container. This replaces the boolean UseLocalClaudeConfig
// with more flexible behavior.
type AgentConfigMode string

const (
	// AgentConfigModeAuto automatically mounts host config if it exists,
	// and silently skips if it doesn't exist. This is the default behavior.
	AgentConfigModeAuto AgentConfigMode = "auto"

	// AgentConfigModeYes always attempts to mount the host config,
	// and emits a warning if the config directory doesn't exist.
	AgentConfigModeYes AgentConfigMode = "yes"

	// AgentConfigModeNo never mounts the host config.
	AgentConfigModeNo AgentConfigMode = "no"
)

// ParseAgentConfigMode parses a string into an AgentConfigMode.
// Case-insensitive. Returns error for invalid values.
func ParseAgentConfigMode(s string) (AgentConfigMode, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case "auto":
		return AgentConfigModeAuto, nil
	case "yes", "y", "true", "1":
		return AgentConfigModeYes, nil
	case "no", "n", "false", "0":
		return AgentConfigModeNo, nil
	default:
		return "", fmt.Errorf("invalid agent config mode %q; must be one of: auto, yes, no", s)
	}
}

// String returns the string representation of the mode.
func (m AgentConfigMode) String() string {
	return string(m)
}

// ShouldMount determines whether to mount the config based on the mode
// and whether the config directory exists.
func (m AgentConfigMode) ShouldMount(dirExists bool) bool {
	switch m {
	case AgentConfigModeYes:
		// Always attempt mount, even if directory doesn't exist
		// (warning will be emitted elsewhere)
		return true
	case AgentConfigModeAuto:
		// Only mount if directory exists
		return dirExists
	case AgentConfigModeNo:
		// Never mount
		return false
	default:
		// Default to auto behavior for unknown modes
		return dirExists
	}
}

// WarnIfMissing returns true if a warning should be emitted when
// the config directory doesn't exist.
func (m AgentConfigMode) WarnIfMissing() bool {
	return m == AgentConfigModeYes
}
