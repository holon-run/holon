package docker

import (
	"crypto/sha256"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"syscall"
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
			"/holon/workspace":             "/tmp/workspace-snapshot",
			"/holon/input/spec.yaml":       specFile,
			"/holon/output":                outDir,
			"/holon/input/context":         contextDir,
			"/holon/input/prompts/system.md":  promptFile,
			"/holon/input/prompts/user.md":    userPromptFile,
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
				"DEBUG":              "true",
				"CUSTOM_VAR":         "custom-value",
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
		adapterImage string
	}{
		{
			name:         "standard images",
			baseImage:    "golang:1.22",
			adapterImage: "holon-adapter-claude",
		},
		{
			name:         "same images should produce same tag",
			baseImage:    "golang:1.22",
			adapterImage: "holon-adapter-claude",
		},
		{
			name:         "different base image",
			baseImage:    "python:3.11",
			adapterImage: "holon-adapter-claude",
		},
		{
			name:         "different adapter image",
			baseImage:    "golang:1.22",
			adapterImage: "holon-adapter-custom",
		},
	}

	// Generate tags for each test case
	tags := make(map[string]string)
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			// Generate tag using the same logic as buildComposedImage
			hashInput := tc.baseImage + ":" + tc.adapterImage
			hash := sha256.Sum256([]byte(hashInput))
			tag := fmt.Sprintf("holon-composed-%x", hash[:12]) // Use first 12 bytes of hash

			t.Logf("Generated tag for %s + %s: %s", tc.baseImage, tc.adapterImage, tag)

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
			key := tc.baseImage + ":" + tc.adapterImage
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

