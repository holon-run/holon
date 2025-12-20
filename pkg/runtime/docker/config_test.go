package docker

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/docker/docker/api/types/mount"
	"github.com/jolestar/holon/pkg/api/v1"
)

func TestBuildContainerMounts(t *testing.T) {
	tests := []struct {
		name     string
		cfg      *MountConfig
		expected []mount.Mount
	}{
		{
			name: "required mounts only",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    "/path/to/spec.yaml",
				OutDir:      "/tmp/output",
			},
			expected: []mount.Mount{
				{
					Type:   mount.TypeBind,
					Source: "/tmp/snapshot",
					Target: "/holon/workspace",
				},
				{
					Type:   mount.TypeBind,
					Source: "/path/to/spec.yaml",
					Target: "/holon/input/spec.yaml",
				},
				{
					Type:   mount.TypeBind,
					Source: "/tmp/output",
					Target: "/holon/output",
				},
			},
		},
		{
			name: "all mounts including optional",
			cfg: &MountConfig{
				SnapshotDir:    "/tmp/snapshot",
				SpecPath:       "/path/to/spec.yaml",
				ContextPath:    "/path/to/context",
				OutDir:         "/tmp/output",
				PromptPath:     "/path/to/system.md",
				UserPromptPath: "/path/to/user.md",
			},
			expected: []mount.Mount{
				{
					Type:   mount.TypeBind,
					Source: "/tmp/snapshot",
					Target: "/holon/workspace",
				},
				{
					Type:   mount.TypeBind,
					Source: "/path/to/spec.yaml",
					Target: "/holon/input/spec.yaml",
				},
				{
					Type:   mount.TypeBind,
					Source: "/tmp/output",
					Target: "/holon/output",
				},
				{
					Type:   mount.TypeBind,
					Source: "/path/to/context",
					Target: "/holon/input/context",
				},
				{
					Type:   mount.TypeBind,
					Source: "/path/to/system.md",
					Target: "/holon/input/prompts/system.md",
				},
				{
					Type:   mount.TypeBind,
					Source: "/path/to/user.md",
					Target: "/holon/input/prompts/user.md",
				},
			},
		},
		{
			name: "partial optional mounts",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    "/path/to/spec.yaml",
				OutDir:      "/tmp/output",
				PromptPath:  "/path/to/system.md",
			},
			expected: []mount.Mount{
				{
					Type:   mount.TypeBind,
					Source: "/tmp/snapshot",
					Target: "/holon/workspace",
				},
				{
					Type:   mount.TypeBind,
					Source: "/path/to/spec.yaml",
					Target: "/holon/input/spec.yaml",
				},
				{
					Type:   mount.TypeBind,
					Source: "/tmp/output",
					Target: "/holon/output",
				},
				{
					Type:   mount.TypeBind,
					Source: "/path/to/system.md",
					Target: "/holon/input/prompts/system.md",
				},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := BuildContainerMounts(tt.cfg)

			if len(result) != len(tt.expected) {
				t.Errorf("Expected %d mounts, got %d", len(tt.expected), len(result))
			}

			// Compare mount structures
			for i, expectedMount := range tt.expected {
				if i >= len(result) {
					t.Errorf("Missing mount at index %d", i)
					continue
				}

				actualMount := result[i]
				if actualMount.Type != expectedMount.Type {
					t.Errorf("Mount %d: expected type %v, got %v", i, expectedMount.Type, actualMount.Type)
				}
				if actualMount.Source != expectedMount.Source {
					t.Errorf("Mount %d: expected source %s, got %s", i, expectedMount.Source, actualMount.Source)
				}
				if actualMount.Target != expectedMount.Target {
					t.Errorf("Mount %d: expected target %s, got %s", i, expectedMount.Target, actualMount.Target)
				}
			}
		})
	}
}

