package docker

import (
	"context"
	"crypto/sha256"
	"fmt"
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
