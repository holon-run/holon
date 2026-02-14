package docker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/docker/docker/api/types/mount"
	"github.com/holon-run/holon/pkg/api/v1"
)

func TestParseAgentConfigMode(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		want    AgentConfigMode
		wantErr bool
	}{
		{
			name:  "auto lowercase",
			input: "auto",
			want:  AgentConfigModeAuto,
		},
		{
			name:  "auto uppercase",
			input: "AUTO",
			want:  AgentConfigModeAuto,
		},
		{
			name:  "auto mixed case",
			input: "AuTo",
			want:  AgentConfigModeAuto,
		},
		{
			name:  "auto with spaces",
			input: "  auto  ",
			want:  AgentConfigModeAuto,
		},
		{
			name:  "yes lowercase",
			input: "yes",
			want:  AgentConfigModeYes,
		},
		{
			name:  "yes uppercase",
			input: "YES",
			want:  AgentConfigModeYes,
		},
		{
			name:  "y alias",
			input: "y",
			want:  AgentConfigModeYes,
		},
		{
			name:  "true alias",
			input: "true",
			want:  AgentConfigModeYes,
		},
		{
			name:  "1 alias",
			input: "1",
			want:  AgentConfigModeYes,
		},
		{
			name:  "no lowercase",
			input: "no",
			want:  AgentConfigModeNo,
		},
		{
			name:  "no uppercase",
			input: "NO",
			want:  AgentConfigModeNo,
		},
		{
			name:  "n alias",
			input: "n",
			want:  AgentConfigModeNo,
		},
		{
			name:  "false alias",
			input: "false",
			want:  AgentConfigModeNo,
		},
		{
			name:  "0 alias",
			input: "0",
			want:  AgentConfigModeNo,
		},
		{
			name:    "invalid value",
			input:   "invalid",
			wantErr: true,
		},
		{
			name:    "empty string",
			input:   "",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseAgentConfigMode(tt.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParseAgentConfigMode() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if got != tt.want {
				t.Errorf("ParseAgentConfigMode() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestAgentConfigModeString(t *testing.T) {
	tests := []struct {
		mode AgentConfigMode
		want string
	}{
		{AgentConfigModeAuto, "auto"},
		{AgentConfigModeYes, "yes"},
		{AgentConfigModeNo, "no"},
	}

	for _, tt := range tests {
		t.Run(tt.want, func(t *testing.T) {
			if got := tt.mode.String(); got != tt.want {
				t.Errorf("AgentConfigMode.String() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestAgentConfigModeShouldMount(t *testing.T) {
	tests := []struct {
		name      string
		mode      AgentConfigMode
		dirExists bool
		want      bool
	}{
		{
			name:      "auto with existing dir",
			mode:      AgentConfigModeAuto,
			dirExists: true,
			want:      true,
		},
		{
			name:      "auto without existing dir",
			mode:      AgentConfigModeAuto,
			dirExists: false,
			want:      false,
		},
		{
			name:      "yes with existing dir",
			mode:      AgentConfigModeYes,
			dirExists: true,
			want:      true,
		},
		{
			name:      "yes without existing dir",
			mode:      AgentConfigModeYes,
			dirExists: false,
			want:      true, // yes always tries to mount
		},
		{
			name:      "no with existing dir",
			mode:      AgentConfigModeNo,
			dirExists: true,
			want:      false,
		},
		{
			name:      "no without existing dir",
			mode:      AgentConfigModeNo,
			dirExists: false,
			want:      false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.mode.ShouldMount(tt.dirExists); got != tt.want {
				t.Errorf("AgentConfigMode.ShouldMount() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestAgentConfigModeWarnIfMissing(t *testing.T) {
	tests := []struct {
		mode AgentConfigMode
		want bool
	}{
		{AgentConfigModeAuto, false},
		{AgentConfigModeYes, true},
		{AgentConfigModeNo, false},
	}

	for _, tt := range tests {
		t.Run(tt.mode.String(), func(t *testing.T) {
			if got := tt.mode.WarnIfMissing(); got != tt.want {
				t.Errorf("AgentConfigMode.WarnIfMissing() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestBuildContainerMounts(t *testing.T) {
	// Create temporary directories for testing
	tmpDir := t.TempDir()
	inputDir := filepath.Join(tmpDir, "input")
	outDir := filepath.Join(tmpDir, "output")
	snapshotDir := filepath.Join(tmpDir, "snapshot")

	// Create required directories
	if err := os.MkdirAll(inputDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(outDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(tmpDir, "agent-dist"), 0755); err != nil {
		t.Fatal(err)
	}

	tests := []struct {
		name     string
		cfg      *MountConfig
		expected []mount.Mount
	}{
		{
			name: "required mounts only",
			cfg: &MountConfig{
				SnapshotDir: snapshotDir,
				InputPath:   inputDir,
				OutDir:      outDir,
			},
			expected: []mount.Mount{
				{
					Type:   mount.TypeBind,
					Source: snapshotDir,
					Target: ContainerWorkspaceDir,
				},
				{
					Type:     mount.TypeBind,
					Source:   inputDir,
					Target:   ContainerInputDir,
					ReadOnly: true,
				},
				{
					Type:   mount.TypeBind,
					Source: outDir,
					Target: ContainerOutputDir,
				},
			},
		},
		{
			name: "includes state mount when configured",
			cfg: &MountConfig{
				SnapshotDir: snapshotDir,
				InputPath:   inputDir,
				OutDir:      outDir,
				StateDir:    filepath.Join(tmpDir, "state"),
			},
			expected: []mount.Mount{
				{
					Type:   mount.TypeBind,
					Source: snapshotDir,
					Target: ContainerWorkspaceDir,
				},
				{
					Type:     mount.TypeBind,
					Source:   inputDir,
					Target:   ContainerInputDir,
					ReadOnly: true,
				},
				{
					Type:   mount.TypeBind,
					Source: outDir,
					Target: ContainerOutputDir,
				},
				{
					Type:   mount.TypeBind,
					Source: filepath.Join(tmpDir, "state"),
					Target: ContainerStateDir,
				},
			},
		},
		{
			name: "includes local agent dist mount when configured",
			cfg: &MountConfig{
				SnapshotDir:       snapshotDir,
				InputPath:         inputDir,
				OutDir:            outDir,
				LocalAgentDistDir: filepath.Join(tmpDir, "agent-dist"),
			},
			expected: []mount.Mount{
				{
					Type:   mount.TypeBind,
					Source: snapshotDir,
					Target: ContainerWorkspaceDir,
				},
				{
					Type:     mount.TypeBind,
					Source:   inputDir,
					Target:   ContainerInputDir,
					ReadOnly: true,
				},
				{
					Type:   mount.TypeBind,
					Source: outDir,
					Target: ContainerOutputDir,
				},
				{
					Type:     mount.TypeBind,
					Source:   filepath.Join(tmpDir, "agent-dist"),
					Target:   "/holon/agent/dist",
					ReadOnly: true,
				},
			},
		},
		{
			name: "includes agent home mount when configured",
			cfg: &MountConfig{
				SnapshotDir: snapshotDir,
				InputPath:   inputDir,
				OutDir:      outDir,
				AgentHome:   filepath.Join(tmpDir, "agent-home"),
			},
			expected: []mount.Mount{
				{
					Type:   mount.TypeBind,
					Source: filepath.Join(tmpDir, "agent-home"),
					Target: ContainerAgentHome,
				},
				{
					Type:   mount.TypeBind,
					Source: snapshotDir,
					Target: ContainerWorkspaceDir,
				},
				{
					Type:     mount.TypeBind,
					Source:   inputDir,
					Target:   ContainerInputDir,
					ReadOnly: true,
				},
				{
					Type:   mount.TypeBind,
					Source: outDir,
					Target: ContainerOutputDir,
				},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := BuildContainerMounts(tt.cfg)

			if len(result) != len(tt.expected) {
				t.Errorf("BuildContainerMounts() returned %d mounts, expected %d", len(result), len(tt.expected))
				return
			}

			for i := range result {
				if result[i].Type != tt.expected[i].Type {
					t.Errorf("mount %d: Type = %v, want %v", i, result[i].Type, tt.expected[i].Type)
				}
				if result[i].Source != tt.expected[i].Source {
					t.Errorf("mount %d: Source = %v, want %v", i, result[i].Source, tt.expected[i].Source)
				}
				if result[i].Target != tt.expected[i].Target {
					t.Errorf("mount %d: Target = %v, want %v", i, result[i].Target, tt.expected[i].Target)
				}
				if result[i].ReadOnly != tt.expected[i].ReadOnly {
					t.Errorf("mount %d: ReadOnly = %v, want %v", i, result[i].ReadOnly, tt.expected[i].ReadOnly)
				}
				if tt.expected[i].BindOptions != nil && result[i].BindOptions == nil {
					t.Errorf("mount %d: BindOptions missing", i)
				}
			}
		})
	}
}

func TestBuildContainerEnv(t *testing.T) {
	tests := []struct {
		name     string
		cfg      *EnvConfig
		contains []string
	}{
		{
			name: "basic env vars",
			cfg: &EnvConfig{
				UserEnv: map[string]string{
					"TEST_VAR": "test_value",
				},
				HostUID: 1000,
				HostGID: 1000,
			},
			contains: []string{
				"TEST_VAR=test_value",
				"HOST_UID=1000",
				"HOST_GID=1000",
				"GIT_CONFIG_NOSYSTEM=1",
			},
		},
		{
			name: "with secret injection",
			cfg: &EnvConfig{
				UserEnv: map[string]string{
					"ANTHROPIC_API_KEY": "sk-test-key",
				},
				HostUID: 1000,
				HostGID: 1000,
			},
			contains: []string{
				"ANTHROPIC_API_KEY=sk-test-key",
				"HOST_UID=1000",
				"HOST_GID=1000",
				"GIT_CONFIG_NOSYSTEM=1",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := BuildContainerEnv(tt.cfg)

			for _, expected := range tt.contains {
				found := false
				for _, envVar := range result {
					if envVar == expected {
						found = true
						break
					}
				}
				if !found {
					t.Errorf("BuildContainerEnv() missing expected env var %q. Got: %v", expected, result)
				}
			}
		})
	}
}

func TestBuildContainerHostConfig(t *testing.T) {
	baseMounts := []mount.Mount{
		{
			Type:   mount.TypeBind,
			Source: "/tmp/workspace",
			Target: ContainerWorkspaceDir,
		},
	}

	hostCfg := BuildContainerHostConfig(&HostConfigOptions{Mounts: baseMounts})
	if hostCfg == nil {
		t.Fatal("BuildContainerHostConfig() returned nil")
	}
	if hostCfg.Privileged {
		t.Error("BuildContainerHostConfig() set Privileged=true, want false")
	}
	if hostCfg.ReadonlyRootfs {
		t.Error("BuildContainerHostConfig() set ReadonlyRootfs=true, want false")
	}
	networkMode := string(hostCfg.NetworkMode)
	if strings.HasPrefix(networkMode, "container:") || networkMode == "host" {
		t.Errorf("BuildContainerHostConfig() NetworkMode = %q, expected isolated/default networking", hostCfg.NetworkMode)
	}
	if string(hostCfg.PidMode) != "" {
		t.Errorf("BuildContainerHostConfig() PidMode = %q, want empty", hostCfg.PidMode)
	}
	if len(hostCfg.Mounts) != len(baseMounts) {
		t.Errorf("BuildContainerHostConfig() mounts = %d, want %d", len(hostCfg.Mounts), len(baseMounts))
	}

	t.Run("nil config returns safe defaults", func(t *testing.T) {
		hostCfg := BuildContainerHostConfig(nil)
		if hostCfg == nil {
			t.Fatal("BuildContainerHostConfig(nil) returned nil")
		}
		if hostCfg.Privileged {
			t.Error("BuildContainerHostConfig(nil) set Privileged=true, want false")
		}
	})
}

func TestValidateRequiredArtifacts(t *testing.T) {
	t.Run("all required artifacts present", func(t *testing.T) {
		tmpDir := t.TempDir()
		manifestPath := filepath.Join(tmpDir, "manifest.json")
		if err := os.WriteFile(manifestPath, []byte(`{"status": "success"}`), 0644); err != nil {
			t.Fatal(err)
		}

		requiredArtifacts := []v1.Artifact{
			{Path: "manifest.json", Required: true},
		}

		if err := ValidateRequiredArtifacts(tmpDir, requiredArtifacts); err != nil {
			t.Errorf("ValidateRequiredArtifacts() error = %v", err)
		}
	})

	t.Run("missing required artifact", func(t *testing.T) {
		tmpDir := t.TempDir()
		manifestPath := filepath.Join(tmpDir, "manifest.json")
		if err := os.WriteFile(manifestPath, []byte(`{"status": "success"}`), 0644); err != nil {
			t.Fatal(err)
		}

		requiredArtifacts := []v1.Artifact{
			{Path: "manifest.json", Required: true},
			{Path: "diff.patch", Required: true},
		}

		if err := ValidateRequiredArtifacts(tmpDir, requiredArtifacts); err == nil {
			t.Error("ValidateRequiredArtifacts() expected error for missing diff.patch, got nil")
		}
	})

	t.Run("missing manifest.json", func(t *testing.T) {
		tmpDir := t.TempDir()

		requiredArtifacts := []v1.Artifact{}

		err := ValidateRequiredArtifacts(tmpDir, requiredArtifacts)
		if err == nil {
			t.Error("ValidateRequiredArtifacts() expected error for missing manifest.json, got nil")
			return
		}
		if !strings.Contains(err.Error(), "manifest.json") {
			t.Errorf("ValidateRequiredArtifacts() error = %q, want mention of manifest.json", err.Error())
		}
	})

	t.Run("optional artifact ignored", func(t *testing.T) {
		tmpDir := t.TempDir()
		manifestPath := filepath.Join(tmpDir, "manifest.json")
		if err := os.WriteFile(manifestPath, []byte(`{"status": "success"}`), 0644); err != nil {
			t.Fatal(err)
		}

		requiredArtifacts := []v1.Artifact{
			{Path: "manifest.json", Required: true},
			{Path: "summary.md", Required: false},
		}

		if err := ValidateRequiredArtifacts(tmpDir, requiredArtifacts); err != nil {
			t.Errorf("ValidateRequiredArtifacts() error = %v", err)
		}
	})
}

func TestValidateMountTargets(t *testing.T) {
	t.Run("all required mounts valid", func(t *testing.T) {
		tmpDir := t.TempDir()
		inputDir := filepath.Join(tmpDir, "input")
		outDir := filepath.Join(tmpDir, "output")
		snapshotDir := filepath.Join(tmpDir, "snapshot")

		if err := os.MkdirAll(inputDir, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(outDir, 0755); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: snapshotDir,
			InputPath:   inputDir,
			OutDir:      outDir,
		}

		if err := ValidateMountTargets(cfg); err != nil {
			t.Errorf("ValidateMountTargets() error = %v", err)
		}
	})

	t.Run("missing input path", func(t *testing.T) {
		tmpDir := t.TempDir()
		outDir := filepath.Join(tmpDir, "output")
		snapshotDir := filepath.Join(tmpDir, "snapshot")

		if err := os.MkdirAll(outDir, 0755); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: snapshotDir,
			InputPath:   "/nonexistent/input",
			OutDir:      outDir,
		}

		if err := ValidateMountTargets(cfg); err == nil {
			t.Error("ValidateMountTargets() expected error for missing input path, got nil")
		}
	})

	t.Run("missing output directory", func(t *testing.T) {
		tmpDir := t.TempDir()
		inputDir := filepath.Join(tmpDir, "input")
		snapshotDir := filepath.Join(tmpDir, "snapshot")

		if err := os.MkdirAll(inputDir, 0755); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: snapshotDir,
			InputPath:   inputDir,
			OutDir:      "/nonexistent/output",
		}

		if err := ValidateMountTargets(cfg); err == nil {
			t.Error("ValidateMountTargets() expected error for missing output directory, got nil")
		}
	})

	t.Run("empty snapshot directory", func(t *testing.T) {
		tmpDir := t.TempDir()
		inputDir := filepath.Join(tmpDir, "input")
		outDir := filepath.Join(tmpDir, "output")

		if err := os.MkdirAll(inputDir, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(outDir, 0755); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: "",
			InputPath:   inputDir,
			OutDir:      outDir,
		}

		if err := ValidateMountTargets(cfg); err == nil {
			t.Error("ValidateMountTargets() expected error for empty snapshot directory, got nil")
		}
	})

	t.Run("empty input path", func(t *testing.T) {
		tmpDir := t.TempDir()
		outDir := filepath.Join(tmpDir, "output")
		snapshotDir := filepath.Join(tmpDir, "snapshot")

		if err := os.MkdirAll(outDir, 0755); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: snapshotDir,
			InputPath:   "",
			OutDir:      outDir,
		}

		if err := ValidateMountTargets(cfg); err == nil {
			t.Error("ValidateMountTargets() expected error for empty input path, got nil")
		}
	})

	t.Run("state directory auto-created when missing", func(t *testing.T) {
		tmpDir := t.TempDir()
		inputDir := filepath.Join(tmpDir, "input")
		outDir := filepath.Join(tmpDir, "output")
		snapshotDir := filepath.Join(tmpDir, "snapshot")
		stateDir := filepath.Join(tmpDir, "state")

		if err := os.MkdirAll(inputDir, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(outDir, 0755); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: snapshotDir,
			InputPath:   inputDir,
			OutDir:      outDir,
			StateDir:    stateDir,
		}

		if err := ValidateMountTargets(cfg); err != nil {
			t.Fatalf("ValidateMountTargets() error = %v", err)
		}
		if fi, err := os.Stat(stateDir); err != nil {
			t.Fatalf("expected state directory to exist: %v", err)
		} else if !fi.IsDir() {
			t.Fatalf("expected state path to be directory, got file: %s", stateDir)
		}
	})

	t.Run("state path must be directory", func(t *testing.T) {
		tmpDir := t.TempDir()
		inputDir := filepath.Join(tmpDir, "input")
		outDir := filepath.Join(tmpDir, "output")
		snapshotDir := filepath.Join(tmpDir, "snapshot")
		statePath := filepath.Join(tmpDir, "state-file")

		if err := os.MkdirAll(inputDir, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(outDir, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(statePath, []byte("not-a-dir"), 0644); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir: snapshotDir,
			InputPath:   inputDir,
			OutDir:      outDir,
			StateDir:    statePath,
		}

		if err := ValidateMountTargets(cfg); err == nil {
			t.Fatalf("ValidateMountTargets() expected error for state file path, got nil")
		}
	})

	t.Run("local agent dist directory must exist in dev mode", func(t *testing.T) {
		tmpDir := t.TempDir()
		inputDir := filepath.Join(tmpDir, "input")
		outDir := filepath.Join(tmpDir, "output")
		snapshotDir := filepath.Join(tmpDir, "snapshot")
		distDir := filepath.Join(tmpDir, "missing-dist")

		if err := os.MkdirAll(inputDir, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(outDir, 0755); err != nil {
			t.Fatal(err)
		}

		cfg := &MountConfig{
			SnapshotDir:       snapshotDir,
			InputPath:         inputDir,
			OutDir:            outDir,
			LocalAgentDistDir: distDir,
		}

		if err := ValidateMountTargets(cfg); err == nil {
			t.Fatalf("ValidateMountTargets() expected error for missing local agent dist directory, got nil")
		}
	})
}

func TestInputMountReadOnly(t *testing.T) {
	// Test that input directory is mounted read-only to prevent modification of context files
	// This is a security feature to ensure PR context cannot be overwritten by the agent
	tmpDir := t.TempDir()
	inputDir := filepath.Join(tmpDir, "input")
	outDir := filepath.Join(tmpDir, "output")
	snapshotDir := filepath.Join(tmpDir, "snapshot")

	// Create required directories
	if err := os.MkdirAll(inputDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(outDir, 0755); err != nil {
		t.Fatal(err)
	}

	cfg := &MountConfig{
		SnapshotDir: snapshotDir,
		InputPath:   inputDir,
		OutDir:      outDir,
	}

	mounts := BuildContainerMounts(cfg)

	// Find the input mount
	var inputMount *mount.Mount
	for i := range mounts {
		if mounts[i].Target == ContainerInputDir {
			inputMount = &mounts[i]
			break
		}
	}

	if inputMount == nil {
		t.Fatalf("BuildContainerMounts() did not create %s mount", ContainerInputDir)
	}

	// Verify that the input mount is read-only
	if !inputMount.ReadOnly {
		t.Error("BuildContainerMounts() input mount is not read-only. Input directory must be mounted read-only to prevent context modification")
	}

	// Verify mount type is bind
	if inputMount.Type != mount.TypeBind {
		t.Errorf("BuildContainerMounts() input mount type = %v, want %v", inputMount.Type, mount.TypeBind)
	}

	// Verify source is correct
	if inputMount.Source != inputDir {
		t.Errorf("BuildContainerMounts() input mount source = %v, want %v", inputMount.Source, inputDir)
	}
}

func TestWorkspaceAndOutputMountsReadWrite(t *testing.T) {
	// Test that workspace and output mounts are read-write
	// This ensures the agent can write code changes and outputs
	tmpDir := t.TempDir()
	inputDir := filepath.Join(tmpDir, "input")
	outDir := filepath.Join(tmpDir, "output")
	snapshotDir := filepath.Join(tmpDir, "snapshot")

	// Create required directories
	if err := os.MkdirAll(inputDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(outDir, 0755); err != nil {
		t.Fatal(err)
	}

	cfg := &MountConfig{
		SnapshotDir: snapshotDir,
		InputPath:   inputDir,
		OutDir:      outDir,
	}

	mounts := BuildContainerMounts(cfg)

	// Find workspace and output mounts
	var workspaceMount, outputMount *mount.Mount
	for i := range mounts {
		if mounts[i].Target == ContainerWorkspaceDir {
			workspaceMount = &mounts[i]
		}
		if mounts[i].Target == ContainerOutputDir {
			outputMount = &mounts[i]
		}
	}

	if workspaceMount == nil {
		t.Fatalf("BuildContainerMounts() did not create %s mount", ContainerWorkspaceDir)
	}
	if outputMount == nil {
		t.Fatalf("BuildContainerMounts() did not create %s mount", ContainerOutputDir)
	}

	// Verify workspace mount is read-write
	if workspaceMount.ReadOnly {
		t.Error("BuildContainerMounts() workspace mount is read-only. Workspace must be read-write for code changes")
	}

	// Verify output mount is read-write
	if outputMount.ReadOnly {
		t.Error("BuildContainerMounts() output mount is read-only. Output must be read-write for artifact creation")
	}
}
