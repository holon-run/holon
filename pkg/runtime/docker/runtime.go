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
}

func (r *Runtime) RunHolon(ctx context.Context, cfg *ContainerConfig) error {
	// 1. Prepare Workspace using WorkspacePreparer
	snapshotDir, preparer, err := prepareWorkspace(ctx, cfg)
	if err != nil {
		return err
	}

	// Set up cleanup function
	cleanupSnapshot := func() error {
		return preparer.Cleanup(snapshotDir)
	}
	defer func() {
		if err := cleanupSnapshot(); err != nil {
			fmt.Printf("Warning: failed to cleanup snapshot at %s: %v\n", snapshotDir, err)
		}
	}()

	// 2. Prepare Image (Build-on-Run composition)
	if cfg.AgentBundle == "" {
		return fmt.Errorf("agent bundle is required")
	}
	if cfg.BaseImage == "" {
		return fmt.Errorf("base image is required")
	}

	fmt.Printf("Composing execution image for %s + agent bundle %s...\n", cfg.BaseImage, cfg.AgentBundle)
	composedImage, err := r.buildComposedImageFromBundle(ctx, cfg.BaseImage, cfg.AgentBundle)
	if err != nil {
		return fmt.Errorf("failed to compose image: %w", err)
	}
	finalImage := composedImage

	// Pull final image if not present locally
	_, err = r.cli.ImageInspect(ctx, finalImage)
	if err != nil {
		fmt.Printf("Image %s not found locally, attempting to pull...\n", finalImage)
		reader, err := r.cli.ImagePull(ctx, finalImage, image.PullOptions{})
		if err != nil {
			fmt.Printf("Warning: failed to pull image %s: %v\n", finalImage, err)
		} else {
			defer reader.Close()
			io.Copy(io.Discard, reader)
		}
	} else {
		fmt.Printf("Image %s found locally.\n", finalImage)
	}

	// 3. Create Container
	// Inject host git identity
	gitName, err := getGitConfig("user.name")
	if err != nil {
		fmt.Printf("Warning: failed to get host git config 'user.name': %v\n", err)
	}
	gitEmail, err := getGitConfig("user.email")
	if err != nil {
		fmt.Printf("Warning: failed to get host git config 'user.email': %v\n", err)
	}

	if gitName != "" || gitEmail != "" {
		if cfg.Env == nil {
			cfg.Env = make(map[string]string)
		}
	}

	if gitName != "" {
		cfg.Env["GIT_AUTHOR_NAME"] = gitName
		cfg.Env["GIT_COMMITTER_NAME"] = gitName
	}
	if gitEmail != "" {
		cfg.Env["GIT_AUTHOR_EMAIL"] = gitEmail
		cfg.Env["GIT_COMMITTER_EMAIL"] = gitEmail
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
	mounts := BuildContainerMounts(mountConfig)

	fmt.Printf("Creating container from image %s...\n", finalImage)
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
		return fmt.Errorf("failed to create container: %w", err)
	}

	// 4. Start Container
	fmt.Printf("Starting container %s...\n", resp.ID[:12])
	if err := r.cli.ContainerStart(ctx, resp.ID, container.StartOptions{}); err != nil {
		return fmt.Errorf("failed to start container: %w", err)
	}

	// 4.5 Stream Logs
	fmt.Println("Streaming container logs...")
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
	fmt.Println("Waiting for container completion...")
	statusCh, errCh := r.cli.ContainerWait(ctx, resp.ID, container.WaitConditionNotRunning)
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

	// 6. Artifact Validation (RFC-0002)
	// Read the spec to verify required artifacts, plus manifest.json
	// For now, validate basic manifest.json requirement
	if err := ValidateRequiredArtifacts(cfg.OutDir, nil); err != nil {
		return err
	}

	return nil
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
    elif command -v yum >/dev/null 2>&1; then \
        yum install -y curl ca-certificates git; \
        curl -fsSL https://rpm.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        yum install -y nodejs; \
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
	// Create snapshot directory outside workspace
	snapshotDir, err := workspace.MkdirTempOutsideWorkspace(cfg.Workspace, "holon-workspace-*")
	if err != nil {
		return "", nil, fmt.Errorf("failed to create snapshot dir: %w", err)
	}

	// Determine the strategy to use
	strategyName := cfg.WorkspaceStrategy
	if strategyName == "" {
		// Auto-detect: use git-clone for git repos, snapshot otherwise
		if workspace.IsGitRepo(cfg.Workspace) {
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
	fmt.Printf("Preparing workspace using %s strategy...\n", strategyName)
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

	// Log preparation details
	fmt.Printf("  Strategy: %s\n", result.Strategy)
	if result.HeadSHA != "" {
		fmt.Printf("  HEAD: %s\n", result.HeadSHA)
	}
	if result.HasHistory {
		if result.IsShallow {
			fmt.Printf("  History: shallow\n")
		} else {
			fmt.Printf("  History: full\n")
		}
	} else {
		fmt.Printf("  History: none\n")
	}

	// Log any notes
	for _, note := range result.Notes {
		fmt.Printf("  Note: %s\n", note)
	}

	return snapshotDir, preparer, nil
}
