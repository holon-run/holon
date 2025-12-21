package docker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestNewRuntime(t *testing.T) {
	rt, err := NewRuntime()
	if err != nil {
		t.Skipf("Skipping integration test: Docker daemon not reachable or client error: %v", err)
	}
	if rt.cli == nil {
		t.Error("Expected non-nil docker client")
	}
}

// TestRunHolon_ConfigAssembly tests the pure configuration assembly logic
// without requiring Docker daemon by using the extracted functions
func TestRunHolon_ConfigAssembly(t *testing.T) {
	// Create temporary test files
	tmpDir, err := os.MkdirTemp("", "holon-config-test-*")
	if err != nil {
		t.Fatalf("Failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

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

	// Test mount configuration assembly
	t.Run("mount assembly", func(t *testing.T) {
		cfg := &MountConfig{
			SnapshotDir:    "/tmp/workspace-snapshot",
			SpecPath:       specFile,
			ContextPath:    contextDir,
			OutDir:         outDir,
			PromptPath:     promptFile,
			UserPromptPath: userPromptFile,
		}

		mounts := BuildContainerMounts(cfg)

		// Verify we get expected number of mounts
		expectedMountCount := 6 // workspace, spec, output, context, system.md, user.md
		if len(mounts) != expectedMountCount {
			t.Errorf("Expected %d mounts, got %d", expectedMountCount, len(mounts))
		}

		// Verify mount targets are correct
		targets := make(map[string]string)
		for _, m := range mounts {
			targets[m.Target] = m.Source
		}

		expectedTargets := map[string]string{
			"/holon/workspace":               "/tmp/workspace-snapshot",
			"/holon/input/spec.yaml":         specFile,
			"/holon/output":                  outDir,
			"/holon/input/context":           contextDir,
			"/holon/input/prompts/system.md": promptFile,
			"/holon/input/prompts/user.md":   userPromptFile,
		}

		for target, expectedSource := range expectedTargets {
			actualSource, exists := targets[target]
			if !exists {
				t.Errorf("Missing mount for target: %s", target)
			} else if actualSource != expectedSource {
				t.Errorf("Mount target %s: expected source %s, got %s", target, expectedSource, actualSource)
			}
		}
	})

	// Test environment variable assembly
	t.Run("env assembly", func(t *testing.T) {
		cfg := &EnvConfig{
			UserEnv: map[string]string{
				"ANTHROPIC_API_KEY": "test-key-123",
				"DEBUG":             "true",
				"CUSTOM_VAR":        "custom-value",
			},
			HostUID: 1000,
			HostGID: 1000,
		}

		env := BuildContainerEnv(cfg)

		// Verify we get expected number of env vars
		expectedEnvCount := 5 // 3 user vars + HOST_UID + HOST_GID
		if len(env) != expectedEnvCount {
			t.Errorf("Expected %d env vars, got %d", expectedEnvCount, len(env))
		}

		// Verify specific env vars
		envSet := make(map[string]bool)
		for _, e := range env {
			envSet[e] = true
		}

		expectedEnv := []string{
			"ANTHROPIC_API_KEY=test-key-123",
			"DEBUG=true",
			"CUSTOM_VAR=custom-value",
			"HOST_UID=1000",
			"HOST_GID=1000",
		}

		for _, expectedVar := range expectedEnv {
			if !envSet[expectedVar] {
				t.Errorf("Missing expected env var: %s", expectedVar)
			}
		}
	})

	// Test mount target validation
	t.Run("mount target validation", func(t *testing.T) {
		cfg := &MountConfig{
			SnapshotDir:    "/tmp/snapshot-test",
			SpecPath:       specFile,
			ContextPath:    contextDir,
			OutDir:         outDir,
			PromptPath:     promptFile,
			UserPromptPath: userPromptFile,
		}

		// Should pass validation
		if err := ValidateMountTargets(cfg); err != nil {
			t.Errorf("Expected no validation error, got: %v", err)
		}

		// Test invalid configuration
		invalidCfg := &MountConfig{
			SnapshotDir: "", // Empty snapshot dir should fail
			SpecPath:    specFile,
			OutDir:      outDir,
		}

		if err := ValidateMountTargets(invalidCfg); err == nil {
			t.Error("Expected validation error for empty snapshot dir")
		}
	})
}

// TestComposedImageTagGeneration verifies that the tag generation is stable and valid
func TestComposedImageTagGeneration(t *testing.T) {
	// Test data
	testCases := []struct {
		name         string
		baseImage    string
		bundleDigest string
	}{
		{
			name:         "standard bundle",
			baseImage:    "golang:1.22",
			bundleDigest: "bundle-a",
		},
		{
			name:         "same bundle should produce same tag",
			baseImage:    "golang:1.22",
			bundleDigest: "bundle-a",
		},
		{
			name:         "different base image",
			baseImage:    "python:3.11",
			bundleDigest: "bundle-a",
		},
		{
			name:         "different bundle digest",
			baseImage:    "golang:1.22",
			bundleDigest: "bundle-b",
		},
	}

	// Generate tags for each test case
	tags := make(map[string]string)
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			// Generate tag using the same logic as buildComposedImage
			tag := composeImageTag(tc.baseImage, tc.bundleDigest)

			t.Logf("Generated tag for %s + %s: %s", tc.baseImage, tc.bundleDigest, tag)

			// Verify tag format
			if !strings.HasPrefix(tag, "holon-composed-") {
				t.Errorf("Tag should start with 'holon-composed-', got: %s", tag)
			}

			// Verify tag contains valid hex characters only after prefix
			hashPart := strings.TrimPrefix(tag, "holon-composed-")
			if len(hashPart) != 24 { // 12 bytes = 24 hex characters
				t.Errorf("Hash part should be 24 characters, got: %d", len(hashPart))
			}

			// Store for consistency check
			key := tc.baseImage + ":" + tc.bundleDigest
			if existingTag, exists := tags[key]; exists {
				if existingTag != tag {
					t.Errorf("Inconsistent tag generation: same inputs produced different tags: %s vs %s", existingTag, tag)
				}
			} else {
				tags[key] = tag
			}

			// Verify tag doesn't contain invalid characters (only check the hash part)
			hashPart = strings.TrimPrefix(tag, "holon-composed-")
			for _, r := range hashPart {
				if !((r >= 'a' && r <= 'f') || (r >= '0' && r <= '9')) {
					t.Errorf("Tag hash part contains invalid character '%c': %s", r, tag)
				}
			}
		})
	}

	// Verify that different inputs produce different tags
	uniqueTags := make(map[string]bool)
	for _, tag := range tags {
		uniqueTags[tag] = true
	}

	if len(uniqueTags) != len(tags) {
		t.Errorf("Different inputs should produce different tags. Got %d unique tags for %d input combinations", len(uniqueTags), len(tags))
	}
}

func TestMkdirTempOutsideWorkspace_DoesNotNest(t *testing.T) {
	workspace := t.TempDir()
	tmpInside := filepath.Join(workspace, "tmp")
	if err := os.MkdirAll(tmpInside, 0o755); err != nil {
		t.Fatalf("mkdir tmp: %v", err)
	}
	t.Setenv("TMPDIR", tmpInside)

	dir, err := mkdirTempOutsideWorkspace(workspace, "holon-test-*")
	if err != nil {
		t.Fatalf("mkdirTempOutsideWorkspace: %v", err)
	}
	defer os.RemoveAll(dir)

	absWorkspace, err := cleanAbs(workspace)
	if err != nil {
		t.Fatalf("cleanAbs workspace: %v", err)
	}
	absDir, err := cleanAbs(dir)
	if err != nil {
		t.Fatalf("cleanAbs dir: %v", err)
	}
	if isSubpath(absDir, absWorkspace) {
		t.Fatalf("expected snapshot dir to be outside workspace:\nworkspace=%s\ndir=%s", absWorkspace, absDir)
	}
}
