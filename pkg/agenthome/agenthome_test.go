package agenthome

import (
	"os"
	"path/filepath"
	"strings"
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
	t.Cleanup(func() {
		_ = os.RemoveAll(res.AgentHome)
	})
	if !res.Ephemeral {
		t.Fatalf("expected ephemeral resolution")
	}
	if _, err := os.Stat(res.AgentHome); err != nil {
		t.Fatalf("temp home should exist: %v", err)
	}
}

func TestResolveRunWithAgentID_IsPersistent(t *testing.T) {
	res, err := Resolve(ResolveOptions{
		Command:          "run",
		AgentID:          "custom",
		EphemeralAllowed: true,
	})
	if err != nil {
		t.Fatalf("resolve run with agent id: %v", err)
	}
	if res.AgentID != "custom" {
		t.Fatalf("expected custom agent id, got %s", res.AgentID)
	}
	if filepath.Base(res.AgentHome) != "custom" {
		t.Fatalf("expected agent home suffix custom, got %s", res.AgentHome)
	}
	if res.Ephemeral {
		t.Fatalf("run with explicit agent id should not be ephemeral")
	}
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
		filepath.Join(home, "AGENTS.md"),
		filepath.Join(home, "CLAUDE.md"),
		filepath.Join(home, "ROLE.md"),
		filepath.Join(home, "IDENTITY.md"),
		filepath.Join(home, "SOUL.md"),
		filepath.Join(home, "agent.yaml"),
	} {
		if _, err := os.Stat(path); err != nil {
			t.Fatalf("expected %s to exist: %v", path, err)
		}
	}
	if _, err := os.Stat(filepath.Join(home, "workspace")); err == nil {
		t.Fatalf("workspace dir should not be created by default")
	} else if !os.IsNotExist(err) {
		t.Fatalf("unexpected error stating workspace dir: %v", err)
	}
}

func TestEnsureLayoutWithOptions_TemplateDefault(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")

	if err := EnsureLayoutWithOptions(home, InitOptions{Template: TemplateDefault}); err != nil {
		t.Fatalf("ensure layout with run template: %v", err)
	}

	roleData, err := os.ReadFile(filepath.Join(home, "ROLE.md"))
	if err != nil {
		t.Fatalf("read ROLE.md: %v", err)
	}
	if !strings.Contains(string(roleData), "ROLE: EXECUTOR") {
		t.Fatalf("expected default ROLE.md content, got: %s", string(roleData))
	}
}

func TestEnsureLayoutWithOptions_TemplateGitHubSolver(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")

	if err := EnsureLayoutWithOptions(home, InitOptions{Template: TemplateGitHubSolver}); err != nil {
		t.Fatalf("ensure layout with solve template: %v", err)
	}

	roleData, err := os.ReadFile(filepath.Join(home, "ROLE.md"))
	if err != nil {
		t.Fatalf("read ROLE.md: %v", err)
	}
	if !strings.Contains(string(roleData), "ROLE: GITHUB_SOLVER") {
		t.Fatalf("expected github-solver ROLE.md content, got: %s", string(roleData))
	}
}

func TestEnsureLayoutWithOptions_ForceOverwritesPersonaFiles(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")

	if err := EnsureLayoutWithOptions(home, InitOptions{Template: TemplateDefault}); err != nil {
		t.Fatalf("initial ensure layout: %v", err)
	}
	if err := os.WriteFile(filepath.Join(home, "ROLE.md"), []byte("custom role"), 0o644); err != nil {
		t.Fatalf("write ROLE.md: %v", err)
	}
	if err := EnsureLayoutWithOptions(home, InitOptions{Template: TemplateAutonomous, Force: true}); err != nil {
		t.Fatalf("ensure layout with force: %v", err)
	}

	roleData, err := os.ReadFile(filepath.Join(home, "ROLE.md"))
	if err != nil {
		t.Fatalf("read ROLE.md: %v", err)
	}
	if !strings.Contains(string(roleData), "persistent autonomous PM agent") {
		t.Fatalf("expected force overwrite with autonomous content, got: %s", string(roleData))
	}
}

func TestEnsureLayoutWithOptions_InvalidTemplate(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")

	err := EnsureLayoutWithOptions(home, InitOptions{Template: "unknown"})
	if err == nil {
		t.Fatal("expected error for unsupported template")
	}
	if !strings.Contains(err.Error(), "unsupported template") {
		t.Fatalf("expected unsupported template error, got: %v", err)
	}
}

func TestLoadPersonaTemplate_FromAssets(t *testing.T) {
	for _, template := range AvailableTemplates() {
		template := template
		t.Run(template, func(t *testing.T) {
			files, err := loadPersonaTemplate(template)
			if err != nil {
				t.Fatalf("loadPersonaTemplate(%q): %v", template, err)
			}
			for _, name := range []string{"AGENTS.md", "CLAUDE.md", "ROLE.md", "IDENTITY.md", "SOUL.md"} {
				if _, ok := files[name]; !ok {
					t.Fatalf("template %q missing %s", template, name)
				}
			}
			if !strings.Contains(files["CLAUDE.md"], "AGENTS.md") {
				t.Fatalf("template %q CLAUDE.md should point to AGENTS.md", template)
			}
		})
	}
}

