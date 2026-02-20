package docker

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
	"github.com/docker/docker/client"
	"github.com/docker/docker/errdefs"
	"github.com/docker/docker/pkg/stdcopy"
	"github.com/holon-run/holon/pkg/builtin"
	"github.com/holon-run/holon/pkg/git"
	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/logs/redact"
	"github.com/holon-run/holon/pkg/runtime/tools"
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

	// Extra same-path host mounts configured by agent-home runtime policy.
	ExtraMounts []ExtraMount
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
const imageInspectRetryWindow = 20 * time.Second
const imageInspectRetryDelay = 500 * time.Millisecond

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

	if err := r.ensureImagePresent(ctx, finalImage); err != nil {
		return "", err
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
		ExtraMounts:    cfg.ExtraMounts,
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
	logEffectiveMounts(mounts)

	holonlog.Progress("creating container", "image", finalImage)
	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      finalImage,
		Cmd:        cfg.Cmd,
		Env:        env,
		WorkingDir: ContainerWorkspaceDir,
		Tty:        false,
	}, BuildContainerHostConfig(&HostConfigOptions{
		Mounts: mounts,
	}), nil, nil, "")
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
	// In skill-first mode (UseSkillMode=true), skills are responsible for all IO including publish.
	// manifest.json is no longer a blocking requirement since skills may succeed without generating it.
	// Log a warning for debugging but don't fail execution.
	if err := ValidateRequiredArtifacts(cfg.OutDir, nil); err != nil {
		// In skill-first mode, log a warning but don't fail
		if cfg.UseSkillMode {
			// List output directory contents for debugging
			var contents []string
			if entries, readErr := os.ReadDir(cfg.OutDir); readErr == nil {
				for _, entry := range entries {
					contents = append(contents, entry.Name())
				}
			}

			holonlog.Warn("artifact validation warning",
				"error", err,
				"output_dir", cfg.OutDir,
				"contents", contents,
				"note", "in skill-first mode, skills handle all IO; missing manifest.json may indicate skill completed without generating it",
			)
		} else {
			// In non-skill-first mode, manifest.json is still required
			return "", err
		}
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

	if err := r.ensureImagePresent(ctx, finalImage); err != nil {
		cleanupOnError()
		return nil, err
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
		ExtraMounts:    cfg.ExtraMounts,
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
	logEffectiveMounts(mounts)
	holonlog.Progress("creating session container", "image", finalImage)
	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      finalImage,
		Cmd:        cfg.Cmd,
		Env:        env,
		WorkingDir: ContainerWorkspaceDir,
		Tty:        false,
	}, BuildContainerHostConfig(&HostConfigOptions{
		Mounts: mounts,
	}), nil, nil, "")
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

func logEffectiveMounts(mounts []mount.Mount) {
	if len(mounts) == 0 {
		return
	}
	summaries := make([]string, 0, len(mounts))
	for _, m := range mounts {
		mode := "rw"
		if m.ReadOnly {
			mode = "ro"
		}
		summaries = append(summaries, fmt.Sprintf("%s -> %s (%s)", m.Source, m.Target, mode))
	}
	holonlog.Info("effective runtime mounts", "mounts", summaries)
}

