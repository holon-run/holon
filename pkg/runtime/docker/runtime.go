package docker

import (
	"archive/tar"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/client"
	"github.com/docker/docker/errdefs"
	"github.com/docker/docker/pkg/stdcopy"
	"github.com/holon-run/holon/pkg/builtin"
	"github.com/holon-run/holon/pkg/git"
	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/logs/redact"
	"github.com/holon-run/holon/pkg/skills"
	"github.com/holon-run/holon/pkg/workspace"
)

type Runtime struct {
	cli *client.Client
}

func NewRuntime() (*Runtime, error) {
	cli, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, err
	}
	return &Runtime{cli: cli}, nil
}

type ContainerConfig struct {
	BaseImage   string // e.g., golang:1.22 (The toolchain)
	AgentBundle string // Required path to agent bundle archive (.tar.gz)
	Workspace   string
	InputPath   string // Path to input directory (contains spec.yaml, context/, prompts/)
	OutDir      string
	StateDir    string // Path to state directory for cross-run skill caches (optional)
	AgentHome   string // Path to agent home mounted to /root (optional, persistent agent identity/state)
	Env         map[string]string
	Cmd         []string // Optional command override

	// Workspace preparation options
	WorkspaceStrategy    string                // Workspace preparation strategy (e.g., "git-clone", "snapshot")
	WorkspaceHistory     workspace.HistoryMode // How much git history to include
	WorkspaceRef         string                // Git ref to checkout (optional)
	WorkspaceIsTemporary bool                  // true when workspace should be used directly (already prepared, no extra snapshot)

	// Agent config mount mode
	AgentConfigMode string // Agent config mount mode: "auto", "yes", "no"

	// Git configuration (already resolved by caller)
	// These values are pre-resolved using git.ResolveConfig() with proper priority:
	// host git config > ProjectConfig > env vars > defaults
	GitAuthorName  string // Git author name for commits
	GitAuthorEmail string // Git author email for commits

	// Skills configuration
	Skills []string // Paths to skill directories to include

	// Builtin skills configuration (optional, overrides embedded skills)
	BuiltinSkillsSource string // Remote source URL for builtin skills (e.g., "https://github.com/holon-run/holon/releases/download/v1.0.0/holon-skills-v1.0.0.zip")
	BuiltinSkillsRef    string // Version/ref tag for builtin skills (e.g., "v1.0.0")

	// Skill mode configuration
	UseSkillMode bool // True if using skill mode (agent handles collect/publish)

	// Runtime mode configuration
	RuntimeMode       string // Runtime mode: "prod" (default) or "dev"
	DevAgentSourceDir string // Local agent source directory for runtime-mode=dev (expects dist/)
}

// SessionHandle tracks a long-running runtime session container.
type SessionHandle struct {
	ContainerID         string
	SnapshotDir         string
	OwnsSnapshotDir     bool
	SkillsDir           string
	logsReadCloser      io.ReadCloser
	logsPumpErr         chan error
	logsPumpInitialized bool
}

const sessionStopTimeoutSeconds = 10

