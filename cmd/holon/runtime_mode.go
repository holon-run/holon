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
	source, _, err := resolveRuntimeDevAgentSourceWithOrigin(mode, explicit)
	return source, err
}

func resolveRuntimeDevAgentSourceWithOrigin(mode, explicit string) (string, string, error) {
	parsedMode, err := docker.ParseRuntimeMode(mode)
	if err != nil {
		return "", "", err
	}
	if parsedMode != docker.RuntimeModeDev {
		return "", "", nil
	}

	type candidate struct {
		path   string
		source string
	}

	candidates := []candidate{
		{path: strings.TrimSpace(explicit), source: "flag"},
		{path: strings.TrimSpace(os.Getenv("HOLON_RUNTIME_DEV_AGENT_SOURCE")), source: "env:HOLON_RUNTIME_DEV_AGENT_SOURCE"},
		{path: strings.TrimSpace(os.Getenv("HOLON_DEV_AGENT_SOURCE")), source: "env:HOLON_DEV_AGENT_SOURCE"},
	}

	if wd, err := os.Getwd(); err == nil {
		candidates = append(candidates, candidate{
			path:   filepath.Join(wd, "agents", "claude"),
			source: "default:./agents/claude",
		})
	}

	for _, candidate := range candidates {
		if candidate.path == "" {
			continue
		}
		abs, err := filepath.Abs(candidate.path)
		if err != nil {
			return "", "", fmt.Errorf("failed to resolve runtime dev agent source path %q: %w", candidate.path, err)
		}
		info, err := os.Stat(abs)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return "", "", fmt.Errorf("failed to stat runtime dev agent source path %q: %w", abs, err)
		}
		if !info.IsDir() {
			continue
		}
		return abs, candidate.source, nil
	}

	return "", "", fmt.Errorf("runtime-mode=dev requires a local agent source directory; set --runtime-dev-agent-source or HOLON_RUNTIME_DEV_AGENT_SOURCE")
}
