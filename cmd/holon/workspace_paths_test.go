package main

import (
	"path/filepath"
	"testing"
)

func TestResolveRunWorkspacePath_DefaultUnderAgentHome(t *testing.T) {
	t.Parallel()

	agentHome := t.TempDir()
	got, err := resolveRunWorkspacePath(agentHome, "")
	if err != nil {
		t.Fatalf("resolveRunWorkspacePath() error = %v", err)
	}
	want := filepath.Join(agentHome, "workspaces", "run", defaultWorkspaceTrack)
	if got != want {
		t.Fatalf("resolveRunWorkspacePath() = %q, want %q", got, want)
	}
}

func TestWorkspacePathForRepoRef(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	path, ref, err := workspacePathForRepoRef(root, "holon-run/holon", "feature/test")
	if err != nil {
		t.Fatalf("workspacePathForRepoRef() error = %v", err)
	}
	if ref != "holon-run/holon@feature_test" {
		t.Fatalf("workspace ref = %q", ref)
	}
	wantSuffix := filepath.Join("repos", "holon-run", "holon", "feature_test")
	if filepath.Clean(path) != filepath.Clean(filepath.Join(root, wantSuffix)) {
		t.Fatalf("workspace path = %q, want suffix %q", path, wantSuffix)
	}
}

func TestWorkspacePathForRepoRef_RejectsUnsafeSegments(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	if _, _, err := workspacePathForRepoRef(root, "./repo", "main"); err == nil {
		t.Fatalf("expected error for unsafe repo segment")
	}
	path, ref, err := workspacePathForRepoRef(root, "holon-run/holon", "..")
	if err != nil {
		t.Fatalf("workspacePathForRepoRef() error = %v", err)
	}
	if ref != "holon-run/holon@default" {
		t.Fatalf("workspace ref = %q, want holon-run/holon@default", ref)
	}
	wantSuffix := filepath.Join("repos", "holon-run", "holon", defaultWorkspaceTrack)
	if filepath.Clean(path) != filepath.Clean(filepath.Join(root, wantSuffix)) {
		t.Fatalf("workspace path = %q, want suffix %q", path, wantSuffix)
	}
}
