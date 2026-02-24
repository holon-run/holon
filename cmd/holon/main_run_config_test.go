package main

import (
	"os"
	"path/filepath"
	"testing"
)

func writeRunProjectConfig(t *testing.T, root string, content string) {
	t.Helper()

	configDir := filepath.Join(root, ".holon")
	if err := os.MkdirAll(configDir, 0755); err != nil {
		t.Fatalf("failed to create config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(configDir, "config.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("failed to write config file: %v", err)
	}
}

func TestLoadRunProjectConfig_DefaultWorkspaceDoesNotInheritUnrelatedConfig(t *testing.T) {
	agentHome := t.TempDir()
	unrelatedProject := t.TempDir()

	writeRunProjectConfig(t, unrelatedProject, `base_image: "python:3.11"`)

	workspace, err := resolveRunWorkspacePath(agentHome, "")
	if err != nil {
		t.Fatalf("resolveRunWorkspacePath() error = %v", err)
	}

	cfg, err := loadRunProjectConfig(workspace)
	if err != nil {
		t.Fatalf("loadRunProjectConfig() error = %v", err)
	}

	if cfg.BaseImage != "" {
		t.Fatalf("BaseImage = %q, want empty for workspace without project config", cfg.BaseImage)
	}
}

func TestLoadRunProjectConfig_UsesResolvedWorkspaceConfig(t *testing.T) {
	workspace := t.TempDir()
	writeRunProjectConfig(t, workspace, `base_image: "node:20"`)

	cfg, err := loadRunProjectConfig(workspace)
	if err != nil {
		t.Fatalf("loadRunProjectConfig() error = %v", err)
	}

	if cfg.BaseImage != "node:20" {
		t.Fatalf("BaseImage = %q, want %q", cfg.BaseImage, "node:20")
	}
}
