package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/holon-run/holon/pkg/image"
	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/runtime/docker"
)

// SessionRunner launches and manages a long-running serve runtime session.
type SessionRunner interface {
	Start(ctx context.Context, cfg RuntimeSessionConfig) (*docker.SessionHandle, error)
	Wait(ctx context.Context, handle *docker.SessionHandle) error
	Stop(ctx context.Context, handle *docker.SessionHandle) error
}

type sessionRuntime interface {
	StartSession(ctx context.Context, cfg *docker.ContainerConfig) (*docker.SessionHandle, error)
	WaitSession(ctx context.Context, handle *docker.SessionHandle) error
	StopSession(ctx context.Context, handle *docker.SessionHandle) error
}

// RuntimeSessionConfig defines runtime config for serve runtime sessions.
type RuntimeSessionConfig struct {
	Workspace             string
	InputPath             string
	OutputPath            string
	StateDir              string
	AgentHome             string
	LogLevel              string
	Env                   map[string]string
	RuntimeMode           string
	RuntimeDevAgentSource string
	ExtraMounts           []docker.ExtraMount
}

type dockerSessionRunner struct {
	runtime sessionRuntime
}

func newDockerSessionRunner(runtime sessionRuntime) SessionRunner {
	return &dockerSessionRunner{runtime: runtime}
}

func (r *dockerSessionRunner) Start(ctx context.Context, cfg RuntimeSessionConfig) (*docker.SessionHandle, error) {
	baseImage := resolveServeBaseImage(cfg.Workspace)
	agentBundle, err := resolveServeAgentBundle(ctx, cfg.Workspace)
	if err != nil {
		return nil, err
	}

	containerCfg := &docker.ContainerConfig{
		BaseImage:            baseImage,
		AgentBundle:          agentBundle,
		Workspace:            cfg.Workspace,
		InputPath:            cfg.InputPath,
		OutDir:               cfg.OutputPath,
		StateDir:             cfg.StateDir,
		AgentHome:            cfg.AgentHome,
		Env:                  map[string]string{"LOG_LEVEL": cfg.LogLevel, "ASSISTANT_OUTPUT": "none"},
		AgentConfigMode:      "no",
		WorkspaceIsTemporary: true,
		UseSkillMode:         true,
		RuntimeMode:          cfg.RuntimeMode,
		DevAgentSourceDir:    cfg.RuntimeDevAgentSource,
		ExtraMounts:          append([]docker.ExtraMount(nil), cfg.ExtraMounts...),
	}
	for k, v := range cfg.Env {
		containerCfg.Env[k] = v
	}

	return r.runtime.StartSession(ctx, containerCfg)
}

func (r *dockerSessionRunner) Wait(ctx context.Context, handle *docker.SessionHandle) error {
	return r.runtime.WaitSession(ctx, handle)
}

func (r *dockerSessionRunner) Stop(ctx context.Context, handle *docker.SessionHandle) error {
	return r.runtime.StopSession(ctx, handle)
}

func resolveServeBaseImage(workspace string) string {
	result := image.Detect(workspace)
	if result.Image == "" {
		return image.DefaultImage
	}
	holonlog.Info("serve base image", "image", result.Image, "rationale", result.Rationale)
	return result.Image
}

func resolveServeAgentBundle(ctx context.Context, workspace string) (string, error) {
	channel := strings.TrimSpace(os.Getenv("HOLON_AGENT_CHANNEL"))
	if channel == "" {
		channel = "latest"
	}
	channelSource := "default"
	if strings.TrimSpace(os.Getenv("HOLON_AGENT_CHANNEL")) != "" {
		channelSource = "config"
	}

	runner := NewRunner(nil)
	return runner.resolveAgentBundle(ctx, RunnerConfig{
		AgentBundle:        strings.TrimSpace(os.Getenv("HOLON_AGENT")),
		AgentChannel:       channel,
		AgentChannelSource: channelSource,
	}, workspace)
}

func defaultServeWorkspace() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("failed to resolve user home directory: %w", err)
	}
	return filepath.Join(home, ".holon", "workspace"), nil
}