func (r *Runtime) RunHolon(ctx context.Context, cfg *ContainerConfig) (string, error) {
	// 1. Prepare Workspace using WorkspacePreparer
	snapshotDir, skillsDir, _, err := prepareWorkspace(ctx, cfg)
	if err != nil {
		return "", err
	}

	// Ensure cleanup of skills directory
	if skillsDir != "" {
		defer os.RemoveAll(skillsDir)
	}

	// Note: We do NOT cleanup snapshotDir here.
	// This allows post-execution operations (like publish) to work with the actual workspace.
	// Workspace cleanup must be handled elsewhere.

	// 2. Prepare Image (Build-on-Run composition)
	if cfg.AgentBundle == "" {
		return "", fmt.Errorf("agent bundle is required")
	}
	if cfg.BaseImage == "" {
		return "", fmt.Errorf("base image is required")
	}

	holonlog.Progress("composing execution image", "base_image", cfg.BaseImage, "agent_bundle", cfg.AgentBundle)
	composedImage, err := r.buildComposedImageFromBundle(ctx, cfg.BaseImage, cfg.AgentBundle)
	if err != nil {
		return "", fmt.Errorf("failed to compose image: %w", err)
	}
	finalImage := composedImage

	// Pull final image if not present locally
	_, err = r.cli.ImageInspect(ctx, finalImage)
	if err != nil {
		holonlog.Info("image not found locally, attempting to pull", "image", finalImage)
		reader, err := r.cli.ImagePull(ctx, finalImage, image.PullOptions{})
		if err != nil {
			holonlog.Warn("failed to pull image", "image", finalImage, "error", err)
		} else {
			defer reader.Close()
			io.Copy(io.Discard, reader)
		}
	} else {
		holonlog.Debug("image found locally", "image", finalImage)
	}

	// 3. Create Container
	// Inject git identity with proper priority
	// The caller (runner.go) has already resolved git config using git.ResolveConfig()
	// Priority: host git config (local>global>system) > ProjectConfig > env vars > defaults
	// We just use the pre-resolved values here - no more double-setting/overriding

	if cfg.Env == nil {
		cfg.Env = make(map[string]string)
	}

	// Set git config environment variables for the container
	// These are the resolved values with proper priority handling
	if cfg.GitAuthorName != "" {
		cfg.Env["GIT_AUTHOR_NAME"] = cfg.GitAuthorName
		cfg.Env["GIT_COMMITTER_NAME"] = cfg.GitAuthorName
	}
	if cfg.GitAuthorEmail != "" {
		cfg.Env["GIT_AUTHOR_EMAIL"] = cfg.GitAuthorEmail
		cfg.Env["GIT_COMMITTER_EMAIL"] = cfg.GitAuthorEmail
	}

	runtimeMode, err := ParseRuntimeMode(cfg.RuntimeMode)
	if err != nil {
		return "", err
	}
	cfg.Env["HOLON_RUNTIME_MODE"] = runtimeMode.String()
	env := BuildContainerEnv(&EnvConfig{
		UserEnv: cfg.Env,
		HostUID: os.Getuid(),
		HostGID: os.Getgid(),
	})

	mountConfig := &MountConfig{
		SnapshotDir:    snapshotDir,
		InputPath:      cfg.InputPath,
		OutDir:         cfg.OutDir,
		StateDir:       cfg.StateDir,
		AgentHome:      cfg.AgentHome,
		LocalSkillsDir: skillsDir, // NEW: Pass skills directory
	}
	if runtimeMode == RuntimeModeDev {
		distDir, err := resolveDevAgentDistDir(cfg.DevAgentSourceDir)
		if err != nil {
			return "", err
		}
		mountConfig.LocalAgentDistDir = distDir
		holonlog.Info("runtime mode dev enabled", "agent_dist", distDir)
	}

	// Handle agent config mounting based on mode
	// Parse the config mode, default to "no" if empty or invalid
	configMode, err := ParseAgentConfigMode(cfg.AgentConfigMode)
	if err != nil {
		holonlog.Warn("invalid agent config mode, defaulting to 'no'", "mode", cfg.AgentConfigMode, "error", err)
		configMode = AgentConfigModeNo
	}

	// For "no" mode, skip entirely
	if configMode != AgentConfigModeNo {
		homeDir, err := os.UserHomeDir()
		if err != nil {
			holonlog.Warn("failed to get home directory", "error", err)
		} else {
			claudeDir := filepath.Join(homeDir, ".claude")
			dirExists := true
			if _, err := os.Stat(claudeDir); err != nil {
				if os.IsNotExist(err) {
					dirExists = false
				} else {
					holonlog.Warn("failed to stat ~/.claude", "error", err)
					dirExists = false
				}
			}

			// Determine whether to mount based on mode and directory existence
			shouldMount := configMode.ShouldMount(dirExists)
			shouldWarn := configMode.WarnIfMissing() && !dirExists

			if shouldWarn {
				holonlog.Warn("--agent-config-mode=yes specified, but ~/.claude does not exist")
			}

			if shouldMount && dirExists {
				// For "auto" mode, check if the config is compatible before mounting
				// For "yes" mode, skip the compatibility check and force mount
				if configMode == AgentConfigModeAuto && isIncompatibleClaudeConfig(claudeDir) {
					holonlog.Warn("skipping mount of ~/.claude: config appears incompatible (likely headless/container Claude)")
					holonlog.Info("to force mount anyway, use --agent-config-mode=yes (use with caution)")
				} else {
					// Mount the config directory
					mountConfig.LocalClaudeConfigDir = claudeDir
					holonlog.Warn("mounting host ~/.claude into container")
					holonlog.Warn("this exposes your personal Claude login and session to the container")
					holonlog.Warn("do NOT use this in CI or shared environments")
					// Set environment variable to indicate mounted config is available
					// Add directly to env slice since BuildContainerEnv was already called
					env = append(env, "HOLON_MOUNTED_CLAUDE_CONFIG=1")
				}
			}
		}
	}

	if err := ValidateMountTargets(mountConfig); err != nil {
		return "", fmt.Errorf("validating mount targets: %w", err)
	}

	mounts := BuildContainerMounts(mountConfig)

	holonlog.Progress("creating container", "image", finalImage)
	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      finalImage,
		Cmd:        cfg.Cmd,
		Env:        env,
		WorkingDir: ContainerWorkspaceDir,
		Tty:        false,
	}, &container.HostConfig{
		Mounts: mounts,
	}, nil, nil, "")
	if err != nil {
		return "", fmt.Errorf("failed to create container: %w", err)
	}

	// 4. Start Container
	holonlog.Progress("starting container", "id", resp.ID[:12])
	if err := r.cli.ContainerStart(ctx, resp.ID, container.StartOptions{}); err != nil {
		return "", fmt.Errorf("failed to start container: %w", err)
	}

	// 4.5 Stream Logs
	holonlog.Debug("streaming container logs", "id", resp.ID[:12])
	out, err := r.cli.ContainerLogs(ctx, resp.ID, container.LogsOptions{
		ShowStdout: true,
		ShowStderr: true,
		Follow:     true,
	})
	if err == nil {
		defer out.Close()
		go io.Copy(os.Stdout, out)
	}

	// 5. Wait for completion
	holonlog.Progress("waiting for container completion", "id", resp.ID[:12])
	statusCh, errCh := r.cli.ContainerWait(ctx, resp.ID, container.WaitConditionNotRunning)
	select {
	case err := <-errCh:
		if err != nil {
			return "", fmt.Errorf("container wait error: %w", err)
		}
	case status := <-statusCh:
		if status.StatusCode != 0 {
			return "", fmt.Errorf("container failed with exit code %d", status.StatusCode)
		}
	}

	// 6. Log Redaction (CI security)
	// Redact sensitive information from logs before artifact validation
	// This prevents secret leakage in public CI logs
	if err := redactLogs(cfg.OutDir); err != nil {
		// Log but don't fail on redaction errors
		// Redaction is a security best-effort, not a blocking operation
		holonlog.Warn("failed to redact logs", "error", err)
	}

	// 7. Artifact Validation (RFC-0002)
	// Read the spec to verify required artifacts, plus manifest.json
	// For now, validate basic manifest.json requirement
	if err := ValidateRequiredArtifacts(cfg.OutDir, nil); err != nil {
		return "", err
	}

	return snapshotDir, nil
}

