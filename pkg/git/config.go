// Package git provides a shared utility layer for git operations.
// It wraps system git commands, providing a consistent API for use across
// workspace preparers and publishers. The design allows for future migration
// to go-git or a hybrid approach without changing the consumer API.
package git

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
)

// DefaultAuthorName is the default git author name when no other config is available.
const DefaultAuthorName = "Holon Bot"

// DefaultAuthorEmail is the default git author email when no other config is available.
const DefaultAuthorEmail = "bot@holon.run"

// Config holds resolved git configuration.
type Config struct {
	// AuthorName is the git user.name for commits.
	AuthorName string

	// AuthorEmail is the git user.email for commits.
	AuthorEmail string
}

// ConfigOptions holds options for resolving git configuration.
type ConfigOptions struct {
	// ProjectAuthorName is the project-level git author name from .holon/config.yaml.
	ProjectAuthorName string

	// ProjectAuthorEmail is the project-level git author email from .holon/config.yaml.
	ProjectAuthorEmail string

	// EnvAuthorName is the author name from environment variables (GIT_AUTHOR_NAME).
	EnvAuthorName string

	// EnvAuthorEmail is the author email from environment variables (GIT_AUTHOR_EMAIL).
	EnvAuthorEmail string
}

// ResolveConfig resolves git configuration with the following priority:
// 1. Host git config (local > global > system)
// 2. ProjectConfig (.holon/config.yaml git.author_*)
// 3. Environment variables (GIT_AUTHOR_NAME, GIT_AUTHOR_EMAIL)
// 4. Defaults ("Holon Bot <bot@holon.run>")
//
// This function consolidates all git config resolution logic into one place.
// It reads host git config with proper scope awareness (local > global > system).
//
// The returned Config contains the resolved author name and email that should
// be used for all git operations (run, publish, runtime).
//
// Usage:
//
//	cfg := git.ResolveConfig(git.ConfigOptions{
//	    ProjectAuthorName: projectConfig.Git.AuthorName,
//	    ProjectAuthorEmail: projectConfig.Git.AuthorEmail,
//	})
//	fmt.Printf("Git author: %s <%s>", cfg.AuthorName, cfg.AuthorEmail)
func ResolveConfig(opts ConfigOptions) Config {
	// Start with defaults
	cfg := Config{
		AuthorName: DefaultAuthorName,
		AuthorEmail: DefaultAuthorEmail,
	}

	// Priority 2: ProjectConfig (.holon/config.yaml)
	// Note: Host git config (Priority 1) is checked first below
	if opts.ProjectAuthorName != "" {
		cfg.AuthorName = opts.ProjectAuthorName
	}
	if opts.ProjectAuthorEmail != "" {
		cfg.AuthorEmail = opts.ProjectAuthorEmail
	}

	// Priority 3: Environment variables
	// These are checked before host git config because they represent explicit user intent
	if opts.EnvAuthorName != "" {
		cfg.AuthorName = opts.EnvAuthorName
	}
	if opts.EnvAuthorEmail != "" {
		cfg.AuthorEmail = opts.EnvAuthorEmail
	}

	// Priority 1: Host git config (highest priority)
	// This reads from local > global > system git config
	// and overrides all other sources.
	if hostName := getHostGitConfig("user.name"); hostName != "" {
		cfg.AuthorName = hostName
	}
	if hostEmail := getHostGitConfig("user.email"); hostEmail != "" {
		cfg.AuthorEmail = hostEmail
	}

	return cfg
}

// getHostGitConfig reads a git configuration value from the host system.
// It checks local, global, and system git config in that order.
// Returns empty string if the configuration is not set.
func getHostGitConfig(key string) string {
	// Try --get (checks local, then global, then system)
	cmd := exec.Command("git", "config", "--get", key)
	output, err := cmd.Output()
	if err == nil {
		return strings.TrimSpace(string(output))
	}
	return ""
}

// GetHostGitConfig reads a git configuration value from the host system.
// This is a public wrapper around getHostGitConfig for external use.
// Returns empty string if the configuration is not set.
//
// Deprecated: Use ResolveConfig() instead which provides proper priority handling.
// This function is kept for backward compatibility.
func GetHostGitConfig(key string) string {
	return getHostGitConfig(key)
}

