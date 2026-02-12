package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/holon-run/holon/pkg/runtime/docker"
)

func resolveRuntimeMode(mode string) (string, error) {
	parsed, err := docker.ParseRuntimeMode(mode)
	if err != nil {
		return "", err
	}
	return parsed.String(), nil
}

func resolveRuntimeDevAgentSource(mode, explicit string) (string, error) {
	parsedMode, err := docker.ParseRuntimeMode(mode)
	if err != nil {
		return "", err
	}
	if parsedMode != docker.RuntimeModeDev {
		return "", nil
	}

	candidates := []string{
		strings.TrimSpace(explicit),
		strings.TrimSpace(os.Getenv("HOLON_RUNTIME_DEV_AGENT_SOURCE")),
		strings.TrimSpace(os.Getenv("HOLON_DEV_AGENT_SOURCE")),
	}

	if wd, err := os.Getwd(); err == nil {
		candidates = append(candidates, filepath.Join(wd, "agents", "claude"))
	}

	for _, candidate := range candidates {
		if candidate == "" {
			continue
		}
		abs, err := filepath.Abs(candidate)
		if err != nil {
			return "", fmt.Errorf("failed to resolve runtime dev agent source path %q: %w", candidate, err)
		}
		info, err := os.Stat(abs)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return "", fmt.Errorf("failed to stat runtime dev agent source path %q: %w", abs, err)
		}
		if !info.IsDir() {
			continue
		}
		return abs, nil
	}

	return "", fmt.Errorf("runtime-mode=dev requires a local agent source directory; set --runtime-dev-agent-source or HOLON_RUNTIME_DEV_AGENT_SOURCE")
}