// StartSession starts a long-running Holon container session.
// Unlike RunHolon, this does not wait for completion or validate output artifacts.
func (r *Runtime) StartSession(ctx context.Context, cfg *ContainerConfig) (*SessionHandle, error) {
	// 1. Prepare Workspace using WorkspacePreparer
	snapshotDir, skillsDir, _, err := prepareWorkspace(ctx, cfg)
	if err != nil {
		return nil, err
	}
	ownsSnapshotDir := snapshotDir != cfg.Workspace
	cleanupOnError := func() {
		if skillsDir != "" {
			_ = os.RemoveAll(skillsDir)
		}
		if ownsSnapshotDir && snapshotDir != "" {
			_ = os.RemoveAll(snapshotDir)
		}
	}

	// 2. Prepare Image (Build-on-Run composition)
	if cfg.AgentBundle == "" {
		cleanupOnError()
		return nil, fmt.Errorf("agent bundle is required")
	}
	if cfg.BaseImage == "" {
		cleanupOnError()
		return nil, fmt.Errorf("base image is required")
	}

	holonlog.Progress("composing execution image", "base_image", cfg.BaseImage, "agent_bundle", cfg.AgentBundle)
	composedImage, err := r.buildComposedImageFromBundle(ctx, cfg.BaseImage, cfg.AgentBundle)
	if err != nil {
		cleanupOnError()
		return nil, fmt.Errorf("failed to compose image: %w", err)
	}
	finalImage := composedImage

	// Pull final image if not present locally
	_, err = r.cli.ImageInspect(ctx, finalImage)
	if err != nil {
		holonlog.Info("image not found locally, attempting to pull", "image", finalImage)
		reader, err := r.cli.ImagePull(ctx, finalImage, image.PullOptions{})
		if err != nil {
			holonlog.Warn("failed to pull image", "image", finalImage, "error", err)
		} else {
			defer reader.Close()
			_, _ = io.Copy(io.Discard, reader)
		}
	} else {
		holonlog.Debug("image found locally", "image", finalImage)
	}

	// 3. Create Container
	if cfg.Env == nil {
		cfg.Env = make(map[string]string)
	}
	if cfg.GitAuthorName != "" {
		cfg.Env["GIT_AUTHOR_NAME"] = cfg.GitAuthorName
		cfg.Env["GIT_COMMITTER_NAME"] = cfg.GitAuthorName
	}
	if cfg.GitAuthorEmail != "" {
		cfg.Env["GIT_AUTHOR_EMAIL"] = cfg.GitAuthorEmail
		cfg.Env["GIT_COMMITTER_EMAIL"] = cfg.GitAuthorEmail
	}

	runtimeMode, err := ParseRuntimeMode(cfg.RuntimeMode)
	if err != nil {
		cleanupOnError()
		return nil, err
	}
	cfg.Env["HOLON_RUNTIME_MODE"] = runtimeMode.String()
	env := BuildContainerEnv(&EnvConfig{
		UserEnv: cfg.Env,
		HostUID: os.Getuid(),
		HostGID: os.Getgid(),
	})

	mountConfig := &MountConfig{
		SnapshotDir:    snapshotDir,
		InputPath:      cfg.InputPath,
		OutDir:         cfg.OutDir,
		StateDir:       cfg.StateDir,
		AgentHome:      cfg.AgentHome,
		LocalSkillsDir: skillsDir,
	}
	if runtimeMode == RuntimeModeDev {
		distDir, err := resolveDevAgentDistDir(cfg.DevAgentSourceDir)
		if err != nil {
			cleanupOnError()
			return nil, err
		}
		mountConfig.LocalAgentDistDir = distDir
		holonlog.Info("runtime mode dev enabled", "agent_dist", distDir)
	}

	configMode, err := ParseAgentConfigMode(cfg.AgentConfigMode)
	if err != nil {
		holonlog.Warn("invalid agent config mode, defaulting to 'no'", "mode", cfg.AgentConfigMode, "error", err)
		configMode = AgentConfigModeNo
	}
	if configMode != AgentConfigModeNo {
		homeDir, err := os.UserHomeDir()
		if err != nil {
			holonlog.Warn("failed to get home directory", "error", err)
		} else {
			claudeDir := filepath.Join(homeDir, ".claude")
			dirExists := true
			if _, err := os.Stat(claudeDir); err != nil {
				if os.IsNotExist(err) {
					dirExists = false
				} else {
					holonlog.Warn("failed to stat ~/.claude", "error", err)
					dirExists = false
				}
			}
			shouldMount := configMode.ShouldMount(dirExists)
			shouldWarn := configMode.WarnIfMissing() && !dirExists
			if shouldWarn {
				holonlog.Warn("--agent-config-mode=yes specified, but ~/.claude does not exist")
			}
			if shouldMount && dirExists {
				if configMode == AgentConfigModeAuto && isIncompatibleClaudeConfig(claudeDir) {
					holonlog.Warn("skipping mount of ~/.claude: config appears incompatible (likely headless/container Claude)")
					holonlog.Info("to force mount anyway, use --agent-config-mode=yes (use with caution)")
				} else {
					mountConfig.LocalClaudeConfigDir = claudeDir
					holonlog.Warn("mounting host ~/.claude into container")
					holonlog.Warn("this exposes your personal Claude login and session to the container")
					holonlog.Warn("do NOT use this in CI or shared environments")
					env = append(env, "HOLON_MOUNTED_CLAUDE_CONFIG=1")
				}
			}
		}
	}

	if err := ValidateMountTargets(mountConfig); err != nil {
		cleanupOnError()
		return nil, fmt.Errorf("validating mount targets: %w", err)
	}

	mounts := BuildContainerMounts(mountConfig)
	holonlog.Progress("creating session container", "image", finalImage)
	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      finalImage,
		Cmd:        cfg.Cmd,
		Env:        env,
		WorkingDir: ContainerWorkspaceDir,
		Tty:        false,
	}, &container.HostConfig{
		Mounts: mounts,
	}, nil, nil, "")
	if err != nil {
		cleanupOnError()
		return nil, fmt.Errorf("failed to create container: %w", err)
	}

	holonlog.Progress("starting session container", "id", resp.ID[:12])
	if err := r.cli.ContainerStart(ctx, resp.ID, container.StartOptions{}); err != nil {
		// Best-effort cleanup for created but not started container.
		if rmErr := r.cli.ContainerRemove(ctx, resp.ID, container.RemoveOptions{Force: true}); rmErr != nil {
			holonlog.Debug("container remove after start failure returned error", "id", resp.ID, "error", rmErr)
		}
		cleanupOnError()
		return nil, fmt.Errorf("failed to start container: %w", err)
	}

	handle := &SessionHandle{
		ContainerID:     resp.ID,
		SnapshotDir:     snapshotDir,
		OwnsSnapshotDir: ownsSnapshotDir,
		SkillsDir:       skillsDir,
	}

	out, err := r.cli.ContainerLogs(ctx, resp.ID, container.LogsOptions{
		ShowStdout: true,
		ShowStderr: true,
		Follow:     true,
	})
	if err == nil {
		handle.logsReadCloser = out
		handle.logsPumpErr = make(chan error, 1)
		handle.logsPumpInitialized = true
		go func() {
			defer out.Close()
			_, cpErr := stdcopy.StdCopy(os.Stdout, os.Stderr, out)
			if cpErr != nil {
				handle.logsPumpErr <- cpErr
				return
			}
			handle.logsPumpErr <- nil
		}()
	}

	return handle, nil
}

