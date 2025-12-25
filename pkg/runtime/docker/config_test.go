package docker

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/docker/docker/api/types/mount"
	"github.com/holon-run/holon/pkg/api/v1"
)

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
					Target: "/holon/workspace",
				},
				{
					Type:   mount.TypeBind,
					Source: inputDir,
					Target: "/holon/input",
				},
				{
					Type:   mount.TypeBind,
					Source: outDir,
					Target: "/holon/output",
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

		if err := ValidateRequiredArtifacts(tmpDir, requiredArtifacts); err == nil {
			t.Error("ValidateRequiredArtifacts() expected error for missing manifest.json, got nil")
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
}