func TestBuildContainerEnv(t *testing.T) {
	tests := []struct {
		name     string
		cfg      *EnvConfig
		expected []string
	}{
		{
			name: "no user env",
			cfg: &EnvConfig{
				UserEnv: map[string]string{},
				HostUID: 1000,
				HostGID: 1000,
			},
			expected: []string{
				"HOST_UID=1000",
				"HOST_GID=1000",
			},
		},
		{
			name: "with user env",
			cfg: &EnvConfig{
				UserEnv: map[string]string{
					"ANTHROPIC_API_KEY": "test-key",
					"DEBUG":              "true",
				},
				HostUID: 1001,
				HostGID: 1001,
			},
			expected: []string{
				"ANTHROPIC_API_KEY=test-key",
				"DEBUG=true",
				"HOST_UID=1001",
				"HOST_GID=1001",
			},
		},
		{
			name: "empty user env",
			cfg: &EnvConfig{
				UserEnv: nil,
				HostUID: 0,
				HostGID: 0,
			},
			expected: []string{
				"HOST_UID=0",
				"HOST_GID=0",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := BuildContainerEnv(tt.cfg)

			// Check count
			if len(result) != len(tt.expected) {
				t.Errorf("Expected %d env vars, got %d", len(tt.expected), len(result))
			}

			// Convert to sets for comparison (order doesn't matter for user env)
			expectedSet := make(map[string]bool)
			for _, env := range tt.expected {
				expectedSet[env] = true
			}

			resultSet := make(map[string]bool)
			for _, env := range result {
				resultSet[env] = true
			}

			// Check that all expected env vars are present
			for env := range expectedSet {
				if !resultSet[env] {
					t.Errorf("Missing expected env var: %s", env)
				}
			}

			// Check that there are no extra env vars
			for env := range resultSet {
				if !expectedSet[env] {
					t.Errorf("Unexpected extra env var: %s", env)
				}
			}
		})
	}
}

