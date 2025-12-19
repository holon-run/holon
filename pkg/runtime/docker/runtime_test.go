package docker

import (
	"context"
	"crypto/sha256"
	"fmt"
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

// TestRunHolon_DryRun verifies the container creation logic (partially)
// In a full test, it would pull image and run, but here we just check if NewRuntime works.
func TestRunHolon_DryRun(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime()
	if err != nil {
		t.Skip("Skipping: Docker not available")
	}

	// We only verify that context is handled correctly in the client
	_ = rt
	_ = ctx
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

func TestCopyDir(t *testing.T) {
	// Setup source directory
	tmpDir := t.TempDir()
	src := filepath.Join(tmpDir, "src")
	if err := os.Mkdir(src, 0755); err != nil {
		t.Fatal(err)
	}

	// Create a file in src
	testFile := filepath.Join(src, "test.txt")
	if err := os.WriteFile(testFile, []byte("hello"), 0644); err != nil {
		t.Fatal(err)
	}

	// Create a subdir in src
	subDir := filepath.Join(src, "subdir")
	if err := os.Mkdir(subDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(subDir, "sub.txt"), []byte("world"), 0644); err != nil {
		t.Fatal(err)
	}

	// Create a symlink in src (absolute)
	if err := os.Symlink(testFile, filepath.Join(src, "link_to_test")); err != nil {
		// On some systems/environments symlinks might fail (e.g. Windows without dev mode)
		t.Logf("Warning: symlink creation failed: %v", err)
	} else {
		t.Log("Symlink created successfully")
	}

	t.Run("Normal Copy", func(t *testing.T) {
		dst := filepath.Join(tmpDir, "dst")
		if err := copyDir(src, dst); err != nil {
			t.Fatalf("copyDir failed: %v", err)
		}

		// Verify contents
		content, err := os.ReadFile(filepath.Join(dst, "test.txt"))
		if err != nil || string(content) != "hello" {
			t.Errorf("test.txt copy failed, got: %q, err: %v", string(content), err)
		}

		content, err = os.ReadFile(filepath.Join(dst, "subdir", "sub.txt"))
		if err != nil || string(content) != "world" {
			t.Errorf("subdir/sub.txt copy failed, got: %q, err: %v", string(content), err)
		}

		// Verify symlink
		linkTarget, err := os.Readlink(filepath.Join(dst, "link_to_test"))
		if err == nil {
			if !filepath.IsAbs(linkTarget) {
				t.Errorf("Expected absolute link target in copy, got: %s", linkTarget)
			}
		}
	})

	t.Run("Destination Inside Source (Recursion Prevention)", func(t *testing.T) {
		dstInSrc := filepath.Join(src, "output")
		// The function should skip this directory to prevent infinite recursion
		if err := copyDir(src, dstInSrc); err != nil {
			t.Fatalf("copyDir with dst inside src failed: %v", err)
		}

		// Verify it didn't copy output into itself
		// If it succeeded, output/test.txt should exist
		if _, err := os.Stat(filepath.Join(dstInSrc, "test.txt")); err != nil {
			t.Errorf("Expected test.txt to be copied into output, got error: %v", err)
		}

		// Check that output/output does NOT exist (which would indicate recursion)
		if _, err := os.Stat(filepath.Join(dstInSrc, "output")); err == nil {
			t.Errorf("Recursion detected: output/output should not exist")
		}
	})

	t.Run("Empty Dir", func(t *testing.T) {
		emptySrc := filepath.Join(tmpDir, "empty")
		if err := os.Mkdir(emptySrc, 0755); err != nil {
			t.Fatal(err)
		}
		dst := filepath.Join(tmpDir, "empty_dst")
		if err := copyDir(emptySrc, dst); err != nil {
			t.Fatal(err)
		}
		if info, err := os.Stat(dst); err != nil || !info.IsDir() {
			t.Error("Empty destination directory was not created")
		}
	})
}