// TestCopyDir tests the copyDir function behavior
func TestCopyDir(t *testing.T) {
	t.Run("copies regular files and preserves permissions", func(t *testing.T) {
		srcDir := t.TempDir()
		dstDir := t.TempDir()

		// Create test files with different permissions
		testFiles := map[string]os.FileMode{
			"file1.txt":      0644,
			"file2.sh":       0755,
			"file3.readonly": 0444,
			"subdir/file4":   0640,
		}

		for relPath, perm := range testFiles {
			fullPath := filepath.Join(srcDir, relPath)
			if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
				t.Fatalf("Failed to create directory for %s: %v", relPath, err)
			}
			content := fmt.Sprintf("Content of %s", relPath)
			if err := os.WriteFile(fullPath, []byte(content), perm); err != nil {
				t.Fatalf("Failed to create file %s: %v", relPath, err)
			}
		}

		// Test copyDir
		if err := copyDir(srcDir, dstDir); err != nil {
			t.Fatalf("copyDir failed: %v", err)
		}

		// Verify files were copied with correct permissions
		for relPath, expectedPerm := range testFiles {
			srcFile := filepath.Join(srcDir, relPath)
			dstFile := filepath.Join(dstDir, relPath)

			// Check file exists
			if _, err := os.Stat(dstFile); os.IsNotExist(err) {
				t.Errorf("Destination file does not exist: %s", relPath)
				continue
			}

			// Check permissions match (on Unix systems)
			if stat, err := os.Stat(srcFile); err == nil {
				srcMode := stat.Mode()
				if dstStat, err := os.Stat(dstFile); err == nil {
					dstMode := dstStat.Mode()
					// Compare permission bits (remove type bits)
					srcPerm := srcMode.Perm()
					dstPerm := dstMode.Perm()
					if srcPerm != dstPerm {
						t.Errorf("Permission mismatch for %s: expected %o, got %o", relPath, srcPerm, dstPerm)
					}
				}
			}

			// Verify content matches
			srcContent, err := os.ReadFile(srcFile)
			if err != nil {
				t.Errorf("Failed to read source file %s: %v", relPath, err)
				continue
			}
			dstContent, err := os.ReadFile(dstFile)
			if err != nil {
				t.Errorf("Failed to read destination file %s: %v", relPath, err)
				continue
			}
			if string(srcContent) != string(dstContent) {
				t.Errorf("Content mismatch for %s", relPath)
			}
		}
	})

	t.Run("copies symlinks as symlinks", func(t *testing.T) {
		srcDir := t.TempDir()
		dstDir := t.TempDir()

		// Create a target file
		targetFile := filepath.Join(srcDir, "target.txt")
		if err := os.WriteFile(targetFile, []byte("target content"), 0644); err != nil {
			t.Fatalf("Failed to create target file: %v", err)
		}

		// Create a symlink to the target
		symlinkPath := filepath.Join(srcDir, "link.txt")
		if err := os.Symlink("target.txt", symlinkPath); err != nil {
			t.Fatalf("Failed to create symlink: %v", err)
		}

		// Create an absolute symlink
		absSymlinkPath := filepath.Join(srcDir, "abslink.txt")
		if err := os.Symlink(targetFile, absSymlinkPath); err != nil {
			t.Fatalf("Failed to create absolute symlink: %v", err)
		}

		// Test copyDir
		if err := copyDir(srcDir, dstDir); err != nil {
			t.Fatalf("copyDir failed: %v", err)
		}

		// Check symlinks are preserved as symlinks
		dstSymlink := filepath.Join(dstDir, "link.txt")
		if info, err := os.Lstat(dstSymlink); err != nil {
			t.Errorf("Failed to stat destination symlink: %v", err)
		} else if info.Mode()&os.ModeSymlink == 0 {
			t.Errorf("Destination is not a symlink: %s", dstSymlink)
		} else {
			// Read link target
			if target, err := os.Readlink(dstSymlink); err != nil {
				t.Errorf("Failed to read symlink target: %v", err)
			} else if target != "target.txt" {
				t.Errorf("Symlink target mismatch: expected 'target.txt', got '%s'", target)
			}
		}

		dstAbsSymlink := filepath.Join(dstDir, "abslink.txt")
		if info, err := os.Lstat(dstAbsSymlink); err != nil {
			t.Errorf("Failed to stat destination absolute symlink: %v", err)
		} else if info.Mode()&os.ModeSymlink == 0 {
			t.Errorf("Destination is not a symlink: %s", dstAbsSymlink)
		}
	})

	t.Run("handles symlinks to directories", func(t *testing.T) {
		srcDir := t.TempDir()
		dstDir := t.TempDir()

		// Create a subdirectory
		subDir := filepath.Join(srcDir, "subdir")
		if err := os.MkdirAll(subDir, 0755); err != nil {
			t.Fatalf("Failed to create subdirectory: %v", err)
		}
		if err := os.WriteFile(filepath.Join(subDir, "file.txt"), []byte("content"), 0644); err != nil {
			t.Fatalf("Failed to create file in subdirectory: %v", err)
		}

		// Create a symlink to the directory
		dirSymlinkPath := filepath.Join(srcDir, "dirlink")
		if err := os.Symlink("subdir", dirSymlinkPath); err != nil {
			t.Fatalf("Failed to create directory symlink: %v", err)
		}

		// Test copyDir
		if err := copyDir(srcDir, dstDir); err != nil {
			t.Fatalf("copyDir failed: %v", err)
		}

		// Check directory symlink is preserved
		dstDirSymlink := filepath.Join(dstDir, "dirlink")
		if info, err := os.Lstat(dstDirSymlink); err != nil {
			t.Errorf("Failed to stat destination directory symlink: %v", err)
		} else if info.Mode()&os.ModeSymlink == 0 {
			t.Errorf("Destination is not a symlink: %s", dstDirSymlink)
		}
	})

	t.Run("handles broken symlinks", func(t *testing.T) {
		srcDir := t.TempDir()
		dstDir := t.TempDir()

		// Create a broken symlink (target doesn't exist)
		brokenSymlinkPath := filepath.Join(srcDir, "brokenlink")
		if err := os.Symlink("nonexistent.txt", brokenSymlinkPath); err != nil {
			t.Fatalf("Failed to create broken symlink: %v", err)
		}

		// Test copyDir
		if err := copyDir(srcDir, dstDir); err != nil {
			t.Fatalf("copyDir failed: %v", err)
		}

		// Check broken symlink is preserved
		dstBrokenSymlink := filepath.Join(dstDir, "brokenlink")
		if info, err := os.Lstat(dstBrokenSymlink); err != nil {
			t.Errorf("Failed to stat destination broken symlink: %v", err)
		} else if info.Mode()&os.ModeSymlink == 0 {
			t.Errorf("Destination is not a symlink: %s", dstBrokenSymlink)
		} else {
			// Verify it's still broken
			if _, err := os.Stat(dstBrokenSymlink); err == nil {
				t.Errorf("Broken symlink should still be broken after copy")
			}
		}
	})

	t.Run("detects destination inside source", func(t *testing.T) {
		// Note: This test documents current behavior, but the current implementation
		// doesn't actually check for dst-in-src conditions. This test will help
		// verify when this feature is implemented.
		srcDir := t.TempDir()
		dstDir := filepath.Join(srcDir, "nested", "destination")

		// Create a simple file in source
		testFile := filepath.Join(srcDir, "test.txt")
		if err := os.WriteFile(testFile, []byte("content"), 0644); err != nil {
			t.Fatalf("Failed to create test file: %v", err)
		}

		// Current implementation doesn't check for dst-in-src, so it will fail
		// when trying to copy into a non-existent directory or create circular copies
		err := copyDir(srcDir, dstDir)
		// The current behavior might vary depending on the system's cp command
		// This test documents that we should handle this case properly
		if err == nil {
			t.Logf("Current copyDir allows dst-in-src (destination was: %s)", dstDir)
		} else {
			t.Logf("Current copyDir fails with dst-in-src: %v", err)
		}
	})

	t.Run("handles empty directory", func(t *testing.T) {
		srcDir := t.TempDir()
		dstDir := t.TempDir()

		// Test copying empty directory
		if err := copyDir(srcDir, dstDir); err != nil {
			t.Fatalf("copyDir failed with empty source: %v", err)
		}

		// Destination should exist and be empty
		entries, err := os.ReadDir(dstDir)
		if err != nil {
			t.Fatalf("Failed to read destination directory: %v", err)
		}
		if len(entries) != 0 {
			t.Errorf("Expected empty destination directory, got %d entries", len(entries))
		}
	})

	t.Run("handles source with hidden files", func(t *testing.T) {
		srcDir := t.TempDir()
		dstDir := t.TempDir()

		// Create hidden files and directories
		if err := os.WriteFile(filepath.Join(srcDir, ".hidden"), []byte("hidden content"), 0644); err != nil {
			t.Fatalf("Failed to create hidden file: %v", err)
		}

		hiddenDir := filepath.Join(srcDir, ".hiddendir")
		if err := os.MkdirAll(hiddenDir, 0755); err != nil {
			t.Fatalf("Failed to create hidden directory: %v", err)
		}
		if err := os.WriteFile(filepath.Join(hiddenDir, "file"), []byte("content"), 0644); err != nil {
			t.Fatalf("Failed to create file in hidden directory: %v", err)
		}

		// Test copyDir
		if err := copyDir(srcDir, dstDir); err != nil {
			t.Fatalf("copyDir failed: %v", err)
		}

		// Check hidden files were copied
		if _, err := os.Stat(filepath.Join(dstDir, ".hidden")); os.IsNotExist(err) {
			t.Error("Hidden file was not copied")
		}

		if _, err := os.Stat(filepath.Join(dstDir, ".hiddendir", "file")); os.IsNotExist(err) {
			t.Error("File in hidden directory was not copied")
		}
	})
}

