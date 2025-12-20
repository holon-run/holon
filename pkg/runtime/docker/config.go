package docker

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/docker/docker/api/types/mount"
	"github.com/jolestar/holon/pkg/api/v1"
)

// Pure helper functions for container configuration assembly

// MountConfig represents the mount configuration for a container
type MountConfig struct {
	SnapshotDir    string
	SpecPath       string
	ContextPath    string
	OutDir         string
	PromptPath     string
	UserPromptPath string
}

// EnvConfig represents the environment configuration for a container
type EnvConfig struct {
	UserEnv     map[string]string
	HostUID     int
	HostGID     int
}

// BuildContainerMounts assembles the Docker mounts configuration
// This function is pure and deterministic - no Docker client interaction
func BuildContainerMounts(cfg *MountConfig) []mount.Mount {
	mounts := []mount.Mount{
		{
			Type:   mount.TypeBind,
			Source: cfg.SnapshotDir,
			Target: "/holon/workspace",
		},
		{
			Type:   mount.TypeBind,
			Source: cfg.SpecPath,
			Target: "/holon/input/spec.yaml",
		},
		{
			Type:   mount.TypeBind,
			Source: cfg.OutDir,
			Target: "/holon/output",
		},
	}

	// Optional context mount
	if cfg.ContextPath != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.ContextPath,
			Target: "/holon/input/context",
		})
	}

	// Optional system prompt mount
	if cfg.PromptPath != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.PromptPath,
			Target: "/holon/input/prompts/system.md",
		})
	}

	// Optional user prompt mount
	if cfg.UserPromptPath != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.UserPromptPath,
			Target: "/holon/input/prompts/user.md",
		})
	}

	return mounts
}

// BuildContainerEnv assembles the environment variables for a container
// This function is pure and deterministic - no Docker client interaction
func BuildContainerEnv(cfg *EnvConfig) []string {
	env := make([]string, 0, len(cfg.UserEnv)+2)

	// Add user-provided environment variables
	for k, v := range cfg.UserEnv {
		env = append(env, fmt.Sprintf("%s=%s", k, v))
	}

	// Add host UID/GID for proper file permissions
	env = append(env, fmt.Sprintf("HOST_UID=%d", cfg.HostUID))
	env = append(env, fmt.Sprintf("HOST_GID=%d", cfg.HostGID))

	return env
}

// ValidateRequiredArtifacts checks that all required artifacts are present
// This function is pure and deterministic - no Docker client interaction
func ValidateRequiredArtifacts(outDir string, requiredArtifacts []v1.Artifact) error {
	// Always check for manifest.json as a basic requirement
	manifestPath := filepath.Join(outDir, "manifest.json")
	if _, err := os.Stat(manifestPath); os.IsNotExist(err) {
		return fmt.Errorf("missing required artifact: manifest.json")
	}

	// Check spec-defined required artifacts
	for _, artifact := range requiredArtifacts {
		if artifact.Required {
			artifactPath := filepath.Join(outDir, artifact.Path)
			if _, err := os.Stat(artifactPath); os.IsNotExist(err) {
				return fmt.Errorf("missing required artifact: %s", artifact.Path)
			}
		}
	}

	return nil
}

// ValidateMountTargets validates that all mount sources exist
// This function is pure and deterministic - no Docker client interaction
func ValidateMountTargets(cfg *MountConfig) error {
	// Check required mount sources
	if cfg.SnapshotDir == "" {
		return fmt.Errorf("snapshot directory cannot be empty")
	}
	if cfg.SpecPath == "" {
		return fmt.Errorf("spec path cannot be empty")
	}
	if cfg.OutDir == "" {
		return fmt.Errorf("output directory cannot be empty")
	}

	// Check that mount sources exist (except snapshot which will be created)
	if _, err := os.Stat(cfg.SpecPath); os.IsNotExist(err) {
		return fmt.Errorf("spec path does not exist: %s", cfg.SpecPath)
	}
	if _, err := os.Stat(cfg.OutDir); os.IsNotExist(err) {
		return fmt.Errorf("output directory does not exist: %s", cfg.OutDir)
	}

	// Check optional mount sources if provided
	if cfg.ContextPath != "" {
		if _, err := os.Stat(cfg.ContextPath); os.IsNotExist(err) {
			return fmt.Errorf("context path does not exist: %s", cfg.ContextPath)
		}
	}

	if cfg.PromptPath != "" {
		if _, err := os.Stat(cfg.PromptPath); os.IsNotExist(err) {
			return fmt.Errorf("prompt path does not exist: %s", cfg.PromptPath)
		}
	}

	if cfg.UserPromptPath != "" {
		if _, err := os.Stat(cfg.UserPromptPath); os.IsNotExist(err) {
			return fmt.Errorf("user prompt path does not exist: %s", cfg.UserPromptPath)
		}
	}

	return nil
}