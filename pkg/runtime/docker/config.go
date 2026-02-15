package docker

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/mount"
	"github.com/holon-run/holon/pkg/api/v1"
)

const (
	ContainerAgentHome    = "/root"
	ContainerWorkspaceDir = ContainerAgentHome + "/workspace"
	ContainerInputDir     = ContainerAgentHome + "/input"
	ContainerOutputDir    = ContainerAgentHome + "/output"
	ContainerStateDir     = ContainerAgentHome + "/state"
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
	ExtraMounts          []ExtraMount
}

// ExtraMount defines a validated host path bind-mounted into the same path in container.
// Same-path mapping avoids host/container path ambiguity in user-agent conversations.
type ExtraMount struct {
	Path     string
	ReadOnly bool
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
	mounts := []mount.Mount{}

	// Add agent home mount first (if provided) so nested runtime mounts
	// (/root/workspace, /root/input, /root/output, /root/state) can overlay it.
	if cfg.AgentHome != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.AgentHome,
			Target: ContainerAgentHome,
		})
	}

	mounts = append(mounts,
		mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.SnapshotDir,
			Target: ContainerWorkspaceDir,
		},
		mount.Mount{
			Type:     mount.TypeBind,
			Source:   cfg.InputPath,
			Target:   ContainerInputDir,
			ReadOnly: true,
		},
		mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.OutDir,
			Target: ContainerOutputDir,
		},
	)

	// Add state directory mount (if provided)
	// This mounts to /root/state for cross-run skill caches.
	if cfg.StateDir != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.StateDir,
			Target: ContainerStateDir,
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

	// Add extra same-path mounts configured from agent-home runtime policy.
	for _, extra := range cfg.ExtraMounts {
		mounts = append(mounts, mount.Mount{
			Type:        mount.TypeBind,
			Source:      extra.Path,
			Target:      extra.Path,
			ReadOnly:    extra.ReadOnly,
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

	if err := validateExtraMounts(cfg.ExtraMounts); err != nil {
		return err
	}

	return nil
}

func validateExtraMounts(extra []ExtraMount) error {
	if len(extra) == 0 {
		return nil
	}
	seen := make([]string, 0, len(extra))
	for i, mount := range extra {
		path := strings.TrimSpace(mount.Path)
		if path == "" {
			return fmt.Errorf("extra mount %d path cannot be empty", i)
		}
		if !filepath.IsAbs(path) {
			return fmt.Errorf("extra mount %d path must be absolute: %s", i, path)
		}
		cleaned := filepath.Clean(path)
		if isFilesystemRoot(cleaned) {
			return fmt.Errorf("extra mount %d path cannot be filesystem root: %s", i, cleaned)
		}
		if conflictsWithReservedTargets(cleaned) {
			return fmt.Errorf("extra mount %d path conflicts with reserved runtime paths: %s", i, cleaned)
		}
		if _, err := os.Stat(cleaned); err != nil {
			if os.IsNotExist(err) {
				return fmt.Errorf("extra mount %d path does not exist: %s", i, cleaned)
			}
			return fmt.Errorf("failed to stat extra mount %d path %s: %w", i, cleaned, err)
		}
		resolved, err := filepath.EvalSymlinks(cleaned)
		if err != nil {
			return fmt.Errorf("failed to resolve symlinks for extra mount %d path %s: %w", i, cleaned, err)
		}
		resolvedAbs, err := filepath.Abs(resolved)
		if err != nil {
			return fmt.Errorf("failed to resolve absolute path for extra mount %d path %s: %w", i, resolved, err)
		}
		for idx, existing := range seen {
			if pathOverlaps(resolvedAbs, existing) {
				return fmt.Errorf("extra mount %d path %s conflicts with extra mount %d path %s", i, resolvedAbs, idx, existing)
			}
		}
		seen = append(seen, resolvedAbs)
	}
	return nil
}

func conflictsWithReservedTargets(path string) bool {
	reserved := []string{
		ContainerAgentHome,
		ContainerWorkspaceDir,
		ContainerInputDir,
		ContainerOutputDir,
		ContainerStateDir,
		"/root/.claude",
		"/root/.claude/skills",
		"/holon/agent/dist",
	}
	for _, target := range reserved {
		if pathOverlaps(path, target) {
			return true
		}
	}
	return false
}

func pathOverlaps(a, b string) bool {
	if a == b {
		return true
	}
	relAB, err := filepath.Rel(a, b)
	if err == nil && relAB != "." && relAB != ".." && !strings.HasPrefix(relAB, ".."+string(filepath.Separator)) {
		return true
	}
	relBA, err := filepath.Rel(b, a)
	if err == nil && relBA != "." && relBA != ".." && !strings.HasPrefix(relBA, ".."+string(filepath.Separator)) {
		return true
	}
	return false
}

func isFilesystemRoot(path string) bool {
	clean := filepath.Clean(path)
	if clean == string(filepath.Separator) {
		return true
	}
	volume := filepath.VolumeName(clean)
	return volume != "" && clean == volume+string(filepath.Separator)
}