// WaitSession waits for a session container to exit.
func (r *Runtime) WaitSession(ctx context.Context, handle *SessionHandle) error {
	if handle == nil || handle.ContainerID == "" {
		return fmt.Errorf("session handle is required")
	}
	containerID := handle.ContainerID
	var waitErr error
	statusCh, errCh := r.cli.ContainerWait(ctx, handle.ContainerID, container.WaitConditionNotRunning)
	select {
	case err := <-errCh:
		if err != nil {
			waitErr = fmt.Errorf("container wait error: %w", err)
		}
	case status := <-statusCh:
		if status.StatusCode != 0 {
			waitErr = r.buildContainerExitError(ctx, containerID, status.StatusCode)
		}
	}
	if err := r.cli.ContainerRemove(ctx, containerID, container.RemoveOptions{Force: true}); err != nil {
		holonlog.Debug("container remove returned error", "id", containerID, "error", err)
		removeErr := fmt.Errorf("failed to remove session container %s: %w", containerID, err)
		if waitErr != nil {
			waitErr = errors.Join(waitErr, removeErr)
		} else {
			waitErr = removeErr
		}
	}
	r.closeSessionLogStream(handle)
	r.cleanupSessionHandle(handle)
	return waitErr
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

func (r *Runtime) buildContainerExitError(ctx context.Context, containerID string, statusCode int64) error {
	suffix := ""
	inspect, err := r.cli.ContainerInspect(ctx, containerID)
	if err == nil {
		if inspect.State != nil {
			stateParts := make([]string, 0, 4)
			if inspect.State.OOMKilled {
				stateParts = append(stateParts, "oom_killed=true")
			}
			if msg := strings.TrimSpace(inspect.State.Error); msg != "" {
				stateParts = append(stateParts, fmt.Sprintf("state_error=%q", msg))
			}
			if finished := strings.TrimSpace(inspect.State.FinishedAt); finished != "" && finished != "0001-01-01T00:00:00Z" {
				stateParts = append(stateParts, "finished_at="+finished)
			}
			if len(stateParts) > 0 {
				suffix = " (" + strings.Join(stateParts, ", ") + ")"
			}
		}
	} else if !errdefs.IsNotFound(err) {
		holonlog.Debug("failed to inspect exited container", "id", containerID, "error", err)
	}
	return fmt.Errorf("container failed with exit code %d%s", statusCode, suffix)
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

	bundleName := "agent-bundle.tar.gz"
	bundleDest := filepath.Join(tmpDir, bundleName)
	if err := copyFile(bundlePath, bundleDest); err != nil {
		return "", fmt.Errorf("failed to stage agent bundle: %w", err)
	}

	installScript := tools.BuildInstallScript()
	installScriptName := "holon-install-tools.sh"
	installScriptPath := filepath.Join(tmpDir, installScriptName)
	if err := os.WriteFile(installScriptPath, []byte(installScript), 0o755); err != nil {
		return "", fmt.Errorf("failed to stage install script: %w", err)
	}
	dockerfile := fmt.Sprintf(`
FROM %s
SHELL ["/bin/sh", "-c"]

COPY %s /tmp/holon-install-tools.sh
RUN /bin/sh /tmp/holon-install-tools.sh && rm -f /tmp/holon-install-tools.sh

COPY %s /holon/agent-bundle.tar.gz
RUN mkdir -p /holon/agent && tar -xzf /holon/agent-bundle.tar.gz -C /holon/agent

ENV PATH="/holon/agent/node_modules/.bin:${PATH}"
ENV IS_SANDBOX=1
WORKDIR %s
ENTRYPOINT ["/holon/agent/bin/agent"]
`, baseImage, installScriptName, bundleName, ContainerWorkspaceDir)

	dfPath := filepath.Join(tmpDir, "Dockerfile")
	if err := os.WriteFile(dfPath, []byte(dockerfile), 0644); err != nil {
		return "", err
	}

	tag := composeImageTag(baseImage, bundleDigest)
	cmd := exec.Command("docker", "build", "-t", tag, tmpDir)
	if out, err := cmd.CombinedOutput(); err != nil {
		// Concurrent runs may race on deterministic tags. If another process
		// already produced the target image, reuse it instead of failing.
		if isImageAlreadyExistsBuildError(string(out)) {
			if _, inspectErr := r.cli.ImageInspect(ctx, tag); inspectErr == nil {
				return tag, nil
			}
		}
		return "", fmt.Errorf("composition build failed: %v, output: %s", err, string(out))
	}

	return tag, nil
}

func (r *Runtime) ensureImagePresent(ctx context.Context, imageRef string) error {
	err := r.inspectImage(ctx, imageRef)
	if err == nil {
		holonlog.Debug("image found locally", "image", imageRef)
		return nil
	}
	if !errdefs.IsNotFound(err) {
		return fmt.Errorf("failed to inspect image %q: %w", imageRef, err)
	}
	if strings.HasPrefix(imageRef, "holon-composed-") {
		// Composed images are built locally with deterministic names; never pull.
		return fmt.Errorf("composed image %q not found locally after build", imageRef)
	}

	holonlog.Info("image not found locally, attempting to pull", "image", imageRef)
	reader, pullErr := r.cli.ImagePull(ctx, imageRef, image.PullOptions{})
	if pullErr != nil {
		return fmt.Errorf("failed to pull image %q: %w", imageRef, pullErr)
	}
	defer reader.Close()
	if _, copyErr := io.Copy(io.Discard, reader); copyErr != nil {
		return fmt.Errorf("failed to read pull stream for %q: %w", imageRef, copyErr)
	}
	return nil
}

func (r *Runtime) inspectImage(ctx context.Context, imageRef string) error {
	deadline := time.Now().Add(imageInspectRetryWindow)
	if ctxDeadline, ok := ctx.Deadline(); ok && ctxDeadline.Before(deadline) {
		deadline = ctxDeadline
	}

	var lastErr error
	for {
		_, err := r.cli.ImageInspect(ctx, imageRef)
		if err == nil {
			return nil
		}
		lastErr = err

		if !isTransientImageInspectError(err) || time.Now().After(deadline) {
			return lastErr
		}

		holonlog.Warn("transient docker image inspect failure; retrying", "image", imageRef, "error", err)
		timer := time.NewTimer(imageInspectRetryDelay)
		select {
		case <-ctx.Done():
			timer.Stop()
			return ctx.Err()
		case <-timer.C:
		}
	}
}

func isTransientImageInspectError(err error) bool {
	if err == nil {
		return false
	}
	if errdefs.IsNotFound(err) {
		return false
	}
	if errors.Is(err, context.Canceled) {
		return false
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return true
	}

	var netErr net.Error
	if errors.As(err, &netErr) && netErr.Timeout() {
		return true
	}

	msg := strings.ToLower(err.Error())
	return strings.Contains(msg, "context deadline exceeded") ||
		strings.Contains(msg, "i/o timeout") ||
		strings.Contains(msg, "connection reset by peer") ||
		strings.Contains(msg, "cannot connect to the docker daemon")
}

func composeImageTag(baseImage, bundleDigest string) string {
	hashInput := baseImage + ":" + bundleDigest
	hash := sha256.Sum256([]byte(hashInput))
	return fmt.Sprintf("holon-composed-%x", hash[:12])
}

func isImageAlreadyExistsBuildError(output string) bool {
	text := strings.ToLower(output)
	return strings.Contains(text, "already exists") &&
		strings.Contains(text, "holon-composed-")
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
	// If a remote builtin source is configured, require it to succeed.
	// If not configured, use embedded builtin skills as default.
	var builtinSkillsList []string
	builtinSource := "builtin-default"

	if cfg.BuiltinSkillsSource != "" {
		// Use remote builtin skills only; no fallback to embedded.
		holonlog.Info("loading builtin skills from remote source", "url", cfg.BuiltinSkillsSource, "ref", cfg.BuiltinSkillsRef)

		// Resolve the configured remote source directly.
		// BuiltinSkillsRef is metadata for auditing/version pin visibility.
		// Resolve as a config-origin skill so resolver errors indicate config source.
		remoteSkills, err := resolver.Resolve([]string{}, []string{cfg.BuiltinSkillsSource}, []string{})
		if err != nil {
			return nil, fmt.Errorf("failed to load remote builtin skills from %q: %w", cfg.BuiltinSkillsSource, err)
		}
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
// - remote builtin configured and successfully loaded: source/ref set, commit empty
// - embedded/default: commit set, source/ref empty
func builtinSkillsManifestFields(cfg *ContainerConfig, resolved []skills.Skill) (commit, source, ref string) {
	// If remote builtin skills were configured, record the source/ref in the manifest.
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