func TestEnsureLayout_InvalidExistingConfig(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")
	if err := os.MkdirAll(home, 0o755); err != nil {
		t.Fatalf("mkdir home: %v", err)
	}
	cfgPath := filepath.Join(home, "agent.yaml")
	if err := os.WriteFile(cfgPath, []byte("version: v1\nagent:\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	if err := EnsureLayout(home); err == nil {
		t.Fatalf("expected EnsureLayout to fail with invalid config")
	}
}

func TestEnsureLayout_FailsWhenPersonaPathIsDirectory(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")
	if err := os.MkdirAll(filepath.Join(home, "AGENTS.md"), 0o755); err != nil {
		t.Fatalf("mkdir AGENTS.md dir: %v", err)
	}
	if err := EnsureLayout(home); err == nil {
		t.Fatalf("expected EnsureLayout to fail when AGENTS.md is a directory")
	}
}

func TestResolveAgentHome_WithInvalidDerivedID(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "my agent")
	_, err := Resolve(ResolveOptions{AgentHome: home})
	if err == nil {
		t.Fatalf("expected error for invalid derived agent id")
	}
}

func TestLoadConfig_RuntimeMountsValid(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")
	if err := EnsureLayout(home); err != nil {
		t.Fatalf("ensure layout: %v", err)
	}

	desktop := filepath.Join(td, "Desktop")
	sshDir := filepath.Join(td, ".ssh")
	if err := os.MkdirAll(desktop, 0o755); err != nil {
		t.Fatalf("mkdir desktop: %v", err)
	}
	if err := os.MkdirAll(sshDir, 0o700); err != nil {
		t.Fatalf("mkdir .ssh: %v", err)
	}

	cfgData := "version: v1\nagent:\n  id: main\n  profile: default\nruntime:\n  mounts:\n    - path: " + desktop + "\n      mode: rw\n    - path: " + sshDir + "\n"
	if err := os.WriteFile(filepath.Join(home, "agent.yaml"), []byte(cfgData), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	cfg, err := LoadConfig(home)
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if len(cfg.Runtime.Mounts) != 2 {
		t.Fatalf("runtime mounts = %d, want 2", len(cfg.Runtime.Mounts))
	}
	if cfg.Runtime.Mounts[0].Mode != "rw" && cfg.Runtime.Mounts[1].Mode != "rw" {
		t.Fatalf("expected one rw mount, got %+v", cfg.Runtime.Mounts)
	}
}

func TestLoadConfig_RuntimeMountsRejectInvalid(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")
	if err := EnsureLayout(home); err != nil {
		t.Fatalf("ensure layout: %v", err)
	}
	existsDir := filepath.Join(td, "exists")
	if err := os.MkdirAll(existsDir, 0o755); err != nil {
		t.Fatalf("mkdir exists: %v", err)
	}

	tests := []struct {
		name    string
		mounts  string
		wantErr string
	}{
		{
			name:    "relative path",
			mounts:  "  mounts:\n    - path: ./relative\n",
			wantErr: "absolute path",
		},
		{
			name:    "filesystem root",
			mounts:  "  mounts:\n    - path: /\n",
			wantErr: "cannot be filesystem root",
		},
		{
			name:    "missing path",
			mounts:  "  mounts:\n    - path: " + filepath.Join(td, "missing") + "\n",
			wantErr: "does not exist",
		},
		{
			name:    "invalid mode",
			mounts:  "  mounts:\n    - path: " + existsDir + "\n      mode: write\n",
			wantErr: "must be ro or rw",
		},
		{
			name:    "overlap",
			mounts:  "  mounts:\n    - path: " + existsDir + "\n    - path: " + filepath.Join(existsDir, "child") + "\n",
			wantErr: "overlapping paths",
		},
	}

	if err := os.MkdirAll(filepath.Join(existsDir, "child"), 0o755); err != nil {
		t.Fatalf("mkdir child: %v", err)
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfgData := "version: v1\nagent:\n  id: main\n  profile: default\nruntime:\n" + tt.mounts
			if err := os.WriteFile(filepath.Join(home, "agent.yaml"), []byte(cfgData), 0o644); err != nil {
				t.Fatalf("write config: %v", err)
			}
			_, err := LoadConfig(home)
			if err == nil {
				t.Fatalf("expected error")
			}
			if !strings.Contains(err.Error(), tt.wantErr) {
				t.Fatalf("error %q does not contain %q", err.Error(), tt.wantErr)
			}
		})
	}
}

func TestLoadConfig_RuntimeMountsRejectSymlinkOverlap(t *testing.T) {
	td := t.TempDir()
	home := filepath.Join(td, "agent-home")
	if err := EnsureLayout(home); err != nil {
		t.Fatalf("ensure layout: %v", err)
	}

	realDir := filepath.Join(td, "real")
	linkDir := filepath.Join(td, "link")
	if err := os.MkdirAll(realDir, 0o755); err != nil {
		t.Fatalf("mkdir real: %v", err)
	}
	if err := os.Symlink(realDir, linkDir); err != nil {
		t.Fatalf("symlink: %v", err)
	}

	cfgData := "version: v1\nagent:\n  id: main\n  profile: default\nruntime:\n  mounts:\n    - path: " + realDir + "\n    - path: " + linkDir + "\n"
	if err := os.WriteFile(filepath.Join(home, "agent.yaml"), []byte(cfgData), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	_, err := LoadConfig(home)
	if err == nil {
		t.Fatalf("expected overlap error for symlink/real path")
	}
	if !strings.Contains(err.Error(), "duplicates") && !strings.Contains(err.Error(), "overlapping paths") {
		t.Fatalf("unexpected error: %v", err)
	}
}
