package docker

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/mount"
	"github.com/holon-run/holon/pkg/api/v1"
)

const (
	ContainerWorkspaceDir = "/workspace"
	ContainerInputDir     = "/input"
	ContainerOutputDir    = "/output"
	ContainerStateDir     = "/state"
	ContainerAgentHome    = "/root"
)

// Pure helper functions for container configuration assembly

// MountConfig represents the mount configuration for a container
type MountConfig struct {
	SnapshotDir          string
	InputPath            string // Path to input directory (contains spec.yaml, context/, prompts/)
	OutDir               string
	StateDir             string // Path to state directory for cross-run skill caches (optional, for mounting)
	AgentHome            string // Path to agent home mounted to /root (optional, for persistent persona/state)
	LocalClaudeConfigDir string // Path to host ~/.claude directory (optional, for mounting)
	LocalSkillsDir       string // Path to skills staging directory (optional, for mounting)
	LocalAgentDistDir    string // Path to local agent dist directory for dev runtime mode (optional, for mounting)
}

// EnvConfig represents the environment configuration for a container
type EnvConfig struct {
	UserEnv map[string]string
	HostUID int
	HostGID int
}

// HostConfigOptions represents docker host configuration inputs.
type HostConfigOptions struct {
	Mounts []mount.Mount
}

// BuildContainerMounts assembles the Docker mounts configuration.
func BuildContainerMounts(cfg *MountConfig) []mount.Mount {
	mounts := []mount.Mount{
		{
			Type:   mount.TypeBind,
			Source: cfg.SnapshotDir,
			Target: ContainerWorkspaceDir,
		},
		{
			Type:     mount.TypeBind,
			Source:   cfg.InputPath,
			Target:   ContainerInputDir,
			ReadOnly: true,
		},
		{
			Type:   mount.TypeBind,
			Source: cfg.OutDir,
			Target: ContainerOutputDir,
		},
	}

	// Add state directory mount (if provided)
	// This mounts to /state for cross-run skill caches
	if cfg.StateDir != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.StateDir,
			Target: ContainerStateDir,
		})
	}

	// Add agent home mount (if provided)
	// This mounts host agent_home directly to container /root.
	if cfg.AgentHome != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.AgentHome,
			Target: ContainerAgentHome,
		})
	}

	// Add Claude config directory mount (if provided)
	// This mounts to /root/.claude
	if cfg.LocalClaudeConfigDir != "" {
		mounts = append(mounts, mount.Mount{
			Type:        mount.TypeBind,
			Source:      cfg.LocalClaudeConfigDir,
			Target:      "/root/.claude",
			ReadOnly:    true, // Mount read-only to prevent accidental modifications
			BindOptions: &mount.BindOptions{Propagation: mount.PropagationRPrivate},
		})
	}

	// Add skills directory mount SECOND (if provided)
	// This mounts to /root/.claude/skills and will overlay any existing skills subdir
	if cfg.LocalSkillsDir != "" {
		mounts = append(mounts, mount.Mount{
			Type:        mount.TypeBind,
			Source:      cfg.LocalSkillsDir,
			Target:      "/root/.claude/skills",
			ReadOnly:    true, // Mount read-only to prevent accidental modifications
			BindOptions: &mount.BindOptions{Propagation: mount.PropagationRPrivate},
		})
	}

	// Add local agent dist mount for runtime-mode=dev (if provided)
	// This overlays bundled agent dist with locally built dist for faster iteration.
	if cfg.LocalAgentDistDir != "" {
		mounts = append(mounts, mount.Mount{
			Type:        mount.TypeBind,
			Source:      cfg.LocalAgentDistDir,
			Target:      "/holon/agent/dist",
			ReadOnly:    true,
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

// BuildContainerHostConfig assembles host-level sandbox settings for containers.
// Runtime mode is included to make mode-specific policies explicit in one place.
func BuildContainerHostConfig(cfg *HostConfigOptions) *container.HostConfig {
	if cfg == nil {
		cfg = &HostConfigOptions{}
	}

	return &container.HostConfig{
		Mounts: cfg.Mounts,
		// Keep explicit non-privileged defaults for regression visibility.
		Privileged:     false,
		ReadonlyRootfs: false,
		NetworkMode:    container.NetworkMode("default"),
	}
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

// ValidateMountTargets validates mount sources and prepares optional state mounts.
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

	// Check state directory if provided (create if missing)
	if cfg.StateDir != "" {
		info, err := os.Stat(cfg.StateDir)
		if err != nil {
			if !os.IsNotExist(err) {
				return fmt.Errorf("failed to stat state directory: %s: %w", cfg.StateDir, err)
			}
			if err := os.MkdirAll(cfg.StateDir, 0755); err != nil {
				return fmt.Errorf("failed to create state directory: %s: %w", cfg.StateDir, err)
			}
		} else if !info.IsDir() {
			return fmt.Errorf("state path is not a directory: %s", cfg.StateDir)
		}
	}

	if cfg.LocalAgentDistDir != "" {
		info, err := os.Stat(cfg.LocalAgentDistDir)
		if err != nil {
			if os.IsNotExist(err) {
				return fmt.Errorf("local agent dist directory does not exist: %s", cfg.LocalAgentDistDir)
			}
			return fmt.Errorf("failed to stat local agent dist directory: %s: %w", cfg.LocalAgentDistDir, err)
		}
		if !info.IsDir() {
			return fmt.Errorf("local agent dist path is not a directory: %s", cfg.LocalAgentDistDir)
		}
	}

	// Check agent home if provided (create if missing)
	if cfg.AgentHome != "" {
		info, err := os.Stat(cfg.AgentHome)
		if err != nil {
			if !os.IsNotExist(err) {
				return fmt.Errorf("failed to stat agent home: %s: %w", cfg.AgentHome, err)
			}
			if err := os.MkdirAll(cfg.AgentHome, 0755); err != nil {
				return fmt.Errorf("failed to create agent home: %s: %w", cfg.AgentHome, err)
			}
		} else if !info.IsDir() {
			return fmt.Errorf("agent home path is not a directory: %s", cfg.AgentHome)
		}
	}

	return nil
}
