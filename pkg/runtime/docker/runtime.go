package docker

import (
	"context"
	"fmt"
	"io"
	"os"

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
	Image      string
	Workspace  string
	AdapterBin string
	SpecPath   string
	OutDir     string
	Env        map[string]string
}

func (r *Runtime) RunHolon(ctx context.Context, cfg *ContainerConfig) error {
	// 1. Pull Image (if not present)
	reader, err := r.cli.ImagePull(ctx, cfg.Image, image.PullOptions{})
	if err != nil {
		return fmt.Errorf("failed to pull image: %w", err)
	}
	defer reader.Close()
	io.Copy(os.Stdout, reader)

	// 2. Create Container
	env := []string{}
	for k, v := range cfg.Env {
		env = append(env, fmt.Sprintf("%s=%s", k, v))
	}

	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      cfg.Image,
		Cmd:        []string{"/usr/local/bin/holon-adapter", "run", "/holon/input/spec.yaml"},
		Env:        env,
		WorkingDir: "/workspace",
		Tty:        false,
	}, &container.HostConfig{
		Mounts: []mount.Mount{
			{
				Type:   mount.TypeBind,
				Source: cfg.Workspace,
				Target: "/workspace",
			},
			{
				Type:   mount.TypeBind,
				Source: cfg.AdapterBin,
				Target: "/usr/local/bin/holon-adapter",
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
		},
	}, nil, nil, "")
	if err != nil {
		return fmt.Errorf("failed to create container: %w", err)
	}

	// 3. Start Container
	if err := r.cli.ContainerStart(ctx, resp.ID, container.StartOptions{}); err != nil {
		return fmt.Errorf("failed to start container: %w", err)
	}

	// 4. Wait for completion
	statusCh, errCh := r.cli.ContainerWait(ctx, resp.ID, container.WaitConditionNotRunning)
	select {
	case err := <-errCh:
		if err != nil {
			return fmt.Errorf("container wait error: %w", err)
		}
	case <-statusCh:
	}

	// 5. Logs
	out, err := r.cli.ContainerLogs(ctx, resp.ID, container.LogsOptions{ShowStdout: true, ShowStderr: true})
	if err != nil {
		return fmt.Errorf("failed to get logs: %w", err)
	}
	defer out.Close()
	io.Copy(os.Stdout, out)

	return nil
}
