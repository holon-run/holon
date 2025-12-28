package docker

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/docker/docker/api/types/mount"
	"github.com/holon-run/holon/pkg/api/v1"
)

// Pure helper functions for container configuration assembly

// MountConfig represents the mount configuration for a container
type MountConfig struct {
	SnapshotDir          string
	InputPath            string // Path to input directory (contains spec.yaml, context/, prompts/)
	OutDir               string
	LocalClaudeConfigDir string // Path to host ~/.claude directory (optional, for mounting)
}

// EnvConfig represents the environment configuration for a container
type EnvConfig struct {
	UserEnv map[string]string
	HostUID int
	HostGID int
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
			Type:     mount.TypeBind,
			Source:   cfg.InputPath,
			Target:   "/holon/input",
			ReadOnly: true,
		},
		{
			Type:   mount.TypeBind,
			Source: cfg.OutDir,
			Target: "/holon/output",
		},
	}

	// Add Claude config directory mount if provided
	if cfg.LocalClaudeConfigDir != "" {
		mounts = append(mounts, mount.Mount{
			Type:        mount.TypeBind,
			Source:      cfg.LocalClaudeConfigDir,
			Target:      "/root/.claude",
			ReadOnly:    true, // Mount read-only to prevent accidental modifications
			BindOptions: &mount.BindOptions{Propagation: mount.PropagationRPrivate},
		})
	}

	return mounts
}

// BuildContainerEnv assembles the environment variables for a container
// This function is pure and deterministic - no Docker client interaction
func BuildContainerEnv(cfg *EnvConfig) []string {
	env := make([]string, 0, len(cfg.UserEnv)+3)

	// Add user-provided environment variables
	for k, v := range cfg.UserEnv {
		env = append(env, fmt.Sprintf("%s=%s", k, v))
	}

	// Add host UID/GID for proper file permissions
	env = append(env, fmt.Sprintf("HOST_UID=%d", cfg.HostUID))
	env = append(env, fmt.Sprintf("HOST_GID=%d", cfg.HostGID))

	// Disable Git's safe directory check
	// This is needed because Docker containers may have different UIDs
	// than the host, causing Git to detect "dubious ownership"
	env = append(env, "GIT_CONFIG_NOSYSTEM=1")

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
	if cfg.InputPath == "" {
		return fmt.Errorf("input path cannot be empty")
	}
	if cfg.OutDir == "" {
		return fmt.Errorf("output directory cannot be empty")
	}

	// Check that mount sources exist (except snapshot which will be created)
	if _, err := os.Stat(cfg.InputPath); os.IsNotExist(err) {
		return fmt.Errorf("input path does not exist: %s", cfg.InputPath)
	}
	if _, err := os.Stat(cfg.OutDir); os.IsNotExist(err) {
		return fmt.Errorf("output directory does not exist: %s", cfg.OutDir)
	}

	return nil
}