// WaitSession waits for a session container to exit.
func (r *Runtime) WaitSession(ctx context.Context, handle *SessionHandle) error {
	if handle == nil || handle.ContainerID == "" {
		return fmt.Errorf("session handle is required")
	}
	statusCh, errCh := r.cli.ContainerWait(ctx, handle.ContainerID, container.WaitConditionNotRunning)
	select {
	case err := <-errCh:
		if err != nil {
			return fmt.Errorf("container wait error: %w", err)
		}
	case status := <-statusCh:
		if status.StatusCode != 0 {
			return fmt.Errorf("container failed with exit code %d", status.StatusCode)
		}
	}
	if err := r.cli.ContainerRemove(ctx, handle.ContainerID, container.RemoveOptions{Force: true}); err != nil {
		holonlog.Debug("container remove returned error", "id", handle.ContainerID, "error", err)
	}
	r.closeSessionLogStream(handle)
	r.cleanupSessionHandle(handle)
	return nil
}

// StopSession stops and removes a session container.
func (r *Runtime) StopSession(ctx context.Context, handle *SessionHandle) error {
	if handle == nil || handle.ContainerID == "" {
		return nil
	}
	timeout := sessionStopTimeoutSeconds
	var stopErr error
	if err := r.cli.ContainerStop(ctx, handle.ContainerID, container.StopOptions{Timeout: &timeout}); err != nil {
		if !errdefs.IsNotFound(err) {
			stopErr = fmt.Errorf("failed to stop session container %s: %w", handle.ContainerID, err)
		}
	}
	var removeErr error
	if err := r.cli.ContainerRemove(ctx, handle.ContainerID, container.RemoveOptions{Force: true}); err != nil {
		if !errdefs.IsNotFound(err) {
			removeErr = fmt.Errorf("failed to remove session container %s: %w", handle.ContainerID, err)
		}
	}
	r.closeSessionLogStream(handle)
	r.cleanupSessionHandle(handle)
	if stopErr != nil {
		if removeErr != nil {
			return fmt.Errorf("%v; %w", stopErr, removeErr)
		}
		return stopErr
	}
	if removeErr != nil {
		return removeErr
	}
	return nil
}

func (r *Runtime) cleanupSessionHandle(handle *SessionHandle) {
	if handle.SkillsDir != "" {
		_ = os.RemoveAll(handle.SkillsDir)
		handle.SkillsDir = ""
	}
	if handle.OwnsSnapshotDir && handle.SnapshotDir != "" {
		_ = os.RemoveAll(handle.SnapshotDir)
		handle.SnapshotDir = ""
	}
}

func (r *Runtime) closeSessionLogStream(handle *SessionHandle) {
	if handle == nil || !handle.logsPumpInitialized {
		return
	}
	if handle.logsReadCloser != nil {
		_ = handle.logsReadCloser.Close()
		handle.logsReadCloser = nil
	}
	select {
	case err := <-handle.logsPumpErr:
		if err != nil {
			holonlog.Debug("session log stream exited with error", "container_id", handle.ContainerID, "error", err)
		}
	default:
	}
	handle.logsPumpInitialized = false
}