// ResolveConfigForWorkspace resolves git configuration for a specific workspace directory.
// This is useful when you need workspace-specific git config (e.g., for publishers).
//
// Priority:
// 1. Workspace local git config
// 2. Host git config (global > system)
// 3. ProjectConfig (.holon/config.yaml)
// 4. Defaults ("Holon Bot <bot@holon.run>")
func ResolveConfigForWorkspace(ctx context.Context, workspaceDir string, opts ConfigOptions) (Config, error) {
	cfg := Config{
		AuthorName: DefaultAuthorName,
		AuthorEmail: DefaultAuthorEmail,
	}

	// Priority 2: ProjectConfig
	if opts.ProjectAuthorName != "" {
		cfg.AuthorName = opts.ProjectAuthorName
	}
	if opts.ProjectAuthorEmail != "" {
		cfg.AuthorEmail = opts.ProjectAuthorEmail
	}

	// Priority 3: Environment variables
	if opts.EnvAuthorName != "" {
		cfg.AuthorName = opts.EnvAuthorName
	}
	if opts.EnvAuthorEmail != "" {
		cfg.AuthorEmail = opts.EnvAuthorEmail
	}

	// Priority 1: Workspace git config (local) then host config (global/system)
	client := NewClient(workspaceDir)

	// Check workspace local config first
	if localName, err := client.ConfigGet(ctx, "user.name"); err == nil && localName != "" {
		cfg.AuthorName = localName
	}
	if localEmail, err := client.ConfigGet(ctx, "user.email"); err == nil && localEmail != "" {
		cfg.AuthorEmail = localEmail
	}

	// Fall back to host global/system config if workspace local is not set
	if cfg.AuthorName == DefaultAuthorName || cfg.AuthorEmail == DefaultAuthorEmail {
		if hostName := getHostGitConfig("user.name"); hostName != "" {
			cfg.AuthorName = hostName
		}
		if hostEmail := getHostGitConfig("user.email"); hostEmail != "" {
			cfg.AuthorEmail = hostEmail
		}
	}

	return cfg, nil
}

// GetGlobalConfig reads a global git configuration value.
// Returns empty string if the configuration is not set.
// This function is useful for getting the host's git user.name and user.email
// to use as fallback when project config doesn't specify them.
//
// Deprecated: Use ResolveConfig() instead which provides proper priority handling.
// This function is kept for backward compatibility.
func GetGlobalConfig(key string) string {
	cmd := exec.Command("git", "config", "--global", "--get", key)
	output, err := cmd.Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(output))
}

// GetUserName returns the resolved git user name.
// This is a convenience function that resolves the full config and returns just the name.
//
// Deprecated: Use ResolveConfig() instead.
func GetUserName(opts ConfigOptions) string {
	cfg := ResolveConfig(opts)
	return cfg.AuthorName
}

// GetUserEmail returns the resolved git user email.
// This is a convenience function that resolves the full config and returns just the email.
//
// Deprecated: Use ResolveConfig() instead.
func GetUserEmail(opts ConfigOptions) string {
	cfg := ResolveConfig(opts)
	return cfg.AuthorEmail
}

// GetUserInfo returns both user name and email from host git config.
// This is used by various parts of the system that need git identity.
//
// Deprecated: Use ResolveConfig() instead which provides proper priority handling.
// This function is kept for backward compatibility.
func GetUserInfo() (string, string) {
	name := getHostGitConfig("user.name")
	email := getHostGitConfig("user.email")
	if name == "" {
		name = DefaultAuthorName
	}
	if email == "" {
		email = DefaultAuthorEmail
	}
	return name, email
}

// FormatGitAuthor formats a git author string in the format "Name <email>".
func FormatGitAuthor(name, email string) string {
	if name == "" && email == "" {
		return ""
	}
	if name == "" {
		return email
	}
	if email == "" {
		return name
	}
	return fmt.Sprintf("%s <%s>", name, email)
}

// ParseGitAuthor parses a git author string in the format "Name <email>".
// Returns the name and email separately.
func ParseGitAuthor(author string) (name, email string) {
	author = strings.TrimSpace(author)

	// Find the last < and matching >
	leftAngle := strings.LastIndex(author, "<")
	rightAngle := strings.LastIndex(author, ">")

	if leftAngle != -1 && rightAngle != -1 && rightAngle > leftAngle {
		name = strings.TrimSpace(author[:leftAngle])
		email = strings.TrimSpace(author[leftAngle+1 : rightAngle])
	} else {
		name = author
	}

	return name, email
}

// ResolveConfigFromEnv resolves git configuration from environment variables only.
// This is useful for testing and scenarios where only env vars should be considered.
func ResolveConfigFromEnv() Config {
	return ResolveConfig(ConfigOptions{
		EnvAuthorName:  os.Getenv("GIT_AUTHOR_NAME"),
		EnvAuthorEmail: os.Getenv("GIT_AUTHOR_EMAIL"),
	})
}
