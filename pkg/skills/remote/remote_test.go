package remote

import (
	"archive/zip"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestParseSkillRef tests parsing skill references
func TestParseSkillRef(t *testing.T) {
	tests := []struct {
		name        string
		ref         string
		wantURL     string
		wantChecksum string
		wantErr     bool
	}{
		{
			name:    "simple URL",
			ref:     "https://example.com/skill.zip",
			wantURL: "https://example.com/skill.zip",
			wantErr: false,
		},
		{
			name:        "URL with sha256",
			ref:         "https://example.com/skill.zip#sha256=" + strings.Repeat("a", 64),
			wantURL:     "https://example.com/skill.zip",
			wantChecksum: strings.Repeat("a", 64),
			wantErr:     false,
		},
		{
			name:    "HTTP URL",
			ref:     "http://example.com/skills.zip",
			wantURL: "http://example.com/skills.zip",
			wantErr: false,
		},
		{
			name:    "URL with query params",
			ref:     "https://example.com/download?version=1.0",
			wantURL: "https://example.com/download?version=1.0",
			wantErr: false,
		},
		{
			name:        "GitHub archive URL with checksum",
			ref:         "https://github.com/org/repo/archive/refs/tags/v1.2.3.zip#sha256=" + strings.Repeat("a", 64),
			wantURL:     "https://github.com/org/repo/archive/refs/tags/v1.2.3.zip",
			wantChecksum: strings.Repeat("a", 64),
			wantErr:     false,
		},
		{
			name:    "not a URL",
			ref:     "/path/to/skill",
			wantErr: true,
		},
		{
			name:        "invalid checksum length",
			ref:         "https://example.com/skill.zip#sha256=abc",
			wantErr:     true,
		},
		{
			name:        "invalid checksum hex",
			ref:         "https://example.com/skill.zip#sha256=" + strings.Repeat("x", 64),
			wantErr:     true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			skillRef, err := ParseSkillRef(tt.ref)

			if tt.wantErr {
				if err == nil {
					t.Error("expected error, got nil")
				}
				return
			}

			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}

			if skillRef.URL != tt.wantURL {
				t.Errorf("URL mismatch: got %s, want %s", skillRef.URL, tt.wantURL)
			}

			if skillRef.Checksum != tt.wantChecksum {
				t.Errorf("checksum mismatch: got %s, want %s", skillRef.Checksum, tt.wantChecksum)
			}
		})
	}
}