func (r *Runtime) buildComposedImageFromBundle(ctx context.Context, baseImage, bundlePath string) (string, error) {
	tmpDir, err := os.MkdirTemp("", "holon-build-*")
	if err != nil {
		return "", err
	}
	defer os.RemoveAll(tmpDir)

	bundleDigest, err := hashFile(bundlePath)
	if err != nil {
		return "", fmt.Errorf("failed to hash agent bundle: %w", err)
	}

	runtimeVersion, err := readBundleRuntimeVersion(bundlePath)
	if err != nil {
		return "", fmt.Errorf("failed to read bundle manifest: %w", err)
	}
	nodeMajor := nodeMajorVersion(runtimeVersion)

	bundleName := "agent-bundle.tar.gz"
	bundleDest := filepath.Join(tmpDir, bundleName)
	if err := copyFile(bundlePath, bundleDest); err != nil {
		return "", fmt.Errorf("failed to stage agent bundle: %w", err)
	}

	dockerfile := fmt.Sprintf(`
FROM %s
ARG NODE_MAJOR=%s
SHELL ["/bin/sh", "-c"]

RUN set -e; \
    if command -v apt-get >/dev/null 2>&1; then \
        apt-get update; \
        apt-get install -y --no-install-recommends curl ca-certificates git gnupg; \
        curl -fsSL https://deb.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        apt-get install -y --no-install-recommends nodejs; \
        rm -rf /var/lib/apt/lists/*; \
        if ! command -v gh >/dev/null 2>&1; then \
            curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg; \
            chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg; \
            echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | tee /etc/apt/sources.list.d/github-cli.list >/dev/null; \
            apt-get update; \
            apt-get install -y --no-install-recommends gh || true; \
            rm -rf /var/lib/apt/lists/*; \
        fi; \
        if command -v gh >/dev/null 2>&1; then \
            gh extension install cli/gh-webhook 2>/dev/null || true; \
        fi; \
    elif command -v dnf >/dev/null 2>&1; then \
        dnf install -y curl ca-certificates git; \
        curl -fsSL https://rpm.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        dnf install -y nodejs; \
        if ! command -v gh >/dev/null 2>&1; then \
            curl -o /etc/yum.repos.d/gh-cli.repo https://cli.github.com/packages/rpm/gh-cli.repo; \
            dnf install -y gh || true; \
        fi; \
        if command -v gh >/dev/null 2>&1; then \
            gh extension install cli/gh-webhook 2>/dev/null || true; \
        fi; \
    elif command -v yum >/dev/null 2>&1; then \
        yum install -y curl ca-certificates git; \
        curl -fsSL https://rpm.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        yum install -y nodejs; \
        if ! command -v gh >/dev/null 2>&1; then \
            yum install -y yum-utils; \
            yum-config-manager --add-repo https://cli.github.com/packages/rpm/gh-cli.repo; \
            yum install -y gh || true; \
        fi; \
        if command -v gh >/dev/null 2>&1; then \
            gh extension install cli/gh-webhook 2>/dev/null || true; \
        fi; \
    else \
        echo "Unsupported base image: no apt-get, dnf, or yum detected." >&2; \
        exit 1; \
    fi

COPY %s /holon/agent-bundle.tar.gz
RUN mkdir -p /holon/agent && tar -xzf /holon/agent-bundle.tar.gz -C /holon/agent

ENV PATH="/holon/agent/node_modules/.bin:${PATH}"
ENV IS_SANDBOX=1
WORKDIR %s
ENTRYPOINT ["/holon/agent/bin/agent"]
`, baseImage, nodeMajor, bundleName, ContainerWorkspaceDir)

	dfPath := filepath.Join(tmpDir, "Dockerfile")
	if err := os.WriteFile(dfPath, []byte(dockerfile), 0644); err != nil {
		return "", err
	}

	tag := composeImageTag(baseImage, bundleDigest)
	cmd := exec.Command("docker", "build", "-t", tag, tmpDir)
	if out, err := cmd.CombinedOutput(); err != nil {
		return "", fmt.Errorf("composition build failed: %v, output: %s", err, string(out))
	}

	return tag, nil
}

func composeImageTag(baseImage, bundleDigest string) string {
	hashInput := baseImage + ":" + bundleDigest
	hash := sha256.Sum256([]byte(hashInput))
	return fmt.Sprintf("holon-composed-%x", hash[:12])
}

type bundleManifest struct {
	Runtime struct {
		Type    string `json:"type"`
		Version string `json:"version"`
	} `json:"runtime"`
}

func readBundleRuntimeVersion(bundlePath string) (string, error) {
	file, err := os.Open(bundlePath)
	if err != nil {
		return "", err
	}
	defer file.Close()

	gz, err := gzip.NewReader(file)
	if err != nil {
		return "", err
	}
	defer gz.Close()

	tr := tar.NewReader(gz)
	for {
		header, err := tr.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return "", err
		}
		name := strings.TrimPrefix(header.Name, "./")
		if name != "manifest.json" {
			continue
		}
		payload, err := io.ReadAll(tr)
		if err != nil {
			return "", err
		}
		var manifest bundleManifest
		if err := json.Unmarshal(payload, &manifest); err != nil {
			return "", err
		}
		return manifest.Runtime.Version, nil
	}

	return "", fmt.Errorf("manifest.json not found in bundle")
}

func nodeMajorVersion(version string) string {
	if version == "" || version == "unknown" {
		return "20"
	}
	trimmed := strings.TrimPrefix(version, "v")
	parts := strings.Split(trimmed, ".")
	if len(parts) == 0 {
		return "20"
	}
	if _, err := strconv.Atoi(parts[0]); err != nil {
		return "20"
	}
	return parts[0]
}

func hashFile(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return "", err
	}
	return fmt.Sprintf("%x", hash.Sum(nil)), nil
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	defer out.Close()
	if _, err := io.Copy(out, in); err != nil {
		return err
	}
	return out.Sync()
}

// prepareSkillsDir creates a temporary directory for skills staging
func prepareSkillsDir() (string, error) {
	skillsDir, err := os.MkdirTemp("", "holon-skills-*")
	if err != nil {
		return "", fmt.Errorf("failed to create skills dir: %w", err)
	}
	// Convert to absolute path for Docker bind mount
	absSkillsDir, err := filepath.Abs(skillsDir)
	if err != nil {
		os.RemoveAll(skillsDir) // Cleanup on error
		return "", fmt.Errorf("failed to get absolute path for skills dir: %w", err)
	}
	return absSkillsDir, nil
}

