package agent

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
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
		Version:  "agent-claude-v0.6.1",
		URL:      "https://github.com/holon-run/holon/releases/download/agent-claude-v0.6.1/holon-agent-claude-0.6.1.tar.gz",
		Checksum: "bf256b074cc818bd0e125afa736a137c0832d49ffa138633a60e525824be99a7",
	}
}

// IsAutoInstallDisabled checks if the auto-install feature is disabled
func IsAutoInstallDisabled() bool {
	disabled := os.Getenv("HOLON_NO_AUTO_INSTALL")
	return strings.ToLower(disabled) == "1" || strings.ToLower(disabled) == "true"
}

// stalenessCheckRateLimiter implements rate limiting for staleness checks
type stalenessCheckRateLimiter struct {
	mu             sync.Mutex
	lastCheckTime  time.Time
	checkInterval  time.Duration
}

// Global rate limiter - checks once per hour by default
var globalRateLimiter = &stalenessCheckRateLimiter{
	checkInterval: 1 * time.Hour,
}

// shouldCheck returns true if a staleness check should be performed
func (r *stalenessCheckRateLimiter) shouldCheck() bool {
	r.mu.Lock()
	defer r.mu.Unlock()

	now := time.Now()
	if now.Sub(r.lastCheckTime) >= r.checkInterval {
		r.lastCheckTime = now
		return true
	}
	return false
}

// CheckBuiltinAgentStaleness checks if the builtin agent is stale compared to the latest release
// Returns (isStale bool, latestVersion string, error)
// Logs warnings if unable to fetch latest version or if version is stale
func CheckBuiltinAgentStaleness(repo string) (bool, string, error) {
	builtin := DefaultBuiltinAgent()

	// Fetch latest release from GitHub
	latest, err := GetLatestAgentRelease(repo)
	if err != nil {
		holonlog.Warn("failed to fetch latest agent release from GitHub", "error", err)
		holonlog.Warn("builtin agent version could not be verified against latest release", "version", builtin.Version)
		return false, "", fmt.Errorf("failed to fetch latest release: %w", err)
	}

	// Compare versions
	if latest.TagName != builtin.Version {
		holonlog.Warn("builtin agent version is behind latest release", "current", builtin.Version, "latest", latest.TagName)
		holonlog.Info("consider updating DefaultBuiltinAgent() to use latest version", "version", latest.TagName)
		return true, latest.TagName, nil
	}

	// Version is current
	holonlog.Info("builtin agent version is up to date", "version", builtin.Version)
	return false, latest.TagName, nil
}

// CheckBuiltinAgentStalenessWithLimit performs staleness check with rate limiting
// Returns (isStale bool, latestVersion string, error)
// If rate limited, returns (false, "", nil) - no error but no check performed
func CheckBuiltinAgentStalenessWithLimit(ctx context.Context, repo string) (bool, string, error) {
	// Check rate limit first
	if !globalRateLimiter.shouldCheck() {
		// Rate limited - skip the check silently
		return false, "", nil
	}

	// Respect context cancellation
	select {
	case <-ctx.Done():
		return false, "", ctx.Err()
	default:
	}

	return CheckBuiltinAgentStaleness(repo)
}
