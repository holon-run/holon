package agenthome

import (
	"os"
	"path/filepath"
	"testing"
)

func TestValidateAgentID(t *testing.T) {
	valid := []string{"main", "pm_agent", "dev-1", "A1"}
	for _, id := range valid {
		if err := ValidateAgentID(id); err != nil {
			t.Fatalf("expected valid id %q, got error: %v", id, err)
		}
	}

	invalid := []string{"", "a b", "a/b", "a.b"}
	for _, id := range invalid {
		if err := ValidateAgentID(id); err == nil {
			t.Fatalf("expected invalid id %q", id)
		}
	}
}

func TestResolveServeDefault(t *testing.T) {
	res, err := Resolve(ResolveOptions{Command: "serve"})
	if err != nil {
		t.Fatalf("resolve serve: %v", err)
	}
	if res.AgentID != "main" {
		t.Fatalf("expected main, got %s", res.AgentID)
	}
	if filepath.Base(res.AgentHome) != "main" {
		t.Fatalf("expected agent home suffix main, got %s", res.AgentHome)
	}
	if res.Ephemeral {
		t.Fatalf("serve default should not be ephemeral")
	}
}

func TestResolveEphemeral(t *testing.T) {
	res, err := Resolve(ResolveOptions{Command: "run", EphemeralAllowed: true})
	if err != nil {
		t.Fatalf("resolve ephemeral: %v", err)
	}
	if !res.Ephemeral {
		t.Fatalf("expected ephemeral resolution")
	}
	if _, err := os.Stat(res.AgentHome); err != nil {
		t.Fatalf("temp home should exist: %v", err)
	}
	_ = os.RemoveAll(res.AgentHome)
}

func TestEnsureLayout(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")

	if err := EnsureLayout(home); err != nil {
		t.Fatalf("ensure layout: %v", err)
	}
	for _, path := range []string{
		filepath.Join(home, "state"),
		filepath.Join(home, "sessions"),
		filepath.Join(home, "channels"),
		filepath.Join(home, "jobs"),
		filepath.Join(home, "workspace"),
		filepath.Join(home, "AGENT.md"),
		filepath.Join(home, "ROLE.md"),
		filepath.Join(home, "IDENTITY.md"),
		filepath.Join(home, "SOUL.md"),
		filepath.Join(home, "agent.yaml"),
	} {
		if _, err := os.Stat(path); err != nil {
			t.Fatalf("expected %s to exist: %v", path, err)
		}
	}
}
