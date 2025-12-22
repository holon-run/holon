package cache

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestNewCache(t *testing.T) {
	// Test with custom cache directory
	customDir := t.TempDir()
	cache1 := New(customDir)
	if cache1 == nil {
		t.Fatal("New() returned nil")
	}

	// Test with empty string (should use default)
	cache2 := New("")
	if cache2 == nil {
		t.Fatal("New() returned nil")
	}
}

func TestCacheKey(t *testing.T) {
	cache := &Cache{}

	tests := []struct {
		url      string
		checksum string
		expected string
	}{
		{
			url:      "https://example.com/bundle.tar.gz",
			checksum: "",
			expected: "https___example_com_bundle.tar.gz",
		},
		{
			url:      "https://example.com/bundle.tar.gz",
			checksum: "abcd1234ef567890",
			expected: "https___example_com_bundle.tar.gz_abcd1234",
		},
		{
			url:      "https://example.com/path/bundle.tar.gz?version=1.0",
			checksum: "1234567890abcdef",
			expected: "https___example_com_path_bundle.tar.gz_version_1.0_12345678",
		},
		{
			url:      "https://github.com/user/repo/releases/download/v1.0.0/agent-bundle-v1.0.0-linux-amd64.tar.gz",
			checksum: "deadbeefdeadbeef",
			expected: "https___github_com_user_repo_releases_download_v1.0.0_agent-bundle-v1.0.0-linux-amd64.tar.gz_deadbeef",
		},
	}

	for _, tt := range tests {
		t.Run(tt.url, func(t *testing.T) {
			key := cache.cacheKey(tt.url, tt.checksum)
			if key != tt.expected {
				t.Errorf("cacheKey() = %q, want %q", key, tt.expected)
			}
		})
	}
}

func TestValidateAliasName(t *testing.T) {
	cache := &Cache{}

	tests := []struct {
		name      string
		alias     string
		shouldErr bool
	}{
		{
			name:      "valid simple alias",
			alias:     "myagent",
			shouldErr: false,
		},
		{
			name:      "valid alias with hyphens",
			alias:     "my-agent",
			shouldErr: false,
		},
		{
			name:      "valid alias with numbers",
			alias:     "agent123",
			shouldErr: false,
		},
		{
			name:      "empty alias",
			alias:     "",
			shouldErr: true,
		},
		{
			name:      "alias with slash",
			alias:     "my/agent",
			shouldErr: true,
		},
		{
			name:      "alias with backslash",
			alias:     "my\\agent",
			shouldErr: true,
		},
		{
			name:      "alias with dot",
			alias:     "my.agent",
			shouldErr: false,
		},
		{
			name:      "very long alias",
			alias:     strings.Repeat("a", 101),
			shouldErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := cache.validateAliasName(tt.alias)
			if (err != nil) != tt.shouldErr {
				t.Errorf("validateAliasName() error = %v, shouldErr %v", err, tt.shouldErr)
			}
		})
	}
}

func TestSetGetRemoveAlias(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Test setting and getting an alias
	aliasName := "testagent"
	url := "https://example.com/bundle.tar.gz"

	// Should not exist initially
	_, err := cache.GetAlias(aliasName)
	if err == nil {
		t.Error("Expected error when getting non-existent alias")
	}

	// Set alias
	if err := cache.SetAlias(aliasName, url); err != nil {
		t.Fatalf("Failed to set alias: %v", err)
	}

	// Get alias
	retrievedURL, err := cache.GetAlias(aliasName)
	if err != nil {
		t.Errorf("Failed to get alias: %v", err)
	}
	if retrievedURL != url {
		t.Errorf("GetAlias() = %q, want %q", retrievedURL, url)
	}

	// List aliases
	aliases, err := cache.ListAliases()
	if err != nil {
		t.Errorf("Failed to list aliases: %v", err)
	}
	if len(aliases) != 1 {
		t.Errorf("ListAliases() returned %d aliases, want 1", len(aliases))
	}
	if aliases[aliasName] != url {
		t.Errorf("ListAliases() returned wrong URL for alias: got %q, want %q", aliases[aliasName], url)
	}

	// Remove alias
	if err := cache.RemoveAlias(aliasName); err != nil {
		t.Errorf("Failed to remove alias: %v", err)
	}

	// Should not exist after removal
	_, err = cache.GetAlias(aliasName)
	if err == nil {
		t.Error("Expected error when getting removed alias")
	}
}

func TestStoreAndGetBundle(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	url := "https://example.com/bundle.tar.gz"
	checksum := "abcd1234ef567890"
	content := "mock bundle content"

	// Create a reader with test content
	tmpFile := filepath.Join(t.TempDir(), "test-bundle.tar.gz")
	if err := os.WriteFile(tmpFile, []byte(content), 0644); err != nil {
		t.Fatalf("Failed to create test file: %v", err)
	}

	file, err := os.Open(tmpFile)
	if err != nil {
		t.Fatalf("Failed to open test file: %v", err)
	}
	defer file.Close()

	// Store bundle
	bundlePath, err := cache.StoreBundle(url, checksum, file, int64(len(content)))
	if err != nil {
		t.Fatalf("Failed to store bundle: %v", err)
	}

	// Verify file exists
	if _, err := os.Stat(bundlePath); err != nil {
		t.Errorf("Stored bundle file does not exist: %v", err)
	}

	// Get bundle
	retrievedPath, err := cache.GetBundle(url, checksum)
	if err != nil {
		t.Errorf("Failed to get bundle: %v", err)
	}
	if retrievedPath != bundlePath {
		t.Errorf("GetBundle() returned %q, want %q", retrievedPath, bundlePath)
	}

	// Verify content
	retrievedContent, err := os.ReadFile(retrievedPath)
	if err != nil {
		t.Errorf("Failed to read retrieved bundle: %v", err)
	}
	if string(retrievedContent) != content {
		t.Errorf("Retrieved content = %q, want %q", string(retrievedContent), content)
	}
}

func TestGetBundleWithWrongChecksum(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	url := "https://example.com/bundle.tar.gz"
	checksum := "abcd1234ef567890"
	content := "mock bundle content"

	tmpFile := filepath.Join(t.TempDir(), "test-bundle.tar.gz")
	if err := os.WriteFile(tmpFile, []byte(content), 0644); err != nil {
		t.Fatalf("Failed to create test file: %v", err)
	}

	file, err := os.Open(tmpFile)
	if err != nil {
		t.Fatalf("Failed to open test file: %v", err)
	}
	defer file.Close()

	// Store bundle
	_, err = cache.StoreBundle(url, checksum, file, int64(len(content)))
	if err != nil {
		t.Fatalf("Failed to store bundle: %v", err)
	}

	// Try to get with wrong checksum
	wrongChecksum := "deadbeefdeadbeef"
	_, err = cache.GetBundle(url, wrongChecksum)
	if err == nil {
		t.Error("Expected error when getting bundle with wrong checksum")
	}
}

func TestGetNonExistentBundle(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	url := "https://example.com/nonexistent.tar.gz"
	checksum := "abcd1234ef567890"

	// Should not exist
	_, err := cache.GetBundle(url, checksum)
	if err == nil {
		t.Error("Expected error when getting non-existent bundle")
	}
}