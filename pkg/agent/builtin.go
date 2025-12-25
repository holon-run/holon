package agent

import (
	"fmt"
	"log"
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
// This is updated with each Holon release to point to the latest agent release
func DefaultBuiltinAgent() *BuiltinAgent {
	return &BuiltinAgent{
		Name:     "claude-agent",
		Version:  "agent-claude-v0.3.0",
		URL:      "https://github.com/holon-run/holon/releases/download/agent-claude-v0.3.0/holon-agent-claude-0.3.0.tar.gz",
		Checksum: "5df9f9344ebd14d8b3975e552d23a4bf195a59208710353f069bd50a3814db96",
	}
}

// IsAutoInstallDisabled checks if the auto-install feature is disabled
func IsAutoInstallDisabled() bool {
	disabled := os.Getenv("HOLON_NO_AUTO_INSTALL")
	return strings.ToLower(disabled) == "1" || strings.ToLower(disabled) == "true"
}

// CheckBuiltinAgentStaleness checks if the builtin agent is stale compared to the latest release
// Returns (isStale bool, latestVersion string, error)
// Logs warnings if unable to fetch latest version or if version is stale
func CheckBuiltinAgentStaleness(repo string) (bool, string, error) {
	builtin := DefaultBuiltinAgent()

	// Fetch latest release from GitHub
	latest, err := GetLatestAgentRelease(repo)
	if err != nil {
		log.Printf("[warn] failed to fetch latest agent release from GitHub: %v", err)
		log.Printf("[warn] builtin agent version %s could not be verified against latest release", builtin.Version)
		return false, "", fmt.Errorf("failed to fetch latest release: %w", err)
	}

	// Compare versions
	if latest.TagName != builtin.Version {
		log.Printf("[warn] builtin agent version %s is behind latest release %s", builtin.Version, latest.TagName)
		log.Printf("[info] consider updating DefaultBuiltinAgent() to use %s", latest.TagName)
		return true, latest.TagName, nil
	}

	// Version is current
	log.Printf("[info] builtin agent version %s is up to date", builtin.Version)
	return false, latest.TagName, nil
}