func resolveDevAgentDistDir(sourceDir string) (string, error) {
	src := strings.TrimSpace(sourceDir)
	if src == "" {
		return "", fmt.Errorf("runtime-mode=dev requires a local agent source directory (--runtime-dev-agent-source or HOLON_RUNTIME_DEV_AGENT_SOURCE)")
	}
	absSource, err := filepath.Abs(src)
	if err != nil {
		return "", fmt.Errorf("failed to resolve runtime dev agent source path: %w", err)
	}
	distDir := filepath.Join(absSource, "dist")
	info, err := os.Stat(distDir)
	if err != nil {
		if os.IsNotExist(err) {
			return "", fmt.Errorf("runtime-mode=dev requires %s to exist; run 'npm run build' in %s first", distDir, absSource)
		}
		return "", fmt.Errorf("failed to stat dev agent dist directory %s: %w", distDir, err)
	}
	if !info.IsDir() {
		return "", fmt.Errorf("dev agent dist path is not a directory: %s", distDir)
	}
	agentEntry := filepath.Join(distDir, "agent.js")
	if _, err := os.Stat(agentEntry); err != nil {
		if os.IsNotExist(err) {
			return "", fmt.Errorf("runtime-mode=dev requires %s; run 'npm run build' in %s first", agentEntry, absSource)
		}
		return "", fmt.Errorf("failed to stat dev agent entry %s: %w", agentEntry, err)
	}
	return distDir, nil
}

// prepareWorkspace prepares the workspace using the configured strategy
func prepareWorkspace(ctx context.Context, cfg *ContainerConfig) (string, string, workspace.Preparer, error) {
	// If workspace is already prepared for direct use,
	// use it directly instead of creating another snapshot.
	if cfg.WorkspaceIsTemporary {
		holonlog.Info("using workspace directly (no snapshot needed)", "workspace", cfg.Workspace)

		// Use an ExistingPreparer to prepare the workspace and
		// still write the workspace manifest so downstream consumers see
		// consistent metadata regardless of how the workspace was created.
		preparer := workspace.NewExistingPreparer()

		// Determine history mode for manifest generation
		historyMode := cfg.WorkspaceHistory
		if historyMode == "" {
			historyMode = workspace.HistoryFull // Default to full history
		}

		prepareResult, err := preparer.Prepare(ctx, workspace.PrepareRequest{
			Source:     cfg.Workspace,
			Dest:       cfg.Workspace,
			Ref:        cfg.WorkspaceRef,
			History:    historyMode,
			Submodules: workspace.SubmodulesNone,
			CleanDest:  false,
		})
		if err != nil {
			return "", "", nil, fmt.Errorf("failed to prepare workspace for direct use: %w", err)
		}

		// Create skills staging directory
		skillsDir, err := prepareSkillsDir()
		if err != nil {
			return "", "", nil, fmt.Errorf("failed to create skills dir: %w", err)
		}

		// Stage skills to dedicated directory (not workspace)
		resolvedSkills, err := resolveSkills(ctx, cfg)
		if err != nil {
			os.RemoveAll(skillsDir) // Cleanup on error
			return "", "", nil, fmt.Errorf("failed to resolve skills: %w", err)
		}
		if len(resolvedSkills) > 0 {
			holonlog.Info("staging skills", "count", len(resolvedSkills))
			// Stage skills without .claude/skills prefix since skillsDir is mounted to /root/.claude/skills
			if err := skills.StageWithPrefix(skillsDir, resolvedSkills, false); err != nil {
				os.RemoveAll(skillsDir) // Cleanup on error
				return "", "", nil, fmt.Errorf("failed to stage skills: %w", err)
			}
			for _, skill := range resolvedSkills {
				holonlog.Debug("staged skill", "name", skill.Name, "source", skill.Source)
			}
		}

		// Write workspace manifest to output directory if specified
		if cfg.OutDir != "" {
			commit, source, ref := builtinSkillsManifestFields(cfg, resolvedSkills)
			prepareResult.BuiltinSkillsCommit = commit
			prepareResult.BuiltinSkillsSource = source
			prepareResult.BuiltinSkillsRef = ref
			if err := writeWorkspaceManifest(cfg.OutDir, prepareResult); err != nil {
				os.RemoveAll(skillsDir) // Cleanup on error
				return "", "", nil, fmt.Errorf("failed to write workspace manifest: %w", err)
			}
		}

		// Return the workspace as-is with an existing preparer (no-op cleanup)
		return cfg.Workspace, skillsDir, preparer, nil
	}

	// Create snapshot directory outside workspace
	snapshotDir, err := workspace.MkdirTempOutsideWorkspace(cfg.Workspace, "holon-workspace-*")
	if err != nil {
		return "", "", nil, fmt.Errorf("failed to create snapshot dir: %w", err)
	}

	// Determine the strategy to use
	strategyName := cfg.WorkspaceStrategy
	if strategyName == "" {
		// Auto-detect: use git-clone for git repos, snapshot otherwise
		client := git.NewClient(cfg.Workspace)
		if client.IsRepo(ctx) {
			strategyName = "git-clone"
		} else {
			strategyName = "snapshot"
		}
	}

	// Get the preparer
	preparer := workspace.Get(strategyName)
	if preparer == nil {
		os.RemoveAll(snapshotDir)
		return "", "", nil, fmt.Errorf("workspace strategy '%s' not found", strategyName)
	}

	// Determine history mode
	historyMode := cfg.WorkspaceHistory
	if historyMode == "" {
		historyMode = workspace.HistoryFull // Default to full history
	}

	// Prepare the workspace
	holonlog.Progress("preparing workspace", "strategy", strategyName)
	result, err := preparer.Prepare(ctx, workspace.PrepareRequest{
		Source:     cfg.Workspace,
		Dest:       snapshotDir,
		Ref:        cfg.WorkspaceRef,
		History:    historyMode,
		Submodules: workspace.SubmodulesNone,
		CleanDest:  true,
	})

	if err != nil {
		os.RemoveAll(snapshotDir)
		return "", "", nil, fmt.Errorf("failed to prepare workspace: %w", err)
	}

	// IMPORTANT: Fix the origin URL when cloning from a local git repo
	// When using git clone --local, origin points to the local path
	// We need to preserve the correct GitHub origin from the source workspace
	if strategyName == "git-clone" {
		sourceClient := git.NewClient(cfg.Workspace)
		if sourceClient.IsRepo(ctx) {
			// Try to get the origin URL from the source workspace
			if originURL, err := sourceClient.ConfigGet(ctx, "remote.origin.url"); err == nil && originURL != "" {
				// Check if the source origin is a GitHub URL (not a local path)
				if strings.HasPrefix(originURL, "https://github.com/") || strings.HasPrefix(originURL, "git@github.com:") {
					snapshotClient := git.NewClient(snapshotDir)
					if err := snapshotClient.SetRemote(ctx, "origin", originURL); err == nil {
						holonlog.Info("preserved origin from source", "url", originURL)
					} else {
						holonlog.Warn("failed to preserve origin from source", "url", originURL, "error", err)
					}

					// Configure git credential helper for GitHub HTTPS authentication.
					// This works for both skill mode and traditional mode:
					// - Skill mode: agent can push branches using gh auth
					// - Traditional mode: publisher can push (simplifies publisher logic)
					if err := snapshotClient.ConfigCredentialHelper(ctx, git.GitHubCredentialHelperScript); err != nil {
						holonlog.Warn("failed to configure git credential helper", "error", err)
					} else {
						holonlog.Info("configured git credential helper for GitHub auth")
					}
				}
			}
		}
	}

	// Log preparation details
	holonlog.Info("workspace prepared", "strategy", result.Strategy, "head", result.HeadSHA, "has_history", result.HasHistory, "is_shallow", result.IsShallow)

	// Log any notes
	for _, note := range result.Notes {
		holonlog.Info("workspace note", "note", note)
	}

	// Create skills staging directory
	skillsDir, err := prepareSkillsDir()
	if err != nil {
		os.RemoveAll(snapshotDir) // Cleanup snapshot on error
		return "", "", nil, fmt.Errorf("failed to create skills dir: %w", err)
	}

	// Stage skills to dedicated directory (not workspace)
	resolvedSkills, err := resolveSkills(ctx, cfg)
	if err != nil {
		os.RemoveAll(snapshotDir) // Cleanup snapshot on error
		os.RemoveAll(skillsDir)   // Cleanup skills on error
		return "", "", nil, fmt.Errorf("failed to resolve skills: %w", err)
	}
	if len(resolvedSkills) > 0 {
		holonlog.Info("staging skills", "count", len(resolvedSkills))
		// Stage skills without .claude/skills prefix since skillsDir is mounted to /root/.claude/skills
		if err := skills.StageWithPrefix(skillsDir, resolvedSkills, false); err != nil {
			os.RemoveAll(snapshotDir) // Cleanup snapshot on error
			os.RemoveAll(skillsDir)   // Cleanup skills on error
			return "", "", nil, fmt.Errorf("failed to stage skills: %w", err)
		}
		for _, skill := range resolvedSkills {
			holonlog.Debug("staged skill", "name", skill.Name, "source", skill.Source)
		}
	}

	// Write workspace manifest to output directory (not workspace)
	// This avoids polluting the workspace with metadata files
	if cfg.OutDir != "" {
		commit, source, ref := builtinSkillsManifestFields(cfg, resolvedSkills)
		result.BuiltinSkillsCommit = commit
		result.BuiltinSkillsSource = source
		result.BuiltinSkillsRef = ref
		if err := writeWorkspaceManifest(cfg.OutDir, result); err != nil {
			holonlog.Warn("failed to write workspace manifest", "error", err)
		}
	}

	return snapshotDir, skillsDir, preparer, nil
}

