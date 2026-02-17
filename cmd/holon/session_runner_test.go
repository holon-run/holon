package main

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/holon-run/holon/pkg/runtime/docker"
)

type captureSessionRuntime struct {
	startCfg *docker.ContainerConfig
	handle   *docker.SessionHandle
}

func (m *captureSessionRuntime) StartSession(_ context.Context, cfg *docker.ContainerConfig) (*docker.SessionHandle, error) {
	m.startCfg = cfg
	if m.handle == nil {
		m.handle = &docker.SessionHandle{ContainerID: "session-1"}
	}
	return m.handle, nil
}

func (m *captureSessionRuntime) WaitSession(_ context.Context, _ *docker.SessionHandle) error {
	return nil
}

func (m *captureSessionRuntime) StopSession(_ context.Context, _ *docker.SessionHandle) error {
	return nil
}

func TestDockerSessionRunnerStart_MapsConfig(t *testing.T) {
	t.Setenv("HOLON_AGENT", filepath.Join("testdata", "missing-agent.tar.gz"))
	t.Setenv("HOLON_AGENT_CHANNEL", "builtin")

	rt := &captureSessionRuntime{}
	runner := &dockerSessionRunner{runtime: rt}

	workspace := t.TempDir()
	inputDir := t.TempDir()
	outputDir := t.TempDir()
	stateDir := t.TempDir()
	agentHome := t.TempDir()

	// Provide a fake agent path so resolveServeAgentBundle does not attempt remote resolution.
	agentPath := filepath.Join(workspace, "agent.tar.gz")
	if err := os.WriteFile(agentPath, []byte("fake"), 0644); err != nil {
		t.Fatalf("write fake agent bundle: %v", err)
	}
	t.Setenv("HOLON_AGENT", agentPath)

	_, err := runner.Start(context.Background(), RuntimeSessionConfig{
		Workspace:             workspace,
		InputPath:             inputDir,
		OutputPath:            outputDir,
		StateDir:              stateDir,
		AgentHome:             agentHome,
		LogLevel:              "progress",
		RuntimeMode:           "dev",
		RuntimeDevAgentSource: workspace,
		Env: map[string]string{
			"HOLON_AGENT_SESSION_MODE": "serve",
		},
	})
	if err != nil {
		t.Fatalf("Start() error = %v", err)
	}
	if rt.startCfg == nil {
		t.Fatalf("StartSession not called")
	}
	if rt.startCfg.Workspace != workspace {
		t.Fatalf("Workspace = %q, want %q", rt.startCfg.Workspace, workspace)
	}
	if rt.startCfg.AgentHome != agentHome {
		t.Fatalf("AgentHome = %q, want %q", rt.startCfg.AgentHome, agentHome)
	}
	if !rt.startCfg.WorkspaceIsTemporary {
		t.Fatalf("WorkspaceIsTemporary = false, want true")
	}
	if got := rt.startCfg.Env["HOLON_AGENT_SESSION_MODE"]; got != "serve" {
		t.Fatalf("HOLON_AGENT_SESSION_MODE = %q, want serve", got)
	}
	if len(rt.startCfg.Skills) != 0 {
		t.Fatalf("Skills = %v, want empty", rt.startCfg.Skills)
	}
	if rt.startCfg.RuntimeMode != "dev" {
		t.Fatalf("RuntimeMode = %q, want dev", rt.startCfg.RuntimeMode)
	}
	if rt.startCfg.DevAgentSourceDir != workspace {
		t.Fatalf("DevAgentSourceDir = %q, want %q", rt.startCfg.DevAgentSourceDir, workspace)
	}
}

func TestDefaultServeWorkspace(t *testing.T) {
	path, err := defaultServeWorkspace()
	if err != nil {
		t.Fatalf("defaultServeWorkspace() error = %v", err)
	}
	if !filepath.IsAbs(path) {
		t.Fatalf("defaultServeWorkspace() = %q, want absolute path", path)
	}
}
