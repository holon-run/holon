package main

import (
	"strings"

	"github.com/holon-run/holon/pkg/agenthome"
	"github.com/holon-run/holon/pkg/runtime/docker"
)

type runtimeMountDiagnostic struct {
	Path string `json:"path"`
	Mode string `json:"mode"`
}

func resolveRuntimeMounts(agentHome string) ([]docker.ExtraMount, []runtimeMountDiagnostic, error) {
	cfg, err := agenthome.LoadConfig(agentHome)
	if err != nil {
		return nil, nil, err
	}

	mounts := make([]docker.ExtraMount, 0, len(cfg.Runtime.Mounts))
	diagnostics := make([]runtimeMountDiagnostic, 0, len(cfg.Runtime.Mounts))
	for _, spec := range cfg.Runtime.Mounts {
		mode := strings.ToLower(strings.TrimSpace(spec.Mode))
		if mode == "" {
			mode = "ro"
		}
		readOnly := mode != "rw"
		mounts = append(mounts, docker.ExtraMount{Path: spec.Path, ReadOnly: readOnly})
		diagnostics = append(diagnostics, runtimeMountDiagnostic{Path: spec.Path, Mode: mode})
	}

	return mounts, diagnostics, nil
}
