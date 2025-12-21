package docker

import (
	"context"
	"crypto/sha256"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/client"
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
	AdapterImage   string // e.g., holon-adapter-claude (The adapter logic)
	Workspace      string
	SpecPath       string
	ContextPath    string // Optional: path to context files
	OutDir         string
	Env            map[string]string
	PromptPath     string   // Path to compiled system.md
	UserPromptPath string   // Path to compiled user.md
	Cmd            []string // Optional command override
}

func (r *Runtime) RunHolon(ctx context.Context, cfg *ContainerConfig) error {
	// 1. Snapshot Workspace (Isolation)
	snapshotDir, err := mkdirTempOutsideWorkspace(cfg.Workspace, "holon-workspace-*")
	if err != nil {
		return fmt.Errorf("failed to create snapshot dir: %w", err)
	}
	defer os.RemoveAll(snapshotDir)

	fmt.Printf("Snapshotting workspace to %s...\n", snapshotDir)
	if err := copyDir(cfg.Workspace, snapshotDir); err != nil {
		return fmt.Errorf("failed to snapshot workspace: %w", err)
	}

	// 2. Prepare Image (Build-on-Run composition)
	adapterImage := cfg.AdapterImage
	if adapterImage == "" {
		adapterImage = "holon-adapter-claude"
	}

	finalImage := adapterImage
	if cfg.BaseImage != "" && cfg.BaseImage != adapterImage {
		fmt.Printf("Composing runtime image for %s + %s...\n", cfg.BaseImage, adapterImage)
		composedImage, err := r.buildComposedImage(ctx, cfg.BaseImage, adapterImage)
		if err != nil {
			return fmt.Errorf("failed to compose image: %w", err)
		}
		finalImage = composedImage
	}

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
	env := BuildContainerEnv(&EnvConfig{
		UserEnv: cfg.Env,
		HostUID: os.Getuid(),
		HostGID: os.Getgid(),
	})

	mountConfig := &MountConfig{
		SnapshotDir:    snapshotDir,
		SpecPath:       cfg.SpecPath,
		ContextPath:    cfg.ContextPath,
		OutDir:         cfg.OutDir,
		PromptPath:     cfg.PromptPath,
		UserPromptPath: cfg.UserPromptPath,
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

func (r *Runtime) buildComposedImage(ctx context.Context, baseImage, adapterImage string) (string, error) {
	// Implementation follows RFC-0002: Create a transient Dockerfile
	// and run docker build.
	tmpDir, err := os.MkdirTemp("", "holon-build-*")
	if err != nil {
		return "", err
	}
	defer os.RemoveAll(tmpDir)

	dockerfile := fmt.Sprintf(`
FROM %s
# Install Node, Python and GitHub CLI if missing
RUN apt-get update && apt-get install -y curl git python3 python3-pip
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - && \
    apt-get install -y nodejs
# Try to install GitHub CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg && \
    chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | tee /etc/apt/sources.list.d/github-cli.list > /dev/null && \
    apt-get update && apt-get install -y gh
# Layer the adapter from the adapter image
COPY --from=%s /app /app
COPY --from=%s /root/.claude /root/.claude
COPY --from=%s /root/.claude.json /root/.claude.json
# Install Claude Code and dependencies
RUN npm install -g @anthropic-ai/claude-code@2.0.74 && \
    if [ -f /app/requirements.txt ]; then pip3 install --no-cache-dir -r /app/requirements.txt --break-system-packages; fi
	# Ensure environment
	ENV IS_SANDBOX=1
	ENV PYTHONUNBUFFERED=1
	ENV PYTHONDONTWRITEBYTECODE=1
	WORKDIR /holon/workspace
	ENTRYPOINT ["sh", "-c", "if [ -f /app/dist/adapter.js ]; then echo 'Starting Node adapter: /app/dist/adapter.js' >&2; node /app/dist/adapter.js; status=$?; echo \"Node adapter exited with status $status\" >&2; if [ $status -ne 0 ] && [ -f /app/adapter.py ]; then echo 'Falling back to Python adapter: /app/adapter.py' >&2; exec python3 /app/adapter.py; else exit $status; fi; elif [ -f /app/adapter.py ]; then echo 'Starting Python adapter: /app/adapter.py' >&2; exec python3 /app/adapter.py; else echo 'adapter entrypoint not found' >&2; exit 1; fi"]
`, baseImage, adapterImage, adapterImage, adapterImage)

	dfPath := filepath.Join(tmpDir, "Dockerfile")
	if err := os.WriteFile(dfPath, []byte(dockerfile), 0644); err != nil {
		return "", err
	}

	// Generate stable hash for composed image tag
	hashInput := baseImage + ":" + adapterImage
	hash := sha256.Sum256([]byte(hashInput))
	tag := fmt.Sprintf("holon-composed-%x", hash[:12]) // Use first 12 bytes of hash
	cmd := exec.Command("docker", "build", "-t", tag, tmpDir)
	if out, err := cmd.CombinedOutput(); err != nil {
		return "", fmt.Errorf("composition build failed: %v, output: %s", err, string(out))
	}

	return tag, nil
}

// copyDir is a helper to snapshot the workspace
func copyDir(src string, dst string) error {
	// Using cp -a for recursive copy on Darwin/Linux
	cmd := exec.Command("cp", "-a", src+"/.", dst+"/")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("cp failed: %v, output: %s", err, string(out))
	}
	return nil
}

func mkdirTempOutsideWorkspace(workspace, pattern string) (string, error) {
	absWorkspace, err := cleanAbs(workspace)
	if err != nil {
		return "", err
	}

	var baseCandidates []string
	if v := strings.TrimSpace(os.Getenv("HOLON_SNAPSHOT_BASE")); v != "" {
		baseCandidates = append(baseCandidates, v)
	}
	baseCandidates = append(baseCandidates, os.TempDir())

	if cacheDir, err := os.UserCacheDir(); err == nil && cacheDir != "" {
		baseCandidates = append(baseCandidates, filepath.Join(cacheDir, "holon"))
	}

	// Parent directory is a good, usually writable, fallback.
	baseCandidates = append(baseCandidates, filepath.Dir(absWorkspace))

	if runtime.GOOS != "windows" {
		baseCandidates = append(baseCandidates, "/tmp")
	}

	var lastErr error
	for _, base := range baseCandidates {
		if strings.TrimSpace(base) == "" {
			continue
		}
		absBase, err := cleanAbs(base)
		if err != nil {
			lastErr = err
			continue
		}
		if isSubpath(absBase, absWorkspace) {
			continue
		}
		if err := os.MkdirAll(absBase, 0o755); err != nil {
			lastErr = err
			continue
		}
		dir, err := os.MkdirTemp(absBase, pattern)
		if err != nil {
			lastErr = err
			continue
		}
		return dir, nil
	}

	if lastErr != nil {
		return "", lastErr
	}
	return "", fmt.Errorf("unable to create temp dir outside workspace %q", absWorkspace)
}

func cleanAbs(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	if resolved, err := filepath.EvalSymlinks(abs); err == nil {
		return resolved, nil
	}
	return abs, nil
}

func isSubpath(candidate, parent string) bool {
	rel, err := filepath.Rel(parent, candidate)
	if err != nil {
		return false
	}
	rel = filepath.Clean(rel)
	if rel == "." {
		return true
	}
	return !strings.HasPrefix(rel, ".."+string(filepath.Separator)) && rel != ".."
}
