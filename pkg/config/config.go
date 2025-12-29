// Package config provides project-level configuration for Holon.
// It supports loading configuration from .holon/config.yaml files with
// proper precedence: CLI flags > project config > defaults.
package config

import (
	"fmt"
	"os"
	"path/filepath"

	"gopkg.in/yaml.v3"
)

const (
	// ConfigDir is the directory name for Holon configuration
	ConfigDir = ".holon"
	// ConfigFile is the name of the configuration file
	ConfigFile = "config.yaml"
	// ConfigPath is the full path to the config file relative to project root
	ConfigPath = ConfigDir + "/" + ConfigFile
)

// ProjectConfig represents the project-level configuration for Holon.
// It provides defaults that can be overridden by CLI flags.
type ProjectConfig struct {
	// BaseImage is the default container toolchain image (e.g., "golang:1.22")
	// Set to "auto" or "auto-detect" to enable automatic detection based on workspace files.
	BaseImage string `yaml:"base_image,omitempty"`

	// Agent is the default agent bundle reference (path, URL, or alias)
	Agent string `yaml:"agent,omitempty"`

	// AgentChannel controls how the agent is resolved when no explicit agent is specified.
	// Values: "latest" (default), "builtin", "pinned:<version>"
	AgentChannel string `yaml:"agent_channel,omitempty"`

	// LogLevel is the default Holon log level (debug, info, progress, minimal)
	LogLevel string `yaml:"log_level,omitempty"`

	// Git configuration overrides for container operations
	Git GitConfig `yaml:"git,omitempty"`
}

// GitConfig contains Git identity overrides for container operations.
// These settings override the host's git config when running inside containers.
type GitConfig struct {
	// AuthorName overrides git config user.name for containers
	AuthorName string `yaml:"author_name,omitempty"`

	// AuthorEmail overrides git config user.email for containers
	AuthorEmail string `yaml:"author_email,omitempty"`
}

// Load loads the project configuration from the given directory.
// It searches for .holon/config.yaml in the directory and its parents.
//
// If no config file is found, it returns a zero config and nil error.
// If a config file is found but cannot be parsed, it returns an error.
func Load(dir string) (*ProjectConfig, error) {
	configPath, err := findConfigPath(dir)
	if err != nil {
		return nil, err
	}
	if configPath == "" {
		// No config file found, return zero config
		return &ProjectConfig{}, nil
	}

	data, err := os.ReadFile(configPath)
	if err != nil {
		return nil, fmt.Errorf("failed to read config file: %w", err)
	}

	var cfg ProjectConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("failed to parse config file: %w", err)
	}

	return &cfg, nil
}

// LoadFromCurrentDir loads the project configuration from the current working directory.
func LoadFromCurrentDir() (*ProjectConfig, error) {
	dir, err := os.Getwd()
	if err != nil {
		return nil, fmt.Errorf("failed to get current directory: %w", err)
	}
	return Load(dir)
}

// findConfigPath searches for .holon/config.yaml in dir and its parent directories.
// It returns the full path to the config file, or empty string if not found.
func findConfigPath(dir string) (string, error) {
	// First, check if dir exists
	absDir, err := filepath.Abs(dir)
	if err != nil {
		return "", fmt.Errorf("failed to resolve absolute path: %w", err)
	}

	// Search upward through directory tree
	for {
		configPath := filepath.Join(absDir, ConfigPath)
		if _, err := os.Stat(configPath); err == nil {
			return configPath, nil
		}

		// Move to parent directory
		parentDir := filepath.Dir(absDir)
		if parentDir == absDir {
			// Reached root without finding config
			return "", nil
		}
		absDir = parentDir
	}
}

// ResolveString returns the effective value for a string configuration field.
// Precedence: cliValue > configValue > defaultValue.
// Returns the effective value and its source ("cli", "config", or "default").
func (c *ProjectConfig) ResolveString(cliValue, configValue, defaultValue string) (string, string) {
	if cliValue != "" {
		return cliValue, "cli"
	}
	if configValue != "" {
		return configValue, "config"
	}
	return defaultValue, "default"
}

// ResolveBaseImage returns the effective base image and its source.
func (c *ProjectConfig) ResolveBaseImage(cliValue, defaultValue string) (string, string) {
	return c.ResolveString(cliValue, c.BaseImage, defaultValue)
}

// ResolveAgent returns the effective agent bundle and its source.
func (c *ProjectConfig) ResolveAgent(cliValue string) (string, string) {
	return c.ResolveString(cliValue, c.Agent, "")
}

// ResolveAgentChannel returns the effective agent channel and its source.
// Default is "latest" if not specified.
func (c *ProjectConfig) ResolveAgentChannel(cliValue string) (string, string) {
	if cliValue != "" {
		return cliValue, "cli"
	}
	if c.AgentChannel != "" {
		return c.AgentChannel, "config"
	}
	return "latest", "default"
}

// ResolveLogLevel returns the effective log level and its source.
func (c *ProjectConfig) ResolveLogLevel(cliValue, defaultValue string) (string, string) {
	return c.ResolveString(cliValue, c.LogLevel, defaultValue)
}

// GetGitAuthorName returns the configured git author name, or empty if not set.
func (c *ProjectConfig) GetGitAuthorName() string {
	return c.Git.AuthorName
}

// GetGitAuthorEmail returns the configured git author email, or empty if not set.
func (c *ProjectConfig) GetGitAuthorEmail() string {
	return c.Git.AuthorEmail
}

// HasGitConfig returns true if any git configuration is set.
func (c *ProjectConfig) HasGitConfig() bool {
	return c.Git.AuthorName != "" || c.Git.AuthorEmail != ""
}

// ShouldAutoDetectImage returns true if auto-detection is enabled.
// Auto-detection is enabled if base_image is "auto" or "auto-detect".
func (c *ProjectConfig) ShouldAutoDetectImage() bool {
	return c.BaseImage == "auto" || c.BaseImage == "auto-detect"
}

// IsImageAutoDetectEnabled returns true if auto-detection is explicitly
// configured in the project config (vs. default behavior).
func (c *ProjectConfig) IsImageAutoDetectEnabled() bool {
	return c.BaseImage == "auto" || c.BaseImage == "auto-detect"
}
