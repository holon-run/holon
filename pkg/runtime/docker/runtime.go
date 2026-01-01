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
	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/git"
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
	BaseImage      string // e.g., golang:1.22 (The toolchain)
	AgentBundle    string // Required path to agent bundle archive (.tar.gz)
	Workspace      string
	InputPath      string // Path to input directory (contains spec.yaml, context/, prompts/)
	OutDir         string
	Env            map[string]string
	Cmd            []string // Optional command override

	// Workspace preparation options
	WorkspaceStrategy string                 // Workspace preparation strategy (e.g., "git-clone", "snapshot")
	WorkspaceHistory  workspace.HistoryMode // How much git history to include
	WorkspaceRef      string                 // Git ref to checkout (optional)
	WorkspaceIsTemporary bool                // true if workspace is a temporary directory (vs user-provided)

	// Agent config mount mode
	AgentConfigMode string // Agent config mount mode: "auto", "yes", "no"

	// Git configuration (already resolved by caller)
	// These values are pre-resolved using git.ResolveConfig() with proper priority:
	// host git config > ProjectConfig > env vars > defaults
	GitAuthorName  string // Git author name for commits
	GitAuthorEmail string // Git author email for commits

	// Skills configuration
	Skills []string // Paths to skill directories to include
}

func (r *Runtime) RunHolon(ctx context.Context, cfg *ContainerConfig) (string, error) {
	// 1. Prepare Workspace using WorkspacePreparer
	snapshotDir, _, err := prepareWorkspace(ctx, cfg)
	if err != nil {
		return "", err
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

	env := BuildContainerEnv(&EnvConfig{
		UserEnv: cfg.Env,
		HostUID: os.Getuid(),
		HostGID: os.Getgid(),
	})

	mountConfig := &MountConfig{
		SnapshotDir: snapshotDir,
		InputPath:   cfg.InputPath,
		OutDir:      cfg.OutDir,
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

	mounts := BuildContainerMounts(mountConfig)

	holonlog.Progress("creating container", "image", finalImage)
	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      finalImage,
		Cmd:        cfg.Cmd,
		Env:        env,
		WorkingDir: "/holon/workspace",
		Tty:        false,
	}, &container.HostConfig{
		Mounts:     mounts,
		AutoRemove: true,
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

	// 6. Artifact Validation (RFC-0002)
	// Read the spec to verify required artifacts, plus manifest.json
	// For now, validate basic manifest.json requirement
	if err := ValidateRequiredArtifacts(cfg.OutDir, nil); err != nil {
		return "", err
	}

	return snapshotDir, nil
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

	claudeCodeVersion := os.Getenv("HOLON_CLAUDE_CODE_VERSION")
	if claudeCodeVersion == "" {
		claudeCodeVersion = "2.0.74"
	}

	dockerfile := fmt.Sprintf(`
FROM %s
ARG NODE_MAJOR=%s
ARG CLAUDE_CODE_VERSION=%s
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
    elif command -v dnf >/dev/null 2>&1; then \
        dnf install -y curl ca-certificates git; \
        curl -fsSL https://rpm.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        dnf install -y nodejs; \
        if ! command -v gh >/dev/null 2>&1; then \
            curl -o /etc/yum.repos.d/gh-cli.repo https://cli.github.com/packages/rpm/gh-cli.repo; \
            dnf install -y gh || true; \
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
    else \
        echo "Unsupported base image: no apt-get, dnf, or yum detected." >&2; \
        exit 1; \
    fi

RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}

COPY %s /holon/agent-bundle.tar.gz
RUN mkdir -p /holon/agent && tar -xzf /holon/agent-bundle.tar.gz -C /holon/agent

ENV IS_SANDBOX=1
WORKDIR /holon/workspace
ENTRYPOINT ["/holon/agent/bin/agent"]
`, baseImage, nodeMajor, claudeCodeVersion, bundleName)

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

// prepareWorkspace prepares the workspace using the configured strategy
func prepareWorkspace(ctx context.Context, cfg *ContainerConfig) (string, workspace.Preparer, error) {
	// If the workspace is already a temporary directory (created by solve),
	// use it directly instead of creating another snapshot.
	// This optimization avoids double cloning when solve creates a temp workspace.
	if cfg.WorkspaceIsTemporary {
		holonlog.Info("using temporary workspace directly (no snapshot needed)", "workspace", cfg.Workspace)

		// Use an ExistingPreparer to prepare the temporary workspace and
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
			return "", nil, fmt.Errorf("failed to prepare temporary workspace: %w", err)
		}

		// Stage skills to workspace
		resolvedSkills, err := resolveSkills(ctx, cfg)
		if err != nil {
			return "", nil, fmt.Errorf("failed to resolve skills: %w", err)
		}
		if len(resolvedSkills) > 0 {
			holonlog.Info("staging skills", "count", len(resolvedSkills))
			if err := skills.Stage(cfg.Workspace, resolvedSkills); err != nil {
				return "", nil, fmt.Errorf("failed to stage skills: %w", err)
			}
			for _, skill := range resolvedSkills {
				holonlog.Debug("staged skill", "name", skill.Name, "source", skill.Source)
			}
		}

		// Write workspace manifest to output directory if specified
		if cfg.OutDir != "" {
			if err := writeWorkspaceManifest(cfg.OutDir, prepareResult); err != nil {
				return "", nil, fmt.Errorf("failed to write workspace manifest: %w", err)
			}
		}

		// Return the workspace as-is with an existing preparer (no-op cleanup)
		return cfg.Workspace, preparer, nil
	}

	// Create snapshot directory outside workspace
	snapshotDir, err := workspace.MkdirTempOutsideWorkspace(cfg.Workspace, "holon-workspace-*")
	if err != nil {
		return "", nil, fmt.Errorf("failed to create snapshot dir: %w", err)
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
		return "", nil, fmt.Errorf("workspace strategy '%s' not found", strategyName)
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
		return "", nil, fmt.Errorf("failed to prepare workspace: %w", err)
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

	// Stage skills to snapshot workspace
	resolvedSkills, err := resolveSkills(ctx, cfg)
	if err != nil {
		return "", nil, fmt.Errorf("failed to resolve skills: %w", err)
	}
	if len(resolvedSkills) > 0 {
		holonlog.Info("staging skills", "count", len(resolvedSkills))
		if err := skills.Stage(snapshotDir, resolvedSkills); err != nil {
			return "", nil, fmt.Errorf("failed to stage skills: %w", err)
		}
		for _, skill := range resolvedSkills {
			holonlog.Debug("staged skill", "name", skill.Name, "source", skill.Source)
		}
	}

	// Write workspace manifest to output directory (not workspace)
	// This avoids polluting the workspace with metadata files
	if cfg.OutDir != "" {
		if err := writeWorkspaceManifest(cfg.OutDir, result); err != nil {
			holonlog.Warn("failed to write workspace manifest", "error", err)
		}
	}

	return snapshotDir, preparer, nil
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

	// If no skills explicitly provided, just auto-discover from workspace
	if len(cfg.Skills) == 0 {
		return resolver.Resolve([]string{}, []string{}, []string{})
	}

	// Skills are already resolved by caller (cmd/holon/main.go with proper precedence)
	// Just validate and normalize them to Skill structs
	var validated []skills.Skill
	for _, path := range cfg.Skills {
		skill, err := resolver.ValidateAndNormalize(path, "cli")
		if err != nil {
			return nil, fmt.Errorf("invalid skill path: %w", err)
		}
		validated = append(validated, skill)
	}

	// Auto-discover additional skills from workspace (add those not already specified)
	discovered, err := resolver.Resolve([]string{}, []string{}, []string{})
	if err != nil {
		return nil, fmt.Errorf("failed to discover skills: %w", err)
	}

	// Merge validated and discovered skills (validated take precedence)
	for _, skill := range discovered {
		if !containsSkill(validated, skill) {
			validated = append(validated, skill)
		}
	}

	return validated, nil
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