func TestValidateRequiredArtifacts(t *testing.T) {
	// Create temporary directory for test artifacts
	tmpDir, err := os.MkdirTemp("", "holon-test-*")
	if err != nil {
		t.Fatalf("Failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

	tests := []struct {
		name             string
		setupArtifacts   func(string) error
		requiredArtifacts []v1.Artifact
		expectError      bool
		errorContains    string
	}{
		{
			name: "valid manifest only",
			setupArtifacts: func(outDir string) error {
				return os.WriteFile(filepath.Join(outDir, "manifest.json"), []byte(`{"status": "success"}`), 0644)
			},
			requiredArtifacts: nil,
			expectError:       false,
		},
		{
			name:             "missing manifest",
			setupArtifacts:   func(outDir string) error { return nil },
			requiredArtifacts: nil,
			expectError:       true,
			errorContains:     "missing required artifact: manifest.json",
		},
		{
			name: "valid manifest and required artifacts",
			setupArtifacts: func(outDir string) error {
				if err := os.WriteFile(filepath.Join(outDir, "manifest.json"), []byte(`{"status": "success"}`), 0644); err != nil {
					return err
				}
				if err := os.WriteFile(filepath.Join(outDir, "diff.patch"), []byte("diff content"), 0644); err != nil {
					return err
				}
				return os.WriteFile(filepath.Join(outDir, "summary.md"), []byte("# Summary"), 0644)
			},
			requiredArtifacts: []v1.Artifact{
				{Path: "diff.patch", Required: true},
				{Path: "summary.md", Required: true},
			},
			expectError: false,
		},
		{
			name: "missing required artifact",
			setupArtifacts: func(outDir string) error {
				return os.WriteFile(filepath.Join(outDir, "manifest.json"), []byte(`{"status": "success"}`), 0644)
			},
			requiredArtifacts: []v1.Artifact{
				{Path: "diff.patch", Required: true},
				{Path: "summary.md", Required: false},
			},
			expectError:    true,
			errorContains:  "missing required artifact: diff.patch",
		},
		{
			name: "optional artifact missing",
			setupArtifacts: func(outDir string) error {
				if err := os.WriteFile(filepath.Join(outDir, "manifest.json"), []byte(`{"status": "success"}`), 0644); err != nil {
					return err
				}
				return os.WriteFile(filepath.Join(outDir, "diff.patch"), []byte("diff content"), 0644)
			},
			requiredArtifacts: []v1.Artifact{
				{Path: "diff.patch", Required: true},
				{Path: "summary.md", Required: false},
			},
			expectError: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create a clean output directory for each test
			testOutDir := filepath.Join(tmpDir, t.Name())
			if err := os.MkdirAll(testOutDir, 0755); err != nil {
				t.Fatalf("Failed to create test output dir: %v", err)
			}

			// Setup artifacts
			if err := tt.setupArtifacts(testOutDir); err != nil {
				t.Fatalf("Failed to setup artifacts: %v", err)
			}

			// Run validation
			err := ValidateRequiredArtifacts(testOutDir, tt.requiredArtifacts)

			if tt.expectError {
				if err == nil {
					t.Errorf("Expected error but got none")
				} else if tt.errorContains != "" && err.Error() != tt.errorContains {
					t.Errorf("Expected error containing %q, got %q", tt.errorContains, err.Error())
				}
			} else {
				if err != nil {
					t.Errorf("Expected no error but got: %v", err)
				}
			}
		})
	}
}

func TestValidateMountTargets(t *testing.T) {
	// Create temporary directory and files for testing
	tmpDir, err := os.MkdirTemp("", "holon-test-*")
	if err != nil {
		t.Fatalf("Failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

	// Create test files
	specFile := filepath.Join(tmpDir, "spec.yaml")
	if err := os.WriteFile(specFile, []byte("test: spec"), 0644); err != nil {
		t.Fatalf("Failed to create test spec file: %v", err)
	}

	outDir := filepath.Join(tmpDir, "output")
	if err := os.MkdirAll(outDir, 0755); err != nil {
		t.Fatalf("Failed to create output dir: %v", err)
	}

	contextDir := filepath.Join(tmpDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		t.Fatalf("Failed to create context dir: %v", err)
	}

	promptFile := filepath.Join(tmpDir, "system.md")
	if err := os.WriteFile(promptFile, []byte("# System Prompt"), 0644); err != nil {
		t.Fatalf("Failed to create test prompt file: %v", err)
	}

	userPromptFile := filepath.Join(tmpDir, "user.md")
	if err := os.WriteFile(userPromptFile, []byte("# User Prompt"), 0644); err != nil {
		t.Fatalf("Failed to create test user prompt file: %v", err)
	}

	tests := []struct {
		name        string
		cfg         *MountConfig
		expectError bool
		errorContains string
	}{
		{
			name: "valid required mounts",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    specFile,
				OutDir:      outDir,
			},
			expectError: false,
		},
		{
			name: "all mounts valid",
			cfg: &MountConfig{
				SnapshotDir:    "/tmp/snapshot",
				SpecPath:       specFile,
				ContextPath:    contextDir,
				OutDir:         outDir,
				PromptPath:     promptFile,
				UserPromptPath: userPromptFile,
			},
			expectError: false,
		},
		{
			name: "missing snapshot dir",
			cfg: &MountConfig{
				SnapshotDir: "",
				SpecPath:    specFile,
				OutDir:      outDir,
			},
			expectError:    true,
			errorContains:  "snapshot directory cannot be empty",
		},
		{
			name: "missing spec path",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    "",
				OutDir:      outDir,
			},
			expectError:    true,
			errorContains:  "spec path cannot be empty",
		},
		{
			name: "missing output dir",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    specFile,
				OutDir:      "",
			},
			expectError:    true,
			errorContains:  "output directory cannot be empty",
		},
		{
			name: "non-existent spec path",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    "/non/existent/spec.yaml",
				OutDir:      outDir,
			},
			expectError:    true,
			errorContains:  "spec path does not exist",
		},
		{
			name: "non-existent output dir",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    specFile,
				OutDir:      "/non/existent/output",
			},
			expectError:    true,
			errorContains:  "output directory does not exist",
		},
		{
			name: "non-existent optional context path",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    specFile,
				ContextPath: "/non/existent/context",
				OutDir:      outDir,
			},
			expectError:    true,
			errorContains:  "context path does not exist",
		},
		{
			name: "non-existent optional prompt path",
			cfg: &MountConfig{
				SnapshotDir: "/tmp/snapshot",
				SpecPath:    specFile,
				OutDir:      outDir,
				PromptPath:  "/non/existent/prompt.md",
			},
			expectError:    true,
			errorContains:  "prompt path does not exist",
		},
		{
			name: "non-existent optional user prompt path",
			cfg: &MountConfig{
				SnapshotDir:    "/tmp/snapshot",
				SpecPath:       specFile,
				OutDir:         outDir,
				UserPromptPath: "/non/existent/user.md",
			},
			expectError:    true,
			errorContains:  "user prompt path does not exist",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := ValidateMountTargets(tt.cfg)

			if tt.expectError {
				if err == nil {
					t.Errorf("Expected error but got none")
				} else if tt.errorContains != "" {
					if !contains(err.Error(), tt.errorContains) {
						t.Errorf("Expected error containing %q, got %q", tt.errorContains, err.Error())
					}
				}
			} else {
				if err != nil {
					t.Errorf("Expected no error but got: %v", err)
				}
			}
		})
	}
}

// Helper function to check if string contains substring (case-sensitive)
func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr ||
		(len(s) > len(substr) &&
			(s[:len(substr)] == substr ||
			 s[len(s)-len(substr):] == substr ||
			 containsMiddle(s, substr))))
}

func containsMiddle(s, substr string) bool {
	for i := 1; i < len(s)-len(substr)+1; i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}