// writeWorkspaceManifest writes the workspace manifest to the output directory
func writeWorkspaceManifest(outDir string, result workspace.PrepareResult) error {
	// Ensure output directory exists
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		return fmt.Errorf("failed to create output directory: %w", err)
	}

	// Delegate to the shared workspace manifest writer to avoid duplicating logic
	return workspace.WriteManifest(outDir, result)
}

// resolveSkills validates skills from ContainerConfig.Skills and merges with auto-discovered skills
// Returns empty list if no skills are configured
func resolveSkills(ctx context.Context, cfg *ContainerConfig) ([]skills.Skill, error) {
	resolver := skills.NewResolver(cfg.Workspace)

	// Skills from ContainerConfig.Skills can be:
	// - Builtin skill references (e.g., "ghx")
	// - Local filesystem paths
	// - Remote URLs
	// Use Resolve to handle all types properly. When no skills are explicitly
	// provided, this still returns auto-discovered workspace skills.
	resolved, err := resolver.Resolve(cfg.Skills, []string{}, []string{})
	if err != nil {
		return nil, err
	}

	// Always make builtin skills available in the container so entry skills can
	// invoke shared helper skills (e.g. ghx).
	// Keep explicit/workspace skills first; append builtin defaults only if not
	// already present by skill name.
	//
	// If a remote builtin source is configured, use it instead of embedded skills.
	// Otherwise, fall back to embedded builtin skills.
	var builtinSkillsList []string
	builtinSource := "builtin-default"

	if cfg.BuiltinSkillsSource != "" {
		// Use remote builtin skills
		holonlog.Info("loading builtin skills from remote source", "url", cfg.BuiltinSkillsSource, "ref", cfg.BuiltinSkillsRef)

		// Resolve the configured remote source directly.
		// BuiltinSkillsRef is metadata for auditing/version pin visibility.
		remoteSkills, err := resolver.Resolve([]string{cfg.BuiltinSkillsSource}, []string{}, []string{})
		if err != nil {
			holonlog.Warn("failed to load remote builtin skills, falling back to embedded", "error", err)
			// Fall back to embedded skills
			builtinSkillsList, err = builtin.List()
			if err != nil {
				return nil, fmt.Errorf("failed to list builtin skills: %w", err)
			}
			builtinSource = "builtin-fallback"
		} else {
			// Use the remote skills as builtin skills
			for _, skill := range remoteSkills {
				name := filepath.Base(skill.Path)
				alreadyPresent := false
				for _, existingSkill := range resolved {
					if existingSkill.Name == name {
						alreadyPresent = true
						break
					}
				}
				if !alreadyPresent {
					resolved = append(resolved, skills.Skill{
						Path:    skill.Path,
						Name:    name,
						Source:  "builtin-remote",
						Builtin: false, // Remote skills are not embedded
					})
				}
			}
			builtinSource = "builtin-remote"
		}
	} else {
		// Use embedded builtin skills
		builtinSkillsList, err = builtin.List()
		if err != nil {
			return nil, fmt.Errorf("failed to list builtin skills: %w", err)
		}
	}

	// Add builtin skills (either embedded or from list)
	for _, ref := range builtinSkillsList {
		name := filepath.Base(ref)
		alreadyPresent := false
		for _, skill := range resolved {
			if skill.Name == name {
				alreadyPresent = true
				break
			}
		}
		if alreadyPresent {
			continue
		}
		resolved = append(resolved, skills.Skill{
			Path:    ref,
			Name:    name,
			Source:  builtinSource,
			Builtin: true,
		})
	}

	return resolved, nil
}

