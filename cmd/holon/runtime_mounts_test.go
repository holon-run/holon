package main

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/holon-run/holon/pkg/agenthome"
)

func TestResolveRuntimeMounts(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")
	if err := agenthome.EnsureLayout(home); err != nil {
		t.Fatalf("ensure layout: %v", err)
	}
	desktop := filepath.Join(td, "Desktop")
	if err := os.MkdirAll(desktop, 0o755); err != nil {
		t.Fatalf("mkdir desktop: %v", err)
	}

	cfg := "version: v1\nagent:\n  id: main\n  profile: default\nruntime:\n  mounts:\n    - path: " + desktop + "\n"
	if err := os.WriteFile(filepath.Join(home, "agent.yaml"), []byte(cfg), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	mounts, diag, err := resolveRuntimeMounts(home)
	if err != nil {
		t.Fatalf("resolveRuntimeMounts: %v", err)
	}
	if len(mounts) != 1 {
		t.Fatalf("mount count = %d, want 1", len(mounts))
	}
	wantPath, err := filepath.EvalSymlinks(desktop)
	if err != nil {
		t.Fatalf("eval symlink desktop: %v", err)
	}
	if mounts[0].Path != wantPath {
		t.Fatalf("mount path = %q, want %q", mounts[0].Path, wantPath)
	}
	if !mounts[0].ReadOnly {
		t.Fatalf("default mount mode should be read-only")
	}
	if len(diag) != 1 || diag[0].Mode != "ro" {
		t.Fatalf("unexpected diagnostics: %+v", diag)
	}
}