// TestCopyDirFilesystemTypes tests how copyDir handles different filesystem types
func TestCopyDirFilesystemTypes(t *testing.T) {
	// This test requires root privileges to create some special file types
	// On systems where we can't create them, we'll skip those parts
	t.Run("skips non-regular files safely", func(t *testing.T) {
		if os.Getuid() != 0 {
			t.Skip("Skipping non-regular file test: requires root privileges")
		}

		srcDir := t.TempDir()
		dstDir := t.TempDir()

		// Create regular files to ensure they're copied
		regularFile := filepath.Join(srcDir, "regular.txt")
		if err := os.WriteFile(regularFile, []byte("regular content"), 0644); err != nil {
			t.Fatalf("Failed to create regular file: %v", err)
		}

		// Try to create special files (this will test safety)
		// Note: These operations require root privileges
		fifoPath := filepath.Join(srcDir, "fifo")
		if err := syscall.Mkfifo(fifoPath, 0666); err != nil {
			t.Logf("Could not create FIFO (expected without root): %v", err)
		}

		// Test copyDir
		if err := copyDir(srcDir, dstDir); err != nil {
			t.Fatalf("copyDir failed: %v", err)
		}

		// Verify regular file was copied
		if _, err := os.Stat(filepath.Join(dstDir, "regular.txt")); os.IsNotExist(err) {
			t.Error("Regular file was not copied")
		}

		// Check what happened to special file (behavior may vary)
		if info, err := os.Stat(filepath.Join(dstDir, "fifo")); err == nil {
			if info.Mode()&fs.ModeNamedPipe != 0 {
				t.Logf("FIFO was copied as named pipe")
			} else {
				t.Logf("FIFO was copied as regular file (mode: %v)", info.Mode())
			}
		} else {
			t.Logf("FIFO was not copied: %v", err)
		}
	})
}