// builtinSkillsManifestFields returns the manifest fields for builtin skill provenance.
// - remote builtin configured (even if fell back to embedded): source/ref set, commit empty
// - embedded/default: commit set, source/ref empty
func builtinSkillsManifestFields(cfg *ContainerConfig, resolved []skills.Skill) (commit, source, ref string) {
	// If remote builtin skills were configured, record the source/ref in the manifest
	// regardless of whether they loaded successfully or fell back to embedded skills.
	// This provides an audit trail of what was intended vs what actually happened.
	if cfg.BuiltinSkillsSource != "" {
		return "", cfg.BuiltinSkillsSource, cfg.BuiltinSkillsRef
	}
	// No remote source configured, using embedded builtin skills
	return builtin.GitCommit(), "", ""
}

// containsSkill checks if a skill is already in the list (by path)
func containsSkill(skills []skills.Skill, skill skills.Skill) bool {
	for _, s := range skills {
		if s.Path == skill.Path {
			return true
		}
	}
	return false
}

// isIncompatibleClaudeConfig checks if a ~/.claude config directory appears
// incompatible with mounting into a container. This detects headless/container
// Claude configs that may cause failures when mounted.
//
// Returns true if the config appears incompatible (should skip mount).
func isIncompatibleClaudeConfig(claudeDir string) bool {
	// Check for settings.json - the main Claude config file
	settingsPath := filepath.Join(claudeDir, "settings.json")
	data, err := os.ReadFile(settingsPath)
	if err != nil {
		// If we can't read the file, assume it's compatible
		// (don't block mount on read errors)
		return false
	}

	// Parse JSON to check for headless/container Claude indicators
	// This is more robust than string matching and handles formatting variations
	var config map[string]interface{}
	if err := json.Unmarshal(data, &config); err != nil {
		// If JSON parsing fails, assume it's compatible
		// (don't block mount on parse errors)
		return false
	}

	// Check for "container" or "headless" mode indicators
	// These are the most common markers of incompatible configs
	if container, ok := config["container"].(bool); ok && container {
		return true
	}
	if headless, ok := config["headless"].(bool); ok && headless {
		return true
	}
	// Check for IS_SANDBOX environment variable indicator
	if isSandbox, ok := config["IS_SANDBOX"].(string); ok && isSandbox == "1" {
		return true
	}

	return false
}

// redactLogs applies log redaction to sensitive files in the output directory.
// This prevents secret leakage in CI logs and artifacts.
func redactLogs(outDir string) error {
	// Create redactor from environment variables
	redactor := redact.RedactFromEnv()

	// Files to redact (if they exist)
	// - execution.log: Main execution log from the agent
	// - *.log: Any other log files
	filesToRedact := []string{
		"evidence/execution.log",
		"execution.log",
	}

	for _, file := range filesToRedact {
		filePath := filepath.Join(outDir, file)
		if _, err := os.Stat(filePath); err == nil {
			holonlog.Info("redacting sensitive data from log", "file", file)
			if err := redactor.RedactFile(filePath); err != nil {
				return fmt.Errorf("failed to redact %s: %w", file, err)
			}
		}
	}

	// Also scan for other .log files in the output directory
	walkErr := filepath.Walk(outDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			// Best-effort scan: ignore per-path access errors and continue redacting other files.
			return nil // Skip errors accessing individual files
		}
		if info.IsDir() {
			return nil
		}
		if strings.HasSuffix(info.Name(), ".log") {
			// Skip already processed files
			relPath, err := filepath.Rel(outDir, path)
			if err != nil {
				return nil
			}
			for _, skip := range filesToRedact {
				if relPath == skip {
					return nil
				}
			}

			holonlog.Debug("redacting sensitive data from log", "file", relPath)
			if err := redactor.RedactFile(path); err != nil {
				// Log but continue on error
				holonlog.Warn("failed to redact log file", "file", relPath, "error", err)
			}
		}
		return nil
	})

	return walkErr
}