// TestIsURL tests URL detection
func TestIsURL(t *testing.T) {
	tests := []struct {
		name string
		ref  string
		want bool
	}{
		{
			name: "HTTPS URL",
			ref:  "https://example.com/skill.zip",
			want: true,
		},
		{
			name: "HTTP URL",
			ref:  "http://example.com/skill.zip",
			want: true,
		},
		{
			name: "local path",
			ref:  "/path/to/skill",
			want: false,
		},
		{
			name: "relative path",
			ref:  "./skills/test",
			want: false,
		},
		{
			name: "empty string",
			ref:  "",
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := IsURL(tt.ref); got != tt.want {
				t.Errorf("IsURL() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestDownloadAndExtract tests the full download and extract workflow
func TestDownloadAndExtract(t *testing.T) {
	// Create a test zip file with skills
	zipData, err := createTestZip()
	if err != nil {
		t.Fatalf("failed to create test zip: %v", err)
	}

	// Create a test server
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/zip")
		w.WriteHeader(http.StatusOK)
		w.Write(zipData)
	}))
	defer server.Close()

	// Create a temporary cache directory
	cacheDir, err := os.MkdirTemp("", "holon-skills-cache-*")
	if err != nil {
		t.Fatalf("failed to create temp cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	cache := NewCache(cacheDir)

	// Test 1: Download without checksum
	t.Run("download without checksum", func(t *testing.T) {
		skillRef := &SkillRef{
			URL: server.URL + "/skills.zip",
		}

		skills, err := cache.DownloadAndExtract(skillRef)
		if err != nil {
			t.Fatalf("DownloadAndExtract failed: %v", err)
		}

		// Should find both skills
		if len(skills) != 2 {
			t.Errorf("expected 2 skills, got %d", len(skills))
		}

		// Verify skills exist
		for _, skillPath := range skills {
			if _, err := os.Stat(skillPath); os.IsNotExist(err) {
				t.Errorf("skill path does not exist: %s", skillPath)
			}

			// Check for SKILL.md
			skillManifestPath := filepath.Join(skillPath, "SKILL.md")
			if _, err := os.Stat(skillManifestPath); os.IsNotExist(err) {
				t.Errorf("SKILL.md not found in %s", skillPath)
			}
		}
	})

	// Test 2: Download with checksum
	t.Run("download with checksum", func(t *testing.T) {
		// Calculate checksum of zip data
		checksum := calculateChecksum(zipData)

		skillRef := &SkillRef{
			URL:      server.URL + "/skills-with-checksum.zip",
			Checksum: checksum,
		}

		skills, err := cache.DownloadAndExtract(skillRef)
		if err != nil {
			t.Fatalf("DownloadAndExtract failed: %v", err)
		}

		if len(skills) != 2 {
			t.Errorf("expected 2 skills, got %d", len(skills))
		}
	})

	// Test 3: Cache hit (second download should use cache)
	t.Run("cache hit", func(t *testing.T) {
		skillRef := &SkillRef{
			URL: server.URL + "/skills.zip",
		}

		// First download
		_, err := cache.DownloadAndExtract(skillRef)
		if err != nil {
			t.Fatalf("first download failed: %v", err)
		}

		// Second download should hit cache
		skills, err := cache.DownloadAndExtract(skillRef)
		if err != nil {
			t.Fatalf("second download failed: %v", err)
		}

		if len(skills) != 2 {
			t.Errorf("expected 2 skills from cache, got %d", len(skills))
		}
	})

	// Test 4: Invalid checksum
	t.Run("invalid checksum", func(t *testing.T) {
		skillRef := &SkillRef{
			URL:      server.URL + "/skills.zip",
			Checksum: strings.Repeat("0", 64), // Wrong checksum
		}

		_, err := cache.DownloadAndExtract(skillRef)
		if err == nil {
			t.Error("expected checksum error, got nil")
		}
		if !strings.Contains(err.Error(), "checksum") {
			t.Errorf("error should mention checksum: %v", err)
		}
	})
}

// TestDiscoverSkills tests skill discovery in a directory tree
func TestDiscoverSkills(t *testing.T) {
	// Create a temporary directory structure
	tempDir, err := os.MkdirTemp("", "holon-discover-*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tempDir)

	// Create skill directories
	skill1Dir := filepath.Join(tempDir, "skill1")
	if err := os.MkdirAll(skill1Dir, 0755); err != nil {
		t.Fatalf("failed to create skill1: %v", err)
	}
	if err := os.WriteFile(filepath.Join(skill1Dir, "SKILL.md"), []byte("# Skill 1"), 0644); err != nil {
		t.Fatalf("failed to create skill1 manifest: %v", err)
	}

	// Nested skill directory
	nestedDir := filepath.Join(tempDir, "subdir", "skill2")
	if err := os.MkdirAll(nestedDir, 0755); err != nil {
		t.Fatalf("failed to create nested skill2: %v", err)
	}
	if err := os.WriteFile(filepath.Join(nestedDir, "SKILL.md"), []byte("# Skill 2"), 0644); err != nil {
		t.Fatalf("failed to create skill2 manifest: %v", err)
	}

	// Directory without SKILL.md (should be ignored)
	noSkillDir := filepath.Join(tempDir, "no-skill")
	if err := os.MkdirAll(noSkillDir, 0755); err != nil {
		t.Fatalf("failed to create no-skill dir: %v", err)
	}

	// Discover skills
	skills, err := DiscoverSkills(tempDir)
	if err != nil {
		t.Fatalf("DiscoverSkills failed: %v", err)
	}

	// Should find 2 skills
	if len(skills) != 2 {
		t.Errorf("expected 2 skills, got %d", len(skills))
	}

	// Check that both skills are found
	skillMap := make(map[string]bool)
	for _, skill := range skills {
		skillMap[filepath.Base(skill)] = true
	}

	if !skillMap["skill1"] {
		t.Error("skill1 not found")
	}
	if !skillMap["skill2"] {
		t.Error("skill2 not found")
	}
}

// TestZipSlipProtection tests zip-slip vulnerability protection
func TestZipSlipProtection(t *testing.T) {
	// Create a malicious zip with path traversal
	zipData, err := createMaliciousZip()
	if err != nil {
		t.Fatalf("failed to create malicious zip: %v", err)
	}

	cacheDir, err := os.MkdirTemp("", "holon-zipslip-test-*")
	if err != nil {
		t.Fatalf("failed to create temp cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	cache := NewCache(cacheDir)

	// Attempting to extract should fail
	_, err = cache.extract(zipData)
	if err == nil {
		t.Error("expected error for zip-slip attack, got nil")
	}
	if !strings.Contains(err.Error(), "zip-slip") {
		t.Errorf("error should mention zip-slip: %v", err)
	}
}

// Helper functions

func createTestZip() ([]byte, error) {
	buf := new(bytes.Buffer)
	zipWriter := zip.NewWriter(buf)

	// Create skill1
	skill1Files := map[string]string{
		"skill1/SKILL.md": "# Skill 1\n",
	}

	for path, content := range skill1Files {
		w, err := zipWriter.Create(path)
		if err != nil {
			return nil, err
		}
		if _, err := w.Write([]byte(content)); err != nil {
			return nil, err
		}
	}

	// Create skill2
	skill2Files := map[string]string{
		"skill2/SKILL.md": "# Skill 2\n",
		"skill2/README.md": "Skill 2 documentation\n",
	}

	for path, content := range skill2Files {
		w, err := zipWriter.Create(path)
		if err != nil {
			return nil, err
		}
		if _, err := w.Write([]byte(content)); err != nil {
			return nil, err
		}
	}

	if err := zipWriter.Close(); err != nil {
		return nil, err
	}

	return buf.Bytes(), nil
}

func createMaliciousZip() ([]byte, error) {
	buf := new(bytes.Buffer)
	zipWriter := zip.NewWriter(buf)

	// Try to write outside the extraction directory
	w, err := zipWriter.Create("../../../tmp/malicious.txt")
	if err != nil {
		return nil, err
	}
	if _, err := w.Write([]byte("malicious content")); err != nil {
		return nil, err
	}

	if err := zipWriter.Close(); err != nil {
		return nil, err
	}

	return buf.Bytes(), nil
}

func calculateChecksum(data []byte) string {
	hash := sha256.Sum256(data)
	return hex.EncodeToString(hash[:])
}