// TestCopyDirErrorConditions tests various error conditions
func TestCopyDirErrorConditions(t *testing.T) {
	t.Run("non-existent source", func(t *testing.T) {
		srcDir := filepath.Join(t.TempDir(), "nonexistent")
		dstDir := t.TempDir()

		err := copyDir(srcDir, dstDir)
		if err == nil {
			t.Error("Expected error for non-existent source directory")
		}
		t.Logf("Expected error for non-existent source: %v", err)
	})

	t.Run("non-existent source (absolute path)", func(t *testing.T) {
		srcDir := "/definitely/does/not/exist/path"
		dstDir := t.TempDir()

		err := copyDir(srcDir, dstDir)
		if err == nil {
			t.Error("Expected error for non-existent absolute source path")
		}
		t.Logf("Expected error for non-existent absolute source: %v", err)
	})

	t.Run("destination in non-existent parent", func(t *testing.T) {
		srcDir := t.TempDir()
		dstDir := filepath.Join("/nonexistent", "parent", "dest")

		// Create a test file in source
		testFile := filepath.Join(srcDir, "test.txt")
		if err := os.WriteFile(testFile, []byte("content"), 0644); err != nil {
			t.Fatalf("Failed to create test file: %v", err)
		}

		err := copyDir(srcDir, dstDir)
		if err != nil {
			t.Logf("Expected error for destination in non-existent parent: %v", err)
		}
		// Some cp implementations might create parent directories, so don't require error
	})

	t.Run("source is a file not directory", func(t *testing.T) {
		srcFile := filepath.Join(t.TempDir(), "source.txt")
		if err := os.WriteFile(srcFile, []byte("content"), 0644); err != nil {
			t.Fatalf("Failed to create source file: %v", err)
		}

		dstDir := t.TempDir()

		err := copyDir(srcFile, dstDir)
		// Behavior may vary depending on cp implementation
		if err != nil {
			t.Logf("copyDir with file source failed (expected): %v", err)
		} else {
			t.Logf("copyDir with file source succeeded (cp may handle this case)")
		}
	})
}
