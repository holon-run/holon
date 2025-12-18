package docker

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
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
	BaseImage    string // e.g., golang:1.22 (The toolchain)
	AdapterImage string // e.g., holon-adapter-claude (The adapter logic)
	Workspace    string
	SpecPath     string
	ContextPath  string // Optional: path to context files
	OutDir       string
	Env          map[string]string
	Cmd          []string // Optional command override
}

func (r *Runtime) RunHolon(ctx context.Context, cfg *ContainerConfig) error {
	// 1. Snapshot Workspace (Isolation)
	snapshotDir, err := os.MkdirTemp("", "holon-workspace-*")
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

	// Pull final image if not present
	reader, err := r.cli.ImagePull(ctx, finalImage, image.PullOptions{})
	if err != nil {
		fmt.Printf("Warning: failed to pull image %s (might be local): %v\n", finalImage, err)
	} else {
		defer reader.Close()
		io.Copy(os.Stdout, reader)
	}

	// 3. Create Container
	env := []string{}
	for k, v := range cfg.Env {
		env = append(env, fmt.Sprintf("%s=%s", k, v))
	}

	mounts := []mount.Mount{
		{
			Type:   mount.TypeBind,
			Source: snapshotDir,
			Target: "/workspace",
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

	if cfg.ContextPath != "" {
		mounts = append(mounts, mount.Mount{
			Type:   mount.TypeBind,
			Source: cfg.ContextPath,
			Target: "/holon/input/context",
		})
	}

	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      finalImage,
		Cmd:        cfg.Cmd,
		Env:        env,
		WorkingDir: "/workspace",
		Tty:        false,
	}, &container.HostConfig{
		Mounts:     mounts,
		AutoRemove: true,
	}, nil, nil, "")
	if err != nil {
		return fmt.Errorf("failed to create container: %w", err)
	}

	// 4. Start Container
	if err := r.cli.ContainerStart(ctx, resp.ID, container.StartOptions{}); err != nil {
		return fmt.Errorf("failed to start container: %w", err)
	}

	// 4.5 Stream Logs
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
	// We should read the manifest and/or spec to verify required artifacts.
	// For now, let's just check for the basic required manifest.json.
	manifestPath := filepath.Join(cfg.OutDir, "manifest.json")
	if _, err := os.Stat(manifestPath); os.IsNotExist(err) {
		return fmt.Errorf("missing required artifact: manifest.json")
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
RUN apt-get update && apt-get install -y curl git python3 python3-pip || true
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - && \
    apt-get install -y nodejs || true
# Try to install GitHub CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg && \
    chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | tee /etc/apt/sources.list.d/github-cli.list > /dev/null && \
    apt-get update && apt-get install -y gh || true
# Layer the adapter from the adapter image
COPY --from=%s /app /app
COPY --from=%s /root/.claude /root/.claude
COPY --from=%s /root/.claude.json /root/.claude.json
# Install Claude Code and dependencies
RUN npm install -g @anthropic-ai/claude-code@2.0.72 && \
    if [ -f /app/requirements.txt ]; then pip3 install --no-cache-dir -r /app/requirements.txt --break-system-packages || pip3 install --no-cache-dir -r /app/requirements.txt; fi
# Ensure environment
ENV IS_SANDBOX=1
WORKDIR /workspace
ENTRYPOINT ["python3", "/app/adapter.py"]
`, baseImage, adapterImage, adapterImage, adapterImage)

	dfPath := filepath.Join(tmpDir, "Dockerfile")
	if err := os.WriteFile(dfPath, []byte(dockerfile), 0644); err != nil {
		return "", err
	}

	tag := fmt.Sprintf("holon-composed-%x", baseImage+adapterImage)
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
