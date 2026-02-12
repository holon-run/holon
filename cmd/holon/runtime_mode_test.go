package main

import (
	"path/filepath"
	"testing"
)

func TestResolveRuntimeMode(t *testing.T) {
	got, err := resolveRuntimeMode("dev")
	if err != nil {
		t.Fatalf("resolveRuntimeMode(dev) error = %v", err)
	}
	if got != "dev" {
		t.Fatalf("resolveRuntimeMode(dev) = %q, want dev", got)
	}

	got, err = resolveRuntimeMode("")
	if err != nil {
		t.Fatalf("resolveRuntimeMode(\"\") error = %v", err)
	}
	if got != "prod" {
		t.Fatalf("resolveRuntimeMode(\"\") = %q, want prod", got)
	}
}

func TestResolveRuntimeDevAgentSource(t *testing.T) {
	sourceDir := t.TempDir()
	t.Setenv("HOLON_RUNTIME_DEV_AGENT_SOURCE", sourceDir)
	got, err := resolveRuntimeDevAgentSource("dev", "")
	if err != nil {
		t.Fatalf("resolveRuntimeDevAgentSource(dev) error = %v", err)
	}
	if got != sourceDir {
		t.Fatalf("resolveRuntimeDevAgentSource(dev) = %q, want %q", got, sourceDir)
	}

	got, err = resolveRuntimeDevAgentSource("prod", "")
	if err != nil {
		t.Fatalf("resolveRuntimeDevAgentSource(prod) error = %v", err)
	}
	if got != "" {
		t.Fatalf("resolveRuntimeDevAgentSource(prod) = %q, want empty", got)
	}

	t.Setenv("HOLON_RUNTIME_DEV_AGENT_SOURCE", "")
	t.Setenv("HOLON_DEV_AGENT_SOURCE", "")
	scratch := t.TempDir()
	t.Chdir(scratch)
	_, err = resolveRuntimeDevAgentSource("dev", filepath.Join(scratch, "missing"))
	if err == nil {
		t.Fatalf("resolveRuntimeDevAgentSource(dev, missing) expected error")
	}
}
